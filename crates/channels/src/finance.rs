//! The finance console: how staff reach the finance ledger.
//!
//! That module (`finance` on the nodes) keeps the general ledger, the
//! month-end close, manual journals and regulatory returns, and takes
//! everything a person does from the caller `finance-console`, with the
//! person and the roles this service states. The roles follow from the
//! staff actions a person's templates hold here.
//!
//! The two modules share no code. `tests/http.rs` runs the console against
//! that module's own state machine.

use std::collections::BTreeMap;

use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The finance ledger's name on the nodes.
pub const MODULE: &str = "finance";
/// The caller that module takes people's commands from.
pub const CALLER: &str = "finance-console";
/// A journal line's amount there is in millionths of a minor unit.
const FINE_PER_MINOR: i64 = 1_000_000;

/// Staff actions that open the console and give that module's roles.
pub mod actions {
    pub const VIEW: &str = "finance.view";
    pub const OPERATE: &str = "finance.operate";
    pub const APPROVE: &str = "finance.approve";
    pub const LEAD: &str = "finance.lead";
    pub const ACTUARY: &str = "finance.actuary";
}

const ROLES: [(&str, &str); 4] = [
    (actions::OPERATE, "finance_operator"),
    (actions::APPROVE, "finance_approver"),
    (actions::LEAD, "finance_lead"),
    (actions::ACTUARY, "appointed_actuary"),
];

/// Commands that module takes from its own services and feeds, never from
/// a person at the console.
const SERVICE_ONLY: [&str; 7] = [
    "configure",
    "ingest",
    "core_event",
    "record_merkle_root",
    "register_extract",
    "record_conduct_facts",
    "report_aged_items",
];

/// A question's fields: text that must be there, text that may be, and
/// whole numbers that may be.
type Fields = (&'static [&'static str], &'static [&'static str], &'static [&'static str]);

const QUESTIONS: [(&str, Fields); 15] = [
    ("status", (&[], &[], &[])),
    ("work", (&[], &[], &[])),
    ("dashboard", (&["period", "today"], &[], &[])),
    ("checks", (&["period"], &[], &[])),
    ("trial_balance", (&["period"], &["fund"], &[])),
    ("movements", (&["period"], &[], &[])),
    ("sources", (&["period"], &[], &[])),
    ("backdated", (&["period"], &["origin"], &[])),
    ("journal", (&["id"], &[], &[])),
    ("conduct", (&["period"], &["product"], &[])),
    ("return", (&["id"], &[], &[])),
    ("aged_suspense", (&["as_at"], &[], &[])),
    ("extract", (&["kind", "period"], &[], &["offset", "limit"])),
    // How many policies are in each accounting group, and one policy's group
    // (the partition is a number; the module refuses a question without it).
    ("accounting_groups", (&[], &[], &[])),
    ("accounting_group", (&["policy"], &[], &["partition"])),
];

/// What a member of staff may do in the console: whether they may open it,
/// and the roles they act in.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let roles: Vec<&str> = ROLES
        .iter()
        .filter(|(action, _)| staff.holds_action(party, action, at))
        .map(|(_, role)| *role)
        .collect();
    json!({ "view": !roles.is_empty() || staff.holds_action(party, actions::VIEW, at), "roles": roles })
}

/// A question for the finance ledger, from its name and the fields the
/// page sent. Only the console's questions pass, each with its own fields.
pub fn question(name: &str, fields: &BTreeMap<String, String>) -> Result<Value, String> {
    let (_, (needed, optional, numbers)) = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    let given = |field: &str| fields.get(field).map(|v| v.trim()).filter(|v| !v.is_empty());
    let mut out = Map::new();
    out.insert("q".to_string(), json!(name));
    for field in *needed {
        out.insert((*field).to_string(), json!(given(field).ok_or_else(|| format!("{name} needs {field}"))?));
    }
    for field in *optional {
        if let Some(value) = given(field) {
            out.insert((*field).to_string(), json!(value));
        }
    }
    for field in *numbers {
        if let Some(value) = given(field) {
            let number: u64 = value.parse().map_err(|_| format!("{field} is a whole number"))?;
            out.insert((*field).to_string(), json!(number));
        }
    }
    Ok(Value::Object(out))
}

/// A journal line may give its amount as `signed_minor`, in minor units:
/// debits positive, credits negative. That module counts in millionths of
/// a minor unit, which a browser cannot hold exactly, so the conversion is
/// made here. A cut-over total may likewise come as `total_minor`.
fn exact_lines(command: &mut Value) -> Result<(), String> {
    // A cut-over total is on the trial balance's scale, so it comes as minor units too.
    if let Some(fields) = command.pointer_mut("/record_cutover_total").and_then(Value::as_object_mut) {
        if let Some(minor) = fields.remove("total_minor") {
            let fine = minor
                .as_i64()
                .and_then(|m| m.checked_mul(FINE_PER_MINOR))
                .ok_or("a cut-over total_minor is a whole number of minor units, within range")?;
            fields.insert("total".to_string(), json!(fine));
        }
    }
    // By pointer, not by index: indexing a command of another name would add the key.
    let Some(lines) = command.pointer_mut("/propose_journal/journal/lines").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for line in lines {
        let Some(fields) = line.as_object_mut() else { continue };
        if let Some(minor) = fields.remove("signed_minor") {
            let fine = minor
                .as_i64()
                .and_then(|m| m.checked_mul(FINE_PER_MINOR))
                .ok_or("a journal line's signed_minor is a whole number of minor units, within range")?;
            fields.insert("signed_fine".to_string(), json!(fine));
        }
    }
    Ok(())
}

/// The envelope that carries one person's command to the finance ledger:
/// who they are and the roles they hold. Commands that module takes only
/// from its own services are refused.
pub fn envelope(key: &str, actor: &PartyId, roles: &Value, mut command: Value) -> Result<Value, String> {
    let name = match &command {
        Value::Object(fields) if fields.len() == 1 => fields.keys().next().cloned(),
        // A command with no fields may be sent by its name alone.
        Value::String(name) => Some(name.clone()),
        _ => None,
    }
    .ok_or("a command is one name with its fields")?;
    if SERVICE_ONLY.contains(&name.as_str()) {
        return Err(format!("{name} is not sent by a person"));
    }
    exact_lines(&mut command)?;
    Ok(json!({ "key": key, "caller": CALLER, "actor": actor, "roles": roles, "command": command }))
}
