//! The API's configuration, read from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveDate;
use ic_ledger::PartitionId;
use ic_raft::{ClusterAuth, ClusterClient};

use crate::auth::Auth;
use crate::engine::{AzureConfig, EngineConfig};

/// How callers of the API are identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// Trust the caller's own headers. For a laptop only.
    Dev,
    /// Microsoft Entra ID access tokens for this tenant and audience.
    Entra { tenant: String, audience: String },
}

impl Identity {
    pub fn auth(&self) -> Auth {
        match self {
            Identity::Dev => Auth::Dev,
            Identity::Entra { tenant, audience } => Auth::entra(tenant, audience),
        }
    }
}

/// The ledger nodes a gateway stands in front of.
#[derive(Debug, Clone)]
pub struct ClusterSettings {
    /// Node addresses as `host:port`.
    pub nodes: Vec<String>,
    /// The cluster token presented to the nodes.
    pub auth: ClusterAuth,
    pub partitions: PartitionId,
    /// How long a request keeps trying the nodes before it answers 503.
    pub patience: Duration,
}

#[derive(Debug, Clone)]
pub enum Mode {
    /// Every partition in this process: the insurance in a box.
    Single(EngineConfig),
    /// A stateless gateway to replicated ledger nodes.
    Cluster(ClusterSettings),
}

#[derive(Debug, Clone)]
pub struct ApiSettings {
    pub listen: SocketAddr,
    pub identity: Identity,
    pub mode: Mode,
}

impl ApiSettings {
    /// Read settings through `get`, which is `std::env::var` in production.
    /// `today` is the date an empty single-process ledger opens on when
    /// `IC_OPENING_DATE` does not say.
    ///
    /// Common:
    /// - `IC_LISTEN`: default `0.0.0.0:8080`.
    /// - `IC_PARTITIONS`: how many partitions the ledger has. Default 64.
    /// - `IC_ENTRA_TENANT_ID`, `IC_ENTRA_AUDIENCE`: validate Entra ID tokens.
    /// - `IC_AUTH=dev`: trust caller headers instead. Refused in Kubernetes
    ///   and whenever an Azure endpoint is configured.
    ///
    /// Gateway to ledger nodes, chosen by `IC_NODES`:
    /// - `IC_NODES`: `host:port,host:port,...` of the nodes.
    /// - `IC_CLUSTER_TOKEN_FILE`, `IC_CLUSTER_TOKEN`, `IC_CLUSTER_AUTH`: the
    ///   token the nodes require; see [`ClusterAuth::from_lookup`].
    /// - `IC_LEDGER_PATIENCE_SECS`: default 10.
    ///
    /// The insurance in a box, when `IC_NODES` is not set:
    /// - `IC_DATA_DIR`: keep the partition logs here. Without it, in memory.
    /// - `IC_OPENING_DATE`: the first business date of an empty ledger.
    /// - `IC_LOAD_SHIPPED_CONFIG=false`: start with no products active.
    /// - `IC_BLOB_ENDPOINT`, `IC_MERKLE_CONTAINER`, `IC_KEY_VAULT_URL`,
    ///   `IC_MERKLE_KEY`, `IC_EVENT_HUBS_ENDPOINT`, `IC_EVENT_HUB`: use the
    ///   Azure service instead of its in-memory stand-in.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, today: NaiveDate) -> Result<Self, String> {
        let raw = &get;
        let get = |key: &str| raw(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let listen = get("IC_LISTEN")
            .unwrap_or_else(|| "0.0.0.0:8080".to_string())
            .parse()
            .map_err(|e| format!("IC_LISTEN: {e}"))?;
        let partitions: PartitionId = match get("IC_PARTITIONS") {
            Some(p) => p.parse().map_err(|e| format!("IC_PARTITIONS: {e}"))?,
            None => ic_ledger::PARTITION_COUNT,
        };
        if partitions == 0 || partitions > ic_ledger::PARTITION_COUNT {
            return Err(format!("IC_PARTITIONS must be between 1 and {}", ic_ledger::PARTITION_COUNT));
        }
        let azure = AzureConfig {
            blob_endpoint: get("IC_BLOB_ENDPOINT"),
            merkle_container: get("IC_MERKLE_CONTAINER").unwrap_or_else(|| "merkle-roots".to_string()),
            key_vault_url: get("IC_KEY_VAULT_URL"),
            merkle_key: get("IC_MERKLE_KEY").unwrap_or_else(|| "merkle-root-signing".to_string()),
            event_hubs_endpoint: get("IC_EVENT_HUBS_ENDPOINT"),
            event_hub: get("IC_EVENT_HUB").unwrap_or_else(|| "policy-events".to_string()),
        };
        let on_azure = azure.blob_endpoint.is_some() || azure.key_vault_url.is_some() || azure.event_hubs_endpoint.is_some();
        let in_kubernetes = get("KUBERNETES_SERVICE_HOST").is_some();
        let identity = match (get("IC_AUTH").as_deref(), get("IC_ENTRA_TENANT_ID"), get("IC_ENTRA_AUDIENCE")) {
            (Some("dev"), _, _) if on_azure => return Err("IC_AUTH=dev is refused when any Azure endpoint is configured".to_string()),
            (Some("dev"), _, _) if in_kubernetes => return Err("IC_AUTH=dev is refused in Kubernetes: callers must present Entra ID tokens".to_string()),
            (Some("dev"), _, _) => Identity::Dev,
            (None | Some("entra"), Some(tenant), Some(audience)) => Identity::Entra { tenant, audience },
            _ => return Err("set IC_ENTRA_TENANT_ID and IC_ENTRA_AUDIENCE, or IC_AUTH=dev for the insurance in a box".to_string()),
        };
        let mode = match get("IC_NODES") {
            Some(list) => {
                if get("IC_DATA_DIR").is_some() {
                    return Err("IC_DATA_DIR and IC_NODES are both set: a gateway to ledger nodes keeps no ledger of its own".to_string());
                }
                let nodes = ClusterClient::from_list(&list).map_err(|e| format!("IC_NODES: {e}"))?.nodes().to_vec();
                let patience = match get("IC_LEDGER_PATIENCE_SECS") {
                    Some(v) => v.parse().map_err(|e| format!("IC_LEDGER_PATIENCE_SECS: {e}"))?,
                    None => 10,
                };
                Mode::Cluster(ClusterSettings {
                    nodes,
                    auth: ClusterAuth::from_lookup(raw, |path| std::fs::read_to_string(path))?,
                    partitions,
                    patience: Duration::from_secs(patience),
                })
            }
            None => {
                // The business date is set once, on an empty ledger, and from
                // then on moves only by the cutoff.
                let opening_date = match get("IC_OPENING_DATE") {
                    Some(d) => NaiveDate::parse_from_str(&d, "%Y-%m-%d").map_err(|e| format!("IC_OPENING_DATE: {e}"))?,
                    None => today,
                };
                Mode::Single(EngineConfig {
                    partitions,
                    data_dir: get("IC_DATA_DIR").map(PathBuf::from),
                    opening_date,
                    load_shipped_config: get("IC_LOAD_SHIPPED_CONFIG").is_none_or(|v| v != "false"),
                    azure,
                })
            }
        };
        Ok(Self { listen, identity, mode })
    }
}
