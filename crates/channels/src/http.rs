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
    /// The unit pricing module, for the staff console.
    pub pricing: Arc<dyn crate::forward::Source>,
    /// The finance ledger, for the finance console.
    pub finance: Arc<dyn crate::forward::Source>,
    /// The core's partitions, where claims live, for the claims console.
    pub claims: Arc<dyn crate::claims::Partitions>,
    /// The reinsurance module, for the exceptions console.
    pub reinsurance: Arc<dyn crate::forward::Source>,
    /// The customer master, for a person's own record. `None` where it is
    /// not configured: those routes answer 503.
    pub master: Option<Arc<dyn crate::customers::Master>>,
    /// The complaints module, for the complaints and conduct workbench.
    pub complaints: Arc<dyn crate::forward::Source>,
    /// The underwriting intake, for signing disclosures. `None` where it is
    /// not configured: those routes answer 503.
    pub intake: Option<Arc<dyn crate::disclosures::Intake>>,
    /// The documents module, for downloading a PDF. `None` where it is not
    /// configured: those routes answer 503.
    pub documents: Option<Arc<dyn crate::documents::Documents>>,
    /// The group service, for an employer's membership files and invoices.
    /// `None` where it is not configured: those routes answer 503.
    pub group: Option<Arc<dyn crate::group::Group>>,
    /// The commission module, for replacement matches.
    pub commission: Arc<dyn crate::forward::Source>,
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
            "duplicate" | "already_resolved" | "not_awaiting" | "already_approved" | "separation" | "signed" | "exists" => StatusCode::CONFLICT,
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
fn device_required(what: &str) -> Problem {
    Problem::new(
        StatusCode::UNAUTHORIZED,
        "device_required",
        format!("{what} needs proof of an enrolled device's key"),
    )
}

impl AppState {
    fn master(&self) -> Result<&Arc<dyn crate::customers::Master>, Problem> {
        self.master.as_ref().ok_or_else(|| {
            Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "customer_master_unavailable",
                "the customer master is not configured here",
            )
        })
    }
}

/// The customer master's answer, passed on: its status and body, or 503
/// when it could not be reached.
fn mastered(answer: Result<crate::customers::Answered, String>) -> Answer {
    let (status, body) = answer.map_err(|e| Problem::new(StatusCode::SERVICE_UNAVAILABLE, "customer_master_unavailable", e))?;
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    Ok((status, Json(body)).into_response())
}

/// Your own record in the customer master: name, contact details and
/// consents, with the version a change must name.
async fn profile(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    mastered(s.master()?.record(&session.party).await)
}

/// The intake's answer, as it gave it; 503 when it cannot be reached.
fn intake_answer(answer: Result<crate::disclosures::Answered, String>) -> Answer {
    let (status, body) = answer.map_err(|e| Problem::new(StatusCode::SERVICE_UNAVAILABLE, "underwriting_intake_unavailable", e))?;
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response())
}

impl AppState {
    fn intake(&self) -> Result<&Arc<dyn crate::disclosures::Intake>, Problem> {
        self.intake.as_ref().ok_or_else(|| {
            Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "underwriting_intake_unavailable",
                "the underwriting intake is not configured here",
            )
        })
    }

    /// The enrolled key of the device this person proved, once it is clear
    /// they are the life on this proposal. Anyone else gets 404.
    async fn signer(&self, session: &Session, proposal: &str, life: &str) -> Result<Option<String>, Problem> {
        let signer = self
            .ask(Question::DisclosureSigner {
                party: session.party.clone(),
                device: session.device.clone(),
                proposal: proposal.to_string(),
                life: life.to_string(),
            })
            .await?;
        if signer["life"] != true {
            return Err(Problem::not_found());
        }
        Ok(signer["public_key"].as_str().map(str::to_string))
    }
}

/// What the person insured signs for their answers. The answers go to the
/// intake, which checks them against the question set and keeps nothing.
async fn disclosure_to_sign(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((proposal, life)): Path<(String, String)>,
    body: Bytes,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    s.signer(&session, &proposal, &life).await?;
    let request = crate::disclosures::answers(&proposal, &life, &parse(&body)?).map_err(Problem::bad_request)?;
    intake_answer(s.intake()?.to_sign(request).await)
}

/// The signed answers, from a device the person enrolled.
async fn disclosure_submit(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((proposal, life)): Path<(String, String)>,
    body: Bytes,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let Some(device) = session.device.clone() else {
        return Err(device_required("signing your answers"));
    };
    let key = s
        .signer(&session, &proposal, &life)
        .await?
        .ok_or_else(|| device_required("signing your answers"))?;
    let body: Value = parse(&body)?;
    let request = crate::disclosures::answers(&proposal, &life, &body).map_err(Problem::bad_request)?;
    let request = crate::disclosures::signed(request, &body, &device, &key).map_err(|e| Problem::new(StatusCode::BAD_REQUEST, "bad_signature", e))?;
    intake_answer(s.intake()?.submit(request).await)
}

