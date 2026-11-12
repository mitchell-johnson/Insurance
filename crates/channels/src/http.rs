//! The portal service: one HTTP surface for the customer app, the adviser
//! portal, the employer portal, the staff console and the operations
//! console.
//!
//! The service is stateless. It authenticates the caller with Microsoft
//! Entra (External ID for customers, advisers and employers; Entra ID for
//! the workforce and for other modules), resolves the sign-in to a party
//! through the channel module, and then asks the module a question or
//! proposes a command to it. Every decision about who may see or do what is
//! made in the module's replicated state, not here.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use base64::Engine;
use ic_api::auth::{Auth, AuthError, Caller};
use ic_core::{PartyId, PolicyId, Reject};
use ic_log::StateMachine;
use ic_raft::ClusterClient;
use ring::rand::SecureRandom;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::directory::{Actor, DelegateAccess, DirectoryCommand};
use crate::identity::{action_hash, Enrolment, IdentityCommand, MAX_CHALLENGE_SECS};
use crate::ops::{RegionSource, StatusPage};
use crate::query::{self, DeviceProof, Question, MODULE};
use crate::staff::{AccessRequest, At, Decision, Purpose, PurposeKind, StaffCommand};
use crate::state::{ChannelState, Command, Envelope, Output, GATEWAY};
use crate::ui::UiConfig;
use crate::workbench::WorkbenchCommand;

/// The workforce role that sees the operations console.
pub const OPERATIONS: &str = "operations";

/// The module could not be reached. The caller may retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable(pub String);

type Reply<'a, T> = Pin<Box<dyn Future<Output = Result<T, Unavailable>> + Send + 'a>>;

/// The channel module, wherever it runs.
pub trait Channel: Send + Sync + 'static {
    fn propose(&self, envelope: Envelope) -> Reply<'_, Output>;
    fn ask(&self, question: Question) -> Reply<'_, Value>;
}

/// The module in this process: for tests and a single-process box.
pub struct Local {
    state: Mutex<(ChannelState, u64)>,
}

impl Local {
    pub fn new(state: ChannelState) -> Self {
        Self { state: Mutex::new((state, 0)) }
    }

    /// Run `f` against the state. A poisoned lock is taken as it is: the
    /// state machine validates before it writes.
    pub fn with<T>(&self, f: impl FnOnce(&mut ChannelState) -> T) -> T {
        let mut guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard.0)
    }
}

impl Channel for Local {
    fn propose(&self, envelope: Envelope) -> Reply<'_, Output> {
        let mut guard = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.1 += 1;
        let index = guard.1;
        let output = guard.0.apply(index, envelope);
        Box::pin(async move { Ok(output) })
    }

    fn ask(&self, question: Question) -> Reply<'_, Value> {
        let answer = self.with(|state| query::ask(state, question));
        Box::pin(async move { Ok(answer) })
    }
}

/// The module on the ledger nodes, through its group's leader.
pub struct Hosted {
    pub client: ClusterClient,
    /// How long a request keeps trying the nodes before it answers 503.
    pub patience: Duration,
}

