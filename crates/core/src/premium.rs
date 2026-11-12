//! Premium for a policy period. One code path for quotes, issue, billing,
//! anniversaries and retrospective adjustments.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_money::{round_to_money, Dec, MoneyError, RoundingMode};
use ic_product::rating::{cover_premium, modal_premium, ModalPremium, RatingError, RatingFactors};
use ic_product::schedule::{age_on, anniversary, policy_year};
use ic_product::{CoverDefinition, PremiumType, ProductDefinition, RateBasis};

use crate::catalog::Catalog;
use crate::policy::{CoverStatus, Layer, LayerOrigin, LifeFactors, LifeRecord, Policy};
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
    let mut annual: BTreeMap<String, Dec> = BTreeMap::new();
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
            let life = policy.lives.get(&layer.life).ok_or_else(|| PremiumError::UnknownLife(layer.life.clone()))?;
            let f = factors_on(life, period_start)?;
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
                loadings: layer.loadings.clone(),
            };
            let p = cover_premium(def, table, &rf, layer.sum_insured_minor)?;
            *annual.entry(cover.code.clone()).or_insert(Dec::from(0i64)) += p;
        }
    }
    Ok(modal_premium(product, &annual, policy.frequency)?)
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

/// A member's underwritten layers in force on a date, with the cover each
/// sits on.
pub fn excess_layers_on<'a>(policy: &'a Policy, life: &LifeId, on: NaiveDate) -> Vec<(&'a str, &'a Layer)> {
    policy
        .covers
        .values()
        .filter(|c| c.formula_driven)
        .flat_map(|c| c.layers.iter().map(move |l| (c.code.as_str(), l)))
        .filter(|(_, l)| l.life == *life && l.origin == LayerOrigin::UnderwrittenExcess && l.in_force_on(on))
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
    for cover in policy.covers.values().filter(|c| c.formula_driven && c.status == CoverStatus::InForce) {
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
    into.policy_fee = into.policy_fee.checked_add(p.policy_fee)?;
    Ok(())
}

pub fn zero_premium(currency: ic_money::Currency) -> ModalPremium {
    ModalPremium {
        by_cover: BTreeMap::new(),
        policy_fee: ic_money::Money::zero(currency),
        gst: BTreeMap::new(),
        levies: BTreeMap::new(),
    }
}