/// A document's PDF as the documents module gave it: the file on 200,
/// that module's refusal otherwise (404 for one that is not the asker's,
/// 403 for a clinical one to staff, 410 for one destroyed).
async fn fetched(s: &AppState, raw: &str, asker: crate::documents::Asker) -> Answer {
    let id = crate::documents::id(raw).ok_or_else(Problem::not_found)?;
    let documents = s.documents.as_ref().ok_or_else(|| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "documents_unavailable",
            "the documents module is not configured here",
        )
    })?;
    let got = documents
        .pdf(id, asker)
        .await
        .map_err(|e| Problem::new(StatusCode::SERVICE_UNAVAILABLE, "documents_unavailable", e))?;
    match got.status {
        200 => Ok((
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, "application/pdf".to_string()),
                (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
                (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{id}.pdf\"")),
            ],
            got.body,
        )
            .into_response()),
        404 => Err(Problem::not_found()),
        403 => Err(Problem::forbidden("that document stays with the team that holds it")),
        410 => Err(Problem::new(
            StatusCode::GONE,
            "destroyed",
            "that document has been destroyed under its retention rule",
        )),
        other => Err(Problem::new(
            StatusCode::BAD_GATEWAY,
            "documents_refused",
            format!("the documents module answered {other}"),
        )),
    }
}

/// One of your own documents, as a PDF.
async fn own_document(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(id): Path<String>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    fetched(&s, &id, crate::documents::Asker::Party(session.party.0)).await
}

/// A document for a member of staff who holds `documents.download`, with
/// the purpose they gave. Clinical documents are not served to staff.
async fn staff_document(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(id): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let standing = s
        .ask(Question::DocumentsStaff {
            staff: session.party.clone(),
            at: session.at,
        })
        .await?;
    if standing != true {
        return Err(Problem::forbidden("your roles do not include downloading documents"));
    }
    let purpose = fields
        .get("purpose")
        .map(|p| p.trim())
        .filter(|p| !p.is_empty() && p.len() <= 200)
        .ok_or_else(|| Problem::bad_request("say why you need it: the purpose is logged with the download"))?;
    fetched(
        &s,
        &id,
        crate::documents::Asker::Staff {
            actor: session.party.0,
            purpose: purpose.to_string(),
        },
    )
    .await
}

/// Change your contact details or consents, from an enrolled device.
async fn profile_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    if session.device.is_none() {
        return Err(device_required("changing your details"));
    }
    let key = key(&headers, &session.party.0)?;
    let command: Value = parse(&body)?;
    crate::customers::check(&command).map_err(Problem::forbidden)?;
    let version = header(&headers, "if-match");
    mastered(s.master()?.command(&session.party, version, key, command).await)
}

/// A copy of everything the insurer holds about you, from an enrolled device.
async fn profile_export(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    if session.device.is_none() {
        return Err(device_required("a copy of your record"));
    }
    mastered(s.master()?.export(&session.party).await)
}

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
    if new.kind == crate::replacements::DISPUTE {
        return Err(Problem::bad_request("dispute a replacement from your matches in the adviser portal"));
    }
    let action = request_action(&new.id, new.policy.as_ref(), &new.kind, &new.payload);
    let signed = redeem(&s, &headers, &session, &action, new.signed).await?;
    let routing = match &s.master {
        Some(master) => Some(master.routing(&session.party).await),
        None => None,
    };
    let route_to_person = crate::customers::route_to_person(routing);
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
                route_to_person,
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

/// The claims on a policy as its adviser or a scheme's administrator may
/// see them: each claim's ID, status, and the dates it was notified and
/// closed, and never its cause (Mitchell's decision, 13 November 2026).
/// Anyone else, including a policy the party has no part in, gets 404.
async fn policy_claims(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let all = s
        .ask(Question::Policies {
            party: session.party,
            at: session.at,
        })
        .await?;
    let allowed = all.as_array().into_iter().flatten().any(|p| {
        p["policy"] == policy.as_str()
            && p["relationships"]
                .as_array()
                .is_some_and(|r| r.iter().any(|x| x == "adviser" || x == "employer_admin"))
    });
    if !allowed {
        return Err(Problem::not_found());
    }
    let question = ic_core::claims_module::report::Question::PolicyStatus { policy: policy.clone() };
    let mut rows: Vec<Value> = Vec::new();
    for (_, reply) in s.claims_everywhere(&question).await? {
        if let ic_core::claims_module::report::Reply::PolicyStatus(found) = reply {
            rows.extend(
                found
                    .into_iter()
                    .map(|r| json!({ "claim": r.claim, "status": r.status, "notified_on": r.notified_on, "closed_on": r.closed_on })),
            );
        }
    }
    ok(json!({ "rows": rows }))
}

impl AppState {
    /// The replacement matches on business this party wrote as an adviser.
    async fn own_matches(&self, party: &PartyId) -> Result<Value, Problem> {
        let engagements = self.ask(Question::AdviserEngagements { party: party.clone() }).await?;
        if engagements.as_array().is_none_or(Vec::is_empty) {
            return Ok(json!([]));
        }
        let matches = self
            .commission
            .ask(crate::replacements::question())
            .await
            .map_err(|e| module_problem("commission", StatusCode::BAD_REQUEST, e))?;
        Ok(crate::replacements::for_adviser(&matches, &engagements))
    }
}

/// The replacement matches on business this adviser wrote.
async fn adviser_replacements(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    ok(s.own_matches(&session.party).await?)
}

