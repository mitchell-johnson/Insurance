//! The collections module for the collections console's tests: its own
//! state machine, set up as its own tests set it up (one product with the
//! design notes' sequence), with one policy 21 days in arrears on Sunday
//! 14 November 2027 and a call task open for it, behind the same JSON a
//! node serves.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ic_channels::forward::{Source, REFUSED};
use ic_collections::Envelope;
use ic_log::StateMachine;
use serde_json::Value;

#[path = "../../../collections/tests/common/mod.rs"]
pub mod world;

pub use world::World;

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// The collections module's own code, asked and told as a node would be.
pub struct CollectionsModule(pub Mutex<(World, u64, Vec<Envelope>)>);

impl CollectionsModule {
    /// Every command the console sent, as the module received it.
    pub fn received(&self) -> Vec<Envelope> {
        self.0.lock().unwrap().2.clone()
    }
}

pub fn seeded() -> CollectionsModule {
    let mut w = World::new();
    w.today = world::d(2027, 10, 20);
    w.issue("pol-c1", "party-c1", 10_000);
    w.overdue("pol-c1", world::d(2027, 10, 24));
    w.close_through(world::d(2027, 11, 14));
    CollectionsModule(Mutex::new((w, 1_000, vec![])))
}

impl Source for CollectionsModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_collections::query::answer(&self.0.lock().unwrap().0.state, question).map_err(|e| format!("{REFUSED}{e}"));
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
