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

use chrono::NaiveDate;
use ic_ledger::PartitionId;
use ic_money::DecStr;
use ic_product::{RateRow, RateTable, Sex};
use serde::{Deserialize, Serialize};

use crate::mart::RatedCover;

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
    /// The occupation classes every table must carry. Empty when the table
    /// is not rated by occupation class.
    #[serde(default)]
    pub occupation_classes: Vec<String>,
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

    /// What the report is known by: the SHA-256 of its JSON.
    pub fn hash(&self) -> String {
        crate::config::content_hash(&serde_json::to_string(self).unwrap_or_default())
    }

    /// The record of this report that the core keeps beside the table, for
    /// the close to match against the activation. A report with a blocking
    /// finding has no record: the table did not pass.
    pub fn record(&self, by: &str) -> Result<ic_core::command::TableValidation, String> {
        if !self.passed() {
            let first = self.blocking().first().map(|f| f.message.clone()).unwrap_or_default();
            return Err(format!("rate table {} version {} did not pass validation: {first}", self.table, self.version));
        }
        Ok(ic_core::command::TableValidation {
            id: self.table.clone(),
            version: self.version,
            content_hash: self.content_hash.clone(),
            report_hash: self.hash(),
            schema_version: self.schema_version,
            by: by.to_string(),
        })
    }
}

/// The rating cell a row belongs to, apart from its age band.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    sex: Option<Sex>,
    smoker: Option<bool>,
    category: Option<String>,
    occupation_class: Option<String>,
}

