//! Changing how often, and on which day, premium falls due.
//!
//! The billing schedule is counted from the policy's inception at the
//! frequency chosen at issue. A change the owner asks for does not edit
//! anything already billed: it starts a new schedule from a billing period
//! not yet billed, and the periods before it stay as they were.
//!
//! - **Frequency.** The product says when a new frequency takes effect: from
//!   the next period not yet billed, or from the first period on or after
//!   the next policy anniversary. A product that says neither does not let
//!   the frequency change.
//! - **Billing day.** Where the product allows it, the day of the month
//!   moves from the next period not yet billed. The days between the end of
//!   what is billed and the first new due day are one short period, billed
//!   for its days at the new frequency's premium.
//!
//! A change still waiting for its first period is replaced by a later
//! request, and taken back by a request for the frequency the policy has. Schemes are billed on their own cycle and are refused.

use chrono::{Datelike, Months, NaiveDate};
use ic_product::schedule::{anniversary, period_start, policy_year};
use ic_product::{Frequency, FrequencyChange};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::PolicyId;

/// The owner's request to change the billing frequency, the billing day,
/// or both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeBilling {
    pub policy: PolicyId,
    /// The request's own reference. The same one again changes nothing.
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency: Option<Frequency>,
    /// The day of the month premium falls due, 1 to 28.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_day: Option<u32>,
}

/// One change of schedule, kept on the policy in the order they take effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingChange {
    pub reference: String,
    /// The business date the change was asked for.
    pub requested_on: NaiveDate,
    /// The first billing period (counted from inception) on the new schedule.
    pub from_period: u32,
    /// That period's first day.
    pub first_day: NaiveDate,
    /// The day the new schedule is counted from. Later than `first_day`
    /// where the billing day moved: the days between are one short period.
    pub anchor: NaiveDate,
    pub frequency: Frequency,
    /// The frequency this change took over from.
    pub replaced: Frequency,
}

impl BillingChange {
    fn short_period(&self) -> bool {
        self.anchor > self.first_day
    }
    fn start_of(&self, n: u32) -> Option<NaiveDate> {
        let k = n.checked_sub(self.from_period)?;
        match (self.short_period(), k) {
            (true, 0) => Some(self.first_day),
            (true, k) => period_start(self.anchor, self.frequency, k - 1),
            (false, k) => period_start(self.anchor, self.frequency, k),
        }
    }
}

fn months(frequency: Frequency) -> Option<u32> {
    match frequency {
        Frequency::Fortnightly => None,
        Frequency::Monthly => Some(1),
        Frequency::Quarterly => Some(3),
        Frequency::HalfYearly => Some(6),
        Frequency::Yearly => Some(12),
    }
}

impl Policy {
    /// The first day of billing period `n`, counted from inception through
    /// every change of schedule.
    pub fn period_start(&self, n: u32) -> Option<NaiveDate> {
        match self.billing_changes.iter().rev().find(|c| c.from_period <= n) {
            Some(change) => change.start_of(n),
            None => period_start(self.inception, self.first_frequency(), n),
        }
    }
    fn first_frequency(&self) -> Frequency {
        self.billing_changes.first().map_or(self.frequency, |c| c.replaced)
    }
    /// The frequency of the billing period that `day` falls in.
    pub fn frequency_on(&self, day: NaiveDate) -> Frequency {
        self.billing_changes
            .iter()
            .rev()
            .find(|c| c.first_day <= day)
            .map_or_else(|| self.first_frequency(), |c| c.frequency)
    }
    /// Where the period starting on `start` is the short one before a new
    /// billing day: its days, and the days of a whole period ending on the
    /// same day, which its premium is a share of.
    pub fn short_period(&self, start: NaiveDate) -> Option<(i64, i64)> {
        let change = self.billing_changes.iter().find(|c| c.first_day == start && c.short_period())?;
        let whole_from = change.anchor.checked_sub_months(Months::new(months(change.frequency)?))?;
        Some(((change.anchor - change.first_day).num_days(), (change.anchor - whole_from).num_days()))
    }
    /// Bring `frequency` up to the schedule of period `n`, which is about
    /// to be billed.
    pub(crate) fn enter_period(&mut self, n: u32) {
        if let Some(change) = self.billing_changes.iter().rev().find(|c| c.from_period <= n) {
            self.frequency = change.frequency;
        }
    }
}

