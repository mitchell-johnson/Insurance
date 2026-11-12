//! The group service process. Configuration is in the environment; see
//! `ic_group::service::Config` and `docs/group.md`.

use std::sync::Arc;
use std::time::Duration;

use ic_azure::blob::BlobContainer;
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::EventHubsSink;
use ic_group::adapters::{ClusterCore, ClusterModule, ClusterPeers, QuarantineBlobs};
use ic_group::driver::Driver;
use ic_group::service::{router, run_passes, serve, Config, HttpCustomerMaster};
use ic_raft::{ClusterAuth, ClusterClient};

/// Completes on SIGTERM or ctrl-c. The handlers are installed when this is
/// called, not when the result is first awaited, so a signal that arrives
/// while the service is busy starting or in the middle of a pass is kept.
fn stop_signal() -> Result<tokio::sync::watch::Receiver<bool>, String> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| format!("cannot listen for SIGTERM: {e}"))?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|e| format!("cannot listen for SIGINT: {e}"))?;
    let (stopped, stop) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        let _ = stopped.send(true);
    });
    Ok(stop)
}

async fn stopped(mut stop: tokio::sync::watch::Receiver<bool>) {
    // An error means the sender is gone, which only happens at exit.
    let _ = stop.wait_for(|stopped| *stopped).await;
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let stop = stop_signal()?;
    let config = Config::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    let credential = AzureCredential::from_env().map_err(|e| e.to_string())?;
    let auth = ClusterAuth::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let partitions = client.partitions(Duration::from_secs(60)).await?;
    let patience = Duration::from_secs(10);
    let driver = Arc::new(Driver::new(
        ClusterModule::new(client.clone(), patience),
        ClusterCore::new(client.clone(), partitions, patience),
        ClusterPeers::new(
            client,
            patience,
            &config.reinsurance_module,
            &config.underwriting_module,
            EventHubsSink::new(&config.event_hubs_namespace, &config.event_hub, credential.clone()),
        ),
        HttpCustomerMaster::new(&config.customer_master, &config.customer_master_scope, credential.clone()),
        QuarantineBlobs::new(BlobContainer::new(&config.quarantine_endpoint, &config.quarantine_container, credential)),
        &config.fingerprint_salt,
    ));
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!(?config, "group service started");
    let web = tokio::spawn(serve(listener, router(driver.clone(), &config.api_key), stopped(stop.clone())));
    run_passes(driver, config.cycle_every, stopped(stop), |_| {}).await;
    web.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}
