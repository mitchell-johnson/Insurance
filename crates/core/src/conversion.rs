//! Converting cover without evidence of health.
//!
//! A cover may declare conversions (`conversion` in the product):
//! all or part of a life's cover moved, from today and without evidence, to
//! another choice on one of its axes, for any kind of cover (AIA's Premium Conversion Facility: a
//! stepped premium to a level one, or a level term to a shorter one), or to
//! another lump sum cover of the product, with as much of a third cover where the
//! product says so (AIA's Standalone Conversion Facility: stand-alone trauma
//! to accelerated trauma with an equal amount of life cover). The converted
//! amount is priced from today at the life's age today; premium already
//! billed for the days ahead is adjusted once for the whole change. Nothing
//! is written unless all of it is.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use ic_ledger::Ledger;
use ic_product::schedule::age_on;
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::event::{Event, LayerInfo};
use crate::interest::plain;
use crate::machine::{Ctx, Reject};
use crate::policy::{Cover, CoverStatus, Layer, LayerOrigin, MemberPays, Policy, PolicyStatus};
use crate::{LifeId, PolicyId};

/// The owner asks to convert all or part of a life's cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertCover {
    pub policy: PolicyId,
    pub cover: String,
    pub life: LifeId,
    /// How much of the life's cover is converted.
    pub sum_insured_minor: i64,
    /// The cover converted to: `cover` itself for a change of choice.
    pub to_cover: String,
    /// The choices on the cover converted to (`axis=choice,...`).
    pub excess_option: String,
    /// The request's own reference. The same one again changes nothing.
    pub reference: String,
}

/// A conversion made, kept on the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionRecord {
    pub cover: String,
    pub to_cover: String,
    pub life: LifeId,
    pub on: NaiveDate,
    pub sum_insured_minor: i64,
    pub excess_option: String,
}

fn held_layers<'a>(policy: &'a Policy, cover: &str, life: &LifeId, on: NaiveDate) -> Vec<&'a Layer> {
    policy
        .covers
        .get(cover)
        .filter(|c| c.status == CoverStatus::InForce)
        .map(|c| c.layers.iter().filter(|l| &l.life == life && l.in_force_on(on)).collect())
        .unwrap_or_default()
}

/// The choice an option code makes on an axis.
fn choice_on<'a>(option: &'a str, axis: &str) -> Option<&'a str> {
    option.split(',').filter_map(|p| p.split_once('=')).find(|(a, _)| *a == axis).map(|(_, c)| c)
}

/// Write a layer of `sum` on a cover for a life from `on`, announcing it.
fn write_layer(policy: &mut Policy, ctx: &mut Ctx<'_>, cover: &str, template: Layer, on: NaiveDate) -> u32 {
    let id = policy.id.clone();
    let Some(c) = policy.covers.get_mut(cover) else { return 0 };
    let layer_id = c.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let layer = Layer {
        id: layer_id,
        start: on,
        end: None,
        ..template
    };
    ctx.emit(Event::LayerAdded {
        policy: id,
        layer: LayerInfo {
            cover: cover.to_string(),
            cover_type: c.cover_type.clone(),
            layer: layer_id,
            life: layer.life.clone(),
            sum_insured_minor: layer.sum_insured_minor,
            start: on,
            option: layer.excess_option.clone(),
        },
        origin: layer.origin,
        continues: None,
    });
    c.layers.push(layer);
    layer_id
}

