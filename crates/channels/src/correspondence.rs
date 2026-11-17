//! The documents console: how staff reach the documents module
//! (`documents` on the nodes, `docs/documents.md`).
//!
//! That module keeps templates and their approvals, every document issued
//! and its delivery, the documents the core's events require, notice
//! defects, legal holds and letters other modules asked for. It decides a
//! command by the service named as its caller (`Command::allowed_callers`)
//! and records the person named as its actor; it keeps an author from
//! approving their own template and one person from giving two of a
//! template's approvals. So the console names the caller from the staff
//! actions the person holds, never from the page, and the actor is always
//! whoever signed in.
//!
//! Rendering, sending, the core's events, listing and deletion are the
//! module's drivers' work, never sent from here.

use std::collections::BTreeMap;

use chrono::DateTime;
use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The documents module's name on the nodes.
pub const MODULE: &str = "documents";

/// Staff actions that open the console and name the caller a command goes as.
pub mod actions {
    /// Read templates, documents, triggers, letters, defects, holds and the report.
    pub const VIEW: &str = "documents.view";
    /// Place and release legal holds.
    pub const LEGAL: &str = "documents.legal";
    /// Raise, fill, exclude from and close notice defects; disable a template that is wrong.
    pub const REMEDIATION: &str = "documents.remediation";
    /// Crypto-shred a party's documents when the customer master erases them.
    pub const PRIVACY: &str = "documents.privacy";
    /// Dismiss a document the core's events required that is not to be issued.
    pub const TRIGGERS: &str = "documents.triggers";
    /// Start delivery again, and drop a requested letter still waiting.
    pub const DELIVERY: &str = "documents.delivery";
    /// Submit, test and activate templates and parts; settings and retention.
    pub const CONFIGURATION: &str = "documents.configuration";
    /// Give a template's legal, product or compliance approval.
    pub const APPROVE_LEGAL: &str = "documents.approve_legal";
    pub const APPROVE_PRODUCT: &str = "documents.approve_product";
    pub const APPROVE_COMPLIANCE: &str = "documents.approve_compliance";
}

/// Each staff action that lets a person send as one of the module's callers.
pub const CALLERS: [(&str, &str); 6] = [
    (actions::LEGAL, "legal"),
    (actions::REMEDIATION, "remediation"),
    (actions::PRIVACY, "privacy"),
    (actions::TRIGGERS, "documents"),
    (actions::DELIVERY, "servicing"),
    (actions::CONFIGURATION, "config"),
];

/// Each staff action that gives one of a template's approvals.
pub const APPROVALS: [(&str, &str); 3] = [
    (actions::APPROVE_LEGAL, "legal"),
    (actions::APPROVE_PRODUCT, "product"),
    (actions::APPROVE_COMPLIANCE, "compliance"),
];

/// The commands a person may send, with the callers that module takes each
/// from (as its `allowed_callers` says, less the drivers). An approval goes
/// as `config` and needs the approval action instead.
pub const COMMANDS: [(&str, &[&str]); 19] = [
    ("place_legal_hold", &["legal"]),
    ("release_legal_hold", &["legal"]),
    ("raise_notice_defect", &["remediation"]),
    ("attach_defect_notices", &["remediation"]),
    ("exclude_from_defect", &["remediation"]),
    ("close_notice_defect", &["remediation"]),
    ("correct_defect_notices", &["remediation"]),
    ("disable_template", &["remediation", "config"]),
    ("shred_party", &["privacy"]),
    ("dismiss_trigger", &["documents"]),
    ("retry_delivery", &["servicing"]),
    ("record_returned_post", &["servicing"]),
    ("cancel_letter_request", &["servicing"]),
    ("activate_settings", &["config"]),
    ("activate_retention", &["config"]),
    ("register_part", &["config"]),
    ("submit_template", &["config"]),
    ("run_rendering_tests", &["config"]),
    ("activate_template", &["config"]),
];

/// The most lines a list answer carries to the page.
pub const LIMIT: usize = 200;

/// The questions the console may put, with the fields the page sends.
const QUESTIONS: [(&str, &[&str]); 20] = [
    ("templates", &[]),
    ("legal_holds", &[]),
    ("open_defects", &[]),
    ("triggers", &[]),
    ("letters_requested", &[]),
    ("pending_sends", &[]),
    ("undeliverable", &[]),
    ("settings", &[]),
    ("authoring", &[]),
    ("document", &["id"]),
    ("defect", &["id"]),
    ("letter_request", &["request"]),
    ("template", &["id", "version"]),
    ("render_sample", &["id", "version"]),
    ("template_for", &["id"]),
    ("part_usage", &["part"]),
    ("notices_by_template", &["id", "version", "from", "to"]),
    ("report", &["from", "to"]),
    ("policy_documents", &["policy"]),
    ("retention", &["kind"]),
];

/// Whether a member of staff may open the console, the callers they may
/// send as and the approvals they may give.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let held = |pairs: &[(&'static str, &'static str)]| -> Vec<&'static str> {
        pairs
            .iter()
            .filter(|(action, _)| staff.holds_action(party, action, at))
            .map(|(_, r)| *r)
            .collect()
    };
    let callers = held(&CALLERS);
    let approvals = held(&APPROVALS);
    let view = !callers.is_empty() || !approvals.is_empty() || staff.holds_action(party, actions::VIEW, at);
    let download = staff.holds_action(party, crate::documents::actions::DOWNLOAD, at);
    json!({ "staff": party, "view": view, "callers": callers, "approvals": approvals, "download": download })
}

