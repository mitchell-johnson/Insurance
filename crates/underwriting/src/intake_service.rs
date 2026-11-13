//! `ic-underwriting-intake` as a process: the disclosure intake
//! ([`crate::disclosures`]) over HTTP, for the portal and the new business
//! service.
//!
//! Callers are services with a workload identity; a token is accepted only
//! for this service's audience and only with the app role `new-business`,
//! the caller the module takes disclosures from. The process runs under the
//! clinical identity (`infra/underwriting.bicep`), the one identity that may
//! wrap and unwrap people's keys and write the clinical container.
//!
//! - `POST /v1/disclosures/to-sign`: the answers; returns the hash and the
//!   bytes the life signs. Nothing is stored.
//! - `POST /v1/disclosures`: the answers and the signature; seals the record
//!   and submits the disclosure, and returns where it is sealed.
//! - `POST /v1/disclosures/open`: an underwriter reads a life's
//!   disclosures, through the staff console (app role
//!   `underwriting-console`). The module decides and logs the access first.
//! - `GET /healthz`.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use ic_api::auth::Auth;
use ic_azure::credential::AzureCredential;
use ic_azure::keywrap::{KeyVaultWrapper, KeyWrapper};
use ic_azure::records::{BlobRecords, RecordStore};
use ic_raft::{ClusterAuth, ClusterClient};
use serde_json::json;

use crate::clinical::ClinicalStore;
use crate::disclosures::{Answers, Intake, IntakeError, OpenRequest, Signed, CALLER, CONSOLE};
use crate::driver::Underwriting;
use crate::service::Cluster;

/// Settings of the intake process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeConfig {
    /// The ledger nodes, as `IC_NODES` lists them.
    pub cluster: String,
    pub listen: String,
    pub authority: String,
    pub tenant: String,
    pub audience: String,
    pub blob_endpoint: String,
    pub container: String,
    pub key_vault: String,
    pub wrapping_key: String,
    /// How long one command keeps looking for a leader.
    pub patience: Duration,
}

impl IntakeConfig {
    /// Required: `IC_NODES`, `IC_ENTRA_TENANT_ID`,
    /// `IC_UNDERWRITING_INTAKE_AUDIENCE`, `IC_CLINICAL_BLOB_ENDPOINT`,
    /// `IC_CLINICAL_KEY_VAULT_URL`, `IC_CLINICAL_WRAPPING_KEY`. Defaults:
    /// `IC_UNDERWRITING_INTAKE_LISTEN` (`0.0.0.0:8080`),
    /// `IC_ENTRA_AUTHORITY` (the global Entra ID authority),
    /// `IC_CLINICAL_CONTAINER` (`clinical`).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("set {key}"));
        Ok(IntakeConfig {
            cluster: need("IC_NODES")?,
            listen: get("IC_UNDERWRITING_INTAKE_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            authority: get("IC_ENTRA_AUTHORITY").unwrap_or_else(|| ic_api::auth::ENTRA_AUTHORITY.into()),
            tenant: need("IC_ENTRA_TENANT_ID")?,
            audience: need("IC_UNDERWRITING_INTAKE_AUDIENCE")?,
            blob_endpoint: need("IC_CLINICAL_BLOB_ENDPOINT")?,
            container: get("IC_CLINICAL_CONTAINER").unwrap_or_else(|| "clinical".into()),
            key_vault: need("IC_CLINICAL_KEY_VAULT_URL")?,
            wrapping_key: need("IC_CLINICAL_WRAPPING_KEY")?,
            patience: Duration::from_secs(10),
        })
    }
}

struct App<U, W, R> {
    intake: Intake<U, W, R>,
    auth: Auth,
}

fn error(status: StatusCode, code: &str, message: impl std::fmt::Display) -> Response {
    (status, Json(json!({"code": code, "message": message.to_string()}))).into_response()
}

fn refusal(e: IntakeError) -> Response {
    match &e {
        IntakeError::UnknownProposal(_) | IntakeError::UnknownLife(_) | IntakeError::UnknownRequest(_) => error(StatusCode::NOT_FOUND, "unknown", &e),
        IntakeError::Document(_) => error(StatusCode::UNPROCESSABLE_ENTITY, "document", &e),
        IntakeError::NoRules | IntakeError::NoDateOfBirth => error(StatusCode::CONFLICT, "not_ready", &e),
        IntakeError::Answers(problems) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"code": "answers", "message": e.to_string(), "problems": problems})),
        )
            .into_response(),
        IntakeError::Signature => error(StatusCode::BAD_REQUEST, "signature", &e),
        IntakeError::Refused(r) => error(StatusCode::CONFLICT, &r.code, &r.message),
        IntakeError::Store(_) | IntakeError::Unavailable(_) | IntakeError::NoView => {
            tracing::warn!(error = %e, "disclosure intake could not finish");
            error(StatusCode::SERVICE_UNAVAILABLE, "unavailable", &e)
        }
    }
}

