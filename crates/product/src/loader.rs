//! Load Starlark configuration into typed, validated definitions.

use std::collections::BTreeSet;

use ic_money::Dec;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

use crate::discount::{DiscountBasis, DiscountCondition};
use crate::{Benefit, Definition, Feature, PremiumType, ProductDefinition, RateTable, UnmatchedSexRating};

const PRELUDE: &str = include_str!("prelude.star");
const FUND_PRELUDE: &str = include_str!("fund_prelude.star");

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("starlark: {0}")]
    Starlark(String),
    #[error("{file}: `{symbol}` is not defined")]
    MissingSymbol { file: String, symbol: &'static str },
    #[error("{file}: does not match the schema: {error}")]
    Schema { file: String, error: String },
    #[error("{file}: invalid: {error}")]
    Invalid { file: String, error: String },
}

fn content_hash(source: &str) -> String {
    hash_with(PRELUDE, source)
}

fn hash_with(prelude: &str, source: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"ic.config.v1\0");
    // The authoring kit is part of what the source means.
    h.update(Sha256::digest(prelude.as_bytes()));
    h.update(source.as_bytes());
    hex::encode(h.finalize())
}

fn eval<T: DeserializeOwned>(file: &str, source: &str, symbol: &'static str) -> Result<T, LoadError> {
    let json = eval_json(PRELUDE, file, source, symbol)?;
    serde_json::from_value(json).map_err(|e| LoadError::Schema {
        file: file.to_string(),
        error: e.to_string(),
    })
}

/// Load a fund module as data, with the hash of its source. The module must
/// define `FUND`. The investment ledger owns the fund's type and turns this
/// into a definition (`ic_core::invest::load_fund`).
pub fn load_fund_source(file: &str, source: &str) -> Result<(serde_json::Value, String), LoadError> {
    Ok((eval_json(FUND_PRELUDE, file, source, "FUND")?, hash_with(FUND_PRELUDE, source)))
}

fn eval_json(prelude_source: &str, file: &str, source: &str, symbol: &'static str) -> Result<serde_json::Value, LoadError> {
    let sl = |e: starlark::Error| LoadError::Starlark(e.to_string());
    let prelude = AstModule::parse("prelude.star", prelude_source.to_owned(), &Dialect::Standard).map_err(sl)?;
    let ast = AstModule::parse(file, source.to_owned(), &Dialect::Standard).map_err(sl)?;
    let globals = Globals::standard();
    let json = Module::with_temp_heap(|module| -> Result<serde_json::Value, LoadError> {
        {
            let mut ev = Evaluator::new(&module);
            ev.eval_module(prelude, &globals).map_err(sl)?;
            ev.eval_module(ast, &globals).map_err(sl)?;
        }
        let v = module.get(symbol).ok_or_else(|| LoadError::MissingSymbol {
            file: file.to_string(),
            symbol,
        })?;
        v.to_json_value().map_err(|e| LoadError::Starlark(e.to_string()))
    })?;
    Ok(json)
}

fn invalid(file: &str, error: impl Into<String>) -> LoadError {
    LoadError::Invalid {
        file: file.to_string(),
        error: error.into(),
    }
}

/// Load a product module. The module must define `PRODUCT`.
pub fn load_product(file: &str, source: &str) -> Result<ProductDefinition, LoadError> {
    let mut p: ProductDefinition = eval(file, source, "PRODUCT")?;
    p.content_hash = content_hash(source);
    validate_product(file, &mut p)?;
    Ok(p)
}

