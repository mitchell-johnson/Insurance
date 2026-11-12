//! The complaints module for the workbench's tests: the real state machine
//! behind the same JSON a node serves.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use chrono::DateTime;
use ic_channels::forward::{Source, REFUSED};
use ic_complaints::state::ComplaintsState;
use ic_log::StateMachine;
use serde_json::{json, Value};

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// The complaints module's own code, asked and told as a node would be.
pub struct ComplaintsModule(pub Mutex<(ComplaintsState, u64)>);

impl ComplaintsModule {
    fn apply(&self, envelope: Value) -> Result<Value, String> {
        let envelope: ic_complaints::command::Envelope = serde_json::from_value(envelope).map_err(|e| format!("{REFUSED}not a command of this module: {e}"))?;
        let mut held = self.0.lock().unwrap();
        held.1 += 1;
        let index = held.1;
        serde_json::to_value(held.0.apply(index, envelope)).map_err(|e| e.to_string())
    }
}

/// A complaints module with its timers in force: authority 1 may pay up to
/// 500.00 of goodwill and authority 3 up to 5,000.00.
pub fn seeded(at: i64) -> ComplaintsModule {
    let module = ComplaintsModule(Mutex::new((ComplaintsState::new(), 0)));
    let timers = json!({
        "version": 1, "acknowledge_days": 5, "update_every_days": 10, "decide_days": 20, "deadlock_days": 40, "warning_days": 2,
        "conduct_officer_after_days": 3, "complainant_response_days": 10, "vulnerable_extra_days": 10, "front_line_limit_minor": 10_000,
        "ex_gratia_limits_minor": { "1": 50_000, "3": 500_000 }, "regulator_notification_days": 10, "retain_closed_days": 100,
        "cause_codes": ["delay", "wrong_decision", "poor_explanation", "adviser_conduct"],
    });
    let when = DateTime::from_timestamp(at, 0).unwrap();
    let activated = module
        .apply(json!({ "key": "timers-1", "caller": "config", "actor": "config", "at": when, "command": { "activate_timers": timers } }))
        .unwrap();
    assert!(activated["result"].get("Err").is_none(), "{activated}");
    module
}

impl Source for ComplaintsModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_complaints::module::answer(&self.0.lock().unwrap().0, question).map_err(|e| format!("{REFUSED}{e}"));
        Box::pin(async move { answer })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        let output = self.apply(envelope);
        Box::pin(async move { output })
    }
}
