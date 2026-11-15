//! The close service process. Configuration is in the environment; see
//! `ic_finance::service::Config` and `docs/finance.md`.

use std::sync::Arc;
use std::time::Duration;

use ic_azure::blob::BlobContainer;
use ic_azure::credential::AzureCredential;
use ic_finance::driver::{core_sources, distribution_sources, finance_sources, payments_sources, reinsurance_sources, unit_pricing_sources, Driver};
use ic_finance::service::{run_on_temporal, run_passes, ClusterBooks, ClusterModules, Config};
use ic_raft::{ClusterAuth, ClusterClient};
use temporalio_client::envconfig::LoadClientConfigProfileOptions;
use temporalio_client::{Client, ClientOptions, Connection};

/// The stop signal: SIGTERM (a pod being replaced) or SIGINT. Both handlers are
/// installed when this is called, not when the future is first polled. The pass loop
/// polls its stop future only between passes, so a lazily installed handler left the
/// whole first pass exposed: a SIGTERM then took the default action and killed the
/// process mid-pass instead of letting the pass finish and the process exit cleanly.
fn stop_signal() -> Result<impl std::future::Future<Output = ()>, String> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut terminate = signal(SignalKind::terminate()).map_err(|e| format!("SIGTERM handler: {e}"))?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|e| format!("SIGINT handler: {e}"))?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // JSON log lines, and traces and logs over OTLP (crates/telemetry).
    let _telemetry = ic_telemetry::init("ic-finance-close")?;
    let stop = stop_signal()?;
    let config = Config::from_lookup(|key| std::env::var(key).ok())?;
    let credential = AzureCredential::from_env().map_err(|e| e.to_string())?;
    let auth = ClusterAuth::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let partitions = client.partitions(Duration::from_secs(60)).await?;
    let driver = Driver {
        books: ClusterBooks::new(client.clone(), Duration::from_secs(10)),
        modules: ClusterModules::new(client, partitions),
        roots: BlobContainer::new(&config.roots_endpoint, &config.roots_container, credential.clone()),
        extracts: BlobContainer::new(&config.extracts_endpoint, &config.extracts_container, credential),
        location: config.location(),
        sources: distribution_sources()
            .into_iter()
            .chain(payments_sources())
            .chain(reinsurance_sources())
            .chain(core_sources())
            .chain(unit_pricing_sources())
            .chain(finance_sources())
            .collect(),
    };
    tracing::info!(?config, "finance close service started");
    if !config.temporal {
        run_passes(&driver, config.pass_every, stop, |_| {}).await;
        return Ok(());
    }
    // The connection is read by the SDK from TEMPORAL_ADDRESS,
    // TEMPORAL_NAMESPACE and the other TEMPORAL_* variables.
    let (connection, options) = ClientOptions::load_from_config(LoadClientConfigProfileOptions::default()).map_err(|e| e.to_string())?;
    let connection = Connection::connect(connection).await.map_err(|e| format!("temporal: {e}"))?;
    let temporal = Client::new(connection, options).map_err(|e| e.to_string())?;
    run_on_temporal(temporal, Arc::new(driver), &config, stop, |_| {}).await
}
