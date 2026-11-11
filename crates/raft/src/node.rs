//! One member of a partition's Raft group.

use openraft::async_runtime::WatchReceiver;
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use ic_core::Envelope;
use ic_ledger::PartitionId;
use openraft::{BasicNode, Config, SnapshotPolicy};
use serde::{Deserialize, Serialize};

use crate::group;
use crate::machine::{Backlog, Machine};
use crate::network::{Network, RaftGroup};
use crate::peers::Identity;
use crate::store::LogStore;
use crate::types::{Housekeeping, NodeId, Proposal, Reply};

/// Why a proposal was not applied by this member. In every case the command
/// may be retried with the same idempotency key: the core applies it once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ProposeError {
    /// This member is not the leader. Retry against the leader, if one is known.
    #[error("not the leader; the leader is {leader:?}")]
    NotLeader { leader: Option<NodeId>, addr: Option<String> },
    /// The proposal was accepted but its fate is unknown: leadership changed
    /// or the group lost its quorum before it was confirmed.
    #[error("the outcome is unknown: {0}")]
    Unknown(String),
}

/// A member's view of itself, for health checks and for the resilience tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeStatus {
    pub id: NodeId,
    pub partition: PartitionId,
    /// `leader`, `follower`, `candidate` or `learner`.
    pub role: String,
    pub leader: Option<NodeId>,
    pub term: u64,
    pub last_log_index: Option<u64>,
    pub last_applied: Option<u64>,
    pub business_date: Option<NaiveDate>,
    /// Hash of the whole replicated state. Members that have applied the same
    /// entries report the same hash.
    pub state_hash: String,
    pub ledger_head: String,
    pub ledger_entries: u64,
    pub policies: u64,
    /// The group's voters as this member knows them.
    pub voters: Vec<NodeId>,
    /// The first log entry still held; earlier ones are behind a snapshot.
    pub first_log_index: Option<u64>,
    /// Output waiting for the leader's host to move it on.
    pub backlog: Backlog,
}

pub struct Node {
    pub id: NodeId,
    pub partition: PartitionId,
    pub raft: RaftGroup,
    pub machine: Machine,
    pub log: LogStore,
    pub identity: Identity,
}

/// Timings for a group whose members are a few milliseconds apart. A
/// cross-region group raises the heartbeat and election timeouts to match
/// its round-trip times.
pub fn config(cluster_name: &str, heartbeat_ms: u64, snapshot_every: u64) -> Result<Arc<Config>, String> {
    let config = Config {
        cluster_name: cluster_name.to_string(),
        heartbeat_interval: heartbeat_ms,
        election_timeout_min: heartbeat_ms * 4,
        election_timeout_max: heartbeat_ms * 8,
        // A snapshot once the log has grown this much; the log behind it is
        // then dropped, less a tail kept so a briefly absent member can
        // catch up from the log instead of being sent the whole state.
        snapshot_policy: SnapshotPolicy::LogsSinceLast(snapshot_every),
        max_in_snapshot_log_to_keep: snapshot_every / 5,
        // A snapshot is the whole partition; give it time to cross regions.
        install_snapshot_timeout: 120_000,
        // A member that was cut off asks whether it could win before it
        // raises the term, so its return does not unseat a healthy leader.
        enable_pre_vote: Some(true),
        ..Config::default()
    };
    config.validate().map(Arc::new).map_err(|e| e.to_string())
}

impl Node {
    /// Start a member over its log. An existing log is replayed: the member
    /// comes back with the state it had and rejoins its group.
    pub async fn start(
        identity: Identity,
        partition: PartitionId,
        log: LogStore,
        machine: Machine,
        network: Network,
        config: Arc<Config>,
    ) -> Result<Self, String> {
        let id = identity.id;
        let raft = RaftGroup::new(id, config, network, log.clone(), machine.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            id,
            partition,
            raft,
            machine,
            log,
            identity,
        })
    }

    /// Form a new group. Called once, on one member, when the group is created.
    /// Calling it on a group that already exists changes nothing.
    pub async fn initialize(&self, members: BTreeMap<NodeId, BasicNode>) -> Result<(), String> {
        if self.raft.is_initialized().await.map_err(|e| e.to_string())? {
            return Ok(());
        }
        self.raft.initialize(members).await.map_err(|e| e.to_string())
    }

    async fn write(&self, proposal: Proposal) -> Result<Reply, ProposeError> {
        group::write(&self.raft, proposal).await
    }

    /// Replicate a command and apply it. Returns once a majority has it
    /// durably and this member has applied it.
    pub async fn propose(&self, envelope: Envelope) -> Result<Reply, ProposeError> {
        self.write(Proposal::Command(envelope)).await
    }

    /// Change who the group's voters are. Members being added are first
    /// brought up to date as learners, so the group never depends on a member
    /// that is still catching up; then the voter set is switched in one
    /// logged step. Must be asked of the leader.
    ///
    /// This is how a failed node is replaced: start a new node with a new
    /// number and call this with the old number left out and the new one in.
    pub async fn set_members(&self, members: BTreeMap<NodeId, String>) -> Result<(), ProposeError> {
        group::set_members(&self.raft, members).await
    }

    /// Record, through the log, that outputs have left the node.
    pub async fn housekeep(&self, done: Housekeeping) -> Result<(), ProposeError> {
        self.write(Proposal::Housekeeping(done)).await.map(|_| ())
    }

    /// The leader this member follows and where it listens, as the group's
    /// own membership records it. A member added after this node was
    /// configured is therefore found too.
    pub fn leader_hint(&self) -> (Option<NodeId>, Option<String>) {
        group::leader_hint(&self.raft)
    }

    pub fn status(&self) -> NodeStatus {
        let metrics = self.raft.metrics().borrow_watched().clone();
        let role = group::role(metrics.state);
        let (business_date, state_hash, ledger_head, ledger_entries, policies) = self.machine.read(|s| {
            (
                s.business_date,
                s.state_hash().to_hex(),
                s.ledger.head().to_hex(),
                s.ledger.entry_count(),
                s.policies.len() as u64,
            )
        });
        NodeStatus {
            id: self.id,
            partition: self.partition,
            role: role.to_string(),
            leader: metrics.current_leader,
            term: metrics.current_term,
            last_log_index: metrics.last_log_index,
            last_applied: self.machine.last_applied_index(),
            business_date,
            state_hash,
            ledger_head,
            ledger_entries,
            policies,
            voters: self.raft.voter_ids().collect(),
            first_log_index: self.log.first_index(),
            backlog: self.machine.backlog(),
        }
    }

    /// Stop the member. Its log stays where it is.
    pub async fn shutdown(&self) {
        let _ = self.raft.shutdown().await;
    }
}
