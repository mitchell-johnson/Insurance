//! The insurance in a box.
//!
//! [`Cluster`] runs every partition in one process, each a deterministic
//! state machine on a local log behind the consensus seam. It plays the parts
//! the production hosts play around the state machine: the outbox, the
//! cross-partition message relay, the end-of-day orchestrator and the daily
//! reconciliation and Merkle root.

pub mod config;
pub mod cutoff;
pub mod persona;
pub mod year;

use std::collections::{BTreeMap, VecDeque};

use chrono::NaiveDate;
use ic_core::event::{EventRecord, MessageBody, OutboundMessage};
use ic_core::recon::{self, Break};
use ic_core::{home_partition, Applied, Command, CoreState, Envelope, GroupId, PolicyId, Reject};
use ic_ledger::{merkle_root, Hash32, JournalEntry, PartitionId, PARTITION_COUNT};
use ic_log::{LogError, MemoryLog, OrderedLog, Replica};
use sha2::{Digest, Sha256};

pub struct Node<L: OrderedLog = MemoryLog> {
    pub replica: Replica<L, CoreState>,
    /// The journal as the host persisted it. Optional in large simulations.
    pub journal: Vec<JournalEntry>,
    /// Events not yet taken by a consumer.
    pub outbox: VecDeque<EventRecord>,
    /// Messages for other partitions not yet delivered.
    pub messages: VecDeque<OutboundMessage>,
    pub max_work_units: u32,
    pub commands: u64,
    pub entries: u64,
}

impl<L: OrderedLog> Node<L> {
    pub fn state(&self) -> &CoreState {
        &self.replica.state
    }
}

/// What a message relay does with one message: the fault injection point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Deliver,
    /// Deliver now and leave it in the outbox, so it arrives again.
    Duplicate,
    /// Not this time: stays in the outbox for the next pass.
    Delay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayClose {
    pub closed: NaiveDate,
    pub merkle_root: Hash32,
    pub heads: BTreeMap<PartitionId, Hash32>,
    pub breaks: Vec<Break>,
}

pub struct Cluster<L: OrderedLog = MemoryLog> {
    pub nodes: BTreeMap<PartitionId, Node<L>>,
    counter: u64,
    seed: u64,
    pub keep_journal: bool,
    /// Messages a receiver refused outright. Always empty in a healthy run.
    pub relay_errors: Vec<String>,
}

impl Cluster<MemoryLog> {
    /// A cluster of `partitions` in-memory partitions. Production has 64.
    pub fn in_memory(partitions: PartitionId, seed: u64) -> Self {
        let logs = (0..partitions).map(|p| (p, MemoryLog::new())).collect();
        match Self::recover(logs, seed) {
            Ok(c) => c,
            Err(_) => unreachable!("an empty in-memory log cannot fail to replay"),
        }
    }
}

impl<L: OrderedLog> Cluster<L> {
    /// Build a cluster over existing logs, replaying whatever they hold.
    pub fn recover(logs: BTreeMap<PartitionId, L>, seed: u64) -> Result<Self, LogError> {
        let mut nodes = BTreeMap::new();
        for (p, log) in logs {
            // Replaying rebuilds the outbox: read models and the relay start
            // from the beginning and de-duplicate, as they would in production.
            let mut outbox = VecDeque::new();
            let mut messages = VecDeque::new();
            let replica = Replica::recover_with(log, CoreState::new(p), |_, a: Applied| {
                outbox.extend(a.events);
                messages.extend(a.messages);
            })?;
            nodes.insert(
                p,
                Node {
                    replica,
                    journal: vec![],
                    outbox,
                    messages,
                    max_work_units: 0,
                    commands: 0,
                    entries: 0,
                },
            );
        }
        Ok(Self {
            nodes,
            counter: 0,
            seed,
            keep_journal: true,
            relay_errors: vec![],
        })
    }

    pub fn partitions(&self) -> PartitionId {
        self.nodes.len() as PartitionId
    }

    /// The partition a group lives on in this cluster.
    pub fn partition_of(&self, group: &GroupId) -> PartitionId {
        debug_assert!(self.partitions() <= PARTITION_COUNT);
        home_partition(group) % self.partitions()
    }

    pub fn state(&self, p: PartitionId) -> &CoreState {
        &self.nodes[&p].replica.state
    }

    pub fn business_date(&self) -> Option<NaiveDate> {
        self.nodes.values().next().and_then(|n| n.state().business_date)
    }

    /// Find a policy wherever it lives now.
    pub fn locate(&self, policy: &PolicyId) -> Option<PartitionId> {
        self.nodes
            .iter()
            .find(|(_, n)| n.state().policies.get(policy).is_some_and(|p| p.status != ic_core::PolicyStatus::Transferred))
            .map(|(p, _)| *p)
    }

    fn salt(&self, n: u64) -> String {
        // The proposer supplies the salt. Here it is derived from the seed so
        // a run is reproducible; production draws it from the OS.
        let mut h = Sha256::new();
        h.update(self.seed.to_be_bytes());
        h.update(n.to_be_bytes());
        hex::encode(&h.finalize()[..16])
    }

    /// Propose a command with a fresh idempotency key.
    pub fn submit(&mut self, partition: PartitionId, caller: &str, command: Command) -> Result<(), Reject> {
        self.counter += 1;
        let key = format!("sim-{}", self.counter);
        self.submit_keyed(partition, &key, caller, command).result
    }

