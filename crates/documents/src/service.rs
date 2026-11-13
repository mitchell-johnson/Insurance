//! The documents service: the driver run as a process.
//!
//! It runs the driver's cycle on a timer, listens for the delivery reports
//! Azure Event Grid posts from Communication Services, and, where Entra ID
//! is configured, hands out documents as PDFs to the portal and to staff.
//! It holds no state: any number can run, and one that stops is simply
//! restarted.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use ic_azure::credential::AzureCredential;
use ic_raft::{ClusterAuth, ClusterClient};

use crate::auth::{self, AuthError, Entra};
use crate::channels::{delivery_reports, subscription_validation, AzureChannels, Channels, ContactDirectory, HttpDirectory};
use crate::complaint_letters::{run_letters, LetterIssuer, LetterTemplates};
use crate::complaints_relay::{run_relay, ClusterComplaints, ClusterLicences, Relay};
use crate::crypto::{KeyVaultWrap, KeyWrap};
use crate::driver::{ClusterCore, ClusterModule, CoreLink, Cycle, Driver, DriverError, EventHubsPublisher, EventPublisher, Module};
use crate::kinds::DataClass;
use crate::pdf::Fonts;
use crate::requested_letters::run_requested_letters;
use crate::vault::{BlobVault, ObjectStore, VaultError};

/// Where everything is. Read from the environment; nothing here is secret
/// except the webhook key, which comes from a mounted Key Vault secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    /// The ledger nodes, `host:port` separated by commas.
    pub cluster: String,
    pub vault_endpoint: String,
    pub vault_container: String,
    pub print_container: String,
    pub key_vault: String,
    pub key_name: String,
    pub communication_endpoint: String,
    pub email_from: String,
    pub sms_from: String,
    pub customer_master: String,
    pub customer_master_scope: String,
    pub event_hubs_namespace: String,
    pub event_hub: String,
    /// The hub the complaints module's events go to.
    pub complaints_event_hub: String,
    /// Who owns a conduct case a licence event opens.
    pub conduct_officer: String,
    /// The key Event Grid must present with each delivery.
    pub webhook_key: String,
    pub listen: String,
    pub cycle_every: Duration,
    /// How long start-up waits for a ledger node to answer.
    pub cluster_wait: Duration,
    /// Entra ID for the download route: authority, tenant and this
    /// service's app ID URI. None leaves the route off.
    pub entra: Option<EntraConfig>,
    /// The insurer's typeface, regular and bold TrueType files. None sets
    /// documents in the bundled Liberation Sans.
    pub fonts: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntraConfig {
    pub authority: String,
    pub tenant: String,
    pub audience: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the webhook key.
        write!(f, "Config(cluster {}, listen {})", self.cluster, self.listen)
    }
}

pub const MIN_WEBHOOK_KEY: usize = 32;

