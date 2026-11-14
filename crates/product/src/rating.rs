//! The premium calculation: a pure function of the rating factors, the rate
//! table version and the sum insured. The same function runs at quote, issue,
//! anniversary and retrospective adjustment.

use std::collections::BTreeMap;

use ic_money::{round_to_money, Dec, DecStr, Money, MoneyError};
use serde::{Deserialize, Serialize};

use crate::discount::{self, Discount, DiscountBasis, DiscountCondition, DiscountStep, Limits, Line, PricingState};
use crate::{CoverDefinition, Feature, Frequency, ProductDefinition, RateBasis, RateTable, Sex, UnmatchedSexRating};

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
    #[error("the cover does not offer that choice of excess or options")]
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
    /// The option chosen from the cover's menu (an excess level, or a
    /// choice on each of its axes), where the cover offers one.
    #[serde(default)]
    pub excess_option: Option<String>,
    /// Loadings as fractions of the base premium, for example `0.50`.
    #[serde(default)]
    pub loadings: Vec<DecStr>,
}

fn row_rate(table: &RateTable, f: &RatingFactors, sex: Option<Sex>) -> Option<Dec> {
    table
        .rows
        .iter()
        .find(|r| {
            f.age >= r.age_from
                && f.age <= r.age_to
                && (r.sex.is_none() || r.sex == sex)
                && (r.smoker.is_none() || r.smoker == f.smoker)
                && (r.category.is_none() || r.category == f.category)
                && (r.occupation_class.is_none() || r.occupation_class == f.occupation_class)
        })
        .map(|r| r.annual_rate.get())
}

