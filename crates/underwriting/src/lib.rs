//! Underwriting and new business.
//!
//! A proposal is opened against a bound quote, each life gives a signed
//! disclosure, rules written as code assess it, and what the rules cannot
//! settle goes to a person through a queue, under an authority profile that
//! is enforced. A decision carries loadings and exclusions per cover,
//! reserves reinsurance capacity, is offered back where the terms are not
//! standard, and ends as one command to the core.
//!
//! The module is a deterministic state machine ([`UnderwritingState`]): it
//! reads no clock and calls nothing. Drivers propose commands and act on the
//! effects each one returns. Clinical content never enters a command: it is
//! sealed in the clinical store ([`clinical`]) and the state holds only
//! references and hashes.

pub mod authority;
pub mod case;
pub mod codes;
pub mod command;
pub mod config;
pub mod evidence;
pub mod load;
pub mod model;
pub mod questions;
pub mod rules;
pub mod shipped;
pub mod signing;
