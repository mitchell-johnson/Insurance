//! The complaints and conduct workbench: how staff reach the complaints
//! module.
//!
//! That module (`complaints` on the nodes) keeps the register, the timers,
//! referrals and conduct cases, and takes each command from a named calling
//! service with the person responsible. This workbench is those services
//! for people: front-line staff record a complaint as `intake`, the
//! complaints team works it as `complaints`, and legal and the conduct
//! function act as `legal` and `conduct`. Which of those seats a person may
//! take follows from the staff actions their templates hold here.
//!
//! Two things that module is told and cannot check are never typed by the
//! person: a handler's authority level and vulnerability training, and the
//! authority of whoever authorises a remediation. They are read from the
//! staff state and written into the command by the service.
//!
//! The two modules share no code. `tests/complaints.rs` runs the workbench
//! against that module's own state machine.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate};
use ic_core::PartyId;
use serde_json::{json, Value};

use crate::staff::{At, StaffState};

/// The complaints module's name on the nodes.
pub const MODULE: &str = "complaints";

/// Staff actions that give a seat at the workbench.
pub mod actions {
    /// Record a complaint wherever it is heard.
    pub const RECORD: &str = "complaints.record";
    /// Work complaints as the complaints team.
    pub const HANDLE: &str = "complaints.handle";
    pub const LEGAL: &str = "complaints.legal";
    pub const CONDUCT: &str = "complaints.conduct";
    /// Trained to work with vulnerable customers.
    pub const TRAINED: &str = "complaints.vulnerability_trained";
}

/// The authority level a person decides and authorises at, as a limit in
/// their authority profile. Nobody has one unless their profile says so.
pub const AUTHORITY: &str = "complaints_authority";

/// Commands only that module's own services send. Refused here whatever the seat.
const SERVICE_ONLY: [&str; 5] = ["activate_timers", "tick", "mark_published", "attach_population", "complete_population"];

/// Why the workbench will not pass something on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not something the workbench sends or asks.
    Bad(String),
    /// Not for this person.
    Forbidden(String),
}

fn bad<T>(why: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::Bad(why.into()))
}

fn forbidden<T>(why: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::Forbidden(why.into()))
}

/// The seats a member of staff may take, their authority level and
/// whether they are trained to work with vulnerable customers.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let holds = |action: &str| staff.holds_action(party, action, at);
    let seats: Vec<&str> = [
        ("intake", holds(actions::RECORD) || holds(actions::HANDLE)),
        ("complaints", holds(actions::HANDLE)),
        ("legal", holds(actions::LEGAL)),
        ("conduct", holds(actions::CONDUCT)),
    ]
    .into_iter()
    .filter(|(_, held)| *held)
    .map(|(seat, _)| seat)
    .collect();
    let authority = staff.own_access(party, at)["profile"]["limits"][AUTHORITY].as_i64().unwrap_or(0).clamp(0, 255);
    json!({ "seats": seats, "authority": authority, "trained": holds(actions::TRAINED) })
}

fn seated(standing: &Value, seat: &str) -> bool {
    standing["seats"].as_array().is_some_and(|s| s.iter().any(|x| x == seat))
}

fn field<'a>(fields: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, Refusal> {
    match fields.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
        Some(value) => Ok(value),
        None => bad(format!("{name} is needed")),
    }
}

fn day(fields: &BTreeMap<String, String>, name: &str) -> Result<NaiveDate, Refusal> {
    field(fields, name)?.parse().or_else(|_| bad(format!("{name} is a date, as 2027-11-15")))
}

