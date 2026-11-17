//! The group service: the driver run as a process.
//!
//! It runs the driver's pass on a timer and serves the routes the portals
//! and the other services use: upload a file, send a command, ask a
//! question. It holds no state: any number can run, and one that stops is
//! simply restarted. Its callers are services (the portal, the staff
//! console, other modules' drivers), which have already established who
//! the person is; each presents its own Entra ID token ([`crate::auth`]).

use ic_telemetry::Traced;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use ic_azure::credential::AzureCredential;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{AuthError, Caller, Entra};
use crate::command::{caller, Envelope, GroupCommand, Resolution};
use crate::driver::{Core, CustomerMaster, Driver, DriverError, FileStore, Module, Pass, Peers};
use crate::host::Question;
use crate::record::MemberIdentity;

/// The largest file an employer may upload.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;
pub const MIN_SECRET: usize = 32;

/// Where everything is. Read from the environment; the two secrets come
/// from mounted Key Vault secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// The ledger nodes, `host:port` separated by commas.
    pub cluster: String,
    pub quarantine_endpoint: String,
    pub quarantine_container: String,
    pub event_hubs_namespace: String,
    pub event_hub: String,
    pub customer_master: String,
    pub customer_master_scope: String,
    /// The name the reinsurance module is hosted under.
    pub reinsurance_module: String,
    /// The name the underwriting module is hosted under.
    pub underwriting_module: String,
    /// The name the commission module is hosted under.
    pub distribution_module: String,
    /// The name the documents module is hosted under.
    pub documents_module: String,
    /// Salts member fingerprints. It must never change.
    pub fingerprint_salt: String,
    /// The Entra ID tenant whose tokens callers present, where it issues
    /// them, and the audience (this service's application) they are for.
    pub tenant: String,
    pub entra_authority: String,
    pub audience: String,
    pub listen: String,
    pub cycle_every: Duration,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the salt.
        write!(f, "Config(cluster {}, listen {})", self.cluster, self.listen)
    }
}

impl Config {
    /// Read through `get`, which is `std::env::var` in production. Every
    /// setting without a default is required: a service that started
    /// without its storage would fail at the first file, not at start.
    ///
    /// A secret is given as `KEY`, or as `KEY_FILE` naming a file that
    /// holds it (a mounted Key Vault secret), read through `read`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, read: impl Fn(&str) -> std::io::Result<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("{key} is not set"));
        let secret = |key: &str| {
            let value = match (get(key), get(&format!("{key}_FILE"))) {
                (Some(value), _) => value,
                (None, Some(path)) => read(&path).map_err(|e| format!("{key}_FILE cannot be read: {e}"))?.trim().to_string(),
                (None, None) => return Err(format!("{key} is not set")),
            };
            if value.len() < MIN_SECRET {
                return Err(format!("{key} must be at least {MIN_SECRET} characters"));
            }
            Ok(value)
        };
        let seconds = match get("IC_GROUP_CYCLE_SECONDS") {
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|s| *s > 0)
                .ok_or("IC_GROUP_CYCLE_SECONDS must be a whole number of seconds above zero")?,
            None => 5,
        };
        Ok(Self {
            cluster: get("IC_CLUSTER_NODES").or_else(|| get("IC_NODES")).ok_or("IC_CLUSTER_NODES is not set")?,
            quarantine_endpoint: need("IC_GROUP_QUARANTINE_ENDPOINT")?,
            quarantine_container: get("IC_GROUP_QUARANTINE_CONTAINER").unwrap_or_else(|| "group-quarantine".into()),
            event_hubs_namespace: need("IC_GROUP_EVENT_HUBS_NAMESPACE")?,
            event_hub: get("IC_GROUP_EVENT_HUB").unwrap_or_else(|| "group-events".into()),
            customer_master: need("IC_GROUP_CUSTOMER_MASTER")?,
            customer_master_scope: need("IC_GROUP_CUSTOMER_MASTER_SCOPE")?,
            reinsurance_module: get("IC_GROUP_REINSURANCE_MODULE").unwrap_or_else(|| "reinsurance".into()),
            underwriting_module: get("IC_GROUP_UNDERWRITING_MODULE").unwrap_or_else(|| "underwriting".into()),
            distribution_module: get("IC_GROUP_DISTRIBUTION_MODULE").unwrap_or_else(|| "distribution".into()),
            documents_module: get("IC_GROUP_DOCUMENTS_MODULE").unwrap_or_else(|| "documents".into()),
            fingerprint_salt: secret("IC_GROUP_FINGERPRINT_SALT")?,
            tenant: need("AZURE_TENANT_ID")?,
            entra_authority: get("IC_GROUP_ENTRA_AUTHORITY")
                .or_else(|| get("AZURE_AUTHORITY_HOST"))
                .unwrap_or_else(|| crate::auth::ENTRA_AUTHORITY.into()),
            audience: need("IC_GROUP_AUDIENCE")?,
            listen: get("IC_GROUP_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            cycle_every: Duration::from_secs(seconds),
        })
    }
}

