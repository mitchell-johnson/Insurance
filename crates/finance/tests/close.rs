//! The period close: checks, waivers, sign-off and the frozen period.

mod common;

use common::*;
use ic_finance::books::{hash_rows, AgedItem, FinEvent, JournalKind};
use ic_finance::chart::ChartAccount;
use ic_finance::close::{Capacity, CheckStatus, Outcome, PeriodStatus};
use ic_finance::{FinCommand, PeriodId};
use ic_ledger::{AccountClass, Hash32};

fn nov() -> PeriodId {
    period(2027, 11)
}

fn dec() -> PeriodId {
    period(2027, 12)
}

/// Two partitions run from 20 November to `until`, with one paying policy.
fn world(until: chrono::NaiveDate) -> World {
    let mut w = World::new(2, d(2027, 11, 20));
    let (p, id) = w.issue(1, d(2027, 11, 20));
    w.pay(p, &id, "r1", 3 * MONTHLY);
    w.until(until);
    w
}

fn sign(period: PeriodId, capacity: Capacity) -> FinCommand {
    FinCommand::SignOff { period, capacity }
}

fn close(period: PeriodId) -> FinCommand {
    FinCommand::ClosePeriod { period }
}

fn report(period: PeriodId, check: &str, outcome: Outcome) -> FinCommand {
    FinCommand::ReportCheck {
        period,
        check: check.to_string(),
        outcome,
        evidence: Some("vault://close/evidence".to_string()),
    }
}

fn red(item: &str) -> Outcome {
    Outcome::Red { items: vec![item.to_string()] }
}

fn waive(period: PeriodId, check: &str) -> FinCommand {
    FinCommand::WaiveCheck {
        period,
        check: check.to_string(),
        reason: "statement dispute with one adviser group".to_string(),
        remediation_owner: "oscar".to_string(),
        due: d(2027, 12, 31),
    }
}

fn status(w: &World, period: PeriodId, check: &str) -> CheckStatus {
    w.books.state.checks(period).into_iter().find(|c| c.id == check).unwrap().status
}

fn events(w: &World) -> Vec<&FinEvent> {
    w.books.applied.iter().flat_map(|a| &a.events).map(|e| &e.event).collect()
}

