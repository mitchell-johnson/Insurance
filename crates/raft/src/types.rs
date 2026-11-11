//! The types Raft replicates.

use std::fmt;

use ic_core::event::OutboundMessage;
use ic_core::{Applied, Envelope, EventRecord, Reject};
use serde::{Deserialize, Serialize};

pub type NodeId = u64;

/// One log entry's payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proposal {
    /// A command envelope, exactly as the core's own log would hold it.
    Command(Envelope),
    /// The leader's host recording that outputs have safely left the node,
    /// so every member can let go of them.
    Housekeeping(Housekeeping),
}

/// Outputs of applied commands are held by every member until the leader's
/// host has put them where they belong. These entries say it has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Housekeeping {
    /// Cross-partition messages the receiving partition has confirmed.
    Delivered { ids: Vec<String> },
    /// Journal entries up to this sequence number are sealed in the cold tier.
    Sealed { through_seq: u64 },
    /// Events up to this sequence number are on the event stream.
    Published { through_seq: u64 },
}

impl fmt::Display for Proposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The key and the command name only: payloads never go to logs.
            Proposal::Command(e) => write!(f, "{} from {} ({})", e.command.name(), e.caller, e.key),
            Proposal::Housekeeping(Housekeeping::Delivered { ids }) => write!(f, "delivered {} messages", ids.len()),
            Proposal::Housekeeping(Housekeeping::Sealed { through_seq }) => write!(f, "sealed journal through {through_seq}"),
            Proposal::Housekeeping(Housekeeping::Published { through_seq }) => write!(f, "published events through {through_seq}"),
        }
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
}

openraft::declare_raft_types!(
    /// The Raft configuration of a ledger partition group.
    pub TypeConfig:
        D = Proposal,
        R = Reply,
        Node = openraft::BasicNode,
);
