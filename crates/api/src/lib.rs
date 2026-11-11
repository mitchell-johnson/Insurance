//! The platform API (REST and JSON) for the insurance core.
//!
//! Writes are asynchronous by contract: they return a resource with a status
//! and outcomes arrive as events. Authorisation happens here and again in the
//! core, which accepts each command only from the module allowed to send it.
//!
//! The same handlers serve two arrangements, chosen by [`settings`]: the
//! insurance in a box, where every partition is in this process on a local
//! log, and production, where this process is a stateless gateway in front
//! of the partitions' Raft groups on the ledger nodes. See [`backend`].

pub mod admin;
pub mod auth;
pub mod backend;
pub mod engine;
pub mod routes;
pub mod settings;

pub use backend::{Backend, Gateway};
pub use engine::{Engine, EngineConfig};
pub use routes::router;