impl Config {
    /// Read through `get`, which is `std::env::var` in production. Every
    /// setting is required unless it has a default: a service that started
    /// without its vault would fail at the first document, not at start.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("{key} is not set"));
        let webhook_key = need("IC_DOCUMENTS_WEBHOOK_KEY")?;
        if webhook_key.len() < MIN_WEBHOOK_KEY {
            return Err(format!("IC_DOCUMENTS_WEBHOOK_KEY must be at least {MIN_WEBHOOK_KEY} characters"));
        }
        let seconds = match get("IC_DOCUMENTS_CYCLE_SECONDS") {
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|s| *s > 0)
                .ok_or("IC_DOCUMENTS_CYCLE_SECONDS must be a whole number of seconds above zero")?,
            None => 15,
        };
        let entra = match (get("IC_ENTRA_TENANT"), get("IC_DOCUMENTS_AUDIENCE")) {
            (Some(tenant), Some(audience)) => Some(EntraConfig {
                authority: get("IC_ENTRA_AUTHORITY").unwrap_or_else(|| auth::ENTRA_AUTHORITY.into()),
                tenant,
                audience,
            }),
            (None, None) => None,
            _ => return Err("IC_ENTRA_TENANT and IC_DOCUMENTS_AUDIENCE are set together or not at all".into()),
        };
        let fonts = match (get("IC_DOCUMENTS_FONT_REGULAR"), get("IC_DOCUMENTS_FONT_BOLD")) {
            (Some(regular), Some(bold)) => Some((regular, bold)),
            (None, None) => None,
            _ => return Err("IC_DOCUMENTS_FONT_REGULAR and IC_DOCUMENTS_FONT_BOLD are set together or not at all".into()),
        };
        Ok(Self {
            cluster: need("IC_CLUSTER_NODES")?,
            vault_endpoint: need("IC_DOCUMENTS_VAULT_ENDPOINT")?,
            vault_container: get("IC_DOCUMENTS_VAULT_CONTAINER").unwrap_or_else(|| "documents-vault".into()),
            print_container: get("IC_DOCUMENTS_PRINT_CONTAINER").unwrap_or_else(|| "print-outbox".into()),
            key_vault: need("IC_DOCUMENTS_KEY_VAULT")?,
            key_name: get("IC_DOCUMENTS_KEY_NAME").unwrap_or_else(|| "documents-kek".into()),
            communication_endpoint: need("IC_COMMUNICATION_ENDPOINT")?,
            email_from: need("IC_DOCUMENTS_EMAIL_FROM")?,
            sms_from: need("IC_DOCUMENTS_SMS_FROM")?,
            customer_master: need("IC_CUSTOMER_MASTER_URL")?,
            customer_master_scope: need("IC_CUSTOMER_MASTER_SCOPE")?,
            event_hubs_namespace: need("IC_EVENT_HUBS_NAMESPACE")?,
            event_hub: get("IC_DOCUMENTS_EVENT_HUB").unwrap_or_else(|| "documents-events".into()),
            complaints_event_hub: get("IC_COMPLAINTS_EVENT_HUB").unwrap_or_else(|| "complaints-events".into()),
            conduct_officer: get("IC_CONDUCT_OFFICER").unwrap_or_else(|| "conduct-officer".into()),
            webhook_key,
            listen: get("IC_DOCUMENTS_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            cycle_every: Duration::from_secs(seconds),
            cluster_wait: Duration::from_secs(60),
            entra,
            fonts,
        })
    }
}

/// Read the insurer's typeface, or the bundled one when none is set.
pub fn load_fonts(paths: Option<&(String, String)>) -> Result<Fonts, String> {
    let Some((regular, bold)) = paths else {
        return Ok(Fonts::bundled());
    };
    let read = |path: &str| std::fs::read(path).map_err(|e| format!("font {path}: {e}"));
    Fonts::new(read(regular)?, read(bold)?).map_err(|e| format!("the typeface cannot be used: {e}"))
}

/// What the download route needs: who may ask, and the typeface to set
/// documents in.
#[derive(Clone)]
pub struct Downloads {
    pub entra: Entra,
    pub fonts: Fonts,
}

struct Web<M, S, W, C, D> {
    driver: Arc<Driver<M, S, W, C, D>>,
    /// The hash of the webhook key: comparing hashes takes the same time
    /// whatever was presented.
    key_hash: [u8; 32],
    downloads: Option<Downloads>,
}

fn hash(text: &[u8]) -> [u8; 32] {
    Sha256::digest(text).into()
}

async fn reports<M, S, W, C, D>(State(web): State<Arc<Web<M, S, W, C, D>>>, headers: HeaderMap, body: Bytes) -> (StatusCode, Json<Value>)
where
    M: Module + 'static,
    S: ObjectStore + 'static,
    W: KeyWrap + 'static,
    C: Channels + 'static,
    D: ContactDirectory + 'static,
{
    let presented = headers.get("x-webhook-key").map(|v| hash(v.as_bytes()));
    if presented != Some(web.key_hash) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "the webhook key is missing or wrong" })));
    }
    // Event Grid proves the endpoint is ours by asking for a code back.
    if let Some(code) = subscription_validation(&body) {
        return (StatusCode::OK, Json(json!({ "validationResponse": code })));
    }
    let reports = match delivery_reports(&body) {
        Ok(reports) => reports,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))),
    };
    let mut recorded = 0;
    for report in reports {
        match web.driver.report(report).await {
            Ok(true) => recorded += 1,
            Ok(false) => {}
            // Not recorded: answer so that Event Grid delivers it again.
            Err(e) => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": e.to_string() }))),
        }
    }
    (StatusCode::OK, Json(json!({ "recorded": recorded })))
}