/// Purpose: a month moves to closing when its last day has closed on every partition, cannot
/// be signed off or closed while a blocking check is red, closes once the checks are green
/// and the finance lead has signed, and from then on takes no journal, report, waiver or
/// sign-off. Its trial balance is stored with a hash when it closes.
/// Why it matters: a closed period is what was reported to the board and the regulator. If
/// anything could post into it afterwards, the reported figures would stop matching the
/// ledger and nobody could say which was right.
#[test]
fn a_month_closes_once_its_checks_are_green_and_then_never_changes() {
    let mut w = world(d(2027, 12, 3));
    assert_eq!(w.books.state.periods[&nov()].status, PeriodStatus::Closing);
    assert_eq!(w.books.state.periods[&dec()].status, PeriodStatus::Open);
    assert_eq!(events(&w).iter().filter(|e| matches!(e, FinEvent::PeriodClosing { .. })).count(), 1);

    let b = &mut w.books;
    assert_eq!(b.console_code("lena", sign(nov(), Capacity::FinanceLead)), "blocked");
    assert_eq!(b.code("close-workflow", None, close(nov())), "blocked");
    assert_eq!(b.console_code("lena", sign(dec(), Capacity::FinanceLead)), "period");
    assert_eq!(b.console_code("lena", sign(period(2028, 5), Capacity::FinanceLead)), "period");
    let before = b.state.dashboard(nov(), d(2027, 12, 3));
    assert_eq!(before.sign_offs_needed, vec![Capacity::FinanceLead]);
    assert!(before.blockers.contains(&"valuation_extracts".to_string()));
    assert_eq!(before.trial_balance_hash, None);

    w.make_green(nov());
    w.waive_if_red(nov(), "fund_balance");
    let b = &mut w.books;
    assert!(b.state.blockers(nov()).is_empty());
    assert_eq!(b.code("close-workflow", None, close(nov())), "sign_off");
    assert_eq!(b.console_code("ana", sign(nov(), Capacity::FinanceLead)), "authority");
    b.console("lena", sign(nov(), Capacity::FinanceLead)).unwrap();
    let again = b.send_keyed("sign-again", "finance-console", Some("liam"), sign(nov(), Capacity::FinanceLead));
    assert_eq!((again.result, again.events.len()), (Ok(()), 0), "one sign-off per capacity");
    assert_eq!(b.code("commission", None, close(nov())), "caller");
    b.console("lena", close(nov())).unwrap();

    let state = &b.state;
    let closed = state.periods[&nov()].closed.clone().unwrap();
    assert_eq!(closed.trial_balance, state.trial_balance(nov(), None));
    assert_eq!(closed.trial_balance_hash, hash_rows(&closed.trial_balance));
    assert_eq!((closed.final_seq, closed.final_head), (state.ledger.entry_count(), state.ledger.head()));
    assert_eq!(closed.by.as_deref(), Some("lena"));
    let after = state.dashboard(nov(), d(2027, 12, 6));
    assert_eq!(
        (after.status, after.trial_balance_hash),
        (Some(PeriodStatus::Closed), Some(closed.trial_balance_hash))
    );
    assert!(after.sign_offs_needed.is_empty() && after.blockers.is_empty());
    assert_eq!((after.days_since_period_end, after.extracts.len()), (6, 2));

    // Nothing gets in afterwards.
    let late = journal("late", JournalKind::Accrual, nov(), "6500", "2500", 10_00);
    assert_eq!(b.console_code("olive", FinCommand::ProposeJournal { journal: late }), "period_closed");
    assert_eq!(b.code("commission", None, report(nov(), "commission_sweep", Outcome::Green)), "period_closed");
    assert_eq!(b.console_code("lena", waive(nov(), "commission_sweep")), "period_closed");
    assert_eq!(b.console_code("lena", sign(nov(), Capacity::FinanceLead)), "period_closed");
    assert_eq!(b.console_code("lena", close(nov())), "period_closed");
    assert_eq!(b.state.periods[&nov()].closed, Some(closed));
}

