//! Group scheme administration in the core: membership as effective-dated
//! events, scheme billing in chunks, and retrospective premium adjustment.

use std::ops::Bound;

use chrono::{Days, NaiveDate};
use ic_ledger::Ledger;
use ic_product::schedule::period_start;
use ic_product::ProductDefinition;

use serde::{Deserialize, Serialize};

use crate::billing::{raise_receivable, raise_receivable_for, Bill};
use crate::bitemporal::Bitemporal;
use crate::catalog::Catalog;
use crate::command::{ExcessLayer, MemberChange};
use crate::event::{Event, LayerInfo, LifeInfo, MemberCover};
use crate::lifecycle::{member_covers, BACKDATING_LIMIT_DAYS};
use crate::machine::{CoreState, Ctx, Reject, APPLY_BUDGET, MAX_MEMBERSHIP_CHUNK, SCHEME_BILLING_CHUNK};
use crate::policy::{CoverStatus, Layer, LayerOrigin, LifeFactors, LifeKind, LifeRecord, MemberPays, PendingInvoice, Policy, PolicyStatus, ReceivableKind};
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
        members += declared.map_or(1, |d| d.headcount);
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
        return Ok((vec![], effective));
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
    Ok((ended.into_iter().map(|(cover, id, _)| (cover, id)).collect(), effective))
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

/// A member's underwritten layers on formula-driven cover, with the cover
/// each sits on, copied out so the policy can change underneath.
fn member_layers(policy: &Policy, life: &LifeId) -> Vec<(String, Layer)> {
    policy
        .covers
        .values()
        .filter(|c| c.formula_driven)
        .flat_map(|c| c.layers.iter().map(move |l| (c.code.clone(), l.clone())))
        .filter(|(_, l)| l.life == *life && l.origin == LayerOrigin::UnderwrittenExcess)
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
    restored: Vec<LayerInfo>,
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
    let limit = date
        .checked_sub_days(Days::new(BACKDATING_LIMIT_DAYS))
        .ok_or_else(|| Reject::new("date", "date out of range"))?;

    let mut staged: Vec<Staged> = Vec::new();
    let mut scratch = policy.clone();
    let mut estimated_work = 0u32;
    for change in changes {
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
                // Underwritten cover is the member's while they are a member.
                for cover in scratch.covers.values_mut().filter(|c| c.formula_driven) {
                    for layer in cover
                        .layers
                        .iter_mut()
                        .filter(|l| l.life == *life && l.origin == LayerOrigin::UnderwrittenExcess)
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
                for cover in scratch.covers.values_mut().filter(|c| c.formula_driven) {
                    let (code, cover_type) = (cover.code.clone(), cover.cover_type.clone());
                    for layer in cover
                        .layers
                        .iter_mut()
                        .filter(|l| l.life == *life && l.origin == LayerOrigin::UnderwrittenExcess)
                    {
                        if layer.end == Some(effective) && layer.start < effective {
                            layer.end = None;
                            restored.push(LayerInfo {
                                cover: code.clone(),
                                cover_type: cover_type.clone(),
                                layer: layer.id,
                                life: life.clone(),
                                sum_insured_minor: layer.sum_insured_minor,
                                start: layer.start,
                            });
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
        for layer in s.restored {
            ctx.emit(Event::LayerAdded {
                policy: policy.id.clone(),
                layer,
                origin: LayerOrigin::UnderwrittenExcess,
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
    let before = member_layers(policy, &spec.life);
    let id = policy.covers[&spec.cover].layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let layer = Layer {
        id,
        life: spec.life.clone(),
        start: spec.start,
        end: None,
        sum_insured_minor: spec.sum_insured_minor,
        origin: LayerOrigin::UnderwrittenExcess,
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
            continues: None,
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
