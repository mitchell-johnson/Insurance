//! The reinsurance exceptions console: what staff see and do about the
//! reinsurance module's exceptions.
//!
//! That module (`reinsurance` on the nodes) is the source of truth: an
//! exception opens when something fails and closes itself when the cause
//! is cured. It takes people's commands from the caller `reinsurance-ops`
//! and keeps no actor beyond the `approved_by` of an abandonment, so who
//! may act, and that abandoning takes two different people, is enforced
//! here: an abandonment is prepared by one person, kept in the channel
//! state (`countersign`), and sent only when a second person signs it, with
//! the signer as `approved_by`.

use std::collections::BTreeMap;

use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The reinsurance module's name on the nodes.
pub const MODULE: &str = "reinsurance";
/// The caller that module takes people's commands from.
pub const CALLER: &str = "reinsurance-ops";

/// Staff actions for the console.
pub mod actions {
    /// Read the exceptions, notices, abandonments and the month-end check.
    pub const VIEW: &str = "reinsurance.view";
    /// Retry a notice, record a settlement, record that a reinsurer was told,
    /// and prepare an abandonment.
    pub const OPERATE: &str = "reinsurance.operate";
    /// Sign an abandonment someone else prepared.
    pub const APPROVE: &str = "reinsurance.approve";
}

/// Commands one person sends.
pub const DIRECT: [&str; 3] = ["retry_notice", "record_settlement", "facultative_reinsurer_told"];
/// Commands that need a second person: they leave the books apart, or a
/// reinsurer untold.
pub const COUNTERSIGNED: [&str; 2] = ["abandon_notice", "abandon_instruction"];

/// The console's questions: name, required text fields, required whole-number fields.
const QUESTIONS: [(&str, &[&str], &[&str]); 7] = [
    ("exceptions", &[], &[]),
    ("notices", &[], &[]),
    ("abandoned", &[], &[]),
    ("abandoned_notices", &[], &[]),
    ("sweep_check", &["period_end"], &[]),
    ("statement", &[], &["number"]),
    ("notice_retries", &[], &["seq"]),
];

/// What this person may do in the console.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let operate = staff.holds_action(party, actions::OPERATE, at);
    let approve = staff.holds_action(party, actions::APPROVE, at);
    json!({
        "staff": party,
        "view": operate || approve || staff.holds_action(party, actions::VIEW, at),
        "operate": operate,
        "approve": approve,
    })
}

/// A question to the reinsurance module, from its name and the page's fields.
pub fn question(name: &str, fields: &BTreeMap<String, String>) -> Result<Value, String> {
    let (_, text, numbers) = QUESTIONS
        .iter()
        .find(|(q, _, _)| *q == name)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    let given = |field: &str| fields.get(field).map(|v| v.trim()).filter(|v| !v.is_empty());
    // Unit questions are a bare string; staff notices are asked for their audience.
    if name == "notices" {
        return Ok(json!({ "notices": { "audience": "staff", "limit": 100 } }));
    }
    if text.is_empty() && numbers.is_empty() {
        return Ok(json!(name));
    }
    let mut inner = Map::new();
    for field in *text {
        inner.insert((*field).to_string(), json!(given(field).ok_or_else(|| format!("{name} needs {field}"))?));
    }
    for field in *numbers {
        let value = given(field).ok_or_else(|| format!("{name} needs {field}"))?;
        let number: u64 = value.parse().map_err(|_| format!("{field} is a whole number"))?;
        inner.insert((*field).to_string(), json!(number));
    }
    Ok(json!({ name: inner }))
}

/// Check a command a person sends alone, or prepares for a second person.
/// Returns its name and whether it needs a second person. `approved_by` is
/// never the page's to give.
pub fn check(command: &Value) -> Result<(&str, bool), String> {
    let name = crate::countersign::command_name(command).ok_or("a command is one name with its fields")?;
    if !command[name].is_object() {
        return Err("a command's fields are an object".to_string());
    }
    if command[name].get("approved_by").is_some() {
        return Err("approved_by is whoever signs it, not something to type".to_string());
    }
    if DIRECT.contains(&name) {
        Ok((name, false))
    } else if COUNTERSIGNED.contains(&name) {
        let reason = command[name]["reason"].as_str().unwrap_or_default();
        if reason.trim().is_empty() {
            return Err("say why it is being abandoned".to_string());
        }
        Ok((name, true))
    } else {
        Err(format!("{name} is not sent from the console"))
    }
}

/// The envelope that carries a command to the module. A countersigned
/// command carries its signer as `approved_by`.
pub fn envelope(key: &str, on: &str, mut command: Value, signer: Option<&PartyId>) -> Value {
    if let (Some(signer), Some(name)) = (signer, crate::countersign::command_name(&command).map(str::to_string)) {
        command[name]["approved_by"] = json!(signer);
    }
    json!({ "key": key, "caller": CALLER, "on": on, "command": command })
}
