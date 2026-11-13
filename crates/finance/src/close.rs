//! The period close: declared checks, waivers and sign-off.
//!
//! The close adds no postings to the core. It confirms that the finance
//! ledger equals the core for every day of the period, that the sweeps,
//! reconciliations, extracts and journals the period needs are done, and
//! then freezes the period. Which checks run and which of them block
//! sign-off is configuration. A blocking check that is red stops sign-off
//! unless the finance lead waives it, on the record, with a reason, a
//! remediation owner and a date.

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDate, Weekday};
use ic_ledger::Hash32;
use serde::{Deserialize, Serialize};

use crate::books::{Anchor, FinanceState, JournalKind, JournalStatus};
use crate::config::{content_hash, eval, ConfigError};
use crate::period::PeriodId;

/// The checks the finance ledger works out from its own state.
pub const AUTOMATIC_CHECKS: [&str; 9] = [
    CONTROL_CHECK,
    VALUATION_EXTRACTS,
    UNAPPROVED_JOURNALS,
    ACTUARIAL_JOURNALS,
    TRIAL_BALANCE,
    FUND_BALANCE,
    MIGRATION_CONTROL,
    RATE_TABLE_VALIDATION,
    TAX_POSTING_TYPES,
];
pub const CONTROL_CHECK: &str = "control_check";
pub const VALUATION_EXTRACTS: &str = "valuation_extracts";
pub const UNAPPROVED_JOURNALS: &str = "unapproved_journals";
pub const ACTUARIAL_JOURNALS: &str = "actuarial_journals";
pub const TRIAL_BALANCE: &str = "trial_balance";
pub const FUND_BALANCE: &str = "fund_balance";
pub const MIGRATION_CONTROL: &str = "migration_control";
pub const RATE_TABLE_VALIDATION: &str = "rate_table_validation";
pub const TAX_POSTING_TYPES: &str = "tax_posting_types";

