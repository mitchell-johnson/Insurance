//! `ic-worker`: the end-of-day worker, and the command that starts a run.
//!
//! - `ic-worker` runs a worker until it is told to stop.
//! - `ic-worker close <date> <partitions>` starts the run that closes a
//!   business date, or attaches to it if it is already running, waits for it
//!   and prints its report.
//!
//! - `ic-worker close-due` is what the schedule runs: it closes every business
//!   date whose midnight in New Zealand has passed, oldest first, and does
//!   nothing if none has. See [`ic_workflows::due`].
//!
//! See [`ic_workflows::settings::WorkerSettings`] for the environment.

use chrono::NaiveDate;
use ic_workflows::due::{is_due, ledger_day};
use ic_workflows::settings::WorkerSettings;
use ic_workflows::temporal::{close_day, run_worker};
use ic_workflows::EndOfDayInput;
use temporalio_client::envconfig::LoadClientConfigProfileOptions;
use temporalio_client::{Client, ClientOptions, Connection};

async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}

async fn connect() -> Result<Client, String> {
    let (connection, options) = ClientOptions::load_from_config(LoadClientConfigProfileOptions::default()).map_err(|e| e.to_string())?;
    let connection = Connection::connect(connection).await.map_err(|e| format!("temporal: {e}"))?;
    Client::new(connection, options).map_err(|e| e.to_string())
}

/// `close <date> <partitions>` as an input, or why it is not one.
fn close_input(args: &[String], settings: &WorkerSettings) -> Result<EndOfDayInput, String> {
    let [date, partitions] = args else {
        return Err("usage: ic-worker close <YYYY-MM-DD> <partitions>".to_string());
    };
    Ok(EndOfDayInput {
        closing: NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|e| format!("date: {e}"))?,
        partitions: partitions.parse().map_err(|e| format!("partitions: {e}"))?,
        step_timeout_secs: settings.step_timeout.as_secs(),
    })
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let settings = WorkerSettings::from_lookup(|key| std::env::var(key).ok())?;
    match args.split_first() {
        None => {
            let steps = settings.steps()?;
            let client = connect().await?;
            tracing::info!(task_queue = %settings.task_queue, nodes = ?settings.nodes, "end-of-day worker started");
            run_worker(client, &settings.task_queue, steps, stop_signal()).await
        }
        Some((command, rest)) if command == "close" => {
            let input = close_input(rest, &settings)?;
            let report = close_day(&connect().await?, &settings.task_queue, input).await?;
            println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
            if report.breaks.is_empty() {
                Ok(())
            } else {
                Err(format!("{} reconciliation breaks", report.breaks.len()))
            }
        }
        Some((command, [])) if command == "close-due" => {
            let ledger = ic_raft::ClusterClient::new(settings.nodes.clone()).with_auth(settings.auth.clone());
            let temporal = connect().await?;
            let mut closed = Vec::new();
            loop {
                let day = ledger_day(&ledger, settings.patience).await?;
                if !is_due(day.business_date, chrono::Utc::now()) {
                    println!("{}", serde_json::json!({ "closed": closed, "business_date": day.business_date }));
                    return Ok(());
                }
                let input = EndOfDayInput {
                    closing: day.business_date,
                    partitions: day.partitions,
                    step_timeout_secs: settings.step_timeout.as_secs(),
                };
                let report = close_day(&temporal, &settings.task_queue, input).await?;
                if !report.breaks.is_empty() {
                    // The next day is not started over books that do not reconcile.
                    return Err(format!(
                        "{} closed with {} reconciliation breaks: {:?}",
                        report.closed,
                        report.breaks.len(),
                        report.breaks
                    ));
                }
                closed.push(report.closed);
            }
        }
        Some((other, _)) => Err(format!("unknown command {other:?}; expected no command, `close` or `close-due`")),
    }
}
