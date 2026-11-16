//! The employer portal's half of group scheme administration
//! (`docs/group.md`, "Interfaces").
//!
//! An employer administrator uploads the scheme's membership file, reads
//! its preview, approves or withdraws it, declares pools, and reads the
//! scheme's standing and invoices. The group service holds all of that;
//! the portal decides who may ask. It calls the service with its own Entra
//! token, under the app role `employer-portal`, and names the signed-in
//! administrator as the actor.
//!
//! The portal knows an administrator by the scheme's policy, and the group
//! service names schemes by its own ID: every call first asks the service
//! which scheme is on the policy, once the portal has found that the
//! signed-in person administers it.

use ic_telemetry::Traced;
use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};

/// The caller name, and the app role the portal's token carries for it.
pub const CALLER: &str = "employer-portal";

/// The caller name for group administration staff, and the second app
/// role the portal's token carries on the group app.
pub const STAFF_CALLER: &str = "group-admin";

/// Staff actions that open the group console.
pub mod actions {
    /// Read schemes, files, members, mappings and the exception queue.
    pub const VIEW: &str = "group.view";
    /// Act on them as group administration staff.
    pub const ADMIN: &str = "group.admin";
}

/// The largest file the group service takes.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;

/// What the group service answered: its status and its body.
pub type Answered = (u16, Value);

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Answered, String>> + Send + 'a>>;

/// The group service, as the portal calls it.
pub trait Group: Send + Sync {
    /// Store a membership file in quarantine.
    fn upload(&self, scheme: &str, file: &str, actor: &str, bytes: Vec<u8>) -> Asked<'_>;
    /// Send a command under the employer portal's name.
    fn command(&self, key: &str, actor: &str, command: Value) -> Asked<'_>;
    /// Ask a question.
    fn ask(&self, question: Value) -> Asked<'_>;
    /// Send a command as group administration staff.
    fn staff_command(&self, key: &str, actor: &str, command: Value) -> Asked<'_>;
    /// Try a mapping against a sample file, as staff. Nothing is stored.
    fn try_mapping(&self, scheme: &str, trial: Value) -> Asked<'_>;
}

/// The fields of a member an employer sees, as the group service names
/// them. The service already leaves out health, loadings, reasons and the
/// person; the portal keeps to this list as well, so that a field added
/// there later reaches an employer only once it is added here.
pub const MEMBER_FIELDS: [&str; 13] = [
    "scheme",
    "employee",
    "category",
    "salary_minor",
    "hours_hundredths",
    "billing_group",
    "employment_start",
    "joined",
    "left",
    "in_force",
    "on_leave_since",
    "covers",
    "above_automatic",
];
const COVER_FIELDS: [&str; 4] = ["cover", "cover_type", "formula_minor", "automatic_minor"];
const ABOVE_FIELDS: [&str; 5] = ["cover", "asked_minor", "state", "in_place_minor", "from"];

fn only(value: &Value, fields: &[&str]) -> Value {
    let kept = value
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| fields.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()));
    Value::Object(kept.collect())
}

fn each(value: &Value, fields: &[&str]) -> Value {
    Value::Array(value.as_array().into_iter().flatten().map(|v| only(v, fields)).collect())
}

/// A member as an employer sees them: the listed fields and nothing else.
pub fn employer_member(answer: &Value) -> Value {
    let mut member = only(answer, &MEMBER_FIELDS);
    member["covers"] = each(&answer["covers"], &COVER_FIELDS);
    member["above_automatic"] = each(&answer["above_automatic"], &ABOVE_FIELDS);
    member
}

/// The commands an administrator may send. Every other command is staff's
/// or another module's, and the group service would refuse it anyway; the
/// portal does not pass it on.
pub const EMPLOYER_COMMANDS: [&str; 4] = ["approve_file", "withdraw_file", "declare_pool", "choose_health_plan"];

/// Which scheme is on a policy.
pub fn scheme_of(policy: &str) -> Value {
    json!({ "ask": "scheme_of", "policy": policy })
}

