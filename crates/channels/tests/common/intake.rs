//! The underwriting intake for the portal's tests: it says what to sign as
//! the real one does (the step's domain, the proposal, the life and the
//! hash, joined by zero bytes), checks a P-256 signature it is given, and
//! notes what it received.

use std::sync::Mutex;

use base64::Engine;
use ic_channels::disclosures::{Answered, Intake};
use serde_json::{json, Value};

type Asked<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Answered, String>> + Send + 'a>>;

#[derive(Default)]
pub struct IntakeDouble {
    pub received: Mutex<Vec<Value>>,
}

/// The exact bytes a life signs for these answers.
pub fn message(request: &Value) -> Vec<u8> {
    let mut m = format!("ic.underwriting.{}.v1", request["step"].as_str().unwrap()).into_bytes();
    for part in [request["proposal"].as_str().unwrap(), request["life"].as_str().unwrap(), "hash-1"] {
        m.push(0);
        m.extend_from_slice(part.as_bytes());
    }
    m
}

impl Intake for IntakeDouble {
    fn to_sign(&self, body: Value) -> Asked<'_> {
        self.received.lock().unwrap().push(body.clone());
        let message = base64::engine::general_purpose::STANDARD.encode(message(&body));
        Box::pin(async move { Ok((200, json!({ "hash": "hash-1", "message": message }))) })
    }

    fn submit(&self, body: Value) -> Asked<'_> {
        self.received.lock().unwrap().push(body.clone());
        let s = &body["signature"];
        let good = s["alg"] == "ecdsa-p256"
            && ic_channels::identity::verify(
                s["public_key"].as_str().unwrap_or_default(),
                &message(&body),
                s["signature"].as_str().unwrap_or_default(),
            );
        let answer = if good {
            (
                200,
                json!({ "hash": "hash-1", "vault_ref": "clinical://party/disclosure/hash-1", "duplicate": false }),
            )
        } else {
            (400, json!({ "title": "bad_signature" }))
        };
        Box::pin(async move { Ok(answer) })
    }
}
