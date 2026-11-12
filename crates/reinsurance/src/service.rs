//! The reinsurance driver as a process: its settings, read from the
//! environment by `ic-reinsurance`.

use std::time::Duration;

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
}

impl Config {
    /// Read the settings. `IC_NODES` is required; the rest have
    /// defaults: `IC_REINSURANCE_CYCLE_MS` (2000), `IC_REINSURANCE_PATIENCE_MS`
    /// (10000) and `IC_REINSURANCE_BATCH` (100).
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
        })
    }
}