/// The customer master over HTTP, with an Entra ID token. The route is
/// this module's proposal: no customer master exists in the repository.
/// It answers with a [`Resolution`]: one high-confidence match is
/// `linked`, no candidate creates the person and is `created`, and
/// anything between is `possible` with the candidates.
#[derive(Clone)]
pub struct HttpCustomerMaster {
    base: String,
    scope: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

impl HttpCustomerMaster {
    /// `scope` is the Entra ID scope of the customer master's API.
    pub fn new(base: &str, scope: &str, credential: AzureCredential) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            scope: scope.to_string(),
            credential,
            http: ic_azure::http_client(),
        }
    }
}

impl CustomerMaster for HttpCustomerMaster {
    async fn resolve(&self, scheme: &str, identity: &MemberIdentity) -> Result<Resolution, String> {
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let body = json!({
            "source": format!("group-scheme:{scheme}"),
            "given_name": identity.given_name,
            "family_name": identity.family_name,
            "date_of_birth": identity.date_of_birth,
            "email": identity.email,
            "national_id": identity.national_id,
            "mobile_phone": identity.mobile_phone,
            "create_if_none": true,
        });
        let resp = self
            .http
            .post(format!("{}/v1/parties/match", self.base))
            .bearer_auth(token)
            // Why the customer master is being asked, which its access policy weighs.
            .header("x-ic-purpose", "scheme_membership")
            .json(&body)
            .traced()
            .send()
            .await
            // The error may quote the request: say only that it failed.
            .map_err(|e| format!("the customer master could not be reached: {}", e.without_url()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("the customer master returned {status}"));
        }
        let answer: Resolution = resp.json().await.map_err(|_| "the customer master's answer was not a resolution".to_string())?;
        match &answer {
            Resolution::Possible { candidates } if candidates.is_empty() => Err("the customer master named no candidates".to_string()),
            _ => Ok(answer),
        }
    }
}

/// What the pass loop has been doing, for `/metrics`. Counted in this
/// process only: every replica reports its own.
#[derive(Debug, Default)]
pub struct Health {
    passes: AtomicU64,
    passes_failed: AtomicU64,
    steps: AtomicU64,
    waiting: AtomicU64,
}

