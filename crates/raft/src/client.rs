//! A client for a cluster of ledger nodes.
//!
//! A client knows the addresses of some nodes. It offers a command to one of
//! them; a follower answers with the leader's address and the client goes
//! there. On any failure the caller retries with the same idempotency key.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::machine::ClosedDay;
use ic_core::{Envelope, PolicyId};
use ic_ledger::PartitionId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::auth::ClusterAuth;
use crate::node::{NodeStatus, ProposeError};
use crate::query::{Answer, Query};
use crate::types::Reply;

/// A reconciliation break, as reported over the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakView {
    pub check: String,
    pub detail: String,
}

/// What the end-of-day orchestration needs to know about a partition, read
/// from the leader after it has confirmed it still leads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndOfDayView {
    pub partition: PartitionId,
    pub business_date: Option<NaiveDate>,
    pub last_close: Option<ClosedDay>,
    /// Every policy an end-of-day run must visit.
    pub open: Vec<PolicyId>,
    /// Policies the current run has not finished with.
    pub unfinished: Vec<PolicyId>,
    pub breaks: Vec<BreakView>,
    /// Net of the in-transit accounts, as a decimal string.
    pub transfer_in_transit: String,
    /// A policy is frozen mid-transfer on this partition.
    pub transfer_in_flight: bool,
}

/// What a node answers when it cannot serve a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub error: ProposeError,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// No node confirmed the request. It may or may not have been applied:
    /// retry with the same idempotency key.
    #[error("no node answered for partition {partition}: {detail}")]
    Unavailable { partition: PartitionId, detail: String },
}

/// A salt for a command's payload commitments, from the operating system's
/// random source. The proposer draws it; replicas never do.
pub fn fresh_salt() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone)]
pub struct ClusterClient {
    http: reqwest::Client,
    /// Node addresses as `host:port`.
    nodes: Arc<Vec<String>>,
    /// Where each partition's leader was last seen.
    leaders: Arc<Mutex<BTreeMap<PartitionId, String>>>,
    attempt_timeout: Duration,
    auth: ClusterAuth,
}

enum Attempt<T> {
    Done(T),
    Redirect(Option<String>),
    Failed(String),
}