/// The table's rate for a life. A life whose rating sex is not recorded is
/// first matched against rows that do not rate by sex; where the table has
/// none for its other facts, it is rated from the female and male rates on
/// the product's basis. A rate is never guessed from one of the two alone.
fn lookup(table: &RateTable, f: &RatingFactors, unmatched: &UnmatchedSexRating) -> Result<Dec, RatingError> {
    let found = row_rate(table, f, f.sex).or_else(|| {
        if f.sex.is_some() {
            return None;
        }
        let female = row_rate(table, f, Some(Sex::Female))?;
        let male = row_rate(table, f, Some(Sex::Male))?;
        Some(match unmatched {
            UnmatchedSexRating::LowerOf => female.min(male),
            UnmatchedSexRating::Blended { female_share } => female * female_share.get() + male * (Dec::from(1i64) - female_share.get()),
        })
    });
    found.ok_or_else(|| RatingError::NoRate {
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
    let rate = lookup(table, f, &cover.rating.unmatched_sex_rating)?;
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
    let excess = match (&f.excess_option, cover.has_options()) {
        (None, false) => Dec::from(1i64),
        (Some(code), true) => cover.option(code).map_err(|_| RatingError::ExcessOption)?.premium_factor,
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
    /// Premium by cover code before discounts, where any discount applied.
    /// Less the discount lines it is the premium by cover above, to the cent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gross_by_cover: BTreeMap<String, Money>,
    /// Each discount that applied, in the product's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discounts: Vec<DiscountLine>,
}

/// One discount on a period's bill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscountLine {
    pub code: String,
    pub name: String,
    pub posting_type: String,
    /// What it takes off the period's premium. Never negative.
    pub amount: Money,
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

/// One period's bill for a policy on one life with no entitlements held.
pub fn modal_premium(product: &ProductDefinition, annual_by_cover: &BTreeMap<String, Dec>, frequency: Frequency) -> Result<ModalPremium, RatingError> {
    let state = PricingState {
        covers_elsewhere: 0,
        covers: annual_by_cover.len() as u32,
        lives: 1,
        frequency,
        entitlements: &[],
        overrides: &[],
        sums_insured: &BTreeMap::new(),
    };
    modal_premium_with(product, annual_by_cover, &state)
}

/// One period's bill from annual premiums by cover, with every cover on one
/// unnamed life: an entitlement reaches it only where the policy holds it.
pub fn modal_premium_with(product: &ProductDefinition, annual_by_cover: &BTreeMap<String, Dec>, state: &PricingState<'_>) -> Result<ModalPremium, RatingError> {
    let lines = annual_by_cover
        .iter()
        .map(|(cover, annual)| {
            let line = Line {
                cover: cover.clone(),
                life: String::new(),
            };
            (line, *annual)
        })
        .collect();
    modal_premium_by_line(product, &lines, state)
}

/// Turn annual premiums per cover and life into one period's bill: the
/// product's discounts in their defined order, the frequency's modal
/// loading, then one rounding to cents per line at the product's rounding
/// mode. Each discount that applied is its own line, and the last of them
/// absorbs the rounding, so the premium before discounts less the lines is
/// the net to the cent.
pub fn modal_premium_by_line(product: &ProductDefinition, annual: &BTreeMap<Line, Dec>, state: &PricingState<'_>) -> Result<ModalPremium, RatingError> {
    let by_cover = |lines: &BTreeMap<Line, Dec>| {
        let mut out: BTreeMap<String, Dec> = BTreeMap::new();
        for (line, amount) in lines {
            *out.entry(line.cover.clone()).or_insert(Dec::from(0i64)) += *amount;
        }
        out
    };
    let annual_by_cover = &by_cover(annual);
    let frequency = state.frequency;
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

    // The chain: the older multi-cover feature first, then the product's own.
    let mut chain: Vec<DiscountStep> = Vec::new();
    let mut limits = Limits::default();
    for f in &product.features {
        match f {
            Feature::MultiCoverDiscount { min_covers, percent } => chain.push(DiscountStep::One {
                discount: Discount {
                    code: "multi-cover".to_string(),
                    name: "Multi-cover discount".to_string(),
                    posting_type: "discount_multi_cover".to_string(),
                    covers: vec![],
                    basis: DiscountBasis::Percent { percent: percent.clone() },
                    condition: DiscountCondition::MinCovers { count: *min_covers },
                    max_annual: None,
                },
            }),
            Feature::Discounts {
                chain: steps,
                max_total_percent,
                min_net_annual,
                ..
            } => {
                chain.extend(steps.iter().cloned());
                limits = Limits {
                    max_total_percent: max_total_percent.as_ref(),
                    min_net_annual: min_net_annual.as_ref(),
                };
            }
            _ => {}
        }
    }
    let (left, applied) = discount::apply(&chain, limits, annual, state);
    let net_annual = by_cover(&left);
    let periods = Dec::from(frequency.periods_per_year());
    let modal = (Dec::from(1i64) + loading) / periods;
    let cur = product.currency;
    let mode = product.rounding;

    let mut out = ModalPremium {
        by_cover: BTreeMap::new(),
        policy_fee: round_to_money(fee_annual / periods, cur, mode)?,
        gst: BTreeMap::new(),
        levies: BTreeMap::new(),
        gross_by_cover: BTreeMap::new(),
        discounts: Vec::new(),
    };
    for (code, annual) in &net_annual {
        let cover = product.cover(code).ok_or_else(|| RatingError::UnknownCover(code.clone()))?;
        let amount = *annual * modal;
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
    if !applied.is_empty() {
        let mut gross = 0i64;
        for (code, annual) in annual_by_cover {
            let line = round_to_money(*annual * modal, cur, mode)?;
            gross += line.minor;
            out.gross_by_cover.insert(code.clone(), line);
        }
        for d in &applied {
            out.discounts.push(DiscountLine {
                code: d.code.clone(),
                name: d.name.clone(),
                posting_type: d.posting_type.clone(),
                amount: round_to_money(d.total() * modal, cur, mode)?,
            });
        }
        // Rounding once: the last line that can carry it absorbs the
        // difference, so the lines sum to the net.
        let net: i64 = out.by_cover.values().map(|m| m.minor).sum();
        let odd = gross - net - out.discounts.iter().map(|d| d.amount.minor).sum::<i64>();
        match out.discounts.iter_mut().rev().find(|d| d.amount.minor + odd >= 0) {
            Some(last) => last.amount = Money::new(last.amount.minor + odd, cur),
            // No discount is large enough to carry it: the base line does.
            None => {
                if let Some(first) = out.gross_by_cover.values_mut().next() {
                    *first = Money::new(first.minor - odd, cur);
                }
            }
        }
        out.discounts.retain(|d| !d.amount.is_zero());
    }
    Ok(out)
}
