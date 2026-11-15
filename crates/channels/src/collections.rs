//! The collections console: how staff reach the collections module
//! (`collections` on the nodes, `docs/payments.md`, "Collections").
//!
//! That module keeps the dunning sequence of each product, the queue of
//! policies in arrears, the tasks of the collections and hardship teams,
//! payment arrangements and hardship cases. It decides each command by the
//! roles it is told the person holds: an officer works the queue and agrees
//! arrangements within the product's limit, a team lead takes what goes
//! beyond it, the hardship team decides hardship (never the person who
//! applied) and a configuration person sets sequences. The console tells it
//! who signed in and the roles that follow from that person's staff
//! actions.
//!
//! The core's events, the documents service's reports on notices and the
//! outcomes of the module's postings are its drivers' work, never sent from
//! here.

use std::collections::BTreeMap;

use chrono::DateTime;
use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The collections module's name on the nodes.
pub const MODULE: &str = "collections";

/// Staff actions that open the console and give that module's roles.
pub mod actions {
    /// Read the queue, tasks, notices and sequences without acting.
    pub const VIEW: &str = "collections.view";
    /// Work the queue: record outcomes, agree arrangements, apply for hardship, ask for reinstatement.
    pub const OFFICER: &str = "collections.officer";
    /// Arrangements beyond an officer's limit, and escalated call tasks.
    pub const TEAM_LEAD: &str = "collections.team_lead";
    /// Decide and end hardship; sees what an application discloses about health.
    pub const HARDSHIP: &str = "collections.hardship";
    /// Set a product's dunning sequence.
    pub const CONFIGURATION: &str = "collections.configuration";
}

/// Each staff action that gives a role there, and the role, most able first
/// for reading: a person who holds several reads the queue as the first.
pub const ROLES: [(&str, &str); 4] = [
    (actions::HARDSHIP, "hardship"),
    (actions::TEAM_LEAD, "team_lead"),
    (actions::OFFICER, "collections"),
    (actions::CONFIGURATION, "configuration"),
];

/// The commands a person may send. That module decides whether this
/// person's roles allow it.
pub const COMMANDS: [&str; 8] = [
    "set_sequence",
    "record_outcome",
    "propose_arrangement",
    "end_arrangement",
    "apply_hardship",
    "decide_hardship",
    "end_hardship",
    "request_reinstatement",
];

/// The questions the console may put, with the fields the page sends.
const QUESTIONS: [(&str, &[&str]); 10] = [
    ("queue", &[]),
    ("hardship_cases", &[]),
    ("policy", &["policy"]),
    ("tasks", &["team"]),
    ("hardship_case", &["case"]),
    ("notices_due", &[]),
    ("notices_sent", &[]),
    ("unsequenced", &[]),
    ("sequences", &[]),
    ("reinstatements_for_underwriting", &[]),
];

/// The most lines a list answer carries to the page.
pub const LIMIT: usize = 200;

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

/// The role a person reads as: the most able they hold among the teams that
/// read the queue, or an officer's view for a reader or configuration person,
/// so nothing is shown to them that an officer would not see.
pub fn reading_role(roles: &[String]) -> &'static str {
    ROLES
        .iter()
        .map(|(_, role)| *role)
        .filter(|role| *role != "configuration")
        .find(|role| roles.iter().any(|r| r == role))
        .unwrap_or("collections")
}

/// A question for the collections module, from its name and the fields the
/// page sent, for a person holding `roles` there.
pub fn question(name: &str, fields: &BTreeMap<String, String>, roles: &[String], at: At) -> Result<Value, String> {
    let wanted = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .map(|(_, w)| *w)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    let mut given = Map::new();
    for field in wanted {
        let value = fields
            .get(*field)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} needs {field}"))?;
        given.insert((*field).to_string(), json!(value));
    }
    let role = reading_role(roles);
    let mut out = json!({ "q": name });
    match name {
        "queue" => {
            out["as_at"] = json!(today(at));
            out["role"] = json!(role);
            out["limit"] = json!(LIMIT);
        }
        "hardship_cases" => {
            out["as_at"] = json!(today(at));
            out["role"] = json!(role);
        }
        "tasks" => {
            // A person reads their own teams' tasks; a reader only the officers'.
            let team = given["team"].as_str().unwrap_or_default();
            let allowed = ["collections", "hardship", "team_lead"];
            if !allowed.contains(&team) {
                return Err(format!("{team} is not a collections team"));
            }
            if team != "collections" && !roles.iter().any(|r| r == team) {
                return Err(format!("the {team} tasks are that team's"));
            }
            out["role"] = json!(team);
            out["limit"] = json!(LIMIT);
        }
        "hardship_case" => {
            out["case"] = given["case"].clone();
            out["role"] = json!(role);
        }
        "notices_due" | "notices_sent" => out["limit"] = json!(LIMIT),
        _ => {
            for (field, value) in given {
                out[field.as_str()] = value;
            }
        }
    }
    Ok(out)
}

/// The business day in New Zealand at `at`.
pub fn today(at: At) -> chrono::NaiveDate {
    DateTime::from_timestamp(at, 0)
        .unwrap_or_default()
        .with_timezone(&chrono_tz::Pacific::Auckland)
        .date_naive()
}

/// The command's name, if it is one a person may send from the console:
/// one object whose `type` names it.
pub fn console_command(command: &Value) -> Option<&str> {
    let name = command.as_object()?.get("type")?.as_str()?;
    COMMANDS.contains(&name).then_some(name)
}

/// The envelope that carries one person's command: who they are and the
/// roles they hold there.
pub fn envelope(key: &str, staff: &PartyId, roles: &[String], at: At, command: Value) -> Value {
    json!({
        "key": format!("console:{key}"),
        "caller": "staff",
        "actor": staff,
        "roles": roles,
        "at": DateTime::from_timestamp(at, 0).unwrap_or_default(),
        "command": command,
    })
}

/// The questions this console forwards, by name, for the MCP tool that asks them.
pub fn question_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = QUESTIONS.iter().map(|q| q.0).collect();
    names.push("shipped_sequence");
    names
}

/// The best-estimate sequence shipped for a product, for a configuration
/// person to start from. Nobody has approved it; setting it is their act.
pub fn shipped_sequence(product: &str) -> Option<Value> {
    ic_collections::shipped_sequences()
        .ok()?
        .into_iter()
        .find(|s| s.product == product)
        .and_then(|s| serde_json::to_value(s).ok())
}
