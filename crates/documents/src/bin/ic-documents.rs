//! The documents service process. Configuration is in the environment; see
//! `ic_documents::service::Config` and `docs/documents.md`.

use std::sync::Arc;
use std::time::Duration;

use ic_azure::credential::AzureCredential;
use ic_documents::channels::{AzureChannels, HttpDirectory};
use ic_documents::crypto::KeyVaultWrap;
use ic_documents::driver::{ClusterCore, ClusterModule, Driver, EventHubsPublisher};
use ic_documents::service::{router, run_cycles, serve, Config};
use ic_documents::vault::BlobVault;
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
    let config = Config::from_lookup(|key| std::env::var(key).ok())?;
    let credential = AzureCredential::from_env().map_err(|e| e.to_string())?;
    let auth = ClusterAuth::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let partitions = client.partitions(Duration::from_secs(60)).await?;
    let core = ClusterCore::new(client.clone(), partitions, Duration::from_secs(10));
    let driver = Arc::new(Driver {
        module: ClusterModule::new(client, Duration::from_secs(10)),
        objects: BlobVault::new(&config.vault_endpoint, &config.vault_container, credential.clone()),
        wrap: KeyVaultWrap::new(&config.key_vault, &config.key_name, credential.clone()),
        channels: AzureChannels::new(
            &config.communication_endpoint,
            &config.email_from,
            &config.sms_from,
            credential.clone(),
            BlobVault::new(&config.vault_endpoint, &config.print_container, credential.clone()),
        ),
        directory: HttpDirectory::new(&config.customer_master, &config.customer_master_scope, credential.clone()),
        actor: "documents-service".to_string(),
        clock: Arc::new(chrono::Utc::now),
        batch: 100,
    });
    let publisher = EventHubsPublisher::new(&config.event_hubs_namespace, &config.event_hub, credential);
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!(?config, "documents service started");
    let web = tokio::spawn(serve(listener, router(driver.clone(), &config.webhook_key), stop_signal()));
    run_cycles(driver, publisher, Some(core), config.cycle_every, stop_signal(), |_| {}).await;
    web.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}
