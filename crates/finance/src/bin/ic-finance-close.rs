//! The close service process. Configuration is in the environment; see
//! `ic_finance::service::Config` and `docs/finance.md`.

use std::time::Duration;

use ic_azure::blob::BlobContainer;
use ic_azure::credential::AzureCredential;
use ic_finance::driver::{distribution_sources, Driver};
use ic_finance::service::{run_passes, ClusterBooks, ClusterModules, Config};
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
    let driver = Driver {
        books: ClusterBooks::new(client.clone(), Duration::from_secs(10)),
        modules: ClusterModules::new(client),
        roots: BlobContainer::new(&config.roots_endpoint, &config.roots_container, credential.clone()),
        extracts: BlobContainer::new(&config.extracts_endpoint, &config.extracts_container, credential),
        location: config.location(),
        sources: distribution_sources(),
    };
    tracing::info!(?config, "finance close service started");
    run_passes(&driver, config.pass_every, stop_signal(), |_| {}).await;
    Ok(())
}
