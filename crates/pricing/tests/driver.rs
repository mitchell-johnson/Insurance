//! The driver's pass against in-process stand-ins for the module, the core
//! and storage: what it proposes, and how it behaves when a part is down.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ic_ledger::PartitionId;
use ic_money::FundId;
use ic_pricing::command::*;
use ic_pricing::driver::{Core, Driver, Inbox, Module, Outbound, Scan};
use ic_pricing::host::answer;
use ic_pricing::state::PricingState;
use serde_json::Value;

#[derive(Default)]
struct FakeModule {
    state: Mutex<PricingState>,
    down: AtomicBool,
}

impl Module for &FakeModule {
    async fn propose(&self, envelope: &Envelope) -> Result<Output, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("module unavailable".into());
        }
        Ok(self.state.lock().unwrap().apply_envelope(envelope.clone()))
    }
    async fn ask(&self, question: &Value) -> Result<Value, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("module unavailable".into());
        }
        answer(&self.state.lock().unwrap(), question.clone())
    }
}

#[derive(Default)]
struct FakeCore {
    figures: Mutex<BTreeMap<PartitionId, (i64, Option<NaiveDate>)>>,
    down: AtomicBool,
}

impl Core for &FakeCore {
    fn partitions(&self) -> PartitionId {
        2
    }
    async fn units_on_issue(&self, partition: PartitionId, _fund: &FundId) -> Result<(i64, Option<NaiveDate>), String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("partition unavailable".into());
        }
        Ok(self.figures.lock().unwrap().get(&partition).copied().unwrap_or((0, None)))
    }
}

#[derive(Default)]
struct FakeStore {
    files: Mutex<BTreeMap<String, (Scan, Vec<u8>)>>,
    written: Mutex<BTreeMap<String, Vec<u8>>>,
    down: AtomicBool,
}

impl FakeStore {
    fn drop_file(&self, name: &str, scan: Scan, body: &str) {
        self.files.lock().unwrap().insert(name.to_string(), (scan, body.as_bytes().to_vec()));
    }
    fn check(&self) -> Result<(), String> {
        if self.down.load(Ordering::SeqCst) {
            Err("storage unavailable".into())
        } else {
            Ok(())
        }
    }
}

impl Inbox for &FakeStore {
    async fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        self.check()?;
        Ok(self.files.lock().unwrap().keys().filter(|k| k.starts_with(prefix)).cloned().collect())
    }
    async fn scan(&self, name: &str) -> Result<Scan, String> {
        self.check()?;
        Ok(self.files.lock().unwrap()[name].0)
    }
    async fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        self.check()?;
        Ok(self.files.lock().unwrap().get(name).map(|f| f.1.clone()))
    }
}

impl Outbound for &FakeStore {
    async fn put_new(&self, name: &str, body: Vec<u8>) -> Result<(), String> {
        self.check()?;
        self.written.lock().unwrap().entry(name.to_string()).or_insert(body);
        Ok(())
    }
}

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 3, 6, 0, 0).unwrap()
}

fn fund() -> ic_core::invest::FundDefinition {
    let pricing = [
        ("previous_move", "0.05"),
        ("stale_asset_days", "3"),
        ("unexplained_move", "0.005"),
        ("error_threshold", "0.003"),
        ("launch_price", "1.0000"),
        ("pricing_lag_days", "1"),
    ];
    ic_core::invest::FundDefinition {
        id: FundId("growth".into()),
        version: 1,
        content_hash: "growth-v1".into(),
        name: "Growth".into(),
        currency: ic_money::Currency::NZD,
        cut_off: "15:00".into(),
        buy_spread_bps: 25,
        sell_spread_bps: 25,
        price_decimals: 4,
        unit_rounding: ic_money::RoundingMode::Down,
        non_pricing_days: Default::default(),
        open_to_new_money: true,
        pricing: pricing.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

fn setup(module: &FakeModule) {
    let e = Envelope {
        key: "fund".into(),
        caller: CALLER_CONFIG.into(),
        actor: "config".into(),
        roles: vec![],
        at: at(),
        today: None,
        command: Command::ActivateFund(Box::new(fund())),
    };
    assert_eq!(module.state.lock().unwrap().apply_envelope(e), Output::Done);
}

const GOOD: &str = "H,acme,VAL-1\nV,growth,2026-11-03,0,0,0,0,,N\nT,1,0\n";

/// Purpose: a pass takes a clean file once, refuses an infected or
/// malformed one with the reason, leaves an unscanned one waiting, ignores
/// folders older than the lookback, and reports the core's units only when
/// they change.
/// Why it matters: the administrator's file is the only input to a price;
/// reading an unscanned file, or losing a malformed one without telling
/// anyone, would either expose the platform or leave a fund unpriced.
#[tokio::test]
async fn takes_each_file_once_and_reports_units_on_change() {
    let (module, core, store) = (FakeModule::default(), FakeCore::default(), FakeStore::default());
    setup(&module);
    store.drop_file("valuations/2026-11-03/good.csv", Scan::Clean, GOOD);
    store.drop_file("valuations/2026-11-02/bad.csv", Scan::Clean, "H,acme,VAL-2\n");
    store.drop_file("valuations/2026-11-02/virus.csv", Scan::Malicious, GOOD);
    store.drop_file("valuations/2026-11-03/later.csv", Scan::Pending, GOOD);
    store.drop_file("valuations/2026-10-01/old.csv", Scan::Clean, GOOD);
    let driver = Driver::new(&module, &core, &store, &store);
    driver.start(at()).await.unwrap();
    let report = driver.pass(at()).await;
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!((report.files_taken, report.files_refused, report.files_waiting), (1, 2, 1));
    assert_eq!(report.units_reported, 2);
    {
        let state = module.state.lock().unwrap();
        assert!(state.refused_files["valuations/2026-11-02/bad.csv"].contains("trailer"));
        assert!(state.refused_files["valuations/2026-11-02/virus.csv"].contains("malware"));
        assert_eq!(state.valuations["growth:2026-11-03"].len(), 1);
    }
    let again = driver.pass(at()).await;
    assert_eq!(
        (again.files_taken, again.files_refused, again.files_waiting, again.units_reported),
        (0, 0, 1, 0)
    );
    core.figures.lock().unwrap().insert(1, (5, None));
    assert_eq!(driver.pass(at()).await.units_reported, 1);
}

/// Purpose: when storage, the core or the module is down, the pass reports
/// the fault for that step and still runs the others; once the part is back
/// the next pass catches up.
/// Why it matters: the driver runs unattended; a fault that stopped the
/// whole pass, or that was retried without its key, would hold up prices or
/// propose a file twice.
#[tokio::test]
async fn a_part_that_is_down_is_reported_and_caught_up() {
    let (module, core, store) = (FakeModule::default(), FakeCore::default(), FakeStore::default());
    setup(&module);
    let driver = Driver::new(&module, &core, &store, &store);
    driver.start(at()).await.unwrap();
    store.drop_file("valuations/2026-11-03/good.csv", Scan::Clean, GOOD);

    store.down.store(true, Ordering::SeqCst);
    let report = driver.pass(at()).await;
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].starts_with("valuation intake"));
    assert_eq!(report.units_reported, 2, "the core's step still ran");
    store.down.store(false, Ordering::SeqCst);

    core.down.store(true, Ordering::SeqCst);
    let report = driver.pass(at()).await;
    assert_eq!(report.files_taken, 1);
    assert!(report.errors.iter().any(|e| e.starts_with("units on issue")), "{report:?}");
    core.down.store(false, Ordering::SeqCst);

    module.down.store(true, Ordering::SeqCst);
    let report = driver.pass(at()).await;
    assert_eq!(report.errors.len(), 3, "{report:?}");
    assert!(driver.start(at()).await.is_err());
    module.down.store(false, Ordering::SeqCst);
    assert!(driver.pass(at()).await.errors.is_empty());
}