/// Purpose: the finance lead can sign off over a red blocking check only by recording a
/// waiver with a reason, a remediation owner who is someone else, and a date after period
/// end. A waiver is refused for a check that is green, unknown or not blocking. It stays on
/// the dashboard, shows overdue once its date passes, and the next period's check starts red
/// again with the earlier waiver still listed.
/// Why it matters: a close that could be forced with no record would hide exactly the items
/// the checks exist to surface, and a waiver that cleared the underlying item would let a
/// one-off decision become a permanent blind spot.
#[test]
fn a_waiver_is_on_the_record_and_never_clears_the_item() {
    let mut w = world(d(2028, 1, 3));
    w.make_green(nov());
    w.waive_if_red(nov(), "fund_balance");
    let b = &mut w.books;
    b.send("commission", None, report(nov(), "commission_sweep", red("clearing is 12.00 over statements")))
        .unwrap();
    assert_eq!(b.state.blockers(nov()).len(), 1);
    assert_eq!(b.console_code("lena", sign(nov(), Capacity::FinanceLead)), "blocked");

    assert_eq!(b.console_code("olive", waive(nov(), "commission_sweep")), "authority");
    let with = |change: &dyn Fn(&mut FinCommand)| {
        let mut cmd = waive(nov(), "commission_sweep");
        change(&mut cmd);
        cmd
    };
    let blank = with(&|c| {
        if let FinCommand::WaiveCheck { reason, .. } = c {
            reason.clear();
        }
    });
    assert_eq!(b.console_code("lena", blank), "waiver");
    let own = with(&|c| {
        if let FinCommand::WaiveCheck { remediation_owner, .. } = c {
            *remediation_owner = "lena".to_string();
        }
    });
    assert_eq!(b.console_code("lena", own), "separation");
    let early = with(&|c| {
        if let FinCommand::WaiveCheck { due, .. } = c {
            *due = d(2027, 11, 30);
        }
    });
    assert_eq!(b.console_code("lena", early), "waiver");
    assert_eq!(b.console_code("lena", waive(nov(), "no_such_check")), "check");
    assert_eq!(b.console_code("lena", waive(nov(), "trial_balance")), "waiver");
    b.console("olive", report(nov(), "analytical_review", red("claims up 40% unexplained")))
        .unwrap();
    assert_eq!(b.console_code("lena", waive(nov(), "analytical_review")), "waiver");
    assert_eq!(b.console_code("lena", waive(period(2028, 6), "commission_sweep")), "period");

    b.console("lena", waive(nov(), "commission_sweep")).unwrap();
    assert!(matches!(status(&w, nov(), "commission_sweep"), CheckStatus::Waived { items } if items == vec!["clearing is 12.00 over statements".to_string()]));
    let b = &mut w.books;
    assert!(b.state.blockers(nov()).is_empty());
    let board = b.state.dashboard(nov(), d(2027, 12, 20));
    let waiver = board.waivers.iter().find(|x| x.waiver.check == "commission_sweep").unwrap();
    assert_eq!((waiver.period, waiver.waiver.by.as_str(), waiver.overdue), (nov(), "lena", false));
    assert_eq!(waiver.waiver.items, vec!["clearing is 12.00 over statements".to_string()]);
    assert!(b.state.dashboard(nov(), d(2028, 1, 1)).waivers.iter().all(|x| x.overdue));
    b.console("lena", sign(nov(), Capacity::FinanceLead)).unwrap();
    b.console("lena", close(nov())).unwrap();

    // December starts red again, and November's waiver is still in view until it is green.
    assert!(matches!(status(&w, dec(), "commission_sweep"), CheckStatus::Red { .. }));
    let carried = w.books.state.dashboard(dec(), d(2028, 1, 3)).waivers;
    assert!(carried.iter().any(|x| x.period == nov() && x.waiver.check == "commission_sweep" && x.overdue));
    w.books.send("commission", None, report(dec(), "commission_sweep", Outcome::Green)).unwrap();
    let carried = w.books.state.dashboard(dec(), d(2028, 1, 3)).waivers;
    assert!(!carried.iter().any(|x| x.waiver.check == "commission_sweep"));
}

/// Purpose: a journal approved after the finance lead has signed a period clears the
/// sign-off, and the period cannot close until it is signed again.
/// Why it matters: a sign-off attests to the books as they stood. If a late journal could
/// ride under an earlier signature, the lead would have signed figures they never saw.
#[test]
fn a_posting_after_sign_off_clears_it() {
    let mut w = world(d(2027, 12, 3));
    w.make_green(nov());
    w.waive_if_red(nov(), "fund_balance");
    let b = &mut w.books;
    b.console("lena", sign(nov(), Capacity::FinanceLead)).unwrap();
    let late = journal("late", JournalKind::Accrual, nov(), "6500", "2500", 10_00);
    b.console("olive", FinCommand::ProposeJournal { journal: late }).unwrap();
    assert_eq!(b.console_code("lena", close(nov())), "blocked", "a proposed journal blocks the close");
    b.console("ana", FinCommand::ApproveJournal { id: "late".to_string() }).unwrap();
    assert!(matches!(
        b.applied.last().unwrap().events.last().unwrap().event,
        FinEvent::SignOffsCleared { .. }
    ));
    assert!(b.state.periods[&nov()].sign_offs.is_empty());
    assert_eq!(b.console_code("lena", close(nov())), "sign_off");
    b.console("liam", sign(nov(), Capacity::FinanceLead)).unwrap();
    b.console("liam", close(nov())).unwrap();
}

