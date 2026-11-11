//! Group scheme administration in the core: membership as effective-dated
//! events, scheme billing in chunks, and retrospective premium adjustment.

use std::ops::Bound;

use chrono::{Days, NaiveDate};
use ic_ledger::Ledger;
use ic_product::schedule::period_start;
use ic_product::ProductDefinition;

use crate::billing::{raise_receivable, Bill};
use crate::bitemporal::Bitemporal;
use crate::catalog::Catalog;
use crate::command::MemberChange;
use crate::event::Event;
use crate::lifecycle::{member_covers, BACKDATING_LIMIT_DAYS};
use crate::machine::{Ctx, Reject, APPLY_BUDGET, SCHEME_BILLING_CHUNK};
use crate::policy::{LifeFactors, LifeKind, LifeRecord, PendingInvoice, Policy, PolicyStatus, ReceivableKind};
use crate::premium::{add_premium, member_period_premium, member_premium_with, zero_premium};
use crate::{LifeId, PartyId};

/// One step of a scheme invoice: price the next chunk of members. Returns
/// true when the invoice is complete and its single receivable has been
/// posted. No single command prices more than a chunk, so a 50,000-member
/// scheme never stalls its partition.
pub(crate) fn bill_scheme_step(
    ledger: &mut Ledger,
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    period: (NaiveDate, NaiveDate),
) -> Result<bool, Reject> {
    let mut pending = policy.pending_invoice.take().unwrap_or_else(|| PendingInvoice {
        period_index: policy.periods_billed,
        cursor: None,
        members: 0,
        premium: zero_premium(policy.currency),
    });
    let range = match &pending.cursor {
        Some(c) => (Bound::Excluded(c.clone()), Bound::Unbounded),
        None => (Bound::Unbounded, Bound::Unbounded),
    };
    let mut lines = Vec::new();
    let mut last = None;
    let mut visited = 0;
    let mut priced = pending.premium.clone();
    let mut members = pending.members;
    for (id, life) in policy.lives.range::<LifeId, _>(range) {
        if visited == SCHEME_BILLING_CHUNK {
            break;
        }
        visited += 1;
        last = Some(id.clone());
        if !life.in_force_on(period.0) || matches!(life.kind, LifeKind::Pool { .. }) {
            continue;
        }
        ctx.work += 4;
        let p = member_period_premium(catalog, product, policy, life, period.0)?;
        add_premium(&mut priced, &p)?;
        members += 1;
        lines.push((id.clone(), p.total()));
    }
    let complete = visited < SCHEME_BILLING_CHUNK || last.as_ref() == policy.lives.keys().next_back();
    pending.premium = priced;
    pending.members = members;
    pending.cursor = last;
    ctx.emit(Event::SchemeInvoiceChunk {
        policy: policy.id.clone(),
        period_index: pending.period_index,
        lines,
        complete,
    });
    if !complete {
        policy.pending_invoice = Some(pending);
        return Ok(false);
    }
    let bill = Bill::from_modal(policy, &pending.premium);
    let id = raise_receivable(ledger, policy, ctx, &bill, ReceivableKind::Period, period, period.0, "scheme_premium_due")?;
    policy.periods_billed += 1;
    if let Some(id) = id {
        ctx.emit(Event::PremiumDue {
            policy: policy.id.clone(),
            receivable: id,
            period_start: period.0,
            period_end: period.1,
            total_minor: bill.total(),
            members: Some(pending.members),
        });
    }
    Ok(true)
}

/// Billed periods a change effective on `effective` reaches into.
fn affected_periods(policy: &Policy, effective: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    (0..policy.periods_billed)
        .filter_map(|n| {
            Some((
                period_start(policy.inception, policy.frequency, n)?,
                period_start(policy.inception, policy.frequency, n + 1)?,
            ))
        })
        .filter(|(_, end)| *end > effective)
        .collect()
}

