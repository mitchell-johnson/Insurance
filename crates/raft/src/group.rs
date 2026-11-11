//! What every Raft group on a node does the same way, whatever state machine
//! it drives: a partition's ledger or a module's state.

use std::collections::{BTreeMap, BTreeSet};

use openraft::async_runtime::WatchReceiver;
use openraft::errors::{ClientWriteError, ForwardToLeader, RaftError};
use openraft::storage::RaftStateMachine;
use openraft::{BasicNode, ServerState};

use crate::network::GroupOf;
use crate::node::ProposeError;
use crate::types::{NodeId, Proposal, Reply, TypeConfig};

fn refused(e: RaftError<TypeConfig, ClientWriteError<TypeConfig>>) -> ProposeError {
    match e {
        RaftError::APIError(ClientWriteError::ForwardToLeader(ForwardToLeader { leader_id, leader_node, .. })) => ProposeError::NotLeader {
            leader: leader_id,
            addr: leader_node.map(|n| n.addr),
        },
        other => ProposeError::Unknown(other.to_string()),
    }
}

/// Replicate a proposal and apply it. Returns once a majority has it durably
/// and this member has applied it.
pub(crate) async fn write<SM: RaftStateMachine<TypeConfig>>(raft: &GroupOf<SM>, proposal: Proposal) -> Result<Reply, ProposeError> {
    raft.client_write(proposal).await.map(|response| response.data).map_err(refused)
}

/// Change who the group's voters are. Members being added are first brought
/// up to date as learners, so the group never depends on a member that is
/// still catching up; then the voter set is switched in one logged step.
pub(crate) async fn set_members<SM: RaftStateMachine<TypeConfig>>(raft: &GroupOf<SM>, members: BTreeMap<NodeId, String>) -> Result<(), ProposeError> {
    let voters: BTreeSet<NodeId> = raft.voter_ids().collect();
    for (id, addr) in &members {
        if !voters.contains(id) {
            raft.add_learner(*id, BasicNode::new(addr.clone()), true).await.map_err(refused)?;
        }
    }
    let wanted: BTreeSet<NodeId> = members.keys().copied().collect();
    if wanted != voters {
        raft.change_membership(wanted, false).await.map_err(refused)?;
    }
    Ok(())
}

/// The leader this member follows and where it listens, as the group's own
/// membership records it.
pub(crate) fn leader_hint<SM: RaftStateMachine<TypeConfig>>(raft: &GroupOf<SM>) -> (Option<NodeId>, Option<String>) {
    let metrics = raft.metrics().borrow_watched().clone();
    let leader = metrics.current_leader;
    let addr = leader.and_then(|l| metrics.membership_config.nodes().find(|(id, _)| **id == l).map(|(_, n)| n.addr.clone()));
    (leader, addr)
}

pub(crate) fn role(state: ServerState) -> &'static str {
    match state {
        ServerState::Leader => "leader",
        ServerState::Follower => "follower",
        ServerState::Candidate => "candidate",
        _ => "learner",
    }
}
