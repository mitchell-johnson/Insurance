//! Durable orchestration of the end-of-day run.
//!
//! The run is a Temporal workflow. Temporal keeps the run's history, so a
//! worker that dies is replaced by any other worker, which picks the run up
//! at the step it had reached. The ledger does not depend on that: every
//! step is idempotent on its own, so a step run twice, or by two workers at
//! once, has the effect of running it once.
//!
//! - [`eod`]: the order of the steps, independent of Temporal.
//! - [`steps`]: the steps done against the ledger nodes and the root store.
//! - [`store`]: where the signed Merkle root goes, and who signs it.
//! - [`temporal`]: the workflow, its activities and the worker.
//! - [`settings`]: the worker's configuration.

pub mod due;
pub mod eod;
pub mod settings;
pub mod steps;
pub mod store;
pub mod temporal;

pub use eod::{run_end_of_day, run_unit_orders, EndOfDayError, EndOfDayInput, EndOfDayReport, Steps};
pub use steps::{LedgerSteps, StepError};
