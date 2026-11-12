//! Cover added to a retail policy in force on underwriting's decision.
//!
//! The increase is its own layer with its own terms, in force from the
//! business date. Premium is billed in advance, so the days left of a period
//! already billed are billed now as an adjustment; later periods price the
//! layer like any other. Nothing already billed or earned is touched.
//!
//! A change of terms on cover already held ends each layer and writes it
//! again on the new terms, keeping its sum insured, its origin and the date
//! a level premium is priced from.

use chrono::NaiveDate;
use ic_ledger::Ledger;
use ic_product::rating::ModalPremium;
use ic_product::schedule::period_start;

use crate::billing::{raise_receivable, Bill};
use crate::cash;
use crate::catalog::Catalog;
use crate::command::{AlteredTerms, UnderwrittenLayer};
use crate::event::{Event, LayerInfo};
use crate::machine::{Ctx, Reject};
use crate::policy::{CoverStatus, Layer, LayerOrigin, MemberPays, Policy, PolicyStatus, ReceivableKind};
use crate::premium::{retail_period_premium, retail_premium_with, Layers};
use crate::LifeId;

pub(crate) fn add_layer(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: UnderwrittenLayer) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "cover can only be added to a policy in force"));
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
    if !policy.lives.get(&spec.life).is_some_and(|l| l.in_force_on(effective)) {
        return Err(Reject::new("unknown_life", format!("life {} is not on the policy", spec.life)));
    }
    if spec.sum_insured_minor <= 0 {
        return Err(Reject::new("sum_insured", "an added layer needs a positive sum insured"));
    }
    if spec.loadings.iter().any(|l| l.get().is_negative()) {
        return Err(Reject::new("invalid_loading", "a loading cannot be negative"));
    }
    let held: i64 = cover
        .layers
        .iter()
        .filter(|l| l.life == spec.life && l.in_force_on(effective))
        .map(|l| l.sum_insured_minor)
        .sum();
    let total = held
        .checked_add(spec.sum_insured_minor)
        .ok_or_else(|| Reject::new("sum_insured", "the sum insured is out of range"))?;
    ic_product::rating::check_sum_insured(def, total).map_err(|e| Reject::new("sum_insured", format!("cover {}: {e}", spec.cover)))?;

    let billed = billed_period(policy, effective)?;
    let without = match billed {
        Some((start, _)) => Some(retail_period_premium(catalog, &product, policy, start, &mut ctx.work)?),
        None => None,
    };

    let cover_type = cover.cover_type.clone();
    let id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    if let Some(cover) = policy.covers.get_mut(&spec.cover) {
        cover.layers.push(Layer {
            id,
            life: spec.life.clone(),
            start: effective,
            end: None,
            sum_insured_minor: spec.sum_insured_minor,
            origin: LayerOrigin::Increase,
            loadings: spec.loadings,
            exclusions: spec.exclusions,
            loading_itemised: false,
            rated_from: None,
            member_pays: MemberPays::Nothing,
        });
    }
    ctx.emit(Event::LayerAdded {
        policy: policy.id.clone(),
        layer: LayerInfo {
            cover: spec.cover.clone(),
            cover_type,
            layer: id,
            life: spec.life.clone(),
            sum_insured_minor: spec.sum_insured_minor,
            start: effective,
        },
        origin: LayerOrigin::Increase,
    });

    if let (Some(period), Some(without)) = (billed, without) {
        let with = retail_premium_with(catalog, &product, policy, period.0, &mut ctx.work, Layers::With(&spec.cover, id))?;
        adjust(ledger, policy, ctx, &spec.life, effective, period, &with, &without)?;
    }
    Ok(())
}

/// The billed period `effective` falls in, if it has been billed: its first
/// day and the first day not yet billed. Billing that has fallen behind
/// catches up at the end of the day; until then the days between could not
/// be priced for a change made today.
fn billed_period(policy: &Policy, effective: NaiveDate) -> Result<Option<(NaiveDate, NaiveDate)>, Reject> {
    let date = || Reject::new("date", "billing period out of range");
    let next = period_start(policy.inception, policy.frequency, policy.periods_billed).ok_or_else(date)?;
    if effective > next {
        return Err(Reject::new("billing_behind", "billing has not caught up to today; retry after the end of day"));
    }
    match policy.periods_billed.checked_sub(1) {
        Some(n) if effective < next => Ok(Some((period_start(policy.inception, policy.frequency, n).ok_or_else(date)?, next))),
        _ => Ok(None),
    }
}