impl Channel for Hosted {
    fn propose(&self, envelope: Envelope) -> Reply<'_, Output> {
        Box::pin(async move {
            let command = serde_json::to_value(&envelope).map_err(|e| Unavailable(e.to_string()))?;
            let reply = self
                .client
                .propose_module_until(MODULE, &command, self.patience)
                .await
                .map_err(|e| Unavailable(e.to_string()))?;
            let output = reply.result.map_err(Unavailable)?;
            serde_json::from_value(output).map_err(|e| Unavailable(format!("the module's reply was not understood: {e}")))
        })
    }

    fn ask(&self, question: Question) -> Reply<'_, Value> {
        Box::pin(async move {
            let question = serde_json::to_value(&question).map_err(|e| Unavailable(e.to_string()))?;
            let deadline = tokio::time::Instant::now() + self.patience;
            loop {
                match self.client.query_module(MODULE, &question).await {
                    Ok(answer) => return answer.map_err(Unavailable),
                    Err(e) if tokio::time::Instant::now() >= deadline => return Err(Unavailable(e.to_string())),
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
    }
}

pub struct AppState {
    pub channel: Arc<dyn Channel>,
    /// Customers, advisers and employer administrators: Entra External ID.
    pub customers: Auth,
    /// Staff and other modules: Entra ID.
    pub workforce: Auth,
    /// Seconds since the epoch. The only clock the service reads.
    pub clock: Arc<dyn Fn() -> At + Send + Sync>,
    /// Where the operations console and status page learn the regions' health.
    pub regions: Arc<dyn RegionSource>,
    /// How the web pages sign people in.
    pub ui: UiConfig,
}

pub type Shared = Arc<AppState>;

/// Errors as RFC 9457 problem details.
#[derive(Debug)]
pub struct Problem {
    status: StatusCode,
    code: String,
    detail: String,
}

impl Problem {
    fn new(status: StatusCode, code: &str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code: code.to_string(),
            detail: detail.into(),
        }
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such resource")
    }

    fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", detail)
    }

    fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", detail)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let body =
            json!({ "type": format!("urn:insurance-core:problem:{}", self.code), "title": self.code, "status": self.status.as_u16(), "detail": self.detail });
        let mut response = (self.status, [("content-type", "application/problem+json")], body.to_string()).into_response();
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            response.headers_mut().insert("retry-after", HeaderValue::from_static("1"));
        }
        response
    }
}

impl From<AuthError> for Problem {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Keys(detail) => Problem::new(StatusCode::SERVICE_UNAVAILABLE, "identity_unavailable", detail),
            other => Problem::new(StatusCode::UNAUTHORIZED, "unauthenticated", other.to_string()),
        }
    }
}

impl From<Unavailable> for Problem {
    fn from(e: Unavailable) -> Self {
        Problem::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", e.0)
    }
}

impl From<Reject> for Problem {
    fn from(r: Reject) -> Self {
        let status = match r.code.as_str() {
            "forbidden" | "not_authorised" | "adviser_read_only" => StatusCode::FORBIDDEN,
            "not_found" | "unknown_party" | "unknown_policy" => StatusCode::NOT_FOUND,
            "duplicate" | "already_resolved" | "not_awaiting" | "already_approved" => StatusCode::CONFLICT,
            "step_up_required" => StatusCode::UNAUTHORIZED,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        Problem::new(status, &r.code, r.message)
    }
}

type Answer = Result<Response, Problem>;

/// Who is calling, as the module knows them.
#[derive(Debug, Clone)]
pub struct Session {
    pub party: PartyId,
    /// The caller proved an enrolled device's key on this request.
    pub device: Option<String>,
    pub staff: bool,
    pub at: At,
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

/// When the caller's token was issued. The token has already been verified;
/// this reads its `iat`. A token that does not say is treated as the oldest
/// possible, so any revocation refuses it.
fn issued(headers: &HeaderMap, now: At) -> At {
    if let Some(dev) = header(headers, "x-dev-issued") {
        return dev.parse().unwrap_or(0);
    }
    let Some(token) = header(headers, "authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string)) else {
        // Development identity carries no token: the session starts now.
        return now;
    };
    token
        .split('.')
        .nth(1)
        .and_then(|payload| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|claims| claims.get("iat").and_then(Value::as_i64))
        .unwrap_or(0)
}

impl AppState {
    fn now(&self) -> At {
        (self.clock)()
    }

    async fn ask(&self, question: Question) -> Result<Value, Problem> {
        Ok(self.channel.ask(question).await?)
    }

    /// Propose a command and return its effects, or the module's refusal.
    async fn propose(&self, key: String, actor: Actor, command: Command) -> Result<Value, Problem> {
        let envelope = Envelope {
            key,
            at: self.now(),
            actor,
            command,
        };
        let output = self.channel.propose(envelope).await?;
        let effects = output.result?;
        Ok(json!({ "accepted": true, "duplicate": output.duplicate, "effects": effects }))
    }

