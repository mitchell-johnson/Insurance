//! Regulatory returns and conduct measures.
//!
//! A return is a versioned definition: lines, each built from account
//! balances, account movements or a conduct measure. The ledger works a
//! return out itself from a closed period, so nobody keys a figure, and
//! stores what was filed for ever. Every line carries its lineage: the
//! accounts behind it as the period closed, and anything posted since for
//! that period.
//!
//! A filed return is never edited. When a later posting restates the
//! period, such as a late-notified death unwinding premium, the return is
//! filed again as a new version with a reason, and the difference can be
//! walked to the backdated entries that caused it.
//!
//! Conduct measures are counts of facts other modules report. A decline
//! that is later overturned stays a decline in the period it was made; the
//! overturn is counted on its own, in the period it happened, so the two
//! are never netted.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::Reject;
use ic_ledger::{BalanceFamily, EntryId, Hash32};
use ic_money::Currency;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::books::FinanceState;
use crate::close::PeriodStatus;
use crate::config::{content_hash, eval, ConfigError};
use crate::period::PeriodId;

/// Most facts one command may carry.
pub const MAX_CONDUCT_FACTS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConductMeasure {
    ClaimAccepted,
    ClaimDeclined,
    /// A decline reversed on review or by a dispute scheme. Never netted
    /// against the declines of the period it was made in.
    DeclineOverturned,
    /// An admitted claim later reversed.
    AdmissionReversed,
    ComplaintReceived,
    ComplaintUpheld,
    /// A policy lapsed for non-payment. Counted from the core's events.
    PolicyLapsed,
    /// A lapsed policy put back in force. Counted from the core's events.
    PolicyReinstated,
    /// New cover written in place of cover given up, declared or matched by
    /// the commission rules, as the distribution module reports it.
    ReplacementWritten,
    /// A replacement match a dispute decided for the adviser: not a
    /// replacement after all. Counted against the period of the match.
    ReplacementOverturned,
}

impl ConductMeasure {
    /// The service that reports the measure.
    fn reporter(self) -> &'static str {
        match self {
            ConductMeasure::ComplaintReceived | ConductMeasure::ComplaintUpheld => "complaints",
            // Only the feed counts these; no caller may report them.
            ConductMeasure::PolicyLapsed | ConductMeasure::PolicyReinstated => "the core's events",
            ConductMeasure::ReplacementWritten | ConductMeasure::ReplacementOverturned => "distribution",
            _ => "claims",
        }
    }
}

/// One thing that happened, with no personal information: an opaque ID, a
/// date and a product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConductFact {
    /// Unique for ever. A fact sent twice is counted once.
    pub id: String,
    pub measure: ConductMeasure,
    pub date: NaiveDate,
    #[serde(default)]
    pub product: Option<String>,
    /// For an overturn or a reversal: the date of the decision it undoes.
    #[serde(default)]
    pub relates_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConductKey {
    period: PeriodId,
    measure: ConductMeasure,
    product: Option<String>,
    /// The period of the decision an overturn or reversal undoes.
    relates_to: Option<PeriodId>,
}

impl ConductKey {
    pub(crate) fn new(period: PeriodId, measure: ConductMeasure, product: Option<String>, relates_to: Option<PeriodId>) -> Self {
        Self {
            period,
            measure,
            product,
            relates_to,
        }
    }
}

