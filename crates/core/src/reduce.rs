//! Cover reduced or given up on a retail policy at the owner's request.
//!
//! The life's cover is cut to the amount asked for, newest layer first. A
//! layer cut in full ends today. A layer cut in part ends today and a smaller
//! layer takes its place on the same terms and the same rating date, so a
//! level premium keeps its entry age. Premium already billed for the days
//! from today is handed back as a credit adjustment: it reduces what is
//! owed, and any rest is a credit balance in policy suspense, refunded like
//! any other. Nothing already earned is touched.

use chrono::NaiveDate;
use ic_ledger::Ledger;
use ic_product::schedule::period_start;

use crate::billing::{raise_receivable, Bill};
use crate::catalog::Catalog;
use crate::command::ReduceCover;
use crate::event::{Event, LayerInfo};
use crate::ids::LifeId;
use crate::machine::{Ctx, Reject};
use crate::policy::{ClaimCut, CoverStatus, Layer, Policy, PolicyStatus, ReceivableKind};
use crate::premium::{retail_premium_with, Layers};

/// Take `cut` off a life's cover from a date, newest layer first. Giving
/// the cover up ends every layer, whatever its amount. Returns each layer
/// ended and what was taken from it.
fn cut_layers(policy: &mut Policy, ctx: &mut Ctx<'_>, code: &str, life: &LifeId, effective: NaiveDate, mut cut: i64, give_up: bool) -> Vec<(u32, i64)> {
    let id = policy.id.clone();
    let mut taken = Vec::new();
    let Some(cover) = policy.covers.get_mut(code) else {
        return taken;
    };
    let cover_type = cover.cover_type.clone();
    let mut next_id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let mut order: Vec<usize> = (0..cover.layers.len())
        .filter(|&i| &cover.layers[i].life == life && cover.layers[i].in_force_on(effective))
        .collect();
    order.sort_by_key(|&i| std::cmp::Reverse((cover.layers[i].start, cover.layers[i].id)));
    for i in order {
        if cut == 0 && !give_up {
            break;
        }
        let old = cover.layers[i].clone();
        cover.layers[i].end = Some(effective);
        if old.sum_insured_minor > cut {
            ctx.emit(Event::LayerReduced {
                policy: id.clone(),
                cover: code.to_string(),
                layer: old.id,
                continued_as: next_id,
                effective,
                sum_insured_minor: old.sum_insured_minor,
                kept_minor: old.sum_insured_minor - cut,
            });
        }
        ctx.emit(Event::LayerEnded {
            policy: id.clone(),
            cover: code.to_string(),
            layer: old.id,
            life: old.life.clone(),
            effective,
            sum_insured_minor: old.sum_insured_minor,
        });
        let kept = (old.sum_insured_minor - cut).max(0);
        cut -= old.sum_insured_minor - kept;
        taken.push((old.id, old.sum_insured_minor - kept));
        if kept > 0 {
            cover.layers.push(Layer {
                id: next_id,
                start: effective,
                end: None,
                sum_insured_minor: kept,
                rated_from: Some(old.rating_start()),
                ..old.clone()
            });
            ctx.emit(Event::LayerAdded {
                policy: id.clone(),
                layer: LayerInfo {
                    cover: code.to_string(),
                    cover_type: cover_type.clone(),
                    layer: next_id,
                    life: old.life,
                    sum_insured_minor: kept,
                    start: effective,
                    option: old.excess_option.clone(),
                },
                origin: old.origin,
                continues: None,
            });
            next_id += 1;
        }
    }
    taken
}

/// The billed period today falls in, where premium for the days from today
/// has already been billed: its first day and the next period's.
fn billed_window(policy: &Policy, effective: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let next = period_start(policy.inception, policy.frequency, policy.periods_billed)?;
    let last = policy.periods_billed.checked_sub(1)?;
    (effective < next)
        .then(|| period_start(policy.inception, policy.frequency, last))
        .flatten()
        .map(|start| (start, next))
}

