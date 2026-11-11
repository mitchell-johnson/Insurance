//! One member of a partition's Raft group.

use openraft::async_runtime::WatchReceiver;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::Envelope;
use ic_ledger::PartitionId;
use openraft::errors::{ClientWriteError, RaftError};
use openraft::{BasicNode, Config, ServerState, SnapshotPolicy};
use serde::{Deserialize, Serialize};

use crate::machine::Machine;
use crate::network::{Network, RaftGroup};
use crate::store::LogStore;
use crate::types::{NodeId, Proposal, Reply};

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
}

pub struct Node {
    pub id: NodeId,
    pub partition: PartitionId,
    pub raft: RaftGroup,
    pub machine: Machine,
    pub log: LogStore,
}

/// Timings for a group whose members are a few milliseconds apart. A
/// cross-region group raises the heartbeat and election timeouts to match
/// its round-trip times.
pub fn config(cluster_name: &str, heartbeat_ms: u64) -> Result<Arc<Config>, String> {
    let config = Config {
        cluster_name: cluster_name.to_string(),
        heartbeat_interval: heartbeat_ms,
        election_timeout_min: heartbeat_ms * 4,
        election_timeout_max: heartbeat_ms * 8,
        // The log is the ledger's history; it is not compacted in this phase.
        snapshot_policy: SnapshotPolicy::Never,
        ..Config::default()
    };
    config.validate().map(Arc::new).map_err(|e| e.to_string())
}

impl Node {
    /// Start a member over its log. An existing log is replayed: the member
    /// comes back with the state it had and rejoins its group.
    pub async fn start(id: NodeId, partition: PartitionId, log: LogStore, network: Network, config: Arc<Config>) -> Result<Self, String> {
        let machine = Machine::new(partition);
        let raft = RaftGroup::new(id, config, network, log.clone(), machine.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            id,
            partition,
            raft,
            machine,
            log,
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

    /// Replicate a command and apply it. Returns once a majority has it
    /// durably and this member has applied it.
    pub async fn propose(&self, envelope: Envelope) -> Result<Reply, ProposeError> {
        let forward = |f: openraft::errors::ForwardToLeader<crate::types::TypeConfig>| ProposeError::NotLeader {
            leader: f.leader_id,
            addr: f.leader_node.map(|n| n.addr),
        };
        match self.raft.client_write(Proposal(envelope)).await {
            Ok(response) => Ok(response.data),
            Err(RaftError::APIError(ClientWriteError::ForwardToLeader(f))) => Err(forward(f)),
            Err(e) => Err(ProposeError::Unknown(e.to_string())),
        }
    }

    pub fn status(&self) -> NodeStatus {
        let metrics = self.raft.metrics().borrow_watched().clone();
        let role = match metrics.state {
            ServerState::Leader => "leader",
            ServerState::Follower => "follower",
            ServerState::Candidate => "candidate",
            _ => "learner",
        };
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
        }
    }

    /// Wait until this member has applied at least `index`, or time out.
    pub async fn wait_applied(&self, index: u64, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.machine.last_applied_index().is_none_or(|i| i < index) {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    /// Stop the member. Its log stays where it is.
    pub async fn shutdown(&self) {
        let _ = self.raft.shutdown().await;
    }
}