/// Purpose: months close in order, and a month that ends a quarter also needs an actuarial
/// journal posted with the appointed actuary's sign-off, the quarterly sweeps and handover,
/// and the appointed actuary's sign-off on the period itself.
/// Why it matters: opening balances are the previous month's closing balances, so closing
/// out of order would freeze a month on top of one that can still move. The quarter is what
/// goes to the regulator, and the law puts the actuary's name on it.
#[test]
fn months_close_in_order_and_a_quarter_needs_the_actuary() {
    let mut w = world(d(2028, 1, 4));
    assert_eq!(status(&w, nov(), "actuarial_journals"), CheckStatus::NotDue);
    assert_eq!(status(&w, nov(), "reinsurance_sweep"), CheckStatus::NotDue);
    assert!(
        matches!(status(&w, dec(), "actuarial_journals"), CheckStatus::Red { items } if items == vec!["no actuarial journal is posted for the quarter".to_string()])
    );
    assert_eq!(
        w.books.state.dashboard(dec(), d(2028, 1, 4)).sign_offs_needed,
        vec![Capacity::FinanceLead, Capacity::AppointedActuary]
    );

    w.make_green(dec());
    w.waive_if_red(dec(), "fund_balance");
    let mut reserves = journal("q3", JournalKind::Actuarial, dec(), "6000", "2400", 5_000_00);
    reserves.extract_id = Some("in_force-2027-12".to_string());
    w.books.console("olive", FinCommand::ProposeJournal { journal: reserves }).unwrap();
    w.books.console("ana", FinCommand::ApproveJournal { id: "q3".to_string() }).unwrap();
    assert!(matches!(status(&w, dec(), "actuarial_journals"), CheckStatus::Red { items } if items[0].contains("without the appointed actuary's sign-off")));
    w.books.console("aroha", FinCommand::SignActuarialJournal { id: "q3".to_string() }).unwrap();
    assert_eq!(status(&w, dec(), "actuarial_journals"), CheckStatus::Green);

    let b = &mut w.books;
    b.console("lena", sign(dec(), Capacity::FinanceLead)).unwrap();
    assert_eq!(b.console_code("lena", close(dec())), "order");
    assert_eq!(b.console_code("lena", sign(dec(), Capacity::AppointedActuary)), "authority");
    w.close(nov());
    let b = &mut w.books;
    assert_eq!(b.console_code("lena", close(dec())), "sign_off");
    b.console("aroha", sign(dec(), Capacity::AppointedActuary)).unwrap();
    b.console("lena", close(dec())).unwrap();
    assert_eq!(b.state.periods[&dec()].sign_offs.len(), 2);
    // January opens on December's closing balances.
    let jan = b.state.movements(period(2028, 1));
    let reserve = jan.iter().find(|r| r.account == "2400").unwrap();
    assert_eq!((reserve.opening, reserve.activity), (-5_000_00 * ic_money::FINE_PER_MINOR, 0));
}

/// Purpose: a check's result is accepted only from the caller the close configuration names
/// for it, for a check that exists and is not worked out by the ledger itself. A red result
/// must list what is wrong, and a person's report carries the person.
/// Why it matters: each step of the close has an owner. If any module could mark another's
/// step green, the dashboard would show a close that nobody accountable had confirmed.
#[test]
fn a_check_is_reported_only_by_its_declared_reporter() {
    let mut w = world(d(2027, 12, 3));
    let b = &mut w.books;
    assert_eq!(b.code("payments", None, report(nov(), "commission_sweep", Outcome::Green)), "caller");
    assert_eq!(b.code("commission", None, report(nov(), "no_such_check", Outcome::Green)), "check");
    assert_eq!(
        b.code("finance-console", Some("olive"), report(nov(), "control_check", Outcome::Green)),
        "check"
    );
    assert_eq!(
        b.code("commission", None, report(nov(), "commission_sweep", Outcome::Red { items: vec![] })),
        "check"
    );
    assert_eq!(b.code("finance-console", None, report(nov(), "tax_reconciliation", Outcome::Green)), "actor");
    assert_eq!(
        b.code("commission", None, report(period(2028, 6), "commission_sweep", Outcome::Green)),
        "period"
    );

    b.console("olive", report(nov(), "tax_reconciliation", red("GST return differs by 3.10")))
        .unwrap();
    let stored = &b.state.periods[&nov()].reports["tax_reconciliation"];
    assert_eq!((stored.by.as_str(), stored.actor.as_deref()), ("finance-console", Some("olive")));
    let board = b.state.dashboard(nov(), d(2027, 12, 10));
    let step = board.steps.iter().find(|s| s.check.id == "tax_reconciliation").unwrap();
    assert_eq!(step.check.evidence.as_deref(), Some("vault://close/evidence"));
    assert_eq!((step.due, step.overdue), (d(2027, 12, 3), true));
    let unreported = board.steps.iter().find(|s| s.check.id == "analytical_review").unwrap();
    assert!(matches!(&unreported.check.status, CheckStatus::Red { items } if items[0] == "not yet reported for this period"));
    assert!(
        !board.blockers.contains(&"analytical_review".to_string()),
        "a check that does not block is not a blocker"
    );
}

