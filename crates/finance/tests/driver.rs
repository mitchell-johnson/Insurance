//! The driver of the close, against a ledger fed by a simulated core.

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::NaiveDate;
use common::*;
use ic_azure::blob::{MemoryWormStore, WormStore};
use ic_azure::keyvault::DevSigner;
use ic_azure::merkle::{publish_root, root_name};
use ic_azure::AzureError;
use ic_core::Reject;
use ic_finance::close::{Capacity, CheckStatus, Outcome, PeriodStatus};
use ic_finance::driver::{
    self, core_sources, distribution_sources, finance_sources, payments_sources, reinsurance_sources, unit_pricing_sources, Driver, DriverError, Pass, Reports,
    Source, SUBLEDGER_CHECK,
};
use ic_finance::{host, FinCommand, FinEnvelope, PeriodId};
use ic_ledger::sha256;
use serde_json::{json, Value};

/// The ledger as the driver reaches it, with switches for what can go wrong.
struct Ledger {
    books: Mutex<Books>,
    down: AtomicBool,
    /// Lines per page of an extract, to make the driver read several.
    page: Option<u64>,
    /// Change a line of each extract page on the way out.
    garble: AtomicBool,
}

impl driver::Books for Ledger {
    async fn propose(&self, envelope: FinEnvelope) -> Result<Result<(), Reject>, DriverError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(DriverError::Unavailable("no leader".to_string()));
        }
        Ok(self.books.lock().unwrap().apply(envelope).result)
    }

    async fn ask(&self, mut question: Value) -> Result<Value, DriverError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(DriverError::Unavailable("no leader".to_string()));
        }
        if let (Some(page), true) = (self.page, question["q"] == "extract") {
            question["limit"] = json!(page);
        }
        let mut answer = host::answer(&self.books.lock().unwrap().state, question).map_err(DriverError::Answer)?;
        if self.garble.load(Ordering::SeqCst) {
            if let Some(line) = answer.get_mut("lines").and_then(|l| l.get_mut(0)) {
                *line = json!("changed");
            }
        }
        Ok(answer)
    }
}

/// The other modules: one that answers like the commission module, and
/// payments and the ledger itself, which answers for policy suspense.
#[derive(Default)]
struct Others {
    down: Mutex<BTreeSet<String>>,
    sweep: Mutex<Option<Value>>,
}

impl driver::Modules for Others {
    async fn ask(&self, module: &str, question: Value) -> Result<Value, DriverError> {
        if self.down.lock().unwrap().contains(module) {
            return Err(DriverError::Unavailable(format!("{module} has no leader")));
        }
        let aged = |as_at: &Value| json!({ "kind": "any", "as_at": as_at, "items": [], "total_minor": 0, "count": 0 });
        Ok(if question == json!("sweep_check") {
            self.sweep.lock().unwrap().clone().unwrap_or(json!({ "green": true, "items": [] }))
        } else if let Some(held) = question.get("aged_held").or(question.get("waiting_cash")) {
            aged(&held["as_at"])
        } else if question.get("aged").is_some() || question["q"].as_str().is_some_and(|q| q.starts_with("aged_")) {
            aged(&question["as_at"])
        } else {
            json!({ "green": true })
        })
    }
}

/// A write-once store that can be made unreachable.
#[derive(Default)]
struct Store {
    inner: MemoryWormStore,
    down: AtomicBool,
}

impl WormStore for Store {
    async fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(AzureError::Response("storage is unreachable".to_string()));
        }
        self.inner.put_new(name, content_type, body).await
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(AzureError::Response("storage is unreachable".to_string()));
        }
        self.inner.get(name).await
    }
}

type TestDriver = Driver<Ledger, Others, Store, Store>;

fn sources() -> Vec<Source> {
    let mut all = distribution_sources();
    all.extend(payments_sources());
    all.extend(reinsurance_sources());
    all.extend(core_sources());
    all.extend(unit_pricing_sources());
    all.extend(finance_sources());
    all
}

fn block<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}

