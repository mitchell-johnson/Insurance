//! The MCP endpoint: every function of the portal as a Model Context
//! Protocol tool, for a person working through an MCP client.
//!
//! A tool is one of the portal's own routes. A call is dispatched through
//! the same router, with the caller's own bearer token, so it passes every
//! check the screens pass: who signed in, the session and its revocation,
//! staff templates, grants and limits, maker and checker, and each module's
//! own rules. Nothing here decides what a person may do, and nothing is
//! reachable here that is not reachable on the screens. A caller is offered
//! only the tools of the sign-ins they hold (customer, staff, operations),
//! and each call is checked again by the route.
//!
//! Transport: Streamable HTTP, JSON responses only, no server-sent stream.
//! Sign-in: OAuth 2.0 bearer tokens from the two tenants the portal already
//! trusts, advertised by the protected resource metadata (RFC 9728) at
//! `/.well-known/oauth-protected-resource/mcp`.
//!
//! Steps that need the customer's enrolled phone (the device's key signs a
//! payment, an approval, a disclosure) are refused here as they are to any
//! caller without the device: a tool can prepare, the phone signs.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde_json::{json, Map, Value};
use tower::ServiceExt;

use crate::http::{router, AppState, OPERATIONS};
use crate::ui::UiConfig;

/// The protocol versions this endpoint speaks, newest first.
pub const VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// The largest response a tool hands back whole (a PDF, an extract).
const MAX_RESPONSE: usize = 32 * 1024 * 1024;

/// Which sign-in a tool needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Realm {
    /// Anyone, signed in or not (the public status).
    Public,
    /// A customer, adviser, advice provider or employer administrator:
    /// External ID.
    Customer,
    /// A member of staff registered on the platform: Entra ID.
    Staff,
    /// The workforce role that runs the platform.
    Operations,
}

/// What a tool sends as the request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Payload {
    None,
    /// A JSON object, described for the caller.
    Json(&'static str),
    /// A file's bytes, given as base64.
    File(&'static str),
}

/// One function of the system.
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub realm: Realm,
    pub method: Method,
    /// The route, with `{param}` for each path parameter.
    pub path: &'static str,
    /// Allowed values of path parameters that name one of a fixed set.
    pub choices: Vec<(&'static str, Vec<&'static str>)>,
    /// Query parameters, described. Empty for none.
    pub query: &'static str,
    pub body: Payload,
}

impl Tool {
    fn writes(&self) -> bool {
        self.method != Method::GET
    }

    /// The path parameters, in order.
    pub fn params(&self) -> Vec<&'static str> {
        self.path
            .split('/')
            .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            .collect()
    }

    /// The tool as MCP lists it.
    pub fn listed(&self) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for param in self.params() {
            let mut schema = json!({ "type": "string", "minLength": 1 });
            if let Some((_, values)) = self.choices.iter().find(|(name, _)| *name == param) {
                schema["enum"] = json!(values);
            }
            properties.insert(param.to_string(), schema);
            required.push(param);
        }
        if !self.query.is_empty() {
            properties.insert(
                "query".to_string(),
                json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": self.query }),
            );
        }
        match self.body {
            Payload::None => {}
            Payload::Json(about) => {
                properties.insert("body".to_string(), json!({ "type": "object", "description": about }));
                required.push("body");
            }
            Payload::File(about) => {
                properties.insert(
                    "content_base64".to_string(),
                    json!({ "type": "string", "contentEncoding": "base64", "description": about }),
                );
                required.push("content_base64");
            }
        }
        if self.writes() {
            properties.insert(
                "idempotency_key".to_string(),
                json!({ "type": "string", "maxLength": 100, "description": "Send the same key to retry this action safely: it is done once. A fresh key is made when none is given." }),
            );
        }
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
            "annotations": {
                "title": self.title,
                "readOnlyHint": !self.writes(),
                "destructiveHint": self.method == Method::DELETE,
                "idempotentHint": self.method == Method::GET || self.method == Method::PUT,
                "openWorldHint": false,
            },
        })
    }
}

fn tool(name: &'static str, title: &'static str, realm: Realm, method: Method, path: &'static str, description: &'static str) -> Tool {
    Tool {
        name,
        title,
        description,
        realm,
        method,
        path,
        choices: vec![],
        query: "",
        body: Payload::None,
    }
}