/// A question for the complaints module, from its name and the fields the
/// page sent. The person asking is always the one signed in, and conduct
/// cases are asked for in the role the person holds.
pub fn question(name: &str, fields: &BTreeMap<String, String>, actor: &PartyId, standing: &Value) -> Result<Value, Refusal> {
    let works_cases = ["complaints", "legal", "conduct"].iter().any(|seat| seated(standing, seat));
    let conduct_role = ["conduct", "legal"].into_iter().find(|seat| seated(standing, seat));
    let restricted = || forbidden("conduct cases are open to the conduct function and legal only");
    match name {
        "conduct_case" | "open_conduct_cases" => {
            let Some(role) = conduct_role else { return restricted() };
            Ok(match name {
                "conduct_case" => json!({ "conduct_case": { "id": field(fields, "id")?, "role": role } }),
                _ => json!({ "open_conduct_cases": { "role": role } }),
            })
        }
        _ if !works_cases => forbidden("the register is open to the complaints team, legal and the conduct function"),
        "queue" => Ok(json!({ "queue": { "limit": 200 } })),
        "complaint" => Ok(json!({ "complaint": { "id": field(fields, "id")?, "actor": actor } })),
        "running" => Ok(json!({ "running": { "id": field(fields, "id")? } })),
        "subject_record" => Ok(json!({ "subject_record": { "id": field(fields, "id")? } })),
        "complaints_of" => Ok(json!({ "complaints_of": { "party": field(fields, "party")? } })),
        "register" | "report" => Ok(json!({ name: { "from": day(fields, "from")?, "to": day(fields, "to")? } })),
        _ => bad(format!("{name} is not a question the workbench asks")),
    }
}

/// The name of a command and its fields, if it is one a person may send in
/// this seat.
fn named<'a>(command: &'a mut Value, seat: &str, standing: &Value) -> Result<(String, &'a mut Value), Refusal> {
    if !["intake", "complaints", "legal", "conduct"].contains(&seat) {
        return bad("a command is sent as intake, complaints, legal or conduct");
    }
    if !seated(standing, seat) {
        return forbidden(format!("your roles do not let you act as {seat}"));
    }
    let Some(fields) = command.as_object_mut().filter(|o| o.len() == 1) else {
        return bad("a command is one name with its fields");
    };
    let Some((name, body)) = fields.iter_mut().next() else {
        return bad("a command is one name with its fields");
    };
    if SERVICE_ONLY.contains(&name.as_str()) {
        return bad(format!("{name} is not sent by a person"));
    }
    Ok((name.clone(), body))
}

/// The staff whose standing a command depends on: the handler a complaint
/// is assigned to, or whoever authorises a remediation (the sender, unless
/// the command names someone else).
pub fn depends_on(command: &Value, actor: &PartyId) -> Option<PartyId> {
    if let Some(handler) = command["assign"]["handler"].as_str() {
        return Some(PartyId::from(handler));
    }
    command
        .get("record_remediation")
        .map(|r| r["authorised_by"].as_str().map_or_else(|| actor.clone(), PartyId::from))
}

/// The envelope that carries one person's command to the complaints
/// module, with what the staff state knows written in. `other` is the
/// standing of the person [`depends_on`] named.
pub fn envelope(
    key: &str,
    actor: &PartyId,
    seat: &str,
    at: At,
    mut command: Value,
    standing: &Value,
    other: Option<(&PartyId, &Value)>,
) -> Result<Value, Refusal> {
    let (name, body) = named(&mut command, seat, standing)?;
    match (name.as_str(), other) {
        ("assign", Some((handler, theirs))) => {
            if !seated(theirs, "complaints") {
                return forbidden(format!("{handler} does not work complaints"));
            }
            body["authority"] = theirs["authority"].clone();
            body["trained_for_vulnerability"] = theirs["trained"].clone();
        }
        ("record_remediation", Some((authoriser, theirs))) => {
            if !seated(theirs, "complaints") {
                return forbidden(format!("{authoriser} does not work complaints"));
            }
            body["authorised_by"] = json!(authoriser);
            body["authority"] = theirs["authority"].clone();
        }
        ("assign" | "record_remediation", None) => return bad("the person the command names could not be looked up"),
        _ => {}
    }
    let when = DateTime::from_timestamp(at, 0).unwrap_or_default();
    Ok(json!({ "key": key, "caller": seat, "actor": actor, "at": when, "command": command }))
}
