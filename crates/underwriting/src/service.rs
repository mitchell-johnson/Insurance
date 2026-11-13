//! The effects driver as a process: its settings, its connections to the
//! modules and the core on the cluster, and the loop `ic-underwriting` runs.
//!
//! It holds no state. Everything it needs to carry on after a restart is in
//! the module's outbox and in the keys on what it sends, so any number may
//! run at once.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::EventHubsSink;
use ic_ledger::PartitionId;
use ic_raft::{ClusterAuth, ClusterClient};
use ic_reinsurance::capacity::Outcome;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::command::{Applied, Envelope, Reject};
use crate::driver::{Channels, Core, CoreAnswer, Driver, DriverError, Forward, Refusal, Reinsurer, Schemes, Underwriting};
use crate::notices::{Letters, Notices};
use crate::progress::ProgressRow;
use crate::special::Query;

/// Settings of the driver process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The ledger nodes, as `IC_NODES` lists them.
    pub cluster: String,
    /// The pause between delivery passes.
    pub cycle_every: Duration,
    /// How long one command keeps looking for a leader.
    pub patience: Duration,
    /// Effects read from the outbox at a time.
    pub batch: usize,
    /// Open proposals the daily sweep takes in one command.
    pub sweep_page: u32,
    /// The Event Hubs namespace, `https://<namespace>.servicebus.windows.net`.
    pub event_hubs: String,
    /// The hub staff notices are published to.
    pub event_hub: String,
}

impl Config {
    /// Read the settings. `IC_NODES` and `IC_EVENT_HUBS_ENDPOINT` are
    /// required: staff notices have nowhere else to go. The rest have
    /// defaults: `IC_UNDERWRITING_EVENT_HUB` (`underwriting-notices`),
    /// `IC_UNDERWRITING_CYCLE_MS` (2000), `IC_UNDERWRITING_PATIENCE_MS`
    /// (10000), `IC_UNDERWRITING_BATCH` (100) and
    /// `IC_UNDERWRITING_SWEEP_PAGE` (200, at most 1000).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let number = |key: &str, default: u64, most: u64| match get(key) {
            None => Ok(default),
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=most).contains(n))
                .ok_or_else(|| format!("{key} must be a whole number from 1 to {most}, not {text:?}")),
        };
        Ok(Config {
            cluster: get("IC_NODES").ok_or("set IC_NODES to the ledger nodes' addresses")?,
            cycle_every: Duration::from_millis(number("IC_UNDERWRITING_CYCLE_MS", 2_000, 3_600_000)?),
            patience: Duration::from_millis(number("IC_UNDERWRITING_PATIENCE_MS", 10_000, 3_600_000)?),
            batch: number("IC_UNDERWRITING_BATCH", 100, 10_000)? as usize,
            sweep_page: number("IC_UNDERWRITING_SWEEP_PAGE", 200, 1_000)? as u32,
            event_hubs: get("IC_EVENT_HUBS_ENDPOINT").ok_or("set IC_EVENT_HUBS_ENDPOINT to the Event Hubs namespace")?,
            event_hub: get("IC_UNDERWRITING_EVENT_HUB").unwrap_or_else(|| "underwriting-notices".into()),
        })
    }
}

/// The modules and the core, on the cluster.
#[derive(Clone)]
pub struct Cluster {
    client: ClusterClient,
    patience: Duration,
    /// How many partitions the ledger has. A group's home is its frozen
    /// number modulo this.
    partitions: PartitionId,
}

/// What a module that answers with a `result` made of a command.
#[derive(serde::Deserialize)]
struct Answered<R> {
    result: Result<(), R>,
}

/// As [`Answered`], for a module whose `result` carries something this
/// driver does not read.
#[derive(serde::Deserialize)]
struct AnsweredWith<R> {
    result: Result<serde::de::IgnoredAny, R>,
}

/// A command to the channel module, as it reads one: rows published by this
/// module under its own name.
pub fn channels_envelope(key: &str, at: DateTime<Utc>, rows: &[ProgressRow]) -> Value {
    serde_json::json!({
        "key": key,
        "at": at.timestamp(),
        "actor": {"kind": "module", "id": crate::progress::MODULE},
        "command": {"publish_rows": {"rows": rows, "remove": []}},
    })
}

/// A command to the documents module, as it reads one: sent by this module
/// under its own name, by the driver, at the driver's clock.
pub fn documents_envelope(key: &str, at: DateTime<Utc>, command: Value) -> Value {
    serde_json::json!({
        "key": key,
        "caller": crate::progress::MODULE,
        "actor": "underwriting-driver",
        "at": at,
        "command": command,
    })
}

impl Cluster {
    /// `patience` is how long one command keeps looking for a leader.
    pub fn new(client: ClusterClient, partitions: PartitionId, patience: Duration) -> Self {
        Self {
            client,
            patience,
            partitions: partitions.max(1),
        }
    }

