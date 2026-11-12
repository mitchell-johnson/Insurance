//! The driver's ports as they are in production: the module and the core
//! on the cluster, reinsurance in its own replicated group, the event
//! stream on Azure Event Hubs, and quarantine storage on Azure Blob Storage
//! with Microsoft Defender for Storage scanning what lands there.

use std::time::Duration;

use chrono::NaiveDate;
use ic_azure::blob::{BlobContainer, WormStore, MALWARE_SCAN_CLEAN, MALWARE_SCAN_TAG};
use ic_azure::eventhubs::EventHubsSink;
use ic_azure::AzureError;
use ic_core::ids::home_partition;
use ic_core::{Command as CoreCommand, GroupId};
use ic_raft::ClusterClient;
use serde_json::{json, Value};

use crate::command::Envelope;
use crate::driver::{Core, DriverError, FileStore, Module, Peers, Scan};
use crate::event::{GroupEvent, ReinsuranceNotice, UnderwritingRequest};
use crate::host::{Question, MODULE};
use crate::state::Outcome;

/// The group module in its replicated group on the cluster.
#[derive(Clone)]
pub struct ClusterModule {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterModule {
    /// `patience` is how long one command keeps looking for a leader.
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Module for ClusterModule {
    async fn propose(&self, envelope: Envelope) -> Result<Outcome, DriverError> {
        let command = serde_json::to_value(&envelope).map_err(|e| DriverError::Answer(e.to_string()))?;
        let reply = self
            .client
            .propose_module_until(MODULE, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        serde_json::from_value(reply.result.map_err(DriverError::Answer)?).map_err(|e| DriverError::Answer(e.to_string()))
    }

    async fn ask(&self, question: Question) -> Result<Value, DriverError> {
        let question = serde_json::to_value(&question).map_err(|e| DriverError::Answer(e.to_string()))?;
        self.client
            .query_module(MODULE, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(DriverError::Answer)
    }
}

/// The core's partitions on the cluster.
#[derive(Clone)]
pub struct ClusterCore {
    client: ClusterClient,
    partitions: u16,
    patience: Duration,
}

impl ClusterCore {
    pub fn new(client: ClusterClient, partitions: u16, patience: Duration) -> Self {
        Self {
            client,
            partitions: partitions.max(1),
            patience,
        }
    }

    async fn propose(&self, partition: u16, key: &str, command: &CoreCommand) -> Result<Option<String>, String> {
        let envelope = ic_core::Envelope {
            key: key.to_string(),
            caller: crate::command::caller::STAFF.to_string(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command: command.clone(),
        };
        let reply = self
            .client
            .propose_until(partition, &envelope, self.patience)
            .await
            .map_err(|e| e.to_string())?;
        Ok(reply.rejected.map(|r| r.code))
    }
}

impl Core for ClusterCore {
    async fn submit(&self, key: &str, group: &GroupId, command: &CoreCommand) -> Result<Option<String>, String> {
        let home = home_partition(group) % self.partitions;
        let answer = self.propose(home, key, command).await?;
        if answer.as_deref() != Some("policy_transferred") {
            return Ok(answer);
        }
        // The group's policies were moved to another partition: the command
        // goes to whichever partition holds them now.
        for partition in (0..self.partitions).filter(|p| *p != home) {
            let elsewhere = self.propose(partition, key, command).await?;
            if !matches!(elsewhere.as_deref(), Some("unknown_policy" | "policy_transferred")) {
                return Ok(elsewhere);
            }
        }
        Ok(answer)
    }

    async fn business_date(&self) -> Result<Option<NaiveDate>, String> {
        // Not open until every partition is; the earliest is the date.
        let mut earliest: Option<NaiveDate> = None;
        for partition in 0..self.partitions {
            match self.client.business_date(partition).await.map_err(|e| e.to_string())? {
                None => return Ok(None),
                Some(date) => earliest = Some(earliest.map_or(date, |e| e.min(date))),
            }
        }
        Ok(earliest)
    }
}

/// The command the reinsurance module takes for a notice, in its own envelope.
pub fn reinsurance_envelope(key: &str, on: NaiveDate, notice: &ReinsuranceNotice) -> Value {
    let command = match notice {
        ReinsuranceNotice::RecordRiskFactors {
            policy,
            life,
            date_of_birth,
            sex,
            smoker,
        } => json!({ "record_risk_factors": {
            "policy": policy,
            "life": life,
            "factors": { "date_of_birth": date_of_birth, "sex": sex, "smoker": smoker },
        }}),
        other => json!(other),
    };
    json!({ "key": key, "caller": crate::command::caller::STAFF, "on": on, "command": command })
}

/// The event as it is put on the stream, with what a consumer needs to
/// order and de-duplicate it without reading the body.
pub fn event_message(seq: u64, date: NaiveDate, event: &GroupEvent) -> (Value, Value) {
    let body = json!({ "seq": seq, "date": date, "event": event });
    let kind = body["event"]["type"].as_str().unwrap_or_default().to_string();
    (body, json!({ "event-id": format!("group:{seq}"), "seq": seq, "module": MODULE, "type": kind }))
}

/// The other modules on the cluster and the event stream on Event Hubs.
#[derive(Clone)]
pub struct ClusterPeers {
    client: ClusterClient,
    patience: Duration,
    /// The name the reinsurance module is hosted under.
    reinsurance: String,
    events: EventHubsSink,
}

impl ClusterPeers {
    pub fn new(client: ClusterClient, patience: Duration, reinsurance: &str, events: EventHubsSink) -> Self {
        Self {
            client,
            patience,
            reinsurance: reinsurance.to_string(),
            events,
        }
    }
}

impl Peers for ClusterPeers {
    async fn reinsurance(&self, key: &str, on: NaiveDate, notice: &ReinsuranceNotice) -> Result<(), String> {
        let command = reinsurance_envelope(key, on, notice);
        let reply = self
            .client
            .propose_module_until(&self.reinsurance, &command, self.patience)
            .await
            .map_err(|e| e.to_string())?;
        // Applied is delivered, whatever the module made of it: its own
        // refusal is its to raise, and sending again would not change it.
        reply.result.map(|_| ()).map_err(|e| format!("reinsurance could not read the notice: {e}"))
    }

    async fn underwriting(&self, _key: &str, request: &UnderwritingRequest) -> Result<(), String> {
        // The underwriting module has no intake for a scheme member's
        // excess yet. The request stays in the outbox, counted as not
        // delivered, and is on the event stream as
        // `excess_underwriting_requested`.
        Err(format!("underwriting has no intake for scheme excess (case {})", request.case))
    }

    async fn publish(&self, seq: u64, date: NaiveDate, event: &GroupEvent) -> Result<(), String> {
        self.events.send_json(0, &[event_message(seq, date, event)]).await.map_err(|e| e.to_string())
    }
}

/// Quarantine storage: a container of its own, written by the portal's
/// upload and read only here. Defender for Storage scans each blob as it
/// lands and records the result as an index tag; nothing reads a blob
/// whose tag does not say it is clean.
#[derive(Clone)]
pub struct QuarantineBlobs {
    container: BlobContainer,
}

impl QuarantineBlobs {
    pub fn new(container: BlobContainer) -> Self {
        Self { container }
    }

    /// Scheme and file names come from outside: only plain names make a
    /// blob path.
    fn name(scheme: &str, file: &str) -> Result<String, String> {
        let plain = |s: &str| !s.is_empty() && s.len() <= 128 && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        if plain(scheme) && plain(file) {
            Ok(format!("{scheme}/{file}"))
        } else {
            Err("scheme and file names are letters, digits, hyphens, underscores and dots".to_string())
        }
    }
}

impl FileStore for QuarantineBlobs {
    async fn put(&self, scheme: &str, file: &str, bytes: Vec<u8>) -> Result<(), String> {
        match self.container.put_new(&Self::name(scheme, file)?, "application/octet-stream", bytes).await {
            // Already stored: the module decides whether it is the same upload.
            Ok(()) | Err(AzureError::AlreadyExists(_)) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn scan(&self, scheme: &str, file: &str) -> Result<Scan, String> {
        let tags = self.container.tags(&Self::name(scheme, file)?).await.map_err(|e| e.to_string())?;
        Ok(match tags.as_ref().and_then(|t| t.get(MALWARE_SCAN_TAG)) {
            None => Scan::Pending,
            Some(result) if result == MALWARE_SCAN_CLEAN => Scan::Clean,
            Some(_) => Scan::Malicious,
        })
    }

    async fn read(&self, scheme: &str, file: &str) -> Result<Option<Vec<u8>>, String> {
        self.container.get(&Self::name(scheme, file)?).await.map_err(|e| e.to_string())
    }
}
