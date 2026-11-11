//! The pump's settings, from the environment.

use std::path::PathBuf;
use std::time::Duration;

use ic_raft::{ClusterAuth, ClusterClient};

#[derive(Debug, Clone)]
pub struct PumpSettings {
    pub nodes: Vec<String>,
    pub auth: ClusterAuth,
    /// The region this pump runs in: it uses that region's bank endpoints.
    pub region: String,
    pub instance: String,
    /// Where the Key Vault CSI driver mounts the bank credentials.
    pub secrets_dir: PathBuf,
    pub key_vault: String,
    pub blob_endpoint: String,
    pub container: String,
    pub interval: Duration,
    pub grace: chrono::Duration,
}

impl PumpSettings {
    /// - `IC_NODES`, and the cluster token settings of every client.
    /// - `IC_REGION`: this pump's region, as the module's configuration names it.
    /// - `IC_BANK_SECRETS_DIR`: bank credentials, one file per secret.
    /// - `IC_KEY_VAULT_URL`: where the bank signing keys are held.
    /// - `IC_BLOB_ENDPOINT`, `IC_PAYMENTS_CONTAINER`: the evidence vault
    ///   (immutable storage). The container defaults to `payments-evidence`.
    /// - `IC_PAYMENTS_INSTANCE` or `HOSTNAME`: unique to this process.
    /// - `IC_PUMP_INTERVAL_SECS` (15), `IC_PUMP_GRACE_MINUTES` (10).
    ///
    /// There are no fallbacks to a local signer or an in-memory vault: a
    /// pump that would send a bank an unsigned file does not start.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let auth = ClusterAuth::from_lookup(&get, |path| std::fs::read_to_string(path))?;
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("{key} is required"));
        let number = |key: &str, default: u64| match get(key) {
            Some(v) => v.parse::<u64>().map_err(|e| format!("{key}: {e}")),
            None => Ok(default),
        };
        let nodes = ClusterClient::from_list(&need("IC_NODES")?)
            .map_err(|e| format!("IC_NODES: {e}"))?
            .nodes()
            .to_vec();
        let interval = number("IC_PUMP_INTERVAL_SECS", 15)?;
        let grace = number("IC_PUMP_GRACE_MINUTES", 10)?;
        // The grace period is what keeps one pump from declaring lost a
        // file another pump is still sending.
        if interval == 0 || grace < 2 {
            return Err("IC_PUMP_INTERVAL_SECS must be at least 1 and IC_PUMP_GRACE_MINUTES at least 2".to_string());
        }
        Ok(Self {
            nodes,
            auth,
            region: need("IC_REGION")?,
            instance: get("IC_PAYMENTS_INSTANCE")
                .or_else(|| get("HOSTNAME"))
                .ok_or("IC_PAYMENTS_INSTANCE or HOSTNAME is required")?,
            secrets_dir: PathBuf::from(need("IC_BANK_SECRETS_DIR")?),
            key_vault: need("IC_KEY_VAULT_URL")?,
            blob_endpoint: need("IC_BLOB_ENDPOINT")?,
            container: get("IC_PAYMENTS_CONTAINER").unwrap_or_else(|| "payments-evidence".to_string()),
            interval: Duration::from_secs(interval),
            grace: chrono::Duration::minutes(grace as i64),
        })
    }
}
