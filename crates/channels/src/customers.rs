//! Self-service against the customer master: a person reads their own
//! record, changes their contact details and consents, and asks for a copy
//! of everything held about them.
//!
//! The customer master (`crates/customers`, `docs/customers.md`) decides
//! what the portal may read and send, by its Cedar policy, for the app role
//! `portal` with `x-ic-party` naming the person signed in. The portal adds
//! only that the party is the one signed in, so nobody reaches another's
//! record through it, and that a change or an export comes from an enrolled
//! device that proved itself.

use std::future::Future;
use std::pin::Pin;

use ic_core::PartyId;
use serde_json::Value;

/// The commands a person sends about themselves. The master checks again
/// that `record` touches contact details only. A person may put a hold on
/// their own money going out, ask for it to end (naming a passed identity
/// check made since it was placed), and take that request back; the hold
/// then stands for its cooling-off period (`docs/customers.md`, "A
/// person's own spending hold").
pub const COMMANDS: [&str; 5] = ["record", "set_consent", "place_spending_hold", "request_hold_removal", "cancel_hold_removal"];

/// What the master answered: its HTTP status and body.
pub type Answered = (u16, Value);

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Answered, String>> + Send + 'a>>;

/// The customer master, as the portal calls it.
pub trait Master: Send + Sync {
    /// The person's own record, for `self_service`.
    fn record(&self, party: &PartyId) -> Asked<'_>;
    /// One of [`COMMANDS`], at the version the person last saw.
    fn command(&self, party: &PartyId, version: Option<String>, key: String, body: Value) -> Asked<'_>;
    /// Everything held about the person (a Privacy Act access request).
    fn export(&self, party: &PartyId) -> Asked<'_>;
    /// Whether the person's requests go to a person first, and the
    /// adjustments to make: never the marker's kind.
    fn routing(&self, party: &PartyId) -> Asked<'_>;
    /// Where the person's own hold on money going out stands:
    /// `{held, placed_at, removal}`.
    fn spending_hold(&self, party: &PartyId) -> Asked<'_>;
    /// A trained member of staff's work on vulnerability markers. The
    /// master is asked as `staff` with that person as the actor and the
    /// purpose `vulnerability_support`, and decides by its own policy.
    fn care(&self, staff: &PartyId, call: Care) -> Asked<'_>;
    /// Whether `caller` holds an authority to be told about `party`'s
    /// affairs (an attorney, a guardian, a written authority that covers
    /// `view`): the master's `allow`, `refuse` or `route`. Asked as `staff`
    /// for the member of staff who took the call, and logged there.
    fn speaks_for(&self, staff: &PartyId, party: &PartyId, caller: &PartyId) -> Asked<'_>;
    /// Whether the person signed in holds an authority in force to view
    /// `party`'s affairs. Asked as the portal for that person, and logged
    /// by the master against them.
    fn represents(&self, representative: &PartyId, party: &PartyId) -> Asked<'_>;
    /// Staff or operations work in the master, for `actor` ([`Desk`]).
    fn desk(&self, actor: &PartyId, desk: Desk) -> Asked<'_>;
}

/// What a member of staff may do with someone who has called about a
/// policy, from who owns it and the master's answers about the caller for
/// each owner. An owner is spoken to. Someone else is spoken to only with
/// an authority from every owner, since what is said about a joint policy
/// is said about each of them. Where the master says a person must decide
/// (capacity in question), the call is referred. Anything else, a caller
/// nobody identified, and a master that could not answer are all refused
/// alike, so that the refusal itself tells the caller nothing.
pub fn caller_outcome(owner: bool, answers: &[Option<Result<Answered, String>>]) -> &'static str {
    if owner {
        return "discuss";
    }
    let decision = |a: &Option<Result<Answered, String>>| match a {
        Some(Ok((200, body))) => body["decision"].as_str().unwrap_or("refuse").to_string(),
        _ => "refuse".to_string(),
    };
    let decisions: Vec<String> = answers.iter().map(decision).collect();
    if decisions.is_empty() || decisions.iter().any(|d| d != "allow" && d != "route") {
        "refuse"
    } else if decisions.iter().any(|d| d == "route") {
        "refer"
    } else {
        "discuss"
    }
}

