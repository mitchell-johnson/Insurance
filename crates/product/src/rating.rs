//! The premium calculation: a pure function of the rating factors, the rate
//! table version and the sum insured. The same function runs at quote, issue,
//! anniversary and retrospective adjustment.

use std::collections::BTreeMap;

use ic_money::{round_to_money, Dec, DecStr, Money, MoneyError};
use serde::{Deserialize, Serialize};

use crate::{CoverDefinition, Feature, Frequency, ProductDefinition, RateBasis, RateTable, Sex};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RatingError {
    #[error("no rate in table {table} v{version} for the rating factors given")]
    NoRate { table: String, version: u32 },
    #[error("sum insured outside the cover's limits")]
    SumInsuredOutOfRange,
    #[error("frequency not offered by the product")]
    FrequencyNotOffered,
    #[error("product has no billing feature")]
    NoBilling,
    #[error("the cover does not offer that choice of excess")]
    ExcessOption,
    #[error("unknown cover {0}")]
    UnknownCover(String),
    #[error(transparent)]
    Money(#[from] MoneyError),
}

/// Rating factors for one life on one cover layer. Loadings come from the
/// underwriting decision as state on the layer, never from the product.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RatingFactors {
    pub age: u32,
    #[serde(default)]
    pub sex: Option<Sex>,
    #[serde(default)]
    pub smoker: Option<bool>,
    /// Group scheme benefit category.
    #[serde(default)]
    pub category: Option<String>,
    /// Occupation class, where the life has one.
    #[serde(default)]
    pub occupation_class: Option<String>,
    /// The excess level chosen, where the cover offers a choice.
    #[serde(default)]
    pub excess_option: Option<String>,
    /// Loadings as fractions of the base premium, for example `0.50`.
    #[serde(default)]
    pub loadings: Vec<DecStr>,
}

fn lookup(table: &RateTable, f: &RatingFactors) -> Result<Dec, RatingError> {
    table
        .rows
        .iter()
        .find(|r| {
            f.age >= r.age_from
                && f.age <= r.age_to
                && (r.sex.is_none() || r.sex == f.sex)
                && (r.smoker.is_none() || r.smoker == f.smoker)
                && (r.category.is_none() || r.category == f.category)
                && (r.occupation_class.is_none() || r.occupation_class == f.occupation_class)
        })
        .map(|r| r.annual_rate.get())
        .ok_or_else(|| RatingError::NoRate {
            table: table.id.clone(),
            version: table.version,
        })
}

/// The cover's limits apply to a life's total sum insured on the cover, not
/// to each layer: an indexation step is far smaller than the minimum.
pub fn check_sum_insured(cover: &CoverDefinition, total_minor: i64) -> Result<(), RatingError> {
    if total_minor < cover.min_sum_insured_minor || cover.max_sum_insured_minor.is_some_and(|m| total_minor > m) {
        return Err(RatingError::SumInsuredOutOfRange);
    }
    Ok(())
}

/// Annual premium for one layer of one cover, in major currency units, before
/// discounts, modal loading, fees and tax. Not yet rounded.
pub fn cover_premium(cover: &CoverDefinition, table: &RateTable, f: &RatingFactors, sum_insured_minor: i64) -> Result<Dec, RatingError> {
    let rate = lookup(table, f)?;
    let base = match cover.rating.basis {
        // minor units -> major units -> thousands
        RateBasis::PerThousand => Dec::from(sum_insured_minor) / Dec::from(100_000i64) * rate,
        RateBasis::PerLife => rate,
    };
    let mut factor = Dec::from(1i64);
    for l in &f.loadings {
        factor += l.get();
    }
    // The excess the customer chose scales the cover's price, loadings included.
    let excess = match (&f.excess_option, cover.excess_options.is_empty()) {
        (None, true) => Dec::from(1i64),
        (Some(code), false) => cover.excess_option(code).ok_or(RatingError::ExcessOption)?.premium_factor.get(),
        _ => return Err(RatingError::ExcessOption),
    };
    Ok(base * factor * excess)
}

/// One period's bill, already rounded to cents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModalPremium {
    /// Premium by cover code, excluding tax and levies.
    pub by_cover: BTreeMap<String, Money>,
    pub policy_fee: Money,
    /// GST by cover code, where the cover is taxable.
    pub gst: BTreeMap<String, Money>,
    /// Levies by levy code.
    pub levies: BTreeMap<String, Money>,
}

impl ModalPremium {
    /// Premium and fee, excluding tax and levies: what is earned.
    pub fn net(&self) -> i64 {
        self.by_cover.values().map(|m| m.minor).sum::<i64>() + self.policy_fee.minor
    }
    /// Everything the policyholder owes for the period.
    pub fn total(&self) -> i64 {
        self.net() + self.gst.values().map(|m| m.minor).sum::<i64>() + self.levies.values().map(|m| m.minor).sum::<i64>()
    }
}

/// Turn annual premiums per cover into one period's bill: discounts in their
/// defined order, the frequency's modal loading, then one rounding to cents
/// per line at the product's rounding mode.
pub fn modal_premium(product: &ProductDefinition, annual_by_cover: &BTreeMap<String, Dec>, frequency: Frequency) -> Result<ModalPremium, RatingError> {
    let (fee_annual, loading) = product
        .features
        .iter()
        .find_map(|f| match f {
            Feature::Billing {
                frequencies,
                policy_fee_annual,
                modal_loading,
            } => Some(if frequencies.contains(&frequency) {
                Ok((policy_fee_annual.get(), modal_loading.get(frequency.key()).map_or(Dec::from(0i64), DecStr::get)))
            } else {
                Err(RatingError::FrequencyNotOffered)
            }),
            _ => None,
        })
        .ok_or(RatingError::NoBilling)??;

    let mut discount = Dec::from(1i64);
    for f in &product.features {
        if let Feature::MultiCoverDiscount { min_covers, percent } = f {
            if annual_by_cover.len() as u32 >= *min_covers {
                discount *= Dec::from(1i64) - percent.get() / Dec::from(100i64);
            }
        }
    }
    let periods = Dec::from(frequency.periods_per_year());
    let modal = (Dec::from(1i64) + loading) / periods;
    let cur = product.currency;
    let mode = product.rounding;

    let mut out = ModalPremium {
        by_cover: BTreeMap::new(),
        policy_fee: round_to_money(fee_annual / periods, cur, mode)?,
        gst: BTreeMap::new(),
        levies: BTreeMap::new(),
    };
    for (code, annual) in annual_by_cover {
        let cover = product.cover(code).ok_or_else(|| RatingError::UnknownCover(code.clone()))?;
        let amount = *annual * discount * modal;
        let premium = round_to_money(amount, cur, mode)?;
        if let Some(rate) = &cover.gst_rate {
            let gst = round_to_money(premium.to_dec() * rate.get(), cur, mode)?;
            if !gst.is_zero() {
                out.gst.insert(code.clone(), gst);
            }
        }
        for levy in &cover.levies {
            let l = round_to_money(premium.to_dec() * levy.rate.get(), cur, mode)?;
            let e = out.levies.entry(levy.code.clone()).or_insert(Money::zero(cur));
            *e = e.checked_add(l)?;
        }
        out.by_cover.insert(code.clone(), premium);
    }
    Ok(out)
}
