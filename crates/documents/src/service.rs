//! The documents service: the driver run as a process.
//!
//! It runs the driver's cycle on a timer and listens for the delivery
//! reports Azure Event Grid posts from Communication Services. It holds no
//! state: any number can run, and one that stops is simply restarted.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::channels::{delivery_reports, subscription_validation, Channels, ContactDirectory};
use crate::crypto::KeyWrap;
use crate::driver::{CoreLink, Cycle, Driver, DriverError, EventPublisher, Module};
use crate::vault::ObjectStore;

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
    /// The key Event Grid must present with each delivery.
    pub webhook_key: String,
    pub listen: String,
    pub cycle_every: Duration,
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
            webhook_key,
            listen: get("IC_DOCUMENTS_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            cycle_every: Duration::from_secs(seconds),
        })
    }
}

struct Web<M, S, W, C, D> {
    driver: Arc<Driver<M, S, W, C, D>>,
    /// The hash of the webhook key: comparing hashes takes the same time
    /// whatever was presented.
    key_hash: [u8; 32],
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
    let web = Arc::new(Web {
        driver,
        key_hash: hash(webhook_key.as_bytes()),
    });
    Router::new()
        .route("/v1/delivery-reports", post(reports::<M, S, W, C, D>))
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
