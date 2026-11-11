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
    /// A command for a module's own group (see [`crate::module`]), as the
    /// module's JSON. A partition's group ignores it.
    Module(serde_json::Value),
    /// A run of one partition's events for a module's group, in sequence.
    /// `after` is the sequence number the run follows.
    Feed {
        partition: ic_ledger::PartitionId,
        after: u64,
        events: Vec<EventRecord>,
    },
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
    /// A consumer of the partition's events (a module's group) holds them up
    /// to this sequence number. The first one from a consumer registers it:
    /// from then on events are kept until it has them too.
    Consumed { consumer: String, through_seq: u64 },
    /// A consumer no longer reads this partition: nothing is kept for it.
    Unsubscribed { consumer: String },
}

impl fmt::Display for Proposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The key and the command name only: payloads never go to logs.
            Proposal::Command(e) => write!(f, "{} from {} ({})", e.command.name(), e.caller, e.key),
            Proposal::Housekeeping(Housekeeping::Delivered { ids }) => write!(f, "delivered {} messages", ids.len()),
            Proposal::Housekeeping(Housekeeping::Sealed { through_seq }) => write!(f, "sealed journal through {through_seq}"),
            Proposal::Housekeeping(Housekeeping::Published { through_seq }) => write!(f, "published events through {through_seq}"),
            Proposal::Housekeeping(Housekeeping::Consumed { consumer, through_seq }) => write!(f, "{consumer} holds events through {through_seq}"),
            Proposal::Housekeeping(Housekeeping::Unsubscribed { consumer }) => write!(f, "{consumer} no longer reads events"),
            Proposal::Module(_) => write!(f, "a module command"),
            Proposal::Feed { partition, after, events } => write!(f, "{} events of partition {partition} after {after}", events.len()),
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
    /// What a module made of a module command: its answer, or why it refused.
    /// `None` for everything a partition's group applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<Result<serde_json::Value, String>>,
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
            module: None,
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
