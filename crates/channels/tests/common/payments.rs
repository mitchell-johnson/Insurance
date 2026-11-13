//! The payments module for the payments console's tests: its own state
//! machine, configured as its own tests configure it (two banks, accounts
//! holding ten million, Monday 15 November 2027), behind the same JSON a
//! node serves. Seeded with one payee that has passed every control.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ic_channels::forward::{Source, REFUSED};
use ic_log::StateMachine;
use ic_payments::model::PayeeKind;
use ic_payments::Envelope;
use serde_json::Value;

#[path = "../../../payments/tests/common/mod.rs"]
pub mod desk;

pub use desk::Desk;

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// The payments module's own code, asked and told as a node would be.
pub struct PaymentsModule(pub Mutex<(Desk, u64, Vec<Envelope>)>);

impl PaymentsModule {
    /// Every command the console sent, as the module received it.
    pub fn received(&self) -> Vec<Envelope> {
        self.0.lock().unwrap().2.clone()
    }
}

pub fn seeded() -> PaymentsModule {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    PaymentsModule(Mutex::new((desk, 1_000, vec![])))
}

impl Source for PaymentsModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_payments::query::answer(&self.0.lock().unwrap().0.state, question).map_err(|e| format!("{REFUSED}{e}"));
        Box::pin(async move { answer })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        let applied = (|| {
            let envelope: Envelope = serde_json::from_value(envelope).map_err(|e| format!("{REFUSED}not a command of this module: {e}"))?;
            let mut held = self.0.lock().unwrap();
            held.1 += 1;
            let index = held.1;
            held.2.push(envelope.clone());
            serde_json::to_value(held.0.state.apply(index, envelope)).map_err(|e| e.to_string())
        })();
        Box::pin(async move { applied })
    }
}
