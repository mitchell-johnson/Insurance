//! The host around the partitions: logs, read models, the event relay and
//! the end-of-day close.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use ic_azure::blob::{BlobContainer, MemoryWormStore, WormStore};
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::{EventHubsSink, EventSink, MemorySink};
use ic_azure::keyvault::{DevSigner, KeyVaultSigner, RootSigner, Signature};
use ic_azure::merkle::{publish_root, PublishedRoot};
use ic_azure::AzureError;
use ic_cashmatch::Matcher;
use ic_core::recon::Break;
use ic_core::{Applied, Command, EventRecord};
use ic_ledger::PartitionId;
use ic_log::{FileLog, MemoryLog, OrderedLog};
use ic_readmodel::Projections;
use ic_sim::{Cluster, Delivery};
use serde::Serialize;

type BoxLog = Box<dyn OrderedLog + Send>;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub partitions: PartitionId,
    /// Directory for the partition logs. None keeps them in memory.
    pub data_dir: Option<PathBuf>,
    pub opening_date: NaiveDate,
    /// Activate the configuration shipped in this repository on an empty cluster.
    pub load_shipped_config: bool,
    pub azure: AzureConfig,
}

/// Azure endpoints. Each is optional: without it the in-memory stand-in is used.
#[derive(Debug, Clone, Default)]
pub struct AzureConfig {
    /// `https://<account>.blob.core.windows.net`, and the immutable container.
    pub blob_endpoint: Option<String>,
    pub merkle_container: String,
    /// `https://<vault>.vault.azure.net` or a Managed HSM, and the signing key.
    pub key_vault_url: Option<String>,
    pub merkle_key: String,
    /// `https://<namespace>.servicebus.windows.net`, and the event hub.
    pub event_hubs_endpoint: Option<String>,
    pub event_hub: String,
}

pub enum Store {
    Azure(BlobContainer),
    Memory(MemoryWormStore),
}

impl WormStore for Store {
    async fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        match self {
            Store::Azure(s) => s.put_new(name, content_type, body).await,
            Store::Memory(s) => s.put_new(name, content_type, body).await,
        }
    }
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        match self {
            Store::Azure(s) => s.get(name).await,
            Store::Memory(s) => s.get(name).await,
        }
    }
}

pub enum Signer {
    Azure(KeyVaultSigner),
    Dev(DevSigner),
}

impl RootSigner for Signer {
    async fn sign(&self, message: &[u8]) -> Result<Signature, AzureError> {
        match self {
            Signer::Azure(s) => s.sign(message).await,
            Signer::Dev(s) => s.sign(message).await,
        }
    }
}

pub enum Sink {
    Azure(EventHubsSink),
    Memory(MemorySink),
}