/// What a trained member of staff asks of the master about markers.
#[derive(Debug, Clone, PartialEq)]
pub enum Care {
    /// The open prompts to consider or review a marker.
    Reviews,
    /// How to serve one person, with the marker's kind.
    Routing(PartyId),
    /// Set, change or remove a person's marker.
    Marker { party: PartyId, key: String, body: Value },
    /// Close a prompt once it is decided.
    Dismiss(String),
}

/// Staff and operations work done in the customer master itself, through
/// the portal: the portal decides by the four questions whether the person
/// may do it, then asks the master in the role the work needs, naming the
/// person as the actor, and the master decides again by its own policy and
/// logs it against them.
#[derive(Debug, Clone, PartialEq)]
pub enum Desk {
    /// Score every record against a probe: names, birth date, contact points.
    Search(Value),
    /// Find the one record a source's person is, or say there is none.
    Match(Value),
    /// Add a person.
    Create {
        key: String,
        body: Value,
    },
    /// A record as the master holds it on a date and as known at a time.
    Read {
        party: PartyId,
        query: String,
    },
    /// Every version of a record's facts.
    History {
        party: PartyId,
        query: String,
    },
    Relationships(PartyId),
    /// What the anti-money laundering checks hold about a person.
    DueDiligence(PartyId),
    /// One of the master's record commands, at the version last seen.
    Change {
        party: PartyId,
        key: String,
        body: Value,
    },
    Merge {
        key: String,
        body: Value,
    },
    Unmerge {
        key: String,
        body: Value,
    },
    /// The open reviews: likely duplicates and markers to consider.
    Reviews,
    /// Close a duplicate review found not to be one.
    Dismiss(String),
    /// Everything held about a person, for a Privacy Act request.
    Export(PartyId),
    /// Who read or changed a record.
    Audit(PartyId),
    /// What one person (staff or service) did across records.
    ActionsBy(PartyId),
    /// Operations: the search index's state.
    Index,
    /// Operations: check a record's hash chain is whole.
    Verify(PartyId),
    /// Operations: re-encrypt records under the current key.
    Rewrap(Value),
    /// Operations: erase again what was shredded, after a restore.
    Reshred(Value),
}

impl Desk {
    /// The role the master is asked in, and the purpose declared to it.
    pub fn role(&self) -> (&'static str, &'static str) {
        match self {
            Desk::Merge { .. } | Desk::Unmerge { .. } | Desk::Reviews | Desk::Dismiss(_) => ("staff", "duplicates"),
            Desk::Export(_) => ("staff", "subject_access"),
            Desk::Audit(_) | Desk::ActionsBy(_) => ("privacy", "audit"),
            Desk::Index | Desk::Verify(_) | Desk::Rewrap(_) | Desk::Reshred(_) => ("operations", "operations"),
            _ => ("staff", "servicing"),
        }
    }