/// Apply one chunk of membership changes. A change notified late is applied
/// with its true effective date; the premium difference for periods already
/// billed is posted as an adjustment, value-dated from the change, and shows
/// as a credit or debit on the next invoice. Nothing already billed is edited.
pub(crate) fn apply_membership(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    changes: Vec<MemberChange>,
) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "membership can only change on a scheme in force"));
    }
    if !policy.covers.values().any(|c| c.formula_driven) {
        return Err(Reject::new("not_a_scheme", "this policy is not a group scheme"));
    }
    if policy.pending_invoice.is_some() {
        return Err(Reject::new("invoice_in_progress", "a scheme invoice is being built; retry shortly"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let limit = ctx
        .date
        .checked_sub_days(Days::new(BACKDATING_LIMIT_DAYS))
        .ok_or_else(|| Reject::new("date", "date out of range"))?;

    // Validate the whole chunk, and price both sides of every change, before
    // anything is written.
    let mut staged: Vec<(MemberChange, Vec<((NaiveDate, NaiveDate), Bill)>)> = Vec::new();
    let mut scratch = policy.clone();
    let mut estimated_work = 0u32;
    for change in changes {
        let (life_id, effective) = match &change {
            MemberChange::Joined { life, effective } => (life.id.clone(), *effective),
            MemberChange::Left { life, effective } | MemberChange::Changed { life, effective, .. } => (life.clone(), *effective),
        };
        if effective < limit || effective < policy.inception {
            return Err(Reject::new(
                "backdating_limit",
                format!("change to {life_id} effective {effective} is outside the backdating limit"),
            ));
        }
        let existing = scratch.lives.get(&life_id);
        let (old, new): (Option<LifeFactors>, Option<LifeFactors>) = match &change {
            MemberChange::Joined { life, .. } => {
                if existing.is_some() {
                    return Err(Reject::new("member_exists", format!("member {life_id} is already on the scheme")));
                }
                (None, Some(life.factors.clone()))
            }
            MemberChange::Left { .. } => {
                let l = existing
                    .filter(|l| l.left.is_none())
                    .ok_or_else(|| Reject::new("unknown_member", format!("member {life_id} is not an active member")))?;
                if effective < l.joined {
                    return Err(Reject::new("invalid_change", format!("member {life_id} cannot leave before joining")));
                }
                (l.factors.as_of(effective).cloned(), None)
            }
            MemberChange::Changed { factors, .. } => {
                let l = existing
                    .filter(|l| l.in_force_on(effective))
                    .ok_or_else(|| Reject::new("unknown_member", format!("member {life_id} is not a member on {effective}")))?;
                (l.factors.as_of(effective).cloned(), Some(factors.clone()))
            }
        };
        let mut adjustments = Vec::new();
        for (start, end) in affected_periods(&scratch, effective) {
            estimated_work += 12;
            let from = start.max(effective);
            let days = (end - start).num_days() as i128;
            let remaining = (end - from).num_days() as i128;
            let price = |f: &Option<LifeFactors>| -> Result<Bill, Reject> {
                match f {
                    Some(f) => Ok(Bill::from_modal(
                        &scratch,
                        &member_premium_with(catalog, &product, &scratch, &life_id, f, start)?,
                    )),
                    None => Ok(Bill::default()),
                }
            };
            let delta = price(&new)?.minus(&price(&old)?).scaled(remaining, days);
            if !delta.is_zero() {
                adjustments.push(((from, end), delta));
            }
        }
        // Later changes in the same chunk see this one.
        match &change {
            MemberChange::Joined { life, effective } => {
                scratch.lives.insert(
                    life.id.clone(),
                    LifeRecord {
                        id: life.id.clone(),
                        kind: life.kind.clone(),
                        factors: Bitemporal::new(*effective, ctx.date, ctx.index, life.factors.clone()),
                        joined: *effective,
                        left: None,
                    },
                );
            }
            MemberChange::Left { life, effective } => {
                if let Some(l) = scratch.lives.get_mut(life) {
                    l.left = Some(*effective);
                }
            }
            MemberChange::Changed { life, effective, factors } => {
                if let Some(l) = scratch.lives.get_mut(life) {
                    l.factors.record(*effective, ctx.date, ctx.index, factors.clone());
                }
            }
        }
        staged.push((change, adjustments));
    }
    if estimated_work > APPLY_BUDGET / 2 {
        return Err(Reject::new("apply_budget", "this chunk reaches into too many billed periods; split it"));
    }

    policy.lives = scratch.lives;
    for (change, adjustments) in staged {
        let (life_id, effective) = match &change {
            MemberChange::Joined { life, effective } => (life.id.clone(), *effective),
            MemberChange::Left { life, effective } | MemberChange::Changed { life, effective, .. } => (life.clone(), *effective),
        };
        ctx.emit(match &change {
            MemberChange::Joined { life, .. } => Event::MemberJoined {
                policy: policy.id.clone(),
                life: life_id.clone(),
                effective,
                covers: member_covers(&product, policy, &life_id, &life.factors),
            },
            MemberChange::Left { .. } => Event::MemberLeft {
                policy: policy.id.clone(),
                life: life_id.clone(),
                effective,
            },
            MemberChange::Changed { factors, .. } => Event::MemberChanged {
                policy: policy.id.clone(),
                life: life_id.clone(),
                effective,
                covers: member_covers(&product, policy, &life_id, factors),
            },
        });
        for (period, delta) in adjustments {
            // Due today, not on the original date: a late notification is the
            // employer's next invoice, not instant arrears.
            if let Some(id) = raise_receivable(ledger, policy, ctx, &delta, ReceivableKind::Adjustment, period, ctx.date, "premium_adjusted")? {
                ctx.emit(Event::PremiumAdjusted {
                    policy: policy.id.clone(),
                    receivable: id,
                    life: life_id.clone(),
                    effective,
                    total_minor: delta.total(),
                });
            }
        }
    }
    Ok(())
}

/// Identification: link an unnamed member to a person in the customer
/// master. The member record is never deleted; it is linked.
pub(crate) fn identify(policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId, party: PartyId) -> Result<(), Reject> {
    let id = policy.id.clone();
    let rec = policy
        .lives
        .get_mut(life)
        .ok_or_else(|| Reject::new("unknown_member", format!("member {life} is not on the policy")))?;
    match &mut rec.kind {
        LifeKind::UnnamedMember { linked_party, .. } => {
            if linked_party.as_ref().is_some_and(|p| *p != party) {
                return Err(Reject::new("already_identified", "the member is already linked to a different person"));
            }
            *linked_party = Some(party.clone());
            ctx.emit(Event::MemberIdentified {
                policy: id,
                life: life.clone(),
                party,
            });
            Ok(())
        }
        _ => Err(Reject::new("not_unnamed", "only an unnamed member record can be identified")),
    }
}
