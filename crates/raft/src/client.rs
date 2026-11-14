//! A client for a cluster of ledger nodes.
//!
//! A client knows the addresses of some nodes. It offers a command to one of
//! them; a follower answers with the leader's address and the client goes
//! there. On any failure the caller retries with the same idempotency key.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use ic_cashmatch::{Candidates, IncomingPayment, Matcher};
use ic_core::command::IssuePolicy;
use ic_core::machine::ClosedDay;
use ic_core::{Envelope, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_product::rating::ModalPremium;
use ic_readmodel::DirectoryEntry;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::auth::ClusterAuth;
use crate::failback::FailbackStatus;
use crate::module::{ModuleReply, ModuleStatus};
use crate::node::{NodeStatus, ProposeError};
use crate::query::{PolicyView, Query};
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
    /// Net of the claims offsets in transit, as a decimal string.
    #[serde(default)]
    pub claims_offset_in_transit: String,
    /// A claims message from this partition has not been delivered yet.
    #[serde(default)]
    pub claims_message_in_flight: bool,
}

/// A run of a partition's events, as its leader holds them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPage {
    /// The sequence number the run follows.
    pub after: u64,
    /// The events, in sequence. Empty when the reader is level.
    pub events: Vec<ic_core::EventRecord>,
    /// How far the partition has recorded this consumer as having its
    /// events. `None` for a consumer the partition keeps nothing for.
    pub acknowledged: Option<u64>,
}

/// An operator's request to hand a group's leadership to a voter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveLeader {
    pub to: crate::NodeId,
}