/// A question for the documents module, from its name and the fields the page sent.
pub fn question(name: &str, fields: &BTreeMap<String, String>, at: At) -> Result<Value, String> {
    let wanted = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .map(|(_, w)| *w)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    let mut given = BTreeMap::new();
    for field in wanted {
        let value = fields
            .get(*field)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} needs {field}"))?;
        given.insert(*field, value.to_string());
    }
    let version = || {
        given
            .get("version")
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| "version is a whole number".to_string())
    };
    let template = || -> Result<Value, String> { Ok(json!({ "id": given["id"], "version": version()? })) };
    Ok(match name {
        "templates" | "open_defects" | "triggers" | "letters_requested" | "pending_sends" | "undeliverable" => {
            json!({ name: { "limit": LIMIT } })
        }
        "legal_holds" | "settings" | "authoring" => json!(name),
        "template" => json!({ "template": { "template": template()? } }),
        // A sample is rendered with the data a product's document of the
        // kind is given, or with the data the page sends as JSON.
        "render_sample" => {
            let data = match fields.get("data").map(|d| d.trim()).filter(|d| !d.is_empty()) {
                Some(text) => Some(serde_json::from_str::<Value>(text).map_err(|e| format!("the sample's data is not JSON: {e}"))?),
                None => None,
            };
            let language = fields.get("language").map(|l| l.trim()).filter(|l| !l.is_empty());
            json!({ name: { "template": template()?, "language": language, "data": data } })
        }
        "template_for" => json!({ "template_for": { "id": given["id"], "on": today(at) } }),
        "notices_by_template" => json!({ name: { "template": template()?, "from": given["from"], "to": given["to"] } }),
        _ => json!({ name: given }),
    })
}

/// The business day in New Zealand at `at`.
pub fn today(at: At) -> chrono::NaiveDate {
    DateTime::from_timestamp(at, 0)
        .unwrap_or_default()
        .with_timezone(&chrono_tz::Pacific::Auckland)
        .date_naive()
}

/// Why the console would not send a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Not a command a person sends there: the page is wrong.
    NotACommand(String),
    /// A command this person's actions do not let them send.
    NotYours(String),
}

/// The command as the module takes it (`{name: fields}`) and the caller it
/// goes as, from the page's `{"type": name, ...}` and what the person holds.
/// The person signed in is the author of a template they submit.
pub fn command(page: &Value, staff: &PartyId, callers: &[String], approvals: &[String]) -> Result<(Value, &'static str), Refused> {
    let not_one = || Refused::NotACommand("a command is one object that names its type".to_string());
    let mut fields: Map<String, Value> = page.as_object().cloned().ok_or_else(not_one)?;
    let name = fields.remove("type").and_then(|t| t.as_str().map(str::to_string)).ok_or_else(not_one)?;
    let caller = if name == "approve_template" {
        let role = fields.get("role").and_then(Value::as_str).unwrap_or_default();
        if !approvals.iter().any(|a| a == role) {
            return Err(Refused::NotYours(format!("you do not give a template's {role} approval")));
        }
        "config"
    } else {
        let allowed = COMMANDS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c).ok_or_else(|| {
            // Say what this person may send instead, so a slip of the keyboard is put right at once.
            let yours: Vec<&str> = COMMANDS
                .iter()
                .filter(|(_, c)| c.iter().any(|c| callers.iter().any(|h| h == c)))
                .map(|(n, _)| *n)
                .collect();
            Refused::NotACommand(format!(
                "{name} is not a command people send to documents; yours are: {}",
                if yours.is_empty() { "none".to_string() } else { yours.join(", ") }
            ))
        })?;
        allowed
            .iter()
            .find(|c| callers.iter().any(|h| h == *c))
            .copied()
            .ok_or_else(|| Refused::NotYours(format!("your roles do not let you {}", name.replace('_', " "))))?
    };
    if name == "submit_template" {
        fields.insert("author".to_string(), json!(staff));
    }
    // A hold is placed or released for a reason, kept with who did it and when.
    let placing = name == "place_legal_hold";
    if (placing || name == "release_legal_hold") && fields.get("reason").and_then(Value::as_str).is_none_or(|r| r.trim().is_empty()) {
        let what = if placing { "placed" } else { "released" };
        return Err(Refused::NotACommand(format!("say why the hold is {what}: the reason is kept with it")));
    }
    Ok((json!({ name: fields }), caller))
}

/// The envelope that carries one person's command.
pub fn envelope(key: &str, staff: &PartyId, caller: &str, at: At, command: Value) -> Value {
    json!({
        "key": format!("console:{key}"),
        "caller": caller,
        "actor": staff,
        "at": DateTime::from_timestamp(at, 0).unwrap_or_default(),
        "command": command,
    })
}

/// The questions this console forwards, by name, for the MCP tool that
/// asks them, and `policies`, which the portal answers itself.
pub fn question_names() -> Vec<&'static str> {
    QUESTIONS.iter().map(|q| q.0).chain([POLICIES]).collect()
}

/// The question the portal answers from its own directory of policies:
/// those whose number is the text, starts with it or ends with it, for a
/// person who has the number and not the internal reference.
pub const POLICIES: &str = "policies";

/// The fields of a stored document a person has no use for and must not
/// be shown: the key its contents are encrypted under, and where in the
/// vault they are.
const KEPT_BACK: [&str; 2] = ["wrapped_key", "blob"];

/// An answer as the console shows it: documents without their key material.
pub fn shown(name: &str, mut answer: Value) -> Value {
    let strip = |doc: &mut Value| {
        if let Some(fields) = doc.as_object_mut() {
            for field in KEPT_BACK {
                fields.remove(field);
            }
        }
    };
    match name {
        "document" => strip(&mut answer),
        "policy_documents" | "undeliverable" => answer.as_array_mut().into_iter().flatten().for_each(strip),
        _ => {}
    }
    answer
}