/// Dispute one of this adviser's matches. The dispute and its reason are
/// recorded here as the adviser's request, carried to the commission
/// module as `DisputeReplacement` with the request's ID as the reference
/// and the adviser named, and the request is resolved with what that
/// module said. A retry with the same key carries it again; the commission
/// module applies it once.
async fn adviser_dispute(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let request: Value = parse(&body)?;
    let theirs = s.own_matches(&session.party).await?;
    let (id, payload) = crate::replacements::dispute(&request, &theirs).map_err(Problem::bad_request)?;
    let key = key(&headers, &session.party.0)?;
    s.propose(
        key.clone(),
        Actor::Party(session.party.clone()),
        Command::SubmitRequest {
            id: id.clone(),
            policy: None,
            kind: crate::replacements::DISPUTE.to_string(),
            module: crate::replacements::DISPUTE_MODULE.to_string(),
            payload: payload.clone(),
            signed: false,
            route_to_person: false,
        },
    )
    .await?;
    let output = s
        .commission
        .tell(crate::replacements::dispute_envelope(&id, &payload, &session.party))
        .await
        .map_err(|e| module_problem("commission", StatusCode::BAD_REQUEST, e))?;
    let refused = output["result"].get("Err").map(|e| {
        (
            e["code"].as_str().unwrap_or("refused").to_string(),
            e["message"].as_str().unwrap_or("the commission module refused it").to_string(),
        )
    });
    let resolve = Command::ResolveRequest {
        id: id.clone(),
        reference: refused.is_none().then(|| id.clone()),
        rejected: refused.clone(),
    };
    // Resolved already by an earlier try: what it said then stands.
    match s
        .propose(
            format!("resolve:{key}"),
            Actor::Module(crate::replacements::DISPUTE_MODULE.to_string()),
            resolve,
        )
        .await
    {
        Err(p) if p.code != "not_pending" => return Err(p),
        _ => {}
    }
    match refused {
        Some((code, message)) => Err(Problem::new(StatusCode::CONFLICT, &code, message)),
        None => ok(json!({ "disputed": id })),
    }
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

// --- the employer's group scheme, held by the group service ---

impl AppState {
    fn group(&self) -> Result<&Arc<dyn crate::group::Group>, Problem> {
        self.group
            .as_ref()
            .ok_or_else(|| Problem::new(StatusCode::SERVICE_UNAVAILABLE, "group_unavailable", "the group service is not configured here"))
    }

    /// The group service's scheme on this policy, once it is clear the
    /// signed-in person administers the policy today. Any other policy, or
    /// one with no scheme, is not found, so nobody learns which exist.
    async fn administered_scheme(&self, session: &Session, policy: &str) -> Result<String, Problem> {
        let all = self
            .ask(Question::Policies {
                party: session.party.clone(),
                at: session.at,
            })
            .await?;
        let administers = all
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| p["policy"] == policy && p["relationships"].as_array().is_some_and(|r| r.iter().any(|x| x == "employer_admin")));
        if !administers {
            return Err(Problem::not_found());
        }
        let (status, answer) = self.group()?.ask(crate::group::scheme_of(policy)).await.map_err(group_down)?;
        match (status, crate::group::named_scheme(&answer)) {
            (200, Some(scheme)) => Ok(scheme),
            (200, None) => Err(Problem::not_found()),
            (status, _) => Err(Problem::new(
                StatusCode::BAD_GATEWAY,
                "group_unavailable",
                format!("the group service answered {status} for the scheme on {policy}"),
            )),
        }
    }
}

fn group_down(e: String) -> Problem {
    Problem::new(StatusCode::SERVICE_UNAVAILABLE, "group_unavailable", e)
}

/// The group service's answer as it gave it.
fn grouped(answer: Result<crate::group::Answered, String>) -> Answer {
    let (status, body) = answer.map_err(group_down)?;
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(body)).into_response())
}

/// The scheme's standing, its invoices, one file with its preview, or one
/// member as the employer sees them.
async fn group_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((policy, name)): Path<(String, String)>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let scheme = s.administered_scheme(&session, &policy).await?;
    let question = crate::group::question(&name, &scheme, |f| q.get(f).cloned()).map_err(Problem::bad_request)?;
    let (status, body) = s.group()?.ask(question).await.map_err(group_down)?;
    let body = if name == "member" && status == 200 {
        crate::group::employer_member(&body)
    } else {
        body
    };
    grouped(Ok((status, body)))
}

/// Upload a membership file. It is stored in quarantine and scanned
/// before anything reads it; the preview follows on the file's question.
async fn group_upload(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((policy, file)): Path<(String, String)>,
    body: Bytes,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let file = crate::group::name(&file).ok_or_else(|| Problem::bad_request("a file name is letters, digits, dots, dashes and underscores"))?;
    if body.is_empty() {
        return Err(Problem::bad_request("the file is empty"));
    }
    let scheme = s.administered_scheme(&session, &policy).await?;
    grouped(s.group()?.upload(&scheme, file, &session.party.0, body.to_vec()).await)
}

/// Approve or withdraw a file, or declare a pool, as the signed-in
/// administrator.
async fn group_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>, body: Bytes) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let key = key(&headers, &session.party.0)?;
    let sent: Value = parse(&body)?;
    let scheme = s.administered_scheme(&session, &policy).await?;
    let command = crate::group::command(&scheme, &sent).map_err(Problem::bad_request)?;
    grouped(s.group()?.command(&key, &session.party.0, command).await)
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

impl AppState {
    /// What this person may do in the unit pricing console. Nobody without
    /// a unit pricing action gets as far as that module.
    async fn pricing_standing(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Session, Value), Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let standing = self
            .ask(Question::PricingStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if standing["view"] != true {
            return Err(Problem::forbidden("your roles do not include unit pricing"));
        }
        Ok((session, standing))
    }
}

