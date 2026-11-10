//! `ic-api`: the platform API and the single-process host.
//!
//! Configuration is by environment variable. With no Azure endpoints set the
//! process is the insurance in a box: local logs, in-memory stand-ins for
//! Blob Storage, Key Vault and Event Hubs.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::NaiveDate;
use ic_api::auth::Auth;
use ic_api::engine::{AzureConfig, Engine, EngineConfig};
use ic_api::routes::{router, AppState};
use ic_readmodel::scope::AudienceScope;
use tokio::sync::Mutex;

fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn config() -> Result<(SocketAddr, EngineConfig, Auth), String> {
    let listen = var("IC_LISTEN")
        .unwrap_or_else(|| "0.0.0.0:8080".to_string())
        .parse()
        .map_err(|e| format!("IC_LISTEN: {e}"))?;
    let partitions = match var("IC_PARTITIONS") {
        Some(p) => p.parse().map_err(|e| format!("IC_PARTITIONS: {e}"))?,
        None => ic_ledger::PARTITION_COUNT,
    };
    if partitions == 0 || partitions > ic_ledger::PARTITION_COUNT {
        return Err(format!("IC_PARTITIONS must be between 1 and {}", ic_ledger::PARTITION_COUNT));
    }
    // The business date is set once, on an empty cluster, and from then on
    // moves only by the cutoff. The wall clock is read here and nowhere else.
    let opening_date = match var("IC_OPENING_DATE") {
        Some(d) => NaiveDate::parse_from_str(&d, "%Y-%m-%d").map_err(|e| format!("IC_OPENING_DATE: {e}"))?,
        None => chrono::Utc::now().with_timezone(&chrono_tz::Pacific::Auckland).date_naive(),
    };
    let azure = AzureConfig {
        blob_endpoint: var("IC_BLOB_ENDPOINT"),
        merkle_container: var("IC_MERKLE_CONTAINER").unwrap_or_else(|| "merkle-roots".to_string()),
        key_vault_url: var("IC_KEY_VAULT_URL"),
        merkle_key: var("IC_MERKLE_KEY").unwrap_or_else(|| "merkle-root-signing".to_string()),
        event_hubs_endpoint: var("IC_EVENT_HUBS_ENDPOINT"),
        event_hub: var("IC_EVENT_HUB").unwrap_or_else(|| "policy-events".to_string()),
    };
    let on_azure = azure.blob_endpoint.is_some() || azure.key_vault_url.is_some() || azure.event_hubs_endpoint.is_some();
    let auth = match (var("IC_AUTH").as_deref(), var("IC_ENTRA_TENANT_ID"), var("IC_ENTRA_AUDIENCE")) {
        (Some("dev"), _, _) if on_azure => return Err("IC_AUTH=dev is refused when any Azure endpoint is configured".to_string()),
        (Some("dev"), _, _) => Auth::Dev,
        (None | Some("entra"), Some(tenant), Some(audience)) => Auth::entra(&tenant, &audience),
        _ => return Err("set IC_ENTRA_TENANT_ID and IC_ENTRA_AUDIENCE, or IC_AUTH=dev for the insurance in a box".to_string()),
    };
    let engine = EngineConfig {
        partitions,
        data_dir: var("IC_DATA_DIR").map(PathBuf::from),
        opening_date,
        load_shipped_config: var("IC_LOAD_SHIPPED_CONFIG").is_none_or(|v| v != "false"),
        azure,
    };
    Ok((listen, engine, auth))
}

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
    let (listen, engine_cfg, auth) = config()?;
    if matches!(auth, Auth::Dev) {
        tracing::warn!("development identity: callers are trusted by header. Never expose this listener.");
    }
    let engine = Engine::start(&engine_cfg)?;
    tracing::info!(partitions = engine_cfg.partitions, business_date = ?engine.cluster.business_date(), durable = engine_cfg.data_dir.is_some(), "insurance core started");
    let state = Arc::new(AppState {
        engine: Mutex::new(engine),
        auth,
        scope: AudienceScope::shipped().map_err(|e| e.to_string())?,
    });
    let listener = tokio::net::TcpListener::bind(listen).await.map_err(|e| format!("bind {listen}: {e}"))?;
    tracing::info!(%listen, "listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())
}
