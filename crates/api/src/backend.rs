//! What the routes stand in front of.
//!
//! The same handlers serve two arrangements. In the insurance in a box,
//! every partition lives in this process ([`Engine`]). In production the
//! partitions are Raft groups on the ledger nodes and this process is a
//! stateless gateway to them ([`Gateway`]): it authenticates the caller,
//! decides which partition a request is for, and holds nothing a restart
//! could lose. Any number of gateways can run side by side.

use std::time::Duration;

use chrono::NaiveDate;
use futures::future::join_all;
use ic_cashmatch::{Candidates, IncomingPayment, Matcher, Outcome};
use ic_core::command::IssuePolicy;
use ic_core::{home_partition, Command, Envelope, GroupId, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_product::rating::ModalPremium;
use ic_raft::client::fresh_salt;
use ic_raft::client::ClientError;
use ic_raft::{ClusterClient, PolicyView};
use ic_readmodel::DirectoryEntry;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::engine::{CloseReport, Engine};

/// The ledger did not answer in time. Nothing is known about a write that
/// ends this way: the caller retries with the same idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the ledger did not answer: {0}")]
pub struct Unavailable(pub String);

/// What a write came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub log_index: u64,
    pub duplicate: bool,
    pub events: usize,
    pub rejected: Option<Reject>,
}

#[derive(Debug, thiserror::Error)]
pub enum CloseError {
    #[error("{0}")]
    Failed(String),
    /// With replicated nodes the close is a Temporal workflow, started by
    /// its schedule or by `ic-worker close`, so that it survives the loss of
    /// whatever started it.
    #[error("the close of a replicated ledger runs as the end-of-day workflow; start it with `ic-worker close <date> <partitions>`")]
    RunsAsWorkflow,
}

/// The reconciliation of the whole insurer as of now.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciliation {
    pub business_date: Option<NaiveDate>,
    pub breaks: Vec<Value>,
}

/// A gateway to a cluster of ledger nodes.
pub struct Gateway {
    pub client: ClusterClient,
    pub partitions: PartitionId,
    /// How long a request keeps trying the cluster, which covers a leader
    /// election, before it tells the caller to try again.
    pub patience: Duration,
    pub matcher: Matcher,
}

impl Gateway {
    pub fn new(settings: &crate::settings::ClusterSettings) -> Self {
        Self {
            client: ClusterClient::new(settings.nodes.clone()).with_auth(settings.auth.clone()),
            partitions: settings.partitions,
            patience: settings.patience,
            matcher: Matcher::default(),
        }
    }