/// Bill or hand back the difference a change of cover today makes to the
/// premium already billed for the days from today. `before` is the policy
/// as it stood.
pub(crate) fn adjust(ledger: &mut Ledger, catalog: &Catalog, before: &Policy, policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId) -> Result<(), Reject> {
    let effective = ctx.date.max(policy.inception);
    let (Some((start, next)), Some(product)) = (billed_window(policy, effective), catalog.product(&policy.product_hash).cloned()) else {
        return Ok(());
    };
    let was = retail_premium_with(catalog, &product, before, start, &mut ctx.work, Layers::On(effective))?;
    let now = retail_premium_with(catalog, &product, policy, start, &mut ctx.work, Layers::On(effective))?;
    let days_left = (next - effective).num_days() as i128;
    let days = (next - start).num_days().max(1) as i128;
    let delta = Bill::from_modal(policy, &now).minus(&Bill::from_modal(policy, &was)).scaled(days_left, days);
    if let Some(receivable) = raise_receivable(
        ledger,
        policy,
        ctx,
        &delta,
        ReceivableKind::Adjustment,
        (effective, next),
        ctx.date,
        "premium_adjusted",
    )? {
        ctx.emit(Event::PremiumAdjusted {
            discounts: delta.discounts.clone(),
            policy: policy.id.clone(),
            receivable,
            life: life.clone(),
            effective,
            total_minor: delta.total(),
        });
    }
    Ok(())
}

/// Bill or hand back what a change that reaches back makes to premium
/// already billed: for every billed period the window `from..until`
/// touches, the difference between the period priced as the policy stood
/// (`before`) and as it stands now, for the window's days in that period.
/// Each adjustment is posted at its own value date.
pub(crate) fn adjust_billed(
    ledger: &mut Ledger,
    catalog: &Catalog,
    before: &Policy,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    life: &LifeId,
    (from, until): (NaiveDate, Option<NaiveDate>),
) -> Result<(), Reject> {
    let Some(product) = catalog.product(&policy.product_hash).cloned() else {
        return Ok(());
    };
    for n in 0..policy.periods_billed {
        let (Some(start), Some(end)) = (
            period_start(policy.inception, policy.frequency, n),
            period_start(policy.inception, policy.frequency, n + 1),
        ) else {
            break;
        };
        let (first, last) = (from.max(start), until.map_or(end, |u| u.min(end)));
        if first >= last {
            continue;
        }
        let was = retail_premium_with(catalog, &product, before, start, &mut ctx.work, Layers::On(first))?;
        let now = retail_premium_with(catalog, &product, policy, start, &mut ctx.work, Layers::On(first))?;
        let days = (end - start).num_days().max(1) as i128;
        let delta = Bill::from_modal(policy, &now)
            .minus(&Bill::from_modal(policy, &was))
            .scaled((last - first).num_days() as i128, days);
        let raised = raise_receivable(
            ledger,
            policy,
            ctx,
            &delta,
            ReceivableKind::Adjustment,
            (first, last),
            ctx.date,
            "premium_adjusted",
        )?;
        if let Some(receivable) = raised {
            ctx.emit(Event::PremiumAdjusted {
                discounts: delta.discounts.clone(),
                policy: policy.id.clone(),
                receivable,
                life: life.clone(),
                effective: first,
                total_minor: delta.total(),
            });
        }
    }
    Ok(())
}