pub(crate) fn convert(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: ConvertCover) -> Result<(), Reject> {
    if !plain(&spec.reference) || spec.sum_insured_minor <= 0 {
        return Err(Reject::new("invalid_request", "a conversion has a plain reference and an amount above zero"));
    }
    if let Some(done) = policy.conversions.get(&spec.reference) {
        let same = done.cover == spec.cover
            && done.to_cover == spec.to_cover
            && done.life == spec.life
            && done.sum_insured_minor == spec.sum_insured_minor
            && done.excess_option == spec.excess_option;
        return if same {
            Ok(())
        } else {
            Err(Reject::new("duplicate_request", format!("reference {} converted other cover", spec.reference)))
        };
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "cover is converted only on a policy in force"));
    }
    if policy.claims.values().any(|c| c.open && (c.cover == spec.cover || c.cover == spec.to_cover)) {
        return Err(Reject::new("claim_open", "cover with a claim open on it is not converted"));
    }
    if policy.policyholder_outstanding_minor() > 0 {
        return Err(Reject::new("premium_owing", "premium is owing; it is paid before cover is converted"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .cloned()
        .ok_or_else(|| Reject::new("unknown_product", "the policy's product version is not active"))?;
    let def = product
        .cover(&spec.cover)
        .ok_or_else(|| Reject::new("unknown_cover", format!("the product has no cover {}", spec.cover)))?;
    let Some(rule) = def.conversions.iter().find(|c| c.to == spec.to_cover) else {
        return Err(Reject::new(
            "no_conversion",
            format!("cover {} cannot be converted to {}", spec.cover, spec.to_cover),
        ));
    };
    let today = ctx.date.max(policy.inception);
    let Some(life) = policy.lives.get(&spec.life).filter(|l| l.in_force_on(today)) else {
        return Err(Reject::new("unknown_life", format!("life {} is not on the policy", spec.life)));
    };
    let born = life
        .factors
        .as_of(today)
        .and_then(|f| f.date_of_birth)
        .ok_or_else(|| Reject::new("unknown_life", format!("life {} has no date of birth", spec.life)))?;
    if age_on(born, today) >= rule.before_age {
        return Err(Reject::new("conversion_age", format!("cover is converted before age {}", rule.before_age)));
    }
    let held = held_layers(policy, &spec.cover, &spec.life, today);
    let total: i64 = held.iter().map(|l| l.sum_insured_minor).sum();
    if held.is_empty() || spec.sum_insured_minor > total {
        return Err(Reject::new(
            "life_not_covered",
            format!("life {} holds less than that of cover {}", spec.life, spec.cover),
        ));
    }
    if rule.standard_terms_only && held.iter().any(|l| !l.loadings.is_empty() || !l.exclusions.is_empty()) {
        return Err(Reject::new("special_terms", "only cover on standard terms is converted this way"));
    }
    let newest = held.iter().max_by_key(|l| (l.start, l.id)).map(|l| (*l).clone()).expect("held is not empty");
    let to_def = product
        .cover(&spec.to_cover)
        .ok_or_else(|| Reject::new("unknown_cover", format!("the product has no cover {}", spec.to_cover)))?;
    if to_def.option(&spec.excess_option).is_err() {
        return Err(Reject::new("excess_option", format!("cover {} does not offer {}", spec.to_cover, spec.excess_option)));
    }
    if let Some(axis) = &rule.axis {
        // Only the axis changes, to a choice the product allows from the one held.
        let from = newest.excess_option.clone().unwrap_or_default();
        let others = |o: &str| -> Vec<String> { o.split(',').filter(|p| !p.starts_with(&format!("{axis}="))).map(str::to_string).collect() };
        let to = choice_on(&spec.excess_option, axis).unwrap_or_default();
        let from_at = choice_on(&from, axis).and_then(|c| rule.choices.iter().position(|x| x == c));
        let to_at = rule.choices.iter().position(|x| x == to);
        let allowed = match (from_at, to_at) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(f), Some(t)) => t >= f,
        };
        if others(&from) != others(&spec.excess_option) || !allowed || spec.excess_option == from {
            return Err(Reject::new(
                "excess_option",
                format!("cover {} converts only its {axis} to one of {:?}, no longer than the one held", spec.cover, rule.choices),
            ));
        }
        // The newest layers are converted first: only as much as is held at the same choices.
        let mut newest_first = held.clone();
        newest_first.sort_by_key(|l| std::cmp::Reverse((l.start, l.id)));
        let alike: i64 = newest_first
            .iter()
            .take_while(|l| l.excess_option.as_deref().unwrap_or_default() == from)
            .map(|l| l.sum_insured_minor)
            .sum();
        if spec.sum_insured_minor > alike {
            return Err(Reject::new(
                "life_not_covered",
                format!("life {} holds {alike} of cover {} at {from}; convert each part on its own", spec.life, spec.cover),
            ));
        }
    } else {
        // Another cover: the life's covers after the conversion must be a set the product sells.
        let mut covers: BTreeSet<&str> = policy
            .covers
            .keys()
            .map(String::as_str)
            .filter(|code| !held_layers(policy, code, &spec.life, today).is_empty())
            .collect();
        if spec.sum_insured_minor == total {
            covers.remove(spec.cover.as_str());
        }
        covers.insert(spec.to_cover.as_str());
        product
            .check_composition(&covers)
            .map_err(|e| Reject::new("composition", format!("life {}: {e}", spec.life)))?;
        crate::lifecycle::entry_age(to_def, life, today)?;
        if policy.covers.get(&spec.to_cover).is_some_and(|c| c.status != CoverStatus::InForce) {
            return Err(Reject::new("cover_ended", format!("{} has ended on this policy", spec.to_cover)));
        }
        let to_total: i64 = held_layers(policy, &spec.to_cover, &spec.life, today).iter().map(|l| l.sum_insured_minor).sum();
        ic_product::rating::check_sum_insured(to_def, to_total + spec.sum_insured_minor)
            .map_err(|e| Reject::new("sum_insured", format!("cover {}: {e}", spec.to_cover)))?;
    }
    let with = match &rule.with_cover {
        Some(w) => {
            let layers = held_layers(policy, w, &spec.life, today);
            let Some(newest) = layers.iter().max_by_key(|l| (l.start, l.id)).map(|l| (*l).clone()) else {
                return Err(Reject::new("life_not_covered", format!("life {} holds no {w} to add to", spec.life)));
            };
            let w_total: i64 = layers.iter().map(|l| l.sum_insured_minor).sum();
            let w_def = product.cover(w).ok_or_else(|| Reject::new("unknown_cover", format!("the product has no cover {w}")))?;
            ic_product::rating::check_sum_insured(w_def, w_total + spec.sum_insured_minor)
                .map_err(|e| Reject::new("sum_insured", format!("cover {w}: {e}")))?;
            Some((w.clone(), newest))
        }
        None => None,
    };

    // Everything is written on a draft, and kept only if the price of it can be billed.
    let mark = ctx.events.len();
    let before = policy.clone();
    let mut draft = policy.clone();
    crate::reduce::cut_layers(&mut draft, ctx, &spec.cover, &spec.life, today, spec.sum_insured_minor, false);
    if !draft.covers.contains_key(&spec.to_cover) {
        let mut bindings = Vec::new();
        for id in &to_def.definitions {
            let Some(bound) = catalog.bind_definition(id, today) else {
                ctx.events.truncate(mark);
                return Err(Reject::new("unknown_definition", format!("definition {id} has no version in force on {today}")));
            };
            bindings.push(bound);
        }
        draft.covers.insert(
            spec.to_cover.clone(),
            Cover {
                code: to_def.code.clone(),
                cover_type: to_def.cover_type.clone(),
                status: CoverStatus::InForce,
                start: today,
                definitions: bindings,
                layers: vec![],
                formula_driven: false,
                ended_on: None,
            },
        );
    }
    let converted = Layer {
        id: 0,
        life: spec.life.clone(),
        start: today,
        end: None,
        sum_insured_minor: spec.sum_insured_minor,
        origin: newest.origin,
        loadings: newest.loadings.clone(),
        exclusions: newest.exclusions.clone(),
        loading_itemised: newest.loading_itemised,
        // Priced at the life's age today.
        rated_from: None,
        excess_option: Some(spec.excess_option.clone()),
        member_pays: MemberPays::Nothing,
        paid_up: false,
    };
    write_layer(&mut draft, ctx, &spec.to_cover, converted, today);
    if let Some((w, template)) = with {
        let added = Layer {
            id: 0,
            sum_insured_minor: spec.sum_insured_minor,
            origin: LayerOrigin::Increase,
            rated_from: None,
            ..template
        };
        write_layer(&mut draft, ctx, &w, added, today);
    }
    if let Err(e) = crate::reduce::adjust(ledger, catalog, &before, &mut draft, ctx, &spec.life) {
        ctx.events.truncate(mark);
        return Err(e);
    }
    *policy = draft;
    ctx.emit(Event::CoverConverted {
        policy: policy.id.clone(),
        cover: spec.cover.clone(),
        to_cover: spec.to_cover.clone(),
        life: spec.life.clone(),
        sum_insured_minor: spec.sum_insured_minor,
        option: spec.excess_option.clone(),
        reference: spec.reference.clone(),
    });
    policy.conversions.insert(
        spec.reference,
        ConversionRecord {
            cover: spec.cover,
            to_cover: spec.to_cover,
            life: spec.life,
            on: today,
            sum_insured_minor: spec.sum_insured_minor,
            excess_option: spec.excess_option,
        },
    );
    Ok(())
}
