//! Raft replication for one ledger partition.
//!
//! Each partition is a Raft group. The shipped deployment runs five voters
//! per group: two in Australia East (Sydney), two in Australia Southeast
//! (Melbourne) and a witness in a third region, which votes and keeps the
//! log but holds no state ([`witness`]). Either main region can be lost with
//! a majority left, and the groups' leadership is shared between the two
//! main regions so both take writes ([`failback`]). See docs/decisions.md,
//! "Where the ledger's voters run".
//!
//! The pieces:
//! - [`types`]: what is replicated (a command envelope) and what comes back.
//! - [`store`]: the durable Raft log, vote and commit index for a node.
//! - [`machine`]: the core's deterministic state machine driven by the log.
//! - [`network`]: Raft RPCs between nodes, over HTTP or in process.
//! - [`node`]: one member of a group, with the client-facing propose path.
//! - [`peers`]: each node's durable identity, so a node that lost its disk
//!   cannot return under its old number.
//! - [`query`]: the questions a partition answers from its state and read models.
//! - [`server`]: a node process hosting a member of every partition's group.
//! - [`auth`]: the cluster token every caller of a node must present.
//! - [`client`]: how callers find a partition's leader and retry safely.
//! - [`failback`]: sharing leadership between regions and handing it back home.
//! - [`witness`]: the tiebreaker that votes and keeps the log, and no state.
//! - [`module`]: a replicated group for a module whose state spans partitions.
//!
//! The state machine is held in memory. It is snapshotted to disk as the log
//! grows, the log behind a snapshot is dropped, and a restarted member loads
//! its snapshot and replays the log after it. Journal entries, events and
//! cross-partition messages stay in the replicated state until the leader
//! has sealed, published or delivered them. Each member also keeps its
//! partition's read models, fed from the same events.

pub mod access;
pub mod auth;
pub mod client;
pub mod failback;
mod group;
pub mod machine;
pub mod module;
pub mod network;
pub mod node;
pub mod peers;
pub mod query;
pub mod server;
pub mod store;
pub mod types;
pub mod witness;

pub use auth::ClusterAuth;
pub use client::{ClusterClient, EndOfDayView};
pub use machine::Machine;
pub use module::{hosted, hosted_reading, CoreOutcome, CorePosting, ModuleReply, ModuleSpec, ModuleState, ModuleStatus, Outbox, Route};
pub use node::{Node, NodeStatus, ProposeError};
pub use peers::Identity;
pub use query::{Answer, PolicyView, Query};
pub use store::LogStore;
pub use types::{NodeId, Proposal, Reply, TypeConfig};
