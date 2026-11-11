//! The end-of-day run as a sequence of idempotent steps.
//!
//! The order of the steps lives here, once. The Temporal workflow runs it
//! with each step as an activity; the tests run it directly against ledger
//! nodes and kill it between steps. Every step can be repeated any number of
//! times, by any worker, with the same effect as running it once. That, not
//! the orchestrator, is what makes a dying worker harmless.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::machine::ClosedDay;
use ic_ledger::{Hash32, PartitionId};
use ic_raft::client::BreakView;
use serde::{Deserialize, Serialize};

/// Rounds of policy work allowed per partition before the run is declared
/// stuck. A round bills one period per policy, so a real run needs a handful.
pub const MAX_ROUNDS: u32 = 1_000;

/// What to close. The workflow ID is derived from it, so a day has one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndOfDayInput {
    /// The business date being closed.
    pub closing: NaiveDate,
    pub partitions: PartitionId,
    /// How long one attempt of a step may run, in seconds. A worker that dies
    /// mid-step is noticed when this expires, and the step goes to another
    /// worker. Longer than the worker's own patience with the ledger.
    #[serde(default = "default_step_timeout_secs")]
    pub step_timeout_secs: u64,
}

pub const fn default_step_timeout_secs() -> u64 {
    60
}

impl EndOfDayInput {
    pub fn new(closing: NaiveDate, partitions: PartitionId) -> Self {
        Self {
            closing,
            partitions,
            step_timeout_secs: default_step_timeout_secs(),
        }
    }

    /// One workflow per business date: starting the same day twice attaches
    /// to the run that exists.
    pub fn workflow_id(&self) -> String {
        format!("end-of-day-{}", self.closing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionStep {
    pub partition: PartitionId,
    pub closing: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundStep {
    pub partition: PartitionId,
    pub closing: NaiveDate,
    pub round: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishStep {
    pub closing: NaiveDate,
    pub heads: BTreeMap<PartitionId, Hash32>,
}

/// A partition's reconciliation after its end-of-day work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionCheck {
    pub breaks: Vec<BreakView>,
    /// Net of the in-transit accounts, as a decimal string.
    pub transfer_in_transit: String,
    pub transfer_in_flight: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedSummary {
    /// Where the signed root was written.
    pub name: String,
    pub root: Hash32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndOfDayReport {
    pub closed: NaiveDate,
    /// Each partition's ledger head at the cutoff.
    pub heads: BTreeMap<PartitionId, Hash32>,
    /// Rounds of policy work each partition needed.
    pub rounds: BTreeMap<PartitionId, u32>,
    /// Reconciliation breaks. Any break is for a person to resolve before
    /// the next day's run.
    pub breaks: Vec<BreakView>,
    pub published: PublishedSummary,
}

/// The steps of the run. Each must be safe to repeat.
#[allow(async_fn_in_trait)]
pub trait Steps {
    type Error;

    /// Advance the partition's business date past `closing`, if it has not
    /// advanced already, and return the ledger head at the cutoff.
    async fn cutoff(&self, step: PartitionStep) -> Result<ClosedDay, Self::Error>;

    /// Run one round of policy work. Returns how many policies still need
    /// another round.
    async fn run_round(&self, step: RoundStep) -> Result<u32, Self::Error>;

    async fn reconcile(&self, step: PartitionStep) -> Result<PartitionCheck, Self::Error>;

    /// Sign the day's Merkle root and write it to write-once storage.
    async fn publish(&self, step: PublishStep) -> Result<PublishedSummary, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndOfDayError<E> {
    #[error(transparent)]
    Step(E),
    /// Policy work did not finish. A defect, not a transient fault: the run
    /// stops for a person to look.
    #[error("partition {partition} still has {unfinished} unfinished policies after {rounds} rounds")]
    Stuck { partition: PartitionId, rounds: u32, unfinished: u32 },
}

/// Close a business day on every partition.
pub async fn run_end_of_day<S: Steps>(steps: &S, input: &EndOfDayInput) -> Result<EndOfDayReport, EndOfDayError<S::Error>> {
    let closing = input.closing;
    // Phase 1: the cutoff, everywhere, before any of the day's work. From
    // here new business lands on the next date on every partition.
    let mut heads = BTreeMap::new();
    for partition in 0..input.partitions {
        let closed = steps.cutoff(PartitionStep { partition, closing }).await.map_err(EndOfDayError::Step)?;
        heads.insert(partition, closed.closing_head);
    }
    // Phases 2 to 5: billing, earning, arrears and housekeeping, in bounded
    // chunks, until no policy needs another pass.
    let mut rounds = BTreeMap::new();
    for partition in 0..input.partitions {
        let mut round = 0;
        loop {
            let unfinished = steps.run_round(RoundStep { partition, closing, round }).await.map_err(EndOfDayError::Step)?;
            round += 1;
            if unfinished == 0 {
                break;
            }
            if round >= MAX_ROUNDS {
                return Err(EndOfDayError::Stuck {
                    partition,
                    rounds: round,
                    unfinished,
                });
            }
        }
        rounds.insert(partition, round);
    }
    // Reconciliation: each partition's own rules, then the one rule that
    // spans partitions.
    let mut breaks = Vec::new();
    let mut in_transit: i128 = 0;
    let mut in_flight = false;
    for partition in 0..input.partitions {
        let check = steps.reconcile(PartitionStep { partition, closing }).await.map_err(EndOfDayError::Step)?;
        breaks.extend(check.breaks);
        in_flight |= check.transfer_in_flight;
        match check.transfer_in_transit.parse::<i128>() {
            Ok(net) => in_transit += net,
            Err(_) => breaks.push(BreakView {
                check: "transfer_in_transit".to_string(),
                detail: format!("partition {partition} reported an unreadable in-transit total"),
            }),
        }
    }
    if in_transit != 0 && !in_flight {
        breaks.push(BreakView {
            check: "transfer_in_transit".to_string(),
            detail: format!("in-transit accounts net to {in_transit} with no transfer in flight"),
        });
    }
    let published = steps
        .publish(PublishStep { closing, heads: heads.clone() })
        .await
        .map_err(EndOfDayError::Step)?;
    Ok(EndOfDayReport {
        closed: closing,
        heads,
        rounds,
        breaks,
        published,
    })
}
