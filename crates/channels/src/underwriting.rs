//! The underwriting console: how underwriters reach the underwriting module
//! (`underwriting` on the nodes, `docs/underwriting.md`).
//!
//! That module keeps proposals, cases and their decisions, and checks each
//! underwriter's authority profile itself: grade, accreditation, limits, a
//! conflict of interest. It takes people's commands only from the caller
//! `underwriting-console`, and believes the console about who the person
//! is, so every field that names the person acting is set here to whoever
//! signed in, whatever the page sent. The underwriter ID in that module's
//! profiles is the workforce directory ID.
//!
//! Reading a life's disclosures back goes through the underwriting intake
//! (`POST /v1/disclosures/open`), which has that module allow it and record
//! the access before any record is opened.
//!
//! A non-disclosure remedy needs two seniors. The first prepares it here,
//! a second signs it (`countersign`), and only then is it sent, with the
//! two named as `decided_by` and `countersigned_by`.

use std::collections::BTreeMap;

use chrono::DateTime;
use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The underwriting module's name on the nodes.
pub const MODULE: &str = "underwriting";
/// The caller that module takes people's commands from.
pub const CALLER: &str = "underwriting-console";
/// The caller a life's own consents come from, through the app.
pub const APP_CALLER: &str = "customer-app";

/// Staff actions that open the console.
pub mod actions {
    /// Work cases: take, move, ask for evidence and opinions, decide.
    pub const WORK: &str = "underwriting.work";
    /// A senior: non-disclosure remedies (two of them) and peer review.
    pub const SENIOR: &str = "underwriting.senior";
}

/// The commands a person sends alone, and the field in each that names
/// the person acting.
const DIRECT: [(&str, &str); 17] = [
    ("declare_conflict", "underwriter"),
    ("take_case", "underwriter"),
    ("hand_over", "by"),
    ("move_case", "underwriter"),
    ("request_evidence", "underwriter"),
    ("ask_opinion", "underwriter"),
    ("record_opinion", "of"),
    ("record_interview", "underwriter"),
    ("propose_decision", "underwriter"),
    ("countersign", "underwriter"),
    ("release_to_reinsurer", "underwriter"),
    ("retry_issue", "underwriter"),
    ("request_delegate_file", "underwriter"),
    ("draw_peer_review_sample", ""),
    ("record_peer_finding", "reviewer"),
    ("schedule_interview", "booked_by"),
    ("requeue_effect", "by"),
];

/// Commands that wait for a second senior.
pub const COUNTERSIGNED: [&str; 1] = ["record_non_disclosure_outcome"];

/// Commands only a senior sends.
const SENIOR_ONLY: [&str; 3] = ["draw_peer_review_sample", "record_peer_finding", "record_non_disclosure_outcome"];

/// The questions the console may put, with their text fields.
const QUESTIONS: [(&str, &[&str]); 8] = [
    ("proposal", &["proposal"]),
    ("disclosure_basis", &["proposal"]),
    ("queue", &["queue"]),
    ("workload", &[]),
    ("report", &["from", "to"]),
    ("exclusions", &["on"]),
    ("reasons", &["on"]),
    ("parked", &[]),
];

/// Whether a member of staff may open the console, and as what.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let senior = staff.holds_action(party, actions::SENIOR, at);
    json!({ "staff": party, "view": senior || staff.holds_action(party, actions::WORK, at), "senior": senior })
}

/// The time of `at` as that module writes times.
fn when(at: At) -> String {
    DateTime::from_timestamp(at, 0).unwrap_or_default().to_rfc3339()
}

/// A question for the underwriting module. A queue is read as of now; a
/// workload is the signed-in person's own.
pub fn question(name: &str, fields: &BTreeMap<String, String>, staff: &PartyId, at: At) -> Result<Value, String> {
    let (_, wanted) = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    let mut out = Map::new();
    out.insert("query".to_string(), json!(name));
    for field in *wanted {
        let value = fields
            .get(*field)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} needs {field}"))?;
        out.insert((*field).to_string(), json!(value));
    }
    match name {
        "queue" => {
            out.insert("now".to_string(), json!(when(at)));
        }
        "workload" => {
            out.insert("underwriter".to_string(), json!(staff));
        }
        _ => {}
    }
    Ok(Value::Object(out))
}

/// A command as it will be sent: the person acting set to whoever signed
/// in. Returns it with its name and whether it waits for a second senior.
pub fn command(sent: &Value, staff: &PartyId, senior: bool) -> Result<(Value, String, bool), String> {
    let name = crate::countersign::command_name(sent).ok_or("a command is one name with its fields")?;
    let Some(fields) = sent[name].as_object() else {
        return Err("a command's fields are an object".to_string());
    };
    if SENIOR_ONLY.contains(&name) && !senior {
        return Err(format!("{name} is for a senior underwriter"));
    }
    let mut fields = fields.clone();
    if COUNTERSIGNED.contains(&name) {
        if fields.get("countersigned_by").is_some_and(|v| !v.is_null()) {
            return Err("the second senior is whoever signs it, not something to type".to_string());
        }
        fields.insert("decided_by".to_string(), json!(staff));
        fields.insert("countersigned_by".to_string(), Value::Null);
        return Ok((json!({ name: fields }), name.to_string(), true));
    }
    let (_, actor) = DIRECT
        .iter()
        .find(|(c, _)| *c == name)
        .ok_or_else(|| format!("{name} is not sent from the console"))?;
    if !actor.is_empty() {
        fields.insert((*actor).to_string(), json!(staff));
    }
    Ok((json!({ name: fields }), name.to_string(), false))
}

/// The envelope that carries a command to the underwriting module, made on
/// business date `today`.
pub fn envelope(key: &str, at: At, today: &str, command: Value) -> Value {
    json!({ "key": key, "caller": CALLER, "at": when(at), "today": today, "command": command })
}

/// The envelope that carries a life's own consent from the app, on the
/// New Zealand date.
pub fn app_envelope(key: &str, at: At, command: Value) -> Value {
    let today = crate::payments::nz(at).date().to_string();
    json!({ "key": key, "caller": APP_CALLER, "at": when(at), "today": today, "command": command })
}

/// A prepared non-disclosure remedy, signed by a second senior.
pub fn countersigned(command: &Value, signer: &PartyId) -> Value {
    let mut command = command.clone();
    if let Some(name) = crate::countersign::command_name(&command).map(str::to_string) {
        command[name]["countersigned_by"] = json!(signer);
    }
    command
}

/// The questions this console forwards, by name, for the MCP tool that asks them.
pub fn question_names() -> Vec<&'static str> {
    QUESTIONS.iter().map(|q| q.0).collect()
}

/// The commands this console forwards, by name, for the MCP tool that sends them.
pub fn command_names() -> Vec<&'static str> {
    DIRECT.iter().map(|c| c.0).chain(COUNTERSIGNED).collect()
}
