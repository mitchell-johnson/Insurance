//! A hardship override: a premium holiday or a reduced premium for a dated
//! period.
//!
//! A hardship team decides it; the core applies it. For the days the
//! override covers, the owner is billed a share of the premium (none, for a
//! holiday) and the insurer earns only that share. Cover carries on. The
//! override can be ended early: the full premium is billed again from that
//! day, and nothing is charged for the days already behind it.

use chrono::NaiveDate;
use ic_ledger::Ledger;
use serde::{Deserialize, Serialize};

use crate::billing::{self, Bill};
use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus, ReceivableKind};

/// The longest an override may run.
pub const MAX_DAYS: i64 = 366 * 2;
const WHOLE: i128 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardshipOverride {
    pub id: String,
    /// First day at the reduced premium.
    pub from: NaiveDate,
    /// First day back at the full premium. Brought forward when the
    /// override is ended early.
    pub to: NaiveDate,
    /// The share of the premium still billed, in basis points. Zero is a
    /// premium holiday.
    pub share_bps: u32,
    /// The end first agreed, where it was ended early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_to: Option<NaiveDate>,
}

fn bad(code: &str, message: &str) -> Reject {
    Reject::new(code, message)
}

/// Days of `period` the override covers.
fn overlap(o: &HardshipOverride, period: (NaiveDate, NaiveDate)) -> i64 {
    (o.to.min(period.1) - o.from.max(period.0)).num_days().max(0)
}

/// A period's bill as the override leaves it: the days it covers at its
/// share, the rest in full.
pub(crate) fn reduced(policy: &Policy, bill: Bill, period: (NaiveDate, NaiveDate)) -> Bill {
    let Some(o) = &policy.hardship else { return bill };
    let covered = overlap(o, period) as i128;
    if covered == 0 {
        return bill;
    }
    let days = (period.1 - period.0).num_days().max(1) as i128;
    bill.scaled((days - covered) * WHOLE + covered * o.share_bps as i128, days * WHOLE)
}

/// The period already billed that today falls in, if billing has got there.
fn current_period(policy: &Policy, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let n = policy.periods_billed.checked_sub(1)?;
    let start = ic_product::schedule::period_start(policy.inception, policy.frequency, n)?;
    let end = ic_product::schedule::period_start(policy.inception, policy.frequency, n + 1)?;
    (start <= today && today < end).then_some((start, end))
}

/// Adjust the period already billed for a change to the override's days:
/// `relief` days at the override's share that were billed in full (a
/// credit), or that were billed at the share and are now back in full (a
/// charge).
#[allow(clippy::too_many_arguments)]
fn adjust(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    period: (NaiveDate, NaiveDate),
    days: (NaiveDate, NaiveDate),
    share_bps: u32,
    credit: bool,
) -> Result<i64, Reject> {
    let covered = (days.1 - days.0).num_days().max(0) as i128;
    if covered == 0 {
        return Ok(0);
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "the policy's product version is not active on this partition"))?
        .clone();
    let premium = crate::premium::retail_period_premium(catalog, &product, policy, period.0, &mut ctx.work)?;
    let length = (period.1 - period.0).num_days().max(1) as i128;
    let sign: i128 = if credit { -1 } else { 1 };
    let delta = Bill::from_modal(policy, &premium).scaled(sign * covered * (WHOLE - share_bps as i128), length * WHOLE);
    billing::raise_receivable(ledger, policy, ctx, &delta, ReceivableKind::Adjustment, days, ctx.date, "premium_adjusted")?;
    crate::cash::apply_suspense(ledger, policy, ctx)?;
    Ok(delta.total())
}

pub(crate) fn grant(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    id: String,
    from: NaiveDate,
    to: NaiveDate,
    share_bps: u32,
) -> Result<(), Reject> {
    let plain = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain {
        return Err(bad("invalid_hardship", "an override is named by a plain identifier"));
    }
    match &policy.hardship {
        Some(o) if o.id == id && (o.from, o.planned_to.unwrap_or(o.to), o.share_bps) == (from, to, share_bps) => return Ok(()),
        Some(o) if o.id == id => return Err(bad("duplicate_hardship", "that override was granted on other terms")),
        Some(o) if o.to > ctx.date => return Err(bad("hardship_exists", "the policy already has an override that has not ended")),
        _ => {}
    }
    if policy.status != PolicyStatus::InForce {
        return Err(bad("not_in_force", "the policy is not in force"));
    }
    if policy.is_scheme() {
        return Err(bad("not_retail", "a scheme's premium is not reduced this way"));
    }
    if from < ctx.date || to <= from || (to - from).num_days() > MAX_DAYS || share_bps as i128 >= WHOLE {
        return Err(bad(
            "invalid_hardship",
            "an override starts today or later, ends after it starts within two years, and bills less than the whole premium",
        ));
    }
    // Worked on a copy, so a refusal part way leaves the policy as it was.
    let mut draft = policy.clone();
    draft.hardship = Some(HardshipOverride {
        id: id.clone(),
        from,
        to,
        share_bps,
        planned_to: None,
    });
    let mut adjusted_minor = 0;
    if let Some(period) = current_period(&draft, ctx.date) {
        let days = (from.max(period.0), to.min(period.1));
        adjusted_minor = adjust(ledger, catalog, &mut draft, ctx, period, days, share_bps, true)?;
    }
    *policy = draft;
    ctx.emit(Event::HardshipGranted {
        policy: policy.id.clone(),
        hardship: id,
        from,
        to,
        share_bps,
        adjusted_minor,
    });
    Ok(())
}

/// End the override today. The full premium is billed from today; the days
/// behind stay as they were billed.
pub(crate) fn end(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, id: &str) -> Result<(), Reject> {
    let Some(o) = policy.hardship.clone().filter(|o| o.id == id) else {
        return Err(bad("unknown_hardship", "the policy has no override of that name"));
    };
    if o.to <= ctx.date {
        // Already over, by its own date or an earlier ending.
        return Ok(());
    }
    let mut draft = policy.clone();
    // Ended before it began: it never applies.
    let to = ctx.date.max(o.from);
    draft.hardship = Some(HardshipOverride {
        to,
        planned_to: Some(o.planned_to.unwrap_or(o.to)),
        ..o.clone()
    });
    let mut adjusted_minor = 0;
    if let Some(period) = current_period(&draft, ctx.date) {
        let days = (to.max(period.0), o.to.min(period.1));
        adjusted_minor = adjust(ledger, catalog, &mut draft, ctx, period, days, o.share_bps, false)?;
    }
    *policy = draft;
    ctx.emit(Event::HardshipEnded {
        policy: policy.id.clone(),
        hardship: o.id,
        on: to,
        planned_to: o.to,
        adjusted_minor,
    });
    Ok(())
}