/// What to say when another module would not answer or could not be
/// reached: its own refusal as `refused`, anything else as unavailable.
/// Another module's refusal of a person's command: 403 where that module
/// says the person may not (it keeps its own authority), 404 for a claim it
/// does not hold, 409 for anything else it refused.
fn module_refusal(code: &str, message: &str) -> Problem {
    let status = match code {
        "unknown_staff" | "authority" | "not_authorised" | "unauthorised" | "not_owner" | "no_access" => StatusCode::FORBIDDEN,
        "unknown_claim" => StatusCode::NOT_FOUND,
        _ => StatusCode::CONFLICT,
    };
    Problem::new(status, code, message)
}

fn module_problem(what: &str, refused: StatusCode, reason: String) -> Problem {
    match reason.strip_prefix(crate::forward::REFUSED) {
        Some(why) => Problem::new(refused, "refused", why),
        None => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!("{what}_unavailable"),
            format!("{} could not be reached: {reason}", what.replace('_', " ")),
        ),
    }
}

fn pricing_down(reason: String) -> Problem {
    module_problem("unit_pricing", StatusCode::BAD_REQUEST, reason)
}

impl From<crate::complaints::Refusal> for Problem {
    fn from(refusal: crate::complaints::Refusal) -> Self {
        match refusal {
            crate::complaints::Refusal::Bad(why) => Problem::bad_request(why),
            crate::complaints::Refusal::Forbidden(why) => Problem::forbidden(why),
        }
    }
}

impl AppState {
    /// What this person may do at the complaints workbench, and the same
    /// for one other member of staff. Nobody without a seat gets further.
    async fn complaints_standing(&self, session: &Session, about: Option<PartyId>) -> Result<(Value, Value), Problem> {
        let answer = self
            .ask(Question::ComplaintsStaff {
                staff: session.party.clone(),
                about,
                at: session.at,
            })
            .await?;
        if !answer["me"]["seats"].as_array().is_some_and(|seats| !seats.is_empty()) {
            return Err(Problem::forbidden("your roles do not include complaints"));
        }
        Ok((answer["me"].clone(), answer["other"].clone()))
    }
}

/// One of the workbench's questions to the complaints module: the queue, a
/// complaint, its timers, the register, the report, conduct cases. `me`
/// answers from here.
async fn complaints_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let (standing, _) = s.complaints_standing(&session, None).await?;
    if name == "me" {
        return ok(standing);
    }
    let question = crate::complaints::question(&name, &fields, &session.party, &standing)?;
    // Every question is built here from checked fields, so a refusal is that module saying this person may not see it.
    let answer = s
        .complaints
        .ask(question)
        .await
        .map_err(|e| module_problem("complaints", StatusCode::FORBIDDEN, e))?;
    if answer.is_null() {
        return Err(Problem::not_found());
    }
    ok(answer)
}

#[derive(Deserialize)]
struct ComplaintsCommand {
    /// The seat the person acts in: `intake`, `complaints`, `legal` or `conduct`.
    #[serde(rename = "as")]
    seat: String,
    command: Value,
}

/// One person's command to the complaints module, in the seat they name.
async fn complaints_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let sent: ComplaintsCommand = parse(&body)?;
    let about = crate::complaints::depends_on(&sent.command, &session.party);
    let (standing, other) = s.complaints_standing(&session, about.clone()).await?;
    let key = key(&headers, &session.party.0)?;
    let other = about.as_ref().map(|who| (who, &other));
    let envelope = crate::complaints::envelope(&key, &session.party, &sent.seat, session.at, sent.command, &standing, other)?;
    let output = s
        .complaints
        .tell(envelope)
        .await
        .map_err(|e| module_problem("complaints", StatusCode::BAD_REQUEST, e))?;
    if let Some(refused) = output["result"].get("Err") {
        let code = refused["code"].as_str().unwrap_or("refused");
        return Err(Problem::new(
            StatusCode::CONFLICT,
            code,
            refused["message"].as_str().unwrap_or("the complaints module refused it"),
        ));
    }
    ok(json!({ "accepted": true, "duplicate": output["duplicate"], "events": output["events"] }))
}

impl AppState {
    /// What this person may do in the finance console. Nobody without a
    /// finance action gets as far as that module.
    async fn finance_standing(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Session, Value), Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let standing = self
            .ask(Question::FinanceStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if standing["view"] != true {
            return Err(Problem::forbidden("your roles do not include finance"));
        }
        Ok((session, standing))
    }
}