impl Health {
    /// Take note of a pass, as `run_passes` reports it.
    pub fn record(&self, pass: &Result<Pass, DriverError>) {
        self.passes.fetch_add(1, Ordering::Relaxed);
        match pass {
            Ok(p) => {
                self.steps.fetch_add(u64::from(p.done), Ordering::Relaxed);
                self.waiting.store(u64::from(p.failed), Ordering::Relaxed);
            }
            Err(_) => {
                self.passes_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The counters in the Prometheus text format.
    pub fn metrics(&self) -> String {
        let line = |name: &str, kind: &str, help: &str, value: &AtomicU64| {
            format!("# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {}\n", value.load(Ordering::Relaxed))
        };
        [
            line("ic_group_passes_total", "counter", "Driver passes started.", &self.passes),
            line(
                "ic_group_passes_failed_total",
                "counter",
                "Driver passes that could not run at all.",
                &self.passes_failed,
            ),
            line("ic_group_steps_total", "counter", "Outbox entries delivered and file steps taken.", &self.steps),
            line(
                "ic_group_waiting",
                "gauge",
                "Steps the last pass could not take and will try again.",
                &self.waiting,
            ),
        ]
        .concat()
    }
}

struct Web<M, C, P, D, F> {
    driver: Arc<Driver<M, C, P, D, F>>,
    health: Arc<Health>,
    /// Validates callers' Entra ID tokens.
    entra: Entra,
}

type Answer = (StatusCode, Json<Value>);

fn refused(status: StatusCode, code: &str, message: &str) -> Answer {
    (status, Json(json!({ "error": { "code": code, "message": message } })))
}

fn failed(e: DriverError) -> Answer {
    match e {
        DriverError::Refused(reject) => refused(StatusCode::UNPROCESSABLE_ENTITY, &reject.code, &reject.message),
        // Nothing is known to have happened: the caller sends the same again.
        DriverError::Unavailable(_) => refused(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "try again shortly"),
        DriverError::Answer(_) => refused(StatusCode::BAD_GATEWAY, "unexpected_answer", "the module's answer could not be read"),
    }
}

impl<M, C, P, D, F> Web<M, C, P, D, F> {
    /// The calling service, from its token. Which caller names it may use
    /// is checked where the name is given.
    async fn authorised(&self, headers: &HeaderMap) -> Result<Caller, Answer> {
        match self.entra.authenticate(headers).await {
            Ok(caller) => Ok(caller),
            // Nothing is known about the token: the caller tries again.
            Err(AuthError::Keys(e)) => {
                tracing::warn!(error = %e, "Entra ID signing keys could not be fetched");
                Err(refused(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "try again shortly"))
            }
            Err(e) => Err(refused(StatusCode::UNAUTHORIZED, "unauthorised", &e.to_string())),
        }
    }
}

/// A caller name the token does not carry as a role.
fn not_your_role(name: &str) -> Answer {
    refused(StatusCode::FORBIDDEN, "role_not_held", &format!("the token does not carry the role {name}"))
}

/// The channels a person or another service reaches the module through.
/// The driver's own commands are not among them.
const OUTSIDE_CALLERS: [&str; 6] = [
    caller::STAFF,
    caller::EMPLOYER,
    caller::MEMBER,
    caller::UNDERWRITING,
    caller::REINSURANCE,
    caller::CASH,
];

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

async fn upload<M, C, P, D, F>(
    State(web): State<Arc<Web<M, C, P, D, F>>>,
    Path((scheme, file)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Answer
where
    M: Module + 'static,
    C: Core + 'static,
    P: Peers + 'static,
    D: CustomerMaster + 'static,
    F: FileStore + 'static,
{
    let service = match web.authorised(&headers).await {
        Ok(service) => service,
        Err(answer) => return answer,
    };
    let (Some(who), Some(actor)) = (header(&headers, "x-ic-caller"), header(&headers, "x-ic-actor")) else {
        return refused(StatusCode::BAD_REQUEST, "missing_identity", "x-ic-caller and x-ic-actor say who is uploading");
    };
    if ![caller::EMPLOYER, caller::STAFF].contains(&who) {
        return refused(StatusCode::FORBIDDEN, "caller_not_allowed", "files come from the employer or from staff");
    }
    if !service.may_act_as(who) {
        return not_your_role(who);
    }
    if body.is_empty() {
        return refused(StatusCode::BAD_REQUEST, "empty_file", "the file is empty");
    }
    match web.driver.upload(who, actor, &scheme, &file, body.to_vec()).await {
        Ok(status) => (StatusCode::ACCEPTED, Json(json!({ "scheme": scheme, "file": file, "status": status }))),
        Err(DriverError::Unavailable(e)) if e.contains("names are") => refused(StatusCode::BAD_REQUEST, "invalid_name", &e),
        Err(e) => failed(e),
    }
}

/// A proposed mapping and a sample of the employer's file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Trial {
    mapping: crate::mapping::FileMapping,
    sample: String,
}

/// Staff try a new mapping version against a sample file before setting
/// it. Nothing is stored and nothing changes.
async fn try_mapping<M, C, P, D, F>(State(web): State<Arc<Web<M, C, P, D, F>>>, Path(scheme): Path<String>, headers: HeaderMap, body: Bytes) -> Answer
where
    M: Module + 'static,
    C: Core + 'static,
    P: Peers + 'static,
    D: CustomerMaster + 'static,
    F: FileStore + 'static,
{
    match web.authorised(&headers).await {
        Ok(service) if service.may_act_as(caller::STAFF) => {}
        Ok(_) => return not_your_role(caller::STAFF),
        Err(answer) => return answer,
    }
    let trial: Trial = match serde_json::from_slice(&body) {
        Ok(t) => t,
        Err(e) => return refused(StatusCode::BAD_REQUEST, "invalid_trial", &e.to_string()),
    };
    match web.driver.try_mapping(&scheme, trial.mapping, trial.sample.as_bytes()).await {
        Ok(answer) => (StatusCode::OK, Json(answer)),
        Err(DriverError::Answer(e)) => refused(StatusCode::NOT_FOUND, "not_found", &e),
        Err(e) => failed(e),
    }
}

/// A command from a portal or another service. The business date is the
/// core's, never the caller's.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    key: String,
    caller: String,
    actor: String,
    command: GroupCommand,
}

async fn command<M, C, P, D, F>(State(web): State<Arc<Web<M, C, P, D, F>>>, headers: HeaderMap, body: Bytes) -> Answer
where
    M: Module + 'static,
    C: Core + 'static,
    P: Peers + 'static,
    D: CustomerMaster + 'static,
    F: FileStore + 'static,
{
    let service = match web.authorised(&headers).await {
        Ok(service) => service,
        Err(answer) => return answer,
    };
    let mut request: Request = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return refused(StatusCode::BAD_REQUEST, "invalid_command", &e.to_string()),
    };
    // A mapping is sealed here, as a trial seals it: a person sets the
    // layout they tried, and never works out its content hash.
    if let GroupCommand::SetMapping { mapping, .. } = &mut request.command {
        match mapping.as_ref().clone().seal() {
            Ok(sealed) => **mapping = sealed,
            Err(e) => return refused(StatusCode::UNPROCESSABLE_ENTITY, "invalid_mapping", &e.to_string()),
        }
    }
    if request.key.is_empty() || request.actor.is_empty() || !OUTSIDE_CALLERS.contains(&request.caller.as_str()) {
        return refused(
            StatusCode::FORBIDDEN,
            "caller_not_allowed",
            "a command needs a key, an actor and a caller this service takes",
        );
    }
    if !service.may_act_as(&request.caller) {
        return not_your_role(&request.caller);
    }
    if matches!(request.command, GroupCommand::ReceiveFile { .. }) {
        return refused(StatusCode::BAD_REQUEST, "use_upload", "a file is received by uploading it");
    }
    let today = match web.driver.core.business_date().await {
        Ok(Some(today)) => today,
        _ => return failed(DriverError::Unavailable("the business date is not open".to_string())),
    };
    let envelope = Envelope {
        // Callers' keys cannot collide with the driver's own.
        key: format!("api:{}:{}", request.caller, request.key),
        caller: request.caller,
        actor: request.actor,
        today,
        command: request.command,
    };
    match web.driver.module.propose(envelope).await {
        Ok(outcome) => match outcome.result {
            Ok(()) => (StatusCode::OK, Json(json!({ "applied": true, "duplicate": outcome.duplicate }))),
            Err(reject) => failed(DriverError::Refused(reject)),
        },
        Err(e) => failed(e),
    }
}

async fn question<M, C, P, D, F>(State(web): State<Arc<Web<M, C, P, D, F>>>, headers: HeaderMap, body: Bytes) -> Answer
where
    M: Module + 'static,
    C: Core + 'static,
    P: Peers + 'static,
    D: CustomerMaster + 'static,
    F: FileStore + 'static,
{
    // Any caller this service knows may ask; what a person may see is the
    // calling service's to decide.
    let staff = match web.authorised(&headers).await {
        Ok(service) if OUTSIDE_CALLERS.iter().any(|name| service.may_act_as(name)) => service.may_act_as(caller::STAFF),
        Ok(_) => return refused(StatusCode::FORBIDDEN, "role_not_held", "the token carries no role this service takes"),
        Err(answer) => return answer,
    };
    let question: Question = match serde_json::from_slice(&body) {
        Ok(q) => q,
        Err(e) => return refused(StatusCode::BAD_REQUEST, "invalid_question", &e.to_string()),
    };
    // Who else a member might be, and whose loading is in the total, are for
    // staff, never the employer.
    if matches!(question, Question::PossibleMatch { .. } | Question::LoadingsInTotal { .. }) && !staff {
        return refused(
            StatusCode::FORBIDDEN,
            "role_not_held",
            "possible matches are decided by group administration staff",
        );
    }
    // The outbox carries commands with underwriting terms: it is the driver's alone.
    if matches!(question, Question::Outbox { .. }) {
        return refused(StatusCode::FORBIDDEN, "not_available", "the outbox is not served here");
    }
    match web.driver.module.ask(question).await {
        Ok(answer) => (StatusCode::OK, Json(answer)),
        Err(DriverError::Answer(e)) => refused(StatusCode::NOT_FOUND, "not_found", &e),
        Err(e) => failed(e),
    }
}

/// Ready when the cluster answers: without it every route here would
/// answer 503, so the replica is better left out until it does.
async fn ready<M, C, P, D, F>(State(web): State<Arc<Web<M, C, P, D, F>>>) -> (StatusCode, &'static str)
where
    M: Module + 'static,
    C: Core + 'static,
    P: Peers + 'static,
    D: CustomerMaster + 'static,
    F: FileStore + 'static,
{
    match web.driver.core.business_date().await {
        Ok(_) => (StatusCode::OK, "ready"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "the cluster is not answering"),
    }
}

async fn metrics<M, C, P, D, F>(State(web): State<Arc<Web<M, C, P, D, F>>>) -> String
where
    M: Module + 'static,
    C: Core + 'static,
    P: Peers + 'static,
    D: CustomerMaster + 'static,
    F: FileStore + 'static,
{
    web.health.metrics()
}

/// The service's routes: `POST /v1/schemes/{scheme}/files/{file}` to
/// upload, `POST /v1/commands`, `POST /v1/questions`, and `GET /healthz`,
/// `GET /readyz` and `GET /metrics`, which need no token.
pub fn router<M, C, P, D, F>(driver: Arc<Driver<M, C, P, D, F>>, entra: Entra, health: Arc<Health>) -> Router
where
    M: Module + 'static,
    C: Core + 'static,
    P: Peers + 'static,
    D: CustomerMaster + 'static,
    F: FileStore + 'static,
{
    let web = Arc::new(Web { driver, entra, health });
    Router::new()
        .route("/v1/schemes/{scheme}/files/{file}", post(upload::<M, C, P, D, F>))
        .route("/v1/schemes/{scheme}/mappings/try", post(try_mapping::<M, C, P, D, F>))
        .route("/v1/commands", post(command::<M, C, P, D, F>))
        .route("/v1/questions", post(question::<M, C, P, D, F>))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready::<M, C, P, D, F>))
        .route("/metrics", get(metrics::<M, C, P, D, F>))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .layer(axum::middleware::from_fn(ic_telemetry::http::trace))
        .with_state(web)
}

/// Run the driver's pass every `every` until `stop` completes. A pass that
/// could not do everything says so in the log and the next one tries
/// again; nothing here ends the loop but `stop`.
pub async fn run_passes<M, C, P, D, F>(
    driver: Arc<Driver<M, C, P, D, F>>,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&Result<Pass, DriverError>),
) where
    M: Module,
    C: Core,
    P: Peers,
    D: CustomerMaster,
    F: FileStore,
{
    tokio::pin!(stop);
    loop {
        // Stopping does not wait for a pass to finish: every step of a pass
        // is taken again from where it was, so one cut short loses nothing.
        let pass = tokio::select! {
            () = &mut stop => return,
            pass = driver.run_once() => pass,
        };
        match &pass {
            Ok(p) if p.failed == 0 => tracing::debug!(done = p.done, "group pass"),
            Ok(p) => tracing::warn!(done = p.done, waiting = p.failed, "a group pass left work waiting on something outside"),
            Err(e) => tracing::warn!(error = %e, "a group pass could not run"),
        }
        seen(&pass);
        // With work flowing, go straight round again; otherwise wait.
        let busy = matches!(&pass, Ok(p) if p.done > 0);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(if busy { Duration::from_millis(10) } else { every }) => {}
        }
    }
}

/// Serve the routes on `listener` until `stop` completes.
pub async fn serve(listener: tokio::net::TcpListener, routes: Router, stop: impl Future<Output = ()> + Send + 'static) -> Result<(), DriverError> {
    axum::serve(listener, routes)
        .with_graceful_shutdown(stop)
        .await
        .map_err(|e| DriverError::Unavailable(e.to_string()))
}