    /// One command to a module. The host refuses what the module cannot
    /// read, which no repeat will change: that is an unexpected answer.
    async fn to_module<T: DeserializeOwned>(&self, module: &str, envelope: &impl Serialize) -> Result<T, DriverError> {
        let command = serde_json::to_value(envelope).map_err(|e| DriverError::Answer(e.to_string()))?;
        let reply = self
            .client
            .propose_module_until(module, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let answer = reply
            .result
            .map_err(|e| DriverError::Answer(format!("{module} could not read the command: {e}")))?;
        serde_json::from_value(answer).map_err(|e| DriverError::Answer(format!("{module} answered in a form this driver does not read: {e}")))
    }

    async fn ask_module(&self, module: &str, question: &impl Serialize) -> Result<Value, DriverError> {
        let question = serde_json::to_value(question).map_err(|e| DriverError::Answer(e.to_string()))?;
        self.client
            .query_module(module, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(|e| DriverError::Answer(format!("{module} could not answer: {e}")))
    }
}

impl Underwriting for Cluster {
    async fn propose(&self, envelope: Envelope) -> Result<Result<Applied, Reject>, DriverError> {
        self.to_module(crate::host::MODULE, &envelope).await
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        self.ask_module(crate::host::MODULE, &query).await
    }
}

impl Reinsurer for Cluster {
    async fn propose(&self, envelope: ic_reinsurance::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        let answered: Answered<Reject> = self.to_module(ic_reinsurance::host::MODULE, &envelope).await?;
        Ok(answered.result.map_err(|r| Refusal::new(&r.code, r.message)))
    }

    async fn capacity(&self, reservation: &str) -> Result<Option<Outcome>, DriverError> {
        let question = ic_reinsurance::host::Query::Capacity {
            reservation_id: reservation.to_string(),
        };
        let answer = self.ask_module(ic_reinsurance::host::MODULE, &question).await?;
        let found: Option<ic_reinsurance::capacity::CapacityAnswer> =
            serde_json::from_value(answer).map_err(|e| DriverError::Answer(format!("reinsurance answered in a form this driver does not read: {e}")))?;
        Ok(found.map(|a| a.outcome))
    }
}

impl Core for Cluster {
    async fn send(&self, partition: PartitionId, key: &str, caller: &str, command: ic_core::Command) -> Result<CoreAnswer, DriverError> {
        let envelope = ic_core::Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command,
        };
        let reply = self
            .client
            .propose_until(partition % self.partitions, &envelope, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        Ok(CoreAnswer {
            refused: reply.rejected.map(|r| Refusal::new(&r.code, r.message)),
            events: reply.events.into_iter().map(|e| e.event).collect(),
        })
    }
}

impl Letters for Cluster {
    async fn request(&self, key: &str, at: DateTime<Utc>, command: Value) -> Result<Result<(), Refusal>, DriverError> {
        let answered: Answered<Reject> = self.to_module("documents", &documents_envelope(key, at, command)).await?;
        Ok(answered.result.map_err(|r| Refusal::new(&r.code, r.message)))
    }
}

impl Channels for Cluster {
    async fn publish(&self, key: &str, at: DateTime<Utc>, rows: &[ProgressRow]) -> Result<Result<(), Refusal>, DriverError> {
        let answered: AnsweredWith<Reject> = self.to_module("channels", &channels_envelope(key, at, rows)).await?;
        Ok(answered.result.map(|_| ()).map_err(|r| Refusal::new(&r.code, r.message)))
    }
}

impl Schemes for Cluster {
    async fn propose(&self, envelope: ic_group::command::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        let answered: Answered<Reject> = self.to_module(ic_group::host::MODULE, &envelope).await?;
        Ok(answered.result.map_err(|r| Refusal::new(&r.code, r.message)))
    }
}

/// The driver over the cluster, once the nodes have said how many
/// partitions there are, handing to `forward` what has no module of its own.
pub async fn connect<F: Forward>(
    client: ClusterClient,
    config: &Config,
    clock: fn() -> DateTime<Utc>,
    forward: F,
) -> Result<Driver<Cluster, Cluster, Cluster, Cluster, F, Cluster>, String> {
    let partitions = client.partitions(config.patience).await?;
    let cluster = Cluster::new(client, partitions, config.patience);
    Ok(Driver {
        underwriting: cluster.clone(),
        reinsurer: cluster.clone(),
        core: cluster.clone(),
        schemes: cluster.clone(),
        channels: cluster,
        forward,
        batch: config.batch,
        clock,
    })
}

/// The process: settings from `get`, the cluster token from `get` or the
/// file it names, the process's Azure identity for the staff notices, and
/// cycles until `stop`. It ends with an error, for the
/// platform to start it again, when no node answers at the start.
pub async fn serve(get: impl Fn(&str) -> Option<String>, read: impl Fn(&str) -> std::io::Result<String>, stop: impl Future<Output = ()>) -> Result<(), String> {
    let config = Config::from_lookup(&get)?;
    let auth = ClusterAuth::from_lookup(&get, read)?;
    let credential = AzureCredential::from_lookup(&get).map_err(|e| e.to_string())?;
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let notices = Notices {
        staff: EventHubsSink::new(&config.event_hubs, &config.event_hub, credential),
        // Letters go to a module, which needs no partition count.
        letters: Cluster::new(client.clone(), 1, config.patience),
        clock: Utc::now,
    };
    let driver = connect(client, &config, Utc::now, notices).await?;
    tracing::info!(?config, "underwriting driver started");
    driver.run(config.cycle_every, config.sweep_page, stop, |_, _| {}).await;
    tracing::info!("underwriting driver stopped");
    Ok(())
}

/// Resolves on SIGTERM or an interrupt.
pub async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