/// A core run from 15 November 2027 to `until` with two policies, whose
/// Merkle roots are left for the driver to record. `publish` says which
/// dates' roots the core has published.
fn driver_after(until: NaiveDate, publish: impl Fn(NaiveDate) -> bool) -> TestDriver {
    let start = d(2027, 11, 15);
    let mut w = World::new(2, start);
    w.record_roots = false;
    for n in 1..=2 {
        let (p, id) = w.issue(n, start);
        w.pay(p, &id, &format!("r{n}"), 4 * MONTHLY);
    }
    w.until(until);
    let roots = Store::default();
    for (date, heads) in &w.heads {
        if publish(*date) {
            block(publish_root(&DevSigner, &roots, *date, heads)).unwrap();
        }
    }
    Driver {
        books: Ledger {
            books: Mutex::new(std::mem::replace(&mut w.books, Books::empty())),
            down: AtomicBool::new(false),
            page: Some(2),
            garble: AtomicBool::new(false),
        },
        modules: Others::default(),
        roots,
        extracts: Store::default(),
        location: "https://stfinance.blob.core.windows.net/extracts".to_string(),
        sources: sources(),
    }
}

fn nov() -> PeriodId {
    period(2027, 11)
}

fn status(driver: &TestDriver, p: PeriodId, check: &str) -> CheckStatus {
    let books = driver.books.books.lock().unwrap();
    books.state.checks(p).into_iter().find(|c| c.id == check).unwrap().status
}

/// What people do at the console once the reports are in: the two checks
/// finance reports itself, and the finance lead's sign-off.
fn people_finish(driver: &TestDriver, p: PeriodId) {
    let mut books = driver.books.books.lock().unwrap();
    for check in ["tax_reconciliation", "analytical_review"] {
        let report = FinCommand::ReportCheck {
            period: p,
            check: check.to_string(),
            outcome: Outcome::Green,
            evidence: None,
        };
        books.console("olive", report).unwrap();
    }
    let blockers = books.state.blockers(p);
    assert!(blockers.is_empty(), "{p} is blocked: {blockers:?}");
    let sign = FinCommand::SignOff {
        period: p,
        capacity: Capacity::FinanceLead,
    };
    books.console("lena", sign).unwrap();
}

/// Purpose: November has ended and nobody has touched the ledger. One pass of the driver
/// records every day's published Merkle root, reads the three extracts from the data mart
/// two lines at a time, writes each file to immutable storage, registers it against the
/// root of 30 November, and passes on what commission, payments, collections and
/// investment report. Once finance has reported its own two checks and the finance lead
/// has signed, the next pass closes the month, and the pass after that finds nothing to do.
/// Why it matters: this is the month-end as it runs unattended. Each step is a hand-off
/// between systems, and a close that needs someone to carry a hash or a file by hand is a
/// close where the registered extract and the stored file can differ.
#[test]
fn a_month_goes_from_its_last_day_to_closed() {
    let driver = driver_after(d(2027, 12, 3), |_| true);
    for check in ["control_check", "valuation_extracts"] {
        assert!(matches!(status(&driver, nov(), check), CheckStatus::Red { .. }), "{check}");
    }

    let first = block(driver.pass()).unwrap();
    assert_eq!(first.roots_recorded.len(), 18, "15 November to 2 December");
    assert_eq!(
        first.extracts_registered,
        ["in_force:2027-11-30:v3", "movement:2027-11-30:v3", "experience:2027-11-30:v3"]
    );
    assert_eq!(
        first.reports_sent,
        [
            "commission_sweep",
            "aged:held_balance",
            "fund_cash_settlement",
            "aged:unidentified_receipts",
            "aged:pending_investment_cash",
            "unit_pricing",
            "aged:policy_suspense",
            SUBLEDGER_CHECK
        ]
    );
    assert!(first.closed.is_empty() && first.waiting.is_empty(), "{:?}", first.waiting);
    for check in ["control_check", "valuation_extracts", "commission_sweep", "aged:held_balance", SUBLEDGER_CHECK] {
        assert_eq!(status(&driver, nov(), check), CheckStatus::Green, "{check}");
    }

    // The stored file is the registered file.
    {
        let books = driver.books.books.lock().unwrap();
        let extracts: Vec<_> = books.state.extracts.values().collect();
        assert_eq!(extracts.len(), 3);
        for e in extracts {
            let name = e.location.strip_prefix("https://stfinance.blob.core.windows.net/extracts/").unwrap();
            let file = block(driver.extracts.get(name)).unwrap().unwrap();
            assert_eq!(sha256(&[&file]), e.content_hash);
            assert_eq!(file.iter().filter(|b| **b == b'\n').count() as u64, e.rows + 1);
            assert!(e.reconciled);
            let published: Value = serde_json::from_slice(&block(driver.roots.get(&root_name(e.as_at))).unwrap().unwrap()).unwrap();
            assert_eq!(published["root"], json!(e.merkle_root));
        }
    }

    people_finish(&driver, nov());
    let second = block(driver.pass()).unwrap();
    assert_eq!(
        second,
        Pass {
            closed: vec![nov()],
            ..Pass::default()
        }
    );
    assert_eq!(driver.books.books.lock().unwrap().state.periods[&nov()].status, PeriodStatus::Closed);
    assert_eq!(block(driver.pass()).unwrap(), Pass::default());
}

