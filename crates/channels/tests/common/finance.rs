//! The finance ledger for the console's tests: the real state machine
//! behind the same JSON a node serves, with the shipped chart and close
//! configuration in force.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ic_channels::forward::{Source, REFUSED};
use ic_finance::{Chart, CloseConfig, FinEnvelope, FinanceState};
use serde_json::{json, Value};

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// The finance ledger's own code, asked and told as a node would be.
pub struct FinanceModule(pub Mutex<(FinanceState, u64)>);

impl FinanceModule {
    fn apply(&self, envelope: Value) -> Result<Value, String> {
        let envelope: FinEnvelope = serde_json::from_value(envelope).map_err(|e| format!("{REFUSED}not a command of this module: {e}"))?;
        let mut held = self.0.lock().unwrap();
        held.1 += 1;
        let index = held.1;
        serde_json::to_value(held.0.apply_envelope(index, envelope)).map_err(|e| e.to_string())
    }

    fn setup(&self, key: &str, caller: &str, actor: Option<(&str, &str)>, command: Value) {
        let mut envelope = json!({ "key": key, "caller": caller, "command": command });
        if let Some((who, role)) = actor {
            envelope["actor"] = json!(who);
            envelope["roles"] = json!([role]);
        }
        let applied = self.apply(envelope).unwrap();
        assert!(applied["result"].get("Err").is_none(), "{key}: {}", applied["result"]);
    }
}

/// One partition, a March year end, the shipped chart and close
/// configuration proposed and approved by two set-up people, and one day of
/// the core's events from 15 November 2027.
pub fn seeded() -> FinanceModule {
    let module = FinanceModule(Mutex::new((FinanceState::new(), 0)));
    module.setup("configure", "operator", None, json!({ "configure": { "partitions": 1, "year_end_month": 3 } }));
    let chart = Chart::load("chart.star", include_str!("../../../../config/finance/chart.star")).unwrap();
    let close = CloseConfig::load("close.star", include_str!("../../../../config/finance/close.star")).unwrap();
    let ids = [
        format!("chart:{}:{}", chart.version, chart.content_hash),
        format!("close:{}:{}", close.version, close.content_hash),
    ];
    let proposals = [
        json!({ "propose_chart": { "chart": chart } }),
        json!({ "propose_close_config": { "config": close } }),
    ];
    for (n, (id, proposal)) in ids.into_iter().zip(proposals).enumerate() {
        module.setup(&format!("propose-{n}"), "finance-console", Some(("setup-1", "finance_operator")), proposal);
        module.setup(
            &format!("approve-{n}"),
            "finance-console",
            Some(("setup-2", "finance_lead")),
            json!({ "approve_config": { "id": id } }),
        );
    }
    // A day of the core's events, so November 2027 is a period the books have started.
    let mut core = ic_sim::Cluster::in_memory(1, 11);
    core.open(chrono::NaiveDate::from_ymd_opt(2027, 11, 15).unwrap()).unwrap();
    for activation in ic_sim::config::shipped_activations().unwrap() {
        core.activate_everywhere(activation).unwrap();
    }
    core.end_of_day().unwrap();
    for (n, events) in core.take_events().chunks(1_000).enumerate() {
        module.setup(&format!("feed-{n}"), "feed", None, json!({ "ingest": { "partition": 0, "events": events } }));
    }
    module
}

impl Source for FinanceModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_finance::host::answer(&self.0.lock().unwrap().0, question).map_err(|e| format!("{REFUSED}{e}"));
        Box::pin(async move { answer })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        let output = self.apply(envelope);
        Box::pin(async move { output })
    }
}
