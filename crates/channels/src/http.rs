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

use crate::classes::DataClass;
use crate::directory::{Actor, DelegateAccess, DirectoryCommand};
use crate::identity::{action_hash, Enrolment, IdentityCommand, MAX_CHALLENGE_SECS};
use crate::ops::{RegionSource, StatusPage};
use crate::query::{self, DeviceProof, Question, MODULE};
use crate::staff::actions as staff_actions;
use crate::staff::{AccessRequest, At, Decision, Purpose, PurposeKind, StaffCommand};
use crate::state::{ChannelState, Command, Effect, Envelope, Output, GATEWAY};
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
    /// Where this service is reached from outside, such as
    /// `https://portal.example.co.nz` (`IC_PUBLIC_URL`). It names the MCP
    /// endpoint in the OAuth discovery document and is the one browser
    /// origin the MCP endpoint accepts. None on a laptop.
    pub public_url: Option<String>,
    /// The unit pricing module, for the staff console.
    pub pricing: Arc<dyn crate::forward::Source>,
    /// The finance ledger, for the finance console.
    pub finance: Arc<dyn crate::forward::Source>,
    /// The core's partitions, where claims live, for the claims console.
    pub claims: Arc<dyn crate::claims::Partitions>,
    /// The reinsurance module, for the exceptions console.
    pub reinsurance: Arc<dyn crate::forward::Source>,
    /// The payments module, for the payments console.
    pub payments: Arc<dyn crate::forward::Source>,
    /// The underwriting module, for the underwriting console.
    pub underwriting: Arc<dyn crate::forward::Source>,
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
    /// The wording of each definition a cover can be bound to, for assessors.
    pub wordings: Arc<crate::wordings::Wordings>,
    /// The return definitions the finance console may file on.
    pub returns: Arc<crate::finance::Returns>,
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

