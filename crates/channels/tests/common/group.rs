//! The group service for the portal's tests. It keeps the group module's
//! rules the portal relies on: a scheme is known by its own ID and names
//! its policy, a file is quarantined on upload and has a preview, only an
//! uploaded file can be approved or withdrawn, the employer portal sends
//! only its own commands, and a key sent twice is one command.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use ic_channels::group::{Answered, Group, CALLER};
use serde_json::{json, Value};

type Asked<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Answered, String>> + Send + 'a>>;

#[derive(Default)]
pub struct GroupDouble {
    /// Scheme ID to its policy.
    pub schemes: BTreeMap<String, String>,
    /// (scheme, file) to who uploaded it, its size and its status.
    pub files: Mutex<BTreeMap<(String, String), (String, usize, String)>>,
    /// Every command, as (key, actor, command).
    pub commands: Mutex<Vec<(String, String, Value)>>,
    pub down: AtomicBool,
}

impl GroupDouble {
    pub fn with(scheme: &str, policy: &str) -> Self {
        GroupDouble {
            schemes: [(scheme.to_string(), policy.to_string())].into(),
            ..GroupDouble::default()
        }
    }

    fn answer(&self, question: &Value) -> Answered {
        if question["ask"] == "scheme_of" {
            let on: Vec<&String> = self.schemes.iter().filter(|(_, p)| question["policy"] == p.as_str()).map(|(s, _)| s).collect();
            return (200, json!({ "schemes": on }));
        }
        let scheme = question["scheme"].as_str().unwrap_or_default().to_string();
        let Some(policy) = self.schemes.get(&scheme) else {
            return (404, json!({ "code": "not_found", "detail": format!("scheme {scheme} is not registered") }));
        };
        match question["ask"].as_str() {
            Some("scheme") => (200, json!({ "scheme": scheme, "policy": policy, "members": 2, "outstanding_minor": 0 })),
            Some("invoices") => (200, json!({ "invoices": [{ "invoice": "inv-1", "total_minor": 12_000 }] })),
            Some("file") => {
                let file = question["file"].as_str().unwrap_or_default().to_string();
                match self.files.lock().unwrap().get(&(scheme, file.clone())) {
                    Some((_, _, status)) => (200, json!({ "file": file, "status": status, "summary": { "joiners": 1 } })),
                    None => (404, json!({ "code": "not_found" })),
                }
            }
            // E001 is a member on cover with cover above the automatic amount
            // in place. The service would never send a loading; this one does,
            // so that the portal is seen to keep it from the employer.
            Some("member") if question["employee"] == "E001" => (
                200,
                json!({ "scheme": scheme, "employee": "E001", "category": "staff", "salary_minor": 80_000_00, "billing_group": "akl",
                    "joined": "2027-11-01", "in_force": true, "loading_percent": 50, "party": "party-m1",
                    "covers": [{ "cover": "group-life", "cover_type": "death", "formula_minor": 240_000_00, "automatic_minor": 200_000_00, "exclusion": "x" }],
                    "above_automatic": [{ "cover": "group-life", "asked_minor": 40_000_00, "state": "in_place", "in_place_minor": 40_000_00, "from": "2027-12-01", "reason": "r" }] }),
            ),
            Some("member") => (404, json!({ "code": "not_found" })),
            _ => (400, json!({ "code": "invalid_question" })),
        }
    }

    fn apply(&self, key: &str, actor: &str, command: &Value) -> Answered {
        let mut seen = self.commands.lock().unwrap();
        if seen.iter().any(|(k, _, _)| k == key) {
            return (200, json!({ "applied": true, "duplicate": true }));
        }
        let Some((name, body)) = command.as_object().and_then(|o| o.iter().next()) else {
            return (400, json!({ "code": "invalid_command" }));
        };
        let at = (
            body["scheme"].as_str().unwrap_or_default().to_string(),
            body["file"].as_str().unwrap_or_default().to_string(),
        );
        let mut files = self.files.lock().unwrap();
        let status = match name.as_str() {
            "approve_file" => "approved",
            "withdraw_file" => "withdrawn",
            "declare_pool" => "",
            _ => return (403, json!({ "code": "caller_not_allowed" })),
        };
        if !status.is_empty() {
            match files.get_mut(&at) {
                Some(f) => f.2 = status.to_string(),
                None => return (422, json!({ "code": "unknown_file" })),
            }
        }
        seen.push((key.to_string(), actor.to_string(), command.clone()));
        (200, json!({ "applied": true, "duplicate": false }))
    }
}

impl Group for GroupDouble {
    fn upload(&self, scheme: &str, file: &str, actor: &str, bytes: Vec<u8>) -> Asked<'_> {
        let got = if self.down.load(Ordering::SeqCst) {
            Err("connection refused".to_string())
        } else {
            self.files.lock().unwrap().insert(
                (scheme.to_string(), file.to_string()),
                (actor.to_string(), bytes.len(), "quarantined".to_string()),
            );
            Ok((202, json!({ "scheme": scheme, "file": file, "status": "quarantined" })))
        };
        Box::pin(async move { got })
    }

    fn command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        let got = if self.down.load(Ordering::SeqCst) {
            Err("connection refused".to_string())
        } else {
            // The group service scopes a caller's key by the caller's name.
            Ok(self.apply(&format!("api:{CALLER}:{key}"), actor, &command))
        };
        Box::pin(async move { got })
    }

    fn ask(&self, question: Value) -> Asked<'_> {
        let got = if self.down.load(Ordering::SeqCst) {
            Err("connection refused".to_string())
        } else {
            Ok(self.answer(&question))
        };
        Box::pin(async move { got })
    }
}