    /// Resolve the caller to a party and check the session still stands.
    async fn session(&self, auth: &Auth, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Caller, Session), Problem> {
        let caller = auth.authenticate(headers).await?;
        let at = self.now();
        let device = match (header(headers, "x-device-id"), header(headers, "x-device-proof")) {
            (Some(device), Some(proof)) => Some(DeviceProof {
                device,
                proof,
                method: method.as_str().to_string(),
                path: uri.path().to_string(),
                body_sha256: hex::encode(Sha256::digest(body)),
            }),
            _ => None,
        };
        let answer = self
            .ask(Question::Session {
                subject: caller.subject.clone(),
                issued: issued(headers, at),
                at,
                device,
            })
            .await?;
        if answer["known"] != true {
            return Err(Problem::forbidden("this sign-in is not registered with the insurer"));
        }
        if answer["device_revoked"] == true {
            // Said before anything else, and whatever the token: this is the app's signal to wipe its local data.
            return Err(Problem::new(
                StatusCode::UNAUTHORIZED,
                "device_revoked",
                "this device is no longer enrolled: remove everything it holds",
            ));
        }
        if answer["valid"] != true {
            return Err(Problem::new(
                StatusCode::UNAUTHORIZED,
                "session_revoked",
                "this session was closed: sign in again",
            ));
        }
        let party = PartyId::from(answer["party"].as_str().unwrap_or_default());
        let session = Session {
            party,
            device: answer["device"].as_str().map(str::to_string),
            staff: answer["staff"] == true,
            at,
        };
        Ok((caller, session))
    }

