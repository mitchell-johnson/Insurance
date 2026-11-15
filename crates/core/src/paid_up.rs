//! Making a limited-pay cover paid up.
//!
//! A cover whose premium is paid only to a chosen age may, once enough of
//! that term has been paid for, stop paying and carry on for life at a
//! reduced sum insured: the share of the premium term paid for, times the
//! product's factor. The product declares the right (`paid_up_option`); the
//! owner asks through servicing. Each layer the life holds on the cover ends
//! today and carries on as a smaller layer that is never billed again, and
//! premium already billed for the days ahead is handed back.

use chrono::NaiveDate;
use ic_ledger::Ledger;
use ic_money::{dec_i128, round_integer, Dec, RoundingMode};
use ic_product::options::PremiumBasis;
use ic_product::schedule::age_on;
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::event::{Event, LayerInfo};
use crate::interest::plain;
use crate::machine::{Ctx, Reject};
use crate::policy::{Layer, Policy, PolicyStatus};
use crate::{LifeId, PolicyId};

/// The owner's request to stop paying for a cover and keep it reduced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MakePaidUp {
    pub policy: PolicyId,
    pub cover: String,
    pub life: LifeId,
    /// The request's own reference. The same one again changes nothing.
    pub reference: String,
}

/// A cover made paid up, kept on the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaidUpRecord {
    pub cover: String,
    pub life: LifeId,
    pub on: NaiveDate,
    pub sum_insured_minor: i64,
    pub paid_up_minor: i64,
}

/// The reduced sum insured for one layer: the sum insured times the whole
/// years paid for over the years of the premium term, times the factor,
/// cut down to a whole cent. None where less than the product's share of
/// the term has been paid for, or the term is already over.
pub(crate) fn reduced(sum_insured_minor: i64, years_paid: u32, term_years: u32, min_share: Dec, factor: Dec) -> Option<i64> {
    if term_years == 0 || years_paid >= term_years || Dec::from(i64::from(years_paid)) < min_share * Dec::from(i64::from(term_years)) {
        return None;
    }
    // Multiplied through first and divided last, so nothing is lost to a rounded fraction.
    let exact = dec_i128(i128::from(sum_insured_minor)) * Dec::from(i64::from(years_paid)) * factor / Dec::from(i64::from(term_years));
    round_integer(exact, RoundingMode::Down).ok().and_then(|v| i64::try_from(v).ok())
}

pub(crate) fn make_paid_up(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: MakePaidUp) -> Result<(), Reject> {
    paid_up(ledger, catalog, policy, ctx, spec, false)
}

/// Premium past its lapse period on a policy whose every cover still paying
/// premium has a paid-up option it qualifies for (AIA Cover for Life after the
/// paid-up date): each is made paid up today instead of the policy lapsing,
/// premium billed for the days ahead handed back and what is left unpaid for
/// the days already covered written off as at a lapse. False, with nothing
/// written, where any cover does not qualify: the policy then lapses as usual.
pub(crate) fn instead_of_lapse(
    ledger: &mut Ledger,
    catalog: &Catalog,
    product: &ic_product::ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
) -> Result<bool, Reject> {
    let today = ctx.date;
    let mut paying: Vec<(String, LifeId)> = Vec::new();
    for (code, cover) in &policy.covers {
        for layer in cover.layers.iter().filter(|l| l.in_force_on(today) && !l.paid_up) {
            if product.cover(code).is_none_or(|d| d.paid_up_option.is_none()) {
                return Ok(false);
            }
            if !paying.contains(&(code.clone(), layer.life.clone())) {
                paying.push((code.clone(), layer.life.clone()));
            }
        }
    }
    if paying.is_empty() {
        return Ok(false);
    }
    // Every cover must qualify before anything is written.
    for (code, life) in &paying {
        if plan_for(catalog, policy, code, life, today).is_err() {
            return Ok(false);
        }
    }
    for (code, life) in paying {
        let reference = format!("arrears-{today}-{code}-{}", life.0);
        let spec = MakePaidUp {
            policy: policy.id.clone(),
            cover: code,
            life,
            reference,
        };
        paid_up(ledger, catalog, policy, ctx, spec, true)?;
    }
    crate::arrears::write_off_open(ledger, product, policy, ctx, "paid_up_arrears")?;
    crate::arrears::set_standing(policy, ctx, None);
    Ok(true)
}

/// How each layer the life holds on the cover and still pays for would be
/// made paid up today: the layer's index and the sum insured it keeps.
fn plan_for(catalog: &Catalog, policy: &Policy, code: &str, life: &LifeId, today: NaiveDate) -> Result<Vec<(usize, i64)>, Reject> {
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "the policy's product version is not active"))?;
    let def = product
        .cover(code)
        .ok_or_else(|| Reject::new("unknown_cover", format!("the product has no cover {code}")))?;
    let option = def
        .paid_up_option
        .as_ref()
        .ok_or_else(|| Reject::new("no_paid_up_option", format!("cover {code} cannot be made paid up")))?;
    let born = policy
        .lives
        .get(life)
        .and_then(|l| l.factors.as_of(today))
        .and_then(|f| f.date_of_birth)
        .ok_or_else(|| Reject::new("life_not_covered", format!("{life} is not a life on the policy with a date of birth")))?;
    let cover = policy
        .covers
        .get(code)
        .ok_or_else(|| Reject::new("unknown_cover", format!("the policy has no cover {code}")))?;
    let held: Vec<usize> = (0..cover.layers.len())
        .filter(|&i| &cover.layers[i].life == life && cover.layers[i].in_force_on(today) && !cover.layers[i].paid_up)
        .collect();
    if held.is_empty() {
        return Err(Reject::new(
            "life_not_covered",
            format!("{life} holds no cover {code} still paying premium"),
        ));
    }
    // Every layer must qualify, or none is touched.
    let mut plan = Vec::new();
    for &i in &held {
        let layer = &cover.layers[i];
        let Some(PremiumBasis::LimitedPay { to_age }) = def.premium_basis(layer.excess_option.as_deref()) else {
            return Err(Reject::new("no_paid_up_option", "only cover on a limited premium term can be made paid up"));
        };
        let term_years = to_age.saturating_sub(age_on(born, layer.rating_start()));
        let years_paid = age_on(layer.rating_start(), today);
        let kept = reduced(
            layer.sum_insured_minor,
            years_paid,
            term_years,
            option.min_share_of_term.get(),
            option.factor.get(),
        )
        .filter(|kept| *kept > 0)
        .ok_or_else(|| {
            Reject::new(
                "paid_up_too_early",
                format!("{years_paid} of {term_years} years of the premium term are paid for; the product asks for more"),
            )
        })?;
        plan.push((i, kept));
    }
    Ok(plan)
}