impl Key {
    fn of(row: &RateRow) -> Self {
        Self {
            sex: row.sex,
            smoker: row.smoker,
            category: row.category.clone(),
            occupation_class: row.occupation_class.clone(),
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
        if let Some(class) = &self.occupation_class {
            parts.push(format!("class {class}"));
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
    let classes: Vec<Option<String>> = if schema.occupation_classes.is_empty() {
        vec![None]
    } else {
        schema.occupation_classes.iter().cloned().map(Some).collect()
    };
    let mut keys = BTreeSet::new();
    for sex in &sexes {
        for smoker in &smokers {
            for category in &categories {
                for class in &classes {
                    keys.insert(Key {
                        sex: *sex,
                        smoker: *smoker,
                        category: category.clone(),
                        occupation_class: class.clone(),
                    });
                }
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
            occupation_class: key.occupation_class.clone(),
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

/// What to price: the cover codes the table rates, how, and the two versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DryRunRequest {
    /// The cover codes rated from this table.
    pub covers: Vec<String>,
    /// The rate is an amount per life, not per thousand of sum insured.
    #[serde(default)]
    pub per_life: bool,
    /// Price on the age when the cover started (level premiums), not the
    /// age on the date asked (stepped premiums).
    #[serde(default)]
    pub age_at_start: bool,
    pub current: RateTable,
    pub proposed: RateTable,
    /// The underwriting loadings on the book's layers, read from the core
    /// for this run by whoever asks. With none, the loaded totals equal the
    /// base totals. They imply health, so the answer gives only totals and
    /// nothing here is kept.
    #[serde(default)]
    pub loadings: Vec<LayerLoading>,
}

/// The loadings on one layer, as the core answers them. They add: the
/// premium is the base times one plus their sum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerLoading {
    pub partition: PartitionId,
    pub policy: String,
    pub cover: String,
    pub layer: u32,
    pub loadings: Vec<DecStr>,
}

/// One rating cell of the book under both versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRunCell {
    pub age_band: String,
    pub sex: String,
    pub smoker: Option<bool>,
    /// Empty where the life has none.
    pub occupation_class: String,
    /// The scheme benefit category, empty where the life has none.
    pub category: String,
    pub covers: u64,
    pub current_annual_minor: i64,
    pub proposed_annual_minor: i64,
}

/// One cover whose premium moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRunChange {
    pub partition: PartitionId,
    pub policy: String,
    pub cover: String,
    pub layer: Option<u32>,
    pub age: u32,
    pub current_annual_minor: i64,
    pub proposed_annual_minor: i64,
}

/// A proposed table version priced against the in-force book beside the
/// version it replaces. Premiums are annual, before loadings, discounts,
/// fees and tax, so the figures show the movement and are not a bill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRun {
    pub as_at: NaiveDate,
    pub current: String,
    pub proposed: String,
    /// Covers priced under both versions.
    pub covers: u64,
    pub current_annual_minor: i64,
    pub proposed_annual_minor: i64,
    pub change_minor: i64,
    /// Priced covers that carry an underwriting loading.
    pub loaded_covers: u64,
    /// The totals with each cover's loadings applied. Before discounts,
    /// fees and tax.
    pub loaded_current_annual_minor: i64,
    pub loaded_proposed_annual_minor: i64,
    /// Loadings given that could not be read as a fraction; their covers
    /// are priced without them.
    pub loadings_unread: u64,
    /// Covers whose life has no age published, so neither version prices them.
    pub no_age: u64,
    /// Covers neither version has a rate for.
    pub no_rate_in_either: u64,
    /// Covers only the proposed version has a rate for.
    pub newly_rated: u64,
    /// Covers priced today that the proposed version has no rate for.
    pub left_without_a_rate: u64,
    /// The first covers of `left_without_a_rate`, as `policy/cover`.
    pub left_without_a_rate_examples: Vec<String>,
    pub cells: Vec<DryRunCell>,
    /// The covers whose premium moves most, largest first.
    pub largest_changes: Vec<DryRunChange>,
    /// Whether the proposed version may go forward to approval.
    pub passed: bool,
}

/// How many covers a dry run names in each of its lists.
pub const DRY_RUN_LISTED: usize = 20;

/// The annual rate for a cover's factors: the first row that matches, as the
/// premium calculation in `ic-product` finds it. A row for a scheme benefit
/// category matches only a member the core has published in that category.
fn rate_for(table: &RateTable, age: u32, c: &RatedCover) -> Option<i128> {
    table
        .rows
        .iter()
        .find(|r| {
            age >= r.age_from
                && age <= r.age_to
                && (r.sex.is_none() || r.sex == c.sex)
                && (r.smoker.is_none() || r.smoker == c.smoker)
                && (r.category.is_none() || r.category == c.category)
                && (r.occupation_class.is_none() || r.occupation_class == c.occupation_class)
        })
        .and_then(|r| scaled(r.annual_rate.as_str()))
        .map(|(rate, _)| rate)
}

/// An annual premium in minor units from a rate at nine decimal places,
/// rounded half up.
fn annual_minor(rate: i128, per_life: bool, sum_insured_minor: i64) -> i64 {
    let one = 10i128.pow(SCALE_DP);
    let (amount, divisor) = if per_life {
        (rate * 100, one)
    } else {
        // minor units of sum insured, per thousand
        (rate * i128::from(sum_insured_minor), one * 1000)
    };
    i64::try_from((amount + divisor / 2) / divisor).unwrap_or(i64::MAX)
}

/// Price the in-force covers under the current and the proposed version.
pub fn dry_run(as_at: NaiveDate, request: &DryRunRequest, book: &[RatedCover]) -> DryRun {
    let name = |t: &RateTable| format!("{} v{}", t.id, t.version);
    let mut run = DryRun {
        as_at,
        current: name(&request.current),
        proposed: name(&request.proposed),
        covers: 0,
        current_annual_minor: 0,
        proposed_annual_minor: 0,
        change_minor: 0,
        loaded_covers: 0,
        loaded_current_annual_minor: 0,
        loaded_proposed_annual_minor: 0,
        loadings_unread: 0,
        no_age: 0,
        no_rate_in_either: 0,
        newly_rated: 0,
        left_without_a_rate: 0,
        left_without_a_rate_examples: Vec::new(),
        cells: Vec::new(),
        largest_changes: Vec::new(),
        passed: true,
    };
    let mut cells: BTreeMap<(u32, String, Option<bool>, String, String), DryRunCell> = BTreeMap::new();
    // One plus the sum of a layer's loadings, at the rate scale.
    let one = 10i128.pow(SCALE_DP);
    let mut factors: BTreeMap<(PartitionId, &str, &str, u32), i128> = BTreeMap::new();
    for l in &request.loadings {
        let sum: Option<i128> = l.loadings.iter().map(|d| scaled(d.as_str()).map(|(v, _)| v)).sum();
        match sum {
            Some(sum) => *factors.entry((l.partition, l.policy.as_str(), l.cover.as_str(), l.layer)).or_insert(one) += sum,
            None => run.loadings_unread += 1,
        }
    }
    let loaded = |base: i64, factor: i128| i64::try_from((i128::from(base) * factor + one / 2) / one).unwrap_or(i64::MAX);
    for c in book {
        let age = if request.age_at_start { c.age_at_start } else { c.age };
        let Some(age) = age else {
            run.no_age += 1;
            continue;
        };
        let (current, proposed) = match (rate_for(&request.current, age, c), rate_for(&request.proposed, age, c)) {
            (Some(current), Some(proposed)) => (current, proposed),
            (None, None) => {
                run.no_rate_in_either += 1;
                continue;
            }
            (None, Some(_)) => {
                run.newly_rated += 1;
                continue;
            }
            (Some(_), None) => {
                run.left_without_a_rate += 1;
                if run.left_without_a_rate_examples.len() < DRY_RUN_LISTED {
                    run.left_without_a_rate_examples.push(format!("{}/{}", c.policy, c.cover));
                }
                continue;
            }
        };
        let current = annual_minor(current, request.per_life, c.sum_insured_minor);
        let proposed = annual_minor(proposed, request.per_life, c.sum_insured_minor);
        run.covers += 1;
        run.current_annual_minor = run.current_annual_minor.saturating_add(current);
        run.proposed_annual_minor = run.proposed_annual_minor.saturating_add(proposed);
        let policy = c.policy.to_string();
        let factor = c.layer.and_then(|layer| factors.get(&(c.partition, policy.as_str(), c.cover.as_str(), layer)));
        run.loaded_covers += u64::from(factor.is_some());
        let factor = factor.copied().unwrap_or(one);
        run.loaded_current_annual_minor = run.loaded_current_annual_minor.saturating_add(loaded(current, factor));
        run.loaded_proposed_annual_minor = run.loaded_proposed_annual_minor.saturating_add(loaded(proposed, factor));
        let sex = crate::mart::sex_name(c.sex).to_string();
        let class = c.occupation_class.clone().unwrap_or_default();
        let category = c.category.clone().unwrap_or_default();
        let key = (age / 5, sex.clone(), c.smoker, class.clone(), category.clone());
        let cell = cells.entry(key).or_insert_with(|| DryRunCell {
            age_band: crate::mart::age_band(Some(age)),
            sex,
            smoker: c.smoker,
            occupation_class: class,
            category,
            covers: 0,
            current_annual_minor: 0,
            proposed_annual_minor: 0,
        });
        cell.covers += 1;
        cell.current_annual_minor = cell.current_annual_minor.saturating_add(current);
        cell.proposed_annual_minor = cell.proposed_annual_minor.saturating_add(proposed);
        if current != proposed {
            run.largest_changes.push(DryRunChange {
                partition: c.partition,
                policy: c.policy.to_string(),
                cover: c.cover.clone(),
                layer: c.layer,
                age,
                current_annual_minor: current,
                proposed_annual_minor: proposed,
            });
        }
    }
    run.change_minor = run.proposed_annual_minor.saturating_sub(run.current_annual_minor);
    run.cells = cells.into_values().collect();
    // A stable sort, so covers that move by the same amount stay in book order.
    run.largest_changes
        .sort_by_key(|m| std::cmp::Reverse(m.proposed_annual_minor.abs_diff(m.current_annual_minor)));
    run.largest_changes.truncate(DRY_RUN_LISTED);
    run.passed = run.left_without_a_rate == 0;
    run
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

/// What the table tool was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRun {
    pub report: ValidationReport,
    /// Who to record the validation as, where recording was asked for.
    pub record_as: Option<String>,
}

/// The table tool's work before anything is sent: read the table, its
/// schema, the manifest and optionally the version it replaces, and
/// validate. Arguments: `<table.star> <schemas.star> <manifest.json>`, then
/// optionally `--previous <table.star>` and `--record <who>`.
pub fn tool(args: &[String], read: impl Fn(&str) -> Result<String, String>) -> Result<ToolRun, String> {
    const USAGE: &str = "usage: ic-finance-tables <table.star> <schemas.star> <manifest.json> [--previous <table.star>] [--record <who>]";
    let [table_file, schemas_file, manifest_file, options @ ..] = args else {
        return Err(USAGE.to_string());
    };
    let (mut previous, mut record_as) = (None, None);
    let mut options = options.iter();
    while let Some(option) = options.next() {
        let value = options.next().ok_or_else(|| USAGE.to_string())?;
        match option.as_str() {
            "--previous" => previous = Some(value),
            "--record" => record_as = Some(value.clone()),
            _ => return Err(USAGE.to_string()),
        }
    }
    let load = |file: &str| ic_product::load_rate_table(file, &read(file)?).map_err(|e| format!("{file}: {e}"));
    let table = load(table_file)?;
    let previous = previous.map(|file| load(file)).transpose()?;
    let schemas = load_schemas(schemas_file, &read(schemas_file)?).map_err(|e| format!("{schemas_file}: {e}"))?;
    let schema = schemas
        .get(&table.id)
        .ok_or_else(|| format!("{schemas_file} declares no schema for table {}", table.id))?;
    let manifest: Manifest = serde_json::from_str(&read(manifest_file)?).map_err(|e| format!("{manifest_file}: {e}"))?;
    Ok(ToolRun {
        report: validate(&table, schema, &manifest, previous.as_ref()),
        record_as,
    })
}
