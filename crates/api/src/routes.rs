//! Routes. Reads are scoped to the caller's audience; writes are accepted
//! only from a caller holding the role of the module the command belongs to.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ic_cashmatch::{IncomingPayment, Outcome};
use ic_core::claims_module::report::{Question, Reply};
use ic_core::command::IssuePolicy;
use ic_core::policy::LifeKind;
use ic_core::{Command, GroupId, PartyId, PolicyId};
use ic_ledger::PartitionId;
use ic_readmodel::scope::{AudienceScope, Relationship, RowScope};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{Auth, AuthError, Caller};
use crate::backend::{Backend, CloseError, Unavailable, Written};

/// The staff role that may run the close and read reconciliation.
pub const OPERATIONS: &str = "operations";
/// Module roles that never arrive over the API: the partition leader and the
/// end-of-day run propose their own commands.
const INTERNAL_CALLERS: [&str; 2] = ["leader", "end-of-day"];

pub struct AppState {
    pub backend: Backend,
    pub auth: Auth,
    pub scope: AudienceScope,
}

pub type Shared = Arc<AppState>;

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .route("/v1/quotes", post(quote))
        .route("/v1/policies", post(issue))
        .route("/v1/policies/{number}", get(policy))
        .route("/v1/payments", post(payment))
        .route("/v1/commands", post(command))
        .route("/v1/claims/{claim}", get(claim))
        .route("/v1/claims/{claim}/work", get(claims_work))
        .route("/v1/claims/queues/{queue}", get(claims_queue))
        .route("/v1/claims/book", get(claims_book))
        .route("/v1/claims/report", get(claims_report))
        .route("/v1/claims/runs/{run}/remittance", get(claims_remittance))
        .route("/v1/admin/end-of-day", post(end_of_day))
        .route("/v1/admin/reconciliation", get(reconciliation))
        .with_state(state)
}

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
    fn forbidden(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", detail)
    }
    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such resource")
    }
    fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", detail)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let body =
            json!({ "type": format!("urn:insurance-core:problem:{}", self.code), "title": self.code, "status": self.status.as_u16(), "detail": self.detail });
        let mut response = (self.status, [("content-type", "application/problem+json")], body.to_string()).into_response();
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            response.headers_mut().insert("retry-after", axum::http::HeaderValue::from_static("1"));
        }
        response
    }
}

impl From<AuthError> for Problem {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Keys(_) => Problem::new(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable", e.to_string()),
            _ => Problem::new(StatusCode::UNAUTHORIZED, "unauthenticated", e.to_string()),
        }
    }
}

impl From<Unavailable> for Problem {
    /// The ledger could not confirm the request in time, for example while
    /// a partition elects a leader. A write may or may not have been applied:
    /// the caller repeats it with the same idempotency key.
    fn from(e: Unavailable) -> Self {
        Problem::new(StatusCode::SERVICE_UNAVAILABLE, "ledger_unavailable", e.to_string())
    }
}

type ApiResult = Result<Response, Problem>;

fn require(caller: &Caller, role: &str) -> Result<(), Problem> {
    if caller.has_role(role) {
        Ok(())
    } else {
        Err(Problem::forbidden(format!("role {role} is required")))
    }
}

/// The caller's idempotency key, namespaced by module so two modules cannot
/// collide.
fn idempotency_key(headers: &HeaderMap, module: &str) -> Result<String, Problem> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|k| !k.is_empty() && k.len() <= 128);
    key.map(|k| format!("{module}:{k}"))
        .ok_or_else(|| Problem::bad_request("an Idempotency-Key header of 1 to 128 characters is required"))
}

/// What a write returns. The command is in the log; its outcome is here and
/// on the event stream.
fn written(partition: PartitionId, written: &Written) -> Response {
    match &written.rejected {
        None => (StatusCode::ACCEPTED, Json(json!({ "status": "applied", "partition": partition, "log_index": written.log_index, "duplicate": written.duplicate, "events": written.events })))
            .into_response(),
        Some(r) => Problem::new(StatusCode::UNPROCESSABLE_ENTITY, &r.code, r.message.clone()).into_response(),
    }
}