    /// Propose a command with the caller's idempotency key.
    pub fn submit_keyed(&mut self, partition: PartitionId, key: &str, caller: &str, command: Command) -> Applied {
        self.counter += 1;
        let env = Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: self.salt(self.counter),
            traceparent: None,
            command,
        };
        let keep = self.keep_journal;
        let Some(node) = self.nodes.get_mut(&partition) else {
            return Applied {
                index: 0,
                result: Err(Reject::new("unknown_partition", format!("no partition {partition}"))),
                events: vec![],
                entries: vec![],
                messages: vec![],
                work_units: 0,
                duplicate: false,
            };
        };
        let applied = match node.replica.propose(env) {
            Ok((_, a)) => a,
            Err(e) => {
                return Applied {
                    index: 0,
                    result: Err(Reject::new("log", e.to_string())),
                    events: vec![],
                    entries: vec![],
                    messages: vec![],
                    work_units: 0,
                    duplicate: false,
                }
            }
        };
        // The host's atomic write: state, journal, events and messages together.
        node.commands += 1;
        node.entries += applied.entries.len() as u64;
        node.max_work_units = node.max_work_units.max(applied.work_units);
        if keep {
            node.journal.extend(applied.entries.iter().cloned());
        }
        node.outbox.extend(applied.events.iter().cloned());
        node.messages.extend(applied.messages.iter().cloned());
        applied
    }

    /// Send a configuration command to every partition (global activation).
    pub fn activate_everywhere(&mut self, command: Command) -> Result<(), Reject> {
        for p in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.submit(p, "config", command.clone())?;
        }
        Ok(())
    }

    pub fn open(&mut self, date: NaiveDate) -> Result<(), Reject> {
        for p in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.submit(p, "leader", Command::OpenBusinessDate { date })?;
        }
        Ok(())
    }

    /// Drain every partition's event outbox, in partition then sequence order.
    pub fn take_events(&mut self) -> Vec<EventRecord> {
        self.nodes.values_mut().flat_map(|n| n.outbox.drain(..).collect::<Vec<_>>()).collect()
    }

    /// One pass of the cross-partition relay: at-least-once delivery, with
    /// the message ID as the receiver's idempotency key.
    pub fn relay_messages(&mut self, mut fault: impl FnMut(&OutboundMessage) -> Delivery) -> usize {
        let mut delivered = 0;
        for p in self.nodes.keys().copied().collect::<Vec<_>>() {
            let pending: Vec<OutboundMessage> = self.nodes.get_mut(&p).map(|n| n.messages.drain(..).collect()).unwrap_or_default();
            for m in pending {
                let how = fault(&m);
                if how != Delivery::Deliver {
                    if let Some(n) = self.nodes.get_mut(&p) {
                        n.messages.push_back(m.clone());
                    }
                }
                if how == Delivery::Delay {
                    continue;
                }
                let command = match m.body.clone() {
                    MessageBody::TransferRecord(record) => Command::LandTransfer { record },
                    MessageBody::TransferLanded { policy, transfer_id } => Command::CompleteTransfer { policy, transfer_id },
                    MessageBody::TransferRejected { policy, transfer_id, .. } => Command::AbortTransfer { policy, transfer_id },
                };
                if let Err(e) = self.submit_keyed(m.to_partition, &m.id, "transfer", command).result {
                    self.relay_errors.push(format!("{}: {e}", m.id));
                }
                delivered += 1;
            }
        }
        delivered
    }

    pub fn messages_in_flight(&self) -> usize {
        self.nodes.values().map(|n| n.messages.len()).sum()
    }

    /// The business-date cutoff and the end-of-day phases on every partition,
    /// then reconciliation and the day's Merkle root.
    pub fn end_of_day(&mut self) -> Result<DayClose, Reject> {
        let closed = self.business_date().ok_or_else(|| Reject::new("business_date", "the cluster is not open"))?;
        let next = closed.succ_opt().ok_or_else(|| Reject::new("business_date", "date out of range"))?;
        let mut heads = BTreeMap::new();
        for p in self.nodes.keys().copied().collect::<Vec<_>>() {
            // Phase 1, owned by the core: the cutoff. The closing head is what
            // feeds the day's Merkle tree.
            heads.insert(p, self.state(p).ledger.head());
            self.submit(p, "leader", Command::AdvanceBusinessDate { to: next })?;
            // Phases 2 to 5, orchestrated outside the core (Temporal in
            // production): chunks of policies, idempotent, restartable.
            let mut todo = self.state(p).open_policy_ids();
            for _round in 0..10_000 {
                for chunk in todo.chunks(ic_core::machine::MAX_EOD_CHUNK) {
                    self.submit(p, "end-of-day", Command::RunEndOfDay { policies: chunk.to_vec() })?;
                }
                // A scheme invoice still being built, or a policy catching up
                // several periods, needs another step.
                let unfinished = self.state(p).end_of_day_unfinished();
                todo.retain(|id| unfinished.contains(id));
                if todo.is_empty() {
                    break;
                }
            }
        }
        let breaks = self.reconcile();
        Ok(DayClose {
            closed,
            merkle_root: merkle_root(closed, &heads),
            heads,
            breaks,
        })
    }

    /// Every partition's reconciliation, plus the checks that span partitions.
    pub fn reconcile(&self) -> Vec<Break> {
        let mut breaks: Vec<Break> = self.nodes.values().flat_map(|n| recon::reconcile(n.state())).collect();
        // In-transit across the insurer nets to the transfers still in flight.
        let in_transit: i128 = self.nodes.values().map(|n| recon::control_totals(n.state()).transfer_in_transit).sum();
        let frozen = self.nodes.values().any(|n| n.state().policies.values().any(ic_core::Policy::is_frozen));
        if in_transit != 0 && !frozen {
            breaks.push(Break {
                check: "transfer_in_transit",
                detail: format!("in-transit accounts net to {in_transit} with no transfer in flight"),
            });
        }
        breaks
    }
}