/// Purpose: held balances are reported by the commission module aged by party. One held
/// longer or larger than the threshold with no documented action turns the month-end check
/// red and blocks the close, naming the party. Recording an action clears it. A report from
/// another caller, for an undeclared kind, with an item dated after the report, or older
/// than one already held is refused, and a report from before period end does not count.
/// Why it matters: money held back from an adviser whose licence was lost can otherwise sit
/// unnoticed for years. The design makes it a blocking month-end item so somebody has to
/// decide, every month, what happens to each balance.
#[test]
fn a_held_balance_with_no_documented_action_blocks_the_close() {
    let mut w = world(d(2027, 12, 3));
    w.make_green(nov());
    w.waive_if_red(nov(), "fund_balance");
    let item = |party: &str, minor: i64, since: chrono::NaiveDate, action: Option<&str>| AgedItem {
        party: party.to_string(),
        reason: "suspended_licence".to_string(),
        amount_minor: minor,
        since,
        owner: Some("counterparty_finance".to_string()),
        action: action.map(str::to_string),
    };
    let held = |as_at: chrono::NaiveDate, items: Vec<AgedItem>| FinCommand::ReportAgedItems {
        kind: "held_balance".to_string(),
        as_at,
        total_minor: items.iter().map(|i| i.amount_minor).sum(),
        count: items.len() as u32,
        items,
    };
    let items = vec![
        item("adviser-17", 4_200_00, d(2027, 7, 1), None),
        item("adviser-08", 150_000_00, d(2027, 11, 20), None),
        item("adviser-30", 900_00, d(2027, 6, 1), Some("vault://actions/30")),
        item("adviser-41", 50_00, d(2027, 11, 1), None),
    ];
    let b = &mut w.books;
    assert_eq!(b.code("payments", None, held(d(2027, 11, 30), items.clone())), "caller");
    let mut other = held(d(2027, 11, 30), vec![]);
    if let FinCommand::ReportAgedItems { kind, .. } = &mut other {
        *kind = "lost_property".to_string();
    }
    assert_eq!(b.code("commission", None, other), "aged");
    assert_eq!(
        b.code("commission", None, held(d(2027, 11, 30), vec![item("x", 1, d(2027, 12, 1), None)])),
        "aged"
    );

    b.send("commission", None, held(d(2027, 11, 30), items.clone())).unwrap();
    let CheckStatus::Red { items: listed } = status(&w, nov(), "aged:held_balance") else {
        panic!("held balances with no action did not turn the check red")
    };
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0], "adviser-17 (suspended_licence): 420000 held for 152 days with no documented action");
    assert!(listed[1].starts_with("adviser-08"));
    let b = &mut w.books;
    assert_eq!(b.console_code("lena", sign(nov(), Capacity::FinanceLead)), "blocked");

    let summary = &b.state.dashboard(nov(), d(2027, 12, 3)).aged;
    let held_summary = summary.iter().find(|a| a.kind == "held_balance").unwrap();
    assert_eq!((held_summary.count, held_summary.total_minor), (4, 155_150_00));
    let ages: Vec<i64> = held_summary.items.iter().map(|i| i.age_days).collect();
    assert_eq!(ages, vec![182, 152, 29, 10], "oldest first");

    // A report as at a date before period end says nothing about the period.
    assert_eq!(b.code("commission", None, held(d(2027, 11, 29), vec![])), "aged");
    let actioned: Vec<AgedItem> = items
        .into_iter()
        .map(|i| AgedItem {
            action: Some("vault://actions/x".to_string()),
            ..i
        })
        .collect();
    b.send("commission", None, held(d(2027, 12, 2), actioned)).unwrap();
    assert_eq!(status(&w, nov(), "aged:held_balance"), CheckStatus::Green);
    assert!(matches!(status(&w, dec(), "aged:held_balance"), CheckStatus::Red { items } if items[0] == "no held_balance report as at 2027-12-31 or later"));
}