fn validate_product(file: &str, p: &mut ProductDefinition) -> Result<(), LoadError> {
    if p.code.is_empty() || p.version == 0 {
        return Err(invalid(file, "product needs a code and a version of at least 1"));
    }
    if p.covers.is_empty() {
        return Err(invalid(file, "product offers no covers"));
    }
    let mut codes = BTreeSet::new();
    for c in &p.covers {
        if !codes.insert(c.code.as_str()) {
            return Err(invalid(file, format!("duplicate cover code {}", c.code)));
        }
        if c.rating.premium_type == PremiumType::Level && c.rating.level_to_age.is_none() {
            return Err(invalid(file, format!("cover {}: level premium needs level_to_age", c.code)));
        }
        if c.rating.premium_type == PremiumType::GroupUnitRate && c.categories.is_empty() {
            return Err(invalid(file, format!("cover {}: group unit rates need benefit categories", c.code)));
        }
        if let UnmatchedSexRating::Blended { female_share } = &c.rating.unmatched_sex_rating {
            if female_share.get() < Dec::from(0i64) || female_share.get() > Dec::from(1i64) {
                return Err(invalid(file, format!("cover {}: the blended share is a fraction between 0 and 1", c.code)));
            }
        }
        if let Some(g) = &c.gst_rate {
            if g.get() < Dec::from(0i64) || g.get() >= Dec::from(1i64) {
                return Err(invalid(file, format!("cover {}: gst_rate must be a fraction", c.code)));
            }
        }
        if c.max_sum_insured_minor.is_some_and(|m| m < c.min_sum_insured_minor) || c.min_sum_insured_minor < 0 {
            return Err(invalid(file, format!("cover {}: sum insured limits are inconsistent", c.code)));
        }
        if c.shared_counter
            .as_ref()
            .is_some_and(|name| name.is_empty() || !p.covers.iter().any(|o| o.code != c.code && o.shared_counter == c.shared_counter))
        {
            return Err(invalid(
                file,
                format!("cover {}: a shared counter is named and shared with another cover", c.code),
            ));
        }
        let lump_sum = |code: &str| p.covers.iter().any(|o| o.code == code && matches!(o.benefit, Benefit::LumpSum { .. }));
        if (c.joint_first_event || !c.reduces.is_empty()) && !lump_sum(&c.code) {
            return Err(invalid(file, format!("cover {}: only a lump sum cover is joint or reduces another", c.code)));
        }
        if c.reduces.iter().any(|other| other == &c.code || !lump_sum(other)) {
            return Err(invalid(
                file,
                format!("cover {}: a cover reduces other lump sum covers of the same product", c.code),
            ));
        }
        let mut options = BTreeSet::new();
        for o in &c.excess_options {
            if o.code.is_empty() || !options.insert(o.code.as_str()) {
                return Err(invalid(file, format!("cover {}: excess options need distinct codes", c.code)));
            }
            if o.excess_minor < 0 || o.premium_factor.get() <= Dec::from(0i64) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: excess option {} needs an excess of zero or more and a premium factor above zero",
                        c.code, o.code
                    ),
                ));
            }
        }
        if let Benefit::InvestmentAccount { funds } = &c.benefit {
            if funds.is_empty() {
                return Err(invalid(file, format!("cover {}: investment account needs a fund menu", c.code)));
            }
        }
    }
    let mut kinds = BTreeSet::new();
    for f in &p.features {
        if !kinds.insert(f.order()) {
            return Err(invalid(file, "a lifecycle feature is declared twice"));
        }
        match f {
            Feature::Billing {
                frequencies,
                policy_fee_annual,
                ..
            } => {
                if frequencies.is_empty() {
                    return Err(invalid(file, "billing offers no frequency"));
                }
                if policy_fee_annual.get() < Dec::from(0i64) {
                    return Err(invalid(file, "policy fee is negative"));
                }
            }
            Feature::Arrears {
                grace_days,
                lapse_days,
                reinstatement_days,
                reinstatement_evidence_free_days,
                ..
            } => {
                if lapse_days < grace_days {
                    return Err(invalid(file, "lapse_days is shorter than grace_days"));
                }
                if reinstatement_evidence_free_days.is_some_and(|free| free > *reinstatement_days) {
                    return Err(invalid(file, "reinstatement_evidence_free_days is longer than reinstatement_days"));
                }
            }
            Feature::MultiCoverDiscount { percent, .. } => {
                if percent.get() < Dec::from(0i64) || percent.get() > Dec::from(100i64) {
                    return Err(invalid(file, "discount percent out of range"));
                }
            }
            Feature::Discounts {
                chain,
                max_total_percent,
                min_net_annual,
                override_single_approver_percent,
                override_max_percent,
            } => {
                let pct = |p: &ic_money::DecStr| p.get() >= Dec::from(0i64) && p.get() <= Dec::from(100i64);
                let amount = |a: &ic_money::DecStr| a.get() >= Dec::from(0i64);
                let limits_ok = override_single_approver_percent.as_ref().is_none_or(pct) && override_max_percent.as_ref().is_none_or(pct);
                if !limits_ok || !max_total_percent.as_ref().is_none_or(pct) || !min_net_annual.as_ref().is_none_or(amount) {
                    return Err(invalid(file, "the cap on all discounts is a percentage and the floor is not negative"));
                }
                let mut codes = BTreeSet::new();
                for d in chain.iter().flat_map(|s| s.discounts()) {
                    if d.code.is_empty() || d.name.is_empty() || d.posting_type.is_empty() || !codes.insert(d.code.as_str()) {
                        return Err(invalid(
                            file,
                            format!("discount {}: a discount has its own code, a name and a posting type", d.code),
                        ));
                    }
                    if d.covers.iter().any(|c| !p.covers.iter().any(|o| &o.code == c)) {
                        return Err(invalid(file, format!("discount {}: it names a cover the product does not have", d.code)));
                    }
                    // A component that would increase the premium is a loading or a fee, never a discount.
                    let never_negative = match &d.basis {
                        DiscountBasis::Percent { percent } => pct(percent),
                        DiscountBasis::FixedAnnual { amount: a } => amount(a),
                        DiscountBasis::Tier { percent_by_tier } => {
                            !percent_by_tier.is_empty() && percent_by_tier.values().all(pct) && matches!(d.condition, DiscountCondition::Entitlement { .. })
                        }
                    };
                    if !never_negative || !d.max_annual.as_ref().is_none_or(amount) {
                        return Err(invalid(
                            file,
                            format!(
                                "discount {}: a discount is between nothing and the whole premium, and a tier needs an entitlement",
                                d.code
                            ),
                        ));
                    }
                    let counted = match &d.condition {
                        DiscountCondition::MinCovers { count } | DiscountCondition::MinLives { count } => *count > 0,
                        DiscountCondition::Frequency { frequencies } => !frequencies.is_empty(),
                        DiscountCondition::Entitlement { code } => !code.is_empty(),
                        DiscountCondition::Always => true,
                    };
                    if !counted {
                        return Err(invalid(file, format!("discount {}: its condition can never be told apart from none", d.code)));
                    }
                }
                if chain.iter().any(|s| s.discounts().is_empty()) {
                    return Err(invalid(file, "a set of discounts has at least one member"));
                }
            }
            Feature::Composition { core, requires, exclusive } => {
                let named = core
                    .iter()
                    .chain(requires.keys())
                    .chain(requires.values().flatten())
                    .chain(exclusive.iter().flatten());
                for code in named {
                    if !codes.contains(code.as_str()) {
                        return Err(invalid(file, format!("composition names cover {code}, which the product does not offer")));
                    }
                }
                if requires.iter().any(|(cover, needed)| needed.contains(cover)) {
                    return Err(invalid(file, "a cover cannot require itself"));
                }
            }
            Feature::Earning { .. } | Feature::Indexation { .. } | Feature::CoolingOff { .. } => {}
        }
    }
    for (needed, name) in [(30u8, "billing"), (40, "earning"), (50, "arrears")] {
        if !kinds.contains(&needed) {
            return Err(invalid(file, format!("product has no {name} feature")));
        }
    }
    // Features run in the engine's defined order, whatever order they were listed in.
    p.features.sort_by_key(Feature::order);
    Ok(())
}

