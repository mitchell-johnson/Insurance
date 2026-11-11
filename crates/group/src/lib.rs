//! Group scheme administration.
//!
//! A scheme is a policy whose policyholder is the employer. Membership is
//! data that arrives by file, API or portal. This module validates it, works
//! out what changed, shows the employer the effect, and then sends the core
//! typed commands. The core holds members as life records and bills from the
//! membership in force.

pub mod benefit;
pub mod command;
pub mod csv;
pub mod diff;
pub mod event;
pub mod host;
pub mod mapping;
pub mod model;
pub mod record;
pub mod state;
pub mod terms;
pub mod work;

pub use host::{answer, react, Question};
pub use state::{GroupState, Outcome};
