//! The platform API (REST and JSON) for the insurance core.
//!
//! Writes are asynchronous by contract: they return a resource with a status
//! and outcomes arrive as events. Authorisation happens here and again in the
//! core, which accepts each command only from the module allowed to send it.
//!
//! This crate also hosts the insurance in a box: every partition in one
//! process on a local log. In production the same handlers sit in front of
//! the partitions' Raft groups.

pub mod auth;
pub mod engine;
pub mod routes;

pub use engine::{Engine, EngineConfig};
pub use routes::router;