impl AppState {
    /// Whether this person may open the claims console. What they may decide
    /// is the claims module's authority book.
    async fn claims_standing(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<Session, Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let standing = self
            .ask(Question::ClaimsStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if standing["view"] != true {
            return Err(Problem::forbidden("your roles do not include claims"));
        }
        Ok(session)
    }

    /// Every partition's answer, with the partition it came from. A partition
    /// whose business date is not open has none.
    async fn claims_everywhere(
        &self,
        question: &ic_core::claims_module::report::Question,
    ) -> Result<Vec<(ic_ledger::PartitionId, ic_core::claims_module::report::Reply)>, Problem> {
        let asks = (0..self.claims.count()).map(|p| async move { (p, self.claims.ask(p, question.clone()).await) });
        let mut out = Vec::new();
        for (p, answer) in futures::future::join_all(asks).await {
            if let Some(reply) = answer.map_err(claims_down)? {
                out.push((p, reply));
            }
        }
        Ok(out)
    }

    /// The partition holding a claim, and the claim as its claimant sees it.
    async fn find_claim(&self, claim: &str) -> Result<(ic_ledger::PartitionId, Box<ic_core::claims_module::report::ClaimRow>), Problem> {
        let question = ic_core::claims_module::report::Question::Claim { claim: claim.to_string() };
        self.claims_everywhere(&question)
            .await?
            .into_iter()
            .find_map(|(p, reply)| match reply {
                ic_core::claims_module::report::Reply::Claim(Some(row)) => Some((p, row)),
                _ => None,
            })
            .ok_or_else(|| {
                // A policy moving partition answers unknown on both sides for a moment.
                Problem::new(
                    StatusCode::NOT_FOUND,
                    "unknown_claim",
                    format!("no partition holds the claim {claim}; if its policy is moving, try again shortly"),
                )
            })
    }
}

impl AppState {
    /// What this person may do in the reinsurance exceptions console. Nobody
    /// without a reinsurance action gets as far as that module.
    async fn reinsurance_standing(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Session, Value), Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let standing = self
            .ask(Question::ReinsuranceStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if standing["view"] != true {
            return Err(Problem::forbidden("your roles do not include reinsurance"));
        }
        Ok((session, standing))
    }

    /// Send one command to the reinsurance module and say what became of it.
    async fn to_reinsurance(&self, envelope: Value) -> Answer {
        let output = self
            .reinsurance
            .tell(envelope)
            .await
            .map_err(|e| module_problem("reinsurance", StatusCode::BAD_REQUEST, e))?;
        if let Some(refused) = output["result"].get("Err") {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                refused["code"].as_str().unwrap_or("refused"),
                refused["message"].as_str().unwrap_or("the reinsurance module refused it"),
            ));
        }
        ok(json!({ "accepted": true, "duplicate": output["duplicate"] }))
    }
}

/// What this person may do with replacement matches, refused if nothing.
async fn replacement_standing(s: &AppState, session: &Session, purpose: &str) -> Result<Value, Problem> {
    let standing = s
        .ask(Question::ReplacementStaff {
            staff: session.party.clone(),
            at: session.at,
            purpose: purpose.to_string(),
        })
        .await?;
    if standing["view"] != true {
        return Err(Problem::forbidden("your roles do not include the held balance workbench"));
    }
    Ok(standing)
}

/// Every replacement the commission module matched, and whether this person may decide a disputed one.
async fn replacements(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let standing = replacement_standing(&s, &session, "").await?;
    let matches = s
        .commission
        .ask(crate::replacements::question())
        .await
        .map_err(|e| module_problem("commission", StatusCode::BAD_REQUEST, e))?;
    ok(json!({ "decide": standing["decide"], "matches": matches }))
}

/// Decide a disputed match, as the person signed in, with the purpose they gave.
async fn replacement_decide(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let request: Value = parse(&body)?;
    let purpose = request["purpose"].as_str().map(str::trim).unwrap_or_default();
    let standing = replacement_standing(&s, &session, purpose).await?;
    if standing["decide"] != true {
        return Err(Problem::forbidden("a disputed match is decided by the conduct officer"));
    }
    if purpose.is_empty() {
        return Err(Problem::bad_request("say why you are deciding it: the purpose is recorded with the decision"));
    }
    let key = key(&headers, &session.party.0)?;
    let command = crate::replacements::decision(&request).map_err(Problem::bad_request)?;
    let output = s
        .commission
        .tell(crate::replacements::envelope(&key, standing["actor"].clone(), command))
        .await
        .map_err(|e| module_problem("commission", StatusCode::BAD_REQUEST, e))?;
    if let Some(refused) = output["result"].get("Err") {
        return Err(module_refusal(
            refused["code"].as_str().unwrap_or("refused"),
            refused["message"].as_str().unwrap_or("the commission module refused it"),
        ));
    }
    ok(json!({ "accepted": true, "duplicate": output["duplicate"] }))
}

/// The business date a reinsurance command is made on: the latest any
/// partition has reported, or today's date while none has.
fn business_date(standing: &Value, at: At) -> String {
    standing["today"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(at, 0).map(|t| t.date_naive().to_string()).unwrap_or_default())
}

/// One of the console's questions to the reinsurance module. `me` answers
/// from here, with the abandonments waiting for a signature.
async fn reinsurance_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let (_, standing) = s.reinsurance_standing(&headers, &method, &uri, b"").await?;
    if name == "me" {
        return ok(standing);
    }
    let question = crate::reinsurance::question(&name, &fields).map_err(Problem::bad_request)?;
    let answer = s
        .reinsurance
        .ask(question)
        .await
        .map_err(|e| module_problem("reinsurance", StatusCode::BAD_REQUEST, e))?;
    if answer.is_null() {
        return Err(Problem::not_found());
    }
    ok(answer)
}

