//! Money in and money out.
//!
//! The core decides what is owed. This module moves it: it holds the direct
//! debit authorities and the accounts that may be paid, assembles payment
//! and collection runs, puts each run through its controls, hands it to a
//! bank channel, follows it to settlement or return, tells the core what the
//! bank did, and reconciles every bank account to its statement.
//!
//! The module is a deterministic state machine ([`PaymentsState`]) hosted in
//! its own replicated group. Everything that touches a bank or the core is a
//! driver that works from the outbox and the queue of submissions and
//! reports back with commands.

mod authority;
pub mod config;
mod console;
mod drill;
pub mod file;
pub mod forecast;
pub mod hold;
pub mod intake;
pub mod iso20022;
pub mod machine;
pub mod model;
pub mod notices;
mod payee;
pub mod query;
mod recon;
mod refund;
mod run;
pub mod sweep;

pub use config::Config;
pub use drill::Readiness;
pub use machine::{Command, Envelope, Event, Output, PaymentsState};
pub use model::{Actor, Reject, Role};
