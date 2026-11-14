//! The commission console: how staff reach the commission module
//! (`distribution` on the nodes, `docs/distribution.md`).
//!
//! That module keeps advisers, providers and their engagements, payees and
//! their accounts, commission rules and statements, and its own conduct and
//! dispute cases. It takes people's commands as a member of staff it is
//! told about, and decides each with its own Cedar policy
//! (`config/cedar/distribution.cedar`). The roles it is told follow from
//! the staff actions a person's templates hold here, so who may manage
//! advisers, propose or approve rules, or record a licence is administered
//! with every other staff entitlement.
//!
//! The held balance workbench, replacement decisions and policy assignment
//! have pages and paths of their own, so the console does not send them.

use std::collections::BTreeMap;

use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// Staff actions that open the console and give that module's roles.
pub mod actions {
    pub const VIEW: &str = "commission.view";
    pub const STRUCTURE: &str = "commission.structure";
    pub const OPS: &str = "commission.ops";
    pub const APPROVE: &str = "commission.approve";
    pub const COMPLIANCE: &str = "commission.compliance";
}

/// Each staff action that gives a role there, and the role.
pub const ROLES: [(&str, &str); 4] = [
    (actions::STRUCTURE, "distribution_admin"),
    (actions::OPS, "commission_ops"),
    (actions::APPROVE, "commission_approver"),
    (actions::COMPLIANCE, "compliance"),
];

/// The commands a person may send from the console. That module decides
/// whether this person may, and records the decision either way.
pub const COMMANDS: [&str; 20] = [
    "add_party",
    "relate",
    "end_relation",
    "start_engagement",
    "end_engagement",
    "record_licence",
    "mark_ceased",
    "register_payee",
    "add_payee_account",
    "set_payment_hold",
    "set_accrual_hold",
    "add_payment_instruction",
    "remove_payment_instruction",
    "propose_change",
    "approve_change",
    "reject_change",
    "cut_statement",
    "attach_document",
    "complete_task",
    "close_case",
];

/// A field a question takes, and whether it is a number.
type Field = (&'static str, bool);

/// The questions the console may put, and the fields each must carry. The
/// outbox and the driver's own questions are not people's.
const QUESTIONS: [(&str, &[Field]); 15] = [
    ("status", &[]),
    ("open_tasks", &[]),
    ("history", &[]),
    ("conduct_cases", &[]),
    ("relations", &[]),
    ("sweep_check", &[]),
    ("position", &[("party", false), ("as_at", false)]),
    ("proof_of_debt", &[("party", false), ("as_at", false)]),
    ("statement", &[("payee", false), ("number", true)]),
    ("held_control", &[("date", false)]),
    ("month_end_blocks", &[("date", false)]),
    ("clawback_exposure", &[("date", false)]),
    ("aged_held", &[("as_at", false)]),
    ("action", &[("id", false)]),
    ("case", &[("id", false)]),
];

/// Whether a member of staff may open the console, and the roles they hold there.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let roles: Vec<&str> = ROLES
        .iter()
        .filter(|(action, _)| staff.holds_action(party, action, at))
        .map(|(_, role)| *role)
        .collect();
    json!({ "view": !roles.is_empty() || staff.holds_action(party, actions::VIEW, at), "roles": roles })
}

/// A question for the commission module, from its name and the fields the
/// page sent. Only the console's questions pass, each with exactly its fields.
pub fn question(name: &str, fields: &BTreeMap<String, String>) -> Result<Value, String> {
    let (_, wanted) = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    if wanted.is_empty() {
        return Ok(json!(name));
    }
    let mut out = Map::new();
    for (field, number) in *wanted {
        let value = fields
            .get(*field)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} needs {field}"))?;
        let value = if *number {
            json!(value.parse::<u32>().map_err(|_| format!("{field} is a number"))?)
        } else {
            json!(value)
        };
        out.insert((*field).to_string(), value);
    }
    Ok(json!({ name: out }))
}

/// A commission statement as its own payee sees it in the adviser portal:
/// the statement's lines and balances, and the totals by what earned them
/// (`base`, `override:{level}`, `fee:{name}`), so a group sees its overrides
/// at a glance. Lines carry policies and amounts, never a claim or its cause.
pub fn own_statement(statement: &Value) -> Value {
    const KEEP: [&str; 14] = [
        "payee",
        "number",
        "date",
        "opening_minor",
        "lines",
        "closing_minor",
        "held_minor",
        "held_reasons",
        "retention_minor",
        "advance_outstanding_minor",
        "advance_recovered_minor",
        "payment_held",
        "payments",
        "hash",
    ];
    let mut out: Map<String, Value> = statement
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| KEEP.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for line in statement["lines"].as_array().into_iter().flatten() {
        let label = match &line["component"] {
            Value::String(name) => name.clone(),
            Value::Object(o) => o.iter().next().map_or_else(String::new, |(kind, level)| {
                let kind = if kind == "fixed_fee" { "fee" } else { kind.as_str() };
                format!("{kind}:{}", level.as_str().unwrap_or_default())
            }),
            _ => line["kind"].as_str().unwrap_or("other").to_string(),
        };
        *totals.entry(label).or_default() += line["amount_minor"].as_i64().unwrap_or(0);
    }
    out.insert("totals".to_string(), json!(totals));
    Value::Object(out)
}

/// The command's name, if it is one a person may send from the console.
pub fn console_command(command: &Value) -> Option<&str> {
    let name = command.as_object().filter(|o| o.len() == 1)?.keys().next()?;
    COMMANDS.contains(&name.as_str()).then_some(name.as_str())
}

/// The envelope that carries one person's command, as that module's
/// description of them.
pub fn envelope(key: &str, staff: Value, command: Value) -> Value {
    json!({ "key": format!("console:{key}"), "actor": { "staff": staff }, "command": command })
}

/// The questions this console forwards, by name, for the MCP tool that asks them.
pub fn question_names() -> Vec<&'static str> {
    QUESTIONS.iter().map(|q| q.0).collect()
}
