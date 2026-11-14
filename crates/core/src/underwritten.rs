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
use crate::command::{AddedCover, AlteredTerms, ChangedExcess, OptionExercise, ReinstatementTerms, UnderwrittenLayer};
use crate::event::{Event, LayerInfo};
use crate::machine::{Ctx, Reject};
use crate::policy::{CoverStatus, Layer, LayerOrigin, LayerTerms, MemberPays, OptionUse, Policy, PolicyStatus, ReceivableKind, ReinstatementApproval};
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
        .filter(|c| c.status == CoverStatus::InForce && !c.formula_driven && !c.chosen_by_members())
        .ok_or_else(unknown_cover)?;
    let def = product.cover(&spec.cover).ok_or_else(unknown_cover)?;
    if spec.start.is_some_and(|s| s < policy.inception || s > ctx.date) {
        return Err(Reject::new("start", "a layer starts no earlier than the policy and no later than today"));
    }
    let effective = spec.start.unwrap_or(ctx.date).max(policy.inception);
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

    // A layer dated back reaches every period billed since its start.
    let before = spec.start.map(|_| policy.clone());
    let billed = match before {
        Some(_) => None,
        None => billed_period(policy, effective)?,
    };
    let without = match billed {
        Some((start, _)) => Some(retail_period_premium(catalog, &product, policy, start, &mut ctx.work)?),
        None => None,
    };

    let cover_type = cover.cover_type.clone();
    let id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    // More of a cover the life holds comes at the excess level it holds it at.
    let excess_option = cover
        .layers
        .iter()
        .find(|l| l.life == spec.life && l.in_force_on(effective))
        .and_then(|l| l.excess_option.clone());
    if !def.accepts_excess(excess_option.as_deref()) {
        return Err(Reject::new(
            "excess_option",
            format!("cover {} needs a choice from its options; add it as a cover", spec.cover),
        ));
    }
    let option = excess_option.clone();
    if let Some(cover) = policy.covers.get_mut(&spec.cover) {
        cover.layers.push(Layer {
            paid_up: false,
            excess_option,
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
            option,
        },
        origin: LayerOrigin::Increase,
        continues: None,
    });

    if let (Some(period), Some(without)) = (billed, without) {
        let with = retail_premium_with(catalog, &product, policy, period.0, &mut ctx.work, Layers::With(&spec.cover, id))?;
        adjust(ledger, policy, ctx, &spec.life, effective, period, &with, &without)?;
    }
    if let Some(before) = before {
        crate::reduce::adjust_billed(ledger, catalog, &before, policy, ctx, &spec.life, (effective, None))?;
        cash::apply_suspense(ledger, policy, ctx)?;
    }
    Ok(())
}

