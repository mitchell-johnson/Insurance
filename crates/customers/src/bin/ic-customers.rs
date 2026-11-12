//! The customer master process. Configuration is in the environment; see
//! `ic_customers::config::Config` and `docs/customers.md`.

use ic_azure::credential::AzureCredential;
use ic_customers::config::Config;

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // The handlers are installed before anything else, so a signal during
    // start-up is kept.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| e.to_string())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|e| e.to_string())?;
    let (stopping, stop) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        let _ = stopping.send(true);
    });
    let config = Config::from_lookup(|k| std::env::var(k).ok(), |p| std::fs::read_to_string(p))?;
    let credential = AzureCredential::from_env().map_err(|e| e.to_string())?;
    ic_customers::work::start(config, credential, stop).await
}
