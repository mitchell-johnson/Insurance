//! The commission module for the portal's tests: its own state machine,
//! open on a business date, answering and applying as it does on the nodes.
//! Nothing is matched as a replacement here, so a decision that passes its
//! authority check is refused for the layer, which shows how far it got.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ic_channels::forward::{Source, REFUSED};
use ic_core::event::{Event, EventRecord};
use ic_distribution::state::{Actor, Envelope};
use ic_distribution::{Command, DistributionState};
use ic_ledger::Hash32;
use serde_json::Value;

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

pub struct CommissionModule(pub Mutex<(DistributionState, u64)>);

/// Open on the fixture's business date.
pub fn seeded() -> CommissionModule {
    let mut state = DistributionState::default();
    let opened = EventRecord {
        id: "0-1".into(),
        partition: 0,
        seq: 1,
        business_date: super::open(),
        log_index: 1,
        traceparent: None,
        event: Event::BusinessDateAdvanced {
            from: None,
            to: super::open(),
            closing_head: Hash32::ZERO,
        },
    };
    let applied = state.apply_envelope(
        1,
        Envelope {
            key: "open".into(),
            actor: Actor::Module("event-relay".into()),
            command: Command::Ingest(Box::new(opened)),
        },
    );
    assert_eq!(applied.result, Ok(()), "the business date opens");
    CommissionModule(Mutex::new((state, 1)))
}

impl Source for CommissionModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_distribution::host::answer(&self.0.lock().unwrap().0, question).map_err(|e| format!("{REFUSED}{e}"));
        Box::pin(async move { answer })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        let applied = (|| {
            let envelope: Envelope = serde_json::from_value(envelope).map_err(|e| format!("{REFUSED}not a command of this module: {e}"))?;
            let mut held = self.0.lock().unwrap();
            held.1 += 1;
            let index = held.1;
            serde_json::to_value(held.0.apply_envelope(index, envelope)).map_err(|e| e.to_string())
        })();
        Box::pin(async move { applied })
    }
}