/// A life uses its cover's insurability option: a new layer from today on
/// the terms of the cover the life started with, with no underwriting,
/// inside the product's limits for the option.
pub(crate) fn exercise_option(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: OptionExercise) -> Result<(), Reject> {
    let plain = crate::interest::plain;
    if !plain(&spec.reference) || spec.evidence.is_empty() || spec.evidence.len() > 200 || spec.sum_insured_minor <= 0 {
        return Err(Reject::new(
            "invalid_option",
            "an option is used under a plain reference, with evidence of the event and an amount above zero",
        ));
    }
    if let Some(used) = policy.option_uses.get(&spec.reference) {
        let same = used.cover == spec.cover
            && used.life == spec.life
            && used.event == spec.event
            && used.event_date == spec.event_date
            && used.sum_insured_minor == spec.sum_insured_minor;
        return if same {
            Ok(())
        } else {
            Err(Reject::new("duplicate_option", format!("{} was used for something else", spec.reference)))
        };
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?;
    let Some(option) = product.cover(&spec.cover).and_then(|c| c.insurability_option.clone()) else {
        return Err(Reject::new("no_option", format!("cover {} has no insurability option", spec.cover)));
    };
    if !option.events.contains(&spec.event) {
        return Err(Reject::new(
            "event_not_covered",
            format!("{} is not an event the option is given for", spec.event),
        ));
    }
    if spec.event_date > ctx.date || (ctx.date - spec.event_date).num_days() > i64::from(option.within_days) {
        return Err(Reject::new(
            "option_window",
            format!("the option is used within {} days of the event", option.within_days),
        ));
    }
    let Some(born) = policy
        .lives
        .get(&spec.life)
        .filter(|l| l.in_force_on(ctx.date))
        .and_then(|l| l.factors.as_of(ctx.date))
        .and_then(|f| f.date_of_birth)
    else {
        return Err(Reject::new("unknown_life", format!("life {} is not on the policy", spec.life)));
    };
    if ic_product::schedule::age_on(born, ctx.date) >= option.to_age {
        return Err(Reject::new("option_age", format!("the option ends at age {}", option.to_age)));
    }
    let theirs = |u: &&OptionUse| u.cover == spec.cover && u.life == spec.life;
    if policy
        .option_uses
        .values()
        .filter(theirs)
        .any(|u| u.event == spec.event && u.event_date == spec.event_date)
    {
        return Err(Reject::new("option_used", "the option has already been used for this event"));
    }
    let uses = policy.option_uses.values().filter(theirs).count() as u32;
    if uses >= option.max_uses {
        return Err(Reject::new("options_exhausted", format!("the option can be used {} times", option.max_uses)));
    }
    // The terms and the measure are the cover the life started with.
    let original: Vec<&Layer> = policy
        .covers
        .get(&spec.cover)
        .map(|c| c.layers.iter().filter(|l| l.life == spec.life && l.origin == LayerOrigin::Original).collect())
        .unwrap_or_default();
    let Some(first) = original.iter().find(|l| l.in_force_on(ctx.date)) else {
        return Err(Reject::new(
            "unknown_cover",
            format!("life {} does not hold cover {} from the start", spec.life, spec.cover),
        ));
    };
    let began: i128 = original.iter().map(|l| i128::from(l.sum_insured_minor)).sum();
    let scale = ic_money::dec_i128(10i128.pow(policy.currency.minor_digits()));
    let share = ic_money::dec_i128(began) * option.max_percent_of_original.get() / ic_money::Dec::from(100i64) / scale;
    let by_share = ic_money::round_to_money(share, policy.currency, ic_money::RoundingMode::Down)?.minor;
    let most = i128::from(by_share.min(option.max_increase_minor));
    if i128::from(spec.sum_insured_minor) > most {
        return Err(Reject::new(
            "option_limit",
            format!("the most one use of the option adds is {most} in minor units"),
        ));
    }
    let layer = UnderwrittenLayer {
        policy: spec.policy.clone(),
        cover: spec.cover.clone(),
        life: spec.life.clone(),
        sum_insured_minor: spec.sum_insured_minor,
        loadings: first.loadings.clone(),
        exclusions: first.exclusions.clone(),
        reference: spec.reference.clone(),
        start: None,
    };
    add_layer(ledger, catalog, policy, ctx, layer)?;
    let id = policy.covers.get(&spec.cover).and_then(|c| c.layers.iter().map(|l| l.id).max()).unwrap_or(0);
    ctx.emit(Event::InsurabilityOptionExercised {
        policy: policy.id.clone(),
        cover: spec.cover.clone(),
        life: spec.life.clone(),
        event: spec.event.clone(),
        event_date: spec.event_date,
        layer: id,
        sum_insured_minor: spec.sum_insured_minor,
        uses: uses + 1,
        reference: spec.reference.clone(),
    });
    policy.option_uses.insert(
        spec.reference,
        OptionUse {
            cover: spec.cover,
            life: spec.life,
            event: spec.event,
            event_date: spec.event_date,
            sum_insured_minor: spec.sum_insured_minor,
            used_on: ctx.date,
            layer: id,
        },
    );
    Ok(())
}

/// Give a life a cover it does not hold, from today. The cover joins the
/// policy if nobody on it had it. The product's combination rules are
/// checked on what the life would hold, and the days left of a period
/// already billed are billed for the new cover at once, at the price the
/// whole policy has with it less the price it had without, so a discount
/// the new cover earns or changes is in the difference.
pub(crate) fn add_cover(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: AddedCover) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "cover can only be added to a policy in force"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let unknown_cover = || Reject::new("unknown_cover", format!("the product does not offer {} as a cover priced per life", spec.cover));
    let def = product
        .cover(&spec.cover)
        .filter(|d| d.rating.premium_type != ic_product::PremiumType::GroupUnitRate)
        .ok_or_else(unknown_cover)?;
    let effective = ctx.date.max(policy.inception);
    if !policy.lives.get(&spec.life).is_some_and(|l| l.in_force_on(effective)) {
        return Err(Reject::new("unknown_life", format!("life {} is not on the policy", spec.life)));
    }
    if spec.sum_insured_minor < 0 || spec.loadings.iter().any(|l| l.get().is_negative()) {
        return Err(Reject::new("invalid_cover", "a sum insured and a loading cannot be negative"));
    }
    ic_product::rating::check_sum_insured(def, spec.sum_insured_minor).map_err(|e| Reject::new("sum_insured", format!("cover {}: {e}", spec.cover)))?;
    if let Some(life) = policy.lives.get(&spec.life) {
        crate::lifecycle::entry_age(def, life, effective)?;
    }
    if !def.accepts_excess(spec.excess_option.as_deref()) {
        return Err(Reject::new(
            "excess_option",
            format!(
                "cover {}: choose from the options it offers (an excess level, or one choice on each of its axes), or none where it offers none",
                spec.cover
            ),
        ));
    }
    let holds = |policy: &Policy, code: &str| {
        policy
            .covers
            .get(code)
            .is_some_and(|c| c.status == CoverStatus::InForce && c.layers.iter().any(|l| l.life == spec.life && l.in_force_on(effective)))
    };
    if holds(policy, &spec.cover) {
        return Err(Reject::new("cover_held", format!("life {} already holds {}", spec.life, spec.cover)));
    }
    if policy.covers.get(&spec.cover).is_some_and(|c| c.status != CoverStatus::InForce) {
        return Err(Reject::new("cover_ended", format!("{} has ended on this policy", spec.cover)));
    }
    let mut held: std::collections::BTreeSet<&str> = policy.covers.keys().map(String::as_str).filter(|code| holds(policy, code)).collect();
    held.insert(&spec.cover);
    product
        .check_composition(&held)
        .map_err(|e| Reject::new("composition", format!("life {}: {e}", spec.life)))?;
    let mut bindings = Vec::new();
    if !policy.covers.contains_key(&spec.cover) {
        for id in &def.definitions {
            let bound = catalog
                .bind_definition(id, effective)
                .ok_or_else(|| Reject::new("unknown_definition", format!("definition {id} has no version in force on {effective}")))?;
            bindings.push(bound);
        }
    }

    let billed = billed_period(policy, effective)?;
    let without = match billed {
        // Every layer in force today counts, so cover added earlier in the
        // same period is in both prices and only this cover is in the difference.
        Some((start, _)) => Some(retail_premium_with(catalog, &product, policy, start, &mut ctx.work, Layers::On(effective))?),
        None => None,
    };

    let cover = policy.covers.entry(spec.cover.clone()).or_insert_with(|| crate::policy::Cover {
        code: def.code.clone(),
        cover_type: def.cover_type.clone(),
        status: CoverStatus::InForce,
        start: effective,
        definitions: bindings,
        layers: vec![],
        formula_driven: false,
        ended_on: None,
    });
    let cover_type = cover.cover_type.clone();
    let id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let option = spec.excess_option.clone();
    cover.layers.push(Layer {
        paid_up: false,
        id,
        life: spec.life.clone(),
        start: effective,
        end: None,
        sum_insured_minor: spec.sum_insured_minor,
        origin: LayerOrigin::Increase,
        excess_option: spec.excess_option,
        loadings: spec.loadings,
        exclusions: spec.exclusions,
        loading_itemised: false,
        rated_from: None,
        member_pays: MemberPays::Nothing,
    });
    ctx.emit(Event::LayerAdded {
        policy: policy.id.clone(),
        layer: LayerInfo {
            cover: spec.cover.clone(),
            cover_type,
            layer: id,
            life: spec.life.clone(),
            sum_insured_minor: spec.sum_insured_minor,
            start: effective,
            option,
        },
        origin: LayerOrigin::Increase,
        continues: None,
    });
    if let (Some(period), Some(without)) = (billed, without) {
        let with = retail_premium_with(catalog, &product, policy, period.0, &mut ctx.work, Layers::On(effective))?;
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
            discounts: delta.discounts.clone(),
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
    let change = Change::Terms {
        loadings: spec.loadings,
        exclusions: spec.exclusions,
        occupation_class: spec.occupation_class,
    };
    alter(ledger, catalog, &mut draft, ctx, &spec.cover, &spec.life, change)?;
    *policy = draft;
    Ok(())
}

/// Another excess level on a life's cover from the business date: the
/// cover carries on as new layers at the new level, priced from today.
pub(crate) fn change_excess(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: ChangedExcess) -> Result<(), Reject> {
    let mut draft = policy.clone();
    alter(ledger, catalog, &mut draft, ctx, &spec.cover, &spec.life, Change::Excess(spec.excess_option))?;
    *policy = draft;
    Ok(())
}

/// What a change of terms changes on the layers it replaces.
enum Change {
    Terms {
        loadings: Vec<ic_money::DecStr>,
        exclusions: Vec<String>,
        occupation_class: Option<String>,
    },
    Excess(String),
}

struct Target<'a> {
    cover: &'a str,
    life: &'a LifeId,
}

