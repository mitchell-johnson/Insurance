//! The insurance core: one deterministic state machine per partition.
//!
//! The central object is a policy. Every premium, receipt, earning, lapse and
//! transfer is a balanced journal entry on the ledger; nothing about a
//! policy's money exists outside it. State changes only by applying a logged
//! [`Command`], every change produces [`Event`]s, and the machine never reads
//! a clock, randomness or anything else outside the log.

pub mod accounts;
pub mod bitemporal;
pub mod catalog;
pub mod command;
pub mod event;
pub mod ids;
pub mod machine;
pub mod policy;
pub mod premium;
pub mod recon;

mod arrears;
mod billing;
mod cash;
mod earning;
mod group;
mod lifecycle;
mod postings;
mod transfer;

pub use command::{Command, Envelope};
pub use event::{Event, EventRecord};
pub use ids::*;
pub use machine::{Applied, CoreState, Reject, APPLY_BUDGET};
pub use policy::*;
