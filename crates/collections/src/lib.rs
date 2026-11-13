//! Collections: what happens between a premium falling due unpaid and the
//! policy lapsing.
//!
//! The core derives standing from the age of the oldest unpaid premium and
//! lapses a policy on the product's lapse day; nobody here sets either.
//! This module decides who is told what and when, and when a person steps
//! in:
//!
//! - **The dunning sequence** is configuration per product: a step on named
//!   days after the due date, each step an event that produces a notice or a
//!   task. The suspension and final notices are mandatory: a sequence that
//!   leaves either out, or puts the final notice closer to lapse than the
//!   minimum, is refused.
//! - **The collections queue** is the day's work, ordered by days to lapse
//!   and then by annual premium. Officers see ordinary and financial
//!   details only, and the answer says what it withheld.
//! - **Payment arrangements** are agreed here and kept by the core, which
//!   holds standing while one is kept and resumes the lapse clock from where
//!   it stood if it is broken.
//! - **Hardship** is an application, a decision by the hardship team by
//!   someone other than the person who applied, and a dated relief.
//!
//! [`CollectionsState`] is the deterministic state machine, hosted in its
//! own replicated group on the ledger nodes as module `collections`. It reads
//! the core's events and posts arrangements to the core through its outbox.

pub mod cases;
pub mod config;
pub mod dunning;
pub mod machine;
pub mod model;
pub mod query;

pub use config::{Channel, EarlyCall, Sequence, Step, StepKind};
pub use machine::{Applied, CollectionsState, Command, CoreFact, Envelope, Event, EventRecord, Reject};
pub use model::{Reason, Relief, Role};
pub use query::{module_spec, MODULE};