/// A person's command about an exception. One a person sends alone goes
/// straight to the module; an abandonment is kept until someone else signs.
async fn reinsurance_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let (session, standing) = s.reinsurance_standing(&headers, &method, &uri, &body).await?;
    if standing["operate"] != true {
        return Err(Problem::forbidden("you may read the exceptions but hold no role to act on them"));
    }
    let key = key(&headers, &session.party.0)?;
    let command: Value = parse(&body)?;
    let (name, countersigned) = crate::reinsurance::check(&command).map_err(Problem::bad_request)?;
    if countersigned {
        let reason = command[name]["reason"].as_str().unwrap_or_default().to_string();
        let prepare = crate::countersign::CountersignCommand::Prepare {
            id: key.clone(),
            module: crate::reinsurance::MODULE.to_string(),
            command: command.clone(),
            reason,
        };
        s.propose(format!("countersign:{key}"), Actor::Staff(session.party), Command::Countersign(prepare))
            .await?;
        return ok(json!({ "prepared": key, "needs": "a second person with reinsurance.approve to sign it" }));
    }
    let on = business_date(&standing, session.at);
    s.to_reinsurance(crate::reinsurance::envelope(&key, &on, command, None)).await
}

/// Sign, or take back, an abandonment waiting for a second person.
async fn reinsurance_decide(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path((verb, id)): Path<(String, String)>) -> Answer {
    use crate::countersign::CountersignCommand as C;
    let (session, standing) = s.reinsurance_standing(&headers, &method, &uri, b"").await?;
    let actor = Actor::Staff(session.party.clone());
    let fresh = |what: &str| format!("countersign:{id}:{what}:{}:{}", session.party.0, session.at);
    match verb.as_str() {
        "withdraw" => {
            s.propose(fresh("withdraw"), actor, Command::Countersign(C::Withdraw { id: id.clone() }))
                .await?;
            return ok(json!({ "withdrawn": id }));
        }
        "sign" => {}
        _ => return Err(Problem::not_found()),
    }
    s.propose(fresh("sign"), actor.clone(), Command::Countersign(C::Sign { id: id.clone() }))
        .await?;
    // The command as prepared, now signed, from the replicated record.
    let (_, standing_now) = s.reinsurance_standing(&headers, &method, &uri, b"").await?;
    let pending = standing_now["pending"]
        .as_array()
        .and_then(|all| all.iter().find(|p| p["id"] == id.as_str()))
        .cloned()
        .ok_or_else(Problem::not_found)?;
    let on = business_date(&standing, session.at);
    // The key is the proposal's, so a sign repeated after a lost answer applies once.
    let sent = s
        .to_reinsurance(crate::reinsurance::envelope(
            &format!("countersigned:{id}"),
            &on,
            pending["command"].clone(),
            Some(&session.party),
        ))
        .await;
    match &sent {
        // Unreachable: leave it signed, so signing again finishes it.
        Err(problem) if problem.status == StatusCode::SERVICE_UNAVAILABLE => {}
        // Done, or refused for good: either way it no longer waits.
        _ => {
            s.propose(fresh("settle"), actor, Command::Countersign(C::Settle { id })).await?;
        }
    }
    sent
}

fn claims_down(reason: String) -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "claims_unavailable",
        format!("the claims could not be reached: {reason}"),
    )
}

/// One of the console's questions about claims: a queue across every
/// partition, a claim, the book and report by partition, or a run's
/// remittance on its partition. `me` answers from here.
async fn claims_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    use ic_core::claims_module::report::{Question as Q, Reply as R};
    let session = s.claims_standing(&headers, &method, &uri, b"").await?;
    if name == "me" {
        return ok(json!({ "staff": session.party, "partitions": s.claims.count() }));
    }
    let field = |f: &str| fields.get(f).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let mut question = crate::claims::question(&name, field).map_err(Problem::bad_request)?;
    // A claim this person is conflicted on is never shown to them in a queue or a working view.
    if let Q::Queue { staff, .. } | Q::Work { staff, .. } = &mut question {
        *staff = Some(session.party.to_string());
    }
    match question {
        Q::Claim { claim } => {
            let (partition, row) = s.find_claim(&claim).await?;
            ok(json!({ "partition": partition, "policy": row.policy, "party": row.party, "view": row.view }))
        }
        Q::Queue { .. } => {
            let mut rows: Vec<Value> = Vec::new();
            let mut sorted = Vec::new();
            for (p, reply) in s.claims_everywhere(&question).await? {
                if let R::Queue(found) = reply {
                    sorted.extend(found.into_iter().map(|r| (p, r)));
                }
            }
            // Most urgent first, then oldest, as the claims module orders one partition's.
            sorted.sort_by(|(_, a), (_, b)| b.priority.cmp(&a.priority).then(b.age_days.cmp(&a.age_days)));
            for (p, row) in sorted {
                rows.push(json!({ "partition": p, "claim": row.claim, "priority": row.priority, "age_days": row.age_days, "owner": row.owner }));
            }
            ok(json!({ "rows": rows }))
        }
        Q::Remittance { .. } => {
            let partition: ic_ledger::PartitionId = field("partition")
                .and_then(|p| p.parse().ok())
                .filter(|p| *p < s.claims.count())
                .ok_or_else(|| Problem::bad_request("a run is on one partition: give partition"))?;
            // Null while the partition's business date is not open.
            let reply = s.claims.ask(partition, question).await.map_err(claims_down)?;
            ok(json!(reply)["remittance"].clone())
        }
        Q::Work { .. } => {
            let answers = s.claims_everywhere(&question).await?;
            let found = answers.into_iter().find_map(|(p, reply)| match reply {
                R::Work(Some(view)) => Some((p, view)),
                _ => None,
            });
            let (partition, view) = found.ok_or_else(Problem::not_found)?;
            ok(json!({ "partition": partition, "view": view }))
        }
        Q::Cases { .. } => {
            // Restricted: the claims module answers investigators only, and
            // the person is whoever signed in.
            let question = Q::Cases {
                staff: session.party.to_string(),
            };
            let mut rows: Vec<Value> = Vec::new();
            for (p, reply) in s.claims_everywhere(&question).await? {
                if let R::Cases(found) = reply {
                    rows.extend(found.into_iter().map(|r| json!({ "partition": p, "case": r })));
                }
            }
            ok(json!({ "rows": rows }))
        }
        Q::PolicyStatus { .. } => {
            // A policy is on one partition, except for a moment while it moves.
            let mut rows: Vec<Value> = Vec::new();
            for (p, reply) in s.claims_everywhere(&question).await? {
                if let R::PolicyStatus(found) = reply {
                    rows.extend(found.into_iter().map(|r| json!({ "partition": p, "status": r })));
                }
            }
            ok(json!({ "rows": rows }))
        }
        Q::Book | Q::Report => {
            let partitions: Vec<Value> = s
                .claims_everywhere(&question)
                .await?
                .into_iter()
                .map(|(p, reply)| json!({ "partition": p, "answer": reply }))
                .collect();
            ok(json!({ "partitions": partitions }))
        }
    }
}

