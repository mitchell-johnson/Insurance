//! The documents service process. Configuration is in the environment; see
//! `ic_documents::service::Config` and `docs/documents.md`.

use ic_azure::credential::AzureCredential;
use ic_documents::service::{run, stop_signal, Config};
use ic_raft::ClusterAuth;

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_lookup(|key| std::env::var(key).ok())?;
    let credential = AzureCredential::from_env().map_err(|e| e.to_string())?;
    let auth = ClusterAuth::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    run(config, credential, auth, stop_signal()).await
}
