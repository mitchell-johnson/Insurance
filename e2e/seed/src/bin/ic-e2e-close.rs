//! Closes the ledger's current business date, as the end-of-day workflow
//! does, and prints the report.
//!
//! The deployed system runs the close as a Temporal workflow (`ic-worker`).
//! The end-to-end stack has no Temporal service, so this runs the workflow's
//! own function, `ic_workflows::run_end_of_day`, over the same real steps
//! against the running cluster. What it does not exercise is Temporal itself:
//! the durable retry of a close that is interrupted part way.
//!
//! Settings are the worker's own (`IC_NODES`, `IC_CLUSTER_TOKEN`,
//! `IC_ROOT_DIR`); see `ic_workflows::settings::WorkerSettings`.

use ic_workflows::due::ledger_day;
use ic_workflows::settings::WorkerSettings;
use ic_workflows::{run_end_of_day, EndOfDayInput};

#[tokio::main]
async fn main() {
    if let Err(e) = close().await {
        eprintln!("ic-e2e-close: {e}");
        std::process::exit(1);
    }
}

async fn close() -> Result<(), String> {
    let settings = WorkerSettings::from_lookup(|key| std::env::var(key).ok())?;
    let ledger = ic_raft::ClusterClient::new(settings.nodes.clone()).with_auth(settings.auth.clone());
    let day = ledger_day(&ledger, settings.patience).await?;
    let input = EndOfDayInput {
        closing: day.business_date,
        partitions: day.partitions,
        step_timeout_secs: settings.step_timeout.as_secs(),
    };
    let steps = settings.steps()?;
    let report = run_end_of_day(&steps, &input).await.map_err(|e| format!("{e:?}"))?;
    println!("{}", serde_json::to_string(&report).map_err(|e| e.to_string())?);
    Ok(())
}
