//! `ic-api`: the platform API.
//!
//! Configuration is by environment variable; see [`ApiSettings::from_lookup`].
//! With `IC_NODES` set the process is a stateless gateway to the ledger
//! nodes. Without it the process is the insurance in a box: every partition
//! here, on local logs, with in-memory stand-ins for any Azure service that
//! is not configured.

use std::sync::Arc;
use std::time::Duration;

use ic_api::backend::{Backend, Gateway};
use ic_api::engine::Engine;
use ic_api::routes::{router, AppState};
use ic_api::settings::{ApiSettings, Identity, Mode};
use ic_readmodel::scope::AudienceScope;

/// How long a starting gateway waits for a ledger node to answer.
const STARTUP_PATIENCE: Duration = Duration::from_secs(60);

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
    tracing::info!("shutting down");
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // JSON lines on stdout: Container Insights and Application Insights pick them up as they are.
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // The wall clock is read here, to open an empty single-process ledger, and nowhere else.
    let today = chrono::Utc::now().with_timezone(&chrono_tz::Pacific::Auckland).date_naive();
    let settings = ApiSettings::from_lookup(|key| std::env::var(key).ok(), today)?;
    if settings.identity == Identity::Dev {
        tracing::warn!("development identity: callers are trusted by header. Never expose this listener.");
    }
    let backend = match &settings.mode {
        Mode::Single(config) => {
            let engine = Engine::start(config)?;
            tracing::info!(partitions = config.partitions, business_date = ?engine.cluster.business_date(), durable = config.data_dir.is_some(), "insurance core started in one process");
            Backend::single(engine)
        }
        Mode::Cluster(cluster) => {
            let gateway = Gateway::new(cluster);
            gateway.verify(STARTUP_PATIENCE).await?;
            tracing::info!(
                partitions = cluster.partitions,
                nodes = cluster.nodes.len(),
                "gateway to the ledger nodes started"
            );
            Backend::cluster(gateway)
        }
    };
    let state = Arc::new(AppState {
        backend,
        auth: settings.identity.auth(),
        scope: AudienceScope::shipped().map_err(|e| e.to_string())?,
    });
    let listen = settings.listen;
    let listener = tokio::net::TcpListener::bind(listen).await.map_err(|e| format!("bind {listen}: {e}"))?;
    tracing::info!(%listen, "listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())
}
