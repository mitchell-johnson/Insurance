//! Raft replication for one ledger partition.
//!
//! Each partition is a Raft group. The shipped deployment runs three voters
//! per group, one in each availability zone of Azure New Zealand North, so a
//! zone can be lost without losing a committed entry. Surviving the loss of
//! the region needs voters in a second and third region, which the software
//! supports and the infrastructure does not yet provision (docs/decisions.md).
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
//! - [`module`]: a replicated group for a module whose state spans partitions.
//!
//! The state machine is held in memory. It is snapshotted to disk as the log
//! grows, the log behind a snapshot is dropped, and a restarted member loads
//! its snapshot and replays the log after it. Journal entries, events and
//! cross-partition messages stay in the replicated state until the leader
//! has sealed, published or delivered them. Each member also keeps its
//! partition's read models, fed from the same events.

pub mod auth;
pub mod client;
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

pub use auth::ClusterAuth;
pub use client::{ClusterClient, EndOfDayView};
pub use machine::Machine;
pub use module::{hosted, hosted_reading, ModuleReply, ModuleSpec, ModuleState, ModuleStatus};
pub use node::{Node, NodeStatus, ProposeError};
pub use peers::Identity;
pub use query::{Answer, PolicyView, Query};
pub use store::LogStore;
pub use types::{NodeId, Proposal, Reply, TypeConfig};
