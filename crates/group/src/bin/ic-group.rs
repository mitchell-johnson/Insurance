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

async fn stop_signal() {
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

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
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
    let web = tokio::spawn(serve(listener, router(driver.clone(), &config.api_key), stop_signal()));
    run_passes(driver, config.cycle_every, stop_signal(), |_| {}).await;
    web.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}