fn named(reference: &str) -> bool {
    !reference.is_empty() && reference.len() <= 64 && reference.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The first day on or after `from` that is day `day` of its month.
fn next_on_day(from: NaiveDate, day: u32) -> Option<NaiveDate> {
    let this_month = from.with_day(day)?;
    if this_month >= from {
        Some(this_month)
    } else {
        this_month.checked_add_months(Months::new(1))
    }
}

pub(crate) fn change(catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: ChangeBilling) -> Result<(), Reject> {
    if !named(&spec.reference) {
        return Err(Reject::new("invalid_request", "a request has a plain reference"));
    }
    if policy.billing_changes.iter().any(|c| c.reference == spec.reference) {
        return Ok(());
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force has its billing changed"));
    }
    if policy.is_scheme() {
        return Err(Reject::new("scheme_policy", "a scheme is billed on its own cycle"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "the policy's product version is not active"))?;
    let (frequency_rule, day_allowed) = product.billing_changes();
    // A change still waiting for its first period gives way to this one.
    let waiting = policy.billing_changes.iter().position(|c| c.from_period >= policy.periods_billed);
    let mut changes = policy.billing_changes.clone();
    if let Some(at) = waiting {
        changes.truncate(at);
    }
    let current = changes.last().map_or(policy.first_frequency(), |c| c.frequency);
    let frequency = spec.frequency.unwrap_or(current);
    let new_frequency = frequency != current;
    if !new_frequency && spec.billing_day.is_none() {
        // Asking for what the policy has now takes back a change that was
        // still waiting.
        if waiting.is_none() {
            return Err(Reject::new(
                "nothing_to_change",
                "the request changes neither the frequency nor the billing day",
            ));
        }
        policy.billing_changes = changes;
        let next = policy
            .period_start(policy.periods_billed)
            .ok_or_else(|| Reject::new("date", "billing period out of range"))?;
        ctx.emit(Event::BillingChanged {
            policy: policy.id.clone(),
            reference: spec.reference,
            frequency,
            first_day: next,
            first_full_period: next,
        });
        return Ok(());
    }
    if !product.allows_frequency(frequency) {
        return Err(Reject::new("frequency_not_offered", "the product does not offer that frequency"));
    }
    if new_frequency && frequency_rule.is_none() {
        return Err(Reject::new("change_not_offered", "the product does not let the billing frequency change"));
    }
    if let Some(day) = spec.billing_day {
        if !day_allowed {
            return Err(Reject::new("change_not_offered", "the product does not let the billing day change"));
        }
        if !(1..=28).contains(&day) || months(frequency).is_none() {
            return Err(Reject::new(
                "invalid_billing_day",
                "a billing day is the 1st to the 28th of the month, on a frequency counted in months",
            ));
        }
    }
    let date = || Reject::new("date", "billing period out of range");
    let was = Policy {
        billing_changes: changes.clone(),
        ..policy.clone()
    };
    // The first period on the new schedule: the next one not yet billed, or
    // the first on or after the next anniversary.
    let mut from_period = policy.periods_billed;
    if new_frequency && frequency_rule == Some(FrequencyChange::Anniversary) {
        let next_anniversary = anniversary(policy.inception, policy_year(policy.inception, ctx.date) + 1).ok_or_else(date)?;
        // At most a year of the shortest periods, with room to spare.
        let reached = (from_period..from_period.saturating_add(64)).find(|n| was.period_start(*n).is_some_and(|start| start >= next_anniversary));
        from_period = reached.ok_or_else(date)?;
    }
    let first_day = was.period_start(from_period).ok_or_else(date)?;
    let anchor = match spec.billing_day {
        Some(day) => next_on_day(first_day, day).ok_or_else(date)?,
        None => first_day,
    };
    changes.push(BillingChange {
        reference: spec.reference.clone(),
        requested_on: ctx.date,
        from_period,
        first_day,
        anchor,
        frequency,
        replaced: current,
    });
    policy.billing_changes = changes;
    ctx.emit(Event::BillingChanged {
        policy: policy.id.clone(),
        reference: spec.reference,
        frequency,
        first_day,
        first_full_period: anchor,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// Purpose: a short period before a new billing day is measured against a whole period
    /// of the new frequency, so each calendar frequency gives its length in months (and a
    /// fortnightly schedule, which is counted in days, gives none); and the new billing day
    /// is the next date on that day of the month, later this month where it has not passed
    /// and next month where it has.
    /// Why it matters: the short period's premium is a share of a whole period's. A quarter
    /// measured as a month would charge three times too much for the days before the new
    /// billing day, and a billing day put a month late would leave a month unbilled.
    #[test]
    fn a_new_billing_day_and_the_period_it_is_measured_against() {
        let lengths: Vec<Option<u32>> = [
            Frequency::Fortnightly,
            Frequency::Monthly,
            Frequency::Quarterly,
            Frequency::HalfYearly,
            Frequency::Yearly,
        ]
        .into_iter()
        .map(months)
        .collect();
        assert_eq!(lengths, vec![None, Some(1), Some(3), Some(6), Some(12)]);
        assert_eq!(next_on_day(d(2027, 11, 10), 20), Some(d(2027, 11, 20)), "later this month");
        assert_eq!(next_on_day(d(2027, 11, 10), 10), Some(d(2027, 11, 10)), "today is on the day");
        assert_eq!(next_on_day(d(2027, 11, 10), 5), Some(d(2027, 12, 5)), "passed, so next month");
    }
}
