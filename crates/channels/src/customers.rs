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
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base.trim_end_matches('/')))
            .bearer_auth(token)
            .header("x-ic-caller", "portal")
            .header("x-ic-purpose", purpose)
            .header("x-ic-party", &party.0)
            .header("x-ic-actor", &party.0);
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
}
