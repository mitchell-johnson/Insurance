//! Unwinding a policy to a date, and ending it with a claim.
//!
//! An insured event can be notified weeks after it happened. By then the
//! policy has carried on: premiums were billed, an anniversary applied
//! indexation, a cover may have expired by age. The same is true when the
//! insurer finds its own error and has to put a policy back to where it
//! stood on a date.
//!
//! Nothing is removed or rewritten. An unwind appends new journal entries
//! today, each the reversal of an earlier entry, carrying the original
//! entry's ID and the original value date. The hash chain extends over them
//! and every root already published still verifies. A layer that started
//! after the date is ended on the day it started, so it existed for no days
//! and its history shows it was created and unwound.

use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use ic_ledger::{Dimensions, Ledger};
use ic_product::schedule::{anniversary, period_start};

use crate::accounts::{self, dims};
use crate::catalog::Catalog;
use crate::event::{Event, LayerInfo, PayableCover};
use crate::machine::{Ctx, Reject};
use crate::policy::{CoverStatus, LayerOrigin, Policy, PolicyStatus, UnwindCause, UnwindHold};
use crate::postings::PostingSet;
use crate::{arrears, earning, lifecycle};

/// How far back a policy can be unwound. End-of-day keeps settled
/// receivables for 400 days; behind that the billing to reverse is no longer
/// in the policy's state, only in the journal.
pub const UNWIND_LIMIT_DAYS: u64 = 365;

/// Reverse the billing of every period that starts after `after`, one entry
/// per receivable with the period's own value date, and put the billing
/// clock back so those periods are billed again when the policy carries on.
/// What was paid on them is the policyholder's money again, in suspense.
pub(crate) fn reverse_billing_after(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    after: NaiveDate,
    reference: &str,
) -> Result<Vec<u32>, Reject> {
    let c = policy.currency;
    let mut reversed = Vec::new();
    let mut unapplied: BTreeMap<String, i64> = BTreeMap::new();
    for i in 0..policy.receivables.len() {
        let r = policy.receivables[i].clone();
        if r.closed || r.period_start <= after {
            continue;
        }
        let paid_net = crate::counterparty::net_by_cover_type(std::slice::from_ref(&r), &BTreeMap::from([(r.id, r.total_minor - r.outstanding_minor)]));
        for (cover_type, amount) in paid_net {
            *unapplied.entry(cover_type).or_insert(0) += amount;
        }
        ctx.work += 1;
        let mut set = PostingSet::new();
        for (cover_type, net) in &r.net_fine {
            let earned = r.earned_fine.get(cover_type).copied().unwrap_or(0);
            set.debit(
                accounts::unearned_premium(&policy.id, c),
                net - earned,
                dims(policy, Some(cover_type), "unwind"),
            );
            set.debit(
                accounts::earned_premium(&policy.product_code, cover_type, c),
                earned,
                dims(policy, Some(cover_type), "unwind"),
            );
        }
        set.debit(accounts::gst_payable(c), r.gst_minor as i128, dims(policy, None, "gst"));
        for (code, amount) in &r.levies_minor {
            set.debit(accounts::levy_payable(code, c), *amount as i128, dims(policy, None, "levy"));
        }
        // What is still owed is no longer owed; what was paid is the
        // policyholder's money again.
        let paid = r.total_minor - r.outstanding_minor;
        set.credit(
            accounts::premium_receivable(&policy.id, c),
            r.outstanding_minor as i128,
            dims(policy, None, "unwind"),
        );
        set.credit(accounts::policy_suspense(&policy.id, c), paid as i128, Dimensions::default());
        let mut detail = BTreeMap::new();
        detail.insert("receivable".to_string(), r.id.to_string());
        detail.insert("reference".to_string(), reference.to_string());
        if let Some(entry) = r.entry {
            detail.insert("reverses".to_string(), entry.to_string());
        }
        let postings = set.into_postings(ledger)?;
        ctx.post(ledger, "premium_due_unwound", Some(policy), r.period_start, postings, detail)?;
        let r = &mut policy.receivables[i];
        r.outstanding_minor = 0;
        r.closed = true;
        reversed.push(r.id);
    }

    if !unapplied.is_empty() {
        ctx.emit(Event::PremiumUnapplied {
            policy: policy.id.clone(),
            reference: reference.to_string(),
            net_by_cover_type: unapplied,
        });
    }
    policy.periods_billed = (0..policy.periods_billed)
        .take_while(|n| period_start(policy.inception, policy.frequency, *n).is_some_and(|start| start <= after))
        .count() as u32;
    Ok(reversed)
}