/// Purpose: the root of 30 November is not yet published and the root of 20 November is
/// published with one partition's head altered. The driver records every other day, says
/// in words what it is waiting for, cuts no extract, and reports its own check red. When
/// the missing root is published the next pass registers the extracts, and the altered
/// root is still refused.
/// Why it matters: the Merkle root is what ties the ledger and every extract to the core's
/// chain. A driver that passed on a root it had not verified, or registered an extract
/// against no root, would remove the one proof that the figures came from the core.
#[test]
fn nothing_is_anchored_to_a_root_that_is_missing_or_does_not_verify() {
    let (end, bad) = (d(2027, 11, 30), d(2027, 11, 20));
    let driver = driver_after(d(2027, 12, 3), |date| date != end && date != bad);
    let heads = {
        let published = block(driver.roots.get(&root_name(d(2027, 11, 21)))).unwrap().unwrap();
        let mut value: Value = serde_json::from_slice(&published).unwrap();
        value["business_date"] = json!(bad);
        serde_json::to_vec(&value).unwrap()
    };
    block(driver.roots.put_new(&root_name(bad), "application/json", heads)).unwrap();

    let first = block(driver.pass()).unwrap();
    assert_eq!(first.roots_recorded.len(), 16);
    assert!(first.extracts_registered.is_empty());
    assert!(first.waiting.contains(&"the Merkle root of 2027-11-30 is not published".to_string()));
    assert!(first.waiting.contains(&"the Merkle root published for 2027-11-20 does not verify".to_string()));
    assert!(first.waiting.iter().any(|w| w == "2027-11: the Merkle root of 2027-11-30 is not published"));
    let CheckStatus::Red { items } = status(&driver, nov(), SUBLEDGER_CHECK) else {
        panic!("the driver's own check is not red");
    };
    assert_eq!(items.len(), 3, "one for each extract not registered: {items:?}");

    // Published at last, by the core's own publisher.
    let all = driver_after(d(2027, 12, 3), |date| date == end);
    let published = block(all.roots.get(&root_name(end))).unwrap().unwrap();
    block(driver.roots.put_new(&root_name(end), "application/json", published)).unwrap();
    let second = block(driver.pass()).unwrap();
    assert_eq!(second.roots_recorded, [end]);
    assert_eq!(second.extracts_registered.len(), 3);
    assert_eq!(second.waiting, ["the Merkle root published for 2027-11-20 does not verify"]);
    assert_eq!(status(&driver, nov(), SUBLEDGER_CHECK), CheckStatus::Green);
    assert_eq!(status(&driver, nov(), "valuation_extracts"), CheckStatus::Green);
    assert!(matches!(status(&driver, nov(), "control_check"), CheckStatus::Red { .. }));

    // A period whose root is published but does not verify gets no extract either.
    let driver = driver_after(d(2027, 12, 3), |date| date != end);
    let other = block(driver.roots.get(&root_name(bad))).unwrap().unwrap();
    block(driver.roots.put_new(&root_name(end), "application/json", other)).unwrap();
    let pass = block(driver.pass()).unwrap();
    assert!(pass.extracts_registered.is_empty());
    assert!(pass
        .waiting
        .contains(&"2027-11: the Merkle root published for 2027-11-30 does not verify".to_string()));
}