trait With {
    fn query(self, about: &'static str) -> Self;
    fn json(self, about: &'static str) -> Self;
    fn file(self, about: &'static str) -> Self;
    fn choose(self, param: &'static str, values: Vec<&'static str>) -> Self;
}

impl With for Tool {
    fn query(mut self, about: &'static str) -> Self {
        self.query = about;
        self
    }
    fn json(mut self, about: &'static str) -> Self {
        self.body = Payload::Json(about);
        self
    }
    fn file(mut self, about: &'static str) -> Self {
        self.body = Payload::File(about);
        self
    }
    fn choose(mut self, param: &'static str, values: Vec<&'static str>) -> Self {
        self.choices.push((param, values));
        self
    }
}

fn with_me(mut names: Vec<&'static str>) -> Vec<&'static str> {
    names.insert(0, "me");
    names
}

/// Routes deliberately not offered as tools, and why.
pub const EXCLUDED: [(&str, &str, &str); 28] = [
    ("GET", "/healthz", "a probe for the platform, not a function"),
    ("GET", "/readyz", "a probe for the platform, not a function"),
    ("GET", "/app/v1/sync", "the device protocol: only an enrolled device, proving its key, syncs"),
    (
        "POST",
        "/app/v1/challenges",
        "the device protocol: only an enrolled device asks for a challenge to sign",
    ),
    ("POST", "/app/v1/devices", "the device protocol: enrolling binds a phone's own key"),
    (
        "POST",
        "/internal/v1/commands",
        "module to module: other services publish here under their own identity",
    ),
    ("GET", "/internal/v1/pending", "module to module: owning modules collect their requests here"),
    ("GET", "/status", "a web page; its content is the status tool"),
    ("GET", "/", "a web page"),
    ("GET", "/customer", "a web page"),
    ("GET", "/adviser", "a web page"),
    ("GET", "/employer", "a web page"),
    ("GET", "/staff", "a web page"),
    ("GET", "/ops", "a web page"),
    ("GET", "/complaints", "a web page"),
    ("GET", "/finance", "a web page"),
    ("GET", "/claims", "a web page"),
    ("GET", "/reinsurance", "a web page"),
    ("GET", "/payments", "a web page"),
    ("GET", "/collections", "a web page"),
    ("GET", "/documents", "a web page"),
    ("GET", "/group", "a web page"),
    ("GET", "/underwriting", "a web page"),
    ("GET", "/pricing", "a web page"),
    ("GET", "/replacements", "a web page"),
    ("GET", "/commission", "a web page"),
    ("GET", "/ui/*", "the pages' scripts, styles, font and sign-in settings"),
    ("*", "/mcp, /.well-known/*", "this endpoint and its sign-in discovery"),
];

/// Every function, as a tool.
pub fn catalogue() -> Vec<Tool> {
    use Method as M;
    use Realm::{Customer as C, Operations as O, Public as P, Staff as S};
    vec![
        tool("status", "Service status", P, M::GET, "/status.json", "Whether the service is running normally, in plain words, as the public status page shows it."),
        // ---- The customer app and the partner portals (External ID).
        tool("my_account", "My account", C, M::GET, "/app/v1/me", "Who you are signed in as, your policies and your relationship to each (policyholder, life insured, member, adviser, administrator, delegate)."),
        tool("my_profile", "My customer record", C, M::GET, "/app/v1/profile", "Your own record in the customer master: identity, contact, tax and organisation fields, with its version."),
        tool("change_my_profile", "Change my customer record", C, M::POST, "/app/v1/profile/commands", "Change your own contact details or a consent, or place, ask to remove or cancel the removal of a hold on your own money going out. Most of these need your enrolled device.")
            .json(r#"{"command": "record" | "set_consent" | "place_spending_hold" | "request_hold_removal" | "cancel_hold_removal", "version"?: "<as my_profile gave it>", "facts"?: [{"field": "email", "value": "..."}], ...}"#),
        tool("export_my_data", "Everything held about me", C, M::GET, "/app/v1/profile/export", "A copy of everything the insurer holds about you (a Privacy Act access request). Needs your enrolled device."),
        tool("my_spending_hold", "My hold on money going out", C, M::GET, "/app/v1/profile/spending-hold", "Whether your own hold on money leaving your policies is on, since when, and any removal waiting to take effect."),
        tool("my_document", "One of my documents", C, M::GET, "/app/v1/documents/{id}/pdf", "A document you own or were sent a copy of, as a PDF. Anyone else's is not found, the same as one that does not exist."),
        tool("disclosure_to_sign", "Disclosure to sign", C, M::POST, "/app/v1/proposals/{proposal}/lives/{life}/disclosures/to-sign", "For the person insured on a proposal: the exact bytes to sign for their health and lifestyle answers.")
            .json(r#"{"step": "disclosure" | "interview", "answers": {...}, "adviser_entered": false}"#),
        tool("submit_disclosure", "Submit a signed disclosure", C, M::POST, "/app/v1/proposals/{proposal}/lives/{life}/disclosures", "Submit the answers with the message disclosure_to_sign gave and the enrolled device's signature over it.")
            .json(r#"{"step", "answers", "adviser_entered", "message": "<base64 from disclosure_to_sign>", "signature": "<the device key's>"}"#),
        tool("consent_to_sign", "Consent to sign", C, M::GET, "/app/v1/proposals/{proposal}/lives/{life}/consents/{kind}/to-sign", "For the life on a proposal: the exact bytes to sign to give a consent, and whether this kind needs the device's signature.")
            .choose("kind", crate::disclosures::CONSENTS.to_vec()),
        tool("give_consent", "Give or withdraw a consent", C, M::POST, "/app/v1/proposals/{proposal}/lives/{life}/consents/{kind}", "Give or withdraw a consent for the life on a proposal. Adviser access, the employer seeing a loading and the replacement disclosure need the enrolled device's signature.")
            .choose("kind", crate::disclosures::CONSENTS.to_vec())
            .json(r#"{"granted": true | false, "signature"?: "<the device key's, over consent_to_sign's bytes>"}"#),
        tool("replacement_statement", "What a replacement changes", C, M::GET, "/app/v1/proposals/{proposal}/lives/{life}/consents/replacement_disclosure/statement", "Before signing the replacement disclosure: the policy being replaced, the cover that is kept, lost and gained."),
        tool("my_policy", "A policy", C, M::GET, "/app/v1/policies/{policy}", "A policy as you may see it: covers, billing, claims, documents and requests, limited by your relationship to it."),
        tool("represented_person", "Someone I act for", C, M::GET, "/app/v1/represented/{party}", "For an attorney, guardian or holder of a written authority: the policies of the person you act for, and your authority."),
        tool("represented_policy", "A policy of someone I act for", C, M::GET, "/app/v1/represented/{party}/policies/{policy}", "One policy of the person you act for, as they see it, without their health information."),
        tool("appoint_adviser", "Appoint an adviser", C, M::POST, "/app/v1/policies/{policy}/advisers", "As policyholder, appoint an adviser to service the policy.")
            .json(r#"{"adviser": "<party>", "engagement": "<the adviser's engagement>"}"#),
        tool("remove_adviser", "Remove an adviser", C, M::DELETE, "/app/v1/policies/{policy}/advisers/{adviser}", "As policyholder, end an adviser's access to the policy at once."),
        tool("family_consent", "Let the policyholder see my health details", C, M::PUT, "/app/v1/policies/{policy}/consent", "As an adult insured on a family policy, let the policyholder see your sensitive rows, or stop it.")
            .json(r#"{"consent": true | false}"#),
        tool("set_delegate", "Let someone look at a policy", C, M::PUT, "/app/v1/policies/{policy}/delegates/{party}", "As policyholder, let someone see the policy (viewer, financial or support), change their access, or end it.")
            .json(r#"{"access": "viewer" | "financial" | "support" | null}"#),
        tool("my_requests", "My requests", C, M::GET, "/app/v1/requests", "Every request you made or that waits for you, with where it stands."),
        tool("make_request", "Ask for a change", C, M::POST, "/app/v1/requests", "Ask for a change to a policy (or prepare one as its adviser). Requests that move money need the enrolled device's signature; an adviser's needs the customer's approval.")
            .json(r#"{"id": "<your own ID for it>", "policy": "<policy>", "kind": "<what you ask, e.g. change_sum_insured>", "module": "<owning module, e.g. policy-servicing>", "payload": {...}, "signed"?: {"challenge", "signature"}}"#),
        tool("withdraw_request", "Withdraw a request", C, M::DELETE, "/app/v1/requests/{id}", "Withdraw a request of yours that has not been acted on."),
        tool("approve_request", "Approve a request prepared for me", C, M::POST, "/app/v1/requests/{id}/approve", "Approve a change your adviser, a support person or a joint owner prepared. Needs the enrolled device's signature over this approval.")
            .json(r#"{"signed": {"challenge": "<from the device>", "signature": "<the device key's>"}}"#),
        tool("my_devices", "My devices", C, M::GET, "/app/v1/devices", "The phones enrolled to act for you."),
        tool("revoke_device", "Revoke a device", C, M::DELETE, "/app/v1/devices/{device}", "Revoke an enrolled phone at once, for example one that was lost. It is told to wipe what it holds."),
        tool("policy_claims", "Claims on a policy", C, M::GET, "/app/v1/policies/{policy}/claims", "For a policy's adviser or a scheme's administrator: each claim, its status and dates, never its cause."),
        tool("adviser_book", "My book", C, M::GET, "/adviser/v1/book", "For an adviser: your engagements, standing, the policies you service and what is held from you."),
        tool("adviser_held_balance", "My held commission", C, M::GET, "/adviser/v1/held-balance", "For an adviser: your own held money and the reason for each amount."),
        tool("adviser_practice", "My practice", C, M::GET, "/adviser/v1/practice", "For an advice provider or group: each adviser engaged today and the policies they service."),
        tool("adviser_statement", "My commission statement", C, M::GET, "/adviser/v1/statements/{number}", "Your own commission statement by number, with its lines, balances and totals."),
        tool("adviser_replacements", "My replacement matches", C, M::GET, "/adviser/v1/replacements", "Layers you wrote that the replacement rule matched to cover given up on the same life."),
        tool("dispute_replacement", "Dispute a replacement match", C, M::POST, "/adviser/v1/replacements/dispute", "Dispute one of your replacement matches, with your reason. The conduct officer decides.")
            .json(r#"{"layer": "<policy/cover/layer>", "reason": "<why>"}"#),
        tool("employer_schemes", "My schemes", C, M::GET, "/employer/v1/schemes", "For a scheme administrator: each scheme, its members and billing."),
        tool("set_scheme_admin", "Name or remove a scheme administrator", C, M::PUT, "/employer/v1/schemes/{policy}/admins/{party}", "Name another administrator of your scheme, or remove one. A scheme keeps at least one.")
            .json(r#"{"admin": true | false}"#),
        tool("set_second_admin", "Changes needing a second administrator", C, M::PUT, "/employer/v1/schemes/{policy}/second-admin", "Choose which kinds of change on your scheme need a second administrator to approve.")
            .json(r#"{"changes": ["remove_member", ...]}"#),
        tool("scheme_command", "Act on my scheme's membership", C, M::POST, "/employer/v1/schemes/{policy}/group/commands", "Approve or withdraw an uploaded membership file, or declare a pool's aggregate.")
            .json(r#"{"command": "approve_file" | "withdraw_file" | "declare_pool", ...}"#),
        tool("upload_membership_file", "Upload a membership file", C, M::POST, "/employer/v1/schemes/{policy}/group/files/{file}", "Upload a membership file (CSV) to quarantine. Approve it with scheme_command after checking its preview.")
            .file("The file's bytes, base64. At most 32 MB."),
        tool("scheme_question", "Ask about my scheme", C, M::GET, "/employer/v1/schemes/{policy}/group/{question}", "Your scheme on the group service: the scheme, its invoices, an uploaded file's preview, or one member's record.")
            .choose("question", vec!["scheme", "invoices", "file", "member"])
            .query("`file` for file; `employee` for member."),
        // ---- Staff (Entra ID).
        tool("staff_me", "My staff access", S, M::GET, "/staff/v1/me", "Your roles, reach, grants and recent access."),
        tool("staff_admin", "Staff administration", S, M::GET, "/staff/v1/admin", "For a staff administrator: grants, approvals waiting, alignment with the entitlement source."),
        tool("staff_access", "Change staff access", S, M::POST, "/staff/v1/access", "Request, approve or revoke a grant, break-glass, recertify, declare a personal connection, or administer staff.")
            .json(r#"{"<command>": {...}} as the staff module takes it, e.g. {"request_grant": {...}}"#),
        tool("staff_policy", "Open a policy", S, M::GET, "/staff/v1/policies/{policy}", "A policy as your role and the case you work on let you see it. Every opening is in the access log.")
            .query("`case`: the case or task you work on (required for most roles); `scope`: comma-separated scope keys such as queue:claims-life."),
        tool("check_caller", "Check a caller", S, M::POST, "/staff/v1/policies/{policy}/caller", "Someone has phoned about a policy: whether to talk to them, and the words to use if not. The contact is logged.")
            .json(r#"{"caller"?: "<their customer reference>", "case": "<case>", "scope"?: "<keys>"}"#),
        tool("staff_document", "A customer's document", S, M::GET, "/staff/v1/documents/{id}/pdf", "A customer's document as a PDF, for a purpose your role allows.")
            .query("`purpose`: why you need it."),
        tool("enter_change_for_customer", "Enter a change for a customer", S, M::POST, "/staff/v1/requests", "A change for a customer on the phone. The customer approves it; above your limit or backdated, a second member of staff agrees it first.")
            .json(r#"{"id", "policy", "kind", "module", "payload": {...}, "reason": "<why>", "case": "<case>"}"#),
        tool("second_approval", "Agree a colleague's change", S, M::POST, "/staff/v1/requests/{id}/second-approval", "As a second member of staff, agree a change a colleague entered."),
        tool("changes_for_second_approval", "Changes waiting for a second person", S, M::GET, "/staff/v1/requests", "Changes colleagues entered for customers that wait for a second member of staff, oldest first. Empty unless your roles include agreeing them."),
        tool("find", "Find a customer or a policy", S, M::GET, "/staff/v1/find", "From what you know: a name with a birth date (Kiri Ngata 7/3/1984), an email, a phone or IRD number, or a policy number in whole or in part. Each person comes with the policies they own or are insured on.")
            .query("`q`: what you know; `case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        tool("colleagues", "Colleagues to pick for work", S, M::GET, "/staff/v1/colleagues", "Current staff with their team and roles; with `action`, only those who may take it today.")
            .query("`action`: a staff action such as `claims.work`, optional."),
        tool("marker_reviews", "Vulnerability markers to review", S, M::GET, "/staff/v1/care/reviews", "For staff trained in vulnerability support: markers due for review and prompts to consider one."),
        tool("decide_marker_review", "Decide a marker review", S, M::POST, "/staff/v1/care/reviews/{review}", "Renew, change or remove a vulnerability marker, or close a prompt with none needed.")
            .json(r#"{"party", "outcome": "renew" | "change" | "remove" | "no_marker", "kind"?, "review_on"?: "YYYY-MM-DD", "adjustments"?: [...]}"#),
        tool("workbench", "Held balance workbench", S, M::GET, "/staff/v1/workbench", "Held money by age, parties with held or owed money, and reviews due."),
        tool("workbench_party", "One party's held position", S, M::GET, "/staff/v1/workbench/{party}", "A party's held lines, net position, documents, cases and actions."),
        tool("proof_of_debt", "Proof of debt extract", S, M::GET, "/staff/v1/workbench/{party}/proof-of-debt", "The proof of debt extract for a party in liquidation or bankruptcy."),
        tool("workbench_action", "Act on held money", S, M::POST, "/staff/v1/workbench/actions", "Prepare, approve, decline, withdraw or resolve an action on held money (release, offset, hold pending, write off, record a dividend), or change the workbench's settings.")
            .json(r#"{"prepare": {...}} | {"approve": {"id", ...}} | {"decline": {...}} | {"withdraw": {"id"}} | {"resolve": {...}} | {"change_settings": {...}}"#),
        tool("complaints_question", "Ask the complaints module", S, M::GET, "/staff/v1/complaints/{question}", "The complaints queue, a complaint, its running timers, the register and reports; conduct cases for conduct and legal.")
            .choose("question", vec!["me", "queue", "complaint", "running", "subject_record", "complaints_of", "register", "report", "open_conduct_cases", "conduct_case"])
            .query("`id` for complaint, running, subject_record, conduct_case; `party` for complaints_of; `from`, `to` (YYYY-MM-DD) for register and report."),
        tool("complaints_command", "Act on a complaint", S, M::POST, "/staff/v1/complaints/commands", "A command to the complaints module in a seat you hold.")
            .json(r#"{"as": "<seat>", "command": {...in that module's JSON}}"#),
        tool("claims_question", "Ask the claims module", S, M::GET, "/staff/v1/claims/{question}", "A queue, a claim (the claimant's view), the work view of a claim, a policy's claim status, your cases, the book, the report, a remittance, or the definitions a cover can be bound to.")
            .choose("question", vec!["me", "queue", "claim", "work", "policy", "cases", "book", "report", "remittance", "definitions"])
            .query("`queue` for queue; `claim` for claim and work; `policy` for policy; `run` for remittance."),
        tool("claims_definition", "A definition's wording", S, M::GET, "/staff/v1/claims/definitions/{id}/{version}", "The wording of one policy definition at one version, with its hash."),
        tool("claims_command", "Act on a claim", S, M::POST, "/staff/v1/claims/commands", "A command to the claims module: notify, record evidence, decide, pay, prepare and approve a run, and the rest. You are recorded as the actor.")
            .json(r#"{"purpose": "<why>", "partition"?: 0, "command": {...in the claims module's JSON}}"#),
        tool("replacements", "Replacement matches", S, M::GET, "/staff/v1/replacements", "Every replacement match, and whether you may decide it."),
        tool("decide_replacement", "Decide a disputed replacement", S, M::POST, "/staff/v1/replacements/decide", "As conduct officer, decide a disputed match: a replacement takes the held commission back; not one makes the layer new business.")
            .json(r#"{"policy", "cover", "layer", "replacement": true | false}"#),
        tool("commission_question", "Ask the commission module", S, M::GET, "/staff/v1/commission/{question}", "Commission status, tasks, history, conduct cases, relations, positions, statements, held control, month-end blocks and clawbacks.")
            .choose("question", with_me(crate::commission::question_names()))
            .query("As the question needs: `party`, `as_at`, `payee`, `number`, `period`."),
        tool("commission_command", "Act in commission", S, M::POST, "/staff/v1/commission/commands", "A command to the commission module.")
            .json(r#"{"purpose": "<why>", "command": {"<name>": {...}}}"#),
        tool("payments_question", "Ask the payments module", S, M::GET, "/staff/v1/payments/{question}", "Runs, exceptions, matches to approve, a party's authorities, holds, submissions, control totals and compliance cases.")
            .choose("question", with_me(crate::payments::question_names()))
            .query("As the question needs: `account`, `exception`, `party`, `run`, `submission`, `case`."),
        tool("payments_command", "Act in payments", S, M::POST, "/staff/v1/payments/commands", "A command to the payments module in its own JSON.")
            .json(r#"{"type": "<command>", ...}"#),
        tool("servicing_waiting", "Customers' requests waiting for a person", S, M::GET, "/staff/v1/servicing/waiting", "Changes customers asked for that the customer master sent to a trained person first: who asked and what for, never why."),
        tool("servicing_carry_out", "Carry out a customer's request", S, M::POST, "/staff/v1/servicing/{id}/carry-out", "Having spoken with the customer, send their change to the core and close it with the core's answer.")
            .json(r#"{"conversation"?: "<the call or case reference>"}"#),
        tool("servicing_decline", "Decline a customer's request", S, M::POST, "/staff/v1/servicing/{id}/decline", "Decline a customer's change, in words the customer will read.")
            .json(r#"{"reason": "<why>"}"#),
        tool("collections_question", "Ask collections", S, M::GET, "/staff/v1/collections/{question}", "The day's queue of policies in arrears, a policy's dunning state, a team's tasks, a hardship case, notices, products with no sequence, the sequences in force, and the best-estimate sequence shipped for a product.")
            .choose("question", with_me(crate::collections::question_names()))
            .query("As the question needs: `policy`, `team` (collections, hardship or team_lead), `case`, `product`."),
        tool("collections_command", "Act in collections", S, M::POST, "/staff/v1/collections/commands", "A command to the collections module in its own JSON: record an outcome, propose or end an arrangement, apply for, decide or end hardship, ask for reinstatement, set a product's sequence.")
            .json(r#"{"type": "<command>", ...}"#),
        tool("documents_question", "Ask documents", S, M::GET, "/staff/v1/documents/{question}", "Templates and where each stands, legal holds, open notice defects, documents the core's events require, letters other modules asked for, sends in progress, settings, one document, defect, letter request or template, the template in force, where a part is used, the notices a template version issued, the report, a policy's documents and a kind's retention.")
            .choose("question", with_me(crate::correspondence::question_names()))
            .query("As the question needs: `id`, `version`, `request`, `part`, `policy`, `kind`, `from`, `to` (dates)."),
        tool("documents_command", "Act in documents", S, M::POST, "/staff/v1/documents/commands", "A command to the documents module as `{\"type\": name, ...}`: legal holds, notice defects, disabling, submitting, testing, approving and activating templates, parts, settings, retention, dismissing a trigger, retrying a delivery, cancelling a requested letter, shredding a party. The caller is named from your own actions.")
            .json(r#"{"type": "<command>", ...}"#),
        tool("group_question", "Ask group administration", S, M::GET, "/staff/v1/group/{question}", "Exceptions, a scheme, its invoices and mapping, an uploaded file, a member, a possible match, or the scheme a policy belongs to.")
            .choose("question", vec!["me", "exceptions", "scheme", "invoices", "mapping", "file", "member", "possible_match", "scheme_of", "loadings_in_total"])
            .query("`scheme`, `file`, `employee`, `policy` as the question needs."),
        tool("group_command", "Act in group administration", S, M::POST, "/staff/v1/group/commands", "A command to group administration.")
            .json(r#"{"command": "<one of the group commands>", ...}"#),
        tool("group_reoffer", "Offer a member their loading", S, M::POST, "/staff/v1/group/reoffer", "When too few members hold underwritten cover for the employer's loading total to hide whose it is, ask underwriting to offer one member the loading on their own cover. Nothing moves until they agree on their device.")
            .json(r#"{"scheme": "<scheme>", "case": <case number from loadings_in_total>, "reason": "<why>"}"#),
        tool("try_mapping", "Try a membership file mapping", S, M::POST, "/staff/v1/group/schemes/{scheme}/mappings/try", "What a sample file would do read with a mapping, before it is set.")
            .json(r#"{"mapping": {...}, "sample": "<CSV text>"}"#),
        tool("first_scheme_admin", "Name a scheme's first administrator", S, M::PUT, "/staff/v1/group/schemes/{policy}/first-admin", "Name the first administrator of a scheme that has none.")
            .json(r#"{"party": "<party>"}"#),
        tool("underwriting_question", "Ask underwriting", S, M::GET, "/staff/v1/underwriting/{question}", "A proposal, its disclosure basis, a queue, your workload, reports, exclusions and parked effects.")
            .choose("question", with_me(crate::underwriting::question_names()))
            .query("`proposal`, `queue`, `from`, `to`, `on` as the question needs."),
        tool("underwriting_command", "Act in underwriting", S, M::POST, "/staff/v1/underwriting/commands", "A command to underwriting in its own JSON. A non-disclosure remedy is prepared for a second senior to sign.")
            .json(r#"{"<command>": {...}}"#),
        tool("underwriting_decide", "Sign or withdraw a prepared remedy", S, M::POST, "/staff/v1/underwriting/{verb}/{id}", "As a second senior, sign and send a prepared remedy, or take it back.")
            .choose("verb", vec!["sign", "withdraw"])
            .json("{}"),
        tool("read_disclosures", "Read a life's disclosure", S, M::POST, "/staff/v1/underwriting/disclosures", "The intake's record of a life's answers, for a purpose your role allows.")
            .json(r#"{"proposal", "life", "purpose"}"#),
        tool("reinsurance_question", "Ask reinsurance", S, M::GET, "/staff/v1/reinsurance/{question}", "Exceptions, notices, abandonments, the sweep check, statements and notice retries.")
            .choose("question", with_me(crate::reinsurance::question_names()))
            .query("`period_end`, `number`, `seq` as the question needs."),
        tool("reinsurance_command", "Act in reinsurance", S, M::POST, "/staff/v1/reinsurance/commands", "A command to reinsurance in its own JSON. An abandonment is prepared for a second person to sign.")
            .json(r#"{"<command>": {...}}"#),
        tool("reinsurance_decide", "Sign or withdraw a prepared abandonment", S, M::POST, "/staff/v1/reinsurance/{verb}/{id}", "Sign and send, or take back, a prepared abandonment.")
            .choose("verb", vec!["sign", "withdraw"])
            .json("{}"),
        tool("finance_question", "Ask the finance ledger", S, M::GET, "/staff/v1/finance/{question}", "The close, checks, trial balance, movements, journals, returns, tax reconciliation, proposals, aged suspense, extracts and accounting groups.")
            .choose("question", {
                let mut names = with_me(crate::finance::question_names());
                names.push("return_definitions");
                names
            })
            .query("As the question needs: `period`, `today`, `fund`, `origin`, `id`, `code`, `version`, `product`, `as_at`, `kind`, `offset`, `limit`, `policy`, `partition`."),
        tool("finance_command", "Act in the finance ledger", S, M::POST, "/staff/v1/finance/commands", "A command to the finance ledger in its own JSON, under the roles you hold. Maker and checker are kept apart.")
            .json(r#"{"<command>": {...}}"#),
        tool("rate_table_dry_run", "Price a proposed rate table", S, M::POST, "/staff/v1/finance/table_dry_run", "For the finance lead or appointed actuary: price the book in force on a date under a current and a proposed rate table, with loadings.")
            .json(r#"{"as_at": "YYYY-MM-DD", "covers": ["life"], "per_life"?: false, "age_at_start"?: false, "current": "<.star source>", "proposed": "<.star source>"}"#),
        tool("pricing_question", "Ask unit pricing", S, M::GET, "/staff/v1/unit-pricing/{question}", "Funds, work, errors, prices, valuations and late prices.")
            .choose("question", with_me(crate::pricing::question_names()))
            .query("`id`, `fund`, `from`, `to`, `pricing_date`, `as_at` as the question needs."),
        tool("pricing_command", "Act in unit pricing", S, M::POST, "/staff/v1/unit-pricing/commands", "Prepare, approve or reject a price, suspend or resume a fund, raise or close a price error, republish.")
            .json(r#"{"<command>": {...}}"#),
        // ---- New business and the core's own view of a policy.
        tool("quote", "Price an application", S, M::POST, "/staff/v1/new-business/quote", "The first period's bill for an application, as the core would charge it. Nothing is written.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record.")
            .json(r#"{"policy", "number", "group", "policyholder", "product_code", "inception": "YYYY-MM-DD", "frequency", "lives": [...], "covers": [...]} as the core's IssuePolicy"#),
        tool("core_policy", "A policy as the core holds it", S, M::GET, "/staff/v1/core/policies/{number}", "A policy by its number with every cover layer, life, loading, exclusion and the billing position. Underwriting terms imply health, so this needs the insurance-sensitive class.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        // ---- The customer master's staff work.
        tool("find_customers", "Search the customer master", S, M::POST, "/staff/v1/customers/search", "Score every person against what you know (names, birth date, contact points), best first.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record.")
            .json(r#"{"family_name", "given_name"?, "date_of_birth"?: "YYYY-MM-DD", "email"?, "mobile_phone"?, "ird_number"?}"#),
        tool("match_customer", "Match a person", S, M::POST, "/staff/v1/customers/match", "The one record a source's person is, or that there is none.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record.")
            .json(r#"{"source": "where they came from, e.g. claims", "family_name", "given_name"?, "date_of_birth"?, "email"?, "mobile_phone"?, "create_if_none"?: false}"#),
        tool("create_customer", "Add a person", S, M::POST, "/staff/v1/customers", "Add a person to the customer master.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record.")
            .json(r#"{"kind": "person" | "organisation", "facts": [...], "allow_duplicate"?: false} as the customer master takes it"#),
        tool("customer", "A person's record", S, M::GET, "/staff/v1/customers/{party}", "A person's record as your role may read it, on a date and as known at a time.")
            .query("`as_of`: the date to read the record on (YYYY-MM-DD); `known_at`: what was known at a time (RFC 3339); `case`, `scope`: why, as for other reads."),
        tool("customer_history", "A record's history", S, M::GET, "/staff/v1/customers/{party}/history", "Every version of a person's facts: what was true when, and when it was learned.")
            .query("`as_of`: the date to read the record on (YYYY-MM-DD); `known_at`: what was known at a time (RFC 3339); `case`, `scope`: why, as for other reads."),
        tool("customer_relationships", "A person's relationships", S, M::GET, "/staff/v1/customers/{party}/relationships", "Who and what the person is related to: policies, schemes, attorneys, employers.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        tool("customer_due_diligence", "Due diligence", S, M::GET, "/staff/v1/customers/{party}/due-diligence", "What the anti-money laundering checks hold about the person.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        tool("change_customer", "Change a person's record", S, M::POST, "/staff/v1/customers/{party}/commands", "One of the customer master's record commands: record facts, decline a correction, add or end a relationship, place a spending hold.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record.")
            .json(r#"{"command": "<name>", "version"?: "<last seen>", ...}"#),
        tool("customer_export", "Everything held about a person", S, M::GET, "/staff/v1/customers/{party}/export", "For a Privacy Act request: every field and its history.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        tool("customer_audit", "Who read or changed a record", S, M::GET, "/staff/v1/customers/{party}/audit", "For the privacy officer: every read and change of the person's record.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        tool("actions_by", "What one person did", S, M::GET, "/staff/v1/actors/{actor}/actions", "For the privacy officer: what one member of staff or service did across records.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        tool("merge_customers", "Merge duplicates", S, M::POST, "/staff/v1/customers/merges", "Merge a duplicate record into the one that stays.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record.")
            .json(r#"{"survivor": "<party that stays>", "absorbed": "<duplicate>", "review"?: "<review id>"}"#),
        tool("unmerge_customers", "Undo a merge", S, M::POST, "/staff/v1/customers/unmerges", "Separate two records merged in error.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record.")
            .json(r#"{"survivor", "absorbed"}"#),
        tool("duplicate_reviews", "Likely duplicates", S, M::GET, "/staff/v1/customers/reviews", "The open reviews of likely duplicate records.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        tool("dismiss_review", "Not a duplicate", S, M::POST, "/staff/v1/customers/reviews/{review}/dismiss", "Close a duplicate review found not to be one.")
            .query("`case`: the case or task you work on; `scope`: comma-separated scope keys that reach the record."),
        // ---- Operations.
        tool("regions", "Regions and fail-over", O, M::GET, "/ops/v1/regions", "For operations: each region's nodes, which is serving, and the channel module's state."),
        tool("reconciliation", "Reconciliation", O, M::GET, "/ops/v1/reconciliation", "For operations: the book's reconciliation across every partition, the checks the close makes, and how many policies the run has still to visit."),
        tool("customer_index", "Customer search index", O, M::GET, "/ops/v1/customers/index", "For operations: the customer master's search index and how far behind it is."),
        tool("verify_customer", "Check a record's chain", O, M::POST, "/ops/v1/customers/{party}/verify", "For operations: check a person's record has an unbroken hash chain."),
        tool("rewrap_customers", "Re-encrypt records", O, M::POST, "/ops/v1/customers/rewrap", "For operations: re-encrypt records still under an old key, a batch at a time.")
            .json(r#"{"current_key_id", "limit"?: 1000}"#),
        tool("reshred_customers", "Erase again after a restore", O, M::POST, "/ops/v1/customers/reshred", "For operations: after a restore from backup, erase again what had been shredded since.")
            .json(r#"{"since": "YYYY-MM-DD, the day the restored backup was taken"}"#),
    ]
}

/// What the person may reach: the sign-ins their token holds.
async fn realms(s: &AppState, headers: &HeaderMap) -> Vec<Realm> {
    let mut held = vec![Realm::Public];
    let uri = Uri::from_static("/mcp");
    if s.session(&s.customers, headers, &Method::POST, &uri, b"").await.is_ok() {
        held.push(Realm::Customer);
    }
    if let Ok(caller) = s.workforce.authenticate(headers).await {
        if caller.has_role(OPERATIONS) {
            held.push(Realm::Operations);
        }
        if s.session(&s.workforce, headers, &Method::POST, &uri, b"")
            .await
            .is_ok_and(|(_, session)| session.staff)
        {
            held.push(Realm::Staff);
        }
    }
    held
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Where a client learns how to sign in.
fn metadata_url(s: &AppState) -> Option<String> {
    s.public_url.as_ref().map(|url| format!("{url}/.well-known/oauth-protected-resource/mcp"))
}

fn unauthorised(s: &AppState) -> Response {
    let challenge = match metadata_url(s) {
        Some(url) => format!("Bearer resource_metadata=\"{url}\""),
        None => "Bearer".to_string(),
    };
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorised", "detail": "sign in as a customer, adviser, employer or member of staff of this insurer" })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert("www-authenticate", value);
    }
    response
}

fn refused(status: StatusCode, detail: &str) -> Response {
    (
        status,
        Json(json!({ "error": status.canonical_reason().unwrap_or("refused"), "detail": detail })),
    )
        .into_response()
}

/// The protected resource metadata (RFC 9728): this endpoint, and the two
/// tenants that issue tokens for it.
pub async fn resource_metadata(State(s): State<Arc<AppState>>) -> Response {
    let (UiConfig::Entra { customers, workforce }, Some(url)) = (&s.ui, &s.public_url) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let issuer = |sign_in: &crate::ui::SignIn| format!("{}/{}/v2.0", sign_in.authority, sign_in.tenant);
    Json(json!({
        "resource": format!("{url}/mcp"),
        "resource_name": "Insurance core",
        "authorization_servers": [issuer(workforce), issuer(customers)],
        "scopes_supported": [workforce.scope, customers.scope],
        "bearer_methods_supported": ["header"],
    }))
    .into_response()
}

/// A browser may call only from this service's own origin; a client that is
/// not a browser sends none. This stops a page elsewhere driving the
/// endpoint through a browser that holds a token (DNS rebinding).
fn origin_allowed(s: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return true;
    };
    match &s.public_url {
        Some(url) => origin == url,
        None => {
            let host = origin.trim_start_matches("http://").trim_start_matches("https://");
            host.starts_with("localhost") || host.starts_with("127.0.0.1")
        }
    }
}

/// `GET /mcp`: no server-initiated stream is offered.
pub async fn no_stream() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [("allow", "POST")]).into_response()
}

/// `POST /mcp`: one JSON-RPC message.
pub async fn endpoint(State(s): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if !origin_allowed(&s, &headers) {
        return refused(StatusCode::FORBIDDEN, "this origin may not call the MCP endpoint");
    }
    if let Some(version) = headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok()) {
        if !VERSIONS.contains(&version) {
            return refused(StatusCode::BAD_REQUEST, "this MCP protocol version is not supported");
        }
    }
    let held = realms(&s, &headers).await;
    if held == [Realm::Public] {
        return unauthorised(&s);
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(m) => m,
        Err(_) => return Json(rpc_error(Value::Null, -32700, "the message is not JSON")).into_response(),
    };
    if message.is_array() {
        return Json(rpc_error(Value::Null, -32600, "send one message at a time")).into_response();
    }
    let Some(id) = message.get("id").cloned() else {
        // A notification or a response: nothing to answer.
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let reply = match message["method"].as_str() {
        Some("initialize") => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
            rpc_result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "insurance-core", "title": "Insurance core", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": "Every tool is one function of the insurance system, done as you: the same checks as the web pages apply to each call, and what you may not do is refused. Writes take an idempotency_key; send the same key to retry safely. Steps the customer must sign on their enrolled phone are refused here.",
                }),
            )
        }
        Some("ping") => rpc_result(id, json!({})),
        Some("tools/list") => {
            let tools: Vec<Value> = catalogue().iter().filter(|t| held.contains(&t.realm)).map(Tool::listed).collect();
            rpc_result(id, json!({ "tools": tools }))
        }
        Some("tools/call") => {
            let name = params["name"].as_str().unwrap_or_default();
            match catalogue().into_iter().find(|t| t.name == name) {
                None => rpc_error(id, -32602, &format!("there is no tool {name:?}")),
                Some(tool) if !held.contains(&tool.realm) => rpc_result(id, failed("this tool needs a sign-in you do not hold")),
                Some(tool) => {
                    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    match call(&s, &headers, &tool, &arguments).await {
                        Ok(result) => rpc_result(id, result),
                        Err(problem) => rpc_result(id, failed(&problem)),
                    }
                }
            }
        }
        Some(_) => rpc_error(id, -32601, "no such method"),
        None => rpc_error(id, -32600, "a request names its method"),
    };
    Json(reply).into_response()
}

/// A tool result that says what went wrong, for the model to read.
fn failed(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

/// A path or query value, percent-encoded so it cannot reach another route.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The request a tool call makes, from its arguments.
pub fn request_for(tool: &Tool, arguments: &Value) -> Result<(String, Vec<u8>), String> {
    let arguments = arguments.as_object().ok_or("the arguments are an object")?;
    let known: Vec<&str> = tool
        .params()
        .into_iter()
        .chain(["query", "body", "content_base64", "idempotency_key"])
        .collect();
    if let Some(stray) = arguments.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(format!("{stray} is not an argument of {}", tool.name));
    }
    let mut path = tool.path.to_string();
    for param in tool.params() {
        let value = arguments
            .get(param)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{param} is required"))?;
        if let Some((_, values)) = tool.choices.iter().find(|(name, _)| *name == param) {
            if !values.contains(&value) {
                return Err(format!("{param} is one of {}", values.join(", ")));
            }
        }
        path = path.replace(&format!("{{{param}}}"), &encode(value));
    }
    if let Some(query) = arguments.get("query").filter(|q| !q.is_null()) {
        if tool.query.is_empty() {
            return Err(format!("{} takes no query", tool.name));
        }
        let query = query.as_object().ok_or("query is an object of strings")?;
        let mut parts = Vec::new();
        for (key, value) in query {
            let value = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return Err(format!("query {key} is a string")),
            };
            parts.push(format!("{}={}", encode(key), encode(&value)));
        }
        if !parts.is_empty() {
            path = format!("{path}?{}", parts.join("&"));
        }
    }
    let body = match tool.body {
        Payload::None => Vec::new(),
        Payload::Json(_) => {
            let body = arguments.get("body").ok_or("body is required")?;
            serde_json::to_vec(body).map_err(|e| e.to_string())?
        }
        Payload::File(_) => {
            let text = arguments.get("content_base64").and_then(Value::as_str).ok_or("content_base64 is required")?;
            base64::engine::general_purpose::STANDARD
                .decode(text.trim())
                .map_err(|_| "content_base64 is not base64".to_string())?
        }
    };
    Ok((path, body))
}

/// The caller's own credentials, carried to the route unchanged.
const CARRIED: [&str; 5] = ["authorization", "x-dev-subject", "x-dev-roles", "x-dev-relationships", "x-dev-issued"];

async fn call(s: &Arc<AppState>, headers: &HeaderMap, tool: &Tool, arguments: &Value) -> Result<Value, String> {
    let (path, body) = request_for(tool, arguments)?;
    let mut request = Request::builder().method(tool.method.clone()).uri(&path);
    for name in CARRIED {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    if matches!(tool.body, Payload::Json(_)) {
        request = request.header("content-type", "application/json");
    }
    if tool.writes() {
        let key = match arguments.get("idempotency_key").and_then(Value::as_str) {
            Some(k) if !k.trim().is_empty() && k.len() <= 100 => format!("mcp:{k}"),
            Some(_) => return Err("idempotency_key is 1 to 100 characters".to_string()),
            None => format!("mcp:{}", fresh_key()?),
        };
        request = request.header("idempotency-key", key);
    }
    let request = request.body(Body::from(body)).map_err(|e| e.to_string())?;
    let response = router(s.clone()).oneshot(request).await.map_err(|e| e.to_string())?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), MAX_RESPONSE)
        .await
        .map_err(|_| "the answer was too large to hand back".to_string())?;
    Ok(result(tool, &path, status, &content_type, &bytes))
}

fn fresh_key() -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut buffer = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut buffer).map_err(|_| "no randomness".to_string())?;
    Ok(hex::encode(buffer))
}

/// What the route answered, as a tool result.
pub fn result(tool: &Tool, path: &str, status: StatusCode, content_type: &str, bytes: &[u8]) -> Value {
    let error = !status.is_success();
    if content_type.starts_with("application/json") || content_type.starts_with("application/problem+json") || bytes.is_empty() {
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(bytes).unwrap_or(Value::Null)
        };
        let text = if error {
            let detail = value["detail"].as_str().or(value["title"].as_str()).unwrap_or_default();
            let code = value["title"].as_str().or(value["code"].as_str()).unwrap_or_default();
            format!("Refused ({} {code}): {detail}", status.as_u16())
        } else {
            serde_json::to_string_pretty(&value).unwrap_or_default()
        };
        let mut out = json!({ "content": [{ "type": "text", "text": text }], "isError": error });
        if value.is_object() {
            out["structuredContent"] = value;
        } else if !value.is_null() {
            out["structuredContent"] = json!({ "result": value });
        }
        return out;
    }
    if error || content_type.starts_with("text/") {
        let text = String::from_utf8_lossy(bytes).to_string();
        return json!({ "content": [{ "type": "text", "text": if error { format!("Refused ({}): {text}", status.as_u16()) } else { text } }], "isError": error });
    }
    // A file: handed back whole, as an embedded resource.
    json!({
        "content": [{
            "type": "resource",
            "resource": {
                "uri": format!("insurance://{}{path}", tool.name),
                "mimeType": content_type,
                "blob": base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        }],
        "isError": false,
    })
}

/// How many tools each sign-in is offered, for the documentation.
pub fn counts() -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for tool in catalogue() {
        *out.entry(format!("{:?}", tool.realm)).or_insert(0) += 1;
    }
    out
}
