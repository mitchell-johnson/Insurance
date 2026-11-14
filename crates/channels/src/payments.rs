//! The payments console: how staff reach the payments module (`payments`
//! on the nodes, `docs/payments.md`).
//!
//! That module keeps authorities, payees, runs, refunds and reconciliation,
//! and decides each command by the roles it is told the person holds: a
//! maker prepares a run, an approver who is not the maker approves it, and
//! an approver never also maintains payees or connectivity. The console
//! tells it who signed in and the roles that follow from that person's
//! staff actions, so who may move money is administered with every other
//! staff entitlement. Times are New Zealand wall-clock time, as the module
//! keeps them.
//!
//! The banks' answers, the drill, the core's events and the module's own
//! outbox are its drivers' work, not people's, and are never sent from here.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDateTime};
use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The payments module's name on the nodes.
pub const MODULE: &str = "payments";

/// Staff actions that open the console and give that module's roles.
pub mod actions {
    /// Read runs, payments waiting, refunds, reconciliation and readiness.
    pub const VIEW: &str = "payments.view";
    pub const MAKER: &str = "payments.maker";
    pub const APPROVER: &str = "payments.approver";
    pub const PAYEES: &str = "payments.payees";
    pub const CONNECTIVITY: &str = "payments.connectivity";
    pub const LEGAL: &str = "payments.legal";
    pub const RECONCILER: &str = "payments.reconciler";
    pub const TREASURY: &str = "payments.treasury";
    pub const COLLECTIONS: &str = "payments.collections";
    /// Works anti-money-laundering cases of premium paid by someone other than the owner.
    pub const COMPLIANCE: &str = "payments.compliance";
}

/// Each staff action that gives a role there, and the role.
pub const ROLES: [(&str, &str); 9] = [
    (actions::MAKER, "maker"),
    (actions::APPROVER, "approver"),
    (actions::PAYEES, "payee_maintainer"),
    (actions::CONNECTIVITY, "connectivity_admin"),
    (actions::LEGAL, "legal"),
    (actions::RECONCILER, "reconciler"),
    (actions::TREASURY, "treasury"),
    (actions::COLLECTIONS, "collections"),
    (actions::COMPLIANCE, "compliance"),
];

/// The commands a person may send. That module decides whether this
/// person's roles allow it.
pub const COMMANDS: [&str; 30] = [
    "activate_config",
    "create_authority",
    "suspend_authority",
    "reinstate_authority",
    "cancel_authority",
    "mark_hardship",
    "record_conversation",
    "register_payee",
    "change_payee_account",
    "confirm_payee",
    "approve_payee",
    "close_payee",
    "prepare_run",
    "add_run_items",
    "take_waiting",
    "remove_run_item",
    "check_run",
    "release_item",
    "approve_run",
    "cancel_run",
    "resubmit",
    "close_run",
    "redirect_route",
    "request_refund",
    "release_refund",
    "assign_exception",
    "resolve_exception",
    // Money nobody could identify, applied by hand: one reconciler proposes, another approves.
    "propose_receipt_match",
    "approve_receipt_match",
    "clear_payer",
];

/// The questions the console may put, with their fields. `reconciliation`
/// and `readiness` are asked as of today; the outbox, messages read and the
/// trail are the drivers'.
const QUESTIONS: [(&str, &[&str]); 25] = [
    ("open_runs", &[]),
    ("open_exceptions", &[]),
    ("matches_to_approve", &[]),
    ("manual_match", &["exception"]),
    ("party", &["party"]),
    ("customer_hold", &["party"]),
    ("waiting", &[]),
    ("run", &["run"]),
    ("control_totals", &["run"]),
    ("submission", &["submission"]),
    ("queued_submissions", &[]),
    ("unconfirmed_submissions", &[]),
    ("authority", &["authority"]),
    ("payee", &["payee"]),
    ("refund", &["refund"]),
    ("awaiting_conversation", &[]),
    ("represent", &[]),
    ("deferred", &["from", "to"]),
    ("fund_sweep", &["as_at"]),
    ("fund_cash_settlement", &["as_at"]),
    ("aged_unidentified_receipts", &["as_at"]),
    ("reconciliation", &["account"]),
    ("readiness", &[]),
    ("payer_cases", &[]),
    ("payer_case", &["case"]),
];