    /// The staff action the four questions are asked about. Operations work
    /// is decided by the operations role instead, so has none.
    pub fn action(&self) -> Option<&'static str> {
        use crate::staff::actions::*;
        Some(match self {
            Desk::Search(_) | Desk::Match(_) => FIND_CUSTOMER,
            Desk::Create { .. } => CREATE_CUSTOMER,
            Desk::Read { .. } | Desk::History { .. } | Desk::Relationships(_) | Desk::DueDiligence(_) => VIEW_CUSTOMER,
            Desk::Change { .. } => CHANGE_CUSTOMER,
            Desk::Merge { .. } | Desk::Unmerge { .. } | Desk::Reviews | Desk::Dismiss(_) => MERGE_CUSTOMERS,
            Desk::Export(_) => EXPORT_CUSTOMER,
            Desk::Audit(_) | Desk::ActionsBy(_) => AUDIT_CUSTOMERS,
            Desk::Index | Desk::Verify(_) | Desk::Rewrap(_) | Desk::Reshred(_) => return None,
        })
    }

    /// The person the record is about, where there is one.
    pub fn about(&self) -> Option<&PartyId> {
        match self {
            Desk::Read { party, .. } | Desk::History { party, .. } | Desk::Change { party, .. } => Some(party),
            Desk::Relationships(p) | Desk::DueDiligence(p) | Desk::Export(p) | Desk::Audit(p) | Desk::Verify(p) => Some(p),
            _ => None,
        }
    }

    /// The request to the master: method, path, idempotency key and body.
    pub fn request(&self) -> (reqwest::Method, String, Option<String>, Option<Value>) {
        use reqwest::Method;
        let at = |party: &PartyId, rest: &str| format!("/v1/parties/{}{rest}", encode(party));
        match self {
            Desk::Search(probe) => (Method::POST, "/v1/parties/search".into(), None, Some(probe.clone())),
            Desk::Match(probe) => (Method::POST, "/v1/parties/match".into(), None, Some(probe.clone())),
            Desk::Create { key, body } => (Method::POST, "/v1/parties".into(), Some(key.clone()), Some(body.clone())),
            Desk::Read { party, query } => (Method::GET, at(party, &format!("?purpose=servicing{query}")), None, None),
            Desk::History { party, query } => (Method::GET, at(party, &format!("/history?purpose=servicing{query}")), None, None),
            Desk::Relationships(party) => (Method::GET, at(party, "/relationships"), None, None),
            Desk::DueDiligence(party) => (Method::GET, at(party, "/due-diligence"), None, None),
            Desk::Change { party, key, body } => (Method::POST, at(party, "/commands"), Some(key.clone()), Some(body.clone())),
            Desk::Merge { key, body } => (Method::POST, "/v1/merges".into(), Some(key.clone()), Some(body.clone())),
            Desk::Unmerge { key, body } => (Method::POST, "/v1/unmerges".into(), Some(key.clone()), Some(body.clone())),
            Desk::Reviews => (Method::GET, "/v1/reviews".into(), None, None),
            Desk::Dismiss(review) => (
                Method::POST,
                format!("/v1/reviews/{}/dismiss", encode(&PartyId::from(review.as_str()))),
                None,
                None,
            ),
            Desk::Export(party) => (Method::GET, at(party, "/export?purpose=subject_access"), None, None),
            Desk::Audit(party) => (Method::GET, at(party, "/audit"), None, None),
            Desk::ActionsBy(actor) => (Method::GET, format!("/v1/actors/{}/actions", encode(actor)), None, None),
            Desk::Index => (Method::GET, "/v1/admin/index".into(), None, None),
            Desk::Verify(party) => (Method::POST, format!("/v1/admin/verify/{}", encode(party)), None, None),
            Desk::Rewrap(body) => (Method::POST, "/v1/admin/rewrap".into(), None, Some(body.clone())),
            Desk::Reshred(body) => (Method::POST, "/v1/admin/reshred".into(), None, Some(body.clone())),
        }
    }
}

/// The dates a record or its history is read as at, as the master takes
/// them (`as_of` a date, `known_at` a time), checked so nothing else rides
/// along in the query.
pub fn as_at(as_of: Option<&str>, known_at: Option<&str>) -> Result<String, String> {
    let mut query = String::new();
    if let Some(d) = as_of {
        d.parse::<chrono::NaiveDate>()
            .map_err(|_| format!("as_of must be a date such as 2027-11-15, not {d:?}"))?;
        query.push_str(&format!("&as_of={d}"));
    }
    if let Some(t) = known_at {
        let parsed = chrono::DateTime::parse_from_rfc3339(t).map_err(|_| format!("known_at must be a time such as 2027-11-15T09:00:00Z, not {t:?}"))?;
        query.push_str(&format!("&known_at={}", parsed.to_utc().format("%Y-%m-%dT%H:%M:%SZ")));
    }
    Ok(query)
}