async fn ready(State(s): State<Shared>) -> Response {
    match s.backend.business_date().await {
        Ok(Some(d)) => Json(json!({ "business_date": d })).into_response(),
        Ok(None) => (StatusCode::SERVICE_UNAVAILABLE, "business date not open").into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
}

/// A dry run of an application: the first period's bill. Never reaches the log.
async fn quote(State(s): State<Shared>, headers: HeaderMap, Json(application): Json<IssuePolicy>) -> ApiResult {
    s.auth.authenticate(&headers).await?;
    match s.backend.quote(application).await? {
        None => Err(Problem::new(StatusCode::SERVICE_UNAVAILABLE, "not_ready", "business date not open")),
        Some(Ok(premium)) => Ok(Json(premium).into_response()),
        Some(Err(r)) => Err(Problem::new(StatusCode::UNPROCESSABLE_ENTITY, &r.code, r.message)),
    }
}

async fn issue(State(s): State<Shared>, headers: HeaderMap, Json(application): Json<IssuePolicy>) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    require(&caller, "new-business")?;
    let key = idempotency_key(&headers, "new-business")?;
    let partition = s.backend.partition_of(&application.group).await;
    let applied = s
        .backend
        .submit(partition, &key, "new-business", Command::IssuePolicy(Box::new(application)))
        .await?;
    Ok(written(partition, &applied))
}

#[derive(Debug, Deserialize)]
struct CommandRequest {
    /// Where to send it. Give one of these; a command about a policy can be
    /// routed by the policy directory.
    #[serde(default)]
    partition: Option<PartitionId>,
    #[serde(default)]
    group: Option<GroupId>,
    #[serde(default)]
    policy: Option<PolicyId>,
    command: Command,
}

/// The module-to-core command path. The caller acts as the first of its
/// roles that is allowed to send the command.
async fn command(State(s): State<Shared>, headers: HeaderMap, Json(req): Json<CommandRequest>) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    let allowed = req.command.allowed_callers();
    let module = allowed
        .iter()
        .copied()
        .find(|m| !INTERNAL_CALLERS.contains(m) && caller.has_role(m))
        .ok_or_else(|| Problem::forbidden(format!("{} needs one of the roles {allowed:?}", req.command.name())))?;
    let key = idempotency_key(&headers, module)?;
    let partitions = s.backend.partitions().await;
    if module == "config" {
        // Configuration is activated on every partition. A caller told the
        // ledger was unavailable part way repeats the request with the same
        // key: partitions that have it answer as duplicates.
        let mut last = None;
        for p in 0..partitions {
            let applied = s.backend.submit(p, &format!("{key}:{p}"), module, req.command.clone()).await?;
            if applied.rejected.is_some() {
                return Ok(written(p, &applied));
            }
            last = Some((p, applied));
        }
        return last.map(|(p, a)| written(p, &a)).ok_or_else(Problem::not_found);
    }
    let partition = match (req.partition, &req.group, &req.policy) {
        (Some(p), _, _) if p < partitions => p,
        (Some(p), _, _) => return Err(Problem::bad_request(format!("there is no partition {p}"))),
        (None, Some(g), _) => s.backend.partition_of(g).await,
        (None, None, Some(policy)) => s.backend.locate(policy).await?.map(|e| e.partition).ok_or_else(Problem::not_found)?,
        (None, None, None) => return Err(Problem::bad_request("give a partition, a group or a policy")),
    };
    let applied = s.backend.submit(partition, &key, module, req.command).await?;
    Ok(written(partition, &applied))
}

