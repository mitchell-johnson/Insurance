//! The validation suite for actuarial tables.
//!
//! Every rate table the platform uses is a versioned file with a declared
//! schema. Before a version can be activated it is checked here: against
//! its schema, for shape, ranges and precision, for the patterns a table of
//! its kind must show (mortality rising with age, no implausible jump
//! between adjacent ages, smokers never cheaper than non-smokers), against
//! the version it replaces, and against the tables it must line up with.
//! A blocking finding stops activation and goes back to the actuary.
//!
//! The adjacent-age checks are the control that would have caught the
//! mis-keyed cell of the design addendum's third decision: one cell a
//! factor of ten out passes a range check and fails these.

use std::collections::{BTreeMap, BTreeSet};

use ic_money::DecStr;
use ic_product::{RateRow, RateTable, Sex};
use serde::{Deserialize, Serialize};

/// Rates are compared as integers at nine decimal places.
const SCALE_DP: u32 = 9;

/// What a table of one kind must look like.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableSchema {
    /// For example `mortality_per_mille`.
    pub kind: String,
    /// Version of these rules. Stored on the report.
    pub version: u32,
    pub by_sex: bool,
    pub by_smoker: bool,
    /// The rating categories every table must carry. Empty when the table
    /// is not rated by category.
    #[serde(default)]
    pub categories: Vec<String>,
    /// The ages every key must cover, without gap or overlap. `None` when
    /// the table is not rated by age.
    #[serde(default)]
    pub ages: Option<(u32, u32)>,
    pub rate_min: DecStr,
    pub rate_max: DecStr,
    /// Rates carry at most this many decimal places, so two systems
    /// computing from the same table agree.
    pub decimal_places: u32,
    /// The rate never falls from one age band to the next.
    #[serde(default)]
    pub rising_with_age: bool,
    /// Smoothness: a band's rate is at most this multiple of the band before.
    #[serde(default)]
    pub max_age_step: Option<DecStr>,
    /// A smoker's rate is never below the non-smoker's for the same cell.
    #[serde(default)]
    pub smoker_not_below_non_smoker: bool,
    /// Cells that moved by more than this percentage since the previous
    /// version are listed for review.
    pub change_review_pct: u32,
    /// A cell that moved by more than this factor, up or down, blocks.
    pub change_block_factor: u32,
}

/// Where a table came from. It travels with the table and is stored on its activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub model: String,
    pub model_version: String,
    pub basis_report: String,
    pub author: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Manifest,
    Schema,
    Shape,
    Range,
    Precision,
    Monotonicity,
    Smoothness,
    SmokerLoading,
    Version,
    Change,
    CrossTable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Listed for the actuary to confirm on the dry-run report.
    Review,
    /// Stops activation.
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub check: Check,
    pub severity: Severity,
    /// The cell or key the finding is about, empty for the table as a whole.
    pub cell: String,
    pub message: String,
}

/// The result of validating one table version. Stored with the activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub table: String,
    pub version: u32,
    pub content_hash: String,
    pub schema_kind: String,
    pub schema_version: u32,
    pub manifest: Manifest,
    /// The version it was compared with.
    pub previous_version: Option<u32>,
    pub cells: usize,
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    /// Whether the table may go forward to dry run and approval.
    pub fn passed(&self) -> bool {
        self.findings.iter().all(|f| f.severity != Severity::Blocking)
    }

    pub fn blocking(&self) -> Vec<&Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Blocking).collect()
    }
}

/// The rating cell a row belongs to, apart from its age band.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    sex: Option<Sex>,
    smoker: Option<bool>,
    category: Option<String>,
}

impl Key {
    fn of(row: &RateRow) -> Self {
        Self {
            sex: row.sex,
            smoker: row.smoker,
            category: row.category.clone(),
        }
    }

    fn label(&self) -> String {
        let mut parts = Vec::new();
        match self.sex {
            Some(Sex::Female) => parts.push("female".to_string()),
            Some(Sex::Male) => parts.push("male".to_string()),
            None => {}
        }
        match self.smoker {
            Some(true) => parts.push("smoker".to_string()),
            Some(false) => parts.push("non-smoker".to_string()),
            None => {}
        }
        if let Some(category) = &self.category {
            parts.push(category.clone());
        }
        if parts.is_empty() {
            "all".to_string()
        } else {
            parts.join(" ")
        }
    }
}