impl<U, W, R> App<U, W, R> {
    async fn allowed(&self, headers: &HeaderMap, role: &str) -> Result<(), Response> {
        match self.auth.authenticate(headers).await {
            Ok(caller) if caller.has_role(role) => Ok(()),
            Ok(_) => Err(error(StatusCode::FORBIDDEN, "forbidden", format!("the caller needs the app role {role}"))),
            Err(e) => Err(error(StatusCode::UNAUTHORIZED, "unauthenticated", e)),
        }
    }
}

async fn to_sign<U: Underwriting, W: KeyWrapper, R: RecordStore>(State(app): State<Arc<App<U, W, R>>>, headers: HeaderMap, Json(a): Json<Answers>) -> Response {
    if let Err(r) = app.allowed(&headers, CALLER).await {
        return r;
    }
    match app.intake.to_sign(&a).await {
        Ok(sign) => Json(sign).into_response(),
        Err(e) => refusal(e),
    }
}

async fn submit<U: Underwriting, W: KeyWrapper, R: RecordStore>(State(app): State<Arc<App<U, W, R>>>, headers: HeaderMap, Json(s): Json<Signed>) -> Response {
    if let Err(r) = app.allowed(&headers, CALLER).await {
        return r;
    }
    match app.intake.submit(&s).await {
        Ok(done) => Json(done).into_response(),
        Err(e) => refusal(e),
    }
}

async fn open<U: Underwriting, W: KeyWrapper, R: RecordStore>(
    State(app): State<Arc<App<U, W, R>>>,
    headers: HeaderMap,
    Json(r): Json<OpenRequest>,
) -> Response {
    if let Err(refused) = app.allowed(&headers, CONSOLE).await {
        return refused;
    }
    match app.intake.open(&r).await {
        Ok(records) => Json(records).into_response(),
        Err(e) => refusal(e),
    }
}

async fn evidence<U: Underwriting, W: KeyWrapper, R: RecordStore>(
    State(app): State<Arc<App<U, W, R>>>,
    headers: HeaderMap,
    Json(a): Json<crate::inbound::EvidenceAnswer>,
) -> Response {
    if let Err(refused) = app.allowed(&headers, crate::inbound::EVIDENCE).await {
        return refused;
    }
    match app.intake.evidence(&a).await {
        Ok(filed) => Json(filed).into_response(),
        Err(e) => refusal(e),
    }
}

async fn delegate_file<U: Underwriting, W: KeyWrapper, R: RecordStore>(
    State(app): State<Arc<App<U, W, R>>>,
    headers: HeaderMap,
    Json(d): Json<crate::inbound::DelegateFile>,
) -> Response {
    if let Err(refused) = app.allowed(&headers, crate::inbound::DELEGATE).await {
        return refused;
    }
    match app.intake.delegate_file(&d).await {
        Ok(filed) => Json(filed).into_response(),
        Err(e) => refusal(e),
    }
}

/// The routes, over any module and store.
pub fn router<U, W, R>(intake: Intake<U, W, R>, auth: Auth) -> Router
where
    U: Underwriting + 'static,
    W: KeyWrapper + 'static,
    R: RecordStore + 'static,
{
    Router::new()
        .route("/v1/disclosures/to-sign", post(to_sign::<U, W, R>))
        .route("/v1/disclosures", post(submit::<U, W, R>))
        .route("/v1/disclosures/open", post(open::<U, W, R>))
        .route("/v1/evidence", post(evidence::<U, W, R>))
        .route("/v1/delegate-files", post(delegate_file::<U, W, R>))
        // A report or a file in base64 is a third larger than the document.
        .layer(axum::extract::DefaultBodyLimit::max(crate::inbound::MOST_BYTES / 3 * 4 + 64 * 1024))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(Arc::new(App { intake, auth }))
}

/// The process: settings and the cluster token from `get` (or the file it
/// names), the clinical identity, then serve until `stop`.
pub async fn serve(
    get: impl Fn(&str) -> Option<String>,
    read: impl Fn(&str) -> std::io::Result<String>,
    stop: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let config = IntakeConfig::from_lookup(&get)?;
    let cluster_auth = ClusterAuth::from_lookup(&get, read)?;
    let credential = AzureCredential::from_lookup(&get).map_err(|e| e.to_string())?;
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(cluster_auth);
    let intake = Intake {
        module: Cluster::new(client, 1, config.patience),
        store: ClinicalStore {
            wrapper: KeyVaultWrapper::new(&config.key_vault, &config.wrapping_key, credential.clone()),
            records: BlobRecords::new(&config.blob_endpoint, &config.container, credential),
        },
        clock: Utc::now,
    };
    let app = router(intake, Auth::entra_at(&config.authority, &config.tenant, &config.audience));
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!(listen = %config.listen, "underwriting intake started");
    axum::serve(listener, app).with_graceful_shutdown(stop).await.map_err(|e| e.to_string())?;
    tracing::info!("underwriting intake stopped");
    Ok(())
}