    async fn customer(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<Session, Problem> {
        Ok(self.session(&self.customers, headers, method, uri, body).await?.1)
    }

    /// A member of staff. The four questions are asked per action in the module.
    async fn staff(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<Session, Problem> {
        let (_, session) = self.session(&self.workforce, headers, method, uri, body).await?;
        if !session.staff {
            return Err(Problem::forbidden("you hold no staff role on this platform"));
        }
        Ok(session)
    }
}

/// The caller's idempotency key, scoped to the caller so one party cannot
/// collide with another's.
fn key(headers: &HeaderMap, who: &str) -> Result<String, Problem> {
    header(headers, "idempotency-key")
        .filter(|k| !k.trim().is_empty() && k.len() <= 128)
        .map(|k| format!("{who}:{k}"))
        .ok_or_else(|| Problem::bad_request("a write carries an Idempotency-Key header of at most 128 characters"))
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Problem> {
    serde_json::from_slice(body).map_err(|e| Problem::bad_request(format!("the request body was not understood: {e}")))
}

fn random_hex(bytes: usize) -> Result<String, Problem> {
    let mut buffer = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut buffer)
        .map_err(|_| Problem::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "no randomness available"))?;
    Ok(hex::encode(buffer))
}

fn ok(value: Value) -> Answer {
    Ok(Json(value).into_response())
}

fn accepted(value: Value) -> Answer {
    Ok((StatusCode::ACCEPTED, Json(value)).into_response())
}

// --- the customer app, shared by advisers and employer administrators ---

async fn me(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let policies = s
        .ask(Question::Policies {
            party: session.party.clone(),
            at: session.at,
        })
        .await?;
    ok(json!({ "party": session.party, "device": session.device, "staff": session.staff, "policies": policies }))
}

#[derive(Deserialize)]
struct Have {
    have: Option<u64>,
}

/// Device sync. Offline data goes only to an enrolled device that proved its key.
async fn sync(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(q): Query<Have>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    if session.device.is_none() {
        return Err(Problem::new(
            StatusCode::UNAUTHORIZED,
            "device_required",
            "sync needs proof of an enrolled device's key",
        ));
    }
    ok(s.ask(Question::Sync {
        party: session.party,
        have: q.have,
    })
    .await?)
}

async fn policy(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let view = s
        .ask(Question::Policy {
            viewer: session.party,
            policy: PolicyId::from(policy.as_str()),
            at: session.at,
        })
        .await?;
    if view.is_null() {
        return Err(Problem::not_found());
    }
    ok(view)
}

async fn requests(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    ok(s.ask(Question::Requests { party: session.party }).await?)
}

#[derive(Deserialize)]
struct Signature {
    challenge: String,
    signature: String,
}

/// Redeem a challenge the caller's device signed for exactly `action`.
async fn redeem(s: &AppState, headers: &HeaderMap, session: &Session, action: &Value, signed: Option<Signature>) -> Result<bool, Problem> {
    let Some(signed) = signed else { return Ok(false) };
    s.propose(
        format!("{}:redeem", key(headers, &session.party.0)?),
        Actor::Party(session.party.clone()),
        Command::Identity(IdentityCommand::RedeemChallenge {
            id: signed.challenge,
            action_hash: action_hash(action),
            signature: signed.signature,
        }),
    )
    .await
    .map_err(|p| Problem::new(StatusCode::UNAUTHORIZED, "step_up_failed", p.detail))?;
    Ok(true)
}

#[derive(Deserialize)]
struct NewRequest {
    id: String,
    policy: Option<PolicyId>,
    kind: String,
    module: String,
    #[serde(default)]
    payload: Value,
    signed: Option<Signature>,
}

/// What a device signs for a request: its identity and exactly what it asks.
pub fn request_action(id: &str, policy: Option<&PolicyId>, kind: &str, payload: &Value) -> Value {
    json!({ "action": "submit_request", "id": id, "policy": policy, "kind": kind, "payload": payload })
}

/// What a device signs to approve a request someone else prepared.
pub fn approval_action(id: &str) -> Value {
    json!({ "action": "approve_request", "id": id })
}

async fn submit_request(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let new: NewRequest = parse(&body)?;
    let action = request_action(&new.id, new.policy.as_ref(), &new.kind, &new.payload);
    let signed = redeem(&s, &headers, &session, &action, new.signed).await?;
    let result = s
        .propose(
            key(&headers, &session.party.0)?,
            Actor::Party(session.party),
            Command::SubmitRequest {
                id: new.id,
                policy: new.policy,
                kind: new.kind,
                module: new.module,
                payload: new.payload,
                signed,
            },
        )
        .await?;
    accepted(result)
}

#[derive(Deserialize, Default)]
struct Approval {
    signed: Option<Signature>,
}

async fn approve_request(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(id): Path<String>, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let approval: Approval = if body.is_empty() { Approval::default() } else { parse(&body)? };
    let signed = redeem(&s, &headers, &session, &approval_action(&id), approval.signed).await?;
    accepted(
        s.propose(
            key(&headers, &session.party.0)?,
            Actor::Party(session.party),
            Command::ApproveRequest { id, signed },
        )
        .await?,
    )
}

async fn cancel_request(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(id): Path<String>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    ok(
        s.propose(key(&headers, &session.party.0)?, Actor::Party(session.party), Command::CancelRequest { id })
            .await?,
    )
}

#[derive(Deserialize)]
struct NewChallenge {
    /// The exact action to be signed, as [`request_action`] or [`approval_action`] builds it.
    action: Value,
}

/// Ask for a challenge. Only an enrolled device that proved its key gets one.
async fn challenge(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let Some(device) = session.device.clone() else {
        return Err(Problem::new(
            StatusCode::UNAUTHORIZED,
            "device_required",
            "signing needs proof of an enrolled device's key",
        ));
    };
    let new: NewChallenge = parse(&body)?;
    let (id, nonce, hash) = (random_hex(16)?, random_hex(16)?, action_hash(&new.action));
    s.propose(
        key(&headers, &session.party.0)?,
        Actor::Party(session.party),
        Command::Identity(IdentityCommand::IssueChallenge {
            id: id.clone(),
            device,
            action_hash: hash.clone(),
            nonce: nonce.clone(),
            ttl_secs: MAX_CHALLENGE_SECS,
        }),
    )
    .await?;
    // The device signs `id\nnonce\naction_hash`.
    ok(json!({ "challenge": id, "nonce": nonce, "action_hash": hash, "expires": session.at + MAX_CHALLENGE_SECS }))
}

async fn devices(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    ok(s.ask(Question::Devices { party: session.party }).await?)
}

#[derive(Deserialize)]
struct NewDevice {
    device: String,
    public_key: String,
    #[serde(default)]
    hardware_backed: bool,
    /// The enrolled device approving this one, and its signature over `enrol\n{device}\n{public_key}`.
    approver: String,
    signature: String,
}

async fn enrol_device(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let new: NewDevice = parse(&body)?;
    let command = IdentityCommand::EnrolDevice {
        device: new.device,
        party: session.party.clone(),
        public_key: new.public_key,
        hardware_backed: new.hardware_backed,
        enrolment: Enrolment::ExistingDevice {
            device: new.approver,
            signature: new.signature,
        },
    };
    ok(
        s.propose(key(&headers, &session.party.0)?, Actor::Party(session.party), Command::Identity(command))
            .await?,
    )
}

async fn revoke_device(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(device): Path<String>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let command = Command::Identity(IdentityCommand::RevokeDevice { device });
    ok(s.propose(key(&headers, &session.party.0)?, Actor::Party(session.party), command).await?)
}

async fn directory(s: &AppState, headers: &HeaderMap, session: Session, command: DirectoryCommand) -> Answer {
    ok(
        s.propose(key(headers, &session.party.0)?, Actor::Party(session.party), Command::Directory(command))
            .await?,
    )
}

#[derive(Deserialize)]
struct Appoint {
    adviser: PartyId,
    engagement: String,
}

async fn appoint_adviser(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let a: Appoint = parse(&body)?;
    let command = DirectoryCommand::GrantAdviserAuthority {
        policy: PolicyId::from(policy.as_str()),
        adviser: a.adviser,
        engagement: a.engagement,
    };
    directory(&s, &headers, session, command).await
}

async fn remove_adviser(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path((policy, adviser)): Path<(String, String)>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let command = DirectoryCommand::WithdrawAdviserAuthority {
        policy: PolicyId::from(policy.as_str()),
        adviser: PartyId::from(adviser.as_str()),
    };
    directory(&s, &headers, session, command).await
}

#[derive(Deserialize)]
struct Delegate {
    access: Option<DelegateAccess>,
}

/// Let someone look at the policy, change what they may see, or stop it (`access: null`).
async fn delegate(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path((policy, party)): Path<(String, String)>, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let d: Delegate = parse(&body)?;
    let command = DirectoryCommand::SetDelegate {
        policy: PolicyId::from(policy.as_str()),
        party: PartyId::from(party.as_str()),
        access: d.access,
    };
    directory(&s, &headers, session, command).await
}

#[derive(Deserialize)]
struct Consent {
    consent: bool,
}

async fn family_consent(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let c: Consent = parse(&body)?;
    let command = DirectoryCommand::SetFamilyConsent {
        policy: PolicyId::from(policy.as_str()),
        consent: c.consent,
    };
    directory(&s, &headers, session, command).await
}

// --- the adviser portal ---

async fn book(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    ok(s.ask(Question::Book {
        adviser: session.party,
        at: session.at,
    })
    .await?)
}

async fn held_balance(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    ok(s.ask(Question::HeldBalance { party: session.party }).await?)
}

// --- the employer portal ---

async fn schemes(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let all = s
        .ask(Question::Policies {
            party: session.party,
            at: session.at,
        })
        .await?;
    let schemes: Vec<&Value> = all
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["relationships"].as_array().is_some_and(|r| r.iter().any(|x| x == "employer_admin")))
        .collect();
    ok(json!(schemes))
}

#[derive(Deserialize)]
struct Admin {
    admin: bool,
}

async fn scheme_admin(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((policy, party)): Path<(String, String)>,
    body: Bytes,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let a: Admin = parse(&body)?;
    let command = DirectoryCommand::SetEmployerAdmin {
        policy: PolicyId::from(policy.as_str()),
        party: PartyId::from(party.as_str()),
        admin: a.admin,
    };
    directory(&s, &headers, session, command).await
}

#[derive(Deserialize)]
struct SecondAdmin {
    changes: std::collections::BTreeSet<String>,
}

async fn scheme_second_admin(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let c: SecondAdmin = parse(&body)?;
    let command = DirectoryCommand::RequireSecondAdmin {
        policy: PolicyId::from(policy.as_str()),
        changes: c.changes,
    };
    directory(&s, &headers, session, command).await
}

// --- the staff console ---

async fn staff_me(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    ok(s.ask(Question::OwnAccess {
        staff: session.party,
        at: session.at,
    })
    .await?)
}

async fn staff_admin(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let view = s
        .ask(Question::StaffAdmin {
            staff: session.party,
            at: session.at,
        })
        .await?;
    if view["allowed"] != true {
        return Err(Problem::forbidden("you do not administer staff access"));
    }
    ok(view)
}

/// A refusal by the four questions, with the reason the person is given.
impl AppState {
    /// Write a staff read to the access log, then refuse it if it was not
    /// allowed. The log is written first and the read fails if it cannot be:
    /// nobody sees a customer's data without a line saying who, what and why.
    /// Refusals are logged too.
    async fn logged(&self, view: Value) -> Result<Value, Problem> {
        // A node from before the access log was wired in answers without the request. Fail closed until it is upgraded.
        let (request, decision): (AccessRequest, Decision) = serde_json::from_value(json!([view["request"], view["decision"]]))
            .map_err(|_| Problem::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "the access log cannot be written yet"))?;
        let key = format!(
            "access:{}:{}:{}:{}",
            request.staff,
            request.at,
            request.action,
            request.record.as_deref().unwrap_or_default()
        );
        let actor = Actor::Staff(request.staff.clone());
        self.propose(key, actor, Command::Staff(StaffCommand::Record { request, decision })).await?;
        match denied(&view) {
            Some(problem) => Err(problem),
            None => Ok(view),
        }
    }
}

fn denied(view: &Value) -> Option<Problem> {
    (view["decision"]["allowed"] != true).then(|| Problem::new(StatusCode::FORBIDDEN, "not_authorised", view["explanation"].as_str().unwrap_or("not allowed")))
}

#[derive(Deserialize)]
struct Why {
    /// The case or task the person is working on.
    case: Option<String>,
    /// Scope keys that reach the record, such as `queue:claims-life`.
    #[serde(default)]
    scope: Option<String>,
}

async fn staff_policy(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>, Query(why): Query<Why>) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let view = s
        .ask(Question::StaffPolicy {
            staff: session.party,
            policy: PolicyId::from(policy.as_str()),
            scopes: why
                .scope
                .iter()
                .flat_map(|s| s.split(','))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            purpose: why.case.map(|reference| Purpose {
                kind: PurposeKind::Task,
                reference,
            }),
            at: session.at,
        })
        .await?;
    let view = s.logged(view).await?;
    if view["found"] != true {
        return Err(Problem::not_found());
    }
    ok(view)
}