fn cell(key: &Key, row: &RateRow, by_age: bool) -> String {
    if by_age {
        format!("{} age {}-{}", key.label(), row.age_from, row.age_to)
    } else {
        key.label()
    }
}

/// A plain decimal as an integer at nine places, with the places it was
/// written to. `None` for an exponent, a sign, or more than nine places.
fn scaled(text: &str) -> Option<(i128, u32)> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || (!fraction.is_empty() && !digits(fraction)) || fraction.len() > SCALE_DP as usize || whole.len() > 20 {
        return None;
    }
    let mut value: i128 = whole.parse().ok()?;
    value *= 10i128.pow(SCALE_DP);
    if !fraction.is_empty() {
        let part: i128 = fraction.parse().ok()?;
        value += part * 10i128.pow(SCALE_DP - fraction.len() as u32);
    }
    Some((value, fraction.len() as u32))
}

struct Findings(Vec<Finding>);

impl Findings {
    fn add(&mut self, check: Check, severity: Severity, cell: impl Into<String>, message: impl Into<String>) {
        self.0.push(Finding {
            check,
            severity,
            cell: cell.into(),
            message: message.into(),
        });
    }

    fn block(&mut self, check: Check, cell: impl Into<String>, message: impl Into<String>) {
        self.add(check, Severity::Blocking, cell, message);
    }
}

/// The keys a schema requires: every combination of its dimensions.
fn required_keys(schema: &TableSchema) -> BTreeSet<Key> {
    let sexes: Vec<Option<Sex>> = if schema.by_sex {
        vec![Some(Sex::Female), Some(Sex::Male)]
    } else {
        vec![None]
    };
    let smokers: Vec<Option<bool>> = if schema.by_smoker { vec![Some(false), Some(true)] } else { vec![None] };
    let categories: Vec<Option<String>> = if schema.categories.is_empty() {
        vec![None]
    } else {
        schema.categories.iter().cloned().map(Some).collect()
    };
    let mut keys = BTreeSet::new();
    for sex in &sexes {
        for smoker in &smokers {
            for category in &categories {
                keys.insert(Key {
                    sex: *sex,
                    smoker: *smoker,
                    category: category.clone(),
                });
            }
        }
    }
    keys
}

