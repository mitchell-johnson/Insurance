//! Premium for a policy period. One code path for quotes, issue, billing,
//! anniversaries and retrospective adjustments.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_money::{round_to_money, Dec, MoneyError, RoundingMode};
use ic_product::rating::{cover_premium, modal_premium, ModalPremium, RatingError, RatingFactors};
use ic_product::schedule::{age_on, anniversary, policy_year};
use ic_product::{CoverDefinition, PremiumType, ProductDefinition};

use crate::catalog::Catalog;
use crate::policy::{CoverStatus, LifeFactors, LifeRecord, Policy};
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

fn factors_on(life: &LifeRecord, on: NaiveDate) -> Result<&LifeFactors, PremiumError> {
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
        for layer in cover.layers.iter().filter(|l| l.in_force_on(period_start)) {
            *work += 1;
            let life = policy.lives.get(&layer.life).ok_or_else(|| PremiumError::UnknownLife(layer.life.clone()))?;
            let f = factors_on(life, period_start)?;
            let dob = f.date_of_birth.ok_or_else(|| PremiumError::MissingFactor(life.id.clone(), "date_of_birth"))?;
            let age = match def.rating.premium_type {
                PremiumType::Stepped | PremiumType::GroupUnitRate => age_on(dob, rated_on),
                PremiumType::Level => age_on(dob, layer.start),
            };
            let rf = RatingFactors {
                age,
                sex: f.sex,
                smoker: f.smoker,
                category: f.category.clone(),
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
    let salary = f.salary_minor.ok_or_else(|| PremiumError::MissingFactor(life.clone(), "salary"))?;
    let si = round_to_money(Dec::from(salary) / Dec::from(100i64) * cat.salary_multiple.get(), currency, rounding)?.minor;
    let mut si = si.min(cat.max_sum_insured_minor);
    if let Some(aal) = def.automatic_acceptance_limit_minor {
        si = si.min(aal);
    }
    Ok(si)
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
            loadings: vec![],
        };
        annual.insert(cover.code.clone(), cover_premium(def, table, &rf, si)?);
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
