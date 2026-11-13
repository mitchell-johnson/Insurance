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
pub mod counterparty;
pub mod event;
pub mod ids;
pub mod interest;
pub mod invest;
pub mod machine;
pub mod payments;
pub mod policy;
pub mod premium;
pub mod recon;
pub mod reinsurance;

pub mod arrangement;
mod arrears;
mod billing;
mod cash;
mod claims;
mod earning;
mod group;
mod lifecycle;
mod nondisclosure;
mod postings;
mod reduce;
mod reopen;
mod transfer;
mod underwritten;
mod unwind;
pub mod voluntary;

pub use command::{Command, Envelope};
pub use event::{Event, EventRecord};
pub use group::{preview_membership, ChangePreview, MembershipPreview};
pub use ic_claims as claims_module;
pub use ids::*;
pub use lifecycle::quote;
pub use machine::{Applied, CoreState, Reject, APPLY_BUDGET};
pub use payments::PayableKind;
pub use policy::*;
