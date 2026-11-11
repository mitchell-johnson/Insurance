//! The types Raft replicates.

use std::fmt;

use ic_core::event::OutboundMessage;
use ic_core::{Applied, Envelope, EventRecord, Reject};
use serde::{Deserialize, Serialize};

pub type NodeId = u64;

/// One log entry's payload: a command envelope, exactly as the core's own
/// log would hold it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal(pub Envelope);

impl fmt::Display for Proposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The key and the command name only: payloads never go to logs.
        write!(f, "{} from {} ({})", self.0.command.name(), self.0.caller, self.0.key)
    }
}

/// What applying a proposal produced, as returned to the proposer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Reply {
    /// The Raft log index the command was applied at. Zero for entries that
    /// carry no command (a new leader's blank entry, a membership change).
    pub log_index: u64,
    pub rejected: Option<Reject>,
    /// The idempotency key had been seen before: this is the original result.
    pub duplicate: bool,
    pub events: Vec<EventRecord>,
    pub messages: Vec<OutboundMessage>,
    /// Journal entries posted by the command.
    pub entries: u64,
    pub work_units: u32,
}

impl Reply {
    pub fn from_applied(a: Applied) -> Self {
        Self {
            log_index: a.index,
            rejected: a.result.err(),
            duplicate: a.duplicate,
            events: a.events,
            messages: a.messages,
            entries: a.entries.len() as u64,
            work_units: a.work_units,
        }
    }

    pub fn result(&self) -> Result<(), Reject> {
        match &self.rejected {
            Some(r) => Err(r.clone()),
            None => Ok(()),
        }
    }
}

openraft::declare_raft_types!(
    /// The Raft configuration of a ledger partition group.
    pub TypeConfig:
        D = Proposal,
        R = Reply,
        Node = openraft::BasicNode,
);