/// The extract kinds a period needs before it can close.
pub const REQUIRED_EXTRACTS: [&str; 2] = ["in_force", "movement"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckSource {
    /// Worked out by the finance ledger from its own state.
    Automatic,
    /// Reported by the named caller.
    Reported { by: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckDecl {
    pub id: String,
    pub name: String,
    /// The role that owns the step.
    pub owner: String,
    /// Business day after period end the step is due.
    pub day: u32,
    pub blocking: bool,
    /// Runs only for a period that ends a quarter.
    #[serde(default)]
    pub quarterly: bool,
    pub source: CheckSource,
}

/// Aged items another module reports, checked against thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgedDecl {
    /// What the items are, for example `held_balance`.
    pub kind: String,
    pub name: String,
    pub owner: String,
    pub day: u32,
    /// The caller allowed to report them.
    pub by: String,
    pub max_age_days: u32,
    pub max_amount_minor: i64,
    pub blocking: bool,
}

impl AgedDecl {
    pub fn check_id(&self) -> String {
        format!("aged:{}", self.kind)
    }
}

/// A tax or levy account and the posting types that explain a movement on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaxDecl {
    pub account: String,
    pub name: String,
    pub posting_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseConfig {
    pub version: u32,
    pub checks: Vec<CheckDecl>,
    #[serde(default)]
    pub aged: Vec<AgedDecl>,
    /// The codes of the accounts a migration posts its other side to. Each
    /// moves in the period the old system is loaded and must then stay still.
    #[serde(default)]
    pub migration_control: Vec<String>,
    /// The tax accounts and the posting types the core may move each with.
    #[serde(default)]
    pub tax: Vec<TaxDecl>,
    /// Public holidays as observed (a holiday that falls on a weekend is
    /// listed on the weekday it is taken). Step due dates skip them.
    #[serde(default)]
    pub holidays: BTreeSet<NaiveDate>,
    #[serde(default)]
    pub content_hash: String,
}

impl CloseConfig {
    /// Load a close configuration module. The module must define `CLOSE`.
    pub fn load(file: &str, source: &str) -> Result<Self, ConfigError> {
        let mut config: CloseConfig = eval(file, source, "CLOSE")?;
        config.content_hash = content_hash(source);
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |m: String| Err(ConfigError::Invalid(m));
        if self.version == 0 {
            return invalid("a close configuration needs a version of at least 1".to_string());
        }
        // A holiday on a weekend moves nothing, so it is the calendar date
        // typed where the observed day was meant.
        if let Some(day) = self.holidays.iter().find(|h| matches!(h.weekday(), Weekday::Sat | Weekday::Sun)) {
            return invalid(format!("holiday {day} falls on a weekend; list the weekday it is observed on"));
        }
        let mut taxed = BTreeSet::new();
        for t in &self.tax {
            if t.account.is_empty() || t.posting_types.is_empty() || !taxed.insert(t.account.as_str()) {
                return invalid(format!("tax account `{}` is unnamed, declared twice or has no posting types", t.account));
            }
        }
        let mut ids = BTreeSet::new();
        for c in &self.checks {
            if c.id.is_empty() || c.id.starts_with("aged:") || !ids.insert(c.id.clone()) {
                return invalid(format!("check `{}` is unnamed, reserved or declared twice", c.id));
            }
            let known = AUTOMATIC_CHECKS.contains(&c.id.as_str());
            match &c.source {
                CheckSource::Automatic if !known => return invalid(format!("the finance ledger has no automatic check called {}", c.id)),
                CheckSource::Reported { .. } if known => return invalid(format!("check {} is automatic and cannot be reported", c.id)),
                CheckSource::Reported { by } if by.is_empty() => return invalid(format!("check {} names nobody to report it", c.id)),
                _ => {}
            }
        }
        // Without these two a period could close on books that do not equal
        // the core, or do not balance.
        for required in [CONTROL_CHECK, TRIAL_BALANCE] {
            if !self.checks.iter().any(|c| c.id == required && c.blocking && !c.quarterly) {
                return invalid(format!("{required} must be declared as a blocking check for every period"));
            }
        }
        for a in &self.aged {
            if a.kind.is_empty() || a.by.is_empty() || a.max_amount_minor < 0 || !ids.insert(a.check_id()) {
                return invalid(format!(
                    "aged items `{}` are unnamed, declared twice, or have no reporter or a negative threshold",
                    a.kind
                ));
            }
        }
        Ok(())
    }
}

/// What a caller reports for a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Outcome {
    Green,
    Red { items: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub outcome: Outcome,
    pub by: String,
    pub actor: Option<String>,
    /// Where the supporting evidence is kept.
    pub evidence: Option<String>,
    pub index: u64,
}

/// A recorded decision to sign off over a red blocking check. It never
/// clears the underlying item: the next period's check starts red again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiver {
    pub check: String,
    pub by: String,
    pub reason: String,
    pub remediation_owner: String,
    pub due: NaiveDate,
    /// What was red when it was waived.
    pub items: Vec<String>,
    pub index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capacity {
    FinanceLead,
    AppointedActuary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignOff {
    pub capacity: Capacity,
    pub by: String,
    pub index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodStatus {
    Open,
    /// The business date has passed period end on every partition.
    Closing,
    /// Frozen. Nothing posts here again.
    Closed,
}

/// One row of a trial balance: debits positive, in fine units of a currency
/// or micro-units of a fund.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BalanceRow {
    pub account: String,
    pub unit: ic_ledger::BalanceFamily,
    pub fund: Option<String>,
    pub balance: i128,
}

/// What a period was when it closed. Never changes afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedPeriod {
    /// Entries in the finance ledger when the period closed.
    pub final_seq: u64,
    pub final_head: Hash32,
    /// The trial balance as at the period, by account, unit and fund.
    pub trial_balance: Vec<BalanceRow>,
    pub trial_balance_hash: Hash32,
    pub by: Option<String>,
    pub index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodState {
    pub status: PeriodStatus,
    pub reports: std::collections::BTreeMap<String, CheckReport>,
    pub waivers: std::collections::BTreeMap<String, Waiver>,
    pub sign_offs: Vec<SignOff>,
    pub closed: Option<ClosedPeriod>,
}

impl PeriodState {
    pub(crate) fn new(status: PeriodStatus) -> Self {
        Self {
            status,
            reports: Default::default(),
            waivers: Default::default(),
            sign_offs: vec![],
            closed: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckStatus {
    Green,
    Red {
        items: Vec<String>,
    },
    /// Red, and signed off over by the finance lead.
    Waived {
        items: Vec<String>,
    },
    /// Does not apply to this period.
    NotDue,
}

impl CheckStatus {
    fn of(items: Vec<String>) -> Self {
        if items.is_empty() {
            CheckStatus::Green
        } else {
            CheckStatus::Red { items }
        }
    }
}

/// One step of the close as the dashboard shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckView {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub day: u32,
    pub blocking: bool,
    #[serde(flatten)]
    pub status: CheckStatus,
    pub evidence: Option<String>,
}

/// How many items a red check lists before it says how many more there are.
const MAX_LISTED: usize = 50;

fn capped(mut items: Vec<String>) -> Vec<String> {
    if items.len() > MAX_LISTED {
        let more = items.len() - MAX_LISTED;
        items.truncate(MAX_LISTED);
        items.push(format!("and {more} more"));
    }
    items
}

impl FinanceState {
    /// Every declared step of a period's close with its present status.
    pub fn checks(&self, period: PeriodId) -> Vec<CheckView> {
        let Some(config) = &self.close_config else {
            return vec![];
        };
        let state = self.periods.get(&period);
        let quarter_end = !period.is_adjustment() && period.is_quarter_end(self.year_end_month());
        let mut views = Vec::new();
        let mut view = |id: String, name: &str, owner: &str, day: u32, blocking: bool, status: CheckStatus, evidence: Option<String>| {
            let status = match (status, state.and_then(|s| s.waivers.get(&id))) {
                (CheckStatus::Red { items }, Some(_)) => CheckStatus::Waived { items },
                (status, _) => status,
            };
            views.push(CheckView {
                id,
                name: name.to_string(),
                owner: owner.to_string(),
                day,
                blocking,
                status,
                evidence,
            });
        };
        for c in &config.checks {
            let report = state.and_then(|s| s.reports.get(&c.id));
            let status = if c.quarterly && !quarter_end {
                CheckStatus::NotDue
            } else {
                match &c.source {
                    CheckSource::Automatic => self.automatic(period, &c.id),
                    // An adjustment period has no sweeps or settlements of its own.
                    CheckSource::Reported { .. } if period.is_adjustment() => CheckStatus::NotDue,
                    CheckSource::Reported { .. } => match report.map(|r| &r.outcome) {
                        Some(Outcome::Green) => CheckStatus::Green,
                        Some(Outcome::Red { items }) => CheckStatus::Red { items: items.clone() },
                        None => CheckStatus::Red {
                            items: vec!["not yet reported for this period".to_string()],
                        },
                    },
                }
            };
            view(
                c.id.clone(),
                &c.name,
                &c.owner,
                c.day,
                c.blocking,
                status,
                report.and_then(|r| r.evidence.clone()),
            );
        }
        for a in &config.aged {
            let status = if period.is_adjustment() {
                CheckStatus::NotDue
            } else {
                self.aged_status(period, a)
            };
            view(a.check_id(), &a.name, &a.owner, a.day, a.blocking, status, None);
        }
        views
    }

    /// The blocking checks of a period that are red and not waived.
    pub fn blockers(&self, period: PeriodId) -> Vec<CheckView> {
        self.checks(period)
            .into_iter()
            .filter(|c| c.blocking && matches!(c.status, CheckStatus::Red { .. }))
            .collect()
    }

    fn automatic(&self, period: PeriodId, id: &str) -> CheckStatus {
        let items = match id {
            CONTROL_CHECK => self.control_breaks(period),
            VALUATION_EXTRACTS => self.extract_gaps(period),
            UNAPPROVED_JOURNALS => self
                .journals
                .values()
                .filter(|j| j.draft.period == period && j.status == JournalStatus::Proposed)
                .map(|j| format!("journal {} proposed by {} is not approved", j.draft.id, j.maker))
                .collect(),
            ACTUARIAL_JOURNALS => {
                let posted: Vec<_> = self
                    .journals
                    .values()
                    .filter(|j| j.draft.period == period && j.draft.kind == JournalKind::Actuarial && matches!(j.status, JournalStatus::Approved { .. }))
                    .collect();
                if posted.is_empty() {
                    vec!["no actuarial journal is posted for the quarter".to_string()]
                } else {
                    posted
                        .iter()
                        .filter(|j| j.actuary.is_none())
                        .map(|j| format!("journal {} is posted without the appointed actuary's sign-off", j.draft.id))
                        .collect()
                }
            }
            FUND_BALANCE => self.imbalances(period, true),
            MIGRATION_CONTROL => self.migration_moves(period),
            RATE_TABLE_VALIDATION => self.unvalidated_tables(period),
            TAX_POSTING_TYPES => self.tax_reconciliation(period).unexplained,
            // The only other automatic check: the declarations were validated when loaded.
            _ => self.imbalances(period, false),
        };
        CheckStatus::of(capped(items))
    }

    /// Every date of the period on every partition must have been checked
    /// against the core, found equal, and anchored to the day's Merkle root.
    fn control_breaks(&self, period: PeriodId) -> Vec<String> {
        if period.is_adjustment() {
            return vec![];
        }
        let mut items = Vec::new();
        let (Some(first), Some(last)) = (self.first_date(), self.last_date()) else {
            return vec!["no business day has been received from the core".to_string()];
        };
        if last < period.end() {
            items.push(format!("the core has not closed the days after {last}"));
        }
        for date in period.start().iter_days().take_while(|d| *d <= period.end().min(last)) {
            // Dates before the insurer's first business date have nothing to check.
            if date < first {
                continue;
            }
            let mut missing = 0;
            for partition in 0..self.partitions() {
                match self.days.get(&(date, partition)) {
                    None => missing += 1,
                    Some(day) => {
                        for d in &day.differences {
                            items.push(format!(
                                "{date} partition {partition}: {} ({:?}) is {} in the core and {} here",
                                d.role, d.class, d.core, d.finance
                            ));
                        }
                    }
                }
            }
            if missing > 0 {
                items.push(format!("{date}: {missing} partitions not yet checked against the core"));
                continue;
            }
            match self.anchor(date) {
                Anchor::Anchored => {}
                Anchor::NoRoot => items.push(format!("{date}: the day's Merkle root has not been recorded")),
                Anchor::Mismatch => items.push(format!("{date}: the chain heads checked here do not produce the published Merkle root")),
            }
        }
        items
    }

    fn extract_gaps(&self, period: PeriodId) -> Vec<String> {
        if period.is_adjustment() {
            return vec![];
        }
        let mut items = Vec::new();
        for kind in REQUIRED_EXTRACTS {
            match self.extracts.values().find(|e| e.kind == kind && e.as_at == period.end()) {
                None => items.push(format!("no {kind} extract has been cut as at {}", period.end())),
                Some(e) if !e.reconciled => items.push(format!("extract {} does not reconcile opening to closing", e.id)),
                Some(_) => {}
            }
        }
        items
    }

    /// The books must net to zero in every currency and every fund's units:
    /// as a whole, and within each statutory fund.
    /// A migration control account takes the other side of what the old
    /// system is loaded with. The first period it moves in is the cutover;
    /// a movement in any later period means something was loaded, or
    /// reversed, after the balances were agreed with the old system.
    fn migration_moves(&self, period: PeriodId) -> Vec<String> {
        let mut items = Vec::new();
        let accounts = self.close_config.as_ref().map(|c| c.migration_control.as_slice()).unwrap_or_default();
        for code in accounts {
            let Some(account) = self.chart.as_ref().and_then(|c| c.account(code)) else {
                items.push(format!("migration control account {code} is not in the chart"));
                continue;
            };
            let through = || self.turnover.iter().filter(|((_, account, _), amount)| account == code && **amount != 0);
            let Some(cutover) = through().map(|((p, _, _), _)| *p).min() else {
                continue;
            };
            if period <= cutover {
                continue;
            }
            for ((_, _, unit), amount) in through().filter(|((p, _, _), _)| *p == period) {
                items.push(format!(
                    "{code} {} had {amount} posted through it in {unit:?} in {period}; it was loaded in {cutover} and should be still",
                    account.name
                ));
            }
        }
        items
    }

    /// Every rate table version activated by the end of the period with no
    /// validation recorded for the same content. The core records a
    /// validation and does not insist on one, so this is where a table that
    /// went live unchecked is seen.
    fn unvalidated_tables(&self, period: PeriodId) -> Vec<String> {
        // The year's last month has already shown them.
        if period.is_adjustment() {
            return Vec::new();
        }
        self.rate_tables
            .iter()
            .filter(|(_, seen)| !seen.validated && seen.activated.is_some_and(|date| date <= period.end()))
            .map(|((id, version, hash), seen)| {
                let date = seen.activated.map(|d| d.to_string()).unwrap_or_default();
                format!("rate table {id} version {version} ({hash}) was activated on {date} with no validation recorded for that content")
            })
            .collect()
    }

    fn imbalances(&self, period: PeriodId, by_fund: bool) -> Vec<String> {
        let mut nets: std::collections::BTreeMap<(Option<String>, ic_ledger::BalanceFamily), i128> = Default::default();
        for row in self.trial_balance(period, None) {
            *nets.entry((row.fund.filter(|_| by_fund), row.unit)).or_insert(0) += row.balance;
        }
        nets.into_iter()
            .filter(|(_, net)| *net != 0)
            .map(|((fund, unit), net)| match (by_fund, fund) {
                (false, _) => format!("the books are out of balance in {unit:?} by {net}"),
                (true, Some(fund)) => format!("statutory fund {fund} is out of balance in {unit:?} by {net}"),
                (true, None) => format!("postings with no statutory fund are out of balance in {unit:?} by {net}"),
            })
            .collect()
    }

    fn aged_status(&self, period: PeriodId, decl: &AgedDecl) -> CheckStatus {
        let Some(report) = self.aged.get(&decl.kind).filter(|r| r.as_at >= period.end()) else {
            return CheckStatus::Red {
                items: vec![format!("no {} report as at {} or later", decl.kind, period.end())],
            };
        };
        let items = report
            .items
            .iter()
            .filter(|i| i.action.is_none())
            .filter_map(|i| {
                let age = (report.as_at - i.since).num_days();
                let old = age > i64::from(decl.max_age_days);
                let large = i.amount_minor.abs() > decl.max_amount_minor;
                (old || large).then(|| format!("{} ({}): {} held for {age} days with no documented action", i.party, i.reason, i.amount_minor))
            })
            .collect();
        CheckStatus::of(capped(items))
    }
}
