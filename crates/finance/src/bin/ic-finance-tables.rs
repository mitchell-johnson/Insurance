//! Validate a rate table against its schema, and optionally record the
//! passed validation on the core so that the close finds it when the table
//! is activated. Run by the step that activates a table. Prints the report
//! as JSON and fails where the table has a blocking finding. With
//! `--record`, the cluster is found as the close service finds it
//! (`IC_CLUSTER_NODES` and the cluster credentials in the environment).

use std::time::Duration;

use ic_finance::service::ClusterModules;
use ic_raft::{ClusterAuth, ClusterClient};

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = ic_finance::tables::tool(&args, |file| std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}")))?;
    println!("{}", serde_json::to_string_pretty(&run.report).map_err(|e| e.to_string())?);
    // Refuse a table that did not pass before anything is sent.
    run.report.record("nobody")?;
    let Some(by) = run.record_as else {
        return Ok(());
    };
    let nodes = std::env::var("IC_CLUSTER_NODES").map_err(|_| "IC_CLUSTER_NODES is not set".to_string())?;
    let auth = ClusterAuth::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    let client = ClusterClient::from_list(&nodes)?.with_auth(auth);
    let partitions = client.partitions(Duration::from_secs(60)).await?;
    ClusterModules::new(client, partitions)
        .record_validation(&run.report, &by, Duration::from_secs(30))
        .await
        .map_err(|e| e.to_string())
}
