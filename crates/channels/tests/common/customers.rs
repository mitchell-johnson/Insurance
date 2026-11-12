//! The customer master for the portal's tests: a stand-in that keeps one
//! record per party, notes every call the portal makes, and answers as the
//! master's HTTP service does. A version mismatch is a 412, and `down`
//! makes every call fail as an unreachable master would.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use ic_channels::customers::{Answered, Master};
use ic_core::PartyId;
use serde_json::{json, Value};

type Asked<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Answered, String>> + Send + 'a>>;

/// One call as the master saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub what: &'static str,
    pub party: String,
    pub version: Option<String>,
    pub key: Option<String>,
    pub body: Option<Value>,
}

#[derive(Default)]
pub struct MasterDouble {
    pub calls: Mutex<Vec<Call>>,
    records: Mutex<BTreeMap<String, (u64, Value)>>,
    pub down: AtomicBool,
}

impl MasterDouble {
    pub fn with(party: &str, record: Value) -> Self {
        let double = Self::default();
        double.records.lock().unwrap().insert(party.to_string(), (1, record));
        double
    }

    fn note(&self, call: Call) -> Result<(), String> {
        self.calls.lock().unwrap().push(call);
        if self.down.load(Ordering::SeqCst) {
            return Err("connection refused".to_string());
        }
        Ok(())
    }

    fn found(&self, party: &PartyId) -> Option<(u64, Value)> {
        self.records.lock().unwrap().get(&party.0).cloned()
    }
}

fn missing() -> Answered {
    (404, json!({ "title": "unknown_party" }))
}

impl Master for MasterDouble {
    fn record(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "record",
                party: party.0.clone(),
                version: None,
                key: None,
                body: None,
            })?;
            Ok(self
                .found(&party)
                .map_or_else(missing, |(v, r)| (200, json!({ "version": v.to_string(), "record": r }))))
        })
    }

    fn command(&self, party: &PartyId, version: Option<String>, key: String, body: Value) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "command",
                party: party.0.clone(),
                version: version.clone(),
                key: Some(key),
                body: Some(body.clone()),
            })?;
            let mut records = self.records.lock().unwrap();
            let Some((current, record)) = records.get_mut(&party.0) else {
                return Ok(missing());
            };
            if version.is_some_and(|v| v != current.to_string()) {
                return Ok((412, json!({ "title": "version_mismatch" })));
            }
            if let Some(fields) = body.get("fields").and_then(Value::as_object) {
                for (name, value) in fields {
                    record[name] = value.clone();
                }
            }
            *current += 1;
            Ok((200, json!({ "version": current.to_string() })))
        })
    }

    fn export(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "export",
                party: party.0.clone(),
                version: None,
                key: None,
                body: None,
            })?;
            Ok(self
                .found(&party)
                .map_or_else(missing, |(_, r)| (200, json!({ "party": party.0, "fields": r, "history": [] }))))
        })
    }
}
