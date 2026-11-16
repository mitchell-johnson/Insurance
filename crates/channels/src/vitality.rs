//! AIA Vitality on the staff console: what the wellness module (`wellness`
//! on the nodes, `docs/wellness.md`) holds about a policy's members and their
//! discount, and the discount changes the core refused.
//!
//! The module takes no command from a person: the programme's feed says who
//! joined, their status and who left, and the module tells the core. So
//! staff only read here, under one staff action.

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// The wellness module's name on the nodes.
pub const MODULE: &str = "wellness";

/// The staff action that reads it.
pub mod actions {
    pub const VIEW: &str = "wellness.view";
}

/// The most lines a list answer carries to the page.
pub const LIMIT: usize = 200;

/// The questions the console may put, with the field each needs.
const QUESTIONS: [(&str, Option<&str>); 4] = [("policy", Some("policy")), ("member", Some("member")), ("refused", None), ("outbox", None)];

/// A question for the wellness module, from its name and the fields the page sent,
/// with the record the access log names.
pub fn question(name: &str, fields: &BTreeMap<String, String>) -> Result<(Value, String), String> {
    let wanted = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .map(|(_, w)| *w)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    Ok(match wanted {
        Some(field) => {
            let value = fields
                .get(field)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} needs {field}"))?;
            (json!({ "q": name, field: value }), format!("{field}:{value}"))
        }
        None => (json!({ "q": name, "limit": LIMIT }), "wellness".to_string()),
    })
}

/// The questions this console forwards, by name, for the MCP tool that asks them.
pub fn question_names() -> Vec<&'static str> {
    QUESTIONS.iter().map(|q| q.0).collect()
}