/// The kinds a marker may name and what may be done differently, as the
/// master lists them. They are offered on the review screen; the master
/// checks what is sent, so a list that has fallen behind refuses nothing
/// the master would take and lets nothing through that it would not.
pub const MARKER_KINDS: [&str; 7] = [
    "health",
    "life_event",
    "bereavement",
    "resilience",
    "capability",
    "financial_abuse_risk",
    "other",
];
/// See [`MARKER_KINDS`].
pub const ADJUSTMENTS: [&str; 9] = [
    "more_time",
    "plain_language",
    "large_print",
    "speak_to_trained_person",
    "pause_before_large_withdrawal",
    "no_phone_calls",
    "support_person_present",
    "written_follow_up",
    "check_understanding",
];

/// The outcomes of reviewing a marker.
pub const OUTCOMES: [&str; 4] = ["renew", "change", "remove", "no_marker"];

/// The master's command for a review's outcome, or `None` where the
/// outcome changes nothing on the person's record (`no_marker`: a prompt
/// looked at, with no marker needed). A marker is a kind, a review date
/// and adjustments from the master's fixed lists; it has no free text, and
/// the master checks the lists. The reason recorded is the outcome and the
/// prompt, never anything about the person.
pub fn marker_command(review: &str, decision: &Value) -> Result<Option<Value>, String> {
    let outcome = decision["outcome"]
        .as_str()
        .filter(|o| OUTCOMES.contains(o))
        .ok_or("the outcome is renew, change, remove or no_marker")?;
    let reason = format!("{outcome} at review {review}");
    let value = match outcome {
        "no_marker" => return Ok(None),
        "remove" => Value::Bool(false),
        _ => {
            let kind = decision["kind"].as_str().filter(|k| !k.is_empty()).ok_or("a marker has a kind")?;
            let review_on = decision["review_on"]
                .as_str()
                .filter(|d| d.parse::<chrono::NaiveDate>().is_ok())
                .ok_or("a marker has the date it is next reviewed")?;
            let adjustments = decision["adjustments"]
                .as_array()
                .filter(|a| a.iter().all(Value::is_string))
                .ok_or("a marker lists its adjustments")?;
            serde_json::json!({ "kind": kind, "review_on": review_on, "adjustments": adjustments })
        }
    };
    Ok(Some(
        serde_json::json!({ "command": "record", "reason": reason, "facts": [{ "field": "vulnerability", "value": value }] }),
    ))
}

/// Whether a request goes to a person first, from the master's routing
/// answer. A master that cannot answer sends it to a person: a request
/// looked at by someone who did not need to is better than one from a
/// vulnerable customer handled without care. With no master configured
/// there is nothing to go on.
pub fn route_to_person(answer: Option<Result<Answered, String>>) -> bool {
    match answer {
        None => false,
        Some(Ok((200, body))) => body["route_to_person"] != false,
        Some(_) => true,
    }
}

/// Check a person's command before it leaves the portal.
pub fn check(body: &Value) -> Result<(), String> {
    match body["command"].as_str() {
        Some(name) if COMMANDS.contains(&name) => Ok(()),
        Some(name) => Err(format!("{name} is not something you change yourself")),
        None => Err("a command names itself in `command`".to_string()),
    }
}

/// The customer master service, reached with the portal's own Entra token.
pub struct Hosted {
    pub base: String,
    /// The master's application ID URI scope, such as `api://ic-customers/.default`.
    pub scope: String,
    pub credential: ic_azure::credential::AzureCredential,
    pub http: reqwest::Client,
}

impl Hosted {
    async fn call(
        &self,
        method: reqwest::Method,
        path: String,
        party: &PartyId,
        purpose: &str,
        extra: Vec<(&'static str, String)>,
        body: Option<Value>,
    ) -> Result<Answered, String> {
        self.call_as("portal", Some(party), party, method, path, purpose, extra, body).await
    }

    /// One call to the master in the role `caller`, for `actor`, about
    /// themselves (`acting`) or as staff.
    #[allow(clippy::too_many_arguments)]
    async fn call_as(
        &self,
        caller: &str,
        acting: Option<&PartyId>,
        actor: &PartyId,
        method: reqwest::Method,
        path: String,
        purpose: &str,
        extra: Vec<(&'static str, String)>,
        body: Option<Value>,
    ) -> Result<Answered, String> {
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base.trim_end_matches('/')))
            .bearer_auth(token)
            .header("x-ic-caller", caller)
            .header("x-ic-purpose", purpose)
            .header("x-ic-actor", &actor.0);
        if let Some(party) = acting {
            request = request.header("x-ic-party", &party.0);
        }
        for (name, value) in extra {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        // 503 from the master always means try again.
        if status == 503 {
            return Err("the customer master asked us to try again".to_string());
        }
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }
}

fn encode(party: &PartyId) -> String {
    party.0.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

impl Master for Hosted {
    fn record(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.call(
                reqwest::Method::GET,
                format!("/v1/parties/{}?purpose=self_service", encode(&party)),
                &party,
                "self_service",
                vec![],
                None,
            )
            .await
        })
    }