/// A period's conduct measures.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ConductReport {
    pub accepted: u64,
    /// Every decline made in the period, whatever happened to it later.
    pub declined: u64,
    /// Overturns that happened in the period, by the period of the decline.
    pub overturned: BTreeMap<PeriodId, u64>,
    /// Admissions reversed in the period, by the period of the admission.
    pub admissions_reversed: BTreeMap<PeriodId, u64>,
    /// Declines made in this period that have been overturned since, in any period.
    pub declines_later_overturned: u64,
    pub complaints_received: u64,
    pub complaints_upheld: u64,
    pub lapsed: u64,
    pub reinstated: u64,
    /// Cover written to replace cover given up.
    #[serde(default)]
    pub replacements: u64,
    /// Replacement matches overturned in the period, by the period of the match.
    #[serde(default)]
    pub replacements_overturned: BTreeMap<PeriodId, u64>,
    /// Claims decisions made in the period and the days each took from
    /// registration, counted from the claims module's facts.
    pub accepted_turnaround: crate::mart::Turnaround,
    pub declined_turnaround: crate::mart::Turnaround,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum LineSource {
    /// Closing balance of the accounts.
    Balance { accounts: Vec<String> },
    /// Movement on the accounts in the period.
    Movement { accounts: Vec<String> },
    /// Count of a conduct measure in the period.
    Conduct { measure: ConductMeasure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineDef {
    pub code: String,
    pub label: String,
    #[serde(flatten)]
    pub source: LineSource,
    /// Report credits as positive, for liabilities and income.
    #[serde(default)]
    pub credit_positive: bool,
    /// One statutory fund only.
    #[serde(default)]
    pub fund: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnDefinition {
    pub id: String,
    pub version: u32,
    pub title: String,
    pub currency: Currency,
    pub lines: Vec<LineDef>,
    /// The first period this version of the mapping is used for. A change
    /// to a line's definition is a new version effective from a period.
    #[serde(default)]
    pub effective_from: Option<PeriodId>,
    /// The last period this version is used for, once a later one replaces it.
    #[serde(default)]
    pub effective_to: Option<PeriodId>,
    #[serde(default)]
    pub content_hash: String,
}

impl ReturnDefinition {
    /// Load a return definition module. The module must define `RETURN`.
    pub fn load(file: &str, source: &str) -> Result<Self, ConfigError> {
        let mut definition: ReturnDefinition = eval(file, source, "RETURN")?;
        definition.content_hash = content_hash(source);
        definition.validate()?;
        Ok(definition)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        if self.id.is_empty() || self.version == 0 || self.lines.is_empty() {
            return invalid("a return needs an ID, a version of at least 1 and at least one line".to_string());
        }
        if matches!((self.effective_from, self.effective_to), (Some(from), Some(to)) if from > to) {
            return invalid("a return's mapping cannot stop being effective before it starts".to_string());
        }
        let mut codes = BTreeSet::new();
        for line in &self.lines {
            if line.code.is_empty() || !codes.insert(line.code.as_str()) {
                return invalid(format!("line `{}` is unnamed or declared twice", line.code));
            }
            match &line.source {
                LineSource::Balance { accounts } | LineSource::Movement { accounts } if accounts.is_empty() => {
                    return invalid(format!("line {} names no accounts", line.code));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether this version of the mapping is the one in effect for a period.
    pub fn in_effect(&self, period: PeriodId) -> bool {
        self.effective_from.is_none_or(|from| period >= from) && self.effective_to.is_none_or(|to| period <= to)
    }
}

/// One line of a filed return with where its figure came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnLine {
    pub code: String,
    pub label: String,
    /// In fine units of the return's currency, or a count.
    pub value: i128,
    /// The figure as the period closed, by account.
    pub as_closed: BTreeMap<String, i128>,
    /// Posted since for this period, by the period it was posted in.
    pub restated: BTreeMap<PeriodId, i128>,
    /// What the line was built from under the definition it was filed on:
    /// the accounts or the measure, and the fund it was limited to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<LineSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fund: Option<String>,
}

/// Where one figure on a filed return came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineLineage {
    pub id: String,
    pub period: PeriodId,
    pub version: u32,
    pub definition: String,
    pub definition_version: u32,
    pub definition_hash: String,
    pub line: ReturnLine,
    /// The closed trial balance the figure was taken from.
    pub trial_balance_hash: Hash32,
    /// The question that gives the same rows from the ledger today.
    pub query: serde_json::Value,
    /// Backdated core entries posted since the period closed that belong to it.
    pub restating_entries: Vec<EntryId>,
    /// The extracts registered as at the period's last day.
    pub extracts: Vec<crate::books::ExtractRecord>,
    pub filed_by: String,
    pub reason: Option<String>,
}

/// A return as filed. Never changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiledReturn {
    pub id: String,
    pub period: PeriodId,
    /// 1 for the first filing, then one more for each restatement.
    pub version: u32,
    pub definition: String,
    pub definition_version: u32,
    pub definition_hash: String,
    pub lines: Vec<ReturnLine>,
    /// The closed trial balance the figures were taken from.
    pub trial_balance_hash: Hash32,
    /// Backdated core entries posted since the period closed that belong to it.
    pub restating_entries: Vec<EntryId>,
    pub hash: Hash32,
    pub filed_by: String,
    /// Why it was restated. `None` on the first filing.
    pub reason: Option<String>,
    pub index: u64,
}

fn reject<T>(code: &str, message: impl Into<String>) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

impl FinanceState {
    /// Count conduct facts. Returns how many were new.
    pub(crate) fn record_conduct(&mut self, caller: &str, facts: Vec<ConductFact>) -> Result<u32, Reject> {
        if facts.len() > MAX_CONDUCT_FACTS {
            return reject("batch", format!("a command carries at most {MAX_CONDUCT_FACTS} facts"));
        }
        let mut fresh: BTreeMap<&str, ConductKey> = BTreeMap::new();
        for fact in &facts {
            if fact.measure.reporter() != caller {
                return reject("caller", format!("{:?} is reported by {}, not {caller}", fact.measure, fact.measure.reporter()));
            }
            let undoes = matches!(
                fact.measure,
                ConductMeasure::DeclineOverturned | ConductMeasure::AdmissionReversed | ConductMeasure::ReplacementOverturned
            );
            if fact.id.is_empty() || undoes != fact.relates_to.is_some() || fact.relates_to.is_some_and(|earlier| earlier > fact.date) {
                return reject(
                    "fact",
                    format!(
                        "fact `{}` needs an ID, and the date of the decision it undoes if and only if it undoes one",
                        fact.id
                    ),
                );
            }
            let period = PeriodId::of(fact.date);
            if self.periods.get(&period).is_some_and(|p| p.status == PeriodStatus::Closed) {
                return reject("period_closed", format!("{period} is closed; fact {} cannot be counted in it", fact.id));
            }
            if !self.conduct_seen.contains_key(&fact.id) {
                fresh.insert(
                    &fact.id,
                    ConductKey {
                        period,
                        measure: fact.measure,
                        product: fact.product.clone(),
                        relates_to: fact.relates_to.map(PeriodId::of),
                    },
                );
            }
        }
        let count = fresh.len() as u32;
        let keys: Vec<(String, ConductKey)> = fresh.into_iter().map(|(id, key)| (id.to_string(), key)).collect();
        for (id, key) in keys {
            self.conduct_seen.insert(id, key.period);
            *self.conduct.entry(key).or_insert(0) += 1;
        }
        Ok(count)
    }

    /// What other modules reported, and what the feed carried.
    fn all_conduct(&self) -> impl Iterator<Item = (&ConductKey, &u64)> {
        self.conduct.iter().chain(self.mart.conduct.iter())
    }

    fn conduct_count(&self, period: PeriodId, measure: ConductMeasure) -> u64 {
        self.all_conduct()
            .filter(|(k, _)| k.period == period && k.measure == measure)
            .map(|(_, n)| n)
            .sum()
    }

    /// A period's conduct measures, for one product or for all.
    pub fn conduct_report(&self, period: PeriodId, product: Option<&str>) -> ConductReport {
        let mut report = ConductReport::default();
        for ((p, measure, prod), t) in &self.mart.turnaround {
            if *p != period || (product.is_some() && prod.as_deref() != product) {
                continue;
            }
            let into = match measure {
                ConductMeasure::ClaimAccepted => &mut report.accepted_turnaround,
                ConductMeasure::ClaimDeclined => &mut report.declined_turnaround,
                _ => continue,
            };
            into.decisions += t.decisions;
            into.total_days += t.total_days;
            into.max_days = into.max_days.max(t.max_days);
        }
        for (key, n) in self.all_conduct().filter(|(k, _)| product.is_none() || k.product.as_deref() == product) {
            if key.measure == ConductMeasure::DeclineOverturned && key.relates_to == Some(period) {
                report.declines_later_overturned += n;
            }
            if key.period != period {
                continue;
            }
            let origin = key.relates_to.unwrap_or(period);
            match key.measure {
                ConductMeasure::ClaimAccepted => report.accepted += n,
                ConductMeasure::ClaimDeclined => report.declined += n,
                ConductMeasure::DeclineOverturned => *report.overturned.entry(origin).or_insert(0) += n,
                ConductMeasure::AdmissionReversed => *report.admissions_reversed.entry(origin).or_insert(0) += n,
                ConductMeasure::ComplaintReceived => report.complaints_received += n,
                ConductMeasure::ComplaintUpheld => report.complaints_upheld += n,
                ConductMeasure::PolicyLapsed => report.lapsed += n,
                ConductMeasure::PolicyReinstated => report.reinstated += n,
                ConductMeasure::ReplacementWritten => report.replacements += n,
                ConductMeasure::ReplacementOverturned => *report.replacements_overturned.entry(origin).or_insert(0) += n,
            }
        }
        report
    }

    /// Work a return out from a closed period as the books stand now.
    fn prepare_return(&self, definition: &ReturnDefinition, period: PeriodId) -> Result<(Vec<ReturnLine>, Hash32), Reject> {
        let Some(closed) = self.periods.get(&period).and_then(|p| p.closed.as_ref()) else {
            return reject("period", format!("{period} is not closed; a return is filed from a closed period"));
        };
        let money = BalanceFamily::Currency(definition.currency);
        let mut lines = Vec::new();
        for def in &definition.lines {
            let in_fund = |fund: Option<&str>| def.fund.is_none() || def.fund.as_deref() == fund;
            let sign = if def.credit_positive { -1 } else { 1 };
            let mut as_closed: BTreeMap<String, i128> = BTreeMap::new();
            let mut restated: BTreeMap<PeriodId, i128> = BTreeMap::new();
            match &def.source {
                LineSource::Conduct { measure } => {
                    lines.push(ReturnLine {
                        code: def.code.clone(),
                        label: def.label.clone(),
                        value: i128::from(self.conduct_count(period, *measure)),
                        as_closed,
                        restated,
                        source: Some(def.source.clone()),
                        fund: None,
                    });
                    continue;
                }
                LineSource::Balance { accounts } => {
                    for row in closed
                        .trial_balance
                        .iter()
                        .filter(|r| r.unit == money && accounts.contains(&r.account) && in_fund(r.fund.as_deref()))
                    {
                        *as_closed.entry(row.account.clone()).or_insert(0) += sign * row.balance;
                    }
                }
                LineSource::Movement { accounts } => {
                    for (key, amount) in self.cube.iter().filter(|(k, _)| k.period == period) {
                        if key.unit == money && accounts.contains(&key.account) && in_fund(key.dims.fund.as_deref()) {
                            *as_closed.entry(key.account.clone()).or_insert(0) += sign * amount;
                        }
                    }
                }
            }
            let (LineSource::Balance { accounts } | LineSource::Movement { accounts }) = &def.source else {
                continue;
            };
            if let Some(unknown) = accounts.iter().find(|a| self.chart.as_ref().is_none_or(|c| c.account(a).is_none())) {
                return reject("account", format!("line {} names account {unknown}, which the chart does not have", def.code));
            }
            // Anything posted since for this period restates it.
            for (key, amount) in self.cube.iter().filter(|(k, _)| k.period > period && k.origin == Some(period)) {
                if key.unit == money && accounts.contains(&key.account) && in_fund(key.dims.fund.as_deref()) {
                    *restated.entry(key.period).or_insert(0) += sign * amount;
                }
            }
            restated.retain(|_, amount| *amount != 0);
            lines.push(ReturnLine {
                code: def.code.clone(),
                label: def.label.clone(),
                value: as_closed.values().sum::<i128>() + restated.values().sum::<i128>(),
                as_closed,
                restated,
                source: Some(def.source.clone()),
                fund: def.fund.clone(),
            });
        }
        Ok((lines, closed.trial_balance_hash))
    }

    /// File a return, or file it again as a restatement when `reason` is given.
    pub(crate) fn file_return(
        &mut self,
        id: &str,
        definition: &ReturnDefinition,
        period: PeriodId,
        by: String,
        reason: Option<String>,
        index: u64,
    ) -> Result<&FiledReturn, Reject> {
        definition.validate().map_err(|e| Reject::new("definition", e.to_string()))?;
        if !definition.in_effect(period) {
            return reject(
                "not_effective",
                format!(
                    "version {} of return {} is not the mapping in effect for {period}",
                    definition.version, definition.id
                ),
            );
        }
        let (lines, trial_balance_hash) = self.prepare_return(definition, period)?;
        let earlier = self.returns.get(id).map(Vec::as_slice).unwrap_or_default();
        match (earlier.last(), &reason) {
            (None, Some(_)) => return reject("return", format!("return {id} has not been filed, so it cannot be restated")),
            (Some(_), None) => return reject("return", format!("return {id} is already filed; a change is a restatement with a reason")),
            (Some(last), Some(reason)) => {
                if reason.trim().is_empty() {
                    return reject("return", "a restatement needs a reason");
                }
                if last.period != period || last.definition != definition.id || definition.version < last.definition_version {
                    return reject(
                        "return",
                        format!("a restatement of {id} is for the same period and return, on the same or a later definition"),
                    );
                }
                if last.lines == lines {
                    return reject("unchanged", format!("nothing in return {id} has changed since version {}", last.version));
                }
            }
            (None, None) => {}
        }
        if id.is_empty() {
            return reject("return", "a return needs an ID");
        }
        let restating_entries: Vec<EntryId> = self.backdated.iter().filter(|b| b.origin == Some(period)).map(|b| b.entry).collect();
        let mut h = Sha256::new();
        h.update(b"ic.finance.return.v1");
        h.update(serde_json::to_vec(&(id, period, &definition.content_hash, &lines)).unwrap_or_default());
        let filed = FiledReturn {
            id: id.to_string(),
            period,
            version: earlier.len() as u32 + 1,
            definition: definition.id.clone(),
            definition_version: definition.version,
            definition_hash: definition.content_hash.clone(),
            lines,
            trial_balance_hash,
            restating_entries,
            hash: Hash32(h.finalize().into()),
            filed_by: by,
            reason,
            index,
        };
        let versions = self.returns.entry(id.to_string()).or_default();
        versions.push(filed);
        // Just pushed.
        versions.last().ok_or_else(|| Reject::new("return", "nothing was filed"))
    }

    /// One figure of a filed return with everything behind it: the
    /// definition version, the accounts or measure, the closed trial
    /// balance, the entries that have restated it and the period's extracts.
    /// `version` is the filing; the latest when not given.
    pub fn line_lineage(&self, id: &str, version: Option<u32>, code: &str) -> Result<LineLineage, String> {
        let versions = self.returns.get(id).ok_or_else(|| format!("return {id} has not been filed"))?;
        let filed = match version {
            Some(v) => versions.iter().find(|f| f.version == v),
            None => versions.last(),
        }
        .ok_or_else(|| format!("return {id} has no version {}", version.unwrap_or_default()))?;
        let line = filed
            .lines
            .iter()
            .find(|l| l.code == code)
            .ok_or_else(|| format!("return {id} has no line {code}"))?;
        let query = match &line.source {
            Some(LineSource::Conduct { .. }) => serde_json::json!({ "q": "conduct", "period": filed.period }),
            _ => serde_json::json!({ "q": "trial_balance", "period": filed.period, "fund": line.fund }),
        };
        Ok(LineLineage {
            id: filed.id.clone(),
            period: filed.period,
            version: filed.version,
            definition: filed.definition.clone(),
            definition_version: filed.definition_version,
            definition_hash: filed.definition_hash.clone(),
            line: line.clone(),
            trial_balance_hash: filed.trial_balance_hash,
            query,
            restating_entries: filed.restating_entries.clone(),
            extracts: self.extracts.values().filter(|x| x.as_at == filed.period.end()).cloned().collect(),
            filed_by: filed.filed_by.clone(),
            reason: filed.reason.clone(),
        })
    }

    /// The returns a later posting has restated: the filed figures no longer
    /// equal what the books would give, so each needs a restatement.
    pub fn returns_needing_restatement(&self, definitions: &[ReturnDefinition]) -> Vec<String> {
        self.returns
            .iter()
            .filter(|(_, versions)| {
                versions.last().is_some_and(|last| {
                    definitions
                        .iter()
                        .find(|d| d.id == last.definition && d.version == last.definition_version)
                        .and_then(|d| self.prepare_return(d, last.period).ok())
                        .is_some_and(|(lines, _)| lines != last.lines)
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}
