//! The documents service process. Configuration is in the environment; see
//! `ic_documents::service::Config` and `docs/documents.md`.

use ic_documents::service::{run_from, stop_signal};

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    run_from(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path), stop_signal()).await
}
