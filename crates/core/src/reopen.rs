//! Choices made on a defective notice.
//!
//! A choice made in reliance on a wrong mandatory notice is not a valid
//! choice. When the documents module finds a notice defective it sends a
//! corrected one, and once that has proof of delivery it opens a reopen
//! window on the policy. Inside the window the policyholder may make the
//! choice again, and the new choice applies by value date at the date the
//! original took effect: no fee, no underwriting, no waiting period, and
//! nothing asked of the policyholder first.
//!
//! Two choices live in the core and can be made again here: a decline of
//! indexation and a cancellation. Nothing is rewritten. Billing that the new
//! choice changes is reversed by new entries with the original value dates
//! and raised again; a cancellation set aside is undone by the reversal of
//! its own entry.

use std::collections::BTreeMap;

use chrono::{Days, NaiveDate};
use ic_ledger::{Ledger, Posting, Side};
use ic_money::Money;
use ic_product::schedule::{anniversary, period_start};
use ic_product::ProductDefinition;

use crate::accounts::{self, dims};
use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{NoticeRecord, Policy, PolicyStatus, Receivable, ReceivableKind, ReopenWindow, ReopenedChoice};
use crate::unwind::{reverse_billing_after, UNWIND_LIMIT_DAYS};
use crate::{arrears, billing, cash, earning, lifecycle};

/// Open a period in which choices made on a defective notice can be made again.
pub(crate) fn open_window(policy: &mut Policy, ctx: &mut Ctx<'_>, reference: String, purpose: String, from: NaiveDate, until: NaiveDate) -> Result<(), Reject> {
    if reference.trim().is_empty() || purpose.trim().is_empty() {
        return Err(Reject::new(
            "reference_required",
            "a reopen window names its reference and the purpose of the notice",
        ));
    }
    if until < from {
        return Err(Reject::new("date", format!("the window ends ({until}) before it starts ({from})")));
    }
    match policy.reopen_windows.get(&reference) {
        // The documents module sends this until it is acknowledged.
        Some(w) if w.purpose == purpose && w.from == from && w.until == until => return Ok(()),
        Some(w) => {
            return Err(Reject::new(
                "reopen_conflict",
                format!("{reference} is already open from {} until {} for {}", w.from, w.until, w.purpose),
            ))
        }
        None => {}
    }
    policy.reopen_windows.insert(
        reference.clone(),
        ReopenWindow {
            purpose: purpose.clone(),
            from,
            until,
            used: Vec::new(),
        },
    );
    ctx.emit(Event::ReopenWindowOpened {
        policy: policy.id.clone(),
        reopen_reference: reference,
        purpose,
        from,
        until,
    });
    Ok(())
}

/// Record proof that a mandatory notice was delivered. The latest delivery
/// for a purpose replaces the one before: a corrected notice restarts the clock.
pub(crate) fn record_notice(
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    purpose: String,
    notice: String,
    delivered_on: NaiveDate,
    earliest_effect: NaiveDate,
) -> Result<(), Reject> {
    if purpose.trim().is_empty() || notice.trim().is_empty() {
        return Err(Reject::new("reference_required", "a delivery names the notice and its purpose"));
    }
    if delivered_on > ctx.date || earliest_effect < delivered_on {
        return Err(Reject::new(
            "date",
            format!(
                "a notice delivered on {delivered_on} cannot be recorded on {} with effect from {earliest_effect}",
                ctx.date
            ),
        ));
    }
    let record = NoticeRecord {
        notice: notice.clone(),
        delivered_on,
        earliest_effect,
    };
    if policy.notices.get(&purpose) == Some(&record) {
        return Ok(());
    }
    policy.notices.insert(purpose.clone(), record);
    ctx.emit(Event::NoticeDeliveryRecorded {
        policy: policy.id.clone(),
        purpose,
        notice,
        delivered_on,
        earliest_effect,
    });
    Ok(())
}

