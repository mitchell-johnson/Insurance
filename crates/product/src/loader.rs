//! Load Starlark configuration into typed, validated definitions.

use std::collections::BTreeSet;

use ic_money::Dec;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

use crate::{Benefit, Definition, Feature, PremiumType, ProductDefinition, RateTable};

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
        if let Some(g) = &c.gst_rate {
            if g.get() < Dec::from(0i64) || g.get() >= Dec::from(1i64) {
                return Err(invalid(file, format!("cover {}: gst_rate must be a fraction", c.code)));
            }
        }
        if c.max_sum_insured_minor.is_some_and(|m| m < c.min_sum_insured_minor) || c.min_sum_insured_minor < 0 {
            return Err(invalid(file, format!("cover {}: sum insured limits are inconsistent", c.code)));
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