/// Purpose: the ledger is configured once, and a chart or close configuration takes effect
/// only when a finance lead other than its proposer approves it. A new chart must be the
/// next version and keep every account, class, lock and role mapping. A proposal overtaken
/// by another activation is refused when its turn comes.
/// Why it matters: the chart decides where every dollar from the core lands, and the close
/// configuration decides what can block sign-off. One person able to change either alone
/// could move balances between accounts or switch off a blocking check.
#[test]
fn configuration_changes_need_a_second_person_and_only_add() {
    let mut b = Books::empty();
    let configure = |partitions, year_end_month| FinCommand::Configure { partitions, year_end_month };
    assert_eq!(b.code("operator", None, configure(0, 3)), "settings");
    assert_eq!(b.code("operator", None, configure(4, 13)), "settings");
    assert_eq!(b.code("feed", None, configure(4, 3)), "caller");
    assert_eq!((b.state.partitions(), b.state.year_end_month()), (0, 12), "defaults before configuration");
    b.send("operator", None, configure(4, 3)).unwrap();
    assert_eq!(b.code("operator", None, configure(4, 3)), "configured");
    assert_eq!((b.state.partitions(), b.state.year_end_month()), (4, 3));
    assert!(b.state.checks(nov()).is_empty(), "no close configuration, no checks");

    let v1 = chart();
    let id = |c: &ic_finance::Chart| format!("chart:{}:{}", c.version, c.content_hash);
    let approve = |id: String| FinCommand::ApproveConfig { id };
    assert_eq!(b.console_code("ana", FinCommand::ProposeChart { chart: v1.clone() }), "authority");
    b.console("lena", FinCommand::ProposeChart { chart: v1.clone() }).unwrap();
    assert_eq!(b.console_code("olive", approve(id(&v1))), "authority");
    assert_eq!(b.console_code("lena", approve(id(&v1))), "separation");
    assert_eq!(b.console_code("liam", approve("chart:9:x".to_string())), "config");
    b.console("liam", approve(id(&v1))).unwrap();
    assert!(matches!(b.applied.last().unwrap().events[0].event, FinEvent::ChartActivated { version: 1, .. }));

    // Version 2 may add an account. It may not drop one, and version 1 cannot be proposed again.
    assert_eq!(b.console_code("olive", FinCommand::ProposeChart { chart: v1.clone() }), "chart");
    let mut dropped = v1.clone();
    dropped.version = 2;
    dropped.accounts.retain(|a| a.code != "6600");
    assert_eq!(b.console_code("olive", FinCommand::ProposeChart { chart: dropped }), "chart");
    let mut invalid = v1.clone();
    invalid.version = 0;
    assert_eq!(b.console_code("olive", FinCommand::ProposeChart { chart: invalid }), "chart");
    let grown = |code: &str| {
        let mut c = v1.clone();
        c.version = 2;
        c.content_hash = format!("hash-{code}");
        c.accounts.push(ChartAccount {
            code: code.to_string(),
            name: "Claims handling expenses".to_string(),
            class: AccountClass::Expense,
            control: false,
            roles: vec![],
        });
        c
    };
    let (first, second) = (grown("6700"), grown("6710"));
    b.console("olive", FinCommand::ProposeChart { chart: first.clone() }).unwrap();
    b.console("olive", FinCommand::ProposeChart { chart: second.clone() }).unwrap();
    b.console("lena", approve(id(&second))).unwrap();
    assert_eq!(b.console_code("lena", approve(id(&first))), "chart", "overtaken by the other version 2");
    assert!(b.state.chart.as_ref().unwrap().account("6710").is_some());

    let c1 = close_config();
    let cid = |c: &ic_finance::CloseConfig| format!("close:{}:{}", c.version, c.content_hash);
    let mut broken = c1.clone();
    broken.checks.retain(|c| c.id != "control_check");
    assert_eq!(b.console_code("olive", FinCommand::ProposeCloseConfig { config: broken }), "close_config");
    b.activate_close(c1.clone());
    assert!(matches!(
        b.applied.last().unwrap().events[0].event,
        FinEvent::CloseConfigActivated { version: 1, .. }
    ));
    let next = |version: u32, tag: &str| {
        let mut c = c1.clone();
        c.version = version;
        c.content_hash = tag.to_string();
        c
    };
    assert_eq!(
        b.console_code("olive", FinCommand::ProposeCloseConfig { config: next(3, "skip") }),
        "close_config"
    );
    b.console("olive", FinCommand::ProposeCloseConfig { config: next(2, "a") }).unwrap();
    b.console("olive", FinCommand::ProposeCloseConfig { config: next(2, "b") }).unwrap();
    b.console("lena", approve(cid(&next(2, "a")))).unwrap();
    assert_eq!(b.console_code("lena", approve(cid(&next(2, "b")))), "close_config");
    assert_eq!(b.state.close_config.as_ref().unwrap().content_hash, "a");
}

