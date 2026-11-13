//! Group scheme administration in the core: membership as effective-dated
//! events, scheme billing in chunks, and retrospective premium adjustment.

use std::collections::{BTreeSet, VecDeque};
use std::ops::Bound;

use chrono::{Days, NaiveDate};
use ic_ledger::{Dimensions, Ledger};
use ic_product::schedule::period_start;
use ic_product::ProductDefinition;

use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::billing::{raise_receivable, raise_receivable_for, Bill};
use crate::bitemporal::Bitemporal;
use crate::catalog::Catalog;
use crate::command::{ExcessLayer, MemberChange};
use crate::event::{Event, LayerInfo, LifeInfo, MemberCover};
use crate::lifecycle::{member_covers, BACKDATING_LIMIT_DAYS};
use crate::machine::{CoreState, Ctx, Reject, APPLY_BUDGET, MAX_MEMBERSHIP_CHUNK, SCHEME_BILLING_CHUNK};
use crate::policy::{
    CoverStatus, Layer, LayerOrigin, LifeFactors, LifeKind, LifeRecord, MemberPays, PendingInvoice, Policy, PolicyStatus, ReceivableKind, Standing,
};
use crate::postings::PostingSet;
use crate::premium::{add_premium, excess_layers_on, factors_on, member_period_line, member_period_premium, member_premium_detail, zero_premium, Loadings};
use crate::voluntary;
use crate::{LifeId, PartyId, PolicyId};
use ic_product::RateBasis;

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
    let mut loading_minor = 0i64;
    let mut member_bills: Vec<(LifeId, Bill)> = Vec::new();
    for (id, life) in policy.lives.range::<LifeId, _>(range) {
        if visited == SCHEME_BILLING_CHUNK {
            break;
        }
        visited += 1;
        last = Some(id.clone());
        if !life.in_force_on(period.0) {
            continue;
        }
        // A pool is billed on what the employer has declared for it. A pool
        // with no declaration has nothing to price.
        let declared = life.factors.as_of(period.0.max(life.joined)).and_then(|f| f.pool);
        if matches!(life.kind, LifeKind::Pool { .. }) && declared.is_none() {
            continue;
        }
        ctx.work += 4;
        let excess = excess_layers_on(policy, id, period.0);
        let (p, line) = if excess.iter().any(|(_, l)| !l.member_pays.is_nothing()) {
            // Part of this member's cover is paid for by the member. The
            // employer is billed as if that part were not there, and the
            // difference is the member's own bill.
            ctx.work += 8;
            let f = factors_on(life, period.0)?;
            let owned: Vec<(String, Layer)> = excess.iter().map(|(c, l)| (c.to_string(), (*l).clone())).collect();
            let employers = employer_part(&owned);
            let refs: Vec<(&str, &Layer)> = employers.iter().map(|(c, l)| (c.as_str(), l)).collect();
            let whole = member_premium_detail(catalog, product, policy, id, f, period.0, &excess, Loadings::All)?;
            let p = member_premium_detail(catalog, product, policy, id, f, period.0, &refs, Loadings::All)?;
            let line = member_premium_detail(catalog, product, policy, id, f, period.0, &refs, Loadings::ItemisedOnly)?.total();
            member_bills.push((id.clone(), Bill::from_modal(policy, &whole).minus(&Bill::from_modal(policy, &p))));
            (p, line)
        } else {
            let p = member_period_premium(catalog, product, policy, life, period.0)?;
            // A loading the member has not agreed to show stays off the member's
            // line and reaches the invoice only in the chunk's total.
            let hidden = excess.iter().any(|(_, l)| !l.loadings.is_empty() && !l.loading_itemised);
            let line = if hidden {
                ctx.work += 4;
                member_period_line(catalog, product, policy, life, period.0)?.total()
            } else {
                p.total()
            };
            (p, line)
        };
        add_premium(&mut priced, &p)?;
        // A dependant is covered but is not a member.
        members += declared.map_or(u32::from(!matches!(life.kind, LifeKind::Dependant { .. })), |d| d.headcount);
        loading_minor += p.total() - line;
        lines.push((id.clone(), line));
    }
    for (life, bill) in member_bills {
        bill_member(ledger, policy, ctx, &life, &bill, period, period.0, false)?;
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
        loading_minor,
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
            discounts: bill.discounts.clone(),
            by_cover_type: bill
                .net
                .iter()
                .filter(|(k, _)| k.as_str() != "policy_fee")
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        });
    }
    Ok(true)
}

/// A member's layers as the employer is billed for them: without the layers
/// the member pays for, and without the loading where the member pays that.
fn employer_part(layers: &[(String, Layer)]) -> Vec<(String, Layer)> {
    layers
        .iter()
        .filter(|(_, l)| l.member_pays != MemberPays::All)
        .map(|(cover, l)| {
            let mut l = l.clone();
            if l.member_pays == MemberPays::Loading {
                l.loadings.clear();
            }
            (cover.clone(), l)
        })
        .collect()
}

/// Bill a member for cover the member pays for themselves, and set any
/// credit the member holds against it.
#[allow(clippy::too_many_arguments)]
fn bill_member(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    life: &LifeId,
    bill: &Bill,
    period: Period,
    due: NaiveDate,
    adjustment: bool,
) -> Result<(), Reject> {
    let (kind, entry) = if adjustment {
        (ReceivableKind::Adjustment, "member_premium_adjusted")
    } else {
        (ReceivableKind::Period, "member_premium_due")
    };
    // A dependant's cover is billed to the member they are covered through.
    let life = &policy.payer_of(life);
    if let Some(receivable) = raise_receivable_for(ledger, policy, ctx, bill, kind, period, due, entry, Some(life))? {
        ctx.emit(Event::MemberPremiumBilled {
            policy: policy.id.clone(),
            life: life.clone(),
            receivable,
            period_start: period.0,
            period_end: period.1,
            total_minor: bill.total(),
            adjustment,
        });
        voluntary::apply_credit(ledger, policy, ctx, life)?;
    }
    Ok(())
}