fn refuse(status: StatusCode, error: impl Into<String>) -> Response {
    (status, Json(json!({ "error": error.into() }))).into_response()
}

/// `GET /v1/documents/{id}/pdf`: the document as issued, as a PDF.
///
/// The portal asks as a customer, named in `x-ic-party`, who must be the
/// document's owner or one of its recipients; any other document is not
/// found, so that the portal cannot learn what exists. Staff ask as a
/// named person (`x-ic-actor`) for a stated purpose (`x-ic-purpose`), and
/// never for clinical documents, which are read where the claims team
/// keeps them. Each download is logged, never its contents.
async fn download<M, S, W, C, D>(State(web): State<Arc<Web<M, S, W, C, D>>>, Path(id): Path<String>, headers: HeaderMap) -> Response
where
    M: Module + 'static,
    S: ObjectStore + 'static,
    W: KeyWrap + 'static,
    C: Channels + 'static,
    D: ContactDirectory + 'static,
{
    let Some(downloads) = &web.downloads else {
        return refuse(StatusCode::NOT_FOUND, "downloads are not configured");
    };
    let caller = match downloads.entra.authenticate(&headers).await {
        Ok(caller) => caller,
        Err(e @ AuthError::Keys(_)) => return refuse(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
        Err(e) => return refuse(StatusCode::UNAUTHORIZED, e.to_string()),
    };
    let party = auth::header(&headers, "x-ic-party");
    let actor = auth::header(&headers, "x-ic-actor");
    let purpose = auth::header(&headers, "x-ic-purpose");
    match caller.role.as_str() {
        "portal" if party.is_none() => return refuse(StatusCode::BAD_REQUEST, "x-ic-party names the customer asking"),
        "staff" if actor.is_none() || purpose.is_none() => return refuse(StatusCode::BAD_REQUEST, "x-ic-actor and x-ic-purpose name who is asking and why"),
        "portal" | "staff" => {}
        other => return refuse(StatusCode::FORBIDDEN, format!("the role {other} may not download documents")),
    }
    let document = match web.driver.document(&id).await {
        Ok(document) => document,
        Err(DriverError::NotFound(_)) => return refuse(StatusCode::NOT_FOUND, "no such document"),
        Err(e) => return refuse(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    };
    if let Some(party) = party.filter(|_| caller.role == "portal") {
        let theirs = document.owner.0 == party || document.deliveries.iter().any(|d| d.recipient.party.0 == party);
        if !theirs {
            return refuse(StatusCode::NOT_FOUND, "no such document");
        }
    }
    if caller.role == "staff" && document.data_class == DataClass::Clinical {
        return refuse(StatusCode::FORBIDDEN, "clinical documents are not downloaded here");
    }
    if document.destroyed_on.is_some() {
        return refuse(StatusCode::GONE, "the document has been destroyed");
    }
    let bytes = match web.driver.pdf(&id, &downloads.fonts).await {
        Ok(bytes) => bytes,
        Err(DriverError::Vault(VaultError::Destroyed)) => return refuse(StatusCode::GONE, "the document has been destroyed"),
        Err(e @ (DriverError::Pdf(_) | DriverError::Template(_) | DriverError::Crypto(_))) => {
            tracing::error!(document = %id, error = %e, "a document could not be set as a PDF");
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, "the document could not be set as a PDF");
        }
        Err(e) => return refuse(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    };
    tracing::info!(
        document = %id,
        role = %caller.role,
        subject = %caller.subject,
        party = party.unwrap_or(""),
        actor = actor.unwrap_or(""),
        purpose = purpose.unwrap_or(""),
        "document downloaded"
    );
    let file: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{file}.pdf\"")).unwrap_or(HeaderValue::from_static("attachment"));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("application/pdf")),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        bytes,
    )
        .into_response()
}

