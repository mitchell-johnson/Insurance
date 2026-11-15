//! The underwriting effects driver process: carries the module's effects to
//! reinsurance, the core and group administration, and runs the daily sweep.
//! Settings are in the environment; see `ic_underwriting::service::Config`
//! and `docs/underwriting.md`. Any number may run: every receiver applies
//! each delivery once.

use ic_underwriting::service::{serve, stop_signal};

#[tokio::main]
async fn main() -> Result<(), String> {
    // JSON log lines, and traces and logs over OTLP (crates/telemetry).
    let _telemetry = ic_telemetry::init("ic-underwriting")?;
    serve(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path), stop_signal()).await
}