/// End, from today, every layer a member pays for in whole or in part, with
/// both payers adjusted for the days left of periods already billed. Returns
/// the layers ended and the date.
pub(crate) fn end_member_paid_layers(
    ledger: &mut Ledger,
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    life: &LifeId,
) -> Result<(Vec<(String, u32)>, NaiveDate), Reject> {
    let effective = ctx.date;
    // The member's own cover, and their dependants' cover that they pay for.
    let mut lives = vec![life.clone()];
    lives.extend(policy.dependants_of(life).map(|l| l.id.clone()));
    let mut all = Vec::new();
    for life in &lives {
        let before = member_layers(policy, life);
        let mut after = before.clone();
        let mut ended = Vec::new();
        for (cover, l) in &mut after {
            if !l.member_pays.is_nothing() && l.start <= effective && l.end.is_none_or(|e| e > effective) {
                l.end = Some(effective);
                ended.push((cover.clone(), l.id, l.sum_insured_minor));
            }
        }
        if ended.is_empty() {
            continue;
        }
        let d = match policy.lives.get(life) {
            Some(record) => deltas(catalog, product, policy, life, effective, same_details(record), &before, &after)?,
            None => Deltas::default(),
        };
        ctx.work += d.work;
        for (cover, id, sum_insured_minor) in &ended {
            if let Some(l) = policy.covers.get_mut(cover).and_then(|c| c.layers.iter_mut().find(|l| l.id == *id)) {
                l.end = Some(effective);
            }
            ctx.emit(Event::LayerEnded {
                policy: policy.id.clone(),
                cover: cover.clone(),
                layer: *id,
                life: life.clone(),
                effective,
                sum_insured_minor: *sum_insured_minor,
            });
        }
        post_deltas(ledger, policy, ctx, life, effective, d)?;
        all.extend(ended.into_iter().map(|(cover, id, _)| (cover, id)));
    }
    Ok((all, effective))
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

type Period = (NaiveDate, NaiveDate);

/// The premium differences a change makes to periods already billed.
#[derive(Debug, Default)]
struct Deltas {
    /// What a member's line may show: the scheme's rates, and loadings the
    /// member agreed to show.
    shown: Vec<(Period, Bill)>,
    /// Loading that reaches the scheme only as a total.
    loading: Vec<(Period, Bill)>,
    /// What the member pays for themselves. Never part of the scheme's
    /// invoice or its totals.
    member: Vec<(Period, Bill)>,
    work: u32,
}

impl Deltas {
    fn total(&self) -> i64 {
        self.shown.iter().chain(&self.loading).map(|(_, b)| b.total()).sum()
    }
}

/// A member's own layers (underwritten cover above the automatic amount and
/// covers chosen from the menu), with the cover each sits on, copied out so
/// the policy can change underneath.
fn member_layers(policy: &Policy, life: &LifeId) -> Vec<(String, Layer)> {
    policy
        .covers
        .values()
        .flat_map(|c| c.layers.iter().filter(|l| c.members_own(l)).map(move |l| (c.code.clone(), l.clone())))
        .filter(|(_, l)| l.life == *life)
        .collect()
}

/// Price both sides of a change for every billed period it reaches into.
/// `sides` gives the member's details before and after for a period (None
/// where the member is not covered); `before` and `after` are the member's
/// underwritten layers on each side.
#[allow(clippy::too_many_arguments)]
fn deltas(
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &Policy,
    life: &LifeId,
    effective: NaiveDate,
    sides: impl Fn(NaiveDate) -> (Option<LifeFactors>, Option<LifeFactors>),
    before: &[(String, Layer)],
    after: &[(String, Layer)],
) -> Result<Deltas, Reject> {
    let mut out = Deltas::default();
    let member_paid = before.iter().chain(after).any(|(_, l)| !l.member_pays.is_nothing());
    let (whole_before, whole_after) = (before, after);
    let (before, after) = (&employer_part(before)[..], &employer_part(after)[..]);
    let hidden = before.iter().chain(after).any(|(_, l)| !l.loadings.is_empty() && !l.loading_itemised);
    for (start, end) in affected_periods(policy, effective) {
        out.work += if hidden { 24 } else { 12 };
        let from = start.max(effective);
        let days = (end - start).num_days() as i128;
        let remaining = (end - from).num_days() as i128;
        let (old, new) = sides(start);
        let price = |f: &Option<LifeFactors>, layers: &[(String, Layer)], view: Loadings| -> Result<Bill, Reject> {
            let Some(f) = f else { return Ok(Bill::default()) };
            let live: Vec<(&str, &Layer)> = layers.iter().filter(|(_, l)| l.in_force_on(from)).map(|(c, l)| (c.as_str(), l)).collect();
            Ok(Bill::from_modal(
                policy,
                &member_premium_detail(catalog, product, policy, life, f, start, &live, view)?,
            ))
        };
        let full = price(&new, after, Loadings::All)?
            .minus(&price(&old, before, Loadings::All)?)
            .scaled(remaining, days);
        let shown = if hidden {
            price(&new, after, Loadings::ItemisedOnly)?
                .minus(&price(&old, before, Loadings::ItemisedOnly)?)
                .scaled(remaining, days)
        } else {
            full.clone()
        };
        if member_paid {
            out.work += 12;
            let whole = price(&new, whole_after, Loadings::All)?
                .minus(&price(&old, whole_before, Loadings::All)?)
                .scaled(remaining, days);
            let member = whole.minus(&full);
            if !member.is_zero() {
                out.member.push(((from, end), member));
            }
        }
        let loading = full.minus(&shown);
        if !shown.is_zero() {
            out.shown.push(((from, end), shown));
        }
        if !loading.is_zero() {
            out.loading.push(((from, end), loading));
        }
    }
    Ok(out)
}

/// Post what [`deltas`] found. Due today, not on the original date: a late
/// notification is the employer's next invoice, not instant arrears.
fn post_deltas(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId, effective: NaiveDate, d: Deltas) -> Result<(), Reject> {
    for (period, delta) in d.shown {
        if let Some(id) = raise_receivable(ledger, policy, ctx, &delta, ReceivableKind::Adjustment, period, ctx.date, "premium_adjusted")? {
            ctx.emit(Event::PremiumAdjusted {
                discounts: delta.discounts.clone(),
                policy: policy.id.clone(),
                receivable: id,
                life: life.clone(),
                effective,
                total_minor: delta.total(),
            });
        }
    }
    for (period, delta) in d.loading {
        if let Some(id) = raise_receivable(ledger, policy, ctx, &delta, ReceivableKind::Adjustment, period, ctx.date, "premium_adjusted")? {
            ctx.emit(Event::LoadingAdjusted {
                policy: policy.id.clone(),
                receivable: id,
                total_minor: delta.total(),
            });
        }
    }
    for (period, delta) in d.member {
        bill_member(ledger, policy, ctx, life, &delta, period, ctx.date, true)?;
    }
    Ok(())
}

fn layer_info(cover: &crate::policy::Cover, layer: &Layer) -> LayerInfo {
    LayerInfo {
        cover: cover.code.clone(),
        cover_type: cover.cover_type.clone(),
        layer: layer.id,
        life: layer.life.clone(),
        sum_insured_minor: layer.sum_insured_minor,
        start: layer.start,
    }
}

fn party_of(kind: &LifeKind) -> Option<PartyId> {
    match kind {
        LifeKind::Named { party } => Some(party.clone()),
        LifeKind::UnnamedMember { linked_party, .. } => linked_party.clone(),
        LifeKind::Dependant { party, .. } => party.clone(),
        LifeKind::Pool { .. } => None,
    }
}

enum Outcome {
    Joined { covers: Vec<MemberCover>, party: Option<PartyId> },
    Left,
    Changed { covers: Vec<MemberCover> },
}

/// One validated and priced membership change, ready to be written.
struct Staged {
    life: LifeId,
    effective: NaiveDate,
    outcome: Outcome,
    deltas: Deltas,
    /// Underwritten layers that end with a leave: cover, layer, sum insured.
    ended: Vec<(String, u32, i64)>,
    /// Underwritten layers that come back with a reinstatement.
    restored: Vec<(LayerInfo, LayerOrigin)>,
}

/// Validate a chunk of membership changes against the policy and price both
/// sides of each, writing nothing. Returns the policy as it would be.
fn stage(
    catalog: &Catalog,
    policy: &Policy,
    date: NaiveDate,
    index: u64,
    changes: Vec<MemberChange>,
) -> Result<(Policy, ProductDefinition, Vec<Staged>), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "membership can only change on a scheme in force"));
    }
    if !policy.is_scheme() {
        return Err(Reject::new("not_a_scheme", "this policy is not a group scheme"));
    }
    if policy.pending_invoice.is_some() {
        return Err(Reject::new("invoice_in_progress", "a scheme invoice is being built; retry shortly"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let limit = date
        .checked_sub_days(Days::new(BACKDATING_LIMIT_DAYS))
        .ok_or_else(|| Reject::new("date", "date out of range"))?;

    let mut staged: Vec<Staged> = Vec::new();
    let mut scratch = policy.clone();
    let mut estimated_work = 0u32;
    let mut queue: VecDeque<MemberChange> = changes.into();
    while let Some(change) = queue.pop_front() {
        let life_id = match &change {
            MemberChange::Joined { life, .. } => life.id.clone(),
            MemberChange::Left { life, .. } | MemberChange::Changed { life, .. } | MemberChange::Reinstated { life } => life.clone(),
        };
        let existing = scratch.lives.get(&life_id);
        let effective = match &change {
            MemberChange::Joined { effective, .. } | MemberChange::Left { effective, .. } | MemberChange::Changed { effective, .. } => *effective,
            MemberChange::Reinstated { .. } => existing
                .and_then(|l| l.left)
                .ok_or_else(|| Reject::new("not_left", format!("member {life_id} has not left the scheme")))?,
        };
        if effective < limit || effective < policy.inception {
            return Err(Reject::new(
                "backdating_limit",
                format!("change to {life_id} effective {effective} is outside the backdating limit"),
            ));
        }
        let (old, new): (Option<LifeFactors>, Option<LifeFactors>) = match &change {
            MemberChange::Joined { life, .. } => {
                if existing.is_some() {
                    return Err(Reject::new("member_exists", format!("member {life_id} is already on the scheme")));
                }
                if let LifeKind::Dependant { of, .. } = &life.kind {
                    let member = scratch.lives.get(of).filter(|m| m.in_force_on(effective));
                    if !member.is_some_and(|m| !matches!(m.kind, LifeKind::Dependant { .. } | LifeKind::Pool { .. })) {
                        return Err(Reject::new(
                            "unknown_member",
                            format!("dependant {life_id} joins through a member of the scheme on {effective}, and {of} is not one"),
                        ));
                    }
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
            MemberChange::Reinstated { .. } => (None, existing.and_then(|l| l.factors.as_of(NaiveDate::MAX).cloned())),
        };
        // A pool nobody has declared has nothing to price, as on the invoice.
        let is_pool = match &change {
            MemberChange::Joined { life, .. } => matches!(life.kind, LifeKind::Pool { .. }),
            _ => existing.is_some_and(|l| matches!(l.kind, LifeKind::Pool { .. })),
        };
        let (old, new) = (old.filter(|f| !is_pool || f.pool.is_some()), new.filter(|f| !is_pool || f.pool.is_some()));
        let before = member_layers(&scratch, &life_id);
        let mut ended = Vec::new();
        let mut restored = Vec::new();
        // Later changes in the same chunk see this one.
        let outcome = match &change {
            MemberChange::Joined { life, effective } => {
                scratch.lives.insert(
                    life.id.clone(),
                    LifeRecord {
                        id: life.id.clone(),
                        kind: life.kind.clone(),
                        factors: Bitemporal::new(*effective, date, index, life.factors.clone()),
                        joined: *effective,
                        left: None,
                    },
                );
                Outcome::Joined {
                    covers: member_covers(&product, &scratch, &life_id, &life.factors),
                    party: party_of(&life.kind),
                }
            }
            MemberChange::Left { life, effective } => {
                if let Some(l) = scratch.lives.get_mut(life) {
                    l.left = Some(*effective);
                }
                // Dependants are on the scheme through their member.
                for dependant in scratch.dependants_of(life).filter(|d| d.left.is_none()) {
                    queue.push_back(MemberChange::Left {
                        life: dependant.id.clone(),
                        effective: (*effective).max(dependant.joined),
                    });
                }
                // A member's own cover is theirs while they are a member.
                for cover in scratch.covers.values_mut() {
                    let formula = cover.formula_driven;
                    for layer in cover
                        .layers
                        .iter_mut()
                        .filter(|l| l.life == *life && (l.origin == LayerOrigin::MemberChoice || (formula && l.origin == LayerOrigin::UnderwrittenExcess)))
                    {
                        if layer.end.is_none_or(|e| e > *effective) {
                            layer.end = Some((*effective).max(layer.start));
                            ended.push((cover.code.clone(), layer.id, layer.sum_insured_minor));
                        }
                    }
                }
                Outcome::Left
            }
            MemberChange::Changed { life, effective, factors } => {
                if let Some(l) = scratch.lives.get_mut(life) {
                    l.factors.record(*effective, date, index, factors.clone());
                }
                Outcome::Changed {
                    covers: member_covers(&product, &scratch, &life_id, factors),
                }
            }
            MemberChange::Reinstated { life } => {
                let mut party = None;
                if let Some(l) = scratch.lives.get_mut(life) {
                    l.left = None;
                    party = party_of(&l.kind);
                }
                // Dependants who left with the member come back with them.
                for dependant in scratch.dependants_of(life).filter(|d| d.left == Some(effective)) {
                    queue.push_back(MemberChange::Reinstated { life: dependant.id.clone() });
                }
                for cover in scratch.covers.values_mut() {
                    let (code, cover_type, formula) = (cover.code.clone(), cover.cover_type.clone(), cover.formula_driven);
                    for layer in cover
                        .layers
                        .iter_mut()
                        .filter(|l| l.life == *life && (l.origin == LayerOrigin::MemberChoice || (formula && l.origin == LayerOrigin::UnderwrittenExcess)))
                    {
                        if layer.end == Some(effective) && layer.start < effective {
                            layer.end = None;
                            restored.push((
                                LayerInfo {
                                    cover: code.clone(),
                                    cover_type: cover_type.clone(),
                                    layer: layer.id,
                                    life: life.clone(),
                                    sum_insured_minor: layer.sum_insured_minor,
                                    start: layer.start,
                                },
                                layer.origin,
                            ));
                        }
                    }
                }
                Outcome::Joined {
                    covers: new.as_ref().map(|f| member_covers(&product, &scratch, &life_id, f)).unwrap_or_default(),
                    party,
                }
            }
        };
        let after = member_layers(&scratch, &life_id);
        let d = deltas(
            catalog,
            &product,
            &scratch,
            &life_id,
            effective,
            |_| (old.clone(), new.clone()),
            &before,
            &after,
        )?;
        estimated_work += d.work;
        staged.push(Staged {
            life: life_id,
            effective,
            outcome,
            deltas: d,
            ended,
            restored,
        });
    }
    if estimated_work > APPLY_BUDGET / 2 {
        return Err(Reject::new("apply_budget", "this chunk reaches into too many billed periods; split it"));
    }
    Ok((scratch, product, staged))
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
    let (scratch, _, staged) = stage(catalog, policy, ctx.date, ctx.index, changes)?;
    policy.lives = scratch.lives;
    policy.covers = scratch.covers;
    for s in staged {
        let info = policy
            .lives
            .get(&s.life)
            .map(|l| LifeInfo::with_factors(l.id.clone(), party_of(&l.kind), l.factors.as_of(NaiveDate::MAX)));
        ctx.emit(match s.outcome {
            Outcome::Joined { covers, party } => Event::MemberJoined {
                policy: policy.id.clone(),
                life: s.life.clone(),
                effective: s.effective,
                covers,
                party,
                info,
            },
            Outcome::Left => Event::MemberLeft {
                policy: policy.id.clone(),
                life: s.life.clone(),
                effective: s.effective,
            },
            Outcome::Changed { covers } => Event::MemberChanged {
                policy: policy.id.clone(),
                life: s.life.clone(),
                effective: s.effective,
                covers,
                info,
            },
        });
        for (cover, layer, sum_insured_minor) in s.ended {
            ctx.emit(Event::LayerEnded {
                policy: policy.id.clone(),
                cover,
                layer,
                life: s.life.clone(),
                effective: s.effective,
                sum_insured_minor,
            });
        }
        for (layer, origin) in s.restored {
            ctx.emit(Event::LayerAdded {
                policy: policy.id.clone(),
                layer,
                origin,
                continues: None,
            });
        }
        post_deltas(ledger, policy, ctx, &s.life, s.effective, s.deltas)?;
    }
    Ok(())
}

/// What one membership change would do, without doing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePreview {
    pub life: LifeId,
    pub effective: NaiveDate,
    /// The member's automatic cover after the change. Empty for a leaver.
    pub covers: Vec<MemberCover>,
    /// The adjustment to premium already billed that a member's line shows.
    pub adjustment_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipPreview {
    pub changes: Vec<ChangePreview>,
    /// Everything the chunk would add to or take off the next invoice.
    pub adjustment_total_minor: i64,
}

/// The dry run of [`Command::ApplyMembership`](crate::Command): the same
/// validation and the same pricing, against the state as it stands, with
/// nothing written. An employer's preview is this, so it cannot disagree with
/// what applying the file then does.
pub fn preview_membership(state: &CoreState, policy: &PolicyId, changes: Vec<MemberChange>) -> Result<MembershipPreview, Reject> {
    if changes.len() > MAX_MEMBERSHIP_CHUNK {
        return Err(Reject::new(
            "apply_budget",
            format!("at most {MAX_MEMBERSHIP_CHUNK} membership changes per command"),
        ));
    }
    let date = state
        .business_date
        .ok_or_else(|| Reject::new("business_date", "the partition has no business date yet"))?;
    let policy = state
        .policies
        .get(policy)
        .ok_or_else(|| Reject::new("unknown_policy", format!("policy {policy} is not on this partition")))?;
    if policy.is_frozen() {
        return Err(Reject::new("policy_transferring", format!("policy {} is being transferred", policy.id)));
    }
    let (_, _, staged) = stage(&state.catalog, policy, date, 0, changes)?;
    let adjustment_total_minor = staged.iter().map(|s| s.deltas.total()).sum();
    let changes = staged
        .into_iter()
        .map(|s| ChangePreview {
            life: s.life,
            effective: s.effective,
            adjustment_minor: s.deltas.shown.iter().map(|(_, b)| b.total()).sum(),
            covers: match s.outcome {
                Outcome::Joined { covers, .. } | Outcome::Changed { covers } => covers,
                Outcome::Left => vec![],
            },
        })
        .collect();
    Ok(MembershipPreview {
        changes,
        adjustment_total_minor,
    })
}

/// Checks shared by the two underwritten-layer commands.
fn excess_guard<'a>(catalog: &'a Catalog, policy: &Policy, ctx: &Ctx<'_>, cover: &str, effective: NaiveDate) -> Result<&'a ProductDefinition, Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "cover can only change on a scheme in force"));
    }
    if policy.pending_invoice.is_some() {
        return Err(Reject::new("invoice_in_progress", "a scheme invoice is being built; retry shortly"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?;
    let on_scheme = policy.covers.get(cover).filter(|c| c.formula_driven && c.status == CoverStatus::InForce);
    let def = on_scheme
        .and_then(|_| product.cover(cover))
        .ok_or_else(|| Reject::new("unknown_cover", format!("{cover} is not a scheme cover in force on this policy")))?;
    if def.rating.basis != RateBasis::PerThousand {
        return Err(Reject::new("not_rated_by_amount", format!("{cover} is not priced by sum insured")));
    }
    let limit = ctx
        .date
        .checked_sub_days(Days::new(BACKDATING_LIMIT_DAYS))
        .ok_or_else(|| Reject::new("date", "date out of range"))?;
    if effective < limit || effective < policy.inception {
        return Err(Reject::new("backdating_limit", format!("{effective} is outside the backdating limit")));
    }
    if effective > ctx.date {
        return Err(Reject::new("future_dated", format!("{effective} is after the business date")));
    }
    Ok(product)
}

/// The member's details at the start of a period, on both sides of a change
/// that does not alter them.
fn same_details(life: &LifeRecord) -> impl Fn(NaiveDate) -> (Option<LifeFactors>, Option<LifeFactors>) + '_ {
    |start| {
        let f = life.factors.as_of(start.max(life.joined)).cloned();
        (f.clone(), f)
    }
}

/// Write a member's underwritten cover above the automatic amount as its own
/// layer. Premium for periods already billed is adjusted from the start date.
pub(crate) fn add_excess_layer(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: ExcessLayer) -> Result<(), Reject> {
    let product = excess_guard(catalog, policy, ctx, &spec.cover, spec.start)?.clone();
    let life = policy
        .lives
        .get(&spec.life)
        .filter(|l| l.in_force_on(spec.start) && !matches!(l.kind, LifeKind::Pool { .. }))
        .ok_or_else(|| Reject::new("unknown_member", format!("member {} is not a member on {}", spec.life, spec.start)))?
        .clone();
    if spec.sum_insured_minor <= 0 {
        return Err(Reject::new("sum_insured", "an underwritten layer needs a positive sum insured"));
    }
    if spec.loadings.iter().any(|l| l.get().is_negative()) {
        return Err(Reject::new("invalid_loading", "a loading cannot be negative"));
    }
    if spec.member_pays == MemberPays::Loading && spec.loadings.is_empty() {
        return Err(Reject::new("invalid_payer", "the member cannot pay the loading on a layer that has none"));
    }
    let replaces = |l: &Layer| Some(l.id) == spec.continues && l.life == spec.life && l.origin == LayerOrigin::UnderwrittenExcess;
    if spec.continues.is_some() && !policy.covers[&spec.cover].layers.iter().any(replaces) {
        return Err(Reject::new(
            "unknown_layer",
            format!("member {} has no underwritten layer of that number on {} to carry on", spec.life, spec.cover),
        ));
    }
    let continues = spec.continues;
    let before = member_layers(policy, &spec.life);
    let id = policy.covers[&spec.cover].layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let layer = Layer {
        id,
        life: spec.life.clone(),
        start: spec.start,
        end: None,
        sum_insured_minor: spec.sum_insured_minor,
        origin: LayerOrigin::UnderwrittenExcess,
        excess_option: None,
        loadings: spec.loadings,
        exclusions: spec.exclusions,
        loading_itemised: spec.loading_itemised,
        rated_from: None,
        member_pays: spec.member_pays,
    };
    let mut after = before.clone();
    after.push((spec.cover.clone(), layer.clone()));
    let d = deltas(catalog, &product, policy, &spec.life, spec.start, same_details(&life), &before, &after)?;
    ctx.work += d.work;
    let info = policy.covers.get_mut(&spec.cover).map(|cover| {
        cover.layers.push(layer.clone());
        layer_info(cover, &layer)
    });
    if let Some(layer) = info {
        ctx.emit(Event::LayerAdded {
            policy: policy.id.clone(),
            layer,
            origin: LayerOrigin::UnderwrittenExcess,
            continues,
        });
    }
    post_deltas(ledger, policy, ctx, &spec.life, spec.start, d)
}

/// End an underwritten layer from a date. Premium already billed for later
/// days is credited.
pub(crate) fn end_excess_layer(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    cover: &str,
    layer: u32,
    effective: NaiveDate,
) -> Result<(), Reject> {
    let product = excess_guard(catalog, policy, ctx, cover, effective)?.clone();
    let found = policy.covers[cover]
        .layers
        .iter()
        .find(|l| l.id == layer && l.origin == LayerOrigin::UnderwrittenExcess)
        .filter(|l| l.end.is_none_or(|e| e > effective) && l.start <= effective)
        .ok_or_else(|| {
            Reject::new(
                "unknown_layer",
                format!("layer {layer} of {cover} is not an underwritten layer in force on {effective}"),
            )
        })?
        .clone();
    let life = policy
        .lives
        .get(&found.life)
        .ok_or_else(|| Reject::new("unknown_member", format!("member {} is not on the policy", found.life)))?
        .clone();
    let before = member_layers(policy, &found.life);
    let mut after = before.clone();
    for (code, l) in &mut after {
        if code == cover && l.id == layer {
            l.end = Some(effective);
        }
    }
    let d = deltas(catalog, &product, policy, &found.life, effective, same_details(&life), &before, &after)?;
    ctx.work += d.work;
    if let Some(l) = policy.covers.get_mut(cover).and_then(|c| c.layers.iter_mut().find(|l| l.id == layer)) {
        l.end = Some(effective);
    }
    ctx.emit(Event::LayerEnded {
        policy: policy.id.clone(),
        cover: cover.to_string(),
        layer,
        life: found.life.clone(),
        effective,
        sum_insured_minor: found.sum_insured_minor,
    });
    post_deltas(ledger, policy, ctx, &found.life, effective, d)
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

/// Where a scheme made by a split came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitFrom {
    pub policy: PolicyId,
    pub effective: NaiveDate,
}

/// Move some of a scheme's members to a new scheme from the first day of a
/// billing period: a sale or a demerger. They go with their details as
/// recorded over time, the date they joined and their underwritten cover on
/// its terms, as members who moved, not as leavers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitScheme {
    pub policy: PolicyId,
    pub new_policy: PolicyId,
    pub new_number: String,
    pub lives: Vec<LifeId>,
    pub effective: NaiveDate,
}

impl SplitScheme {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        &["group-admin"]
    }
}