/// Reverse every effective-dated change after `after` and hold the policy
/// as at that date.
pub(crate) fn unwind_after(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    after: NaiveDate,
    cause: UnwindCause,
    reference: &str,
) -> Result<(), Reject> {
    if reference.trim().is_empty() {
        return Err(Reject::new("reference_required", "an unwind names the claim or remediation case it belongs to"));
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force can be unwound"));
    }
    if policy.covers.values().any(|c| c.formula_driven) || policy.pending_invoice.is_some() {
        return Err(Reject::new(
            "scheme_policy",
            "a scheme is corrected through its membership file, by effective date; it is not unwound as a whole",
        ));
    }
    if let Some(hold) = &policy.unwind {
        return Err(Reject::new(
            "already_unwound",
            format!("the policy is held as at {} under {}", hold.after, hold.reference),
        ));
    }
    if after >= ctx.date || after < policy.inception {
        return Err(Reject::new(
            "date",
            format!(
                "the unwind date must be on or after inception ({}) and before today ({})",
                policy.inception, ctx.date
            ),
        ));
    }
    if ctx.date.checked_sub_days(Days::new(UNWIND_LIMIT_DAYS)).is_some_and(|limit| after < limit) {
        return Err(Reject::new(
            "too_old",
            format!("a policy can be unwound at most {UNWIND_LIMIT_DAYS} days back; an older correction is a remediation case worked from the journal"),
        ));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let keep_indexation = cause == UnwindCause::Claim && product.pays_step_applied_after_event();

    let reversed = reverse_billing_after(ledger, policy, ctx, after, reference)?;

    // Cover: a layer that started after the date never started, and a cover
    // that expired after the date has not expired.
    let id = policy.id.clone();
    for cover in policy.covers.values_mut() {
        let mut restored = false;
        for layer in cover.layers.iter_mut() {
            ctx.work += 1;
            if layer.start > after {
                if layer.end == Some(layer.start) || (keep_indexation && layer.origin == LayerOrigin::Indexation) {
                    continue;
                }
                layer.end = Some(layer.start);
                ctx.emit(Event::LayerUnwound {
                    policy: id.clone(),
                    cover: cover.code.clone(),
                    layer: layer.id,
                    start: layer.start,
                    reference: reference.to_string(),
                });
            } else if layer.end.is_some_and(|end| end > after) {
                layer.end = None;
                restored = true;
            }
        }
        if restored {
            cover.status = CoverStatus::InForce;
            ctx.emit(Event::CoverExpiryUnwound {
                policy: id.clone(),
                cover: cover.code.clone(),
                layers: cover
                    .layers
                    .iter()
                    .filter(|l| l.end.is_none())
                    .map(|l| LayerInfo {
                        cover: cover.code.clone(),
                        cover_type: cover.cover_type.clone(),
                        layer: l.id,
                        life: l.life.clone(),
                        sum_insured_minor: l.sum_insured_minor,
                        start: l.start,
                    })
                    .collect(),
                reference: reference.to_string(),
            });
        }
    }

    // The anniversaries and billing periods after the date have not happened.
    policy.anniversaries_processed = (1..=policy.anniversaries_processed)
        .take_while(|n| anniversary(policy.inception, *n).is_some_and(|on| on <= after))
        .count() as u32;
    policy.unwind = Some(UnwindHold {
        after,
        cause,
        reference: reference.to_string(),
        applied_on: ctx.date,
    });
    ctx.emit(Event::PolicyUnwound {
        policy: id,
        after,
        cause,
        reference: reference.to_string(),
        receivables: reversed,
    });
    arrears::refresh(&product, policy, ctx);
    Ok(())
}

/// Let a policy unwound for remediation carry on: the next end-of-day
/// replays its anniversaries and billing from the unwind date.
pub(crate) fn resume(policy: &mut Policy, ctx: &mut Ctx<'_>, reference: &str) -> Result<(), Reject> {
    match &policy.unwind {
        Some(hold) if hold.reference == reference => {}
        Some(hold) => {
            return Err(Reject::new(
                "wrong_reference",
                format!("the policy is held under {}, not {reference}", hold.reference),
            ))
        }
        None => return Err(Reject::new("not_unwound", "the policy is not held by an unwind")),
    }
    policy.unwind = None;
    ctx.emit(Event::PolicyUnwindResumed {
        policy: policy.id.clone(),
        reference: reference.to_string(),
    });
    Ok(())
}

/// End the policy with a claim as at the insured event: unwind whatever
/// happened after the event, earn exactly up to the day of the event, and
/// return the premium for every later day.
pub(crate) fn terminate_on_claim(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    event_date: NaiveDate,
    claim: &str,
    payer_deceased: bool,
) -> Result<(), Reject> {
    if claim.trim().is_empty() {
        return Err(Reject::new("reference_required", "a termination names the claim it belongs to"));
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force can be ended by a claim"));
    }
    if event_date > ctx.date || event_date < policy.inception {
        return Err(Reject::new(
            "date",
            format!(
                "the event must be on or after inception ({}) and not after today ({})",
                policy.inception, ctx.date
            ),
        ));
    }
    match &policy.unwind {
        Some(hold) if hold.after == event_date && hold.cause == UnwindCause::Claim => {}
        Some(hold) => {
            return Err(Reject::new(
                "already_unwound",
                format!("the policy is held as at {} under {}", hold.after, hold.reference),
            ))
        }
        None if event_date < ctx.date => unwind_after(ledger, catalog, policy, ctx, event_date, UnwindCause::Claim, claim)?,
        None => {}
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    // After the unwind, the layers left open are those in force at the
    // event and any later indexation step the product's wording pays.
    let payable = policy
        .covers
        .values()
        .filter(|c| c.status == CoverStatus::InForce)
        .map(|c| PayableCover {
            cover: c.code.clone(),
            cover_type: c.cover_type.clone(),
            sum_insured_minor: c.layers.iter().filter(|l| l.end.is_none()).map(|l| l.sum_insured_minor).sum(),
        })
        .collect();
    // The day of the event was a day of cover; no later day was.
    earning::earn_through(ledger, policy, ctx, event_date)?;
    let cover_ends = event_date.succ_opt().unwrap_or(event_date);
    let mut detail = BTreeMap::new();
    detail.insert("claim".to_string(), claim.to_string());
    lifecycle::close_out(ledger, &product, policy, ctx, false, "terminated_on_claim", event_date, cover_ends, detail)?;
    policy.unwind = None;
    arrears::set_standing(policy, ctx, None);
    ctx.emit(Event::PolicyStatusChanged {
        policy: policy.id.clone(),
        from: policy.status,
        to: PolicyStatus::Terminated,
    });
    policy.status = PolicyStatus::Terminated;
    ctx.emit(Event::PolicyTerminated {
        policy: policy.id.clone(),
        effective: event_date,
        claim: claim.to_string(),
        payable,
    });
    if payer_deceased {
        policy.refund_with_claim = Some(claim.to_string());
        ctx.emit(Event::PaymentAuthorityCancelled {
            policy: policy.id.clone(),
            claim: claim.to_string(),
        });
    }
    Ok(())
}