/// An incoming payment from the payments module. A confident match becomes a
/// receipt on the policy; anything else is an exception item and the money
/// stays in unidentified receipts.
async fn payment(State(s): State<Shared>, headers: HeaderMap, Json(p): Json<IncomingPayment>) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    require(&caller, "cash-matching")?;
    match s.backend.match_payment(&p).await? {
        Outcome::Apply { matched, command } => {
            // The receipt ID is the idempotency key: a payment file sent twice applies once.
            let applied = s
                .backend
                .submit(matched.partition, &format!("receipt:{}", p.id), "cash-matching", command)
                .await?;
            match &applied.rejected {
                None => Ok((
                    StatusCode::ACCEPTED,
                    Json(json!({ "status": "applied", "match": matched, "duplicate": applied.duplicate })),
                )
                    .into_response()),
                Some(r) => Ok((
                    StatusCode::ACCEPTED,
                    Json(json!({ "status": "exception", "reason": format!("{}: {}", r.code, r.message), "suggestion": matched })),
                )
                    .into_response()),
            }
        }
        Outcome::Exception { reason, suggestion } => Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "status": "exception", "reason": reason, "suggestion": suggestion })),
        )
            .into_response()),
    }
}

/// A policy as this caller may see it. Each row is checked against the
/// audience policy; a caller who may see nothing gets the same answer as for
/// a policy that does not exist.
async fn policy(State(s): State<Shared>, headers: HeaderMap, Path(number): Path<String>) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    let found = s.backend.policy(&number).await?.ok_or_else(Problem::not_found)?;
    let p = &found.policy;
    let today = found.business_date.unwrap_or(p.inception);

    // Staff servicing sees the whole policy. Everyone else sees what their
    // relationship to it allows. Relationships come from the customer master;
    // until that module exists only development mode can assert them.
    let staff = caller.has_role("policy-servicing");
    let relationships: &[Relationship] = &caller.dev_relationships;
    let may = |kind: &str, subject: Option<&PartyId>, sensitive: bool| -> Result<bool, Problem> {
        if staff {
            return Ok(true);
        }
        let row = RowScope {
            kind,
            subject: subject.map(|s| s.0.as_str()),
            sensitive,
        };
        s.scope
            .may_view(&caller.subject, relationships, &row, true)
            .map_err(|e| Problem::new(StatusCode::INTERNAL_SERVER_ERROR, "scope", e.to_string()))
    };

    let mut body = serde_json::Map::new();
    if may("policy", None, false)? {
        body.insert("policy".into(), json!({ "number": p.number, "product": p.product_code, "status": p.status, "standing": p.standing, "inception": p.inception, "frequency": p.frequency }));
    }
    if may("billing", None, false)? {
        if let Some(b) = &found.billing {
            body.insert("billing".into(), json!(b));
        }
    }
    let mut covers: Vec<Value> = Vec::new();
    let mut terms: Vec<Value> = Vec::new();
    for cover in p.covers.values() {
        for layer in cover.layers.iter().filter(|l| l.in_force_on(today)) {
            let party = p.lives.get(&layer.life).and_then(|l| match &l.kind {
                LifeKind::Named { party } => Some(party),
                LifeKind::UnnamedMember { linked_party, .. } => linked_party.as_ref(),
                LifeKind::Pool { .. } => None,
            });
            if may("cover", party, false)? {
                covers.push(json!({ "cover": cover.code, "type": cover.cover_type, "life": layer.life, "layer": layer.id, "start": layer.start, "sum_insured_minor": layer.sum_insured_minor }));
            }
            let has_terms = !layer.loadings.is_empty() || !layer.exclusions.is_empty();
            if has_terms && may("underwriting_terms", party, true)? {
                terms.push(json!({ "cover": cover.code, "life": layer.life, "layer": layer.id, "loadings": layer.loadings, "exclusions": layer.exclusions }));
            }
        }
    }
    if !covers.is_empty() {
        body.insert("covers".into(), Value::Array(covers));
    }
    if !terms.is_empty() {
        body.insert("underwriting_terms".into(), Value::Array(terms));
    }
    if body.is_empty() {
        return Err(Problem::not_found());
    }
    Ok(Json(Value::Object(body)).into_response())
}

/// The role the claims module's own staff hold.
const CLAIMS: &str = "claims";

