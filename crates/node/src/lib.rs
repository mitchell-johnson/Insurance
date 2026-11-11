//! The ledger node as deployed.
//!
//! `ic-raft` hosts partitions and any module handed to it, and knows no
//! module by name. This crate is the one place that names them: it depends
//! on every module crate and lists their specs. A module joins the deployed
//! node by adding itself to [`modules`], and nowhere else.

use ic_raft::ModuleSpec;

/// Every module a ledger node hosts, each in its own replicated group.
/// Every node of a cluster runs the same binary, so they host the same list.
pub fn modules() -> Result<Vec<ModuleSpec>, String> {
    Ok(vec![
        ic_channels::query::module_spec()?,
        ic_group::module_spec()?,
        ic_underwriting::module_spec()?,
    ])
}