fn alter(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, cover: &str, life: &LifeId, change: Change) -> Result<(), Reject> {
    let spec = Target { cover, life };
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
        .get(spec.cover)
        .filter(|c| c.status == CoverStatus::InForce && !c.formula_driven && !c.chosen_by_members())
        .ok_or_else(|| {
            Reject::new(
                "unknown_cover",
                format!("{} is not a cover in force on this policy that is priced per life", spec.cover),
            )
        })?;
    match &change {
        Change::Terms { loadings, .. } => {
            if loadings.iter().any(|l| l.get().is_negative()) {
                return Err(Reject::new("invalid_loading", "a loading cannot be negative"));
            }
        }
        Change::Excess(option) => {
            if product.cover(spec.cover).is_none_or(|def| def.option(option).is_err()) {
                return Err(Reject::new(
                    "excess_option",
                    format!("cover {} does not offer excess level {option}", spec.cover),
                ));
            }
        }
    }
    // A new occupation class is a change even where the terms stay as they were.
    let held = policy.lives.get(spec.life).and_then(|l| l.factors.as_of(effective));
    let reclassed = match &change {
        Change::Terms {
            occupation_class: Some(class), ..
        } if held.is_some_and(|f| f.occupation_class.as_ref() != Some(class)) => held.map(|f| crate::policy::LifeFactors {
            occupation_class: Some(class.clone()),
            ..f.clone()
        }),
        _ => None,
    };
    let altered: Vec<Layer> = cover
        .layers
        .iter()
        .filter(|l| &l.life == spec.life && l.in_force_on(effective))
        .filter(|l| match &change {
            Change::Terms { loadings, exclusions, .. } => reclassed.is_some() || &l.loadings != loadings || &l.exclusions != exclusions,
            Change::Excess(option) => l.excess_option.as_ref() != Some(option),
        })
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
    if let (Some(factors), Some(life)) = (reclassed, policy.lives.get_mut(spec.life)) {
        life.factors.record(effective, ctx.date, ctx.index, factors);
    }
    for (id, old) in (first..).zip(altered) {
        if let Some(cover) = policy.covers.get_mut(spec.cover) {
            if let Some(l) = cover.layers.iter_mut().find(|l| l.id == old.id) {
                l.end = Some(effective);
            }
            cover.layers.push(Layer {
                paid_up: false,
                id,
                life: old.life.clone(),
                start: effective,
                end: None,
                sum_insured_minor: old.sum_insured_minor,
                origin: old.origin,
                loadings: match &change {
                    Change::Terms { loadings, .. } => loadings.clone(),
                    Change::Excess(_) => old.loadings.clone(),
                },
                exclusions: match &change {
                    Change::Terms { exclusions, .. } => exclusions.clone(),
                    Change::Excess(_) => old.exclusions.clone(),
                },
                excess_option: match &change {
                    Change::Terms { .. } => old.excess_option.clone(),
                    Change::Excess(option) => Some(option.clone()),
                },
                loading_itemised: old.loading_itemised,
                rated_from: Some(old.rating_start()),
                member_pays: MemberPays::Nothing,
            });
        }
        ctx.emit(Event::LayerEnded {
            policy: policy.id.clone(),
            cover: spec.cover.to_string(),
            layer: old.id,
            life: old.life.clone(),
            effective,
            sum_insured_minor: old.sum_insured_minor,
        });
        let option = match &change {
            Change::Terms { .. } => old.excess_option.clone(),
            Change::Excess(option) => Some(option.clone()),
        };
        ctx.emit(Event::LayerAdded {
            policy: policy.id.clone(),
            layer: LayerInfo {
                cover: spec.cover.to_string(),
                cover_type: cover_type.clone(),
                layer: id,
                life: old.life,
                sum_insured_minor: old.sum_insured_minor,
                start: effective,
                option,
            },
            origin: old.origin,
            continues: Some(old.id),
        });
    }
    if let (Some(period), Some(without)) = (billed, without) {
        let with = retail_premium_with(catalog, &product, policy, period.0, &mut ctx.work, Layers::On(effective))?;
        adjust(ledger, policy, ctx, spec.life, effective, period, &with, &without)?;
    }
    Ok(())
}