/// Split a scheme. `child` is the new scheme where an earlier command of the
/// same split made it. Returns the new scheme as it now stands; the caller
/// keeps it.
///
/// The new scheme has the old one's product version, group, start date,
/// frequency and renewal, and is billed from the period that starts on the
/// effective date. The old scheme is credited for the moved members on
/// every period already billed from then. Money, claims and history stay
/// with the old scheme.
pub(crate) fn split(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    child: Option<Policy>,
    ctx: &mut Ctx<'_>,
    spec: SplitScheme,
) -> Result<Policy, Reject> {
    if spec.lives.is_empty() || spec.lives.len() > MAX_MEMBERSHIP_CHUNK {
        return Err(Reject::new(
            "apply_budget",
            format!("a split moves between one and {MAX_MEMBERSHIP_CHUNK} members per command"),
        ));
    }
    if !crate::interest::plain(&spec.new_number) || spec.new_policy == policy.id {
        return Err(Reject::new("invalid_split", "the new scheme has its own ID and a plain number"));
    }
    // The period that starts on the effective date: billed already, or next.
    let period = (0..=policy.periods_billed).find(|n| period_start(policy.inception, policy.frequency, *n) == Some(spec.effective));
    let Some(period) = period else {
        return Err(Reject::new(
            "effective",
            "a split takes effect on the first day of a billing period, billed or next",
        ));
    };
    let origin = SplitFrom {
        policy: policy.id.clone(),
        effective: spec.effective,
    };
    if let Some(existing) = &child {
        if existing.split_from.as_ref() != Some(&origin) {
            return Err(Reject::new("policy_exists", format!("policy {} already exists", spec.new_policy)));
        }
        if existing.periods_billed > period || existing.pending_invoice.is_some() {
            return Err(Reject::new(
                "split_billed",
                "the new scheme has been billed; move further members by a new split",
            ));
        }
    }
    let c = policy.currency;
    for life in &spec.lives {
        if policy.lives.get(life).is_some_and(|l| matches!(l.kind, LifeKind::Pool { .. })) {
            return Err(Reject::new("pool_life", format!("{life} is a pool of members, which a split cannot divide")));
        }
    }
    for life in &spec.lives {
        let through = match policy.lives.get(life).map(|l| &l.kind) {
            Some(LifeKind::Dependant { of, .. }) => Some(of),
            _ => None,
        };
        let left_behind = policy.dependants_of(life).any(|d| d.in_force_on(spec.effective) && !spec.lives.contains(&d.id));
        if through.is_some_and(|of| !spec.lives.contains(of)) || left_behind {
            return Err(Reject::new("dependants", format!("{life}: a member and their dependants move together")));
        }
    }
    let staying = policy
        .lives
        .values()
        .filter(|l| l.in_force_on(spec.effective) && !spec.lives.contains(&l.id))
        .count();
    if staying == 0 {
        return Err(Reject::new("not_a_split", "a split leaves members behind; to move a whole scheme, transfer it"));
    }

    // What moves, as it stands before the old scheme lets it go.
    let moving: Vec<LifeRecord> = spec.lives.iter().filter_map(|l| policy.lives.get(l).cloned()).collect();
    let mut layers: Vec<(String, Layer)> = Vec::new();
    for cover in policy.covers.values() {
        for layer in cover
            .layers
            .iter()
            .filter(|l| spec.lives.contains(&l.life) && l.end.is_none_or(|e| e > spec.effective))
        {
            layers.push((cover.code.clone(), layer.clone()));
        }
    }

    // On the old scheme the members stop from the date, priced as leavers
    // are: every period billed from then is credited.
    let changes = spec
        .lives
        .iter()
        // A dependant leaves with their member.
        .filter(|life| !policy.lives.get(*life).is_some_and(|l| matches!(l.kind, LifeKind::Dependant { .. })))
        .map(|life| MemberChange::Left {
            life: life.clone(),
            effective: spec.effective,
        })
        .collect();
    let (scratch, _, staged) = stage(catalog, policy, ctx.date, ctx.index, changes)?;
    policy.lives = scratch.lives;
    policy.covers = scratch.covers;
    for s in staged {
        post_deltas(ledger, policy, ctx, &s.life, s.effective, s.deltas)?;
    }

    let mut new = match child {
        Some(existing) => existing,
        None => {
            let mut covers = policy.covers.clone();
            covers.values_mut().for_each(|cover| cover.layers.clear());
            Policy {
                id: spec.new_policy.clone(),
                number: spec.new_number.clone(),
                group: policy.group.clone(),
                policyholder: policy.policyholder.clone(),
                product_code: policy.product_code.clone(),
                product_hash: policy.product_hash.clone(),
                currency: policy.currency,
                statutory_fund: policy.statutory_fund.clone(),
                inception: policy.inception,
                frequency: policy.frequency,
                status: PolicyStatus::InForce,
                standing: Some(Standing::Paid),
                covers,
                indexation_declined: policy.indexation_declined,
                periods_billed: period,
                next_receivable_id: 1,
                anniversaries_processed: policy.anniversaries_processed,
                variations: policy.variations.clone(),
                declined_anniversaries: policy.declined_anniversaries.clone(),
                split_from: Some(origin),
                scheme: policy.scheme,
                covers_elsewhere: 0,
                lives: Default::default(),
                reopen_windows: Default::default(),
                notices: Default::default(),
                cancellation: None,
                avoidance: None,
                surrender: None,
                arrangement: None,
                arrears_pause: None,
                hardship: None,
                pruned: Default::default(),
                migrated: false,
                receivables: vec![],
                receipts: Default::default(),
                pending_invoice: None,
                lapse: None,
                transfer_out: None,
                history: vec![],
                unwind: None,
                refund_with_claim: None,
                claims: Default::default(),
                refunds_raised: Default::default(),
                payouts: Default::default(),
                member_accounts: Default::default(),
                member_standing: Default::default(),
                interests: Default::default(),
                nominations: vec![],
                owner_hold: None,
                entitlements: Vec::new(),
                discount_overrides: Vec::new(),
                continuation_of: None,
                payments: Default::default(),
                cessions: Default::default(),
            }
        }
    };
    for mut life in moving {
        life.left = None;
        new.lives.insert(life.id.clone(), life);
    }
    let mut moved = Vec::new();
    for (code, layer) in layers {
        moved.push((layer.life.clone(), code.clone(), layer.id));
        if let Some(cover) = new.covers.get_mut(&code) {
            cover.layers.push(layer);
        }
    }
    // A moved member's credit goes with them, to meet their bills on the
    // new scheme. What they owe for cover the old scheme gave stays owed
    // there.
    for life in &spec.lives {
        let credit = i64::try_from(voluntary::credit_minor(ledger, policy, life)).unwrap_or(0);
        if credit <= 0 {
            continue;
        }
        new.member_accounts.insert(life.clone());
        let mut set = PostingSet::new();
        set.debit(accounts::member_suspense(&policy.id, life, c), credit as i128, Dimensions::default());
        set.credit(accounts::member_suspense(&new.id, life, c), credit as i128, Dimensions::default());
        let postings = set.into_postings(ledger)?;
        let detail = std::collections::BTreeMap::from([("to_policy".to_string(), new.id.to_string())]);
        ctx.post(ledger, "member_credit_moved", Some(policy), ctx.date, postings, detail)?;
        ctx.emit(Event::MemberCreditMoved {
            policy: policy.id.clone(),
            new_policy: new.id.clone(),
            life: life.clone(),
            amount_minor: credit,
        });
    }
    ctx.emit(Event::SchemeSplit {
        policy: policy.id.clone(),
        new_policy: new.id.clone(),
        new_number: new.number.clone(),
        lives: spec.lives,
        effective: spec.effective,
        layers: moved,
    });
    Ok(new)
}

