//! Premium for a policy period. One code path for quotes, issue, billing,
//! anniversaries and retrospective adjustments.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_money::{round_to_money, Dec, MoneyError, RoundingMode};
use ic_product::discount::{Line, PricingState};
use ic_product::rating::{cover_premium, modal_premium, modal_premium_by_line, ModalPremium, RatingError, RatingFactors};
use ic_product::schedule::{age_on, anniversary, policy_year};
use ic_product::{CoverDefinition, PremiumType, ProductDefinition, RateBasis};

use crate::catalog::Catalog;
use crate::policy::{CoverStatus, Layer, LifeFactors, LifeKind, LifeRecord, Policy};
use crate::LifeId;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PremiumError {
    #[error("cover {0} is not offered by the product")]
    UnknownCover(String),
    #[error("rate table {0} is not active for the rating date")]
    NoRateTable(String),
    #[error("life {0} is missing the rating factor {1}")]
    MissingFactor(LifeId, &'static str),
    #[error("life {0} is not on the policy")]
    UnknownLife(LifeId),
    #[error("date out of range")]
    Date,
    #[error(transparent)]
    Rating(#[from] RatingError),
    #[error(transparent)]
    Money(#[from] MoneyError),
}

/// Premiums are rated as at the last anniversary on or before the period.
pub fn rating_date(policy: &Policy, period_start: NaiveDate) -> Result<NaiveDate, PremiumError> {
    anniversary(policy.inception, policy_year(policy.inception, period_start)).ok_or(PremiumError::Date)
}

pub(crate) fn factors_on(life: &LifeRecord, on: NaiveDate) -> Result<&LifeFactors, PremiumError> {
    // A life's first factors are valid from the day it joined; a period that
    // starts earlier (a retrospective joiner) rates on the joining facts.
    life.factors
        .as_of(on.max(life.joined))
        .ok_or_else(|| PremiumError::MissingFactor(life.id.clone(), "any"))
}

/// One period's premium for a retail policy: every layer in force on the
/// period's first day, rated on its own terms.
pub fn retail_period_premium(
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &Policy,
    period_start: NaiveDate,
    work: &mut u32,
) -> Result<ModalPremium, PremiumError> {
    retail_premium_with(catalog, product, policy, period_start, work, Layers::AtStart)
}

/// Which layers a period's premium is worked out over.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Layers<'a> {
    /// The layers in force on the period's first day.
    AtStart,
    /// Those, and one more (a cover and a layer number) that started after
    /// the period's first day.
    With(&'a str, u32),
    /// The layers in force on a later day of the period, each priced for
    /// the whole period. The difference between two of these is what one
    /// change of cover on that day is worth.
    On(NaiveDate),
}

/// As [`retail_period_premium`], counting one more layer (a cover code and
/// a layer number) that started after the period's first day. Used to price
/// cover added part way through a period already billed.
pub(crate) fn retail_premium_with(
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &Policy,
    period_start: NaiveDate,
    work: &mut u32,
    layers: Layers<'_>,
) -> Result<ModalPremium, PremiumError> {
    let rated_on = rating_date(policy, period_start)?;
    let mut annual: BTreeMap<Line, Dec> = BTreeMap::new();
    let mut lives = std::collections::BTreeSet::new();
    for cover in policy.covers.values() {
        if cover.status != CoverStatus::InForce || cover.formula_driven {
            continue;
        }
        let def = product.cover(&cover.code).ok_or_else(|| PremiumError::UnknownCover(cover.code.clone()))?;
        let table = catalog
            .rate_table_on(&def.rating.table, rated_on, &policy.group)
            .ok_or_else(|| PremiumError::NoRateTable(def.rating.table.clone()))?;
        for layer in cover.layers.iter().filter(|l| match layers {
            Layers::AtStart => l.in_force_on(period_start),
            Layers::With(code, id) => l.in_force_on(period_start) || (code == cover.code && id == l.id),
            Layers::On(day) => l.in_force_on(day),
        }) {
            *work += 1;
            lives.insert(&layer.life);
            let life = policy.lives.get(&layer.life).ok_or_else(|| PremiumError::UnknownLife(layer.life.clone()))?;
            // Priced as the cover stands on a later day, the life is rated on
            // what is known of it that day: a change of occupation reaches the
            // days left of a period already billed.
            let facts_on = match &layers {
                Layers::On(day) => (*day).max(period_start),
                _ => period_start,
            };
            let f = factors_on(life, facts_on)?;
            let dob = f.date_of_birth.ok_or_else(|| PremiumError::MissingFactor(life.id.clone(), "date_of_birth"))?;
            let age = match def.rating.premium_type {
                PremiumType::Stepped | PremiumType::GroupUnitRate => age_on(dob, rated_on),
                // Level until the cover's stated age, then on attained age
                // like a stepped premium.
                PremiumType::Level => match (age_on(dob, rated_on), def.rating.level_to_age) {
                    (attained, Some(to)) if attained >= to => attained,
                    _ => age_on(dob, layer.rating_start()),
                },
            };
            let rf = RatingFactors {
                age,
                sex: f.sex,
                smoker: f.smoker,
                category: f.category.clone(),
                occupation_class: f.occupation_class.clone(),
                excess_option: layer.excess_option.clone(),
                loadings: layer.loadings.clone(),
            };
            let p = cover_premium(def, table, &rf, layer.sum_insured_minor)?;
            let line = Line {
                cover: cover.code.clone(),
                life: layer.life.to_string(),
            };
            *annual.entry(line).or_insert(Dec::from(0i64)) += p;
        }
    }
    // What of the policy's state the product's discounts are judged on.
    // Entitlements are read as held on the period's first day, or on the
    // later day the cover is priced as it stands on.
    let read_on = match layers {
        Layers::On(day) => day.max(period_start),
        _ => period_start,
    };
    let held = crate::entitlement::held_on(policy, read_on);
    let overrides = crate::discount_override::in_force_on(policy, read_on);
    let covers: std::collections::BTreeSet<&str> = annual.keys().map(|l| l.cover.as_str()).collect();
    let state = PricingState {
        overrides: &overrides,
        covers: covers.len() as u32,
        lives: lives.len() as u32,
        frequency: policy.frequency,
        entitlements: &held,
    };
    Ok(modal_premium_by_line(product, &annual, &state)?)
}

/// Formula-driven sum insured for a scheme member: a multiple of salary, to
/// the category maximum and the automatic acceptance limit. Cover above the
/// limit needs underwriting and is written as a separate layer.
pub fn member_sum_insured(
    def: &CoverDefinition,
    f: &LifeFactors,
    life: &LifeId,
    rounding: RoundingMode,
    currency: ic_money::Currency,
) -> Result<i64, PremiumError> {
    let category = f.category.as_deref().ok_or_else(|| PremiumError::MissingFactor(life.clone(), "category"))?;
    let cat = def
        .categories
        .iter()
        .find(|c| c.code == category)
        .ok_or_else(|| PremiumError::MissingFactor(life.clone(), "category"))?;
    // A pool is rated on its declared total pay; the per-person limits apply
    // to it as many times as there are people.
    let (salary, lives) = match f.pool {
        Some(pool) => (pool.total_salary_minor, i64::from(pool.headcount)),
        None => (f.salary_minor.ok_or_else(|| PremiumError::MissingFactor(life.clone(), "salary"))?, 1),
    };
    let si = round_to_money(Dec::from(salary) / Dec::from(100i64) * cat.salary_multiple.get(), currency, rounding)?.minor;
    let mut si = si.min(cat.max_sum_insured_minor.saturating_mul(lives));
    if let Some(aal) = def.automatic_acceptance_limit_minor {
        si = si.min(aal.saturating_mul(lives));
    }
    if let Some(cap) = f.auto_cover_cap_minor {
        si = si.min(cap.max(0));
    }
    Ok(si)
}

/// Which underwriting loadings a price includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loadings {
    /// Every loading: what the scheme is charged.
    All,
    /// Only loadings the member agreed the employer may see: what a
    /// member's own invoice line shows.
    ItemisedOnly,
}

/// A member's own layers in force on a date (underwritten cover above the
/// automatic amount, and covers chosen from the scheme's menu), with the
/// cover each sits on.
pub fn excess_layers_on<'a>(policy: &'a Policy, life: &LifeId, on: NaiveDate) -> Vec<(&'a str, &'a Layer)> {
    policy
        .covers
        .values()
        .flat_map(|c| c.layers.iter().filter(|l| c.members_own(l)).map(move |l| (c.code.as_str(), l)))
        .filter(|(_, l)| l.life == *life && l.in_force_on(on))
        .collect()
}

/// One member's premium for one scheme billing period.
pub fn member_period_premium(
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &Policy,
    life: &LifeRecord,
    period_start: NaiveDate,
) -> Result<ModalPremium, PremiumError> {
    member_premium_with(catalog, product, policy, &life.id, factors_on(life, period_start)?, period_start)
}

/// The same, showing only the loadings a member's invoice line may show.
pub fn member_period_line(
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &Policy,
    life: &LifeRecord,
    period_start: NaiveDate,
) -> Result<ModalPremium, PremiumError> {
    let excess = excess_layers_on(policy, &life.id, period_start);
    member_premium_detail(
        catalog,
        product,
        policy,
        &life.id,
        factors_on(life, period_start)?,
        period_start,
        &excess,
        Loadings::ItemisedOnly,
    )
}

/// A member's premium for a period on explicit factors: used to price both
/// sides of a retrospective change.
pub fn member_premium_with(
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &Policy,
    life_id: &LifeId,
    f: &LifeFactors,
    period_start: NaiveDate,
) -> Result<ModalPremium, PremiumError> {
    let excess = excess_layers_on(policy, life_id, period_start);
    member_premium_detail(catalog, product, policy, life_id, f, period_start, &excess, Loadings::All)
}

/// A member's premium for a period on explicit factors and an explicit set
/// of underwritten layers: the automatic cover at the scheme's rates, plus
/// each layer at the same rates with its own loadings.
#[allow(clippy::too_many_arguments)]
pub fn member_premium_detail(
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &Policy,
    life_id: &LifeId,
    f: &LifeFactors,
    period_start: NaiveDate,
    excess: &[(&str, &Layer)],
    loadings: Loadings,
) -> Result<ModalPremium, PremiumError> {
    let rated_on = rating_date(policy, period_start)?;
    let mut annual: BTreeMap<String, Dec> = BTreeMap::new();
    // A dependant has no cover the scheme gives automatically.
    let dependant = policy.lives.get(life_id).is_some_and(|l| matches!(l.kind, LifeKind::Dependant { .. }));
    for cover in policy
        .covers
        .values()
        .filter(|c| c.formula_driven && c.status == CoverStatus::InForce && !dependant)
    {
        let def = product.cover(&cover.code).ok_or_else(|| PremiumError::UnknownCover(cover.code.clone()))?;
        let table = catalog
            .rate_table_on(&def.rating.table, rated_on, &policy.group)
            .ok_or_else(|| PremiumError::NoRateTable(def.rating.table.clone()))?;
        let si = member_sum_insured(def, f, life_id, product.rounding, product.currency)?;
        let age = f.date_of_birth.map_or(0, |dob| age_on(dob, rated_on));
        let rf = RatingFactors {
            age,
            sex: f.sex,
            smoker: f.smoker,
            category: f.category.clone(),
            occupation_class: f.occupation_class.clone(),
            excess_option: None,
            loadings: vec![],
        };
        let mut premium = cover_premium(def, table, &rf, si)?;
        if let (Some(pool), RateBasis::PerLife) = (f.pool, def.rating.basis) {
            premium *= Dec::from(i64::from(pool.headcount));
        }
        for (_, layer) in excess.iter().filter(|(code, _)| *code == cover.code) {
            let shown = loadings == Loadings::All || layer.loading_itemised;
            let rf = RatingFactors {
                loadings: if shown { layer.loadings.clone() } else { vec![] },
                ..rf.clone()
            };
            premium += cover_premium(def, table, &rf, layer.sum_insured_minor)?;
        }
        annual.insert(cover.code.clone(), premium);
    }
    // Covers chosen from the scheme's menu: each is the life's own, at the
    // level of excess chosen, rated on the life's own details.
    for (code, layer) in excess {
        let def = product.cover(code).ok_or_else(|| PremiumError::UnknownCover(code.to_string()))?;
        if def.rating.premium_type == PremiumType::GroupUnitRate {
            continue;
        }
        let table = catalog
            .rate_table_on(&def.rating.table, rated_on, &policy.group)
            .ok_or_else(|| PremiumError::NoRateTable(def.rating.table.clone()))?;
        let shown = loadings == Loadings::All || layer.loading_itemised;
        let rf = RatingFactors {
            age: f.date_of_birth.map_or(0, |dob| age_on(dob, rated_on)),
            sex: f.sex,
            smoker: f.smoker,
            category: f.category.clone(),
            occupation_class: f.occupation_class.clone(),
            excess_option: layer.excess_option.clone(),
            loadings: if shown { layer.loadings.clone() } else { vec![] },
        };
        *annual.entry(code.to_string()).or_insert(Dec::from(0i64)) += cover_premium(def, table, &rf, layer.sum_insured_minor)?;
    }
    Ok(modal_premium(product, &annual, policy.frequency)?)
}

/// Add one bill into another, line by line.
pub fn add_premium(into: &mut ModalPremium, p: &ModalPremium) -> Result<(), PremiumError> {
    for (k, v) in &p.by_cover {
        let e = into.by_cover.entry(k.clone()).or_insert(ic_money::Money::zero(v.currency));
        *e = e.checked_add(*v)?;
    }
    for (k, v) in &p.gst {
        let e = into.gst.entry(k.clone()).or_insert(ic_money::Money::zero(v.currency));
        *e = e.checked_add(*v)?;
    }
    for (k, v) in &p.levies {
        let e = into.levies.entry(k.clone()).or_insert(ic_money::Money::zero(v.currency));
        *e = e.checked_add(*v)?;
    }
    for (k, v) in &p.gross_by_cover {
        let e = into.gross_by_cover.entry(k.clone()).or_insert(ic_money::Money::zero(v.currency));
        *e = e.checked_add(*v)?;
    }
    for line in &p.discounts {
        match into.discounts.iter_mut().find(|d| d.code == line.code) {
            Some(d) => d.amount = d.amount.checked_add(line.amount)?,
            None => into.discounts.push(line.clone()),
        }
    }
    into.policy_fee = into.policy_fee.checked_add(p.policy_fee)?;
    Ok(())
}

pub fn zero_premium(currency: ic_money::Currency) -> ModalPremium {
    ModalPremium {
        by_cover: BTreeMap::new(),
        policy_fee: ic_money::Money::zero(currency),
        gst: BTreeMap::new(),
        levies: BTreeMap::new(),
        gross_by_cover: BTreeMap::new(),
        discounts: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ic_money::{Currency::NZD, Money};

    /// Purpose: adding one bill into another adds every line: premium by
    /// cover, tax by cover, each levy and the policy fee, including lines
    /// the first bill did not have.
    /// Why it matters: a scheme's bill and a policy with several lives are
    /// built by adding bills together. A line dropped here would be tax or
    /// a levy collected from nobody and still owed to the government.
    #[test]
    fn adding_bills_adds_tax_and_levies_line_by_line() {
        let m = |minor| Money::new(minor, NZD);
        let bill = |cover: &str, premium, gst, levy, fee| ModalPremium {
            by_cover: [(cover.to_string(), m(premium))].into(),
            policy_fee: m(fee),
            gst: [(cover.to_string(), m(gst))].into(),
            levies: [("fire".to_string(), m(levy))].into(),
            gross_by_cover: [(cover.to_string(), m(i64::saturating_add(premium, 5_00)))].into(),
            discounts: vec![ic_product::rating::DiscountLine {
                code: "loyalty".to_string(),
                name: "Loyalty".to_string(),
                posting_type: "discount_loyalty".to_string(),
                amount: m(5_00),
            }],
        };
        let mut total = bill("life", 100_00, 15_00, 2_00, 3_00);
        add_premium(&mut total, &bill("life", 50_00, 7_50, 1_00, 0)).unwrap();
        add_premium(&mut total, &bill("trauma", 20_00, 3_00, 40, 0)).unwrap();
        assert_eq!((total.by_cover["life"].minor, total.by_cover["trauma"].minor), (150_00, 20_00));
        assert_eq!((total.gst["life"].minor, total.gst["trauma"].minor), (22_50, 3_00));
        assert_eq!((total.levies["fire"].minor, total.policy_fee.minor), (3_40, 3_00));
        // Discount lines add by code, and the premium before them by cover.
        assert_eq!((total.discounts.len(), total.discounts[0].amount.minor), (1, 15_00));
        assert_eq!((total.gross_by_cover["life"].minor, total.gross_by_cover["trauma"].minor), (160_00, 25_00));
        let too_much = bill("life", i64::MAX, 0, 0, 0);
        assert!(add_premium(&mut total, &too_much).is_err(), "an overflow is an error, never a wrapped amount");
    }
}
