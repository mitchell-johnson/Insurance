//! Load Starlark treaty modules into typed, validated terms.
//!
//! A treaty is a reviewed file in the repository. It ships worked examples
//! of what it does, and they are run here: a treaty whose examples do not
//! give the answers its author wrote down does not load.

use chrono::NaiveDate;
use ic_core::{LifeId, PolicyId};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

use crate::cession::{cession_on, timeline, Risk, Source};
use crate::treaty::{AggregateTreaty, ReinsurerRateTable, Treaty};
use crate::RiskKey;

const PRELUDE: &str = include_str!("treaty.star");

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
    #[error("{file}: scenario `{scenario}` fails: {error}")]
    Scenario { file: String, scenario: String, error: String },
}

fn content_hash(source: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"ic.config.v1\0");
    // The authoring kit is part of what the source means.
    h.update(Sha256::digest(PRELUDE.as_bytes()));
    h.update(source.as_bytes());
    hex::encode(h.finalize())
}

fn eval<T: DeserializeOwned>(file: &str, source: &str, symbol: &'static str) -> Result<T, LoadError> {
    let sl = |e: starlark::Error| LoadError::Starlark(e.to_string());
    let prelude = AstModule::parse("treaty.star", PRELUDE.to_owned(), &Dialect::Standard).map_err(sl)?;
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
    serde_json::from_value(json).map_err(|e| LoadError::Schema {
        file: file.to_string(),
        error: e.to_string(),
    })
}

fn invalid(file: &str, error: String) -> LoadError {
    LoadError::Invalid { file: file.to_string(), error }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioCover {
    sum_insured_minor: i64,
    start: NaiveDate,
    #[serde(default)]
    end: Option<NaiveDate>,
    /// A scheme member's cover, written by a membership file.
    #[serde(default)]
    member: bool,
    #[serde(default)]
    cover_type: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Expected {
    retained_minor: i64,
    ceded_minor: i64,
    #[serde(default)]
    facultative_minor: i64,
    #[serde(default)]
    unplaced_minor: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    name: String,
    /// Whether the life is linked to a party. `scenario()` always says.
    matched: bool,
    /// The covers on one life.
    covers: Vec<ScenarioCover>,
    /// What each cover cedes once the last of them has started.
    expect: Vec<Expected>,
}

fn run_scenario(treaty: &Treaty, s: &Scenario) -> Result<(), String> {
    if s.covers.is_empty() || s.covers.len() != s.expect.len() {
        return Err("a scenario needs covers and one expectation for each".to_string());
    }
    let reference = treaty.reference();
    let risks: Vec<Risk> = s
        .covers
        .iter()
        .enumerate()
        .map(|(i, c)| Risk {
            key: RiskKey {
                policy: PolicyId(format!("scenario-{i}")),
                cover: "cover".to_string(),
                layer: 1,
                life: LifeId("life".to_string()),
            },
            partition: 0,
            product: treaty.scope[0].product.clone(),
            cover_type: c.cover_type.clone().unwrap_or_else(|| treaty.scope[0].cover_types[0].clone()),
            source: if c.member { Source::Member } else { Source::Layer },
            underwritten: !c.member,
            start: c.start,
            end: c.end,
            amounts: vec![(c.start, c.sum_insured_minor)],
            suspensions: vec![],
            treaties: vec![(c.start, Some(reference.clone()))],
            facultative: vec![],
            excluded: false,
            excess_path: None,
            migrated: None,
        })
        .collect();
    let on = s.covers.iter().map(|c| c.start).max().unwrap_or_default();
    let got = timeline(&risks, |t| (t == &reference).then_some(treaty), s.matched);
    for (i, (risk, want)) in risks.iter().zip(&s.expect).enumerate() {
        let have = got.get(&risk.key).and_then(|segments| cession_on(segments, on)).map_or(
            Expected {
                retained_minor: 0,
                ceded_minor: 0,
                facultative_minor: 0,
                unplaced_minor: 0,
            },
            |c| Expected {
                retained_minor: c.retained_minor,
                ceded_minor: c.treaty_ceded(),
                facultative_minor: c.facultative_ceded(),
                unplaced_minor: c.unplaced_minor,
            },
        );
        if &have != want {
            return Err(format!("cover {} gives {have:?}, the treaty says {want:?}", i + 1));
        }
    }
    Ok(())
}

/// Load a treaty module. The module must define `TREATY` and `SCENARIOS`.
pub fn load_treaty(file: &str, source: &str) -> Result<Treaty, LoadError> {
    let mut treaty: Treaty = eval(file, source, "TREATY")?;
    treaty.content_hash = content_hash(source);
    treaty.validate().map_err(|e| invalid(file, e))?;
    let scenarios: Vec<Scenario> = eval(file, source, "SCENARIOS")?;
    if scenarios.is_empty() {
        return Err(invalid(file, "a treaty ships at least one worked scenario".to_string()));
    }
    for s in &scenarios {
        run_scenario(&treaty, s).map_err(|error| LoadError::Scenario {
            file: file.to_string(),
            scenario: s.name.clone(),
            error,
        })?;
    }
    Ok(treaty)
}

/// Load a catastrophe or stop loss treaty. The module must define
/// `AGGREGATE_TREATY`.
pub fn load_aggregate_treaty(file: &str, source: &str) -> Result<AggregateTreaty, LoadError> {
    let mut treaty: AggregateTreaty = eval(file, source, "AGGREGATE_TREATY")?;
    treaty.content_hash = content_hash(source);
    treaty.validate().map_err(|e| invalid(file, e))?;
    Ok(treaty)
}

/// Load a reinsurer's rate table. The module must define `RATE_TABLE`.
pub fn load_rate_table(file: &str, source: &str) -> Result<ReinsurerRateTable, LoadError> {
    let mut table: ReinsurerRateTable = eval(file, source, "RATE_TABLE")?;
    table.content_hash = content_hash(source);
    table.validate().map_err(|e| invalid(file, e))?;
    Ok(table)
}
