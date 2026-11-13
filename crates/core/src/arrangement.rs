//! An arrangement to clear overdue premium by instalments.
//!
//! Collections agrees it with the owner; the core only keeps the promise
//! and watches it. While every instalment is met by its date, and premium
//! billed since is paid within the product's grace period, the policy's
//! standing is held where it stood and the policy does not lapse. When the
//! overdue premium is cleared the arrangement is complete. When an
//! instalment is missed it is broken, and the lapse clock carries on from
//! where it stood on the day the arrangement was made: the days it ran are
//! not counted against the owner.

use chrono::NaiveDate;
use ic_product::ProductDefinition;
use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};

/// The most instalments one arrangement may have.
pub const MAX_INSTALMENTS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instalment {
    /// Paid by the end of this day.
    pub due: NaiveDate,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrangementState {
    Active,
    Completed { on: NaiveDate },
    Broken { on: NaiveDate },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arrangement {
    pub id: String,
    pub made_on: NaiveDate,
    /// What was overdue when it was made: the sum of the instalments.
    pub arrears_minor: i64,
    pub instalments: Vec<Instalment>,
    pub state: ArrangementState,
}

/// Days the lapse clock stood still under a broken arrangement, for premium
/// that fell due before it was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrearsPause {
    pub due_before: NaiveDate,
    pub days: i64,
}

fn bad(code: &str, message: &str) -> Reject {
    Reject::new(code, message)
}

/// What the policyholder owes on premium that fell due before a date.
fn overdue_minor(policy: &Policy, before: NaiveDate) -> i64 {
    policy
        .receivables
        .iter()
        .filter(|r| r.payer.is_none() && r.due < before)
        .map(|r| r.outstanding_minor.max(0))
        .sum()
}

pub(crate) fn make(policy: &mut Policy, ctx: &mut Ctx<'_>, id: String, instalments: Vec<Instalment>) -> Result<(), Reject> {
    let plain = !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain {
        return Err(bad("invalid_arrangement", "an arrangement is named by a plain identifier"));
    }
    match &policy.arrangement {
        Some(a) if a.id == id && a.instalments == instalments => return Ok(()),
        Some(a) if a.id == id => return Err(bad("duplicate_arrangement", "that arrangement was made with other instalments")),
        Some(a) if a.state == ArrangementState::Active => return Err(bad("arrangement_exists", "the policy already has an arrangement being kept")),
        _ => {}
    }
    if policy.status != PolicyStatus::InForce {
        return Err(bad("not_in_force", "the policy is not in force"));
    }
    let arrears_minor = overdue_minor(policy, ctx.date);
    if arrears_minor == 0 {
        return Err(bad("not_in_arrears", "the policy has no overdue premium to arrange"));
    }
    let ordered = instalments.windows(2).all(|w| w[0].due < w[1].due);
    let sound =
        !instalments.is_empty() && instalments.len() <= MAX_INSTALMENTS && ordered && instalments.iter().all(|i| i.amount_minor > 0 && i.due >= ctx.date);
    if !sound {
        return Err(bad(
            "invalid_arrangement",
            "instalments are for amounts above zero, on dates from today in order, and at most 60",
        ));
    }
    if instalments.iter().map(|i| i.amount_minor as i128).sum::<i128>() != arrears_minor as i128 {
        return Err(bad("invalid_arrangement", "the instalments add up to the premium overdue today"));
    }
    ctx.emit(Event::ArrangementMade {
        policy: policy.id.clone(),
        arrangement: id.clone(),
        arrears_minor,
        instalments: instalments.clone(),
    });
    policy.arrangement = Some(Arrangement {
        id,
        made_on: ctx.date,
        arrears_minor,
        instalments,
        state: ArrangementState::Active,
    });
    Ok(())
}

/// Collections ends an arrangement the owner has said they cannot keep. It
/// is a break, on the day it is ended.
pub(crate) fn end(policy: &mut Policy, ctx: &mut Ctx<'_>, id: &str) -> Result<(), Reject> {
    match &policy.arrangement {
        Some(a) if a.id == id && a.state == ArrangementState::Active => {
            break_it(policy, ctx);
            Ok(())
        }
        Some(a) if a.id == id => Ok(()),
        _ => Err(bad("unknown_arrangement", "the policy has no arrangement of that name")),
    }
}

fn break_it(policy: &mut Policy, ctx: &mut Ctx<'_>) {
    let Some(a) = policy.arrangement.as_mut() else { return };
    a.state = ArrangementState::Broken { on: ctx.date };
    let days = (ctx.date - a.made_on).num_days();
    // An earlier pause for the same old premium is kept and added to.
    let earlier = policy.arrears_pause.map(|p| p.days).unwrap_or(0);
    policy.arrears_pause = Some(ArrearsPause {
        due_before: a.made_on,
        days: earlier + days,
    });
    ctx.emit(Event::ArrangementBroken {
        policy: policy.id.clone(),
        arrangement: a.id.clone(),
        outstanding_minor: overdue_minor(policy, policy.arrangement.as_ref().map(|a| a.made_on).unwrap_or(ctx.date)),
    });
}

/// Look at the arrangement as at today and say whether it is still being
/// kept. Called before standing is derived, after a payment and in the
/// end-of-day arrears run.
pub(crate) fn watch(product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>) -> bool {
    let Some(a) = policy.arrangement.as_ref().filter(|a| a.state == ArrangementState::Active) else {
        return false;
    };
    let old = overdue_minor(policy, a.made_on);
    if old == 0 {
        let id = a.id.clone();
        if let Some(a) = policy.arrangement.as_mut() {
            a.state = ArrangementState::Completed { on: ctx.date };
        }
        policy.arrears_pause = None;
        ctx.emit(Event::ArrangementCompleted {
            policy: policy.id.clone(),
            arrangement: id,
        });
        return false;
    }
    // Instalments whose day has closed must have been paid, and premium
    // billed since the arrangement was made must not be past its grace.
    let fallen_due: i64 = a.instalments.iter().filter(|i| i.due < ctx.date).map(|i| i.amount_minor).sum();
    let grace = product.arrears().map(|r| r.grace_days as i64).unwrap_or(0);
    let behind_on_new: i64 = policy
        .receivables
        .iter()
        .filter(|r| r.payer.is_none() && r.due >= a.made_on && (ctx.date - r.due).num_days() > grace)
        .map(|r| r.outstanding_minor.max(0))
        .sum();
    // Money is set against the oldest premium first, so an owner who pays
    // an instalment and the month's premium together is ahead on the old
    // and behind on the new by the same amount. Only the net counts.
    let allowed = a.arrears_minor - fallen_due;
    if old > allowed || behind_on_new > allowed - old {
        break_it(policy, ctx);
        return false;
    }
    true
}

/// The date the lapse clock reads for this policy today: today, less the
/// days it stood still under a broken arrangement, for as long as premium
/// from before that arrangement is still unpaid.
pub(crate) fn clock(policy: &Policy, on: NaiveDate) -> NaiveDate {
    match (policy.arrears_pause, policy.oldest_unpaid_due()) {
        (Some(pause), Some(due)) if due < pause.due_before => on - chrono::Duration::days(pause.days),
        _ => on,
    }
}
