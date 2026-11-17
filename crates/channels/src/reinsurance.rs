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
pub const DIRECT: [&str; 18] = [
    "retry_notice",
    "record_settlement",
    "facultative_reinsurer_told",
    "resume_recoveries",
    "reoffer_settlement",
    // The reinsurer accounts: a statement is cut, agreed, held or released,
    // a listing issued again, profit commission reviewed, a settlement paid
    // some other way put through clearing or recorded as sent back.
    "cut_statement",
    "agree_statement",
    "hold_settlement",
    "release_hold",
    "issue_supplementary_listing",
    "review_profit_commission",
    "clear_settlement",
    "settlement_returned",
    // What a reinsurer said when its consent was needed before admission.
    "reinsurer_consent",
    // A facultative offer: the reinsurer's terms, or its decline, and a
    // controlled release of evidence to a reinsurer.
    "record_facultative_terms",
    "decline_facultative",
    "log_evidence_release",
    // A claim fact entered by hand, where the core's events did not carry it.
    "claim",
];
/// Commands that need a second person: they leave the books apart, or a
/// reinsurer untold.
pub const COUNTERSIGNED: [&str; 6] = [
    "abandon_notice",
    "abandon_instruction",
    // Money leaves for the reinsurer, or a dispute is settled.
    "instruct_settlement",
    "resolve_dispute",
    // Cover in force leaves a treaty, or moves to another version of it.
    "recapture",
    "move_treaty_version",
];
/// Countersigned commands the module checks for two people itself: the
/// preparer is the person signed in, and the signer is `approved_by`.
pub const MAKER_CHECKER: [&str; 2] = ["instruct_settlement", "resolve_dispute"];
/// Countersigned commands whose own fields carry the reason given.
const KEEPS_REASON: [&str; 2] = ["abandon_notice", "abandon_instruction"];

/// The console's questions: name, required text fields, required whole-number fields.
const QUESTIONS: [(&str, &[&str], &[&str]); 12] = [
    ("exceptions", &[], &[]),
    ("notices", &[], &[]),
    ("abandoned", &[], &[]),
    ("abandoned_notices", &[], &[]),
    ("sweep_check", &["period_end"], &[]),
    ("statement", &[], &["number"]),
    ("notice_retries", &[], &["seq"]),
    // The reinsurers the insurer has a balance with, as of a date.
    ("exposure", &["on"], &[]),
    ("statements", &["reinsurer"], &[]),
    ("balance", &["treaty", "reinsurer"], &[]),
    ("consent_status", &["claim"], &[]),
    // What the insurer keeps on one person's life on a date, across every policy on it.
    ("retained", &["party", "on"], &[]),
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
    // A reinsurer's statements come a page at a time, newest pages after `after`.
    if name == "statements" {
        let after = match given("after") {
            Some(after) => Some(after.parse::<u32>().map_err(|_| "after is a whole number".to_string())?),
            None => None,
        };
        let reinsurer = given("reinsurer").ok_or("statements needs reinsurer")?;
        return Ok(json!({ "statements": { "reinsurer": reinsurer, "after": after, "limit": 100 } }));
    }
    // A life is asked for by the person it is: their whole accumulation, whichever policies insure them.
    if name == "retained" {
        let party = given("party").ok_or("retained needs party: the person, found by name")?;
        let on = given("on").ok_or("retained needs on, a date")?;
        on.parse::<chrono::NaiveDate>().map_err(|_| "on is a date, as 2027-11-15".to_string())?;
        return Ok(json!({ "retained": { "life": { "party": party }, "on": on } }));
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

/// A countersigned command as its preparer leaves it: where the module
/// checks two people itself, the preparer is the person signed in.
pub fn prepared(mut command: Value, name: &str, preparer: &PartyId) -> Value {
    if MAKER_CHECKER.contains(&name) {
        command[name]["prepared_by"] = json!(preparer);
    }
    command
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
    if command[name].get("prepared_by").is_some() {
        return Err("prepared_by is whoever prepares it, not something to type".to_string());
    }
    if DIRECT.contains(&name) {
        Ok((name, false))
    } else if COUNTERSIGNED.contains(&name) {
        let reason = command[name]["reason"].as_str().unwrap_or_default();
        if reason.trim().is_empty() {
            return Err("say why: the reason goes to the person who signs it".to_string());
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
        if KEEPS_REASON.contains(&name.as_str()) || MAKER_CHECKER.contains(&name.as_str()) {
            command[name.as_str()]["approved_by"] = json!(signer);
        }
        // The reason was for the signer; a command without one of its own does not carry it.
        if !KEEPS_REASON.contains(&name.as_str()) {
            if let Some(fields) = command[name.as_str()].as_object_mut() {
                fields.remove("reason");
            }
        }
    }
    json!({ "key": key, "caller": CALLER, "on": on, "command": command })
}

/// The questions this console forwards, by name, for the MCP tool that asks them.
pub fn question_names() -> Vec<&'static str> {
    QUESTIONS.iter().map(|q| q.0).collect()
}

/// The commands this console forwards, by name, for the MCP tool that sends them.
pub fn command_names() -> Vec<&'static str> {
    DIRECT.iter().copied().chain(COUNTERSIGNED).collect()
}
