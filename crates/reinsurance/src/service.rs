//! The reinsurance driver as a process: its settings, the signals that stop
//! it, and the loop `ic-reinsurance` runs.

use std::future::Future;
use std::time::Duration;

use ic_azure::blob::{BlobContainer, WormStore};
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::EventHubsSink;
use ic_raft::{ClusterAuth, ClusterClient};
use tokio::signal::unix::{signal, SignalKind};

use crate::carriers::{Carriers, StaffFeed};
use crate::driver::{ClusterAudiences, ClusterCore, ClusterModule, Driver};

/// Settings of the driver process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The ledger nodes, as `IC_NODES` lists them.
    pub cluster: String,
    /// The pause between delivery passes.
    pub cycle_every: Duration,
    /// How long one command keeps looking for a leader.
    pub patience: Duration,
    /// Instructions read from the outbox at a time.
    pub batch: usize,
    /// The storage account statements, listings and the release log are
    /// filed in, and the write-once container in it.
    pub records_endpoint: String,
    pub records_container: String,
    /// The Event Hubs namespace and the hub notices for staff go to.
    pub event_hubs: String,
    pub event_hub: String,
}

impl Config {
    /// Read the settings. `IC_NODES`, `IC_BLOB_ENDPOINT` and
    /// `IC_EVENT_HUBS_ENDPOINT` are required: there is no mode in which
    /// statements are not filed or exceptions are told to nobody. The rest
    /// have defaults: `IC_REINSURANCE_CYCLE_MS` (2000),
    /// `IC_REINSURANCE_PATIENCE_MS` (10000), `IC_REINSURANCE_BATCH` (100),
    /// `IC_REINSURANCE_RECORDS_CONTAINER` (`reinsurance-records`) and
    /// `IC_REINSURANCE_EVENT_HUB` (`reinsurance-events`).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let number = |key: &str, default: u64| match get(key) {
            None => Ok(default),
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{key} must be a whole number above zero, not {text:?}")),
        };
        Ok(Config {
            cluster: get("IC_NODES").ok_or("set IC_NODES to the ledger nodes' addresses")?,
            cycle_every: Duration::from_millis(number("IC_REINSURANCE_CYCLE_MS", 2_000)?),
            patience: Duration::from_millis(number("IC_REINSURANCE_PATIENCE_MS", 10_000)?),
            batch: number("IC_REINSURANCE_BATCH", 100)? as usize,
            records_endpoint: get("IC_BLOB_ENDPOINT").ok_or("set IC_BLOB_ENDPOINT to the storage account reinsurance records are filed in")?,
            records_container: get("IC_REINSURANCE_RECORDS_CONTAINER").unwrap_or_else(|| "reinsurance-records".into()),
            event_hubs: get("IC_EVENT_HUBS_ENDPOINT").ok_or("set IC_EVENT_HUBS_ENDPOINT to the Event Hubs namespace")?,
            event_hub: get("IC_REINSURANCE_EVENT_HUB").unwrap_or_else(|| "reinsurance-events".into()),
        })
    }
}

/// A future that resolves when the process is told to stop: SIGTERM, which
/// is how Kubernetes ends a pod, or SIGINT from a terminal. The handlers are
/// in place when this returns, so a signal sent after it is never missed.
pub fn stop_signals() -> Result<impl Future<Output = ()>, String> {
    let mut terminate = signal(SignalKind::terminate()).map_err(|e| format!("cannot listen for SIGTERM: {e}"))?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|e| format!("cannot listen for SIGINT: {e}"))?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

/// Run the driver against the cluster until `stop` resolves, filing records
/// in `records` and telling staff through `staff`. The cycle in progress
/// finishes first.
pub async fn serve(config: Config, auth: ClusterAuth, records: impl WormStore, staff: impl StaffFeed, stop: impl Future<Output = ()>) -> Result<(), String> {
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let driver = Driver {
        module: ClusterModule::new(client.clone(), config.patience),
        core: ClusterCore::new(client.clone(), config.patience),
        batch: config.batch,
    };
    let audiences = Carriers {
        modules: ClusterAudiences::new(client, config.patience),
        records,
        staff,
    };
    tracing::info!(?config, "reinsurance driver started");
    driver.run(&audiences, config.cycle_every, stop, |_, _| {}).await;
    tracing::info!("reinsurance driver stopped");
    Ok(())
}

/// The whole process: logging, settings from `get`, the cluster credential
/// (read with `read` where it is a file), the process's Azure identity, then
/// [`serve`] until a stop signal. Fails before starting when a setting is missing or wrong.
pub async fn run(get: impl Fn(&str) -> Option<String>, read: impl Fn(&str) -> std::io::Result<String>) -> Result<(), String> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    // A second call in one process keeps the logging already set.
    let _ = tracing_subscriber::fmt().json().with_env_filter(filter).try_init();
    let config = Config::from_lookup(&get)?;
    let auth = ClusterAuth::from_lookup(&get, read)?;
    let credential = AzureCredential::from_lookup(&get).map_err(|e| e.to_string())?;
    let records = BlobContainer::new(&config.records_endpoint, &config.records_container, credential.clone());
    let staff = EventHubsSink::new(&config.event_hubs, &config.event_hub, credential);
    serve(config, auth, records, staff, stop_signals()?).await
}
