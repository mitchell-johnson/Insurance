//! Cover reduced or given up on a retail policy at the owner's request.
//!
//! The life's cover is cut to the amount asked for, newest layer first. A
//! layer cut in full ends today. A layer cut in part ends today and a smaller
//! layer takes its place on the same terms and the same rating date, so a
//! level premium keeps its entry age. Premium already billed for the days
//! from today is handed back as a credit adjustment: it reduces what is
//! owed, and any rest is a credit balance in policy suspense, refunded like
//! any other. Nothing already earned is touched.

use ic_ledger::Ledger;
use ic_product::schedule::period_start;

use crate::billing::{raise_receivable, Bill};
use crate::catalog::Catalog;
use crate::command::ReduceCover;
use crate::event::{Event, LayerInfo};
use crate::machine::{Ctx, Reject};
use crate::policy::{CoverStatus, Layer, Policy, PolicyStatus, ReceivableKind};
use crate::premium::{retail_premium_with, Layers};

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
        .filter(|c| c.status == CoverStatus::InForce && !c.formula_driven)
        .ok_or_else(unknown_cover)?;
    let def = product.cover(&spec.cover).ok_or_else(unknown_cover)?;
    let effective = ctx.date.max(policy.inception);
    let held: i64 = cover
        .layers
        .iter()
        .filter(|l| l.life == spec.life && l.in_force_on(effective))
        .map(|l| l.sum_insured_minor)
        .sum();
    if held == 0 {
        return Err(Reject::new("unknown_life", format!("life {} has no {} cover in force", spec.life, spec.cover)));
    }
    if spec.sum_insured_minor < 0 || spec.sum_insured_minor >= held {
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
    let mut cut = held - spec.sum_insured_minor;
    if let Some(cover) = policy.covers.get_mut(&spec.cover) {
        let cover_type = cover.cover_type.clone();
        let mut next_id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
        let mut order: Vec<usize> = (0..cover.layers.len())
            .filter(|&i| cover.layers[i].life == spec.life && cover.layers[i].in_force_on(effective))
            .collect();
        order.sort_by_key(|&i| std::cmp::Reverse((cover.layers[i].start, cover.layers[i].id)));
        for i in order {
            if cut == 0 {
                break;
            }
            let old = cover.layers[i].clone();
            cover.layers[i].end = Some(effective);
            if old.sum_insured_minor > cut {
                ctx.emit(Event::LayerReduced {
                    policy: id.clone(),
                    cover: spec.cover.clone(),
                    layer: old.id,
                    continued_as: next_id,
                    effective,
                    sum_insured_minor: old.sum_insured_minor,
                    kept_minor: old.sum_insured_minor - cut,
                });
            }
            ctx.emit(Event::LayerEnded {
                policy: id.clone(),
                cover: spec.cover.clone(),
                layer: old.id,
                life: old.life.clone(),
                effective,
                sum_insured_minor: old.sum_insured_minor,
            });
            let kept = (old.sum_insured_minor - cut).max(0);
            cut -= old.sum_insured_minor - kept;
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
                        cover: spec.cover.clone(),
                        cover_type: cover_type.clone(),
                        layer: next_id,
                        life: old.life,
                        sum_insured_minor: kept,
                        start: effective,
                    },
                    origin: old.origin,
                });
                next_id += 1;
            }
        }
    }

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
