//! Underwriting loadings, read out for the people allowed to see them.
//!
//! A loading implies something about a person's health, so it is never put
//! on an event. Finance needs loadings to check the premium the book should
//! bill, and reads them here instead: one page of policies at a time, in
//! policy order, so a full pass can be resumed and never holds the whole
//! book in one answer. Who may ask is decided where the question enters.

use std::collections::BTreeMap;
use std::ops::Bound;

use chrono::NaiveDate;
use ic_money::DecStr;
use serde::{Deserialize, Serialize};

use crate::policy::{Policy, PolicyStatus};
use crate::PolicyId;

/// The most policies one page looks at.
pub const MAX_PAGE: u32 = 500;

/// One layer that carries a loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadingRow {
    pub policy: PolicyId,
    pub cover: String,
    pub layer: u32,
    pub start: NaiveDate,
    pub end: Option<NaiveDate>,
    /// Each loading as a fraction of the base premium. They add: the
    /// premium is the base times one plus their sum.
    pub loadings: Vec<DecStr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadingsPage {
    pub rows: Vec<LoadingRow>,
    /// Pass this as `after` for the next page. None when the book is done.
    pub next: Option<PolicyId>,
}

/// The loaded layers on up to `limit` policies after `after`, in policy
/// order. With `as_at`, only layers in force on that date; without it, every
/// loaded layer the policy has ever had, with its dates. A policy that has
/// moved to another partition is answered for there.
pub fn page(policies: &BTreeMap<PolicyId, Policy>, after: Option<&PolicyId>, limit: u32, as_at: Option<NaiveDate>) -> LoadingsPage {
    let limit = limit.clamp(1, MAX_PAGE) as usize;
    let from = after.map_or(Bound::Unbounded, Bound::Excluded);
    let mut walk = policies.range((from, Bound::Unbounded)).peekable();
    let mut rows = Vec::new();
    let mut last = None;
    for (id, policy) in walk.by_ref().take(limit) {
        last = Some(id.clone());
        if policy.status == PolicyStatus::Transferred {
            continue;
        }
        for (code, cover) in &policy.covers {
            let loaded = cover
                .layers
                .iter()
                .filter(|l| !l.loadings.is_empty() && as_at.is_none_or(|on| l.in_force_on(on)));
            rows.extend(loaded.map(|l| LoadingRow {
                policy: id.clone(),
                cover: code.clone(),
                layer: l.id,
                start: l.start,
                end: l.end,
                loadings: l.loadings.clone(),
            }));
        }
    }
    let next = last.filter(|_| walk.peek().is_some());
    LoadingsPage { rows, next }
}
