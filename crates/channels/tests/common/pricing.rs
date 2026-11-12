//! The unit pricing module for the console's tests: the real state machine
//! behind the same JSON a node serves, and one that cannot be reached.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate};
use ic_channels::forward::Source;
use ic_pricing::PricingState;
use serde_json::{json, Value};

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// The unit pricing module's own code, asked and told as a node would be.
pub struct PricingModule(pub Mutex<PricingState>);

impl PricingModule {
    /// Send a command as one of that module's own services.
    pub fn service(&self, key: &str, caller: &str, at: i64, command: Value) -> Value {
        let when = DateTime::from_timestamp(at, 0).unwrap();
        let envelope = json!({ "key": key, "caller": caller, "actor": caller, "roles": [], "at": when, "command": command });
        let output = self.apply(envelope).unwrap();
        assert!(output.get("refused").is_none(), "{key}: {output}");
        output
    }

    fn apply(&self, envelope: Value) -> Result<Value, String> {
        let envelope: ic_pricing::Envelope = serde_json::from_value(envelope).map_err(|e| format!("not a command of this module: {e}"))?;
        serde_json::to_value(self.0.lock().unwrap().apply_envelope(envelope)).map_err(|e| e.to_string())
    }

    /// The administrator's valuation of `growth` for a date: a million units.
    pub fn valuation(&self, date: NaiveDate, nav: &str, at: i64) {
        let valuation = json!({
            "fund": "growth", "pricing_date": date, "nav": nav, "units_on_issue_micro": UNITS, "applications": "0", "redemptions": "0",
            "benchmark_return": "0", "restated": false, "stale_assets": [],
        });
        let received = json!({ "receive_valuations": { "administrator": "acme", "file_id": format!("F-{date}-{nav}"), "valuations": [valuation] } });
        self.service(&format!("val-{date}-{nav}"), "unit-pricing-intake", at, received);
    }

    /// Every partition reports its units of `growth`, with every order up to `through` executed.
    pub fn core_units(&self, through: Option<NaiveDate>, at: i64) {
        for partition in 0..2 {
            let units = json!({ "record_core_units": { "fund": "growth", "partition": partition, "executed_through": through, "units_micro": UNITS / 2 } });
            self.service(&format!("units-{partition}-{through:?}"), "unit-pricing-intake", at, units);
        }
    }

    /// The core applied every publication waiting for it.
    pub fn deliver_all(&self, at: i64) {
        let waiting = ic_pricing::host::answer(&self.0.lock().unwrap(), json!({ "q": "outbox", "limit": 100 })).unwrap();
        for publication in waiting.as_array().unwrap() {
            let id = publication["id"].as_str().unwrap();
            let delivered = json!({ "record_delivery": { "publication": id, "delivery": "applied" } });
            self.service(&format!("delivered-{id}"), "core-outbox", at, delivered);
        }
    }
}

pub const UNITS: i64 = 1_000_000_000_000;

/// The first pricing date the seeded module has a valuation for.
pub fn first_day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2027, 11, 12).unwrap()
}

/// The shipped funds active on two partitions, with a valuation of `growth`
/// received for [`first_day`] and the core's units agreeing with it.
pub fn seeded(at: i64) -> PricingModule {
    let module = PricingModule(Mutex::new(PricingState::new()));
    let when = DateTime::from_timestamp(at, 0).unwrap();
    for fund in ic_sim::config::shipped_funds().unwrap() {
        let envelope = serde_json::to_value(ic_pricing::command::activation(fund, when)).unwrap();
        assert!(module.apply(envelope).unwrap().get("refused").is_none());
    }
    module.service("partitions", "unit-pricing-intake", at, json!({ "record_partitions": { "count": 2 } }));
    module.valuation(first_day(), "1010000", at);
    module.core_units(None, at);
    module.service(
        "unreadable",
        "unit-pricing-intake",
        at,
        json!({ "refuse_valuation_file": { "file_id": "valuations/bad.csv", "reason": "line 3: bad trailer" } }),
    );
    module
}

impl Source for PricingModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_pricing::host::answer(&self.0.lock().unwrap(), question);
        Box::pin(async move { answer })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        let output = self.apply(envelope);
        Box::pin(async move { output })
    }
}

/// A unit pricing module with no leader.
pub struct Down;

impl Source for Down {
    fn ask(&self, _question: Value) -> Asked<'_> {
        Box::pin(async { Err("no leader".to_string()) })
    }

    fn tell(&self, _envelope: Value) -> Asked<'_> {
        Box::pin(async { Err("no leader".to_string()) })
    }
}