/// One person's claims command, as that person: the actor is who signed
/// in, with the purpose they gave, and it goes to the partition holding the
/// claim. The claims module decides what their authority allows.
async fn claims_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(default)]
        purpose: String,
        #[serde(default)]
        partition: Option<ic_ledger::PartitionId>,
        command: Value,
    }
    let session = s.claims_standing(&headers, &method, &uri, &body).await?;
    let key = key(&headers, &session.party.0)?;
    let request: Body = parse(&body)?;
    let (_, target, command) = crate::claims::command(&session.party, &request.purpose, request.partition, request.command).map_err(Problem::bad_request)?;
    let partition = match target {
        crate::claims::Target::Partition(p) if p < s.claims.count() => p,
        crate::claims::Target::Partition(p) => return Err(Problem::bad_request(format!("there is no partition {p}"))),
        crate::claims::Target::Claim(claim) => s.find_claim(&claim).await?.0,
        crate::claims::Target::Policy(policy) => {
            let policy = ic_core::PolicyId(policy);
            let asks = (0..s.claims.count()).map(|p| s.claims.locate(p, policy.clone()));
            let mut found = None;
            for answer in futures::future::join_all(asks).await {
                found = found.or(answer.map_err(claims_down)?);
            }
            found.ok_or_else(|| Problem::new(StatusCode::NOT_FOUND, "unknown_policy", format!("no partition holds the policy {}", policy.0)))?
        }
    };
    let proposed = s.claims.propose(partition, crate::claims::envelope(key, command)).await.map_err(claims_down)?;
    if let Some((code, message)) = proposed.rejected {
        return Err(module_refusal(&code, &message));
    }
    ok(json!({ "accepted": true, "partition": partition, "duplicate": proposed.duplicate, "events": proposed.events }))
}

/// One of the console's questions to the finance ledger: the close
/// dashboard, checks, the trial balance, journals, returns, extracts. `me`
/// answers from here.
async fn finance_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let (_, standing) = s.finance_standing(&headers, &method, &uri, b"").await?;
    if name == "me" {
        return ok(standing);
    }
    let question = crate::finance::question(&name, &fields).map_err(Problem::bad_request)?;
    let answer = s
        .finance
        .ask(question)
        .await
        .map_err(|e| module_problem("finance", StatusCode::BAD_REQUEST, e))?;
    // The ledger answers something it does not hold with a refusal that says so, never with nothing.
    ok(answer)
}

/// One person's command to the finance ledger, as that person with the
/// roles they hold. That module decides what each role may do, keeps maker
/// and checker apart, and records who did what.
async fn finance_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let (session, standing) = s.finance_standing(&headers, &method, &uri, &body).await?;
    if !standing["roles"].as_array().is_some_and(|held| !held.is_empty()) {
        return Err(Problem::forbidden("you may read the finance console but hold no role to act in it"));
    }
    let key = key(&headers, &session.party.0)?;
    let envelope = crate::finance::envelope(&key, &session.party, &standing["roles"], parse(&body)?).map_err(Problem::bad_request)?;
    let output = s
        .finance
        .tell(envelope)
        .await
        .map_err(|e| module_problem("finance", StatusCode::BAD_REQUEST, e))?;
    if let Some(refused) = output["result"].get("Err") {
        let code = refused["code"].as_str().unwrap_or("refused");
        let status = if code == "authority" { StatusCode::FORBIDDEN } else { StatusCode::CONFLICT };
        return Err(Problem::new(
            status,
            code,
            refused["message"].as_str().unwrap_or("the finance ledger refused it"),
        ));
    }
    ok(json!({ "accepted": true, "duplicate": output["duplicate"], "events": output["events"] }))
}

/// One of the console's questions to the unit pricing module: the work
/// queue, funds, prices, valuations, errors. `me` answers from here.
async fn pricing_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let (_, standing) = s.pricing_standing(&headers, &method, &uri, b"").await?;
    if name == "me" {
        return ok(standing);
    }
    let question = crate::pricing::question(&name, &fields).map_err(Problem::bad_request)?;
    let answer = s.pricing.ask(question).await.map_err(pricing_down)?;
    if answer.is_null() {
        return Err(Problem::not_found());
    }
    ok(answer)
}