/// One cover chosen for a life from a scheme's menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenCover {
    pub code: String,
    /// The level of excess, where the cover offers a choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excess_option: Option<String>,
    /// The member pays for this cover themselves. Otherwise the employer
    /// does, on the scheme's invoice. A dependant's cover the member pays
    /// for is billed to the member.
    #[serde(default)]
    pub member_pays: bool,
}

/// Set the covers a scheme member has chosen from the scheme's menu, for
/// themselves or for one dependant: the whole set, from a date. Covers no
/// longer in the set end that day, new ones start, and a cover whose level
/// and payer are unchanged carries on untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMemberCovers {
    pub policy: PolicyId,
    pub life: LifeId,
    pub covers: Vec<ChosenCover>,
    pub effective: NaiveDate,
    /// The enrolment or change this comes from.
    pub reference: String,
}

impl SetMemberCovers {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        &["group-admin"]
    }
}

/// Apply [`SetMemberCovers`]. Premium for periods already billed is adjusted
/// from the date, for whoever pays each cover.
pub(crate) fn set_member_covers(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: SetMemberCovers) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "cover can only change on a scheme in force"));
    }
    if !policy.is_scheme() {
        return Err(Reject::new("not_a_scheme", "this policy is not a group scheme"));
    }
    if policy.pending_invoice.is_some() {
        return Err(Reject::new("invoice_in_progress", "a scheme invoice is being built; retry shortly"));
    }
    if !crate::interest::plain(&spec.reference) {
        return Err(Reject::new("invalid_choice", "the reference is a plain identifier"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let effective = spec.effective;
    let limit = ctx
        .date
        .checked_sub_days(Days::new(BACKDATING_LIMIT_DAYS))
        .ok_or_else(|| Reject::new("date", "date out of range"))?;
    if effective < limit || effective < policy.inception {
        return Err(Reject::new("backdating_limit", format!("{effective} is outside the backdating limit")));
    }
    if effective > ctx.date {
        return Err(Reject::new("future_dated", format!("{effective} is after the business date")));
    }
    let record = policy
        .lives
        .get(&spec.life)
        .filter(|l| l.in_force_on(effective) && !matches!(l.kind, LifeKind::Pool { .. }))
        .ok_or_else(|| Reject::new("unknown_member", format!("{} is not on the scheme on {effective}", spec.life)))?
        .clone();
    let dependant = matches!(record.kind, LifeKind::Dependant { .. });
    // What the life would hold: the scheme's automatic covers for a member,
    // and the covers chosen.
    let mut held: BTreeSet<&str> = policy
        .covers
        .values()
        .filter(|c| c.formula_driven && c.status == CoverStatus::InForce && !dependant)
        .map(|c| c.code.as_str())
        .collect();
    for chosen in &spec.covers {
        let def = product
            .cover(&chosen.code)
            .filter(|d| d.rating.basis == RateBasis::PerLife && d.rating.premium_type != ic_product::PremiumType::GroupUnitRate)
            .ok_or_else(|| Reject::new("not_on_menu", format!("{} is not a cover this scheme's members choose", chosen.code)))?;
        if !held.insert(chosen.code.as_str()) {
            return Err(Reject::new("invalid_choice", format!("{} is chosen twice", chosen.code)));
        }
        if !def.accepts_excess(chosen.excess_option.as_deref()) {
            return Err(Reject::new(
                "excess_option",
                format!("cover {}: choose one of the excess levels it offers, or none where it offers none", chosen.code),
            ));
        }
    }
    product
        .check_composition(&held)
        .map_err(|e| Reject::new("composition", format!("{}: {e}", spec.life)))?;

    let before = member_layers(policy, &spec.life);
    let chosen_layers = || before.iter().filter(|(_, l)| l.origin == LayerOrigin::MemberChoice);
    if chosen_layers().any(|(_, l)| l.start > effective || l.end.is_some_and(|e| e > effective)) {
        return Err(Reject::new(
            "later_change",
            format!("{}'s covers were set from a later date; a change is made from that date on", spec.life),
        ));
    }
    let mut after = before.clone();
    let mut kept: BTreeSet<String> = BTreeSet::new();
    let mut ended: Vec<(String, u32)> = Vec::new();
    for (code, l) in after.iter_mut().filter(|(_, l)| l.origin == LayerOrigin::MemberChoice && l.end.is_none()) {
        let same = spec
            .covers
            .iter()
            .any(|c| c.code == *code && c.excess_option == l.excess_option && c.member_pays == (l.member_pays == MemberPays::All));
        if same {
            kept.insert(code.clone());
        } else {
            l.end = Some(effective);
            ended.push((code.clone(), l.id));
        }
    }
    let mut added: Vec<(String, Layer)> = Vec::new();
    for chosen in spec.covers.iter().filter(|c| !kept.contains(&c.code)) {
        let id = policy.covers.get(&chosen.code).and_then(|c| c.layers.iter().map(|l| l.id).max()).unwrap_or(0) + 1;
        let layer = Layer {
            id,
            life: spec.life.clone(),
            start: effective,
            end: None,
            sum_insured_minor: 0,
            origin: LayerOrigin::MemberChoice,
            excess_option: chosen.excess_option.clone(),
            loadings: vec![],
            exclusions: vec![],
            loading_itemised: false,
            rated_from: None,
            member_pays: if chosen.member_pays { MemberPays::All } else { MemberPays::Nothing },
        };
        after.push((chosen.code.clone(), layer.clone()));
        added.push((chosen.code.clone(), layer));
    }
    if ended.is_empty() && added.is_empty() {
        // The set already stands: the same command sent again.
        return Ok(());
    }
    // A cover nobody on the scheme has chosen before starts with this choice.
    let mut new_covers = Vec::new();
    for (code, _) in added.iter().filter(|(code, _)| !policy.covers.contains_key(code)) {
        let Some(def) = product.cover(code) else { continue };
        let mut definitions = Vec::new();
        for id in &def.definitions {
            definitions.push(
                catalog
                    .bind_definition(id, effective)
                    .ok_or_else(|| Reject::new("unknown_definition", format!("definition {id} has no version in force on {effective}")))?,
            );
        }
        new_covers.push(crate::policy::Cover {
            code: def.code.clone(),
            cover_type: def.cover_type.clone(),
            status: CoverStatus::InForce,
            start: effective,
            definitions,
            layers: vec![],
            formula_driven: false,
        });
    }
    let d = deltas(catalog, &product, policy, &spec.life, effective, same_details(&record), &before, &after)?;
    ctx.work += d.work;

    for cover in new_covers {
        policy.covers.insert(cover.code.clone(), cover);
    }
    for (code, id) in ended {
        if let Some(l) = policy.covers.get_mut(&code).and_then(|c| c.layers.iter_mut().find(|l| l.id == id)) {
            l.end = Some(effective);
            ctx.emit(Event::LayerEnded {
                policy: policy.id.clone(),
                cover: code,
                layer: id,
                life: spec.life.clone(),
                effective,
                sum_insured_minor: 0,
            });
        }
    }
    for (code, layer) in added {
        if let Some(cover) = policy.covers.get_mut(&code) {
            cover.layers.push(layer.clone());
            ctx.emit(Event::LayerAdded {
                policy: policy.id.clone(),
                layer: layer_info(cover, &layer),
                origin: LayerOrigin::MemberChoice,
                continues: None,
            });
        }
    }
    ctx.emit(Event::MemberCoversSet {
        policy: policy.id.clone(),
        payer: policy.payer_of(&spec.life),
        life: spec.life.clone(),
        effective,
        covers: spec.covers,
        reference: spec.reference,
    });
    post_deltas(ledger, policy, ctx, &spec.life, effective, d)
}