/// Staff access commands: request, approve and revoke grants, break-glass,
/// recertify, administer. The module checks each against the caller.
async fn staff_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let command: StaffCommand = parse(&body)?;
    if matches!(command, StaffCommand::Record { .. }) {
        // The access log is written by this service as it serves a read, never by the person read about.
        return Err(Problem::new(StatusCode::FORBIDDEN, "forbidden", "the access log is not written by hand"));
    }
    ok(
        s.propose(key(&headers, &session.party.0)?, Actor::Staff(session.party), Command::Staff(command))
            .await?,
    )
}

/// A request made for a customer who called. The customer approves it in the app.
async fn staff_request(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let new: NewRequest = parse(&body)?;
    let command = Command::SubmitRequest {
        id: new.id,
        policy: new.policy,
        kind: new.kind,
        module: new.module,
        payload: new.payload,
        signed: false,
    };
    accepted(s.propose(key(&headers, &session.party.0)?, Actor::Staff(session.party), command).await?)
}

async fn workbench(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let view = s
        .ask(Question::Workbench {
            staff: session.party,
            party: None,
            at: session.at,
        })
        .await?;
    ok(s.logged(view).await?)
}

async fn workbench_party(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(party): Path<String>) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let view = s
        .ask(Question::Workbench {
            staff: session.party,
            party: Some(PartyId::from(party.as_str())),
            at: session.at,
        })
        .await?;
    let view = s.logged(view).await?;
    if view["position"].is_null() {
        return Err(Problem::not_found());
    }
    ok(view["position"].clone())
}