    fn command(&self, party: &PartyId, version: Option<String>, key: String, body: Value) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            let mut extra = vec![("idempotency-key", key)];
            if let Some(v) = version {
                extra.push(("if-match", v));
            }
            self.call(
                reqwest::Method::POST,
                format!("/v1/parties/{}/commands", encode(&party)),
                &party,
                "self_service",
                extra,
                Some(body),
            )
            .await
        })
    }

    fn export(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.call(
                reqwest::Method::GET,
                format!("/v1/parties/{}/export?purpose=subject_access", encode(&party)),
                &party,
                "subject_access",
                vec![],
                None,
            )
            .await
        })
    }

    fn routing(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.call(
                reqwest::Method::GET,
                format!("/v1/parties/{}/routing", encode(&party)),
                &party,
                "self_service",
                vec![],
                None,
            )
            .await
        })
    }

    fn spending_hold(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.call(
                reqwest::Method::GET,
                format!("/v1/parties/{}/spending-hold", encode(&party)),
                &party,
                "self_service",
                vec![],
                None,
            )
            .await
        })
    }

    fn care(&self, staff: &PartyId, call: Care) -> Asked<'_> {
        let staff = staff.clone();
        Box::pin(async move {
            let (method, path, extra, body) = match call {
                Care::Reviews => (reqwest::Method::GET, "/v1/reviews".to_string(), vec![], None),
                Care::Routing(party) => (reqwest::Method::GET, format!("/v1/parties/{}/routing", encode(&party)), vec![], None),
                Care::Marker { party, key, body } => (
                    reqwest::Method::POST,
                    format!("/v1/parties/{}/commands", encode(&party)),
                    vec![("idempotency-key", key)],
                    Some(body),
                ),
                Care::Dismiss(review) => (
                    reqwest::Method::POST,
                    format!("/v1/reviews/{}/dismiss", encode(&PartyId::from(review.as_str()))),
                    vec![],
                    None,
                ),
            };
            self.call_as("staff", None, &staff, method, path, "vulnerability_support", extra, body).await
        })
    }

    fn represents(&self, representative: &PartyId, party: &PartyId) -> Asked<'_> {
        let (who, path) = (
            representative.clone(),
            format!("/v1/parties/{}/authority?actor={}&act=view", encode(party), encode(representative)),
        );
        Box::pin(async move {
            self.call_as("portal", Some(&who), &who, reqwest::Method::GET, path, "representative", vec![], None)
                .await
        })
    }

    fn desk(&self, actor: &PartyId, desk: Desk) -> Asked<'_> {
        let actor = actor.clone();
        Box::pin(async move {
            let (role, purpose) = desk.role();
            let (method, path, key, body) = desk.request();
            let extra = key.map(|k| vec![("idempotency-key", k)]).unwrap_or_default();
            self.call_as(role, None, &actor, method, path, purpose, extra, body).await
        })
    }

    fn speaks_for(&self, staff: &PartyId, party: &PartyId, caller: &PartyId) -> Asked<'_> {
        let (staff, path) = (
            staff.clone(),
            format!("/v1/parties/{}/authority?actor={}&act=view", encode(party), encode(caller)),
        );
        Box::pin(async move {
            self.call_as("staff", None, &staff, reqwest::Method::GET, path, "caller_check", vec![], None)
                .await
        })
    }
}