/// Purpose: the module refusing what the driver proposes is reported as a
/// fault, naming the file or the fund, and a partition count the module
/// already holds differently stops the driver at start.
/// Why it matters: a refusal is the module saying something is wrong with
/// the world (an unknown fund, a different ledger); carrying on silently
/// would hide it from operations.
#[tokio::test]
async fn refusals_are_reported() {
    let (module, core, store) = (FakeModule::default(), FakeCore::default(), FakeStore::default());
    let driver = Driver::new(&module, &core, &store, &store);
    {
        let mut state = module.state.lock().unwrap();
        state.partitions = Some(3);
    }
    assert!(driver.start(at()).await.unwrap_err().contains("partitions_fixed"));

    let (module, core, store) = (FakeModule::default(), FakeCore::default(), FakeStore::default());
    let driver = Driver::new(&module, &core, &store, &store);
    driver.start(at()).await.unwrap();
    // A file whose valuations are for a fund the module does not know is
    // taken: the module records each valuation's refusal itself.
    store.drop_file("valuations/2026-11-03/x.csv", Scan::Clean, GOOD);
    let report = driver.pass(at()).await;
    assert_eq!(report.files_taken, 1, "{report:?}");
    // The core reports a partition the module does not have.
    setup(&module);
    module.state.lock().unwrap().partitions = Some(1);
    let report = driver.pass(at()).await;
    assert!(report.errors.iter().any(|e| e.contains("growth on partition 1")), "{report:?}");
}

/// Purpose: the driver raises one alert per late price and per refused
/// valuation file, not again on the next pass; a restarted driver raises
/// the late price again (it still stands) but not the file refused before
/// it started; a failing question is reported as an alerts fault.
/// Why it matters: a late price holds up every order of its date and a
/// refused file leaves a fund unpriced; operations only learn of either
/// from these alerts, and an alert repeated every thirty seconds would be
/// muted and then missed.
#[tokio::test]
async fn late_prices_and_refused_files_are_alerted_once() {
    let (module, core, store) = (FakeModule::default(), FakeCore::default(), FakeStore::default());
    setup(&module);
    store.drop_file("valuations/2026-11-03/bad.csv", Scan::Clean, "H,acme,VAL-2\n");
    let later = Utc.with_ymd_and_hms(2026, 11, 6, 6, 0, 0).unwrap();
    let driver = Driver::new(&module, &core, &store, &store);
    driver.start(at()).await.unwrap();
    let first = driver.pass(later).await;
    assert!(first.errors.is_empty(), "{first:?}");
    let alerts = answer(&module.state.lock().unwrap(), serde_json::json!({"q": "alerts", "as_at": "2026-11-06"})).unwrap();
    let kinds: Vec<&str> = alerts.as_array().unwrap().iter().map(|a| a["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"late_price") && kinds.contains(&"file_refused"), "{alerts}");
    assert_eq!(first.alerts_raised, kinds.len());
    assert_eq!(driver.pass(later).await.alerts_raised, 0, "raised once");

    let restarted = Driver::new(&module, &core, &store, &store);
    restarted.start(later).await.unwrap();
    let again = restarted.pass(later).await;
    let late = kinds.iter().filter(|k| **k == "late_price").count();
    assert_eq!(again.alerts_raised, late, "late prices again, the old file not");

    module.down.store(true, Ordering::SeqCst);
    let report = restarted.pass(later).await;
    assert!(report.errors.iter().any(|e| e.starts_with("alerts")), "{report:?}");
}
