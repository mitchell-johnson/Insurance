//! The end-of-day steps, done for real against the ledger nodes.

use std::time::Duration;

use chrono::NaiveDate;
use ic_azure::blob::WormStore;
use ic_azure::keyvault::RootSigner;
use ic_azure::merkle::{publish_root, root_name};
use ic_core::invest::{Executable, InvestCommand, Question, Replayable, Reply};
use ic_core::machine::{ClosedDay, MAX_EOD_CHUNK};
use ic_core::{Command, Envelope, PolicyId};
use ic_ledger::PartitionId;
use ic_raft::client::{fresh_salt, ClientError, ClusterClient, EndOfDayView};
use sha2::{Digest, Sha256};

use crate::eod::{PartitionCheck, PartitionStep, PublishStep, PublishedSummary, RoundStep, Steps, UnitPass, UnitStep};

/// Unit orders executed or replayed by one command. Small enough that the
/// command applies well inside a node's apply budget.
pub const UNIT_CHUNK: u32 = 200;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepError {
    /// The ledger or the store could not be reached. Try the step again.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// The ledger answered and the answer is no. Trying again will not help.
    #[error("refused: {0}")]
    Refused(String),
}

impl StepError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, StepError::Unavailable(_))
    }
}

impl From<ClientError> for StepError {
    fn from(e: ClientError) -> Self {
        StepError::Unavailable(e.to_string())
    }
}

/// The steps against a cluster of ledger nodes, a signer and a write-once store.
pub struct LedgerSteps<S, W> {
    client: ClusterClient,
    signer: S,
    store: W,
    /// How long one step keeps retrying the ledger before it gives the
    /// orchestrator the failure. Long enough to ride out a leader election.
    patience: Duration,
}

/// The idempotency key of a chunk of policy work. It names the day, the
/// partition, the round and the exact policies, so a repeated step sends the
/// same keys and the ledger applies each chunk once.
pub fn chunk_key(closing: NaiveDate, partition: PartitionId, round: u32, policies: &[PolicyId]) -> String {
    let mut h = Sha256::new();
    for id in policies {
        h.update(id.0.as_bytes());
        h.update([0]);
    }
    format!("eod:{closing}:p{partition}:r{round}:{}", hex::encode(&h.finalize()[..12]))
}

pub fn cutoff_key(closing: NaiveDate, partition: PartitionId) -> String {
    format!("eod:{closing}:p{partition}:cutoff")
}

impl<S: RootSigner, W: WormStore> LedgerSteps<S, W> {
    pub fn new(client: ClusterClient, signer: S, store: W) -> Self {
        Self {
            client,
            signer,
            store,
            patience: Duration::from_secs(20),
        }
    }

    pub fn with_patience(mut self, patience: Duration) -> Self {
        self.patience = patience;
        self
    }

    pub fn store(&self) -> &W {
        &self.store
    }