/// Validate one table version against its schema and, when there is one,
/// the version it replaces.
pub fn validate(table: &RateTable, schema: &TableSchema, manifest: &Manifest, previous: Option<&RateTable>) -> ValidationReport {
    let mut f = Findings(Vec::new());
    for (name, value) in [
        ("model", &manifest.model),
        ("model_version", &manifest.model_version),
        ("basis_report", &manifest.basis_report),
        ("author", &manifest.author),
    ] {
        if value.trim().is_empty() {
            f.block(Check::Manifest, "", format!("the manifest does not name the {name}"));
        }
    }

    let by_age = schema.ages.is_some();
    let bounds = scaled(schema.rate_min.as_str()).zip(scaled(schema.rate_max.as_str()));
    if bounds.is_none() {
        f.block(Check::Schema, "", "the schema's rate bounds are not plain decimals");
    }
    let max_step = schema.max_age_step.as_ref().map(|s| scaled(s.as_str()));
    if max_step == Some(None) {
        f.block(Check::Schema, "", "the schema's largest age step is not a plain decimal");
    }

    // Each row against the schema, its range and its precision.
    let required = required_keys(schema);
    let mut by_key: BTreeMap<Key, Vec<(&RateRow, i128)>> = BTreeMap::new();
    for row in &table.rows {
        let key = Key::of(row);
        let at = cell(&key, row, by_age);
        if !required.contains(&key) {
            f.block(Check::Schema, &at, "the schema has no such rating cell");
            continue;
        }
        if !by_age && (row.age_from, row.age_to) != (0, 150) {
            f.block(Check::Schema, &at, "the table is not rated by age but the row carries an age band");
            continue;
        }
        let Some((rate, places)) = scaled(row.annual_rate.as_str()) else {
            f.block(
                Check::Precision,
                &at,
                format!("rate `{}` is not a plain decimal of at most {SCALE_DP} places", row.annual_rate.as_str()),
            );
            continue;
        };
        if places > schema.decimal_places {
            f.block(
                Check::Precision,
                &at,
                format!(
                    "rate {} has {places} decimal places; the schema allows {}",
                    row.annual_rate.as_str(),
                    schema.decimal_places
                ),
            );
        }
        if let Some(((min, _), (max, _))) = bounds {
            if rate < min || rate > max {
                f.block(
                    Check::Range,
                    &at,
                    format!(
                        "rate {} is outside {} to {}",
                        row.annual_rate.as_str(),
                        schema.rate_min.as_str(),
                        schema.rate_max.as_str()
                    ),
                );
            }
        }
        by_key.entry(key).or_default().push((row, rate));
    }

    // Shape: every key present, and its age bands cover the range once.
    for key in &required {
        let Some(rows) = by_key.get_mut(key) else {
            f.block(Check::Shape, key.label(), "no rows for this rating cell");
            continue;
        };
        rows.sort_by_key(|(row, _)| (row.age_from, row.age_to));
        let Some((first, last)) = schema.ages else {
            if rows.len() > 1 {
                f.block(Check::Shape, key.label(), format!("{} rows for one rating cell", rows.len()));
            }
            continue;
        };
        let mut next = first;
        for (row, _) in rows.iter() {
            let at = cell(key, row, true);
            if row.age_from > next {
                f.block(Check::Shape, &at, format!("ages {next} to {} are not covered", row.age_from - 1));
            } else if row.age_from < next {
                f.block(Check::Shape, &at, format!("overlaps the band before it: age {} is rated twice", row.age_from));
            }
            next = next.max(row.age_to.saturating_add(1));
        }
        if next <= last {
            f.block(Check::Shape, key.label(), format!("ages {next} to {last} are not covered"));
        } else if next > last.saturating_add(1) {
            f.block(Check::Shape, key.label(), format!("rates run past the schema's last age {last}"));
        }
    }

    // Adjacent ages: the rate rises, and not by an implausible step.
    for (key, rows) in &by_key {
        for pair in rows.windows(2) {
            let ((before, low), (row, high)) = (pair[0], pair[1]);
            let at = cell(key, row, true);
            if schema.rising_with_age && high < low {
                f.block(
                    Check::Monotonicity,
                    &at,
                    format!(
                        "rate {} is below {} for the band before it",
                        row.annual_rate.as_str(),
                        before.annual_rate.as_str()
                    ),
                );
            }
            if let Some(Some((step, _))) = max_step {
                // high / low > step, without dividing.
                if high.saturating_mul(10i128.pow(SCALE_DP)) > low.saturating_mul(step) {
                    f.block(
                        Check::Smoothness,
                        &at,
                        format!(
                            "rate {} is more than {} times the {} of the band before it",
                            row.annual_rate.as_str(),
                            schema.max_age_step.as_ref().map_or("", |s| s.as_str()),
                            before.annual_rate.as_str()
                        ),
                    );
                }
            }
        }
    }

    // A smoker is never rated below a non-smoker of the same sex, category and age.
    if schema.smoker_not_below_non_smoker && schema.by_smoker {
        for (key, rows) in by_key.iter().filter(|(k, _)| k.smoker == Some(true)) {
            let non_smoker = Key {
                smoker: Some(false),
                ..key.clone()
            };
            for (row, rate) in rows {
                let matching = by_key
                    .get(&non_smoker)
                    .and_then(|n| n.iter().find(|(r, _)| (r.age_from, r.age_to) == (row.age_from, row.age_to)));
                if let Some((other, base)) = matching {
                    if rate < base {
                        f.block(
                            Check::SmokerLoading,
                            cell(key, row, by_age),
                            format!(
                                "smoker rate {} is below the non-smoker rate {}",
                                row.annual_rate.as_str(),
                                other.annual_rate.as_str()
                            ),
                        );
                    }
                }
            }
        }
    }

    if let Some(previous) = previous {
        compare(table, previous, schema, by_age, &mut f);
    }

    ValidationReport {
        table: table.id.clone(),
        version: table.version,
        content_hash: table.content_hash.clone(),
        schema_kind: schema.kind.clone(),
        schema_version: schema.version,
        manifest: manifest.clone(),
        previous_version: previous.map(|p| p.version),
        cells: table.rows.len(),
        findings: f.0,
    }
}