/// Underwriting's decision on a lapsed policy, kept with the lapse until the
/// policy is reinstated or the window closes. A later decision replaces an
/// earlier one.
pub(crate) fn approve_reinstatement(catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: ReinstatementTerms) -> Result<(), Reject> {
    let today = ctx.date;
    let Some(lapse) = policy.lapse.as_ref().filter(|_| policy.status == PolicyStatus::Lapsed) else {
        return Err(Reject::new("not_lapsed", "only a lapsed policy can be reinstated"));
    };
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?;
    let window = product.arrears().map_or(0, |r| r.reinstatement_days) as i64;
    if (today - lapse.lapsed_on).num_days() > window {
        return Err(Reject::new("window_closed", "the reinstatement window has closed; this needs a new policy"));
    }
    if spec.reference.is_empty() {
        return Err(Reject::new("reference", "the decision carries underwriting's reference"));
    }
    if spec.valid_until < today {
        return Err(Reject::new("decision_expired", "the decision can no longer be relied on"));
    }
    for t in &spec.terms {
        let cover = policy
            .covers
            .get(&t.cover)
            .filter(|c| c.status == CoverStatus::InForce && !c.formula_driven && !c.chosen_by_members())
            .ok_or_else(|| Reject::new("unknown_cover", format!("{} is not a cover on this policy that is priced per life", t.cover)))?;
        if !cover.layers.iter().any(|l| l.life == t.life && l.in_force_on(today)) {
            return Err(Reject::new("unknown_life", format!("life {} has no {} cover to reinstate", t.life, t.cover)));
        }
        if t.loadings.iter().any(|l| l.get().is_negative()) {
            return Err(Reject::new("invalid_loading", "a loading cannot be negative"));
        }
    }
    let approval = ReinstatementApproval {
        reference: spec.reference,
        valid_until: spec.valid_until,
        terms: spec.terms,
    };
    if let Some(lapse) = policy.lapse.as_mut().filter(|l| l.approval.as_ref() != Some(&approval)) {
        ctx.emit(Event::ReinstatementApproved {
            policy: policy.id.clone(),
            reference: approval.reference.clone(),
            valid_until: approval.valid_until,
        });
        lapse.approval = Some(approval);
    }
    Ok(())
}