/// One claim. That a claim exists implies health, so the row is sensitive:
/// claims staff see it, and so does the person it is about. Anyone else gets
/// the same answer as for a claim that does not exist.
async fn claim(State(s): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    let replies = s.backend.claims(&Question::Claim { claim: id }).await?;
    let row = replies
        .into_iter()
        .find_map(|r| match r {
            Reply::Claim(Some(row)) => Some(row),
            _ => None,
        })
        .ok_or_else(Problem::not_found)?;
    if !caller.has_role(CLAIMS) {
        let scope = RowScope {
            kind: "claim",
            subject: Some(&row.party),
            sensitive: true,
        };
        let allowed = s
            .scope
            .may_view(&caller.subject, &caller.dev_relationships, &scope, true)
            .map_err(|e| Problem::new(StatusCode::INTERNAL_SERVER_ERROR, "scope", e.to_string()))?;
        if !allowed {
            return Err(Problem::not_found());
        }
    }
    Ok(Json(json!(row.view)).into_response())
}

/// A work queue across every partition, most urgent first and then oldest.
async fn claims_queue(State(s): State<Shared>, headers: HeaderMap, Path(queue): Path<String>) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    require(&caller, CLAIMS)?;
    let queue = serde_json::from_value(Value::String(queue)).map_err(|_| Problem::not_found())?;
    let mut rows: Vec<_> = s
        .backend
        .claims(&Question::Queue { queue })
        .await?
        .into_iter()
        .flat_map(|r| match r {
            Reply::Queue(rows) => rows,
            _ => vec![],
        })
        .collect();
    rows.sort_by(|a, b| b.priority.cmp(&a.priority).then(b.age_days.cmp(&a.age_days)));
    Ok(Json(json!({ "rows": rows })).into_response())
}

/// Each partition's answer to a question only claims staff may ask.
async fn claims_staff(s: &Shared, headers: &HeaderMap, question: Question) -> ApiResult {
    let caller = s.auth.authenticate(headers).await?;
    require(&caller, CLAIMS)?;
    let partitions: Vec<Value> = s
        .backend
        .claims(&question)
        .await?
        .into_iter()
        .map(|r| match r {
            Reply::Book(b) => json!(b),
            Reply::Report(r) => json!(r),
            Reply::Remittance(r) => json!(r),
            Reply::Claim(_) | Reply::Queue(_) | Reply::Work(_) | Reply::PolicyStatus(_) => Value::Null,
        })
        .collect();
    Ok(Json(json!({ "partitions": partitions })).into_response())
}

/// One claim as the people working it need it: owner, tasks, evidence, the
/// decision waiting for a signature and the money. Claims staff only. The
/// clinical file is not in it; that is read by opening the file.
async fn claims_work(State(s): State<Shared>, headers: HeaderMap, Path(claim): Path<String>) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    require(&caller, CLAIMS)?;
    let replies = s.backend.claims(&Question::Work { claim }).await?;
    let view = replies.into_iter().find_map(|r| match r {
        Reply::Work(Some(view)) => Some(view),
        _ => None,
    });
    Ok(Json(json!(view.ok_or_else(Problem::not_found)?)).into_response())
}

/// The claims in payment, by partition. Carries cause analysis.
async fn claims_book(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    claims_staff(&s, &headers, Question::Book).await
}

async fn claims_report(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    claims_staff(&s, &headers, Question::Report).await
}

/// The remittance advice for a payment run: each payee's count and total.
async fn claims_remittance(State(s): State<Shared>, headers: HeaderMap, Path(run): Path<String>) -> ApiResult {
    claims_staff(&s, &headers, Question::Remittance { run }).await
}

async fn end_of_day(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    require(&caller, OPERATIONS)?;
    match s.backend.end_of_day().await {
        Ok(report) => Ok(Json(report).into_response()),
        Err(CloseError::Failed(e)) => Err(Problem::new(StatusCode::CONFLICT, "close_failed", e)),
        Err(e @ CloseError::RunsAsWorkflow) => Err(Problem::new(StatusCode::CONFLICT, "close_runs_as_workflow", e.to_string())),
    }
}

async fn reconciliation(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let caller = s.auth.authenticate(&headers).await?;
    require(&caller, OPERATIONS)?;
    let recon = s.backend.reconciliation().await?;
    Ok(Json(json!({ "business_date": recon.business_date, "clean": recon.breaks.is_empty(), "breaks": recon.breaks })).into_response())
}
