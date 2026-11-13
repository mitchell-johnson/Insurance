//! The close driver as it runs in production: the ledger and the other
//! modules on the cluster, Merkle roots and extracts on Azure Blob Storage
//! with an immutability policy, and a pass on a timer.

use std::future::Future;
use std::time::Duration;

use chrono::NaiveDate;
use ic_azure::blob::WormStore;
use ic_core::invest::{Question, Reply};
use ic_core::Reject;
use ic_ledger::PartitionId;
use ic_raft::ClusterClient;
use serde_json::Value;

use crate::books::{FinApplied, FinEnvelope};
use crate::driver::{waiting_cash_report, Books, Driver, DriverError, Modules, Pass, CORE};
use crate::host::MODULE;

/// Settings of the close service, read from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The ledger nodes, `host:port` separated by commas.
    pub cluster: String,
    /// The storage account and container the core publishes Merkle roots to.
    pub roots_endpoint: String,
    pub roots_container: String,
    /// The storage account and container extracts are written to.
    pub extracts_endpoint: String,
    pub extracts_container: String,
    pub pass_every: Duration,
    /// Whether the close runs as a Temporal workflow. It does wherever a
    /// Temporal service is configured (`TEMPORAL_ADDRESS`); without one the
    /// service runs the same steps itself on the timer.
    pub temporal: bool,
    /// The Temporal task queue the close workers poll.
    pub task_queue: String,
    /// How long Temporal lets one attempt of a step run before it gives the
    /// step to another worker. An extract is written inside one step.
    pub step_timeout: Duration,
}

impl Config {
    /// Read through `get`, which is `std::env::var` in production. A setting
    /// without a default is required: a service that started without its
    /// storage would look healthy and close nothing.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("{key} is not set"));
        let seconds = |key: &str, default: u64| match get(key) {
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|s| *s > 0)
                .ok_or(format!("{key} must be a whole number of seconds above zero")),
            None => Ok(default),
        };
        Ok(Self {
            cluster: get("IC_CLUSTER_NODES").or_else(|| get("IC_NODES")).ok_or("IC_CLUSTER_NODES is not set")?,
            roots_endpoint: need("IC_FINANCE_ROOTS_ENDPOINT")?,
            roots_container: get("IC_FINANCE_ROOTS_CONTAINER").unwrap_or_else(|| "merkle-roots".into()),
            extracts_endpoint: need("IC_FINANCE_EXTRACTS_ENDPOINT")?,
            extracts_container: get("IC_FINANCE_EXTRACTS_CONTAINER").unwrap_or_else(|| "finance-extracts".into()),
            pass_every: Duration::from_secs(seconds("IC_FINANCE_PASS_SECONDS", 60)?),
            temporal: get("TEMPORAL_ADDRESS").is_some(),
            task_queue: get("IC_FINANCE_TASK_QUEUE").unwrap_or_else(|| crate::temporal::TASK_QUEUE.into()),
            step_timeout: Duration::from_secs(seconds("IC_FINANCE_STEP_TIMEOUT_SECS", 300)?),
        })
    }

    /// What a registered extract's location starts with.
    pub fn location(&self) -> String {
        format!("{}/{}", self.extracts_endpoint.trim_end_matches('/'), self.extracts_container)
    }
}