    /// Check that the nodes hold the partitions this gateway was told of.
    /// The partition count decides where every group's policies live, so a
    /// gateway that disagreed with the nodes would file business in the
    /// wrong place. Waits up to `patience` for a node to answer.
    pub async fn verify(&self, patience: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + patience;
        let mut last = "no nodes configured".to_string();
        loop {
            for addr in self.client.nodes() {
                match self.client.status(addr).await {
                    Ok(statuses) if statuses.len() == usize::from(self.partitions) => return Ok(()),
                    Ok(statuses) => {
                        return Err(format!(
                            "node {addr} holds {} partitions and this gateway is configured for {}: set IC_PARTITIONS to match the nodes",
                            statuses.len(),
                            self.partitions
                        ))
                    }
                    Err(e) => last = format!("{addr}: {e}"),
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!("no ledger node answered: {last}"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Keep asking the cluster until it answers or patience runs out.
    async fn retrying<T, F>(&self, ask: impl Fn() -> F) -> Result<T, Unavailable>
    where
        F: std::future::Future<Output = Result<T, ClientError>>,
    {
        let deadline = tokio::time::Instant::now() + self.patience;
        loop {
            match ask().await {
                Ok(answer) => return Ok(answer),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(Unavailable(e.to_string())),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// Ask every partition at once. One partition that cannot answer fails
    /// the question: a partial answer about a policy number or a payment
    /// would be a wrong answer.
    async fn every_partition<T, F>(&self, ask: impl Fn(PartitionId) -> F) -> Result<Vec<T>, Unavailable>
    where
        F: std::future::Future<Output = Result<T, ClientError>>,
    {
        let ask = &ask;
        join_all((0..self.partitions).map(|p| self.retrying(move || ask(p))))
            .await
            .into_iter()
            .collect()
    }

    async fn reconciliation(&self) -> Result<Reconciliation, Unavailable> {
        let views = self.every_partition(|p| self.client.end_of_day_view(p)).await?;
        let mut breaks: Vec<Value> = views
            .iter()
            .flat_map(|v| {
                v.breaks
                    .iter()
                    .map(|b| json!({ "partition": v.partition, "check": b.check, "detail": b.detail }))
            })
            .collect();
        // In-transit across the insurer nets to the transfers still in flight.
        let mut in_transit: i128 = 0;
        for view in &views {
            match view.transfer_in_transit.parse::<i128>() {
                Ok(amount) => in_transit += amount,
                Err(e) => {
                    return Err(Unavailable(format!(
                        "partition {} reported an unreadable in-transit total: {e}",
                        view.partition
                    )))
                }
            }
        }
        if in_transit != 0 && !views.iter().any(|v| v.transfer_in_flight) {
            breaks.push(json!({ "check": "transfer_in_transit", "detail": format!("in-transit accounts net to {in_transit} with no transfer in flight") }));
        }
        Ok(Reconciliation {
            business_date: views.iter().filter_map(|v| v.business_date).min(),
            breaks,
        })
    }
}

pub enum Backend {
    /// The insurance in a box.
    Single(Box<Mutex<Engine>>),
    /// Replicated ledger nodes.
    Cluster(Box<Gateway>),
}

impl Backend {
    pub fn single(engine: Engine) -> Self {
        Self::Single(Box::new(Mutex::new(engine)))
    }

    pub fn cluster(gateway: Gateway) -> Self {
        Self::Cluster(Box::new(gateway))
    }

    pub async fn partitions(&self) -> PartitionId {
        match self {
            Self::Single(engine) => engine.lock().await.cluster.partitions(),
            Self::Cluster(gateway) => gateway.partitions,
        }
    }

    /// The partition a group's policies live on.
    pub async fn partition_of(&self, group: &GroupId) -> PartitionId {
        home_partition(group) % self.partitions().await
    }

    /// The business date: the earliest any partition is on.
    pub async fn business_date(&self) -> Result<Option<NaiveDate>, Unavailable> {
        match self {
            Self::Single(engine) => Ok(engine.lock().await.cluster.business_date()),
            Self::Cluster(gateway) => {
                let dates = gateway.every_partition(|p| gateway.client.business_date(p)).await?;
                // Not ready until every partition is open.
                Ok(dates.into_iter().collect::<Option<Vec<_>>>().and_then(|d| d.into_iter().min()))
            }
        }
    }

    /// A dry run of an application. `None` while the business date is not open.
    pub async fn quote(&self, application: IssuePolicy) -> Result<Option<Result<ModalPremium, Reject>>, Unavailable> {
        let partition = self.partition_of(&application.group).await;
        match self {
            Self::Single(engine) => {
                let engine = engine.lock().await;
                Ok(engine
                    .cluster
                    .business_date()
                    .map(|today| ic_core::quote(&engine.cluster.state(partition).catalog, application, today)))
            }
            Self::Cluster(gateway) => gateway.retrying(|| gateway.client.quote(partition, application.clone())).await,
        }
    }

    /// Put a command to a partition. The key makes it safe to repeat.
    pub async fn submit(&self, partition: PartitionId, key: &str, caller: &str, command: Command) -> Result<Written, Unavailable> {
        match self {
            Self::Single(engine) => {
                let applied = engine.lock().await.submit(partition, key, caller, command).await;
                Ok(Written {
                    log_index: applied.index,
                    duplicate: applied.duplicate,
                    events: applied.events.len(),
                    rejected: applied.result.err(),
                })
            }
            Self::Cluster(gateway) => {
                let envelope = Envelope {
                    key: key.to_string(),
                    caller: caller.to_string(),
                    salt: fresh_salt(),
                    traceparent: None,
                    command,
                };
                let reply = gateway
                    .client
                    .propose_until(partition, &envelope, gateway.patience)
                    .await
                    .map_err(|e| Unavailable(e.to_string()))?;
                Ok(Written {
                    log_index: reply.log_index,
                    duplicate: reply.duplicate,
                    events: reply.events.len(),
                    rejected: reply.rejected,
                })
            }
        }
    }

    /// Where a policy lives now.
    pub async fn locate(&self, policy: &PolicyId) -> Result<Option<DirectoryEntry>, Unavailable> {
        match self {
            Self::Single(engine) => Ok(engine.lock().await.view.policy(policy).cloned()),
            Self::Cluster(gateway) => {
                let found = gateway.every_partition(|p| gateway.client.locate(p, policy)).await?;
                Ok(found.into_iter().flatten().next())
            }
        }
    }

    /// Decide which policy a payment belongs to.
    pub async fn match_payment(&self, payment: &IncomingPayment) -> Result<Outcome, Unavailable> {
        match self {
            Self::Single(engine) => {
                let engine = engine.lock().await;
                Ok(engine.matcher.match_payment(&engine.view, payment))
            }
            Self::Cluster(gateway) => {
                let candidates = gateway
                    .every_partition(|p| gateway.client.payment_candidates(p, &gateway.matcher, payment))
                    .await?;
                let all = candidates.into_iter().fold(Candidates::default(), Candidates::merge);
                Ok(gateway.matcher.decide(all, payment))
            }
        }
    }

    /// A policy by its number, with its billing position.
    pub async fn policy(&self, number: &str) -> Result<Option<PolicyView>, Unavailable> {
        match self {
            Self::Single(engine) => {
                let engine = engine.lock().await;
                Ok(engine.view.by_number(number).and_then(|entry| {
                    let state = engine.cluster.state(entry.partition);
                    let policy = state.policies.get(&entry.policy)?;
                    Some(PolicyView {
                        entry: entry.clone(),
                        policy: policy.clone(),
                        billing: engine.view.billing(&entry.policy).cloned(),
                        business_date: state.business_date,
                    })
                }))
            }
            Self::Cluster(gateway) => {
                let found = gateway.every_partition(|p| gateway.client.policy_by_number(p, number)).await?;
                Ok(found.into_iter().flatten().next())
            }
        }
    }

    /// Ask every partition a claims question. A partition whose business
    /// date is not open has nothing to say.
    pub async fn claims(&self, question: &ic_core::claims_module::report::Question) -> Result<Vec<ic_core::claims_module::report::Reply>, Unavailable> {
        match self {
            Self::Single(engine) => {
                let engine = engine.lock().await;
                Ok((0..engine.cluster.partitions())
                    .filter_map(|p| {
                        let state = engine.cluster.state(p);
                        state.business_date.map(|today| state.claims.answer(question, today))
                    })
                    .collect())
            }
            Self::Cluster(gateway) => Ok(gateway
                .every_partition(|p| gateway.client.claims(p, question))
                .await?
                .into_iter()
                .flatten()
                .collect()),
        }
    }

    pub async fn end_of_day(&self) -> Result<CloseReport, CloseError> {
        match self {
            Self::Single(engine) => engine.lock().await.end_of_day().await.map_err(CloseError::Failed),
            Self::Cluster(_) => Err(CloseError::RunsAsWorkflow),
        }
    }

    pub async fn reconciliation(&self) -> Result<Reconciliation, Unavailable> {
        match self {
            Self::Single(engine) => {
                let engine = engine.lock().await;
                Ok(Reconciliation {
                    business_date: engine.cluster.business_date(),
                    breaks: engine.cluster.reconcile().iter().map(|b| json!(b)).collect(),
                })
            }
            Self::Cluster(gateway) => gateway.reconciliation().await,
        }
    }
}
