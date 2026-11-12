//! The payments module's drivers.
//!
//! The module (`ic-payments`) decides; these act. A [`pump::Pump`] takes the
//! submissions the module has queued to a bank through a [`bank::Bank`]
//! channel, brings the bank's answers back as commands, works the module's
//! outbox against the core, and runs the evacuation drill. Everything here
//! can be retried: the module's idempotency keys, the bank's file IDs and
//! the core's command keys make a repeat harmless.

pub mod bank;
#[cfg(feature = "testing")]
pub mod fake;
pub mod iso20022;
pub mod ports;
pub mod pump;
pub mod settings;

use ic_raft::{hosted_reading, ModuleSpec, ModuleState};

/// The module's permanent name in a cluster.
pub const MODULE: &str = "payments";

fn make() -> Box<dyn ModuleState> {
    hosted_reading(ic_payments::PaymentsState::default(), ic_payments::query::answer, ic_payments::query::react)
}

/// The payments module, for a ledger node to host. It reads the core's
/// events: a payer's death stops collection without waiting for a driver.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events())
}