impl ClusterClient {
    pub fn new(nodes: Vec<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            nodes: Arc::new(nodes),
            leaders: Arc::new(Mutex::new(BTreeMap::new())),
            attempt_timeout: Duration::from_secs(5),
            auth: ClusterAuth::open(),
        }
    }

    /// Parse a comma-separated list of `host:port` addresses.
    pub fn from_list(list: &str) -> Result<Self, String> {
        let nodes: Vec<String> = list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
        if nodes.is_empty() {
            return Err("no node addresses given".to_string());
        }
        Ok(Self::new(nodes))
    }

    /// Present this cluster token to the nodes.
    pub fn with_auth(mut self, auth: ClusterAuth) -> Self {
        self.auth = auth;
        self
    }

    pub fn with_attempt_timeout(mut self, timeout: Duration) -> Self {
        self.attempt_timeout = timeout;
        self
    }

    fn hint(&self, partition: PartitionId) -> Option<String> {
        self.leaders.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&partition).cloned()
    }

    fn remember(&self, partition: PartitionId, addr: Option<&str>) {
        let mut leaders = self.leaders.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match addr {
            Some(a) => leaders.insert(partition, a.to_string()),
            None => leaders.remove(&partition),
        };
    }

    async fn attempt<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Attempt<T> {
        let response = match self.auth.sign(request).timeout(self.attempt_timeout).send().await {
            Ok(r) => r,
            Err(e) => return Attempt::Failed(e.to_string()),
        };
        let status = response.status();
        if status.is_success() {
            return match response.json().await {
                Ok(body) => Attempt::Done(body),
                Err(e) => Attempt::Failed(e.to_string()),
            };
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Attempt::Failed("the node refused this client's cluster token".to_string());
        }
        match response.json::<Refusal>().await {
            Ok(Refusal {
                error: ProposeError::NotLeader { addr, .. },
            }) => Attempt::Redirect(addr),
            Ok(Refusal { error }) => Attempt::Failed(error.to_string()),
            Err(_) => Attempt::Failed(format!("status {status}")),
        }
    }

    /// One pass over the cluster: the last known leader first, then every
    /// node, following a redirect when a follower gives one.
    async fn sweep<T: DeserializeOwned>(&self, partition: PartitionId, build: impl Fn(&str) -> reqwest::RequestBuilder) -> Result<T, ClientError> {
        // A stack: the last known leader is on top, then the nodes in the order given.
        let mut queue: Vec<String> = self.nodes.iter().rev().cloned().chain(self.hint(partition)).collect();
        let mut tried = Vec::new();
        let mut detail = "no nodes configured".to_string();
        while let Some(addr) = queue.pop() {
            if tried.contains(&addr) {
                continue;
            }
            tried.push(addr.clone());
            match self.attempt(build(&addr)).await {
                Attempt::Done(body) => {
                    self.remember(partition, Some(&addr));
                    return Ok(body);
                }
                Attempt::Redirect(Some(leader)) => {
                    detail = format!("{addr} redirected to {leader}");
                    queue.push(leader);
                }
                Attempt::Redirect(None) => detail = format!("{addr} knows no leader"),
                Attempt::Failed(e) => detail = format!("{addr}: {e}"),
            }
        }
        self.remember(partition, None);
        Err(ClientError::Unavailable { partition, detail })
    }

    /// Offer a command to the partition's leader. One pass: the caller
    /// decides how long to keep retrying.
    pub async fn propose(&self, partition: PartitionId, envelope: &Envelope) -> Result<Reply, ClientError> {
        self.sweep(partition, |addr| {
            self.http.post(format!("http://{addr}/v1/partitions/{partition}/commands")).json(envelope)
        })
        .await
    }

    /// Keep offering a command, with the same key, until a leader confirms it
    /// or `patience` runs out.
    pub async fn propose_until(&self, partition: PartitionId, envelope: &Envelope, patience: Duration) -> Result<Reply, ClientError> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            match self.propose(partition, envelope).await {
                Ok(reply) => return Ok(reply),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    pub async fn end_of_day_view(&self, partition: PartitionId) -> Result<EndOfDayView, ClientError> {
        self.sweep(partition, |addr| self.http.get(format!("http://{addr}/v1/partitions/{partition}/end-of-day")))
            .await
    }

    /// Ask a partition's leader a question. One pass.
    pub async fn query(&self, partition: PartitionId, query: &Query) -> Result<Answer, ClientError> {
        self.sweep(partition, |addr| {
            self.http.post(format!("http://{addr}/v1/partitions/{partition}/query")).json(query)
        })
        .await
    }

    /// Set the voters of a partition's group: `id -> host:port`. Members
    /// being added are caught up before the switch. See [`crate::Node::set_members`].
    pub async fn set_members(&self, partition: PartitionId, members: &BTreeMap<crate::NodeId, String>) -> Result<(), ClientError> {
        // Catching a new member up can mean sending it the whole partition.
        let patient = self.clone().with_attempt_timeout(Duration::from_secs(300));
        patient
            .sweep::<()>(partition, |addr| {
                patient.http.put(format!("http://{addr}/v1/partitions/{partition}/members")).json(members)
            })
            .await
    }

    /// Every partition's status as one node sees it.
    pub async fn status(&self, addr: &str) -> Result<Vec<NodeStatus>, String> {
        let response = self
            .auth
            .sign(self.http.get(format!("http://{addr}/v1/status")))
            .timeout(self.attempt_timeout)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        response.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())
    }

    pub fn nodes(&self) -> &[String] {
        &self.nodes
    }
}
