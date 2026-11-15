//! The documents service process. Configuration is in the environment; see
//! `ic_documents::service::Config` and `docs/documents.md`.

use ic_documents::service::{run_from, stop_signal};

#[tokio::main]
async fn main() -> Result<(), String> {
    // JSON log lines, and traces and logs over OTLP (crates/telemetry).
    let _telemetry = ic_telemetry::init("ic-documents")?;
    run_from(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path), stop_signal()).await
}