/// Purpose: an extract is registered once, only by the reporting service, only with the
/// Merkle root published for its date, and can never be replaced. A period's valuation
/// extract check is red until both required extracts exist and reconcile.
/// Why it matters: the actuary's reserves and the regulator's returns are calculated from
/// these files. An extract that could be swapped after the fact, or cut at a point not tied
/// to the signed ledger, would break the chain from a reported number back to the entries.
#[test]
fn an_extract_is_frozen_and_tied_to_the_published_root() {
    let mut w = world(d(2027, 12, 3));
    let good = w.extract("in_force", nov());
    let register = |extract| FinCommand::RegisterExtract { extract };
    let b = &mut w.books;
    assert_eq!(b.code("commission", None, register(good.clone())), "caller");
    let mut unanchored = good.clone();
    unanchored.merkle_root = Hash32([3; 32]);
    assert_eq!(b.code("reporting", None, register(unanchored)), "extract_anchor");
    let mut nowhere = good.clone();
    nowhere.location.clear();
    assert_eq!(b.code("reporting", None, register(nowhere)), "extract");

    b.send("reporting", None, register(good.clone())).unwrap();
    assert_eq!(
        b.send("reporting", None, register(good.clone())),
        Ok(()),
        "the same registration twice is harmless"
    );
    let mut swapped = good.clone();
    swapped.content_hash = Hash32([8; 32]);
    assert_eq!(b.code("reporting", None, register(swapped)), "extract_conflict");
    assert_eq!(b.state.extracts["in_force-2027-11"], good);

    assert!(
        matches!(status(&w, nov(), "valuation_extracts"), CheckStatus::Red { items } if items == vec!["no movement extract has been cut as at 2027-11-30".to_string()])
    );
    let mut movement = w.extract("movement", nov());
    movement.reconciled = false;
    w.books.send("reporting", None, register(movement)).unwrap();
    assert!(
        matches!(status(&w, nov(), "valuation_extracts"), CheckStatus::Red { items } if items == vec!["extract movement-2027-11 does not reconcile opening to closing".to_string()])
    );
    let views = w.books.state.dashboard(nov(), d(2027, 12, 3)).extracts;
    assert_eq!(
        views.iter().map(|e| (e.kind.as_str(), e.reconciled)).collect::<Vec<_>>(),
        vec![("in_force", true), ("movement", false)]
    );
}