async fn proof_of_debt(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(party): Path<String>) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let view = s
        .ask(Question::ProofOfDebt {
            staff: session.party,
            party: PartyId::from(party.as_str()),
            at: session.at,
        })
        .await?;
    let view = s.logged(view).await?;
    if view["extract"].is_null() {
        return Err(Problem::not_found());
    }
    ok(view["extract"].clone())
}

/// Prepare, approve, decline or withdraw a workbench action, or change its thresholds.
async fn workbench_action(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let command: WorkbenchCommand = parse(&body)?;
    if matches!(
        command,
        WorkbenchCommand::Publish { .. } | WorkbenchCommand::RecordStatus { .. } | WorkbenchCommand::Resolve { .. }
    ) {
        return Err(Problem::forbidden(
            "that command comes from the module that owns the money, not from the console",
        ));
    }
    ok(
        s.propose(key(&headers, &session.party.0)?, Actor::Staff(session.party), Command::Workbench(command))
            .await?,
    )
}

// --- other modules ---

/// The module a service caller acts as: named in `x-module`, and one of the
/// app roles on its token.
async fn module_caller(s: &AppState, headers: &HeaderMap) -> Result<String, Problem> {
    let caller = s.workforce.authenticate(headers).await?;
    let module = header(headers, "x-module").ok_or_else(|| Problem::bad_request("name the module you act as in x-module"))?;
    if !caller.has_role(&module) || module == GATEWAY {
        return Err(Problem::forbidden(format!("your token does not carry the role of module {module}")));
    }
    Ok(module)
}