/// The finance ledger in its replicated group on the cluster.
#[derive(Clone)]
pub struct ClusterBooks {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterBooks {
    /// `patience` is how long one command keeps looking for a leader.
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Books for ClusterBooks {
    async fn propose(&self, envelope: FinEnvelope) -> Result<Result<(), Reject>, DriverError> {
        let command = serde_json::to_value(&envelope).map_err(|e| DriverError::Answer(e.to_string()))?;
        let reply = self
            .client
            .propose_module_until(MODULE, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let applied: FinApplied = serde_json::from_value(reply.result.map_err(DriverError::Answer)?).map_err(|e| DriverError::Answer(e.to_string()))?;
        Ok(applied.result)
    }

    async fn ask(&self, question: Value) -> Result<Value, DriverError> {
        ClusterModules::new(self.client.clone(), 0).ask(MODULE, question).await
    }
}

/// Every module of the node, asked by name, and the core's partitions under
/// the name [`CORE`].
#[derive(Clone)]
pub struct ClusterModules {
    client: ClusterClient,
    partitions: PartitionId,
}

impl ClusterModules {
    pub fn new(client: ClusterClient, partitions: PartitionId) -> Self {
        Self { client, partitions }
    }

    /// The underwriting loadings on every layer in force on `as_at`, read
    /// from each partition's core a page of policies at a time, to put on a
    /// table dry run's request. They imply health: pass them on and keep
    /// nothing.
    pub async fn loadings(&self, as_at: NaiveDate) -> Result<Vec<crate::tables::LayerLoading>, DriverError> {
        let mut found = Vec::new();
        for partition in 0..self.partitions {
            let mut after = None;
            loop {
                let page = self
                    .client
                    .loadings(partition, after, ic_core::loadings::MAX_PAGE, Some(as_at))
                    .await
                    .map_err(|e| DriverError::Unavailable(e.to_string()))?;
                found.extend(page.rows.into_iter().map(|r| crate::tables::LayerLoading {
                    partition,
                    policy: r.policy.to_string(),
                    cover: r.cover,
                    layer: r.layer,
                    loadings: r.loadings,
                }));
                after = page.next;
                if after.is_none() {
                    break;
                }
            }
        }
        Ok(found)
    }

    /// The core keeps a policy's money on its partition, so a question to
    /// the core is asked of every partition and the answers put together.
    async fn ask_core(&self, question: Value) -> Result<Value, DriverError> {
        let Some(as_at) = question.get("waiting_cash").and_then(|w| serde_json::from_value(w["as_at"].clone()).ok()) else {
            return Err(DriverError::Answer(format!("not a question the core is asked for the close: {question}")));
        };
        let mut waiting = Vec::new();
        for partition in 0..self.partitions {
            match self.client.investment(partition, &Question::WaitingCash).await {
                Ok(Reply::WaitingCash(found)) => waiting.extend(found),
                Ok(other) => return Err(DriverError::Answer(format!("partition {partition} answered {other:?}"))),
                Err(e) => return Err(DriverError::Unavailable(e.to_string())),
            }
        }
        Ok(waiting_cash_report(as_at, waiting))
    }
}

impl Modules for ClusterModules {
    async fn ask(&self, module: &str, question: Value) -> Result<Value, DriverError> {
        if module == CORE {
            return self.ask_core(question).await;
        }
        self.client
            .query_module(module, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(DriverError::Answer)
    }
}

/// Run a pass every `every` until `stop` completes. A pass that did
/// something is followed at once by another, so a month that has just gone
/// green is closed without waiting out the timer.
pub async fn run_passes<B, M, R, X>(
    driver: &Driver<B, M, R, X>,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&Result<Pass, DriverError>),
) where
    B: Books,
    M: Modules,
    R: WormStore,
    X: WormStore,
{
    tokio::pin!(stop);
    loop {
        let pass = driver.pass().await;
        let busy = match &pass {
            Ok(p) => {
                let done = p.roots_recorded.len() + p.extracts_registered.len() + p.reports_sent.len() + p.closed.len();
                if p.waiting.is_empty() {
                    tracing::debug!(done, "close pass");
                } else {
                    tracing::warn!(done, waiting = ?p.waiting, "a close pass left work waiting");
                }
                done > 0
            }
            Err(e) => {
                tracing::warn!(error = %e, "a close pass could not run");
                false
            }
        };
        seen(&pass);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(if busy { Duration::from_millis(10) } else { every }) => {}
        }
    }
}

/// How much a pass or a run did.
fn done(p: &Pass) -> usize {
    p.roots_recorded.len() + p.extracts_registered.len() + p.reports_sent.len() + p.closed.len()
}

/// Run the close on Temporal until `stop` completes: a worker for the
/// steps, and a run started every `pass_every` (at once after a run that
/// did something). Every replica does both. The runs share one workflow
/// ID, so a replica that starts a run while one is going waits for that one.
pub async fn run_on_temporal(
    client: temporalio_client::Client,
    steps: std::sync::Arc<dyn crate::temporal::CloseSteps>,
    config: &Config,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&Result<Pass, String>),
) -> Result<(), String> {
    let input = crate::temporal::CloseRunInput {
        step_timeout_secs: config.step_timeout.as_secs(),
    };
    let starter = async {
        loop {
            let run = crate::temporal::close_run(&client, &config.task_queue, input.clone()).await;
            let busy = match &run {
                Ok(p) if p.waiting.is_empty() => {
                    tracing::debug!(done = done(p), "close run");
                    done(p) > 0
                }
                Ok(p) => {
                    tracing::warn!(done = done(p), waiting = ?p.waiting, "a close run left work waiting");
                    done(p) > 0
                }
                Err(e) => {
                    tracing::warn!(error = %e, "a close run failed");
                    false
                }
            };
            seen(&run);
            tokio::time::sleep(if busy { Duration::from_millis(10) } else { config.pass_every }).await;
        }
    };
    tokio::select! {
        worker = crate::temporal::run_worker(client.clone(), &config.task_queue, steps, stop) => worker,
        () = starter => Ok(()),
    }
}