/// One life's cover put on underwriting's terms as the policy is reinstated.
/// Cover already on them is left alone.
pub(crate) fn reinstated_terms(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, terms: LayerTerms) -> Result<(), Reject> {
    let spec = AlteredTerms {
        occupation_class: None,
        policy: policy.id.clone(),
        cover: terms.cover,
        life: terms.life,
        loadings: terms.loadings,
        exclusions: terms.exclusions,
        reference: String::new(),
    };
    match alter_terms(ledger, catalog, policy, ctx, spec) {
        Err(r) if r.code == "no_change" => Ok(()),
        done => done,
    }
}

/// Record that cover on this policy replaces cover on another policy for
/// the same person. Nothing is priced or posted: the record is for
/// reinsurance, which carries the old cession on, and for anyone asking
/// later what a policy replaced. The same replacement again changes nothing.
pub(crate) fn record_replacement(policy: &mut Policy, ctx: &mut Ctx<'_>, spec: crate::command::Replacement) -> Result<(), Reject> {
    let plain = |s: &str| !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_graphic());
    if spec.replaced_sum_insured_minor <= 0
        || !plain(&spec.evidence)
        || !plain(&spec.replaced_cover)
        || !plain(&spec.replaced_policy.0)
        || !plain(&spec.replaced_life.0)
    {
        return Err(Reject::new(
            "invalid_replacement",
            "a replacement names the replaced policy, life and cover, an amount above zero and where the disclosure is kept",
        ));
    }
    if spec.replaced_policy == policy.id {
        return Err(Reject::new("invalid_replacement", "a policy does not replace itself"));
    }
    if matches!(policy.status, PolicyStatus::Transferred | PolicyStatus::Avoided) {
        return Err(Reject::new("not_in_force", "the policy is not here to record a replacement on"));
    }
    let same = |r: &crate::command::Replacement| {
        r.replaced_policy == spec.replaced_policy
            && r.replaced_cover == spec.replaced_cover
            && r.replaced_life == spec.replaced_life
            && r.cover == spec.cover
            && r.life == spec.life
    };
    if let Some(known) = policy.replaces.iter().find(|r| same(r)) {
        return if known == &spec {
            Ok(())
        } else {
            Err(Reject::new(
                "replacement_recorded",
                "this replacement is recorded with a different amount or disclosure",
            ))
        };
    }
    let cover = policy
        .covers
        .get(&spec.cover)
        .ok_or_else(|| Reject::new("unknown_cover", format!("the policy has no cover {}", spec.cover)))?;
    let sum_insured_minor: i64 = cover
        .layers
        .iter()
        .filter(|l| l.life == spec.life && l.end.is_none_or(|e| e > ctx.date))
        .map(|l| l.sum_insured_minor)
        .sum();
    if sum_insured_minor <= 0 {
        return Err(Reject::new(
            "life_not_covered",
            format!("{} has no cover {} on the policy", spec.life, spec.cover),
        ));
    }
    ctx.emit(Event::CoverReplaced {
        policy: policy.id.clone(),
        cover: spec.cover.clone(),
        life: spec.life.clone(),
        replaced_policy: spec.replaced_policy.clone(),
        replaced_cover: spec.replaced_cover.clone(),
        replaced_life: spec.replaced_life.clone(),
        replaced_sum_insured_minor: spec.replaced_sum_insured_minor,
        sum_insured_minor,
    });
    policy.replaces.push(spec);
    Ok(())
}
