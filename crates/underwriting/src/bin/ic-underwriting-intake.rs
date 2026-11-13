//! The disclosure intake process: takes a life's answers from the portal or
//! the new business service, seals them in the clinical store and submits
//! the disclosure to the module. Settings are in the environment; see
//! `ic_underwriting::intake_service::IntakeConfig` and `docs/underwriting.md`.

use ic_underwriting::intake_service::serve;
use ic_underwriting::service::stop_signal;

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    serve(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path), stop_signal()).await
}
