//! A business's peak season, as its schedule names it.
//!
//! An income cover with the Peak Season Upgrade pays more for a month of
//! disablement that falls in the business's busiest months
//! (`products/aia_living_business.star`, built-in `peak-season`). Which
//! months those are is not the product's to say: it is written on each
//! policy's schedule, so it is held on the policy, per cover, and may be
//! changed later. Each change runs from the business date it was made, and
//! a claim reads the months in force on its event date, so changing the
//! months never changes a claim already made.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::PolicyId;

/// Set a cover's peak season months.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeakSeasonSpec {
    pub policy: PolicyId,
    pub cover: String,
    /// The months (1 to 12). Empty: the schedule names no peak season from
    /// today.
    pub months: Vec<u32>,
}

/// The months in force from a date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeakSeason {
    pub from: NaiveDate,
    pub months: Vec<u32>,
}

/// The peak season months of a cover on a date.
pub fn months_on(policy: &Policy, cover: &str, on: NaiveDate) -> Vec<u32> {
    policy
        .peak_seasons
        .get(cover)
        .and_then(|history| history.iter().rev().find(|s| s.from <= on))
        .map(|s| s.months.clone())
        .unwrap_or_default()
}

pub(crate) fn set(policy: &mut Policy, ctx: &mut Ctx<'_>, spec: PeakSeasonSpec) -> Result<(), Reject> {
    let mut months = spec.months.clone();
    months.sort_unstable();
    months.dedup();
    // A season is some of the year: every month would be no season at all.
    if months.len() != spec.months.len() || months.iter().any(|m| !(1..=12).contains(m)) || months.len() == 12 {
        return Err(Reject::new(
            "invalid_peak_season",
            "a peak season is some of the months 1 to 12, each named once",
        ));
    }
    if matches!(policy.status, PolicyStatus::Transferred | PolicyStatus::Avoided) {
        return Err(Reject::new("not_in_force", "the policy is not here to set a peak season on"));
    }
    if !policy.covers.contains_key(&spec.cover) {
        return Err(Reject::new("unknown_cover", format!("the policy has no cover {}", spec.cover)));
    }
    if months_on(policy, &spec.cover, ctx.date) == months {
        return Ok(());
    }
    let history = policy.peak_seasons.entry(spec.cover).or_default();
    // A second change on the same day replaces the first.
    history.retain(|s| s.from != ctx.date);
    history.push(PeakSeason { from: ctx.date, months });
    Ok(())
}
