//! Unit pricing.
//!
//! The core holds each policy's units and executes unit orders. This module
//! strikes each fund's price from the fund administrator's valuation, runs
//! the checks, records the maker and the checker, and activates the price
//! on every partition through the core's log. A price error is put right by
//! a corrected version of the price, which the core replays the affected
//! orders against; this module keeps the error's record, its materiality and
//! what the replay did. After each pricing date it sends the administrator
//! every policy's holding and the day's net flow.
//!
//! [`PricingState`] is the deterministic state machine, hosted in its own
//! replicated group as module `unit-pricing` ([`host::module_spec`]).

pub mod command;
pub mod extract;
pub mod fund;
pub mod host;
pub mod state;
pub mod strike;
pub mod valuation;

pub use command::{Command, Envelope, Event, Output};
pub use host::module_spec;
pub use state::PricingState;
