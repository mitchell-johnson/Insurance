//! The worker's configuration, read from the environment.

use std::path::PathBuf;
use std::time::Duration;

use ic_azure::blob::{BlobContainer, MemoryWormStore};
use ic_azure::credential::AzureCredential;
use ic_azure::keyvault::{DevSigner, KeyVaultSigner};
use ic_raft::ClusterClient;

use crate::steps::LedgerSteps;
use crate::store::{DirWormStore, Signer, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootStore {
    /// An immutable Blob Storage container: `(endpoint, container)`.
    Azure(String, String),
    Dir(PathBuf),
    Memory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSettings {
    /// Ledger node addresses as `host:port`.
    pub nodes: Vec<String>,
    pub root_store: RootStore,
    /// `(vault URL, key name)` of the Key Vault key that signs the root.
    pub key_vault: Option<(String, String)>,
    pub task_queue: String,
    /// How long a step retries the ledger before handing the failure to Temporal.
    pub patience: Duration,
    /// How long Temporal lets one attempt of a step run before it gives the
    /// step to another worker.
    pub step_timeout: Duration,
}

impl WorkerSettings {
    /// Read settings through `get`, which is `std::env::var` in production.
    ///
    /// - `IC_NODES`: `host:port,host:port,...` of the ledger nodes. Required.
    /// - `IC_BLOB_ENDPOINT`, `IC_MERKLE_CONTAINER` (default `merkle-roots`):
    ///   publish the root to immutable Blob Storage.
    /// - `IC_ROOT_DIR`: publish the root to a directory instead. Off Azure only.
    /// - `IC_KEY_VAULT_URL`, `IC_MERKLE_KEY` (default `merkle-root-signing`):
    ///   sign the root with a Key Vault key. Required with `IC_BLOB_ENDPOINT`.
    /// - `IC_TASK_QUEUE`: default [`crate::temporal::TASK_QUEUE`].
    /// - `IC_STEP_PATIENCE_SECS`: default 20.
    /// - `IC_STEP_TIMEOUT_SECS`: default 60. Must exceed the patience, or
    ///   Temporal would give up on a step that is still legitimately waiting.
    ///
    /// The Temporal connection is read by the SDK from `TEMPORAL_ADDRESS`,
    /// `TEMPORAL_NAMESPACE` and the other `TEMPORAL_*` variables.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let nodes = ClusterClient::from_list(&get("IC_NODES").ok_or("IC_NODES is required")?)
            .map_err(|e| format!("IC_NODES: {e}"))?
            .nodes()
            .to_vec();
        let key_vault = get("IC_KEY_VAULT_URL").map(|url| (url, get("IC_MERKLE_KEY").unwrap_or_else(|| "merkle-root-signing".to_string())));
        let root_store = match (get("IC_BLOB_ENDPOINT"), get("IC_ROOT_DIR")) {
            (Some(_), Some(_)) => return Err("set IC_BLOB_ENDPOINT or IC_ROOT_DIR, not both".to_string()),
            // A root in immutable storage with a signature that proves
            // nothing would look like evidence and not be.
            (Some(_), None) if key_vault.is_none() => {
                return Err("IC_BLOB_ENDPOINT needs IC_KEY_VAULT_URL: a published root must be signed by a real key".to_string())
            }
            (Some(endpoint), None) => RootStore::Azure(endpoint, get("IC_MERKLE_CONTAINER").unwrap_or_else(|| "merkle-roots".to_string())),
            (None, Some(dir)) => RootStore::Dir(PathBuf::from(dir)),
            (None, None) => RootStore::Memory,
        };
        let patience = match get("IC_STEP_PATIENCE_SECS") {
            Some(v) => v.parse().map_err(|e| format!("IC_STEP_PATIENCE_SECS: {e}"))?,
            None => 20,
        };
        let step_timeout = match get("IC_STEP_TIMEOUT_SECS") {
            Some(v) => v.parse().map_err(|e| format!("IC_STEP_TIMEOUT_SECS: {e}"))?,
            None => crate::eod::default_step_timeout_secs(),
        };
        if step_timeout <= patience {
            return Err(format!("IC_STEP_TIMEOUT_SECS ({step_timeout}) must exceed IC_STEP_PATIENCE_SECS ({patience})"));
        }
        Ok(Self {
            nodes,
            root_store,
            key_vault,
            task_queue: get("IC_TASK_QUEUE").unwrap_or_else(|| crate::temporal::TASK_QUEUE.to_string()),
            patience: Duration::from_secs(patience),
            step_timeout: Duration::from_secs(step_timeout),
        })
    }

    /// Build the steps these settings describe.
    pub fn steps(&self) -> Result<LedgerSteps<Signer, Store>, String> {
        let credential = || AzureCredential::from_env().map_err(|e| e.to_string());
        let store = match &self.root_store {
            RootStore::Azure(endpoint, container) => Store::Azure(BlobContainer::new(endpoint, container, credential()?)),
            RootStore::Dir(dir) => Store::Dir(DirWormStore::new(dir)),
            RootStore::Memory => Store::Memory(MemoryWormStore::default()),
        };
        let signer = match &self.key_vault {
            Some((url, key)) => Signer::Azure(KeyVaultSigner::new(url, key, credential()?)),
            None => Signer::Dev(DevSigner),
        };
        Ok(LedgerSteps::new(ClusterClient::new(self.nodes.clone()), signer, store).with_patience(self.patience))
    }
}