    async fn view(&self, partition: PartitionId) -> Result<EndOfDayView, StepError> {
        let deadline = tokio::time::Instant::now() + self.patience;
        loop {
            match self.client.end_of_day_view(partition).await {
                Ok(view) => return Ok(view),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e.into()),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    async fn propose(&self, partition: PartitionId, key: String, caller: &str, command: Command) -> Result<(), StepError> {
        let envelope = Envelope {
            key,
            caller: caller.to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        };
        let reply = self.client.propose_until(partition, &envelope, self.patience).await?;
        match reply.rejected {
            None => Ok(()),
            Some(reject) => Err(StepError::Refused(format!("{}: {reject}", envelope.key))),
        }
    }

    /// A question to the partition's investment ledger, asked until it is
    /// answered or patience runs out.
    async fn invest(&self, partition: PartitionId, question: Question) -> Result<Reply, StepError> {
        let deadline = tokio::time::Instant::now() + self.patience;
        loop {
            match self.client.investment(partition, &question).await {
                Ok(reply) => return Ok(reply),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e.into()),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// What the unit order run has to do on a partition: the funds and
    /// dates with a price in force and orders pending, and the superseded
    /// prices with orders still on them.
    async fn unit_work(&self, partition: PartitionId) -> Result<(Vec<Executable>, Vec<Replayable>), StepError> {
        let executable = match self.invest(partition, Question::Executable).await? {
            Reply::Executable(list) => list,
            other => {
                return Err(StepError::Refused(format!(
                    "partition {partition} answered {other:?} when asked what is executable"
                )))
            }
        };
        let replayable = match self.invest(partition, Question::ToReplay).await? {
            Reply::ToReplay(list) => list,
            other => {
                return Err(StepError::Refused(format!(
                    "partition {partition} answered {other:?} when asked what is to replay"
                )))
            }
        };
        Ok((executable, replayable))
    }

    /// The view of a partition whose cutoff for `closing` has happened.
    async fn view_after_cutoff(&self, partition: PartitionId, closing: NaiveDate) -> Result<EndOfDayView, StepError> {
        let view = self.view(partition).await?;
        if view.business_date.and_then(|d| d.pred_opt()) == Some(closing) {
            Ok(view)
        } else {
            Err(StepError::Refused(format!(
                "partition {partition} is on {:?}; this run is closing {closing}",
                view.business_date
            )))
        }
    }
}

impl<S: RootSigner, W: WormStore> Steps for LedgerSteps<S, W> {
    type Error = StepError;

    async fn cutoff(&self, step: PartitionStep) -> Result<ClosedDay, StepError> {
        let PartitionStep { partition, closing } = step;
        let view = self.view(partition).await?;
        if view.business_date == Some(closing) {
            let to = closing
                .succ_opt()
                .ok_or_else(|| StepError::Refused("the business date is out of range".to_string()))?;
            self.propose(partition, cutoff_key(closing, partition), "leader", Command::AdvanceBusinessDate { to })
                .await?;
        }
        // Whether this call advanced the date or an earlier attempt did, the
        // ledger itself holds the head at the cutoff.
        let view = self.view_after_cutoff(partition, closing).await?;
        match view.last_close {
            Some(closed) if closed.date == closing => Ok(closed),
            other => Err(StepError::Refused(format!(
                "partition {partition} has no close recorded for {closing}: {other:?}"
            ))),
        }
    }

    async fn run_round(&self, step: RoundStep) -> Result<u32, StepError> {
        let RoundStep { partition, closing, round } = step;
        let view = self.view_after_cutoff(partition, closing).await?;
        // The first round visits every open policy; later rounds only those
        // with more to do.
        let todo = if round == 0 { view.open } else { view.unfinished };
        for chunk in todo.chunks(MAX_EOD_CHUNK) {
            let command = Command::RunEndOfDay { policies: chunk.to_vec() };
            self.propose(partition, chunk_key(closing, partition, round, chunk), "end-of-day", command)
                .await?;
        }
        Ok(self.view_after_cutoff(partition, closing).await?.unfinished.len() as u32)
    }

    async fn unit_orders(&self, step: UnitStep) -> Result<UnitPass, StepError> {
        let UnitStep { partition, closing, .. } = step;
        let before = self.unit_work(partition).await?;
        for slot in &before.0 {
            let command = InvestCommand::ExecuteOrders {
                fund: slot.fund.clone(),
                pricing_date: slot.pricing_date,
                limit: UNIT_CHUNK,
            };
            let key = format!("units:{closing}:p{partition}:x:{}:{}:{}", slot.price_id, slot.first, slot.pending);
            self.propose(partition, key, "unit-orders", Command::Investment(Box::new(command))).await?;
        }
        for price in &before.1 {
            let command = InvestCommand::ReplayOrders {
                superseded: price.superseded.clone(),
                limit: UNIT_CHUNK,
                materiality_minor: price.compensation_minimum_minor,
            };
            let key = format!("units:{closing}:p{partition}:r:{}:{}", price.superseded, price.orders);
            self.propose(partition, key, "unit-orders", Command::Investment(Box::new(command))).await?;
        }
        let after = self.unit_work(partition).await?;
        let left = after.0.iter().map(|s| s.pending).sum::<u32>() + after.1.iter().map(|p| p.orders).sum::<u32>();
        Ok(UnitPass {
            progressed: after != before,
            left,
        })
    }

    async fn reconcile(&self, step: PartitionStep) -> Result<PartitionCheck, StepError> {
        let view = self.view_after_cutoff(step.partition, step.closing).await?;
        Ok(PartitionCheck {
            breaks: view.breaks,
            transfer_in_transit: view.transfer_in_transit,
            transfer_in_flight: view.transfer_in_flight,
        })
    }

    async fn publish(&self, step: PublishStep) -> Result<PublishedSummary, StepError> {
        match publish_root(&self.signer, &self.store, step.closing, &step.heads).await {
            Ok(published) => Ok(PublishedSummary {
                name: root_name(step.closing),
                root: published.root,
            }),
            // A different root already published for this day: closed days
            // never change, so this is for a person, not for a retry.
            Err(e @ ic_azure::AzureError::AlreadyExists(_)) => Err(StepError::Refused(e.to_string())),
            Err(e) => Err(StepError::Unavailable(e.to_string())),
        }
    }
}