/// What a consumer tells a partition it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consumed {
    pub through_seq: u64,
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
    /// As `Unavailable`, for a module's group. Retry with the same command:
    /// the module's own idempotency key makes that safe.
    #[error("no node answered for module {module}: {detail}")]
    ModuleUnavailable { module: String, detail: String },
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
    /// Where each group's leader was last seen: a partition by its number,
    /// a module by `module-<name>`.
    leaders: Arc<Mutex<BTreeMap<String, String>>>,
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

    fn hint(&self, group: &str) -> Option<String> {
        self.leaders.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(group).cloned()
    }

    fn remember(&self, group: &str, addr: Option<&str>) {
        let mut leaders = self.leaders.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match addr {
            Some(a) => leaders.insert(group.to_string(), a.to_string()),
            None => leaders.remove(group),
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
        self.sweep_group(&partition.to_string(), build)
            .await
            .map_err(|detail| ClientError::Unavailable { partition, detail })
    }

    async fn sweep_module<T: DeserializeOwned>(&self, module: &str, build: impl Fn(&str) -> reqwest::RequestBuilder) -> Result<T, ClientError> {
        self.sweep_group(&format!("module-{module}"), build)
            .await
            .map_err(|detail| ClientError::ModuleUnavailable {
                module: module.to_string(),
                detail,
            })
    }

    /// The pass itself, for any group. The error says what the last node tried said.
    async fn sweep_group<T: DeserializeOwned>(&self, group: &str, build: impl Fn(&str) -> reqwest::RequestBuilder) -> Result<T, String> {
        // A stack: the last known leader is on top, then the nodes in the order given.
        let mut queue: Vec<String> = self.nodes.iter().rev().cloned().chain(self.hint(group)).collect();
        let mut tried = Vec::new();
        let mut detail = "no nodes configured".to_string();
        while let Some(addr) = queue.pop() {
            if tried.contains(&addr) {
                continue;
            }
            tried.push(addr.clone());
            match self.attempt(build(&addr)).await {
                Attempt::Done(body) => {
                    self.remember(group, Some(&addr));
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
        self.remember(group, None);
        Err(detail)
    }

    /// A partition's events for a consumer, from its leader: those after
    /// `after`, or after what the consumer last acknowledged when `after` is
    /// not given. The partition keeps events for a consumer only once the
    /// consumer is registered, which its first acknowledgement does.
    pub async fn events(&self, partition: PartitionId, consumer: &str, after: Option<u64>, max: usize) -> Result<EventPage, ClientError> {
        self.sweep(partition, |addr| {
            let request = self
                .http
                .get(format!("http://{addr}/v1/partitions/{partition}/events"))
                .query(&[("consumer", consumer)])
                .query(&[("max", max)]);
            match after {
                Some(after) => request.query(&[("after", after)]),
                None => request,
            }
        })
        .await
    }

    /// Record that a consumer holds a partition's events through
    /// `through_seq`, so the partition can let them go.
    pub async fn acknowledge_events(&self, partition: PartitionId, consumer: &str, through_seq: u64) -> Result<(), ClientError> {
        self.sweep(partition, |addr| {
            self.http
                .put(format!("http://{addr}/v1/partitions/{partition}/consumers/{consumer}"))
                .json(&Consumed { through_seq })
        })
        .await
    }

    /// Stop a partition keeping events for a consumer that no longer reads them.
    pub async fn unsubscribe(&self, partition: PartitionId, consumer: &str) -> Result<(), ClientError> {
        self.sweep(partition, |addr| {
            self.http.delete(format!("http://{addr}/v1/partitions/{partition}/consumers/{consumer}"))
        })
        .await
    }

    /// Offer a command to a module's leader. One pass: the caller decides
    /// how long to keep retrying. The command carries the module's own
    /// idempotency key, so offering it again is safe.
    pub async fn propose_module(&self, module: &str, command: &serde_json::Value) -> Result<ModuleReply, ClientError> {
        self.sweep_module(module, |addr| {
            self.http.post(format!("http://{addr}/v1/modules/{module}/commands")).json(command)
        })
        .await
    }

    /// Keep offering a module command until a leader confirms it or
    /// `patience` runs out.
    pub async fn propose_module_until(&self, module: &str, command: &serde_json::Value, patience: Duration) -> Result<ModuleReply, ClientError> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            match self.propose_module(module, command).await {
                Ok(reply) => return Ok(reply),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// Ask a module's leader a question. One pass. The inner result is the
    /// module's own answer or refusal.
    pub async fn query_module(&self, module: &str, question: &serde_json::Value) -> Result<Result<serde_json::Value, String>, ClientError> {
        self.sweep_module(module, |addr| self.http.post(format!("http://{addr}/v1/modules/{module}/query")).json(question))
            .await
    }

    /// Set the voters of a module's group: `id -> host:port`.
    pub async fn set_module_members(&self, module: &str, members: &BTreeMap<crate::NodeId, String>) -> Result<(), ClientError> {
        let patient = self.clone().with_attempt_timeout(Duration::from_secs(300));
        patient
            .sweep_module::<()>(module, |addr| {
                patient.http.put(format!("http://{addr}/v1/modules/{module}/members")).json(members)
            })
            .await
    }

    /// Hand a partition's leadership to a named voter.
    pub async fn move_leader(&self, partition: PartitionId, to: crate::NodeId) -> Result<(), ClientError> {
        self.sweep(partition, |addr| {
            self.http
                .post(format!("http://{addr}/v1/partitions/{partition}/leader"))
                .json(&MoveLeader { to })
        })
        .await
    }

    /// What fail-back is doing on one node. `action` is `None` to look,
    /// or `pause` or `resume` to tell that node to.
    pub async fn failback(&self, addr: &str, action: Option<&str>) -> Result<FailbackStatus, String> {
        let request = match action {
            Some(action) => self.http.post(format!("http://{addr}/v1/failback/{action}")),
            None => self.http.get(format!("http://{addr}/v1/failback")),
        };
        let response = self.auth.sign(request).timeout(self.attempt_timeout).send().await.map_err(|e| e.to_string())?;
        response.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())
    }

    /// Every hosted module's status as one node sees it.
    pub async fn module_status(&self, addr: &str) -> Result<Vec<ModuleStatus>, String> {
        let response = self
            .auth
            .sign(self.http.get(format!("http://{addr}/v1/modules")))
            .timeout(self.attempt_timeout)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        response.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())
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

    /// Ask a partition's leader a question. One pass. The answer's type is
    /// the question's; an answer of another shape is a failed attempt.
    async fn ask<T: DeserializeOwned>(&self, partition: PartitionId, query: &Query) -> Result<T, ClientError> {
        self.sweep(partition, |addr| {
            self.http.post(format!("http://{addr}/v1/partitions/{partition}/query")).json(query)
        })
        .await
    }

    /// The partition's business date, if it has been opened.
    pub async fn business_date(&self, partition: PartitionId) -> Result<Option<NaiveDate>, ClientError> {
        self.ask(partition, &Query::BusinessDate).await
    }

    /// A dry run of an application on the partition its group lives on.
    /// `None` while the partition's business date is not open.
    pub async fn quote(&self, partition: PartitionId, application: IssuePolicy) -> Result<Option<Result<ModalPremium, Reject>>, ClientError> {
        let query = Query::Quote {
            application: Box::new(application),
        };
        self.ask(partition, &query).await
    }

    /// A policy by its number, if this partition holds it.
    pub async fn policy_by_number(&self, partition: PartitionId, number: &str) -> Result<Option<PolicyView>, ClientError> {
        self.ask(partition, &Query::PolicyByNumber { number: number.to_string() }).await
    }

    /// The partition's directory entry for a policy, if it holds it.
    pub async fn locate(&self, partition: PartitionId, policy: &PolicyId) -> Result<Option<DirectoryEntry>, ClientError> {
        self.ask(partition, &Query::Locate { policy: policy.clone() }).await
    }

    /// A question for the claims held on a partition. `None` while the
    /// partition's business date is not open.
    pub async fn claims(
        &self,
        partition: PartitionId,
        question: &ic_core::claims_module::report::Question,
    ) -> Result<Option<ic_core::claims_module::report::Reply>, ClientError> {
        self.ask(partition, &Query::Claims { question: question.clone() }).await
    }

    /// A question for the investment ledger on a partition.
    pub async fn investment(&self, partition: PartitionId, question: &ic_core::invest::Question) -> Result<ic_core::invest::Reply, ClientError> {
        self.ask(partition, &Query::Investment { question: question.clone() }).await
    }

    /// Withheld tax of one kind a partition still holds, fund by fund.
    pub async fn withholding_held(
        &self,
        partition: PartitionId,
        kind: &str,
        currency: ic_money::Currency,
    ) -> Result<Vec<ic_core::remittance::HeldByFund>, ClientError> {
        let query = Query::WithholdingHeld {
            kind: kind.to_string(),
            currency,
        };
        self.ask(partition, &query).await
    }

    /// Underwriting loadings on a partition, a page of policies at a time.
    /// Pass the page's `next` as `after` until it is None.
    pub async fn loadings(
        &self,
        partition: PartitionId,
        after: Option<PolicyId>,
        limit: u32,
        as_at: Option<NaiveDate>,
    ) -> Result<ic_core::loadings::LoadingsPage, ClientError> {
        self.ask(partition, &Query::Loadings { after, limit, as_at }).await
    }

    /// What this partition's policies offer as matches for a payment.
    pub async fn payment_candidates(&self, partition: PartitionId, matcher: &Matcher, payment: &IncomingPayment) -> Result<Candidates, ClientError> {
        let query = Query::PaymentCandidates {
            matcher: matcher.clone(),
            payment: payment.clone(),
        };
        self.ask(partition, &query).await
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

    /// How many partitions the ledger has, as the nodes report it. Waits up
    /// to `patience` for a node to answer; nodes that disagree are an error.
    pub async fn partitions(&self, patience: Duration) -> Result<PartitionId, String> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            let mut counts = Vec::new();
            for addr in self.nodes.iter() {
                if let Ok(status) = self.status(addr).await {
                    counts.push(status.len());
                }
            }
            match counts.as_slice() {
                [] if tokio::time::Instant::now() >= deadline => return Err("no ledger node answered".to_string()),
                [] => tokio::time::sleep(Duration::from_millis(250)).await,
                [first, rest @ ..] if rest.iter().all(|c| c == first) => {
                    return PartitionId::try_from(*first).map_err(|e| format!("partition count: {e}"));
                }
                _ => return Err(format!("the nodes disagree about how many partitions there are: {counts:?}")),
            }
        }
    }

    /// Ask one node to form every partition's group from its configured
    /// peers. A group that already exists is left as it is.
    pub async fn init(&self, addr: &str) -> Result<(), String> {
        let response = self
            .auth
            .sign(self.http.post(format!("http://{addr}/v1/init")))
            .timeout(self.attempt_timeout)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        response.error_for_status().map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn nodes(&self) -> &[String] {
        &self.nodes
    }
}
