//! The module as a ledger node hosts it (`docs/modules.md`).
//!
//! The state starts empty: code lists, question sets, rule sets and authority
//! profiles arrive as activation commands through the log, each with its
//! approvals, so every member holds the same configuration at the same index.

use ic_raft::{hosted, ModuleSpec, ModuleState};

use crate::special::answer;
use crate::UnderwritingState;

/// The module's permanent name on the cluster.
pub const MODULE: &str = "underwriting";

fn make() -> Box<dyn ModuleState> {
    hosted(UnderwritingState::default(), answer)
}

/// The underwriting module, for a ledger node to host.
pub fn module_spec() -> Result<ModuleSpec, String> {
    ModuleSpec::new(MODULE, make)
}