/// Make a choice again under a reopen reference.
pub(crate) fn choose_again(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    reference: &str,
    choice: ReopenedChoice,
) -> Result<(), Reject> {
    let Some(window) = policy.reopen_windows.get(reference).cloned() else {
        return Err(Reject::new(
            "unknown_reopen_reference",
            format!("no reopen window {reference} is open on this policy"),
        ));
    };
    if ctx.date < window.from || ctx.date > window.until {
        return Err(Reject::new(
            "reopen_window_closed",
            format!("{reference} is open from {} until {}", window.from, window.until),
        ));
    }
    if window.used.contains(&choice) {
        return Err(Reject::new("reopen_used", format!("that choice has already been made again under {reference}")));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let effective = match choice {
        ReopenedChoice::IndexationAccepted => accept_indexation(ledger, catalog, &product, policy, ctx, reference, window.until)?,
        ReopenedChoice::CancellationSetAside => set_cancellation_aside(ledger, catalog, &product, policy, ctx, reference, window.until)?,
    };
    if let Some(w) = policy.reopen_windows.get_mut(reference) {
        w.used.push(choice);
    }
    ctx.emit(Event::ChoiceReopened {
        policy: policy.id.clone(),
        reopen_reference: reference.to_string(),
        choice,
        effective,
    });
    Ok(())
}

/// Set a decline of indexation aside. If the latest anniversary passed
/// with indexation declined, the step it would have added is added from
/// that anniversary and the periods since are billed again at the premium
/// that includes it. The decline no longer counts.
fn accept_indexation(
    ledger: &mut Ledger,
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    reference: &str,
    until: NaiveDate,
) -> Result<NaiveDate, Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "indexation applies to a policy in force"));
    }
    if let Some(hold) = &policy.unwind {
        return Err(Reject::new(
            "already_unwound",
            format!("the policy is held as at {} under {}", hold.after, hold.reference),
        ));
    }
    let Some(rate_code) = product.indexation() else {
        return Err(Reject::new("no_indexation", "this product has no indexation to accept"));
    };
    if !policy.indexation_declined && policy.declined_anniversaries.is_empty() {
        return Err(Reject::new("nothing_to_reopen", "indexation was not declined on this policy"));
    }
    policy.indexation_declined = false;
    ctx.emit(Event::IndexationChoiceRecorded {
        policy: policy.id.clone(),
        declined: false,
    });
    // Only the latest anniversary can be put right here: the notice in
    // question was the one sent before it.
    let latest = policy.anniversaries_processed;
    let missed = policy
        .declined_anniversaries
        .last()
        .copied()
        .filter(|n| *n == latest)
        .and_then(|n| anniversary(policy.inception, n))
        .filter(|on| catalog.index_rate_on(rate_code, *on).is_some())
        .filter(|on| ctx.date.checked_sub_days(Days::new(UNWIND_LIMIT_DAYS)).is_none_or(|limit| *on >= limit));
    let Some(on) = missed else {
        // Nothing has been missed yet: indexation applies at the next anniversary.
        return Ok(ctx.date);
    };
    let Some(day_before) = on.pred_opt() else {
        return Err(Reject::new("date", "the anniversary is out of range"));
    };
    reverse_billing_after(ledger, policy, ctx, day_before, reference)?;
    lifecycle::apply_indexation(catalog, product, policy, ctx, on)?;
    policy.declined_anniversaries.pop();
    carry_on(ledger, catalog, product, policy, ctx, on, until)?;
    Ok(on)
}