fn paid_up(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: MakePaidUp, on_arrears: bool) -> Result<(), Reject> {
    if !plain(&spec.reference) {
        return Err(Reject::new("invalid_request", "a request has a plain reference"));
    }
    if let Some(done) = policy.paid_up.get(&spec.reference) {
        return if done.cover == spec.cover && done.life == spec.life {
            Ok(())
        } else {
            Err(Reject::new(
                "duplicate_request",
                format!("reference {} made another cover paid up", spec.reference),
            ))
        };
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force can be made paid up"));
    }
    // Paid up means paid: premium owing is settled first, or the policy
    // goes the way of any policy in arrears.
    if !on_arrears && policy.policyholder_outstanding_minor() > 0 {
        return Err(Reject::new("premium_owing", "premium is owing; it is paid before a cover is made paid up"));
    }
    let today = ctx.date;
    let plan = plan_for(catalog, policy, &spec.cover, &spec.life, today)?;
    let before = policy.clone();
    let id = policy.id.clone();
    let cover = policy
        .covers
        .get_mut(&spec.cover)
        .ok_or_else(|| Reject::new("unknown_cover", format!("the policy has no cover {}", spec.cover)))?;
    let cover_type = cover.cover_type.clone();
    let mut next_id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
    let (mut was_total, mut kept_total) = (0i64, 0i64);
    for (i, kept) in plan {
        let old = cover.layers[i].clone();
        cover.layers[i].end = Some(today);
        was_total += old.sum_insured_minor;
        kept_total += kept;
        ctx.emit(Event::LayerReduced {
            policy: id.clone(),
            cover: spec.cover.clone(),
            layer: old.id,
            continued_as: next_id,
            effective: today,
            sum_insured_minor: old.sum_insured_minor,
            kept_minor: kept,
        });
        ctx.emit(Event::LayerEnded {
            policy: id.clone(),
            cover: spec.cover.clone(),
            layer: old.id,
            life: old.life.clone(),
            effective: today,
            sum_insured_minor: old.sum_insured_minor,
        });
        cover.layers.push(Layer {
            id: next_id,
            start: today,
            end: None,
            sum_insured_minor: kept,
            rated_from: Some(old.rating_start()),
            paid_up: true,
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
                start: today,
                option: old.excess_option.clone(),
            },
            origin: old.origin,
            continues: None,
        });
        next_id += 1;
    }
    // Premium billed for the days from today is handed back.
    crate::reduce::adjust(ledger, catalog, &before, policy, ctx, &spec.life)?;
    ctx.emit(Event::CoverMadePaidUp {
        policy: id,
        cover: spec.cover.clone(),
        life: spec.life.clone(),
        reference: spec.reference.clone(),
        effective: today,
        sum_insured_minor: was_total,
        paid_up_minor: kept_total,
    });
    policy.paid_up.insert(
        spec.reference,
        PaidUpRecord {
            cover: spec.cover,
            life: spec.life,
            on: today,
            sum_insured_minor: was_total,
            paid_up_minor: kept_total,
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: the paid-up sum insured, over sums from a cent to ten million dollars, every
    /// premium term from 1 to 40 years and every number of years paid, equals what integer
    /// long division gives (the sum times the years paid times 95, over the term times 100,
    /// cut to the cent), and is refused below half the term and once the term is over.
    /// Why it matters: this figure becomes the cover a family is paid for decades later. It
    /// is checked against arithmetic that shares nothing with the decimal library, the way
    /// the premium is, and the boundary at exactly half the term is where an off-by-one
    /// would let a cover be made paid up a year early.
    #[test]
    fn the_paid_up_sum_insured_equals_integer_long_division() {
        let (half, factor) = (ic_money::dec("0.5").unwrap(), ic_money::dec("0.95").unwrap());
        let mut compared = 0;
        for sum in [1i64, 99, 100_001_01, 250_000_00, 333_333_33, 1_000_000_000] {
            for term in 1u32..=40 {
                for paid in 0u32..=41 {
                    let expected =
                        (paid * 2 >= term && paid < term).then(|| i64::try_from(i128::from(sum) * i128::from(paid) * 95 / (i128::from(term) * 100)).unwrap());
                    assert_eq!(reduced(sum, paid, term, half, factor), expected, "{sum} after {paid} of {term} years");
                    compared += 1;
                }
            }
        }
        assert_eq!(compared, 6 * 40 * 42);
        assert_eq!(reduced(100_000_00, 5, 0, half, factor), None, "a term of no years has nothing to make paid up");
    }
}