/// Cell by cell against the version being replaced. A change that moves
/// every cell a little is probably intended. One cell moving by a factor of
/// ten is probably a typo.
fn compare(table: &RateTable, previous: &RateTable, schema: &TableSchema, by_age: bool, f: &mut Findings) {
    if previous.id != table.id {
        f.block(Check::Version, "", format!("compared with {}, which is a different table", previous.id));
        return;
    }
    if table.version != previous.version + 1 {
        f.block(
            Check::Version,
            "",
            format!("version {} cannot follow version {}", table.version, previous.version),
        );
    }
    if table.effective_from < previous.effective_from {
        f.block(Check::Version, "", "takes effect before the version it replaces");
    }
    type Band = (Key, u32, u32);
    let index = |t: &RateTable| -> BTreeMap<Band, i128> {
        t.rows
            .iter()
            .filter_map(|r| scaled(r.annual_rate.as_str()).map(|(rate, _)| ((Key::of(r), r.age_from, r.age_to), rate)))
            .collect()
    };
    let (new, old) = (index(table), index(previous));
    let factor = i128::from(schema.change_block_factor.max(1));
    for ((key, from, to), rate) in &new {
        let row = RateRow {
            age_from: *from,
            age_to: *to,
            sex: key.sex,
            smoker: key.smoker,
            category: key.category.clone(),
            occupation_class: None,
            annual_rate: schema.rate_min.clone(),
        };
        let at = cell(key, &row, by_age);
        let Some(was) = old.get(&(key.clone(), *from, *to)) else {
            f.add(Check::Change, Severity::Review, at, "new cell: the previous version has no rate for it");
            continue;
        };
        let moved = (rate - was).abs();
        if *rate > was.saturating_mul(factor) || rate.saturating_mul(factor) < *was {
            f.block(Check::Change, at, format!("moved by more than a factor of {factor} from the previous version"));
        } else if moved.saturating_mul(100) > was.saturating_mul(i128::from(schema.change_review_pct)) {
            let pct = if *was == 0 { 0 } else { moved.saturating_mul(100) / was };
            let direction = if rate > was { "up" } else { "down" };
            f.add(Check::Change, Severity::Review, at, format!("{direction} {pct}% from the previous version"));
        }
    }
    let dropped = old.keys().filter(|band| !new.contains_key(*band)).count();
    if dropped > 0 {
        f.add(
            Check::Change,
            Severity::Review,
            "",
            format!("{dropped} cells of the previous version have no rate in this one"),
        );
    }
}

/// Two tables that must line up, such as a product's premium table and its
/// reinsurer rate table: the same rating cells over the same ages.
pub fn cross_check(table: &RateTable, other: &RateTable) -> Vec<Finding> {
    let span = |t: &RateTable| -> BTreeMap<Key, (u32, u32)> {
        let mut spans: BTreeMap<Key, (u32, u32)> = BTreeMap::new();
        for r in &t.rows {
            let s = spans.entry(Key::of(r)).or_insert((r.age_from, r.age_to));
            *s = (s.0.min(r.age_from), s.1.max(r.age_to));
        }
        spans
    };
    let (mine, theirs) = (span(table), span(other));
    let mut f = Findings(Vec::new());
    for (key, ages) in &mine {
        match theirs.get(key) {
            None => f.block(Check::CrossTable, key.label(), format!("{} has no rates for this cell", other.id)),
            Some(other_ages) if other_ages != ages => f.block(
                Check::CrossTable,
                key.label(),
                format!(
                    "{} covers ages {}-{} and {} covers {}-{}",
                    table.id, ages.0, ages.1, other.id, other_ages.0, other_ages.1
                ),
            ),
            Some(_) => {}
        }
    }
    for key in theirs.keys().filter(|k| !mine.contains_key(*k)) {
        f.block(Check::CrossTable, key.label(), format!("{} has no rates for this cell", table.id));
    }
    f.0
}

/// Load the schemas each table ID is validated against. The module must
/// define `TABLE_SCHEMAS`, a dictionary from table ID to schema.
pub fn load_schemas(file: &str, source: &str) -> Result<BTreeMap<String, TableSchema>, crate::config::ConfigError> {
    let schemas: BTreeMap<String, TableSchema> = crate::config::eval(file, source, "TABLE_SCHEMAS")?;
    for (id, schema) in &schemas {
        let plain = |s: &DecStr| scaled(s.as_str()).is_some();
        let step_ok = schema.max_age_step.as_ref().is_none_or(plain);
        let ages_ok = schema.ages.is_none_or(|(first, last)| first <= last);
        if schema.version == 0 || !plain(&schema.rate_min) || !plain(&schema.rate_max) || !step_ok || !ages_ok || schema.change_block_factor < 2 {
            return Err(crate::config::ConfigError::Invalid(format!(
                "schema for {id} needs a version, plain decimal bounds, ages in order and a blocking change factor of at least 2"
            )));
        }
    }
    Ok(schemas)
}