/// The scheme the answer to [`scheme_of`] names, if any.
pub fn named_scheme(answer: &Value) -> Option<String> {
    answer["schemes"].get(0).and_then(Value::as_str).map(str::to_string)
}

/// The questions an administrator may ask, each about their scheme: its
/// standing, its invoices, one file with its preview (`file`), or one
/// member as the employer sees them (`employee`): category, salary, cover
/// and whether cover above the automatic amount is in place, never health,
/// a loading, a reason or who the person is. Open exceptions, the mapping
/// and the driver's own work are staff's.
pub fn question(name: &str, scheme: &str, given: impl Fn(&str) -> Option<String>) -> Result<Value, String> {
    let named = |field: &str| {
        let value = given(field).ok_or_else(|| format!("{name} needs ?{field}="))?;
        self::name(&value)
            .map(str::to_string)
            .ok_or_else(|| format!("{field} is letters, digits, dots, dashes and underscores"))
    };
    Ok(match name {
        "scheme" => json!({ "ask": "scheme", "scheme": scheme }),
        "invoices" => json!({ "ask": "invoices", "scheme": scheme }),
        "file" => json!({ "ask": "file", "scheme": scheme, "file": named("file")? }),
        "member" => json!({ "ask": "member", "scheme": scheme, "employee": named("employee")? }),
        _ => return Err("ask for the scheme, its invoices, a file or a member".to_string()),
    })
}

/// The command the page sent, with the scheme set to the one on the path,
/// or why it is not one an administrator sends.
pub fn command(scheme: &str, sent: &Value) -> Result<Value, String> {
    let Some((name, body)) = sent.as_object().filter(|o| o.len() == 1).and_then(|o| o.iter().next()) else {
        return Err("a command is one object naming the command".to_string());
    };
    if !EMPLOYER_COMMANDS.contains(&name.as_str()) {
        return Err(format!("{name} is not a command the employer portal sends"));
    }
    let Some(fields) = body.as_object() else {
        return Err(format!("{name} takes an object"));
    };
    let mut fields = fields.clone();
    fields.insert("scheme".to_string(), json!(scheme));
    Ok(json!({ name: fields }))
}

/// The commands staff send from the console. The driver's, the member's
/// and other modules' are never sent from here, and a file is received
/// only by uploading it.
pub const STAFF_COMMANDS: [&str; 18] = [
    "register_scheme",
    "amend_terms",
    "set_mapping",
    "freeze",
    "unfreeze",
    "resume",
    "confirm_held",
    "decide_backdating",
    "approve_file",
    "withdraw_file",
    "declare_pool",
    "reinstate",
    "request_excess_again",
    "resolve_match",
    "agree_renewal",
    "terminate",
    "split_scheme",
    "choose_health_plan",
];

/// Underwriting's command to offer a member the loading the employer has
/// paid in one total, once too few members hold underwritten cover for
/// the total to hide whose it is. `listed` is the group module's
/// `loadings_in_total` answer, which names the proposal behind each case.
pub fn reoffer(listed: &Value, case: u64, reason: &str) -> Result<Value, String> {
    if reason.trim().is_empty() {
        return Err("say why the member is being asked".to_string());
    }
    let proposal = listed["cases"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["case"] == case)
        .and_then(|c| c["proposal"].as_str())
        .ok_or("that case has no loading in the employer's total")?;
    Ok(json!({ "reoffer_excess_terms": { "proposal": proposal, "reason": reason.trim() } }))
}

/// Whether a member of staff may open the group console, and whether they
/// may act there.
pub fn standing(staff: &crate::staff::StaffState, party: &ic_core::PartyId, at: crate::staff::At) -> Value {
    let admin = staff.holds_action(party, actions::ADMIN, at);
    let view = admin || staff.holds_action(party, actions::VIEW, at);
    json!({ "staff": party, "view": view, "admin": admin })
}