/// Purpose: the commission module is unreachable on the first pass and finds a stray
/// clearing balance on the second. The first pass carries on with everything else, lists
/// what did not answer and reports the driver's own check red with the reason. The second
/// passes the red sweep on with its items. When the sweep is clean the third pass turns
/// both checks green.
/// Why it matters: month-end runs while other modules are being failed over or patched.
/// One module being away must not stop the roots, the extracts or the other reports, and
/// it must never be read as a clean report.
#[test]
fn a_module_that_does_not_answer_is_reported_and_asked_again() {
    let driver = driver_after(d(2027, 12, 3), |_| true);
    driver.modules.down.lock().unwrap().insert("distribution".to_string());
    let first = block(driver.pass()).unwrap();
    assert_eq!(first.roots_recorded.len(), 18);
    assert_eq!(first.extracts_registered.len(), 3);
    assert_eq!(
        first.reports_sent,
        [
            "fund_cash_settlement",
            "aged:unidentified_receipts",
            "aged:pending_investment_cash",
            "unit_pricing",
            "aged:policy_suspense",
            SUBLEDGER_CHECK
        ]
    );
    assert_eq!(first.waiting.len(), 2);
    assert!(first.waiting[0].starts_with("2027-11: distribution did not answer for commission_sweep: unavailable"));
    let CheckStatus::Red { items } = status(&driver, nov(), SUBLEDGER_CHECK) else {
        panic!("the driver's own check is not red");
    };
    assert!(items.iter().all(|i| i.starts_with("distribution did not answer")), "{items:?}");
    assert!(matches!(status(&driver, nov(), "commission_sweep"), CheckStatus::Red { .. }));

    driver.modules.down.lock().unwrap().clear();
    *driver.modules.sweep.lock().unwrap() = Some(json!({ "green": false, "items": [{ "party": "adv-7", "clearing_minor": 1250 }] }));
    let second = block(driver.pass()).unwrap();
    assert_eq!(second.reports_sent, ["commission_sweep", "aged:held_balance", SUBLEDGER_CHECK]);
    assert!(second.waiting.is_empty());
    let CheckStatus::Red { items } = status(&driver, nov(), "commission_sweep") else {
        panic!("the sweep is not red");
    };
    assert_eq!(items, [r#"{"party":"adv-7","clearing_minor":1250}"#]);
    assert_eq!(status(&driver, nov(), SUBLEDGER_CHECK), CheckStatus::Green);

    // Still red, so it is asked again; the same answer is the same report.
    let again = block(driver.pass()).unwrap();
    assert_eq!(again.reports_sent, ["commission_sweep"]);
    *driver.modules.sweep.lock().unwrap() = None;
    let third = block(driver.pass()).unwrap();
    assert_eq!(third.reports_sent, ["commission_sweep"]);
    assert_eq!(status(&driver, nov(), "commission_sweep"), CheckStatus::Green);
    // Nothing is closed without the sign-off, however green it is.
    assert!(third.closed.is_empty());
}

/// Purpose: the driver is stopped at each point of storing an extract. A file written by a
/// pass that died before registering it is registered by the next pass without being
/// written again. A file of that name with other content is never overwritten or
/// registered. An extract whose pages do not hash to the whole is not stored. Storage or
/// the ledger being unreachable fails the pass and changes nothing.
/// Why it matters: an extract is the evidence under a reserve. Write-once storage only
/// protects it if the driver also refuses to register a hash for a file it did not store,
/// and a driver that is restarted mid-month must end in the same place as one that was not.
#[test]
fn an_interrupted_or_conflicting_extract_is_never_registered_wrongly() {
    // A pass that stored the files and died: storage holds them, the ledger does not.
    let earlier = driver_after(d(2027, 12, 3), |_| true);
    block(earlier.pass()).unwrap();
    let driver = driver_after(d(2027, 12, 3), |_| true);
    let name = "extracts/2027-11-30/in_force-v3.csv";
    let stored = block(earlier.extracts.get(name)).unwrap().unwrap();
    block(driver.extracts.put_new(name, "text/csv", stored.clone())).unwrap();
    // Another file under the movement extract's name.
    block(driver.extracts.put_new("extracts/2027-11-30/movement-v3.csv", "text/csv", b"other".to_vec())).unwrap();

    let pass = block(driver.pass()).unwrap();
    assert_eq!(pass.extracts_registered, ["in_force:2027-11-30:v3", "experience:2027-11-30:v3"]);
    assert_eq!(
        pass.waiting,
        ["2027-11: extracts/2027-11-30/movement-v3.csv is already stored with other content"]
    );
    assert_eq!(block(driver.extracts.get(name)).unwrap().unwrap(), stored);
    assert_eq!(block(driver.extracts.get("extracts/2027-11-30/movement-v3.csv")).unwrap().unwrap(), b"other");
    assert!(matches!(status(&driver, nov(), "valuation_extracts"), CheckStatus::Red { .. }));

    // Pages that are not one file.
    let driver = driver_after(d(2027, 12, 3), |_| true);
    driver.books.garble.store(true, Ordering::SeqCst);
    let pass = block(driver.pass()).unwrap();
    assert!(pass.extracts_registered.is_empty());
    assert_eq!(pass.waiting.len(), 3);
    assert_eq!(pass.waiting[0], "2027-11: the in_force extract changed while it was read");
    assert!(driver.extracts.inner.names().is_empty());

    // Storage unreachable, then the ledger: the pass fails and is simply run again.
    let driver = driver_after(d(2027, 12, 3), |_| true);
    driver.extracts.down.store(true, Ordering::SeqCst);
    assert_eq!(
        block(driver.pass()).unwrap_err(),
        DriverError::Unavailable("unexpected response: storage is unreachable".to_string())
    );
    driver.extracts.down.store(false, Ordering::SeqCst);
    driver.roots.down.store(true, Ordering::SeqCst);
    assert!(matches!(block(driver.pass()), Err(DriverError::Unavailable(_))));
    driver.roots.down.store(false, Ordering::SeqCst);
    driver.books.down.store(true, Ordering::SeqCst);
    assert_eq!(block(driver.pass()).unwrap_err().to_string(), "unavailable: no leader");
    driver.books.down.store(false, Ordering::SeqCst);
    let pass = block(driver.pass()).unwrap();
    assert_eq!(pass.extracts_registered.len(), 3);
    assert!(pass.waiting.is_empty());
}

/// Purpose: November and December have both ended. December is signed off and clean but
/// November is not signed, so the driver closes neither and says nothing is wrong. A
/// report sent under a name the close configuration does not allow is refused by the
/// ledger and listed, and an answer that is not a report fails the pass.
/// Why it matters: months close in order, and the driver must not ask the ledger to break
/// that or hide a refusal. A report under the wrong name being dropped silently would
/// leave a check red with nobody told why.
#[test]
fn refusals_are_listed_and_months_close_in_order() {
    let mut driver = driver_after(d(2028, 1, 4), |_| true);
    let dec = period(2027, 12);
    let first = block(driver.pass()).unwrap();
    assert_eq!(first.extracts_registered.len(), 6);
    // The reinsurer's sweep is asked for once: for December, which ends a quarter.
    assert_eq!(first.reports_sent.iter().filter(|r| *r == "reinsurance_sweep").count(), 1);
    assert_eq!(status(&driver, nov(), "reinsurance_sweep"), CheckStatus::NotDue);
    assert_eq!(status(&driver, dec, "reinsurance_sweep"), CheckStatus::Green);
    assert!(first.waiting.is_empty(), "{:?}", first.waiting);
    {
        // December ends a quarter: its own requirements are not the subject here.
        let books = driver.books.books.lock().unwrap();
        let work = books.state.work();
        assert_eq!(work.closing.iter().map(|w| w.period).collect::<Vec<_>>(), [nov(), dec]);
        assert_eq!(work.closing[1].sign_offs_needed, [Capacity::FinanceLead, Capacity::AppointedActuary]);
        assert_eq!(work.closing[0].sign_offs_needed, [Capacity::FinanceLead]);
    }
    let pass = block(driver.pass()).unwrap();
    assert!(pass.closed.is_empty() && pass.waiting.is_empty());

    // November is clean and signed: it closes, and December waits its turn.
    people_finish(&driver, nov());
    let pass = block(driver.pass()).unwrap();
    assert_eq!(pass.closed, [nov()]);

    // A source set up under the wrong name.
    let driver_wrong = {
        driver.sources = vec![Source {
            caller: "payments".to_string(),
            module: "distribution".to_string(),
            reports: Reports::Check("commission_sweep".to_string()),
            question: |_| json!("sweep_check"),
        }];
        *driver.modules.sweep.lock().unwrap() = Some(json!({ "green": false, "items": ["x"] }));
        // Make the check due again for December.
        let mut books = driver.books.books.lock().unwrap();
        let red = FinCommand::ReportCheck {
            period: dec,
            check: "commission_sweep".to_string(),
            outcome: Outcome::Red {
                items: vec!["stray".to_string()],
            },
            evidence: None,
        };
        books.send("commission", None, red).unwrap();
        drop(books);
        &driver
    };
    let pass = block(driver_wrong.pass()).unwrap();
    assert_eq!(pass.waiting.len(), 1);
    assert!(
        pass.waiting[0].starts_with("2027-12: the ledger refused commission_sweep from distribution:"),
        "{:?}",
        pass.waiting
    );
    let CheckStatus::Red { items } = status(&driver, dec, SUBLEDGER_CHECK) else {
        panic!("the driver's own check is not red");
    };
    assert_eq!(items.len(), 1);

    // An answer that is not a report.
    *driver.modules.sweep.lock().unwrap() = Some(json!({ "sweep": "fine" }));
    assert!(matches!(block(driver.pass()), Err(DriverError::Answer(m)) if m.starts_with("commission_sweep:")));
}

/// Purpose: the waiting cash of two partitions becomes one aged report: a policy with
/// nothing waiting is left out, the items run oldest first, the total and the count cover
/// every policy, and a list longer than the cap keeps the oldest.
/// Why it matters: the close turns red on the oldest and largest items. A report that cut
/// off the oldest cash, or whose total covered only the items listed, would show a month
/// as clean while money sat uninvested past the pricing lag.
#[test]
fn waiting_cash_from_every_partition_is_one_report_oldest_first() {
    use ic_core::invest::WaitingCash;
    use ic_money::Currency::NZD;
    let cash = |policy: &str, amount_minor: i64, since: NaiveDate| WaitingCash {
        policy: policy.into(),
        amount_minor,
        currency: NZD,
        since,
    };
    let as_at = d(2027, 11, 30);
    let report = driver::waiting_cash_report(
        as_at,
        vec![
            cash("pol-b", 500, d(2027, 11, 28)),
            cash("pol-z", 0, d(2027, 11, 1)),
            cash("pol-a", 250, d(2027, 11, 20)),
        ],
    );
    assert_eq!(
        report,
        json!({
            "as_at": "2027-11-30",
            "items": [
                { "party": "pol-a", "reason": "pending_investment_cash", "amount_minor": 250, "since": "2027-11-20" },
                { "party": "pol-b", "reason": "pending_investment_cash", "amount_minor": 500, "since": "2027-11-28" },
            ],
            "total_minor": 750,
            "count": 2,
        })
    );
    let many: Vec<WaitingCash> = (0..driver::MAX_AGED_ITEMS as i64 + 5)
        .map(|n| cash(&format!("pol-{n:05}"), 1, as_at - chrono::Days::new(n as u64 % 7)))
        .collect();
    let report = driver::waiting_cash_report(as_at, many);
    assert_eq!(report["items"].as_array().unwrap().len(), driver::MAX_AGED_ITEMS);
    assert_eq!((report["count"].clone(), report["total_minor"].clone()), (json!(2005), json!(2005)));
    assert_eq!(report["items"][0]["since"], json!("2027-11-24"));
}