/// Bill or credit the days left of a billed period for a change of price.
#[allow(clippy::too_many_arguments)]
fn adjust(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    life: &LifeId,
    effective: NaiveDate,
    (start, next): (NaiveDate, NaiveDate),
    with: &ModalPremium,
    without: &ModalPremium,
) -> Result<(), Reject> {
    let days_left = (next - effective).num_days() as i128;
    let days = (next - start).num_days().max(1) as i128;
    let delta = Bill::from_modal(policy, with).minus(&Bill::from_modal(policy, without)).scaled(days_left, days);
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
            policy: policy.id.clone(),
            receivable,
            life: life.clone(),
            effective,
            total_minor: delta.total(),
        });
    }
    // Money paid ahead is consumed by new billing.
    cash::apply_suspense(ledger, policy, ctx)
}

/// New terms on a life's cover from the business date. The sum insured, the
/// origin and the date a level premium is priced from all carry over: only
/// the terms change, and with them the price from today.
pub(crate) fn alter_terms(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: AlteredTerms) -> Result<(), Reject> {
    // Worked on a copy, so a refusal part way leaves the policy as it was.
    let mut draft = policy.clone();
    alter(ledger, catalog, &mut draft, ctx, spec)?;
    *policy = draft;
    Ok(())
}

fn alter(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: AlteredTerms) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "terms can only change on a policy in force"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let effective = ctx.date.max(policy.inception);
    let cover = policy
        .covers
        .get(&spec.cover)
        .filter(|c| c.status == CoverStatus::InForce && !c.formula_driven)
        .ok_or_else(|| {
            Reject::new(
                "unknown_cover",
                format!("{} is not a cover in force on this policy that is priced per life", spec.cover),
            )
        })?;
    if spec.loadings.iter().any(|l| l.get().is_negative()) {
        return Err(Reject::new("invalid_loading", "a loading cannot be negative"));
    }
    let altered: Vec<Layer> = cover
        .layers
        .iter()
        .filter(|l| l.life == spec.life && l.in_force_on(effective))
        .filter(|l| l.loadings != spec.loadings || l.exclusions != spec.exclusions)
        .cloned()
        .collect();
    if altered.is_empty() {
        return Err(Reject::new(
            "no_change",
            format!("life {} has no {} cover in force on other terms", spec.life, spec.cover),
        ));
    }
    let billed = billed_period(policy, effective)?;
    let without = match billed {
        Some((start, _)) => Some(retail_premium_with(catalog, &product, policy, start, &mut ctx.work, Layers::On(effective))?),
        None => None,
    };

    let cover_type = cover.cover_type.clone();
    let first = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    for (id, old) in (first..).zip(altered) {
        if let Some(cover) = policy.covers.get_mut(&spec.cover) {
            if let Some(l) = cover.layers.iter_mut().find(|l| l.id == old.id) {
                l.end = Some(effective);
            }
            cover.layers.push(Layer {
                id,
                life: old.life.clone(),
                start: effective,
                end: None,
                sum_insured_minor: old.sum_insured_minor,
                origin: old.origin,
                loadings: spec.loadings.clone(),
                exclusions: spec.exclusions.clone(),
                loading_itemised: old.loading_itemised,
                rated_from: Some(old.rating_start()),
                member_pays: MemberPays::Nothing,
            });
        }
        ctx.emit(Event::LayerEnded {
            policy: policy.id.clone(),
            cover: spec.cover.clone(),
            layer: old.id,
            life: old.life.clone(),
            effective,
            sum_insured_minor: old.sum_insured_minor,
        });
        ctx.emit(Event::LayerAdded {
            policy: policy.id.clone(),
            layer: LayerInfo {
                cover: spec.cover.clone(),
                cover_type: cover_type.clone(),
                layer: id,
                life: old.life,
                sum_insured_minor: old.sum_insured_minor,
                start: effective,
            },
            origin: old.origin,
        });
    }
    if let (Some(period), Some(without)) = (billed, without) {
        let with = retail_premium_with(catalog, &product, policy, period.0, &mut ctx.work, Layers::On(effective))?;
        adjust(ledger, policy, ctx, &spec.life, effective, period, &with, &without)?;
    }
    Ok(())
}