/// Questions whose answers carry bank account details: only for someone
/// who holds a role there, never a reader. The configuration is the
/// connectivity administrator's alone.
const DETAILED: [&str; 3] = ["authority", "payee", "refund"];

/// Questions about money-laundering cases: who paid whose premium. Only
/// compliance sees them.
const COMPLIANCE_ONLY: [&str; 2] = ["payer_cases", "payer_case"];

/// Whether a member of staff may open the console, and the roles they hold there.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let roles: Vec<&str> = ROLES
        .iter()
        .filter(|(action, _)| staff.holds_action(party, action, at))
        .map(|(_, role)| *role)
        .collect();
    let view = !roles.is_empty() || staff.holds_action(party, actions::VIEW, at);
    json!({ "staff": party, "view": view, "roles": roles })
}

/// New Zealand wall-clock time at `at`, as that module keeps time.
pub fn nz(at: At) -> NaiveDateTime {
    DateTime::from_timestamp(at, 0)
        .unwrap_or_default()
        .with_timezone(&chrono_tz::Pacific::Auckland)
        .naive_local()
}

/// A question for the payments module, from its name and the fields the
/// page sent, for a person holding `roles` there.
pub fn question(name: &str, fields: &BTreeMap<String, String>, roles: &[String], at: At) -> Result<Value, String> {
    let wanted: &[&str] = if name == "config" {
        if !roles.iter().any(|r| r == "connectivity_admin") {
            return Err("the configuration is the connectivity administrator's".to_string());
        }
        &[]
    } else {
        QUESTIONS
            .iter()
            .find(|(q, _)| *q == name)
            .map(|(_, w)| *w)
            .ok_or_else(|| format!("{name} is not a question the console asks"))?
    };
    if DETAILED.contains(&name) && roles.is_empty() {
        return Err(format!("{name} shows bank details, which a reader does not see"));
    }
    if COMPLIANCE_ONLY.contains(&name) && !roles.iter().any(|r| r == "compliance") {
        return Err(format!("{name} is compliance's"));
    }
    let mut out = Map::new();
    out.insert("q".to_string(), json!(name));
    for field in wanted {
        let value = fields
            .get(*field)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} needs {field}"))?;
        out.insert((*field).to_string(), json!(value));
    }
    // Open exceptions may be narrowed to one account.
    if let Some(account) = fields.get("account").map(|a| a.trim()).filter(|a| name == "open_exceptions" && !a.is_empty()) {
        out.insert("account".to_string(), json!(account));
    }
    if matches!(name, "reconciliation" | "readiness") {
        out.insert("today".to_string(), json!(nz(at).date()));
    }
    Ok(Value::Object(out))
}

/// The command's name, if it is one a person may send from the console:
/// one object whose `type` names it.
pub fn console_command(command: &Value) -> Option<&str> {
    let name = command.as_object()?.get("type")?.as_str()?;
    COMMANDS.contains(&name).then_some(name)
}

/// The envelope that carries one person's command: who they are and the
/// roles they hold there, at New Zealand time.
pub fn envelope(key: &str, staff: &PartyId, roles: &[String], at: At, command: Value) -> Value {
    json!({
        "key": format!("console:{key}"),
        "actor": { "id": staff, "roles": roles },
        "at": nz(at),
        "command": command,
    })
}

/// The questions this console forwards, by name, for the MCP tool that asks them.
pub fn question_names() -> Vec<&'static str> {
    QUESTIONS.iter().map(|q| q.0).collect()
}
