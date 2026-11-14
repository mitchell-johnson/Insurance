//! Suspending a policy at the owner's request.
//!
//! Where the product gives the right (`cover_suspension`), the owner may
//! put the whole policy to sleep for a dated period: no premium is billed
//! for those days and nothing that happens in them is covered. Cover comes
//! back on the agreed day, or earlier if the owner asks, with no new
//! evidence of health. The product caps the months of suspension over the
//! policy's life and says how long it must have been in force first.
//!
//! The premium side is a hardship override at a nil share under the same
//! name; this module adds the product's limits and the days without cover,
//! which a claim's cover check reads.

use chrono::{Months, NaiveDate};
use ic_ledger::Ledger;
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::PolicyId;

/// The owner's request to suspend the policy from `from` up to `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspendCover {
    pub policy: PolicyId,
    /// The request's own reference. The same one again changes nothing.
    pub reference: String,
    /// First day without cover or premium.
    pub from: NaiveDate,
    /// First day back on cover.
    pub to: NaiveDate,
}

/// One suspension, kept on the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverSuspension {
    pub from: NaiveDate,
    /// First day back on cover. Brought forward when cover is resumed early.
    pub to: NaiveDate,
    /// The end first agreed, where cover was resumed early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_to: Option<NaiveDate>,
}

impl CoverSuspension {
    fn days(&self) -> i64 {
        (self.to - self.from).num_days().max(0)
    }
}

impl Policy {
    /// Whether the owner had the policy suspended on `on`.
    pub fn suspended_on(&self, on: NaiveDate) -> bool {
        self.suspensions.values().any(|s| s.from <= on && on < s.to)
    }
}

fn named(reference: &str) -> bool {
    !reference.is_empty() && reference.len() <= 64 && reference.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The most days of suspension a policy may have in all, counted from the
/// first day of this one: the product's months as the calendar has them.
pub(crate) fn allowed_days(from: NaiveDate, max_months: u32) -> i64 {
    from.checked_add_months(Months::new(max_months)).map_or(0, |end| (end - from).num_days())
}

pub(crate) fn suspend(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: SuspendCover) -> Result<(), Reject> {
    if !named(&spec.reference) {
        return Err(Reject::new("invalid_request", "a request has a plain reference"));
    }
    if let Some(done) = policy.suspensions.get(&spec.reference) {
        return if (done.from, done.planned_to.unwrap_or(done.to)) == (spec.from, spec.to) {
            Ok(())
        } else {
            Err(Reject::new(
                "duplicate_request",
                format!("reference {} suspended the policy for other days", spec.reference),
            ))
        };
    }
    if policy.hardship.as_ref().is_some_and(|o| o.id == spec.reference) {
        return Err(Reject::new("duplicate_request", "a hardship override already has that name"));
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force can be suspended"));
    }
    let option = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "the policy's product version is not active"))?
        .cover_suspension
        .ok_or_else(|| Reject::new("no_suspension_option", "the product does not let the owner suspend the policy"))?;
    // Arrears are not parked by a suspension: they are paid first.
    if policy.policyholder_outstanding_minor() > 0 {
        return Err(Reject::new("premium_owing", "premium is owing; it is paid before the policy is suspended"));
    }
    let earliest = policy.inception.checked_add_months(Months::new(option.min_months_in_force));
    if earliest.is_none_or(|earliest| spec.from < earliest) {
        return Err(Reject::new(
            "suspended_too_early",
            format!("the policy is in force {} months before it can be suspended", option.min_months_in_force),
        ));
    }
    let used: i64 = policy.suspensions.values().map(CoverSuspension::days).sum();
    let asked = (spec.to - spec.from).num_days();
    if used + asked > allowed_days(spec.from, option.max_months) {
        return Err(Reject::new(
            "suspension_too_long",
            format!(
                "the policy may be suspended {} months in all; {used} days are used and {asked} more are asked for",
                option.max_months
            ),
        ));
    }
    // No premium for the days: the override checks the dates, that nothing
    // else is running, and hands back what was billed ahead.
    crate::hardship::grant(ledger, catalog, policy, ctx, spec.reference.clone(), spec.from, spec.to, 0).map_err(|r| match r.code.as_str() {
        "invalid_hardship" => Reject::new("invalid_request", "a suspension starts today or later and ends after it starts"),
        _ => r,
    })?;
    policy.suspensions.insert(
        spec.reference.clone(),
        CoverSuspension {
            from: spec.from,
            to: spec.to,
            planned_to: None,
        },
    );
    ctx.emit(Event::CoverSuspended {
        policy: policy.id.clone(),
        reference: spec.reference,
        from: spec.from,
        to: spec.to,
    });
    Ok(())
}

/// Bring cover back today, ahead of the day agreed. Premium is billed
/// again from today.
pub(crate) fn resume(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, reference: &str) -> Result<(), Reject> {
    let Some(s) = policy.suspensions.get(reference).cloned() else {
        return Err(Reject::new("unknown_suspension", "the policy has no suspension of that name"));
    };
    if s.to <= ctx.date {
        // Already over, by its own date or an earlier resumption.
        return Ok(());
    }
    crate::hardship::end(ledger, catalog, policy, ctx, reference)?;
    let on = ctx.date.max(s.from);
    policy.suspensions.insert(
        reference.to_string(),
        CoverSuspension {
            from: s.from,
            to: on,
            planned_to: Some(s.planned_to.unwrap_or(s.to)),
        },
    );
    ctx.emit(Event::CoverResumed {
        policy: policy.id.clone(),
        reference: reference.to_string(),
        on,
        planned_to: s.to,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// Purpose: the days a policy may be suspended in all are the product's months counted
    /// on the calendar from the first day asked for, and a suspension resumed early counts
    /// only the days it ran.
    /// Why it matters: the limit is a promise in the wording. Counted in fixed 30-day months
    /// it would refuse the twelfth month of a suspension the owner is entitled to; counting
    /// the days first planned rather than the days used would take months of the allowance
    /// from an owner who came back early.
    #[test]
    fn the_allowance_is_calendar_months_and_only_days_used_count() {
        assert_eq!(allowed_days(d(2027, 1, 1), 12), 365);
        assert_eq!(allowed_days(d(2028, 1, 1), 12), 366);
        assert_eq!(allowed_days(d(2027, 1, 31), 1), 28);
        assert_eq!(allowed_days(NaiveDate::MAX, 12), 0);
        let resumed = CoverSuspension {
            from: d(2027, 3, 1),
            to: d(2027, 4, 1),
            planned_to: Some(d(2027, 9, 1)),
        };
        assert_eq!(resumed.days(), 31);
        let never_ran = CoverSuspension {
            from: d(2027, 3, 1),
            to: d(2027, 3, 1),
            planned_to: Some(d(2027, 9, 1)),
        };
        assert_eq!(never_ran.days(), 0);
        assert!(named("sus-1") && !named("sus 1") && !named(""));
    }
}
