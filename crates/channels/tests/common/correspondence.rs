//! The documents module for the documents console's tests: its own state
//! machine, set up as its own tests set it up (a wording, schedule, renewal
//! and lapse notice and receipt active), plus one draft letter template
//! written by `author`, behind the same JSON a node serves.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ic_channels::forward::{Source, REFUSED};
use ic_documents::command::{Command, Envelope};
use ic_documents::DocumentKind;
use ic_log::StateMachine;
use serde_json::Value;

#[path = "../../../documents/tests/support/mod.rs"]
pub mod kit;

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// The documents module's own code, asked and told as a node would be.
pub struct DocumentsModule(pub Mutex<(kit::Kit, u64, Vec<Envelope>)>);

impl DocumentsModule {
    /// Every command the console sent, as the module received it.
    pub fn received(&self) -> Vec<Envelope> {
        self.0.lock().unwrap().2.clone()
    }
}

pub fn seeded() -> DocumentsModule {
    let mut k = kit::Kit::ready();
    let mut letter = kit::template("letter", DocumentKind::GeneralCorrespondence, "Dear {{name}}.");
    letter.products.clear();
    k.ok("config", "author", Command::SubmitTemplate(Box::new(letter)));
    DocumentsModule(Mutex::new((k, 1_000, vec![])))
}

impl Source for DocumentsModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_documents::module::answer(&self.0.lock().unwrap().0.state, question).map_err(|e| format!("{REFUSED}{e}"));
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
