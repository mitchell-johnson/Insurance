//! Reinsurance: treaties as code, the per-life accumulation, cessions, the
//! capacity step, ceded premiums, recoveries, reinsurer statements and
//! settlement.
//!
//! The module is a deterministic state machine beside the core. It reads
//! every partition's event stream, keeps what the insurer holds on each life
//! across all of them, and works out what is ceded on each cover as a pure
//! function of that and the treaty version the cover was written under.
//! What it works out goes back to the core as instructions with stable IDs:
//! record this cession, post this entry. Money moves only in the core's
//! ledger.

pub mod account;
pub mod capacity;
pub mod cession;
pub mod claims;
pub mod delivery;
pub mod driver;
mod feed;
pub mod host;
mod ids;
pub mod loader;
pub mod machine;
pub mod math;
pub mod outbox;
pub mod premium;
pub mod service;
pub mod treaty;
pub mod views;

pub use ids::{ClaimId, LifeKey, ReinsurerId, RiskKey, TreatyId, TreatyRef};
pub use machine::{Applied, Command, Envelope, Instruction, InstructionBody, Notice, PostingKind, Reinsurance, Reject};
