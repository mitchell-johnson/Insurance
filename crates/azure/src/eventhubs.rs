//! The event stream on Azure Event Hubs.
//!
//! One event hub with 64 partitions: ledger partition `n` publishes to hub
//! partition `n`, so order per ledger partition is preserved. Delivery is at
//! least once; consumers de-duplicate on the event ID and checkpoint on the
//! source sequence, so the hub is disposable and any stream can be
//! republished from the ledger. Consumers can read with the Kafka protocol
//! (Event Hubs exposes a Kafka endpoint) or AMQP.
//!
//! The relay sends with the REST batch API and an Entra token. It needs no
//! connection string and holds no state: what has been published is the
//! outbox's own cursor.

use std::future::Future;
use std::sync::{Arc, Mutex};

use ic_core::EventRecord;
use ic_ledger::PartitionId;
use serde_json::json;

use crate::credential::{AzureCredential, SCOPE_EVENT_HUBS};
use crate::{check, AzureError};

pub trait EventSink: Send + Sync {
    /// Publish one partition's events, in order. Either the whole batch is
    /// accepted or the call fails and the relay sends it again.
    fn publish(&self, partition: PartitionId, events: &[EventRecord]) -> impl Future<Output = Result<(), AzureError>> + Send;
}

#[derive(Clone)]
pub struct EventHubsSink {
    /// `https://<namespace>.servicebus.windows.net`.
    namespace: String,
    hub: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

/// Events per request. Keeps a batch well under the 1 MB limit.
const BATCH: usize = 200;

impl EventHubsSink {
    pub fn new(namespace: &str, hub: &str, credential: AzureCredential) -> Self {
        Self {
            namespace: namespace.trim_end_matches('/').to_string(),
            hub: hub.to_string(),
            credential,
            http: reqwest::Client::new(),
        }
    }
}

impl EventSink for EventHubsSink {
    async fn publish(&self, partition: PartitionId, events: &[EventRecord]) -> Result<(), AzureError> {
        for chunk in events.chunks(BATCH) {
            let token = self.credential.token(SCOPE_EVENT_HUBS).await?;
            let batch: Vec<_> = chunk
                .iter()
                .map(|e| {
                    json!({
                        "Body": serde_json::to_string(e).unwrap_or_default(),
                        // Consumers de-duplicate and detect gaps from these without parsing the body.
                        "UserProperties": { "event-id": e.id, "seq": e.seq, "ledger-partition": e.partition, "traceparent": e.traceparent },
                    })
                })
                .collect();
            let url = format!("{}/{}/partitions/{partition}/messages?api-version=2014-01&timeout=60", self.namespace, self.hub);
            let resp = self
                .http
                .post(url)
                .bearer_auth(token)
                .header("content-type", "application/vnd.microsoft.servicebus.json")
                .body(serde_json::to_vec(&batch).unwrap_or_default())
                .send()
                .await?;
            check("Event Hubs", resp).await?;
        }
        Ok(())
    }
}

/// Collects events in memory, for the insurance in a box and tests.
#[derive(Clone, Default)]
pub struct MemorySink {
    pub events: Arc<Mutex<Vec<EventRecord>>>,
}

impl EventSink for MemorySink {
    async fn publish(&self, _partition: PartitionId, events: &[EventRecord]) -> Result<(), AzureError> {
        self.events
            .lock()
            .map_err(|_| AzureError::Response("sink poisoned".to_string()))?
            .extend_from_slice(events);
        Ok(())
    }
}
