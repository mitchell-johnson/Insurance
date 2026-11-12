//! The driver as a process, `ic-unit-pricing`: its settings, read from the
//! environment, and the loop that runs a pass every cycle until stopped.
//! Only one need run, though two do no harm: every proposal is keyed.

use std::future::Future;
use std::time::Duration;

use chrono::Utc;
use ic_azure::blob::BlobContainer;
use ic_azure::credential::AzureCredential;
use ic_raft::{ClusterAuth, ClusterClient};
use tokio::signal::unix::{signal, SignalKind};

use crate::adapters::{BlobInbox, BlobOutbound, ClusterCore, ClusterModule};
use crate::driver::{Core, Driver, Inbox, Module, Outbound, PassReport};

/// Settings of the driver process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The ledger nodes, as `IC_NODES` lists them.
    pub cluster: String,
    /// The storage account and container the administrator writes valuations to.
    pub inbox_endpoint: String,
    pub inbox_container: String,
    /// The storage account and container extracts are written to.
    pub outbound_endpoint: String,
    pub outbound_container: String,
    /// The pause between passes.
    pub cycle_every: Duration,
    /// How long one command keeps looking for a leader.
    pub patience: Duration,
}

impl Config {
    /// Read the settings. Required: `IC_NODES`,
    /// `IC_UNIT_PRICING_INBOX_ENDPOINT`, `IC_UNIT_PRICING_OUTBOUND_ENDPOINT`.
    /// Defaults: `IC_UNIT_PRICING_INBOX_CONTAINER` (`unit-pricing-inbound`),
    /// `IC_UNIT_PRICING_OUTBOUND_CONTAINER` (`unit-pricing-outbound`),
    /// `IC_UNIT_PRICING_CYCLE_MS` (30000) and `IC_UNIT_PRICING_PATIENCE_MS` (10000).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("set {key}"));
        let number = |key: &str, default: u64| match get(key) {
            None => Ok(default),
            Some(text) => text
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{key} must be a whole number above zero, not {text:?}")),
        };
        Ok(Config {
            cluster: need("IC_NODES")?,
            inbox_endpoint: need("IC_UNIT_PRICING_INBOX_ENDPOINT")?,
            inbox_container: get("IC_UNIT_PRICING_INBOX_CONTAINER").unwrap_or_else(|| "unit-pricing-inbound".to_string()),
            outbound_endpoint: need("IC_UNIT_PRICING_OUTBOUND_ENDPOINT")?,
            outbound_container: get("IC_UNIT_PRICING_OUTBOUND_CONTAINER").unwrap_or_else(|| "unit-pricing-outbound".to_string()),
            cycle_every: Duration::from_millis(number("IC_UNIT_PRICING_CYCLE_MS", 30_000)?),
            patience: Duration::from_millis(number("IC_UNIT_PRICING_PATIENCE_MS", 10_000)?),
        })
    }
}

/// A future that resolves on SIGTERM (how Kubernetes ends a pod) or SIGINT.
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

/// Run passes every `cycle` until `stop` resolves; the pass in progress
/// finishes first. `on_pass` sees each report.
pub async fn run_passes<M: Module, C: Core, I: Inbox, O: Outbound>(
    driver: &Driver<M, C, I, O>,
    cycle: Duration,
    stop: impl Future<Output = ()>,
    mut on_pass: impl FnMut(&PassReport),
) {
    tokio::pin!(stop);
    loop {
        let report = driver.pass(Utc::now()).await;
        if report.errors.is_empty() {
            tracing::debug!(?report, "unit pricing pass");
        } else {
            tracing::warn!(?report, "unit pricing pass with errors");
        }
        on_pass(&report);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(cycle) => {}
        }
    }
}

/// Run the driver against the cluster and storage until `stop` resolves.
pub async fn serve(config: Config, auth: ClusterAuth, credential: AzureCredential, stop: impl Future<Output = ()>) -> Result<(), String> {
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let partitions = client.partitions(Duration::from_secs(60)).await?;
    let driver = Driver::new(
        ClusterModule::new(client.clone(), config.patience),
        ClusterCore::new(client, partitions),
        BlobInbox::new(&config.inbox_endpoint, &config.inbox_container, credential.clone()),
        BlobOutbound(BlobContainer::new(&config.outbound_endpoint, &config.outbound_container, credential)),
    );
    driver.start(Utc::now()).await?;
    tracing::info!(?config, partitions, "unit pricing driver started");
    run_passes(&driver, config.cycle_every, stop, |_| {}).await;
    tracing::info!("unit pricing driver stopped");
    Ok(())
}

/// The whole process: logging, settings from `get`, the cluster credential
/// (read with `read` where it is a file), the Azure credential from the
/// workload identity, then [`serve`] until a stop signal.
pub async fn run(get: impl Fn(&str) -> Option<String>, read: impl Fn(&str) -> std::io::Result<String>) -> Result<(), String> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt().json().with_env_filter(filter).try_init();
    let config = Config::from_lookup(&get)?;
    let auth = ClusterAuth::from_lookup(&get, read)?;
    let credential = AzureCredential::from_env().map_err(|e| e.to_string())?;
    serve(config, auth, credential, stop_signals()?).await
}