/// Load a rate table module. The module must define `RATE_TABLE`.
pub fn load_rate_table(file: &str, source: &str) -> Result<RateTable, LoadError> {
    let mut t: RateTable = eval(file, source, "RATE_TABLE")?;
    t.content_hash = content_hash(source);
    if t.id.is_empty() || t.version == 0 || t.rows.is_empty() {
        return Err(invalid(file, "rate table needs an id, a version and at least one row"));
    }
    for r in &t.rows {
        if r.age_to < r.age_from || r.annual_rate.get() < Dec::from(0i64) {
            return Err(invalid(file, "rate row has an inverted age band or a negative rate"));
        }
    }
    Ok(t)
}

/// Load a definitions library module. The module must define `DEFINITIONS`.
pub fn load_definitions(file: &str, source: &str) -> Result<Vec<Definition>, LoadError> {
    let mut defs: Vec<Definition> = eval(file, source, "DEFINITIONS")?;
    let mut seen = BTreeSet::new();
    for d in &mut defs {
        if d.text.is_empty() {
            return Err(invalid(file, format!("definition {} has no wording", d.id)));
        }
        if !seen.insert((d.id.clone(), d.version)) {
            return Err(invalid(file, format!("definition {} v{} is declared twice", d.id, d.version)));
        }
        d.text_hash = hex::encode(Sha256::digest(d.text.as_bytes()));
    }
    Ok(defs)
}