async fn module_command(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Answer {
    let module = module_caller(&s, &headers).await?;
    let command: Command = parse(&body)?;
    ok(s.propose(key(&headers, &format!("module:{module}"))?, Actor::Module(module), command).await?)
}

async fn module_pending(State(s): State<Shared>, headers: HeaderMap) -> Answer {
    let module = module_caller(&s, &headers).await?;
    ok(s.ask(Question::Pending { module }).await?)
}

// --- operations console and status page ---

async fn ops_regions(State(s): State<Shared>, headers: HeaderMap) -> Answer {
    let caller = s.workforce.authenticate(&headers).await?;
    if !caller.has_role(OPERATIONS) {
        return Err(Problem::forbidden("the operations console needs the operations role"));
    }
    let report = s.regions.report().await;
    let module = s.channel.ask(Question::Status).await.ok();
    ok(json!({ "regions": report, "status": StatusPage::from(&report), "channels": module }))
}

/// The public status, in plain words. No sign-in.
async fn status_json(State(s): State<Shared>) -> Answer {
    let report = s.regions.report().await;
    let mut response = Json(StatusPage::from(&report)).into_response();
    response.headers_mut().insert("cache-control", HeaderValue::from_static("max-age=15"));
    Ok(response)
}

async fn ready(State(s): State<Shared>) -> Answer {
    s.ask(Question::Status).await?;
    Ok("ready".into_response())
}

/// Headers every page and page asset carries.
fn hardened(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}

fn page(s: &AppState, body: &'static str) -> Response {
    let mut response = Html(body).into_response();
    // The policy is built from validated settings and base64 hashes, so it is always a legal header.
    if let Ok(policy) = HeaderValue::from_str(&s.ui.policy(body)) {
        response.headers_mut().insert("content-security-policy", policy);
    }
    response.headers_mut().insert("cache-control", HeaderValue::from_static("no-cache"));
    hardened(response)
}

fn asset(kind: &'static str, body: &'static str) -> Response {
    hardened(([("content-type", kind), ("cache-control", "no-cache")], body).into_response())
}

async fn ui_config(State(s): State<Shared>) -> Response {
    hardened(([("cache-control", "no-cache")], Json(&s.ui)).into_response())
}

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        // The customer app, shared by every External ID caller.
        .route("/app/v1/me", get(me))
        .route("/app/v1/sync", get(sync))
        .route("/app/v1/policies/{policy}", get(policy))
        .route("/app/v1/policies/{policy}/advisers", post(appoint_adviser))
        .route("/app/v1/policies/{policy}/advisers/{adviser}", delete(remove_adviser))
        .route("/app/v1/policies/{policy}/consent", put(family_consent))
        .route("/app/v1/policies/{policy}/delegates/{party}", put(delegate))
        .route("/app/v1/requests", get(requests).post(submit_request))
        .route("/app/v1/requests/{id}", delete(cancel_request))
        .route("/app/v1/requests/{id}/approve", post(approve_request))
        .route("/app/v1/challenges", post(challenge))
        .route("/app/v1/devices", get(devices).post(enrol_device))
        .route("/app/v1/devices/{device}", delete(revoke_device))
        .route("/adviser/v1/book", get(book))
        .route("/adviser/v1/held-balance", get(held_balance))
        .route("/employer/v1/schemes", get(schemes))
        .route("/employer/v1/schemes/{policy}/admins/{party}", put(scheme_admin))
        .route("/employer/v1/schemes/{policy}/second-admin", put(scheme_second_admin))
        .route("/staff/v1/me", get(staff_me))
        .route("/staff/v1/admin", get(staff_admin))
        .route("/staff/v1/access", post(staff_command))
        .route("/staff/v1/policies/{policy}", get(staff_policy))
        .route("/staff/v1/requests", post(staff_request))
        .route("/staff/v1/workbench", get(workbench))
        .route("/staff/v1/workbench/actions", post(workbench_action))
        .route("/staff/v1/workbench/{party}", get(workbench_party))
        .route("/staff/v1/workbench/{party}/proof-of-debt", get(proof_of_debt))
        .route("/internal/v1/commands", post(module_command))
        .route("/internal/v1/pending", get(module_pending))
        .route("/ops/v1/regions", get(ops_regions))
        .route("/status.json", get(status_json))
        .route("/status", get(|State(s): State<Shared>| async move { page(&s, crate::ui::STATUS) }))
        .route("/", get(|State(s): State<Shared>| async move { page(&s, crate::ui::INDEX) }))
        .route("/adviser", get(|State(s): State<Shared>| async move { page(&s, crate::ui::ADVISER) }))
        .route("/employer", get(|State(s): State<Shared>| async move { page(&s, crate::ui::EMPLOYER) }))
        .route("/staff", get(|State(s): State<Shared>| async move { page(&s, crate::ui::STAFF) }))
        .route("/ops", get(|State(s): State<Shared>| async move { page(&s, crate::ui::OPS) }))
        .route("/ui/app.js", get(|| async { asset("text/javascript; charset=utf-8", crate::ui::SCRIPT) }))
        .route("/ui/app.css", get(|| async { asset("text/css; charset=utf-8", crate::ui::STYLE) }))
        .route("/ui/config.json", get(ui_config))
        .with_state(state)
}