/// A question staff may ask, from its name and the query's fields: a
/// scheme's standing, invoices or mapping, one file, one member, the
/// scheme on a policy, or the open exceptions (all, or one scheme's). The
/// outbox and the driver's work are the driver's alone.
pub fn staff_question(name: &str, given: impl Fn(&str) -> Option<String>) -> Result<Value, String> {
    let named = |field: &str| {
        let value = given(field).ok_or_else(|| format!("{name} needs ?{field}="))?;
        self::name(&value)
            .map(str::to_string)
            .ok_or_else(|| format!("{field} is letters, digits, dots, dashes and underscores"))
    };
    Ok(match name {
        "exceptions" => match given("scheme").filter(|s| !s.is_empty()) {
            Some(_) => json!({ "ask": "exceptions", "scheme": named("scheme")? }),
            None => json!({ "ask": "exceptions" }),
        },
        "scheme" | "invoices" | "mapping" => json!({ "ask": name, "scheme": named("scheme")? }),
        "file" => json!({ "ask": "file", "scheme": named("scheme")?, "file": named("file")? }),
        "member" => json!({ "ask": "member", "scheme": named("scheme")?, "employee": named("employee")? }),
        // Whose loading is in the employer's total, for asking one to take it over.
        "loadings_in_total" => json!({ "ask": "loadings_in_total", "scheme": named("scheme")? }),
        // The people the customer master found a member might be, for deciding the match.
        "possible_match" => json!({ "ask": "possible_match", "scheme": named("scheme")?, "employee": named("employee")? }),
        "scheme_of" => json!({ "ask": "scheme_of", "policy": named("policy")? }),
        _ => return Err(format!("{name} is not a question the group console asks")),
    })
}

/// The command staff sent, if it is one the console passes on: one object
/// naming a command in [`STAFF_COMMANDS`].
pub fn staff_command(sent: &Value) -> Result<&str, String> {
    let name = sent
        .as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.keys().next())
        .ok_or("a command is one object naming the command")?;
    STAFF_COMMANDS
        .iter()
        .find(|c| **c == name)
        .copied()
        .ok_or_else(|| format!("{name} is not a command staff send from the console"))
}

/// A scheme or file name as the group service takes it, or `None`.
pub fn name(raw: &str) -> Option<&str> {
    (!raw.is_empty() && raw.len() <= 64 && raw.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && !raw.contains("..")).then_some(raw)
}

/// The group service, reached with the portal's own Entra token for the
/// group app.
pub struct Hosted {
    pub base: String,
    pub scope: String,
    pub credential: ic_azure::credential::AzureCredential,
    pub http: reqwest::Client,
}

impl Hosted {
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Answered, String> {
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let response = request.bearer_auth(token).traced().send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        if status == 503 {
            return Err("the group service asked us to try again".to_string());
        }
        Ok((status, response.json::<Value>().await.unwrap_or(Value::Null)))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.trim_end_matches('/'))
    }
}

impl Group for Hosted {
    fn upload(&self, scheme: &str, file: &str, actor: &str, bytes: Vec<u8>) -> Asked<'_> {
        let request = self
            .http
            .post(self.url(&format!("/v1/schemes/{scheme}/files/{file}")))
            .header("x-ic-caller", CALLER)
            .header("x-ic-actor", actor)
            .body(bytes);
        Box::pin(self.send(request))
    }

    fn command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        let body = json!({ "key": key, "caller": CALLER, "actor": actor, "command": command });
        let request = self.http.post(self.url("/v1/commands")).json(&body);
        Box::pin(self.send(request))
    }

    fn ask(&self, question: Value) -> Asked<'_> {
        let request = self.http.post(self.url("/v1/questions")).json(&question);
        Box::pin(self.send(request))
    }

    fn staff_command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        let body = json!({ "key": key, "caller": STAFF_CALLER, "actor": actor, "command": command });
        let request = self.http.post(self.url("/v1/commands")).json(&body);
        Box::pin(self.send(request))
    }

    fn try_mapping(&self, scheme: &str, trial: Value) -> Asked<'_> {
        let request = self.http.post(self.url(&format!("/v1/schemes/{scheme}/mappings/try"))).json(&trial);
        Box::pin(self.send(request))
    }
}
