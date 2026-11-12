//! The driver's ports on the deployed system: the module's group and the
//! core's partitions through [`ClusterClient`], and Azure Blob Storage for
//! the administrator's inbound and outbound containers.
//!
//! The administrator writes valuation files over HTTPS under its own Entra
//! identity, allowed only to write under `valuations/` (not SFTP: Blob
//! Storage's SFTP needs a hierarchical namespace, which has no index tags),
//! and reads extracts from the outbound account. Microsoft Defender for
//! Storage scans each inbound blob as it lands and writes its result as an
//! index tag; nothing is read until the tag says it is clean.

use std::time::Duration;

use chrono::NaiveDate;
use ic_azure::blob::{BlobContainer, WormStore, MALWARE_SCAN_CLEAN, MALWARE_SCAN_TAG};
use ic_azure::credential::{AzureCredential, SCOPE_STORAGE};
use ic_azure::AzureError;
use ic_core::invest::{Question, Reply};
use ic_ledger::PartitionId;
use ic_money::FundId;
use ic_raft::ClusterClient;
use serde_json::Value;

use crate::command::{Envelope, Output};
use crate::driver::{Core, Inbox, Module, Outbound, Scan};
use crate::host::NAME;

/// The module's replicated group.
#[derive(Clone)]
pub struct ClusterModule {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterModule {
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Module for ClusterModule {
    async fn propose(&self, envelope: &Envelope) -> Result<Output, String> {
        let command = serde_json::to_value(envelope).map_err(|e| e.to_string())?;
        let reply = self
            .client
            .propose_module_until(NAME, &command, self.patience)
            .await
            .map_err(|e| e.to_string())?;
        let value = reply.result?;
        serde_json::from_value(value).map_err(|e| format!("not an answer of the module: {e}"))
    }

    async fn ask(&self, question: &Value) -> Result<Value, String> {
        self.client.query_module(NAME, question).await.map_err(|e| e.to_string())?
    }
}

/// The core's partitions.
#[derive(Clone)]
pub struct ClusterCore {
    client: ClusterClient,
    partitions: PartitionId,
}

impl ClusterCore {
    pub fn new(client: ClusterClient, partitions: PartitionId) -> Self {
        Self { client, partitions }
    }
}

impl Core for ClusterCore {
    fn partitions(&self) -> PartitionId {
        self.partitions
    }

    async fn units_on_issue(&self, partition: PartitionId, fund: &FundId) -> Result<(i64, Option<NaiveDate>), String> {
        let question = Question::UnitsOnIssue { fund: fund.clone() };
        match self.client.investment(partition, &question).await.map_err(|e| e.to_string())? {
            Reply::UnitsOnIssue(u) => Ok((
                i64::try_from(u.units_micro).map_err(|_| "units on issue out of range".to_string())?,
                u.executed_through,
            )),
            other => Err(format!("the core answered units on issue with {other:?}")),
        }
    }
}

/// The inbound container.
#[derive(Clone)]
pub struct BlobInbox {
    endpoint: String,
    container: String,
    credential: AzureCredential,
    blobs: BlobContainer,
    http: reqwest::Client,
}

const API_VERSION: &str = "2023-11-03";

impl BlobInbox {
    pub fn new(endpoint: &str, container: &str, credential: AzureCredential) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            container: container.to_string(),
            blobs: BlobContainer::new(endpoint, container, credential.clone()),
            credential,
            http: ic_azure::http_client(),
        }
    }
}

/// The blob names and the continuation marker of one page of a List Blobs answer.
pub fn parse_list(xml: &str) -> (Vec<String>, Option<String>) {
    fn all<'a>(s: &'a str, open: &str, close: &str) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len()..];
            let Some(end) = after.find(close) else { break };
            out.push(&after[..end]);
            rest = &after[end + close.len()..];
        }
        out
    }
    let unescape = |s: &str| {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    };
    let names = all(xml, "<Name>", "</Name>").into_iter().map(unescape).collect();
    let marker = all(xml, "<NextMarker>", "</NextMarker>").first().map(|m| unescape(m)).filter(|m| !m.is_empty());
    (names, marker)
}

impl Inbox for BlobInbox {
    async fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let token = self.credential.token(SCOPE_STORAGE).await.map_err(|e| e.to_string())?;
            let mut query = vec![
                ("restype", "container".to_string()),
                ("comp", "list".to_string()),
                ("prefix", prefix.to_string()),
            ];
            if let Some(m) = &marker {
                query.push(("marker", m.clone()));
            }
            let resp = self
                .http
                .get(format!("{}/{}", self.endpoint, self.container))
                .query(&query)
                .bearer_auth(token)
                .header("x-ms-version", API_VERSION)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let body = resp.text().await.map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("Blob Storage answered {status} listing {prefix}: {body}"));
            }
            let (page, next) = parse_list(&body);
            names.extend(page);
            match next {
                Some(m) => marker = Some(m),
                None => return Ok(names),
            }
        }
    }

    async fn scan(&self, name: &str) -> Result<Scan, String> {
        let tags = self.blobs.tags(name).await.map_err(|e| e.to_string())?;
        Ok(match tags.as_ref().and_then(|t| t.get(MALWARE_SCAN_TAG)) {
            None => Scan::Pending,
            Some(result) if result == MALWARE_SCAN_CLEAN => Scan::Clean,
            Some(_) => Scan::Malicious,
        })
    }

    async fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        self.blobs.get(name).await.map_err(|e| e.to_string())
    }
}

/// The outbound container, write-once.
#[derive(Clone)]
pub struct BlobOutbound(pub BlobContainer);

impl Outbound for BlobOutbound {
    async fn put_new(&self, name: &str, body: Vec<u8>) -> Result<(), String> {
        match self.0.put_new(name, "text/csv", body).await {
            Ok(()) | Err(AzureError::AlreadyExists(_)) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
