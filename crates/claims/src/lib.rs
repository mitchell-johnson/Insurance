//! Claims operations.
//!
//! The claims module decides whether an event is covered and manages the
//! relationship with the claimant. It never touches money itself: every
//! reserve, admission and payment is an [`Instruction`] to the core, which
//! posts it to the ledger. The module is a deterministic state machine. It
//! lives on the policy's partition and is replicated, snapshotted and hashed
//! with it, so a decision and its postings commit or fail together.
//!
//! Three rules shape everything here:
//!
//! - **Health information stays inside.** The cause of a claim, reason codes
//!   and evidence references are held in [`ClaimsState`] and never appear in
//!   an [`Instruction`] or a notice. What leaves is that a claim exists, its
//!   type, amounts and status.
//! - **Authority is checked where the decision is made.** Every decision is
//!   checked against the actor's authority profile, records the profile and
//!   rule versions it was made under, and takes a second signature where the
//!   rules require one.
//! - **The right thing is easy and the wrong thing is hard.** Reversing an
//!   admission is heavier than declining a new claim, and an overpayment that
//!   was the insurer's own error is held, not pursued, unless the conduct
//!   officer and legal both sign.

pub mod command;
pub mod config;
pub mod model;
pub mod port;
pub mod report;
pub mod state;
pub mod testing;

mod book;
mod control;
mod decide;
mod intake;
mod overpay;

pub use command::{Actor, Command, Request};
pub use config::{AuthorityBook, Line, Profile, Queue, Role, Rules, Staff};
pub use model::*;
pub use port::{Core, CoverFacts, Instruction, Reject};
pub use state::{ClaimsState, Moved};

/// Staff are known by an opaque ID from the workforce directory.
pub type StaffId = String;
/// A claim's ID, assigned by the channel that took the notification.
pub type ClaimId = String;