/// A lump sum admitted for a life takes what it pays off the cover and off
/// every cover the product says it reduces, from today. A joint cover is
/// reduced for every life on it. `total` is everything the claim has
/// admitted; what the claim already took off is not taken again. Cover on a
/// policy no longer in force, and scheme cover that follows the member's
/// details, is left as it is.
pub(crate) fn for_claim(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    code: &str,
    life: &LifeId,
    total: i64,
) -> Result<(), Reject> {
    let Some(product) = catalog.product(&policy.product_hash).cloned() else {
        return Ok(());
    };
    let Some(def) = product.cover(code).filter(|d| matches!(d.benefit, ic_product::Benefit::LumpSum { .. })) else {
        return Ok(());
    };
    if policy.status != PolicyStatus::InForce {
        return Ok(());
    }
    let effective = ctx.date.max(policy.inception);
    let before = policy.clone();
    let mut cuts = Vec::new();
    for target in std::iter::once(code).chain(def.reduces.iter().map(String::as_str)) {
        let Some(cover) = policy
            .covers
            .get(target)
            .filter(|c| c.status == CoverStatus::InForce && !c.formula_driven && !c.chosen_by_members())
        else {
            continue;
        };
        let mut lives: Vec<LifeId> = vec![life.clone()];
        if product.cover(target).is_some_and(|d| d.joint_first_event) {
            lives = cover.layers.iter().filter(|l| l.in_force_on(effective)).map(|l| l.life.clone()).collect();
            lives.sort();
            lives.dedup();
        }
        for l in lives {
            let already: i64 = before
                .claims
                .get(claim)
                .map_or(0, |c| c.cuts.iter().filter(|x| x.cover == target && x.life == l).map(|x| x.amount_minor).sum());
            let held = policy.covers.get(target).map_or(0, |c| {
                c.layers
                    .iter()
                    .filter(|x| x.life == l && x.in_force_on(effective))
                    .map(|x| x.sum_insured_minor)
                    .sum()
            });
            let cut = (total - already).clamp(0, held);
            for (layer, amount_minor) in cut_layers(policy, ctx, target, &l, effective, cut, false) {
                cuts.push(ClaimCut {
                    cover: target.to_string(),
                    life: l.clone(),
                    layer,
                    amount_minor,
                    on: effective,
                });
            }
        }
    }
    if cuts.is_empty() {
        return Ok(());
    }
    if let Some(record) = policy.claims.get_mut(claim) {
        record.cuts.extend(cuts);
    }
    adjust(ledger, catalog, &before, policy, ctx, life)
}

/// An admission reversed: the cover the claim took off is written again from
/// today, on the terms and rating date of the layer it was taken from, and
/// premium is billed for the days from today.
pub(crate) fn restore_for_claim(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str) -> Result<(), Reject> {
    let cuts = policy.claims.get_mut(claim).map(|c| std::mem::take(&mut c.cuts)).unwrap_or_default();
    if cuts.is_empty() || policy.status != PolicyStatus::InForce {
        return Ok(());
    }
    let effective = ctx.date.max(policy.inception);
    let before = policy.clone();
    let id = policy.id.clone();
    for cut in &cuts {
        let Some(cover) = policy.covers.get_mut(&cut.cover) else {
            continue;
        };
        let Some(old) = cover.layers.iter().find(|l| l.id == cut.layer).cloned() else {
            continue;
        };
        let next_id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
        cover.layers.push(Layer {
            id: next_id,
            start: effective,
            end: None,
            sum_insured_minor: cut.amount_minor,
            rated_from: Some(old.rating_start()),
            ..old.clone()
        });
        ctx.emit(Event::LayerAdded {
            policy: id.clone(),
            layer: LayerInfo {
                cover: cut.cover.clone(),
                cover_type: cover.cover_type.clone(),
                layer: next_id,
                life: old.life,
                sum_insured_minor: cut.amount_minor,
                start: effective,
                option: old.excess_option.clone(),
            },
            origin: old.origin,
            continues: Some(old.id),
        });
    }
    adjust(ledger, catalog, &before, policy, ctx, &cuts[0].life)
}

pub(crate) fn reduce(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: ReduceCover) -> Result<(), Reject> {
    // Worked on a copy, so a refusal part way leaves the policy as it was.
    let mut draft = policy.clone();
    apply(ledger, catalog, &mut draft, ctx, spec)?;
    *policy = draft;
    Ok(())
}