/// Set a cancellation aside: the policy is in force as if it had never
/// been cancelled, with nothing asked of the policyholder first.
fn set_cancellation_aside(
    ledger: &mut Ledger,
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    reference: &str,
    until: NaiveDate,
) -> Result<NaiveDate, Reject> {
    let Some(record) = policy.cancellation.clone().filter(|_| policy.status == PolicyStatus::Cancelled) else {
        return Err(Reject::new("not_cancelled", "only a cancelled policy has a cancellation to set aside"));
    };
    let c = policy.currency;
    let suspense = accounts::policy_suspense(&policy.id, c).id;
    let held = ledger.available(&suspense).max(0);
    // The cancellation's own entry, reversed. The refund it put in suspense
    // comes back out; whatever part of the refund has already been paid to
    // the policyholder is owed again, as a receivable due at the end of the
    // window, not demanded as a condition.
    let mut postings = Vec::new();
    let mut owed_again: i64 = 0;
    for posting in record.postings.iter().map(Posting::reversed) {
        match (&posting.amount, posting.side) {
            (ic_ledger::Amount::Money(m), Side::Debit) if posting.account == suspense => {
                let from_suspense = i64::try_from(held).unwrap_or(i64::MAX).min(m.minor);
                owed_again = m.minor - from_suspense;
                if from_suspense > 0 {
                    let mut p = posting.clone();
                    p.amount = Money {
                        minor: from_suspense,
                        currency: c,
                    }
                    .into();
                    postings.push(p);
                }
                if owed_again > 0 {
                    let mut p = Posting::debit(
                        accounts::premium_receivable(&policy.id, c).id,
                        Money {
                            minor: owed_again,
                            currency: c,
                        },
                    );
                    p.dims = dims(policy, None, "refund_owed_again");
                    postings.push(p);
                }
            }
            _ => postings.push(posting),
        }
    }
    let posted = ctx.entries.len();
    if !postings.is_empty() {
        let mut detail = BTreeMap::new();
        detail.insert("reference".to_string(), reference.to_string());
        if let Some(entry) = record.entry {
            detail.insert("reverses".to_string(), entry.to_string());
        }
        ctx.post(ledger, "cancellation_set_aside", Some(policy), record.cancelled_on, postings, detail)?;
    }
    policy.receivables = record.receivables_before;
    policy.covers = record.covers_before;
    policy.cancellation = None;
    ctx.emit(Event::PolicyStatusChanged {
        policy: policy.id.clone(),
        from: policy.status,
        to: PolicyStatus::InForce,
    });
    policy.status = PolicyStatus::InForce;
    arrears::set_standing(policy, ctx, record.standing_before);
    ctx.emit(Event::PolicyReinstated { policy: policy.id.clone() });
    if owed_again > 0 {
        let id = policy.next_receivable_id;
        policy.next_receivable_id += 1;
        policy.receivables.push(Receivable {
            id,
            kind: ReceivableKind::Adjustment,
            period_start: record.cancelled_on,
            period_end: record.cancelled_on,
            due: until,
            total_minor: owed_again,
            outstanding_minor: owed_again,
            gst_minor: 0,
            levies_minor: BTreeMap::new(),
            net_fine: BTreeMap::new(),
            earned_fine: BTreeMap::new(),
            closed: false,
            payer: None,
            entry: ctx.entries.get(posted).map(|e| e.id),
            settled_on: None,
        });
        ctx.emit(Event::PremiumDue {
            policy: policy.id.clone(),
            receivable: id,
            period_start: record.cancelled_on,
            period_end: record.cancelled_on,
            total_minor: owed_again,
            members: None,
            by_cover_type: Default::default(),
            discounts: vec![],
        });
    }
    carry_on(ledger, catalog, product, policy, ctx, policy.inception, until)?;
    Ok(record.cancelled_on)
}

/// Bring billing, cash and earning up to today after a choice was made
/// again. Money the new choice leaves owing for periods from `from` is due
/// at the end of the window, so making the choice again never puts the
/// policy into arrears by itself.
fn carry_on(
    ledger: &mut Ledger,
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    from: NaiveDate,
    until: NaiveDate,
) -> Result<(), Reject> {
    while period_start(policy.inception, policy.frequency, policy.periods_billed).is_some_and(|s| s <= ctx.date) {
        if !billing::bill_due_periods(ledger, catalog, product, policy, ctx)? {
            break;
        }
    }
    cash::apply_suspense(ledger, policy, ctx)?;
    for r in policy
        .receivables
        .iter_mut()
        .filter(|r| !r.closed && r.outstanding_minor > 0 && r.period_start >= from)
    {
        r.due = r.due.max(until);
    }
    if let Some(yesterday) = ctx.date.pred_opt() {
        earning::earn_through(ledger, policy, ctx, yesterday)?;
    }
    arrears::refresh(product, policy, ctx);
    Ok(())
}