/// One person's command to the unit pricing module: prepare, approve,
/// reject, suspend, resume, raise or close a price error, republish. It goes
/// as that person with the roles they hold; that module checks them again
/// and keeps who did what.
async fn pricing_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let (session, standing) = s.pricing_standing(&headers, &method, &uri, &body).await?;
    let command: Value = parse(&body)?;
    let Some((name, role)) = crate::pricing::role_for(&command) else {
        return Err(Problem::bad_request("that is not a command the unit pricing console sends"));
    };
    if !standing["roles"].as_array().is_some_and(|held| held.iter().any(|r| r == role)) {
        return Err(Problem::forbidden(format!("{name} needs the role {role}")));
    }
    let key = key(&headers, &session.party.0)?;
    let envelope = crate::pricing::envelope(&key, &session.party, &standing["roles"], session.at, command);
    let output = s.pricing.tell(envelope).await.map_err(pricing_down)?;
    if let Some(refused) = output.get("refused") {
        return Err(module_refusal(
            refused["code"].as_str().unwrap_or("refused"),
            refused["message"].as_str().unwrap_or("unit pricing refused it"),
        ));
    }
    ok(json!({ "accepted": true, "output": output }))
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
        route_to_person: false,
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
        .route("/app/v1/profile", get(profile))
        .route("/app/v1/profile/commands", post(profile_command))
        .route("/app/v1/profile/export", get(profile_export))
        .route("/app/v1/documents/{id}/pdf", get(own_document))
        .route("/staff/v1/documents/{id}/pdf", get(staff_document))
        .route("/app/v1/proposals/{proposal}/lives/{life}/disclosures/to-sign", post(disclosure_to_sign))
        .route("/app/v1/proposals/{proposal}/lives/{life}/disclosures", post(disclosure_submit))
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
        .route("/adviser/v1/replacements", get(adviser_replacements))
        .route("/app/v1/policies/{policy}/claims", get(policy_claims))
        .route("/adviser/v1/replacements/dispute", post(adviser_dispute))
        .route("/employer/v1/schemes", get(schemes))
        .route("/employer/v1/schemes/{policy}/admins/{party}", put(scheme_admin))
        .route("/employer/v1/schemes/{policy}/second-admin", put(scheme_second_admin))
        .route("/employer/v1/schemes/{policy}/group/commands", post(group_command))
        .route(
            "/employer/v1/schemes/{policy}/group/files/{file}",
            post(group_upload).layer(axum::extract::DefaultBodyLimit::max(crate::group::MAX_UPLOAD_BYTES)),
        )
        .route("/employer/v1/schemes/{policy}/group/{question}", get(group_question))
        .route("/staff/v1/me", get(staff_me))
        .route("/staff/v1/admin", get(staff_admin))
        .route("/staff/v1/access", post(staff_command))
        .route("/staff/v1/policies/{policy}", get(staff_policy))
        .route("/staff/v1/requests", post(staff_request))
        .route("/staff/v1/workbench", get(workbench))
        .route("/staff/v1/workbench/actions", post(workbench_action))
        .route("/staff/v1/workbench/{party}", get(workbench_party))
        .route("/staff/v1/workbench/{party}/proof-of-debt", get(proof_of_debt))
        .route("/staff/v1/complaints/commands", post(complaints_command))
        .route("/staff/v1/complaints/{question}", get(complaints_question))
        .route("/staff/v1/claims/commands", post(claims_command))
        .route("/staff/v1/replacements", get(replacements))
        .route("/staff/v1/replacements/decide", post(replacement_decide))
        .route("/staff/v1/reinsurance/commands", post(reinsurance_command))
        .route("/staff/v1/reinsurance/{verb}/{id}", post(reinsurance_decide))
        .route("/staff/v1/reinsurance/{question}", get(reinsurance_question))
        .route("/staff/v1/claims/{question}", get(claims_question))
        .route("/staff/v1/finance/commands", post(finance_command))
        .route("/staff/v1/finance/{question}", get(finance_question))
        .route("/staff/v1/unit-pricing/commands", post(pricing_command))
        .route("/staff/v1/unit-pricing/{question}", get(pricing_question))
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
        .route("/complaints", get(|State(s): State<Shared>| async move { page(&s, crate::ui::COMPLAINTS) }))
        .route("/finance", get(|State(s): State<Shared>| async move { page(&s, crate::ui::FINANCE) }))
        .route("/claims", get(|State(s): State<Shared>| async move { page(&s, crate::ui::CLAIMS) }))
        .route("/reinsurance", get(|State(s): State<Shared>| async move { page(&s, crate::ui::REINSURANCE) }))
        .route("/pricing", get(|State(s): State<Shared>| async move { page(&s, crate::ui::PRICING) }))
        .route("/replacements", get(|State(s): State<Shared>| async move { page(&s, crate::ui::REPLACEMENTS) }))
        .route("/ui/app.js", get(|| async { asset("text/javascript; charset=utf-8", crate::ui::SCRIPT) }))
        .route("/ui/app.css", get(|| async { asset("text/css; charset=utf-8", crate::ui::STYLE) }))
        .route("/ui/config.json", get(ui_config))
        .with_state(state)
}
