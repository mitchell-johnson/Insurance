//! Raft replication for one ledger partition.
//!
//! Each partition is a Raft group. Production runs five voters per group
//! across Azure New Zealand North, Australia East and Australia Southeast, so
//! a zone or a whole region can be lost without losing a committed entry.
//!
//! The pieces:
//! - [`types`]: what is replicated (a command envelope) and what comes back.
//! - [`store`]: the durable Raft log, vote and commit index for a node.
//! - [`machine`]: the core's deterministic state machine driven by the log.
//! - [`network`]: Raft RPCs between nodes, over HTTP or in process.
//! - [`node`]: one member of a group, with the client-facing propose path.
//! - [`server`]: a node process hosting a member of every partition's group.
//! - [`client`]: how callers find a partition's leader and retry safely.
//!
//! The state machine is held in memory and rebuilt from the durable log on
//! restart. The log is never compacted in this phase; snapshots are the next
//! step and are noted in `docs/resilience.md`.

pub mod client;
pub mod machine;
pub mod network;
pub mod node;
pub mod server;
pub mod store;
pub mod types;

pub use client::{ClusterClient, EndOfDayView};
pub use machine::Machine;
pub use node::{Node, NodeStatus, ProposeError};
pub use store::LogStore;
pub use types::{NodeId, Proposal, Reply, TypeConfig};
