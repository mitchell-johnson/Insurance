//! The unit pricing console: how staff reach the unit pricing module.
//!
//! That module (`unit-pricing` on the nodes) strikes, checks and publishes
//! unit prices, and takes people's commands only from this console, which
//! says who the person is and which of its roles they hold. The roles follow
//! from the staff actions a person's templates hold here, so who may prepare,
//! approve or lead is administered with every other staff entitlement.
//!
//! The two modules share no code. Questions and commands go across as JSON;
//! `tests/pricing.rs` runs them against that module's own types.

use std::collections::BTreeMap;

use chrono::DateTime;
use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The unit pricing module's name on the nodes.
pub const MODULE: &str = "unit-pricing";
/// The caller that module takes people's commands from.
pub const CALLER: &str = "unit-pricing-console";

/// Staff actions that open the console and give that module's roles.
pub mod actions {
    pub const VIEW: &str = "unit_pricing.view";
    pub const PREPARE: &str = "unit_pricing.prepare";
    pub const APPROVE: &str = "unit_pricing.approve";
    pub const LEAD: &str = "unit_pricing.lead";
}

/// Each staff action that gives a role there, and the role.
const ROLES: [(&str, &str); 3] = [
    (actions::PREPARE, "unit_pricing_preparer"),
    (actions::APPROVE, "unit_pricing_approver"),
    (actions::LEAD, "unit_pricing_lead"),
];

/// The command each person may send, and the role that module asks for it.
const COMMANDS: [(&str, &str); 8] = [
    ("prepare_price", "unit_pricing_preparer"),
    ("approve_price", "unit_pricing_approver"),
    ("reject_price", "unit_pricing_approver"),
    ("suspend_fund", "unit_pricing_lead"),
    ("resume_fund", "unit_pricing_lead"),
    ("raise_price_error", "unit_pricing_lead"),
    ("close_error", "unit_pricing_lead"),
    ("republish", "unit_pricing_lead"),
];

/// The questions the console may put, and the fields each must carry.
const QUESTIONS: [(&str, &[&str]); 9] = [
    ("status", &[]),
    ("funds", &[]),
    ("work", &[]),
    ("errors", &[]),
    ("price", &["id"]),
    ("error", &["id"]),
    ("prices", &["fund", "from", "to"]),
    ("valuations", &["fund", "pricing_date"]),
    ("late", &["as_at"]),
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

/// A question for the unit pricing module, from its name and the fields the
/// page sent. Only the console's questions pass, each with exactly its fields.
pub fn question(name: &str, fields: &BTreeMap<String, String>) -> Result<Value, String> {
    let (_, wanted) = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    let mut out = Map::new();
    out.insert("q".to_string(), json!(name));
    for field in *wanted {
        let value = fields
            .get(*field)
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("{name} needs {field}"))?;
        out.insert((*field).to_string(), json!(value));
    }
    Ok(Value::Object(out))
}

/// The role a command needs, if it is one a person may send from the console.
pub fn role_for(command: &Value) -> Option<(&'static str, &'static str)> {
    let fields = command.as_object().filter(|o| o.len() == 1)?;
    let name = fields.keys().next()?;
    COMMANDS.iter().find(|(c, _)| c == name).copied()
}

/// The envelope that carries one person's command to the unit pricing
/// module: who they are, the roles they hold, and when.
pub fn envelope(key: &str, actor: &PartyId, roles: &Value, at: At, command: Value) -> Value {
    let when = DateTime::from_timestamp(at, 0).unwrap_or_default();
    json!({ "key": key, "caller": CALLER, "actor": actor, "roles": roles, "at": when, "command": command })
}

/// The questions this console forwards, by name, for the MCP tool that asks them.
pub fn question_names() -> Vec<&'static str> {
    QUESTIONS.iter().map(|q| q.0).collect()
}

/// The commands this console forwards, by name, for the MCP tool that sends them.
pub fn command_names() -> Vec<&'static str> {
    COMMANDS.iter().map(|c| c.0).collect()
}