impl EventSink for Sink {
    async fn publish(&self, partition: PartitionId, events: &[EventRecord]) -> Result<(), AzureError> {
        match self {
            Sink::Azure(s) => s.publish(partition, events).await,
            Sink::Memory(s) => s.publish(partition, events).await,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CloseReport {
    pub closed: NaiveDate,
    pub business_date: Option<NaiveDate>,
    pub breaks: Vec<Break>,
    pub published_root: Option<PublishedRoot>,
    pub publish_error: Option<String>,
}

pub struct Engine {
    pub cluster: Cluster<BoxLog>,
    pub view: Projections,
    pub matcher: Matcher,
    pub store: Store,
    pub signer: Signer,
    pub sink: Sink,
    /// Events applied to the read models but not yet accepted by the sink,
    /// per partition. The relay retries until the sink takes them.
    unpublished: BTreeMap<PartitionId, Vec<EventRecord>>,
    /// The last sequence the sink accepted, per partition, and the file
    /// beside the logs where that is kept across restarts.
    published: BTreeMap<PartitionId, u64>,
    published_file: Option<PathBuf>,
}

impl Engine {
    pub fn start(cfg: &EngineConfig) -> Result<Self, String> {
        let mut logs: BTreeMap<PartitionId, BoxLog> = BTreeMap::new();
        for p in 0..cfg.partitions {
            let log: BoxLog = match &cfg.data_dir {
                Some(dir) => {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                    Box::new(FileLog::open(dir.join(format!("partition-{p:02}.log"))).map_err(|e| e.to_string())?)
                }
                None => Box::new(MemoryLog::new()),
            };
            logs.insert(p, log);
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let mut cluster = Cluster::recover(logs, seed).map_err(|e| e.to_string())?;
        cluster.keep_journal = false;
        let credential = || AzureCredential::from_env().map_err(|e| e.to_string());
        let a = &cfg.azure;
        let store = match &a.blob_endpoint {
            Some(e) => Store::Azure(BlobContainer::new(e, &a.merkle_container, credential()?)),
            None => Store::Memory(MemoryWormStore::default()),
        };
        let signer = match &a.key_vault_url {
            Some(v) => Signer::Azure(KeyVaultSigner::new(v, &a.merkle_key, credential()?)),
            None => Signer::Dev(DevSigner),
        };
        let sink = match &a.event_hubs_endpoint {
            Some(e) => Sink::Azure(EventHubsSink::new(e, &a.event_hub, credential()?)),
            None => Sink::Memory(MemorySink::default()),
        };
        let mut engine = Self {
            cluster,
            view: Projections::new(),
            matcher: Matcher::default(),
            store,
            signer,
            sink,
            unpublished: BTreeMap::new(),
            published: BTreeMap::new(),
            published_file: cfg.data_dir.as_ref().map(|dir| dir.join("published.json")),
        };
        // Rebuild the read models from the replayed stream. What the sink
        // had accepted before the restart is on record beside the logs;
        // anything after that record is sent again. Consumers de-duplicate
        // on (partition, sequence).
        if let Some(file) = &engine.published_file {
            match std::fs::read(file) {
                Ok(bytes) => engine.published = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", file.display())),
            }
        }
        engine.project();
        let published = &engine.published;
        for (partition, events) in &mut engine.unpublished {
            let through = published.get(partition).copied().unwrap_or(0);
            events.retain(|e| e.seq > through);
        }
        if engine.cluster.business_date().is_none() {
            engine.cluster.open(cfg.opening_date).map_err(|e| e.to_string())?;
            if cfg.load_shipped_config {
                for cmd in ic_sim::config::shipped_activations().map_err(|e| e.to_string())? {
                    engine.cluster.activate_everywhere(cmd).map_err(|e| e.to_string())?;
                }
            }
            engine.project();
        }
        Ok(engine)
    }

    fn project(&mut self) {
        for e in self.cluster.take_events() {
            if let Err(err) = self.view.apply(&e) {
                tracing::error!(error = %err, "read model gap");
            }
            self.unpublished.entry(e.partition).or_default().push(e);
        }
    }

    /// Relay the outbox to the event stream. At least once: a failed batch
    /// stays queued and is sent again.
    pub async fn relay(&mut self) {
        for (partition, events) in std::mem::take(&mut self.unpublished) {
            if events.is_empty() {
                continue;
            }
            match self.sink.publish(partition, &events).await {
                Ok(()) => {
                    if let Some(last) = events.last() {
                        self.published.insert(partition, last.seq);
                    }
                }
                Err(err) => {
                    tracing::warn!(partition, error = %err, "event relay failed; will retry");
                    self.unpublished.insert(partition, events);
                }
            }
        }
        self.record_published();
    }

    /// Keep the relay's position beside the logs. If this fails the events
    /// are sent again after a restart, which consumers tolerate.
    fn record_published(&self) {
        let Some(file) = &self.published_file else {
            return;
        };
        let tmp = file.with_extension("tmp");
        let written = serde_json::to_vec(&self.published)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, file));
        if let Err(error) = written {
            tracing::warn!(%error, "the event relay's position was not saved");
        }
    }

    /// Propose a command and bring the read models and the stream up to date.
    pub async fn submit(&mut self, partition: PartitionId, key: &str, caller: &str, command: Command) -> Applied {
        let applied = self.cluster.submit_keyed(partition, key, caller, command);
        self.cluster.relay_messages(|_| Delivery::Deliver);
        self.project();
        self.relay().await;
        applied
    }

    /// Cutoff, end-of-day phases, reconciliation, and the day's Merkle root
    /// signed and published to write-once storage.
    pub async fn end_of_day(&mut self) -> Result<CloseReport, String> {
        let close = self.cluster.end_of_day().map_err(|e| e.to_string())?;
        self.cluster.relay_messages(|_| Delivery::Deliver);
        self.project();
        self.relay().await;
        let (published_root, publish_error) = match publish_root(&self.signer, &self.store, close.closed, &close.heads).await {
            Ok(p) => (Some(p), None),
            Err(e) => {
                tracing::error!(error = %e, date = %close.closed, "merkle root not published");
                (None, Some(e.to_string()))
            }
        };
        Ok(CloseReport {
            closed: close.closed,
            business_date: self.cluster.business_date(),
            breaks: close.breaks,
            published_root,
            publish_error,
        })
    }
}