/// When the caller's token was issued, for checking it against a
/// revocation. Under Entra this is the verified token's `iat`; a token that
/// does not say is treated as the oldest possible, so any revocation
/// refuses it. Nothing the caller sends beside the token can change it.
///
/// Under development identity there is no verified token: `x-dev-issued`
/// stands in for it, else the `iat` of any bearer token sent, else now.
fn issued(auth: &Auth, caller: &Caller, headers: &HeaderMap, now: At) -> At {
    if !matches!(auth, Auth::Dev) {
        return caller.issued.unwrap_or(0);
    }
    if let Some(dev) = header(headers, "x-dev-issued") {
        return dev.parse().unwrap_or(0);
    }
    let Some(token) = header(headers, "authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string)) else {
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
        let own = actor.party().cloned();
        let envelope = Envelope {
            key,
            at: self.now(),
            actor,
            command,
        };
        let output = self.channel.propose(envelope).await?;
        let effects = output.result?;
        // What the caller is told: never another party's feed, sessions or
        // control, which would say who else is on the policy. Their own
        // feed's version is the one to sync to for reading their own write.
        let mine = |party: &PartyId| own.as_ref() == Some(party);
        let version = effects.iter().find_map(|e| match e {
            Effect::FeedMoved { party, version } if mine(party) => Some(*version),
            _ => None,
        });
        let effects: Vec<Effect> = effects
            .into_iter()
            .filter(|e| match e {
                Effect::FeedMoved { party, .. } | Effect::Revoked { party } => mine(party),
                Effect::ControlPassed { .. } => false,
                _ => true,
            })
            .collect();
        Ok(json!({ "accepted": true, "duplicate": output.duplicate, "effects": effects, "version": version }))
    }

    /// Resolve the caller to a party and check the session still stands.
    pub(crate) async fn session(&self, auth: &Auth, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Caller, Session), Problem> {
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
                issued: issued(auth, &caller, headers, at),
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

    /// Tell underwriting that a life on an open proposal is to be given more
    /// time, where the customer master's routing for them says so. Asked
    /// each time the life comes to answer or sign, so a marker placed after
    /// the proposal opened is picked up; underwriting extends the dates
    /// once. Never why, and nothing here stops the person: a master or a
    /// module that cannot answer leaves the dates as they are.
    async fn more_time(&self, session: &Session, proposal: &str, life: &str) {
        let Some(master) = &self.master else { return };
        let wanted = matches!(master.routing(&session.party).await, Ok((200, routing))
            if routing["adjustments"].as_array().is_some_and(|a| a.iter().any(|x| x == "more_time")));
        if !wanted {
            return;
        }
        let command = json!({ "set_more_time": { "proposal": proposal, "life": life, "more_time": true } });
        let today = crate::payments::nz(session.at).date().to_string();
        let envelope = crate::underwriting::envelope(&format!("more-time:{proposal}:{life}"), session.at, &today, command);
        if let Err(problem) = self.to_underwriting(envelope).await {
            tracing::warn!(proposal, life, problem = %problem.detail, "underwriting did not take the more-time marker");
        }
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
    s.more_time(&session, &proposal, &life).await;
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

/// What the life signs to give a consent on a proposal: the exact bytes,
/// base64. Only the life on the proposal may ask.
async fn consent_to_sign(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((proposal, life, kind)): Path<(String, String, String)>,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    s.signer(&session, &proposal, &life).await?;
    if !crate::disclosures::CONSENTS.contains(&kind.as_str()) {
        return Err(Problem::not_found());
    }
    s.more_time(&session, &proposal, &life).await;
    let message = base64::engine::general_purpose::STANDARD.encode(crate::disclosures::consent_message(&proposal, &life, &kind));
    ok(json!({ "message": message, "signed": crate::disclosures::SIGNED_CONSENTS.contains(&kind.as_str()) }))
}

/// What the life reads before signing the replacement disclosure: the cover
/// they keep, the cover added, what is lost, and the premium before and
/// after. Only the life may read it, and only for a proposal that replaces.
async fn replacement_statement(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((proposal, life)): Path<(String, String)>,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let statement = s
        .ask(Question::ReplacementStatement {
            party: session.party,
            proposal,
            life,
        })
        .await?;
    match statement["replacement"].as_bool() {
        Some(true) => ok(statement),
        Some(false) => Err(Problem::new(
            StatusCode::CONFLICT,
            "replacement",
            "this proposal does not replace cover you hold, so there is nothing to disclose",
        )),
        None => Err(Problem::not_found()),
    }
}

/// The life gives or withdraws a consent on their proposal, from a device
/// they enrolled; one that needs their signed step carries the device's
/// signature. The underwriting module records it, and checks the signature
/// again.
async fn consent_record(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path((proposal, life, kind)): Path<(String, String, String)>,
    body: Bytes,
) -> Answer {
    let session = s.customer(&headers, &method, &uri, &body).await?;
    let Some(device) = session.device.clone() else {
        return Err(device_required("giving or withdrawing a consent"));
    };
    let idempotency = key(&headers, &session.party.0)?;
    let public_key = s
        .signer(&session, &proposal, &life)
        .await?
        .ok_or_else(|| device_required("giving or withdrawing a consent"))?;
    let command = crate::disclosures::consent(&proposal, &life, &kind, &parse(&body)?, Some((&device, &public_key)))
        .map_err(|e| Problem::new(StatusCode::BAD_REQUEST, "bad_consent", e))?;
    s.to_underwriting(crate::underwriting::app_envelope(&format!("app:{idempotency}"), session.at, command))
        .await
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

/// Where your own hold on money going out stands: whether it is on, and
/// when a removal you asked for takes effect.
async fn profile_spending_hold(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    mastered(s.master()?.spending_hold(&session.party).await)
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

impl AppState {
    /// The authority the person signed in holds to view `party`'s affairs,
    /// as the customer master has it today. With none in force the answer
    /// is the same as for a person who does not exist. Where the master
    /// says a person must decide (capacity is in question) the
    /// representative is told their authority is under review, which they
    /// already know they hold, and sees nothing.
    async fn authority_over(&self, session: &Session, party: &PartyId) -> Result<Value, Problem> {
        let Some(master) = &self.master else { return Err(Problem::not_found()) };
        match master.represents(&session.party, party).await {
            Ok((200, answer)) => match answer["decision"].as_str() {
                Some("allow") => Ok(answer["relationship"].clone()),
                Some("route") => Err(Problem::new(
                    StatusCode::FORBIDDEN,
                    "authority_under_review",
                    "your authority to act is being reviewed; we will be in touch",
                )),
                _ => Err(Problem::not_found()),
            },
            Ok((400 | 404, _)) => Err(Problem::not_found()),
            _ => Err(Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "customer_master_unavailable",
                "we could not check your authority just now; try again shortly",
            )),
        }
    }
}

/// The policies of someone the person signed in acts for under an
/// authority in force: an attorney under an activated enduring power, a
/// guardian, a written authority that covers viewing.
async fn represented(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(party): Path<String>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let party = PartyId::from(party.as_str());
    let authority = s.authority_over(&session, &party).await?;
    let policies = s
        .ask(Question::Policies {
            party: party.clone(),
            at: session.at,
        })
        .await?;
    ok(json!({ "party": party, "authority": authority, "policies": policies }))
}

/// One of those policies, as the person themselves sees it but without
/// their health information: viewing a policy is not authority to read
/// what was disclosed about their health.
async fn represented_policy(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path((party, policy)): Path<(String, String)>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let party = PartyId::from(party.as_str());
    s.authority_over(&session, &party).await?;
    let view = s
        .ask(Question::PolicyFor {
            represented: party,
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
                assisted: None,
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
    let routing = match &s.master {
        Some(master) => Some(master.routing(&session.party).await),
        None => None,
    };
    let route_to_person = crate::customers::route_to_person(routing);
    accepted(
        s.propose(
            key(&headers, &session.party.0)?,
            Actor::Party(session.party),
            Command::ApproveRequest { id, signed, route_to_person },
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
    let book = s
        .ask(Question::Book {
            adviser: session.party.clone(),
            at: session.at,
        })
        .await?;
    // The portal is for advisers and for those with advisers under them. Anyone else is told so, not shown an empty book.
    if book["engagements"].as_array().is_none_or(|e| e.is_empty()) {
        let practice = s
            .ask(Question::Practice {
                party: session.party,
                at: session.at,
            })
            .await?;
        if practice["advisers"].as_array().is_none_or(|a| a.is_empty()) {
            return Err(Problem::forbidden(
                "the adviser portal is for advisers and advice providers, and you are not engaged as either",
            ));
        }
    }
    ok(book)
}

/// What an advice provider or a group sees of the advisers under it and
/// the policies they service. Empty for anyone who is neither.
async fn practice(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    ok(s.ask(Question::Practice {
        party: session.party,
        at: session.at,
    })
    .await?)
}

/// The signed-in party's own commission statement, by number: an adviser
/// sees their own lines, a provider or group the lines paid to it, with
/// totals by component (its overrides among them). Anyone else's
/// statement cannot be named, and one not yet cut is 404.
async fn own_statement(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(number): Path<u32>) -> Answer {
    let session = s.customer(&headers, &method, &uri, b"").await?;
    let question = json!({ "statement": { "payee": session.party, "number": number } });
    let answer = s
        .commission
        .ask(question)
        .await
        .map_err(|e| module_problem("commission", StatusCode::BAD_REQUEST, e))?;
    if answer.is_null() {
        return Err(Problem::not_found());
    }
    ok(crate::commission::own_statement(&answer))
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
            assisted: None,
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

// --- the group console, for group administration staff ---

impl AppState {
    async fn group_standing(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Session, Value), Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let standing = self
            .ask(Question::GroupStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if standing["view"] != true {
            return Err(Problem::forbidden("your roles do not include group schemes"));
        }
        Ok((session, standing))
    }

    /// The standing of someone who may act in the group console.
    async fn group_admin(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<Session, Problem> {
        let (session, standing) = self.group_standing(headers, method, uri, body).await?;
        if standing["admin"] != true {
            return Err(Problem::forbidden("you may read group schemes but not act on them"));
        }
        Ok(session)
    }
}

/// The group service's answer to staff. A refusal comes back as a problem
/// with the service's code and words, as every console's refusals do: the
/// module's own refusal as 409, and a refusal of the portal itself (its
/// token does not carry `group-admin`) as the portal's fault, 502.
fn group_staff_answer(answer: Result<crate::group::Answered, String>) -> Answer {
    let (status, body) = answer.map_err(group_down)?;
    let Some(error) = body.get("error").filter(|_| status >= 400) else {
        return grouped(Ok((status, body)));
    };
    let code = error["code"].as_str().unwrap_or("refused");
    let message = error["message"].as_str().unwrap_or("the group service refused it");
    let status = match (status, code) {
        (_, "role_not_held" | "caller_not_allowed" | "unauthorised") => StatusCode::BAD_GATEWAY,
        (422, _) => StatusCode::CONFLICT,
        (other, _) => StatusCode::from_u16(other).unwrap_or(StatusCode::BAD_GATEWAY),
    };
    Err(Problem::new(status, code, message))
}

/// A question to the group service, as staff: everything about a scheme
/// the service answers, the open exceptions, and the scheme on a policy.
async fn group_staff_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Answer {
    let (_, standing) = s.group_standing(&headers, &method, &uri, b"").await?;
    if name == "me" {
        return ok(standing);
    }
    let question = crate::group::staff_question(&name, |f| q.get(f).cloned()).map_err(Problem::bad_request)?;
    group_staff_answer(s.group()?.ask(question).await)
}

/// A command to the group service as the member of staff signed in, under
/// the caller `group-admin`. The service decides, and keeps uploader and
/// approver apart where the scheme asks for it.
async fn group_staff_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.group_admin(&headers, &method, &uri, &body).await?;
    let key = key(&headers, &session.party.0)?;
    let sent: Value = parse(&body)?;
    crate::group::staff_command(&sent).map_err(Problem::bad_request)?;
    group_staff_answer(s.group()?.staff_command(&key, &session.party.0, sent).await)
}

#[derive(Deserialize)]
struct FirstAdmin {
    party: PartyId,
}

/// Name a scheme's first administrator. Group administration sets up the
/// first; from then on the employer's own administrators manage the list,
/// so this is refused once the scheme has one.
async fn group_first_admin(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>, body: Bytes) -> Answer {
    let session = s.group_admin(&headers, &method, &uri, &body).await?;
    let first: FirstAdmin = parse(&body)?;
    let policy = PolicyId::from(policy.as_str());
    if s.ask(Question::SchemeAdmins { policy: policy.clone() }).await? != json!(0) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "has_administrators",
            "this scheme already has an administrator, and its administrators manage their own list from the employer portal",
        ));
    }
    tracing::info!(staff = %session.party.0, scheme = %policy, "first scheme administrator named by group administration");
    let command = DirectoryCommand::SetEmployerAdmin {
        policy,
        party: first.party,
        admin: true,
    };
    ok(s.propose(
        key(&headers, &session.party.0)?,
        Actor::Module("group-admin".to_string()),
        Command::Directory(command),
    )
    .await?)
}

/// Try a mapping against a sample of the employer's file. Nothing is
/// stored, so reading the console is enough.
async fn group_try_mapping(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(scheme): Path<String>, body: Bytes) -> Answer {
    s.group_standing(&headers, &method, &uri, &body).await?;
    let scheme = crate::group::name(&scheme).ok_or_else(|| Problem::bad_request("a scheme is letters, digits, dots, dashes and underscores"))?;
    let trial: Value = parse(&body)?;
    if !(trial.get("mapping").is_some_and(Value::is_object) && trial.get("sample").is_some_and(Value::is_string)) {
        return Err(Problem::bad_request("a trial is a mapping and a sample of the file"));
    }
    group_staff_answer(s.group()?.try_mapping(scheme, trial).await)
}

// --- the staff console ---

impl AppState {
    /// A member of staff trained to see and review vulnerability markers.
    async fn care(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<Session, Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let trained = self
            .ask(Question::CareStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if trained != true {
            return Err(Problem::forbidden("reviewing a marker is for people trained in vulnerability support"));
        }
        Ok(session)
    }
}

/// The master's answer to a trained person, or why there is none.
fn cared(answer: Result<crate::customers::Answered, String>) -> Result<Value, Problem> {
    match answer {
        Ok((200, body)) => Ok(body),
        Ok((status, body)) => Err(Problem::new(
            StatusCode::from_u16(status)
                .ok()
                .filter(|s| s.is_client_error() && *s != StatusCode::FORBIDDEN)
                .unwrap_or(StatusCode::BAD_GATEWAY),
            body["title"].as_str().unwrap_or("customer_master"),
            body["detail"].as_str().unwrap_or("the customer master did not take it"),
        )),
        Err(e) => Err(Problem::new(StatusCode::SERVICE_UNAVAILABLE, "customer_master_unavailable", e)),
    }
}

/// The markers waiting for a trained person: one that has reached its
/// review date, or a prompt to consider one after a death or a concern
/// someone raised. Each comes with how the person is served now.
async fn care_reviews(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri) -> Answer {
    use crate::customers::Care;
    let session = s.care(&headers, &method, &uri, b"").await?;
    let master = s.master()?;
    let open = cared(master.care(&session.party, Care::Reviews).await)?;
    let mut rows = Vec::new();
    for review in open["reviews"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["kind"] == "consider_marker")
        .take(100)
    {
        let party = PartyId::from(review["party"].as_str().unwrap_or_default());
        // A person whose routing cannot be read is still listed: the review is the work.
        let now = cared(master.care(&session.party, Care::Routing(party.clone())).await).unwrap_or(Value::Null);
        let marker = if now["route_to_person"] == true || now["kind"].is_string() {
            json!({ "kind": now["kind"], "review_on": now["review_on"], "adjustments": now["adjustments"] })
        } else {
            Value::Null
        };
        rows.push(json!({ "review": review["id"], "party": party, "reason": review["reason"], "opened": review["created_at"], "marker": marker }));
    }
    ok(json!({ "rows": rows, "kinds": crate::customers::MARKER_KINDS, "adjustments": crate::customers::ADJUSTMENTS }))
}

/// What the trained person decided: renew the marker to a later date,
/// change it, remove it, or close a prompt with no marker needed. The
/// marker is changed first and the prompt closed after, so a failure
/// leaves the prompt open.
async fn care_decide(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(review): Path<String>, body: Bytes) -> Answer {
    use crate::customers::Care;
    let session = s.care(&headers, &method, &uri, &body).await?;
    let key = key(&headers, &session.party.0)?;
    let decision: Value = parse(&body)?;
    let party = decision["party"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(PartyId::from)
        .ok_or_else(|| Problem::bad_request("say whose marker it is"))?;
    let command = crate::customers::marker_command(&review, &decision).map_err(Problem::bad_request)?;
    let master = s.master()?;
    if let Some(body) = command {
        cared(master.care(&session.party, Care::Marker { party, key, body }).await)?;
    }
    cared(master.care(&session.party, Care::Dismiss(review.clone())).await)?;
    ok(json!({ "review": review, "outcome": decision["outcome"] }))
}

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
        "unknown_staff" | "authority" | "not_authorised" | "unauthorised" | "not_owner" | "no_access" | "not_permitted" | "role_conflict" | "same_person" => {
            StatusCode::FORBIDDEN
        }
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
        return Err(Problem::forbidden("your roles do not include replacement matches"));
    }
    Ok(standing)
}

async fn commission_standing(s: &AppState, session: &Session, purpose: &str) -> Result<Value, Problem> {
    let standing = s
        .ask(Question::CommissionStaff {
            staff: session.party.clone(),
            at: session.at,
            purpose: purpose.to_string(),
        })
        .await?;
    if standing["view"] != true {
        return Err(Problem::forbidden("your roles do not include the commission console"));
    }
    Ok(standing)
}

/// One of the commission console's questions. `me` answers from here:
/// whether the person may open it and the roles they hold there.
async fn commission_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let standing = commission_standing(&s, &session, "").await?;
    if name == "me" {
        return ok(json!({ "staff": session.party, "roles": standing["roles"] }));
    }
    let question = crate::commission::question(&name, &fields).map_err(Problem::bad_request)?;
    let answer = s
        .commission
        .ask(question)
        .await
        .map_err(|e| module_problem("commission", StatusCode::NOT_FOUND, e))?;
    if answer.is_null() {
        return Err(Problem::not_found());
    }
    ok(answer)
}

/// One person's command to the commission module, sent as them with the
/// roles they hold and the purpose they gave. That module decides it with
/// its own policy and records the decision.
async fn commission_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let request: Value = parse(&body)?;
    let purpose = request["purpose"].as_str().map(str::trim).unwrap_or_default();
    let standing = commission_standing(&s, &session, purpose).await?;
    if purpose.is_empty() {
        return Err(Problem::bad_request("say why: the purpose is recorded with the decision"));
    }
    let command = request["command"].clone();
    if crate::commission::console_command(&command).is_none() {
        return Err(Problem::bad_request(
            "that is not a command the console sends: workbench actions, replacement decisions and policy assignment have their own paths",
        ));
    }
    let key = key(&headers, &session.party.0)?;
    let output = s
        .commission
        .tell(crate::commission::envelope(&key, standing["actor"].clone(), command))
        .await
        .map_err(|e| module_problem("commission", StatusCode::BAD_REQUEST, e))?;
    if let Some(refused) = output["result"].get("Err") {
        return Err(module_refusal(
            refused["code"].as_str().unwrap_or("refused"),
            refused["message"].as_str().unwrap_or("the commission module refused it"),
        ));
    }
    ok(json!({ "accepted": true, "duplicate": output["duplicate"], "events": output["result"]["Ok"] }))
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

impl AppState {
    /// Whether this person may open the underwriting console. Nobody
    /// without an underwriting action gets as far as that module.
    async fn underwriting_standing(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Session, Value), Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let standing = self
            .ask(Question::UnderwritingStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if standing["view"] != true {
            return Err(Problem::forbidden("your roles do not include underwriting"));
        }
        Ok((session, standing))
    }

    /// Send one command to the underwriting module and say what became of
    /// it: what it recorded, or why it refused.
    async fn to_underwriting(&self, envelope: Value) -> Answer {
        let output = self
            .underwriting
            .tell(envelope)
            .await
            .map_err(|e| module_problem("underwriting", StatusCode::BAD_REQUEST, e))?;
        if let Some(refused) = output.get("Err") {
            return Err(module_refusal(
                refused["code"].as_str().unwrap_or("refused"),
                refused["message"].as_str().unwrap_or("the underwriting module refused it"),
            ));
        }
        let applied = &output["Ok"];
        ok(json!({ "accepted": true, "events": applied["events"], "view": applied["view"], "duplicate": applied["duplicate"] }))
    }
}

impl AppState {
    /// Whether this person may open the payments console, and their roles
    /// there. Nobody without a payments action gets as far as that module.
    async fn payments_standing(&self, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8]) -> Result<(Session, Value), Problem> {
        let session = self.staff(headers, method, uri, body).await?;
        let standing = self
            .ask(Question::PaymentsStaff {
                staff: session.party.clone(),
                at: session.at,
            })
            .await?;
        if standing["view"] != true {
            return Err(Problem::forbidden("your roles do not include payments"));
        }
        Ok((session, standing))
    }
}

/// The roles a person holds in the payments module, from their standing.
fn payment_roles(standing: &Value) -> Vec<String> {
    standing["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// One of the console's questions to the payments module. `me` answers
/// from here.
async fn payments_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let (session, standing) = s.payments_standing(&headers, &method, &uri, b"").await?;
    if name == "me" {
        return ok(standing);
    }
    let question = crate::payments::question(&name, &fields, &payment_roles(&standing), session.at).map_err(Problem::bad_request)?;
    let answer = s
        .payments
        .ask(question)
        .await
        .map_err(|e| module_problem("payments", StatusCode::BAD_REQUEST, e))?;
    if answer.is_null() {
        return Err(Problem::not_found());
    }
    ok(answer)
}

/// A person's command to the payments module, as the person signed in
/// with the roles their staff actions give them there. That module
/// decides, and keeps makers, approvers and payee maintainers apart.
async fn payments_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let (session, standing) = s.payments_standing(&headers, &method, &uri, &body).await?;
    let roles = payment_roles(&standing);
    if roles.is_empty() {
        return Err(Problem::forbidden("you may read payments but hold no role to act on them"));
    }
    let key = key(&headers, &session.party.0)?;
    let command: Value = parse(&body)?;
    if crate::payments::console_command(&command).is_none() {
        return Err(Problem::bad_request("that is not a command people send to payments"));
    }
    let envelope = crate::payments::envelope(&key, &session.party, &roles, session.at, command);
    let output = s
        .payments
        .tell(envelope)
        .await
        .map_err(|e| module_problem("payments", StatusCode::BAD_REQUEST, e))?;
    if let Some(refused) = output.get("Err") {
        return Err(module_refusal(
            refused["code"].as_str().unwrap_or("refused"),
            refused["message"].as_str().unwrap_or("the payments module refused it"),
        ));
    }
    ok(json!({ "accepted": true, "events": output["Ok"] }))
}

/// One of the console's questions to the underwriting module. `me` answers
/// from here, with the remedies waiting for a second senior.
async fn underwriting_question(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(name): Path<String>,
    Query(fields): Query<std::collections::BTreeMap<String, String>>,
) -> Answer {
    let (session, standing) = s.underwriting_standing(&headers, &method, &uri, b"").await?;
    if name == "me" {
        return ok(standing);
    }
    let question = crate::underwriting::question(&name, &fields, &session.party, session.at).map_err(Problem::bad_request)?;
    let answer = s
        .underwriting
        .ask(question)
        .await
        .map_err(|e| module_problem("underwriting", StatusCode::BAD_REQUEST, e))?;
    if answer.is_null() {
        return Err(Problem::not_found());
    }
    ok(answer)
}

/// A person's command on a case. The person acting is whoever signed in;
/// a non-disclosure remedy is kept until a second senior signs it.
async fn underwriting_command(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let (session, standing) = s.underwriting_standing(&headers, &method, &uri, &body).await?;
    let key = key(&headers, &session.party.0)?;
    let sent: Value = parse(&body)?;
    let senior = standing["senior"] == true;
    let (command, name, countersigned) = crate::underwriting::command(&sent, &session.party, senior).map_err(|e| {
        if e.ends_with("for a senior underwriter") {
            Problem::forbidden(&e)
        } else {
            Problem::bad_request(e)
        }
    })?;
    if countersigned {
        let reason = command[&name]["reasons"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let id = format!("underwriting:{key}");
        let prepare = crate::countersign::CountersignCommand::Prepare {
            id: id.clone(),
            module: crate::underwriting::MODULE.to_string(),
            command,
            reason,
        };
        s.propose(format!("countersign:{id}"), Actor::Staff(session.party), Command::Countersign(prepare))
            .await?;
        return ok(json!({ "prepared": id, "needs": "a second senior underwriter to sign it" }));
    }
    let today = business_date(&standing, session.at);
    s.to_underwriting(crate::underwriting::envelope(&key, session.at, &today, command)).await
}

/// Sign, or take back, a remedy waiting for a second senior.
async fn underwriting_decide(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path((verb, id)): Path<(String, String)>) -> Answer {
    use crate::countersign::CountersignCommand as C;
    let (session, standing) = s.underwriting_standing(&headers, &method, &uri, b"").await?;
    let waiting = |standing: &Value| {
        standing["pending"]
            .as_array()
            .and_then(|all| all.iter().find(|p| p["id"] == id.as_str()))
            .cloned()
    };
    // Only this module's remedies are signed or taken back from here.
    if waiting(&standing).is_none() {
        return Err(Problem::not_found());
    }
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
    let (_, now) = s.underwriting_standing(&headers, &method, &uri, b"").await?;
    let pending = waiting(&now).ok_or_else(Problem::not_found)?;
    let signed = crate::underwriting::countersigned(&pending["command"], &session.party);
    let today = business_date(&standing, session.at);
    // The key is the prepared one's, so a sign repeated after a lost answer applies once.
    let sent = s
        .to_underwriting(crate::underwriting::envelope(&format!("countersigned:{id}"), session.at, &today, signed))
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

/// An underwriter reads a life's disclosures through the intake, which has
/// the underwriting module allow it and record the reading first. The
/// person reading is whoever signed in, and a purpose is required.
async fn underwriting_disclosures(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let (session, _) = s.underwriting_standing(&headers, &method, &uri, &body).await?;
    let key = key(&headers, &session.party.0)?;
    let sent: Value = parse(&body)?;
    let field = |name: &str| {
        sent[name]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .ok_or_else(|| Problem::bad_request(format!("send the {name}")))
    };
    let request = json!({
        "request": key,
        "proposal": field("proposal")?,
        "life": field("life")?,
        "underwriter": session.party,
        "purpose": field("purpose")?,
    });
    intake_answer(s.intake()?.open(request).await)
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
/// The wording of one definition at one version: what a cover bound to it
/// promised, whatever the definition says today.
async fn claims_definition(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path((id, version)): Path<(String, String)>) -> Answer {
    s.claims_standing(&headers, &method, &uri, b"").await?;
    let found = version.parse::<u32>().ok().and_then(|v| s.wordings.get(&id, v));
    ok(found.ok_or_else(Problem::not_found)?)
}

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
    if name == "definitions" {
        return ok(json!({ "rows": s.wordings.list() }));
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
        // The documents service's question: `claims::question` never makes it.
        Q::RepaymentLetters => Err(Problem::not_found()),
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
    if name == "return_definitions" {
        return ok(json!({ "rows": s.returns.list() }));
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

/// Price a proposed rate table against the book in force on a date,
/// beside the current one. For the finance lead and the appointed
/// actuary. The loadings are read from every core partition for that date
/// and handed to the ledger for this one answer; they are not kept here or
/// there, and the answer carries totals only. If the core cannot be read
/// the run still answers, without loaded totals, and says so.
async fn finance_dry_run(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, body: Bytes) -> Answer {
    let (_, standing) = s.finance_standing(&headers, &method, &uri, &body).await?;
    let held = |role: &&str| standing["roles"].as_array().is_some_and(|r| r.iter().any(|h| h == role));
    if !crate::finance::DRY_RUN_ROLES.iter().any(held) {
        return Err(Problem::forbidden(
            "pricing a proposed table against the book is for the finance lead and the appointed actuary",
        ));
    }
    let body: Value = parse(&body)?;
    // Checked before the core is read, so a mistake in the form costs nothing.
    crate::finance::dry_run(&body, vec![]).map_err(Problem::bad_request)?;
    let as_at = body["as_at"]
        .as_str()
        .and_then(|d| d.parse::<chrono::NaiveDate>().ok())
        .ok_or_else(|| Problem::bad_request("give the date"))?;
    let mut loadings = Vec::new();
    let mut unread = None;
    'book: for partition in 0..s.claims.count() {
        let mut after = None;
        loop {
            match s.claims.loadings(partition, after, as_at).await {
                Ok(page) => {
                    loadings.extend(
                        page.rows
                            .into_iter()
                            .map(|r| json!({ "partition": partition, "policy": r.policy, "cover": r.cover, "layer": r.layer, "loadings": r.loadings })),
                    );
                    match page.next {
                        Some(next) => after = Some(next),
                        None => break,
                    }
                }
                Err(e) => {
                    unread = Some(e);
                    break 'book;
                }
            }
        }
    }
    if unread.is_some() {
        loadings.clear();
    }
    let question = crate::finance::dry_run(&body, loadings).map_err(Problem::bad_request)?;
    let mut answer = s
        .finance
        .ask(question)
        .await
        .map_err(|e| module_problem("finance", StatusCode::BAD_REQUEST, e))?;
    answer["loadings_read"] = json!(unread.is_none());
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
    let envelope = crate::finance::envelope(&key, &session.party, &standing["roles"], &s.returns, parse(&body)?).map_err(Problem::bad_request)?;
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
        // Two decisions by one person on one record can fall in the same second: an allowed
        // lookup, then a refused one under another scope. Each keeps its own line.
        let key = format!(
            "access:{}:{}:{}:{}:{}:{}",
            request.staff,
            request.at,
            request.action,
            request.record.as_deref().unwrap_or_default(),
            if decision.allowed { "allowed" } else { "refused" },
            request.scopes.join(",")
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
    let mut view = s.logged(view).await?;
    if view["found"] != true {
        return Err(Problem::not_found());
    }
    let may_see_kind = view.as_object_mut().and_then(|v| v.remove("may_see_kind")) == Some(Value::Bool(true));
    // How to serve the owners: for joint owners, the first who needs care.
    let holders: Vec<PartyId> = view["policyholders"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(PartyId::from)
        .collect();
    if let Some(master) = &s.master {
        for holder in holders {
            if let Some(approach) = approach(master.routing(&holder).await, may_see_kind) {
                view["approach"] = approach;
                break;
            }
        }
    }
    ok(view)
}

#[derive(Deserialize)]
struct CallerCheck {
    /// The caller's customer reference, where they are a customer and
    /// have been identified. Absent for anyone else.
    #[serde(default)]
    caller: Option<String>,
    /// The case or task the call is handled under.
    #[serde(default)]
    case: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// What every refused caller is told, whoever they are and whatever is held.
const CALLER_REFUSED: &str = "I'm sorry, I can't confirm whether we hold a policy for anyone, or talk about one, with someone who isn't named on it. If you are helping someone with their insurance, they can call us themselves or give us a written authority for you.";

/// Someone has phoned about a policy: may the member of staff talk to them
/// about it? The member of staff must be able to open the policy, and that
/// is logged as any read is. The caller is spoken to if they own it or
/// hold an authority from every owner. Everyone else gets the same words,
/// which neither confirm nor deny anything, and the contact is written to
/// the access log either way with who called, where known, and the outcome.
async fn staff_caller(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(policy): Path<String>, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let check: CallerCheck = parse(&body)?;
    let caller = check.caller.as_deref().map(str::trim).filter(|c| !c.is_empty());
    if caller.is_some_and(|c| c.len() > 64 || !c.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        return Err(Problem::bad_request("the caller is given by their customer reference"));
    }
    let view = s
        .ask(Question::StaffPolicy {
            staff: session.party.clone(),
            policy: PolicyId::from(policy.as_str()),
            scopes: check
                .scope
                .iter()
                .flat_map(|s| s.split(','))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            purpose: check.case.map(|reference| Purpose {
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
    let holders: Vec<&str> = view["policyholders"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let owner = caller.is_some_and(|c| holders.contains(&c));
    let mut answers = Vec::new();
    if let (Some(caller), false) = (caller, owner) {
        for holder in &holders {
            answers.push(match &s.master {
                Some(master) => Some(master.speaks_for(&session.party, &PartyId::from(*holder), &PartyId::from(caller)).await),
                None => None,
            });
        }
    }
    let unchecked = answers.iter().any(|a| !matches!(a, Some(Ok((200, _)))));
    let outcome = crate::customers::caller_outcome(owner, &answers);
    // The contact, beside the read: who called (where known) and what they were told.
    let (mut request, decision): (AccessRequest, Decision) = serde_json::from_value(json!([view["request"], view["decision"]]))
        .map_err(|_| Problem::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "the access log cannot be written yet"))?;
    request.action = "contact.caller_check".to_string();
    request.record = Some(format!("policy:{policy}/caller/{}/{outcome}", caller.unwrap_or("unidentified")));
    let key = format!("contact:{}:{}:{}", request.staff, request.at, request.record.as_deref().unwrap_or_default());
    s.propose(
        key,
        Actor::Staff(request.staff.clone()),
        Command::Staff(StaffCommand::Record { request, decision }),
    )
    .await?;
    let say = match outcome {
        "discuss" if owner => "This is the policyholder. Confirm who they are in the usual way before you discuss the policy.",
        "discuss" => "This caller holds an authority to be told about this policy. Confirm who they are in the usual way first.",
        "refer" => "Do not discuss the policy on this call. A decision about who may act for this customer is with the legal team. Take the caller's details and pass the call to them.",
        _ => CALLER_REFUSED,
    };
    ok(json!({ "outcome": outcome, "say": say, "checked": !(unchecked && caller.is_some() && !owner) }))
}

/// How to serve the policyholder, for staff who opened their policy: the
/// adjustments in effect and when they are next reviewed, with no label; why
/// only for those whose role needs it. Nothing when nothing is in effect; a
/// master that cannot answer says so, so staff take care rather than assume.
fn approach(routing: Result<crate::customers::Answered, String>, may_see_kind: bool) -> Option<Value> {
    match routing {
        Ok((200, body)) if body["route_to_person"] == true => {
            let mut approach = json!({ "adjustments": body["adjustments"], "review_on": body["review_on"] });
            if may_see_kind && !body["kind"].is_null() {
                approach["kind"] = body["kind"].clone();
            }
            Some(approach)
        }
        Ok((200, _)) => None,
        _ => Some(json!({ "unavailable": true })),
    }
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
    #[derive(Deserialize)]
    struct Entered {
        id: String,
        policy: Option<PolicyId>,
        kind: String,
        module: String,
        #[serde(default)]
        payload: Value,
        /// Why the change is made, and the case it belongs to.
        #[serde(default)]
        reason: String,
        #[serde(default)]
        case: String,
    }
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let new: Entered = parse(&body)?;
    let command = Command::SubmitRequest {
        id: new.id,
        policy: new.policy,
        kind: new.kind,
        module: new.module,
        payload: new.payload,
        signed: false,
        route_to_person: false,
        assisted: Some(crate::state::Assisted {
            reason: new.reason,
            case: new.case,
            backdated: false,
            above_limit: false,
            second: None,
        }),
    };
    accepted(s.propose(key(&headers, &session.party.0)?, Actor::Staff(session.party), command).await?)
}

/// A second member of staff agrees a change another entered for a customer;
/// it then waits for the customer's approval in the app.
async fn staff_second_approval(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(id): Path<String>) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let command = Command::SecondApproveRequest { id };
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

// ---- New business, the core's own view and the customer master ----------
//
// Work done by another service on a member of staff's behalf. The portal
// asks the four questions first and writes the access log line, then the
// other service does the work and decides again by its own rules.

impl AppState {
    /// Whether this person may take `action` on `record`, logged either way.
    async fn may_act(&self, session: &Session, action: &str, class: DataClass, record: String, about: Option<PartyId>, why: Why) -> Result<(), Problem> {
        let view = self
            .ask(Question::StaffAct {
                staff: session.party.clone(),
                action: action.to_string(),
                class,
                record,
                about,
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
        self.logged(view).await.map(|_| ())
    }

    /// Every partition's answer, or the first that could not be had.
    async fn every_partition<T, F>(&self, ask: impl Fn(ic_ledger::PartitionId) -> F) -> Result<Vec<T>, Problem>
    where
        F: Future<Output = Result<T, String>>,
    {
        futures::future::join_all((0..self.claims.count()).map(ask))
            .await
            .into_iter()
            .map(|a| a.map_err(core_down))
            .collect()
    }
}

fn core_down(reason: String) -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "core_unavailable",
        format!("the core could not be reached: {reason}"),
    )
}

/// An application as the core takes it, or why it is not one.
fn application(body: &[u8]) -> Result<ic_core::command::IssuePolicy, Problem> {
    serde_json::from_slice(body).map_err(|e| Problem::bad_request(format!("that is not an application the core takes: {e}")))
}

/// Price an application: the first period's bill. Nothing is written.
async fn staff_quote(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let application = application(&body)?;
    let record = format!("application:{}", application.number);
    s.may_act(
        &session,
        staff_actions::QUOTE,
        DataClass::Personal,
        record,
        Some(application.policyholder.clone()),
        why,
    )
    .await?;
    let partition = crate::claims::partition_of(&application.group, s.claims.count());
    match s.claims.quote(partition, application).await.map_err(core_down)? {
        None => Err(Problem::new(StatusCode::SERVICE_UNAVAILABLE, "not_ready", "the business date is not open")),
        Some(Ok(premium)) => ok(json!({ "partition": partition, "premium": premium })),
        Some(Err((code, message))) => Err(module_refusal(&code, &message)),
    }
}

/// Issue a policy from an accepted application, as the new business module
/// does. Safe to repeat with the same idempotency key.
async fn staff_issue(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>, body: Bytes) -> Answer {
    let session = s.staff(&headers, &method, &uri, &body).await?;
    let application = application(&body)?;
    let key = key(&headers, &session.party.0)?;
    let record = format!("application:{}", application.number);
    s.may_act(
        &session,
        staff_actions::ISSUE,
        DataClass::Personal,
        record,
        Some(application.policyholder.clone()),
        why,
    )
    .await?;
    let partition = crate::claims::partition_of(&application.group, s.claims.count());
    let envelope = ic_core::Envelope {
        key: format!("new-business:{key}"),
        caller: NEW_BUSINESS.to_string(),
        salt: ic_raft::client::fresh_salt(),
        traceparent: None,
        command: ic_core::Command::IssuePolicy(Box::new(application)),
    };
    let proposed = s.claims.propose(partition, envelope).await.map_err(core_down)?;
    if let Some((code, message)) = proposed.rejected {
        return Err(module_refusal(&code, &message));
    }
    accepted(json!({ "accepted": true, "partition": partition, "duplicate": proposed.duplicate, "events": proposed.events }))
}

/// The caller the core knows new business by.
pub const NEW_BUSINESS: &str = "new-business";

/// A policy as the core holds it: covers with their layers, loadings and
/// exclusions, lives, billing. Underwriting terms imply health, so this is
/// for staff holding the insurance-sensitive class.
async fn staff_core_policy(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(number): Path<String>,
    Query(why): Query<Why>,
) -> Answer {
    let session = s.staff(&headers, &method, &uri, b"").await?;
    let found = s.every_partition(|p| s.claims.policy(p, number.clone())).await?.into_iter().flatten().next();
    // Asked whether or not it exists, so a refusal says nothing about the book.
    let holder = found.as_ref().and_then(|v| v["policy"]["policyholder"].as_str()).map(PartyId::from);
    s.may_act(
        &session,
        staff_actions::CORE_POLICY,
        DataClass::InsuranceSensitive,
        format!("policy:{number}"),
        holder,
        why,
    )
    .await?;
    ok(found.ok_or_else(Problem::not_found)?)
}

/// The book's reconciliation across every partition: what the close checks.
async fn ops_reconciliation(State(s): State<Shared>, headers: HeaderMap) -> Answer {
    let caller = s.workforce.authenticate(&headers).await?;
    if !caller.has_role(OPERATIONS) {
        return Err(Problem::forbidden("the operations console needs the operations role"));
    }
    let views = s.every_partition(|p| s.claims.closing(p)).await?;
    ok(crate::claims::reconciliation(&views).map_err(core_down)?)
}

/// The master's answer as it gave it, or why there is none. Its refusals
/// are passed on with its own reason.
fn desked(answer: Result<crate::customers::Answered, String>) -> Answer {
    match answer {
        Ok((status, body)) if status < 300 => ok(body),
        Ok((status, body)) => Err(Problem::new(
            StatusCode::from_u16(status)
                .ok()
                .filter(StatusCode::is_client_error)
                .unwrap_or(StatusCode::BAD_GATEWAY),
            body["title"].as_str().unwrap_or("customer_master"),
            body["detail"].as_str().unwrap_or("the customer master did not take it"),
        )),
        Err(e) => Err(Problem::new(StatusCode::SERVICE_UNAVAILABLE, "customer_master_unavailable", e)),
    }
}

/// Staff work in the customer master: the four questions, then the master.
async fn desk(s: &Shared, headers: &HeaderMap, method: &Method, uri: &Uri, body: &[u8], why: Why, desk: crate::customers::Desk) -> Answer {
    let session = s.staff(headers, method, uri, body).await?;
    let master = s.master()?;
    let action = desk.action().ok_or_else(|| Problem::forbidden("that is operations work"))?;
    let record = match desk.about() {
        Some(party) => format!("party:{}", party.0),
        None => "customers".to_string(),
    };
    s.may_act(&session, action, DataClass::Personal, record, desk.about().cloned(), why).await?;
    desked(master.desk(&session.party, desk).await)
}

/// Operations work in the customer master: the operations role decides.
async fn ops_desk(s: &Shared, headers: &HeaderMap, desk: crate::customers::Desk) -> Answer {
    let caller = s.workforce.authenticate(headers).await?;
    if !caller.has_role(OPERATIONS) {
        return Err(Problem::forbidden("the operations console needs the operations role"));
    }
    let master = s.master()?;
    desked(master.desk(&PartyId::from(caller.subject.as_str()), desk).await)
}

fn body_json(body: &[u8]) -> Result<Value, Problem> {
    let value: Value = parse(body)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(Problem::bad_request("the request body is an object"))
    }
}

#[derive(Deserialize)]
struct AsAt {
    as_of: Option<String>,
    known_at: Option<String>,
    case: Option<String>,
    scope: Option<String>,
}

impl AsAt {
    fn split(self) -> Result<(String, Why), Problem> {
        let query = crate::customers::as_at(self.as_of.as_deref(), self.known_at.as_deref()).map_err(Problem::bad_request)?;
        Ok((
            query,
            Why {
                case: self.case,
                scope: self.scope,
            },
        ))
    }
}

async fn customers_search(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>, body: Bytes) -> Answer {
    let probe = body_json(&body)?;
    desk(&s, &headers, &method, &uri, &body, why, crate::customers::Desk::Search(probe)).await
}

async fn customers_match(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>, body: Bytes) -> Answer {
    let probe = body_json(&body)?;
    desk(&s, &headers, &method, &uri, &body, why, crate::customers::Desk::Match(probe)).await
}

async fn customers_create(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>, body: Bytes) -> Answer {
    let new = body_json(&body)?;
    let key = key(&headers, "staff")?;
    desk(&s, &headers, &method, &uri, &body, why, crate::customers::Desk::Create { key, body: new }).await
}

async fn customers_read(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(party): Path<String>, Query(at): Query<AsAt>) -> Answer {
    let (query, why) = at.split()?;
    let party = PartyId::from(party.as_str());
    desk(&s, &headers, &method, &uri, b"", why, crate::customers::Desk::Read { party, query }).await
}

async fn customers_history(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(party): Path<String>, Query(at): Query<AsAt>) -> Answer {
    let (query, why) = at.split()?;
    let party = PartyId::from(party.as_str());
    desk(&s, &headers, &method, &uri, b"", why, crate::customers::Desk::History { party, query }).await
}

async fn customers_relationships(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(party): Path<String>,
    Query(why): Query<Why>,
) -> Answer {
    let desk_for = crate::customers::Desk::Relationships(PartyId::from(party.as_str()));
    desk(&s, &headers, &method, &uri, b"", why, desk_for).await
}

async fn customers_due_diligence(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(party): Path<String>,
    Query(why): Query<Why>,
) -> Answer {
    let desk_for = crate::customers::Desk::DueDiligence(PartyId::from(party.as_str()));
    desk(&s, &headers, &method, &uri, b"", why, desk_for).await
}

async fn customers_command(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(party): Path<String>,
    Query(why): Query<Why>,
    body: Bytes,
) -> Answer {
    let change = body_json(&body)?;
    let key = key(&headers, "staff")?;
    let desk_for = crate::customers::Desk::Change {
        party: PartyId::from(party.as_str()),
        key,
        body: change,
    };
    desk(&s, &headers, &method, &uri, &body, why, desk_for).await
}

async fn customers_export(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(party): Path<String>, Query(why): Query<Why>) -> Answer {
    let desk_for = crate::customers::Desk::Export(PartyId::from(party.as_str()));
    desk(&s, &headers, &method, &uri, b"", why, desk_for).await
}

async fn customers_audit(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Path(party): Path<String>, Query(why): Query<Why>) -> Answer {
    let desk_for = crate::customers::Desk::Audit(PartyId::from(party.as_str()));
    desk(&s, &headers, &method, &uri, b"", why, desk_for).await
}

async fn customers_actions_by(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(actor): Path<String>,
    Query(why): Query<Why>,
) -> Answer {
    let desk_for = crate::customers::Desk::ActionsBy(PartyId::from(actor.as_str()));
    desk(&s, &headers, &method, &uri, b"", why, desk_for).await
}

async fn customers_merge(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>, body: Bytes) -> Answer {
    let merge = body_json(&body)?;
    let key = key(&headers, "staff")?;
    desk(&s, &headers, &method, &uri, &body, why, crate::customers::Desk::Merge { key, body: merge }).await
}

async fn customers_unmerge(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>, body: Bytes) -> Answer {
    let unmerge = body_json(&body)?;
    let key = key(&headers, "staff")?;
    desk(&s, &headers, &method, &uri, &body, why, crate::customers::Desk::Unmerge { key, body: unmerge }).await
}

async fn customers_reviews(State(s): State<Shared>, headers: HeaderMap, method: Method, uri: Uri, Query(why): Query<Why>) -> Answer {
    desk(&s, &headers, &method, &uri, b"", why, crate::customers::Desk::Reviews).await
}

async fn customers_dismiss(
    State(s): State<Shared>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    Path(review): Path<String>,
    Query(why): Query<Why>,
) -> Answer {
    desk(&s, &headers, &method, &uri, b"", why, crate::customers::Desk::Dismiss(review)).await
}

async fn ops_customers_index(State(s): State<Shared>, headers: HeaderMap) -> Answer {
    ops_desk(&s, &headers, crate::customers::Desk::Index).await
}

async fn ops_customers_verify(State(s): State<Shared>, headers: HeaderMap, Path(party): Path<String>) -> Answer {
    ops_desk(&s, &headers, crate::customers::Desk::Verify(PartyId::from(party.as_str()))).await
}

async fn ops_customers_rewrap(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Answer {
    let request = body_json(&body)?;
    ops_desk(&s, &headers, crate::customers::Desk::Rewrap(request)).await
}

async fn ops_customers_reshred(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Answer {
    let request = body_json(&body)?;
    ops_desk(&s, &headers, crate::customers::Desk::Reshred(request)).await
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
        // Every function below as an MCP tool, for a person working through an MCP client (src/mcp.rs).
        .route(
            "/mcp",
            post(crate::mcp::endpoint)
                .get(crate::mcp::no_stream)
                .layer(axum::extract::DefaultBodyLimit::max(48 * 1024 * 1024)),
        )
        .route("/.well-known/oauth-protected-resource", get(crate::mcp::resource_metadata))
        .route("/.well-known/oauth-protected-resource/mcp", get(crate::mcp::resource_metadata))
        // The customer app, shared by every External ID caller.
        .route("/app/v1/me", get(me))
        .route("/app/v1/profile", get(profile))
        .route("/app/v1/profile/commands", post(profile_command))
        .route("/app/v1/profile/export", get(profile_export))
        .route("/app/v1/profile/spending-hold", get(profile_spending_hold))
        .route("/app/v1/documents/{id}/pdf", get(own_document))
        .route("/staff/v1/documents/{id}/pdf", get(staff_document))
        .route("/app/v1/proposals/{proposal}/lives/{life}/disclosures/to-sign", post(disclosure_to_sign))
        .route("/app/v1/proposals/{proposal}/lives/{life}/disclosures", post(disclosure_submit))
        .route("/app/v1/proposals/{proposal}/lives/{life}/consents/{kind}", post(consent_record))
        .route("/app/v1/proposals/{proposal}/lives/{life}/consents/{kind}/to-sign", get(consent_to_sign))
        .route(
            "/app/v1/proposals/{proposal}/lives/{life}/consents/replacement_disclosure/statement",
            get(replacement_statement),
        )
        .route("/app/v1/sync", get(sync))
        .route("/app/v1/policies/{policy}", get(policy))
        .route("/app/v1/represented/{party}", get(represented))
        .route("/app/v1/represented/{party}/policies/{policy}", get(represented_policy))
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
        .route("/adviser/v1/practice", get(practice))
        .route("/adviser/v1/statements/{number}", get(own_statement))
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
        .route("/staff/v1/policies/{policy}/caller", post(staff_caller))
        .route("/staff/v1/requests", post(staff_request))
        .route("/staff/v1/requests/{id}/second-approval", post(staff_second_approval))
        .route("/staff/v1/care/reviews", get(care_reviews))
        .route("/staff/v1/care/reviews/{review}", post(care_decide))
        .route("/staff/v1/workbench", get(workbench))
        .route("/staff/v1/workbench/actions", post(workbench_action))
        .route("/staff/v1/workbench/{party}", get(workbench_party))
        .route("/staff/v1/workbench/{party}/proof-of-debt", get(proof_of_debt))
        .route("/staff/v1/complaints/commands", post(complaints_command))
        .route("/staff/v1/complaints/{question}", get(complaints_question))
        .route("/staff/v1/claims/commands", post(claims_command))
        .route("/staff/v1/replacements", get(replacements))
        .route("/staff/v1/commission/commands", post(commission_command))
        .route("/staff/v1/commission/{question}", get(commission_question))
        .route("/staff/v1/replacements/decide", post(replacement_decide))
        .route("/staff/v1/payments/commands", post(payments_command))
        .route("/staff/v1/group/commands", post(group_staff_command))
        .route("/staff/v1/group/schemes/{scheme}/mappings/try", post(group_try_mapping))
        .route("/staff/v1/group/schemes/{policy}/first-admin", put(group_first_admin))
        .route("/staff/v1/group/{question}", get(group_staff_question))
        .route("/staff/v1/payments/{question}", get(payments_question))
        .route("/staff/v1/underwriting/commands", post(underwriting_command))
        .route("/staff/v1/underwriting/disclosures", post(underwriting_disclosures))
        .route("/staff/v1/underwriting/{verb}/{id}", post(underwriting_decide))
        .route("/staff/v1/underwriting/{question}", get(underwriting_question))
        .route("/staff/v1/reinsurance/commands", post(reinsurance_command))
        .route("/staff/v1/reinsurance/{verb}/{id}", post(reinsurance_decide))
        .route("/staff/v1/reinsurance/{question}", get(reinsurance_question))
        .route("/staff/v1/claims/{question}", get(claims_question))
        .route("/staff/v1/claims/definitions/{id}/{version}", get(claims_definition))
        .route("/staff/v1/finance/commands", post(finance_command))
        .route(
            "/staff/v1/finance/table_dry_run",
            post(finance_dry_run).layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/staff/v1/finance/{question}", get(finance_question))
        .route("/staff/v1/unit-pricing/commands", post(pricing_command))
        .route("/staff/v1/unit-pricing/{question}", get(pricing_question))
        .route("/internal/v1/commands", post(module_command))
        .route("/internal/v1/pending", get(module_pending))
        .route("/ops/v1/regions", get(ops_regions))
        .route("/ops/v1/reconciliation", get(ops_reconciliation))
        .route("/ops/v1/customers/index", get(ops_customers_index))
        .route("/ops/v1/customers/{party}/verify", post(ops_customers_verify))
        .route("/ops/v1/customers/rewrap", post(ops_customers_rewrap))
        .route("/ops/v1/customers/reshred", post(ops_customers_reshred))
        .route("/staff/v1/new-business/quote", post(staff_quote))
        .route("/staff/v1/new-business/policies", post(staff_issue))
        .route("/staff/v1/core/policies/{number}", get(staff_core_policy))
        .route("/staff/v1/customers", post(customers_create))
        .route("/staff/v1/customers/search", post(customers_search))
        .route("/staff/v1/customers/match", post(customers_match))
        .route("/staff/v1/customers/merges", post(customers_merge))
        .route("/staff/v1/customers/unmerges", post(customers_unmerge))
        .route("/staff/v1/customers/reviews", get(customers_reviews))
        .route("/staff/v1/customers/reviews/{review}/dismiss", post(customers_dismiss))
        .route("/staff/v1/customers/{party}", get(customers_read))
        .route("/staff/v1/customers/{party}/history", get(customers_history))
        .route("/staff/v1/customers/{party}/relationships", get(customers_relationships))
        .route("/staff/v1/customers/{party}/due-diligence", get(customers_due_diligence))
        .route("/staff/v1/customers/{party}/commands", post(customers_command))
        .route("/staff/v1/customers/{party}/export", get(customers_export))
        .route("/staff/v1/customers/{party}/audit", get(customers_audit))
        .route("/staff/v1/actors/{actor}/actions", get(customers_actions_by))
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
        .route("/payments", get(|State(s): State<Shared>| async move { page(&s, crate::ui::PAYMENTS) }))
        .route("/group", get(|State(s): State<Shared>| async move { page(&s, crate::ui::GROUP) }))
        .route("/underwriting", get(|State(s): State<Shared>| async move { page(&s, crate::ui::UNDERWRITING) }))
        .route("/pricing", get(|State(s): State<Shared>| async move { page(&s, crate::ui::PRICING) }))
        .route("/replacements", get(|State(s): State<Shared>| async move { page(&s, crate::ui::REPLACEMENTS) }))
        .route("/commission", get(|State(s): State<Shared>| async move { page(&s, crate::ui::COMMISSION) }))
        .route("/ui/app.js", get(|| async { asset("text/javascript; charset=utf-8", crate::ui::SCRIPT) }))
        .route("/ui/app.css", get(|| async { asset("text/css; charset=utf-8", crate::ui::STYLE) }))
        .route("/ui/theme.js", get(|| async { asset("text/javascript; charset=utf-8", crate::ui::THEME) }))
        .route(
            "/ui/inter.woff2",
            get(|| async { hardened(([("content-type", "font/woff2"), ("cache-control", "public, max-age=604800")], crate::ui::FONT).into_response()) }),
        )
        .route("/ui/config.json", get(ui_config))
        .with_state(state)
}
