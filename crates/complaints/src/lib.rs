//! Complaints, disputes and conduct cases.
//!
//! A complaint is recorded wherever it is heard, by one command. It runs
//! under timers from the conduct regime and the external scheme, is
//! reviewed by someone who was not involved, and ends with a written
//! outcome, remediation through the ordinary commands of the modules that
//! own them, and a cause code that feeds the monthly root cause review.
//! A conduct case carries an affected population: a licence event opens
//! one for every customer of the advisers who can no longer advise.
//!
//! [`ComplaintsState`] is the deterministic state machine, hosted in its
//! own replicated group as module `complaints`.

pub mod command;
pub mod conduct;
pub mod model;
pub mod module;
pub mod report;
pub mod state;

pub use command::{Command, Envelope, Event, EventRecord, Timers};
pub use module::module_spec;
pub use state::{Applied, ComplaintsState};
