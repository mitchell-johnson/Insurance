//! The close driver as it runs in production: the ledger and the other
//! modules on the cluster, Merkle roots and extracts on Azure Blob Storage
//! with an immutability policy, and a pass on a timer.

use std::future::Future;
use std::time::Duration;

use ic_azure::blob::WormStore;
use ic_core::Reject;
use ic_raft::ClusterClient;
use serde_json::Value;

use crate::books::{FinApplied, FinEnvelope};
use crate::driver::{Books, Driver, DriverError, Modules, Pass};
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
}

impl Config {
    /// Read through `get`, which is `std::env::var` in production. A setting
    /// without a default is required: a service that started without its
    /// storage would look healthy and close nothing.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("{key} is not set"));
        let seconds = match get("IC_FINANCE_PASS_SECONDS") {
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|s| *s > 0)
                .ok_or("IC_FINANCE_PASS_SECONDS must be a whole number of seconds above zero")?,
            None => 60,
        };
        Ok(Self {
            cluster: get("IC_CLUSTER_NODES").or_else(|| get("IC_NODES")).ok_or("IC_CLUSTER_NODES is not set")?,
            roots_endpoint: need("IC_FINANCE_ROOTS_ENDPOINT")?,
            roots_container: get("IC_FINANCE_ROOTS_CONTAINER").unwrap_or_else(|| "merkle-roots".into()),
            extracts_endpoint: need("IC_FINANCE_EXTRACTS_ENDPOINT")?,
            extracts_container: get("IC_FINANCE_EXTRACTS_CONTAINER").unwrap_or_else(|| "finance-extracts".into()),
            pass_every: Duration::from_secs(seconds),
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
        ClusterModules { client: self.client.clone() }.ask(MODULE, question).await
    }
}

/// Every module of the node, asked by name.
#[derive(Clone)]
pub struct ClusterModules {
    client: ClusterClient,
}

impl ClusterModules {
    pub fn new(client: ClusterClient) -> Self {
        Self { client }
    }
}

impl Modules for ClusterModules {
    async fn ask(&self, module: &str, question: Value) -> Result<Value, DriverError> {
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