fn apply(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: ReduceCover) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "cover can only be reduced on a policy in force"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let unknown_cover = || {
        Reject::new(
            "unknown_cover",
            format!("{} is not a cover in force on this policy that is priced per life", spec.cover),
        )
    };
    let cover = policy
        .covers
        .get(&spec.cover)
        .filter(|c| c.status == CoverStatus::InForce && !c.formula_driven && !c.chosen_by_members())
        .ok_or_else(unknown_cover)?;
    let def = product.cover(&spec.cover).ok_or_else(unknown_cover)?;
    let effective = ctx.date.max(policy.inception);
    let held: i64 = cover
        .layers
        .iter()
        .filter(|l| l.life == spec.life && l.in_force_on(effective))
        .map(|l| l.sum_insured_minor)
        .sum();
    // A cover priced per life has no sum insured, so it is the layers that
    // say whether the life holds it.
    let holds = cover.layers.iter().any(|l| l.life == spec.life && l.in_force_on(effective));
    if !holds {
        return Err(Reject::new("unknown_life", format!("life {} has no {} cover in force", spec.life, spec.cover)));
    }
    if spec.sum_insured_minor < 0 || (spec.sum_insured_minor > 0 && spec.sum_insured_minor >= held) {
        return Err(Reject::new(
            "not_a_reduction",
            "the amount to keep is zero or more and below the cover in force",
        ));
    }
    if spec.sum_insured_minor > 0 {
        ic_product::rating::check_sum_insured(def, spec.sum_insured_minor).map_err(|e| Reject::new("sum_insured", format!("cover {}: {e}", spec.cover)))?;
    } else {
        let other = policy.covers.iter().any(|(code, c)| {
            c.status == CoverStatus::InForce
                && (c.formula_driven
                    || c.layers
                        .iter()
                        .any(|l| l.in_force_on(effective) && (code != &spec.cover || l.life != spec.life)))
        });
        if !other {
            return Err(Reject::new("last_cover", "this is the only cover on the policy; cancel the policy instead"));
        }
        // What the life would be left holding must still be a set the
        // product allows: a cover others depend on is not dropped first.
        let left: std::collections::BTreeSet<&str> = policy
            .covers
            .iter()
            .filter(|(code, c)| {
                *code != &spec.cover && c.status == CoverStatus::InForce && c.layers.iter().any(|l| l.in_force_on(effective) && l.life == spec.life)
            })
            .map(|(code, _)| code.as_str())
            .collect();
        product
            .check_composition(&left)
            .map_err(|e| Reject::new("composition", format!("without {}: {e}", spec.cover)))?;
    }

    // As for an increase: the days between the last bill and today cannot be
    // priced until billing has caught up.
    let date = || Reject::new("date", "billing period out of range");
    let next = period_start(policy.inception, policy.frequency, policy.periods_billed).ok_or_else(date)?;
    if effective > next {
        return Err(Reject::new("billing_behind", "billing has not caught up to today; retry after the end of day"));
    }
    let billed_from = match policy.periods_billed.checked_sub(1) {
        Some(n) if effective < next => Some(period_start(policy.inception, policy.frequency, n).ok_or_else(date)?),
        _ => None,
    };
    let before = match billed_from {
        Some(start) => Some(retail_premium_with(catalog, &product, policy, start, &mut ctx.work, Layers::On(effective))?),
        None => None,
    };

    let id = policy.id.clone();
    cut_layers(
        policy,
        ctx,
        &spec.cover,
        &spec.life,
        effective,
        held - spec.sum_insured_minor,
        spec.sum_insured_minor == 0,
    );

    if let (Some(start), Some(before)) = (billed_from, before) {
        let after = retail_premium_with(catalog, &product, policy, start, &mut ctx.work, Layers::On(effective))?;
        let days_left = (next - effective).num_days() as i128;
        let days = (next - start).num_days().max(1) as i128;
        let delta = Bill::from_modal(policy, &after)
            .minus(&Bill::from_modal(policy, &before))
            .scaled(days_left, days);
        if let Some(receivable) = raise_receivable(
            ledger,
            policy,
            ctx,
            &delta,
            ReceivableKind::Adjustment,
            (effective, next),
            ctx.date,
            "premium_adjusted",
        )? {
            ctx.emit(Event::PremiumAdjusted {
                discounts: delta.discounts.clone(),
                policy: id,
                receivable,
                life: spec.life,
                effective,
                total_minor: delta.total(),
            });
        }
    }
    Ok(())
}
