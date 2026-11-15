//! `ic-admin watch`: the ledger's health as log lines the alert rules read.
//!
//! Run every five minutes by the `ic-watch` CronJob (deploy/k8s/admin.yaml).
//! It asks every node for its status and writes one line per thing a person
//! must look at, each with a fixed message the Azure Monitor alert rules
//! search for (infra/main.bicep, "alerts"):
//!
//! - `partition has no leader`: no node that answered names a leader for
//!   the partition, so it takes no writes.
//! - `ledger node unreachable`: a configured node did not answer.
//! - `housekeeping backlog`: a partition's leader holds more output than the
//!   limit (cross-partition messages, journal entries not sealed to Blob
//!   Storage, events not on Event Hubs), so a downstream service is failing.
//! - `state hashes differ`: two members that applied the same entries
//!   report different state, which is never expected.
//!
//! and one `ledger watch` line with the counts, every run, so that an alert
//! can also fire when the watch itself stops running.

use std::collections::{BTreeMap, BTreeSet};

use ic_ledger::PartitionId;
use ic_raft::NodeStatus;
use serde::Serialize;

/// The backlog above which a partition is reported, unless the command
/// names another.
pub const DEFAULT_BACKLOG_LIMIT: u64 = 5000;

/// What one run found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Watch {
    pub nodes: usize,
    pub unreachable: Vec<String>,
    pub partitions: usize,
    pub leaderless: Vec<PartitionId>,
    /// Partitions over the limit, with the leader's backlog (messages, journal, events).
    pub backlogged: BTreeMap<PartitionId, u64>,
    /// The largest backlog seen on any partition.
    pub largest_backlog: u64,
    /// Partitions where members at the same applied index report different state.
    pub diverged: Vec<PartitionId>,
}

impl Watch {
    /// Whether anything needs a person.
    pub fn healthy(&self) -> bool {
        self.unreachable.is_empty() && self.leaderless.is_empty() && self.backlogged.is_empty() && self.diverged.is_empty()
    }
}

/// What the statuses say, against the backlog limit.
pub fn findings(status: &BTreeMap<String, Result<Vec<NodeStatus>, String>>, backlog_limit: u64) -> Watch {
    let mut watch = Watch {
        nodes: status.len(),
        ..Watch::default()
    };
    let mut by_partition: BTreeMap<PartitionId, Vec<&NodeStatus>> = BTreeMap::new();
    for (addr, answer) in status {
        match answer {
            Ok(rows) => rows.iter().for_each(|row| by_partition.entry(row.partition).or_default().push(row)),
            Err(_) => watch.unreachable.push(addr.clone()),
        }
    }
    watch.partitions = by_partition.len();
    for (partition, rows) in &by_partition {
        if rows.iter().all(|r| r.leader.is_none()) {
            watch.leaderless.push(*partition);
        }
        // The leader's own backlog is the one that is moved on; a follower
        // holds the same output until the leader says it has gone.
        let leader = rows.iter().find(|r| r.role == "leader").or_else(|| rows.first());
        if let Some(l) = leader {
            let backlog = l.backlog.messages + l.backlog.journal + l.backlog.events;
            watch.largest_backlog = watch.largest_backlog.max(backlog);
            if backlog > backlog_limit {
                watch.backlogged.insert(*partition, backlog);
            }
        }
        let mut hashes: BTreeMap<Option<u64>, BTreeSet<&str>> = BTreeMap::new();
        for r in rows {
            hashes.entry(r.last_applied).or_default().insert(r.state_hash.as_str());
        }
        if hashes.values().any(|h| h.len() > 1) {
            watch.diverged.push(*partition);
        }
    }
    watch
}

/// Writes the lines the alert rules read.
pub fn log(watch: &Watch, backlog_limit: u64) {
    for node in &watch.unreachable {
        tracing::warn!(target: "ledger.watch", node = %node, "ledger node unreachable");
    }
    for partition in &watch.leaderless {
        tracing::error!(target: "ledger.watch", partition = *partition, "partition has no leader");
    }
    for (partition, backlog) in &watch.backlogged {
        tracing::warn!(target: "ledger.watch", partition = *partition, backlog = *backlog, limit = backlog_limit, "housekeeping backlog");
    }
    for partition in &watch.diverged {
        tracing::error!(target: "ledger.watch", partition = *partition, "state hashes differ");
    }
    tracing::info!(
        target: "ledger.watch",
        nodes = watch.nodes,
        unreachable = watch.unreachable.len(),
        partitions = watch.partitions,
        leaderless = watch.leaderless.len(),
        largest_backlog = watch.largest_backlog,
        healthy = watch.healthy(),
        "ledger watch"
    );
}