/// The service's routes: `POST /v1/delivery-reports` for Event Grid, and
/// `GET /healthz`.
pub fn router<M, S, W, C, D>(driver: Arc<Driver<M, S, W, C, D>>, webhook_key: &str) -> Router
where
    M: Module + 'static,
    S: ObjectStore + 'static,
    W: KeyWrap + 'static,
    C: Channels + 'static,
    D: ContactDirectory + 'static,
{
    router_with(driver, webhook_key, None)
}

/// The routes, with `GET /v1/documents/{id}/pdf` when `downloads` is given.
pub fn router_with<M, S, W, C, D>(driver: Arc<Driver<M, S, W, C, D>>, webhook_key: &str, downloads: Option<Downloads>) -> Router
where
    M: Module + 'static,
    S: ObjectStore + 'static,
    W: KeyWrap + 'static,
    C: Channels + 'static,
    D: ContactDirectory + 'static,
{
    let web = Arc::new(Web {
        driver,
        key_hash: hash(webhook_key.as_bytes()),
        downloads,
    });
    Router::new()
        .route("/v1/delivery-reports", post(reports::<M, S, W, C, D>))
        .route("/v1/documents/{id}/pdf", get(download::<M, S, W, C, D>))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(web)
}

/// Run the driver's cycle every `every` until `stop` completes. A cycle
/// that could not do everything says so in the log and the next one tries
/// again; nothing here ends the loop but `stop`.
pub async fn run_cycles<M, S, W, C, D, P, L>(
    driver: Arc<Driver<M, S, W, C, D>>,
    publisher: P,
    core: Option<L>,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&Cycle),
) where
    M: Module,
    S: ObjectStore,
    W: KeyWrap,
    C: Channels,
    D: ContactDirectory,
    P: EventPublisher,
    L: CoreLink,
{
    tokio::pin!(stop);
    loop {
        let cycle = driver.cycle(&publisher, core.as_ref()).await;
        if cycle.errors.is_empty() {
            tracing::debug!(sent = cycle.sends.sent, published = cycle.published, "documents cycle");
        } else {
            tracing::warn!(errors = ?cycle.errors, "a documents cycle could not do everything");
        }
        seen(&cycle);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}

/// A link to the core for a deployment whose core has no notice commands
/// yet: nothing is carried, and the instructions wait in the module.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCore;

impl CoreLink for NoCore {
    async fn instruct(&self, _id: &str, _instruction: &crate::model::CoreInstruction) -> Result<(), String> {
        Err("the core has no notice commands yet".to_string())
    }
}

/// Serve the routes on `listener` until `stop` completes.
pub async fn serve(listener: tokio::net::TcpListener, routes: Router, stop: impl Future<Output = ()> + Send + 'static) -> Result<(), DriverError> {
    axum::serve(listener, routes)
        .with_graceful_shutdown(stop)
        .await
        .map_err(|e| DriverError::Unavailable(e.to_string()))
}

/// Completes on Ctrl-C or SIGTERM, which is how Kubernetes asks a pod to stop.
pub async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

/// The whole service: the driver's cycle, the complaints relay and the
/// delivery-report webhook, against the cluster and Azure, until `stop`
/// completes. Fails at once if no ledger node answers or the listen
/// address cannot be bound, so a bad deployment is seen at start.
pub async fn run(config: Config, credential: AzureCredential, auth: ClusterAuth, stop: impl Future<Output = ()> + Send + 'static) -> Result<(), String> {
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let partitions = client.partitions(config.cluster_wait).await?;
    let patience = Duration::from_secs(10);
    let fonts = load_fonts(config.fonts.as_ref())?;
    let downloads = config.entra.as_ref().map(|e| Downloads {
        entra: Entra::new(&e.authority, &e.tenant, &e.audience),
        fonts: fonts.clone(),
    });
    let driver = Arc::new(Driver {
        module: ClusterModule::new(client.clone(), patience),
        objects: BlobVault::new(&config.vault_endpoint, &config.vault_container, credential.clone()),
        wrap: KeyVaultWrap::new(&config.key_vault, &config.key_name, credential.clone()),
        channels: AzureChannels::new(
            &config.communication_endpoint,
            &config.email_from,
            &config.sms_from,
            credential.clone(),
            BlobVault::new(&config.vault_endpoint, &config.print_container, credential.clone()),
        )
        .with_fonts(fonts),
        directory: HttpDirectory::new(&config.customer_master, &config.customer_master_scope, credential.clone()),
        actor: "documents-service".to_string(),
        clock: Arc::new(chrono::Utc::now),
        batch: 100,
    });
    let relay = Relay {
        module: ClusterComplaints::new(client.clone(), patience),
        actor: "documents-service".to_string(),
        conduct_officer: config.conduct_officer.clone(),
        clock: Arc::new(chrono::Utc::now),
        batch: 100,
    };
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!(?config, "documents service started");

    // One stop for the three tasks.
    let (stopping, stopped) = tokio::sync::watch::channel(false);
    let until_stopped = move || {
        let mut stopped = stopped.clone();
        async move {
            let _ = stopped.wait_for(|s| *s).await;
        }
    };
    tokio::spawn(async move {
        stop.await;
        let _ = stopping.send(true);
    });
    let web = tokio::spawn(serve(listener, router_with(driver.clone(), &config.webhook_key, downloads), until_stopped()));
    let complaints = tokio::spawn(run_relay(
        relay,
        EventHubsPublisher::new(&config.event_hubs_namespace, &config.complaints_event_hub, credential.clone()),
        ClusterLicences::new(client.clone(), partitions),
        config.cycle_every,
        until_stopped(),
        |_| {},
    ));
    let letters = tokio::spawn(run_letters(
        LetterIssuer {
            complaints: ClusterComplaints::new(client.clone(), patience),
            letters: driver.clone(),
            templates: LetterTemplates::default(),
            actor: "documents-service".to_string(),
            clock: Arc::new(chrono::Utc::now),
            batch: 100,
        },
        config.cycle_every,
        until_stopped(),
        |_| {},
    ));
    let requested = tokio::spawn(run_requested_letters(driver.clone(), config.cycle_every, until_stopped(), |_| {}));
    let publisher = EventHubsPublisher::new(&config.event_hubs_namespace, &config.event_hub, credential);
    let core = ClusterCore::new(client, partitions, patience);
    run_cycles(driver, publisher, Some(core), config.cycle_every, until_stopped(), |_| {}).await;
    complaints.await.map_err(|e| e.to_string())?;
    letters.await.map_err(|e| e.to_string())?;
    requested.await.map_err(|e| e.to_string())?;
    web.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}

/// The service from its environment: `get` is `std::env::var` and `read`
/// is `std::fs::read_to_string` in production. Anything missing or
/// contradictory is an error before anything is started.
pub async fn run_from(
    get: impl Fn(&str) -> Option<String>,
    read: impl Fn(&str) -> std::io::Result<String>,
    stop: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    // The webhook key is a mounted Key Vault secret in production, so that
    // it never sits in a manifest or a Kubernetes Secret.
    let key_from_file = match get("IC_DOCUMENTS_WEBHOOK_KEY_FILE") {
        Some(path) => Some(read(&path).map_err(|e| format!("IC_DOCUMENTS_WEBHOOK_KEY_FILE {path}: {e}"))?),
        None => None,
    };
    let config = Config::from_lookup(|key| match (key, &key_from_file) {
        ("IC_DOCUMENTS_WEBHOOK_KEY", Some(from_file)) => Some(from_file.clone()),
        _ => get(key),
    })?;
    let credential = AzureCredential::from_lookup(&get).map_err(|e| e.to_string())?;
    let auth = ClusterAuth::from_lookup(&get, &read)?;
    run(config, credential, auth, stop).await
}
