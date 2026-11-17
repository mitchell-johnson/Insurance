//! Reports read from the books: the trial balance, the movement report with
//! prior-period items in their own columns, the lineage of a period's
//! figures, and the close dashboard.
//!
//! Nothing here changes state, and nothing here reads a clock: the
//! dashboard is given today's date by whoever asks.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate, Weekday};
use ic_ledger::{AccountClass, BalanceFamily, EntryId, Hash32, PartitionId};
use serde::Serialize;

use crate::books::{Anchor, BackdatedItem, FinanceState, JournalStatus};
use crate::close::{BalanceRow, Capacity, CheckSource, CheckStatus, CheckView, PeriodStatus, SignOff, Waiver};
use crate::period::PeriodId;

/// One account's movement in a period, by unit and statutory fund. Debits
/// are positive, in fine units of a currency or micro-units of a fund.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MovementRow {
    pub account: String,
    pub name: String,
    pub class: AccountClass,
    pub unit: BalanceFamily,
    pub fund: Option<String>,
    /// Everything posted in earlier periods.
    pub opening: i128,
    /// The period's own activity.
    pub activity: i128,
    /// Posted in this period for an earlier one, by the period it belongs
    /// to. A late-notified death that unwinds eight months of premium shows
    /// here, not in `activity`.
    pub prior_period: BTreeMap<PeriodId, i128>,
    pub closing: i128,
    /// Movement since the start of the financial year, this period included.
    pub year_to_date: i128,
}

/// Where one partition's business day in the books came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaySource {
    pub date: NaiveDate,
    pub partition: PartitionId,
    /// The partition's chain head when the day closed.
    pub closing_head: Hash32,
    /// The core entries the day's journals summarise.
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    /// The finance ledger entries that carry the day.
    pub journals: Vec<EntryId>,
    /// The books equalled the core's own balances at the close.
    pub equal: bool,
    pub anchor: Anchor,
}

/// A step of the close with the date it is due.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    #[serde(flatten)]
    pub check: CheckView,
    pub due: NaiveDate,
    /// Red and past its due date.
    pub overdue: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaiverView {
    /// The period the waiver was given for.
    pub period: PeriodId,
    #[serde(flatten)]
    pub waiver: Waiver,
    /// Its remediation date has passed.
    pub overdue: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ControlSummary {
    /// Partition days the period has had so far.
    pub expected: u64,
    pub checked: u64,
    /// Checked and found different from the core.
    pub different: u64,
    /// Dates fully checked whose Merkle root is missing or does not match.
    pub unanchored: Vec<NaiveDate>,
}

/// What was posted in the period for one earlier period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PriorPeriodSummary {
    pub origin: PeriodId,
    /// Backdated core entries received.
    pub entries: u64,
    /// How many of them reverse an earlier entry.
    pub reversals: u64,
    /// Effect on the result by currency, in fine units: positive is profit.
    pub result_effect: BTreeMap<String, i128>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgedPartyView {
    pub party: String,
    pub reason: String,
    pub amount_minor: i64,
    pub age_days: i64,
    pub owner: Option<String>,
    pub action: Option<String>,
}

/// Items another module is holding, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgedSummary {
    pub kind: String,
    pub as_at: NaiveDate,
    pub total_minor: i64,
    pub count: u32,
    pub items: Vec<AgedPartyView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtractView {
    pub id: String,
    pub kind: String,
    pub content_hash: Hash32,
    pub rows: u64,
    pub reconciled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct JournalCounts {
    pub proposed: u64,
    pub approved: u64,
    pub rejected: u64,
}

/// One screen for the close of a period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dashboard {
    pub period: PeriodId,
    /// `None` before the period's first posting.
    pub status: Option<PeriodStatus>,
    /// Negative while the period is still running.
    pub days_since_period_end: i64,
    pub steps: Vec<Step>,
    /// IDs of the blocking checks that are red and not waived.
    pub blockers: Vec<String>,
    /// Waivers of this period, and of earlier periods whose check is still
    /// red here: a waiver never clears the underlying item.
    pub waivers: Vec<WaiverView>,
    pub sign_offs: Vec<SignOff>,
    /// Sign-offs the period still needs before it can close.
    pub sign_offs_needed: Vec<Capacity>,
    pub control: ControlSummary,
    pub prior_period: Vec<PriorPeriodSummary>,
    pub aged: Vec<AgedSummary>,
    /// Extracts cut as at the period end.
    pub extracts: Vec<ExtractView>,
    pub journals: JournalCounts,
    /// Set once the period is closed.
    pub trial_balance_hash: Option<Hash32>,
}

/// The date `n` working days after `from`. Weekends and the listed public
/// holidays are skipped.
pub fn working_days_after(from: NaiveDate, n: u32, holidays: &BTreeSet<NaiveDate>) -> NaiveDate {
    let mut date = from;
    let mut left = n;
    while left > 0 {
        let Some(next) = date.succ_opt() else {
            break;
        };
        date = next;
        if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) && !holidays.contains(&date) {
            left -= 1;
        }
    }
    date
}

impl FinanceState {
    /// The trial balance as at the end of a period: every account's balance
    /// by unit and statutory fund, for one fund or for all of them. A closed
    /// period returns the balance frozen when it closed.
    pub fn trial_balance(&self, period: PeriodId, fund: Option<&str>) -> Vec<BalanceRow> {
        let wanted = |row_fund: Option<&str>| fund.is_none_or(|f| row_fund == Some(f));
        if let Some(closed) = self.periods.get(&period).and_then(|p| p.closed.as_ref()) {
            return closed.trial_balance.iter().filter(|r| wanted(r.fund.as_deref())).cloned().collect();
        }
        let mut rows: BTreeMap<(String, BalanceFamily, Option<String>), i128> = BTreeMap::new();
        for (key, amount) in self.cube.iter().take_while(|(k, _)| k.period <= period) {
            if wanted(key.dims.fund.as_deref()) {
                *rows.entry((key.account.clone(), key.unit.clone(), key.dims.fund.clone())).or_insert(0) += amount;
            }
        }
        rows.into_iter()
            .filter(|(_, balance)| *balance != 0)
            .map(|((account, unit, fund), balance)| BalanceRow { account, unit, fund, balance })
            .collect()
    }

    /// What moved in a period on every account, with the items posted for
    /// earlier periods kept apart from the period's own activity.
    pub fn movements(&self, period: PeriodId) -> Vec<MovementRow> {
        let year_start = period.year_start(self.year_end_month());
        let mut rows: BTreeMap<(String, BalanceFamily, Option<String>), MovementRow> = BTreeMap::new();
        for (key, amount) in self.cube.iter().take_while(|(k, _)| k.period <= period) {
            let row = rows.entry((key.account.clone(), key.unit.clone(), key.dims.fund.clone())).or_insert_with(|| {
                let account = self.chart.as_ref().and_then(|c| c.account(&key.account));
                MovementRow {
                    account: key.account.clone(),
                    name: account.map(|a| a.name.clone()).unwrap_or_default(),
                    // Every posted account is in the chart, and a chart never drops one.
                    class: account.map_or(AccountClass::Asset, |a| a.class),
                    unit: key.unit.clone(),
                    fund: key.dims.fund.clone(),
                    opening: 0,
                    activity: 0,
                    prior_period: BTreeMap::new(),
                    closing: 0,
                    year_to_date: 0,
                }
            });
            row.closing += amount;
            if key.period >= year_start {
                row.year_to_date += amount;
            }
            if key.period < period {
                row.opening += amount;
            } else if let Some(origin) = key.origin {
                *row.prior_period.entry(origin).or_insert(0) += amount;
            } else {
                row.activity += amount;
            }
        }
        rows.into_values()
            .map(|mut r| {
                r.prior_period.retain(|_, amount| *amount != 0);
                r
            })
            .filter(|r| r.opening != 0 || r.closing != 0 || r.activity != 0 || !r.prior_period.is_empty() || r.year_to_date != 0)
            .collect()
    }

    /// Every partition day behind a period's figures: the core entries each
    /// day's journals summarise, and the chain head they were checked at.
    pub fn sources(&self, period: PeriodId) -> Vec<DaySource> {
        self.days
            .range((period.start(), 0)..=(period.end(), PartitionId::MAX))
            .map(|((date, partition), day)| DaySource {
                date: *date,
                partition: *partition,
                closing_head: day.closing_head,
                first_seq: day.first_seq,
                last_seq: day.last_seq,
                journals: day.journals.clone(),
                equal: day.differences.is_empty(),
                anchor: self.anchor(*date),
            })
            .collect()
    }

    /// The backdated entries and reversals posted in a period, optionally
    /// only those for one earlier period. This is how a restated figure is
    /// walked back to the postings behind it.
    pub fn backdated_in(&self, period: PeriodId, origin: Option<PeriodId>) -> Vec<&BackdatedItem> {
        self.backdated
            .iter()
            .filter(|b| b.period == period && (origin.is_none() || b.origin == origin))
            .collect()
    }

    /// The entries that reverse a given core entry.
    pub fn reversals_of(&self, entry: &str) -> Vec<&BackdatedItem> {
        self.backdated.iter().filter(|b| b.reverses.as_deref() == Some(entry)).collect()
    }

    /// The close of a period on one screen, as at `today`.
    pub fn dashboard(&self, period: PeriodId, today: NaiveDate) -> Dashboard {
        let state = self.periods.get(&period);
        let none = BTreeSet::new();
        let holidays = self.close_config.as_ref().map_or(&none, |c| &c.holidays);
        let steps: Vec<Step> = self
            .checks(period)
            .into_iter()
            .map(|check| {
                let due = working_days_after(period.end(), check.day, holidays);
                let overdue = today > due && matches!(check.status, CheckStatus::Red { .. });
                Step { check, due, overdue }
            })
            .collect();
        let blockers = steps
            .iter()
            .filter(|s| s.check.blocking && matches!(s.check.status, CheckStatus::Red { .. }))
            .map(|s| s.check.id.clone())
            .collect();
        let still_red = |check: &str| {
            steps
                .iter()
                .any(|s| s.check.id == check && matches!(s.check.status, CheckStatus::Red { .. } | CheckStatus::Waived { .. }))
        };
        let waivers = self
            .periods
            .range(..=period)
            .flat_map(|(p, s)| s.waivers.values().map(move |w| (*p, w)))
            .filter(|(p, w)| *p == period || still_red(&w.check))
            .map(|(p, w)| WaiverView {
                period: p,
                waiver: w.clone(),
                overdue: today > w.due,
            })
            .collect();
        let sign_offs = state.map(|s| s.sign_offs.clone()).unwrap_or_default();
        let mut needed = vec![Capacity::FinanceLead];
        if !period.is_adjustment() && period.is_quarter_end(self.year_end_month()) {
            needed.push(Capacity::AppointedActuary);
        }
        needed.retain(|c| !sign_offs.iter().any(|s| s.capacity == *c));
        if state.is_some_and(|s| s.status == PeriodStatus::Closed) {
            needed.clear();
        }

        let mut journals = JournalCounts::default();
        for j in self.journals.values().filter(|j| j.draft.period == period) {
            match j.status {
                JournalStatus::Proposed => journals.proposed += 1,
                JournalStatus::Approved { .. } => journals.approved += 1,
                JournalStatus::Rejected { .. } => journals.rejected += 1,
            }
        }

        Dashboard {
            period,
            status: state.map(|s| s.status),
            days_since_period_end: (today - period.end()).num_days(),
            steps,
            blockers,
            waivers,
            sign_offs,
            sign_offs_needed: needed,
            control: self.control_summary(period),
            prior_period: self.prior_period_summary(period),
            aged: self.aged_summary(),
            extracts: self
                .extracts
                .values()
                .filter(|e| !period.is_adjustment() && e.as_at == period.end())
                .map(|e| ExtractView {
                    id: e.id.clone(),
                    kind: e.kind.clone(),
                    content_hash: e.content_hash,
                    rows: e.rows,
                    reconciled: e.reconciled,
                })
                .collect(),
            journals,
            trial_balance_hash: state.and_then(|s| s.closed.as_ref()).map(|c| c.trial_balance_hash),
        }
    }

    fn control_summary(&self, period: PeriodId) -> ControlSummary {
        let mut summary = ControlSummary::default();
        let (Some(first), Some(last)) = (self.first_date(), self.last_date()) else {
            return summary;
        };
        if period.is_adjustment() {
            return summary;
        }
        let partitions = u64::from(self.partitions());
        for date in period.start().iter_days().take_while(|d| *d <= period.end().min(last)) {
            if date < first {
                continue;
            }
            summary.expected += partitions;
            let days = self.days.range((date, 0)..=(date, PartitionId::MAX));
            let mut checked = 0;
            for (_, day) in days {
                checked += 1;
                if !day.differences.is_empty() {
                    summary.different += 1;
                }
            }
            summary.checked += checked;
            if checked == partitions && self.anchor(date) != Anchor::Anchored {
                summary.unanchored.push(date);
            }
        }
        summary
    }

    fn prior_period_summary(&self, period: PeriodId) -> Vec<PriorPeriodSummary> {
        let mut by_origin: BTreeMap<PeriodId, PriorPeriodSummary> = BTreeMap::new();
        fn summary(by_origin: &mut BTreeMap<PeriodId, PriorPeriodSummary>, origin: PeriodId) -> &mut PriorPeriodSummary {
            by_origin.entry(origin).or_insert_with(|| PriorPeriodSummary {
                origin,
                entries: 0,
                reversals: 0,
                result_effect: BTreeMap::new(),
            })
        }
        for item in &self.backdated {
            if let (true, Some(origin)) = (item.period == period, item.origin) {
                let s = summary(&mut by_origin, origin);
                s.entries += 1;
                s.reversals += u64::from(item.reverses.is_some());
            }
        }
        for row in self.movements(period) {
            let BalanceFamily::Currency(currency) = &row.unit else {
                continue;
            };
            if !matches!(row.class, AccountClass::Income | AccountClass::Expense) {
                continue;
            }
            for (origin, amount) in &row.prior_period {
                // Income is a credit, so a credit raises the result.
                *summary(&mut by_origin, *origin).result_effect.entry(currency.to_string()).or_insert(0) -= amount;
            }
        }
        by_origin.into_values().collect()
    }

    fn aged_summary(&self) -> Vec<AgedSummary> {
        self.aged
            .iter()
            .map(|(kind, report)| {
                let mut items: Vec<AgedPartyView> = report
                    .items
                    .iter()
                    .map(|i| AgedPartyView {
                        party: i.party.clone(),
                        reason: i.reason.clone(),
                        amount_minor: i.amount_minor,
                        age_days: (report.as_at - i.since).num_days(),
                        owner: i.owner.clone(),
                        action: i.action.clone(),
                    })
                    .collect();
                items.sort_by(|a, b| b.age_days.cmp(&a.age_days).then_with(|| a.party.cmp(&b.party)));
                AgedSummary {
                    kind: kind.clone(),
                    as_at: report.as_at,
                    total_minor: report.total_minor,
                    count: report.count,
                    items,
                }
            })
            .collect()
    }
}

/// What the close needs from outside the ledger: the worklist of its drivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Work {
    /// Dates checked against the core on every partition whose Merkle root
    /// has not been recorded.
    pub roots_wanted: Vec<NaiveDate>,
    /// Periods past their last day and not yet closed, oldest first.
    pub closing: Vec<PeriodWork>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct PeriodWork {
    pub period: PeriodId,
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// Whether the mart holds every partition's events up to the period end.
    pub mart_ready: bool,
    /// Extract kinds registered as at the period end.
    pub extracts: Vec<String>,
    /// Reported checks due for this period with no report yet.
    pub reports_wanted: Vec<String>,
    /// Aged item kinds with no report as at the period end or later.
    pub aged_wanted: Vec<String>,
    /// Blocking checks that are red and not waived.
    pub blockers: Vec<String>,
    pub sign_offs_needed: Vec<Capacity>,
}

/// Part of an extract file, for a driver that stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct ExtractPage {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub from: Option<NaiveDate>,
    pub as_at: NaiveDate,
    pub rows: u64,
    pub content_hash: Hash32,
    pub control: std::collections::BTreeMap<String, i128>,
    pub reconciled: bool,
    /// Lines in the whole file, the header included.
    pub total_lines: u64,
    pub offset: u64,
    pub lines: Vec<String>,
}

/// The most lines of an extract one answer carries.
pub const MAX_EXTRACT_LINES: u64 = 20_000;

impl FinanceState {
    pub fn work(&self) -> Work {
        let mut roots_wanted: Vec<NaiveDate> = Vec::new();
        for (date, _) in self.days.keys() {
            let complete = (0..self.partitions()).all(|p| self.days.contains_key(&(*date, p)));
            if complete && !self.roots.contains_key(date) && roots_wanted.last() != Some(date) {
                roots_wanted.push(*date);
            }
        }
        let closing = self
            .periods
            .iter()
            .filter(|(p, s)| !p.is_adjustment() && s.status == PeriodStatus::Closing)
            .map(|(period, state)| {
                let checks = self.checks(*period);
                let unreported = |id: &str| checks.iter().any(|c| c.id == id && c.status != CheckStatus::NotDue) && !state.reports.contains_key(id);
                let config = self.close_config.as_ref();
                let mut needed = vec![Capacity::FinanceLead];
                if period.is_quarter_end(self.year_end_month()) {
                    needed.push(Capacity::AppointedActuary);
                }
                needed.retain(|c| !state.sign_offs.iter().any(|s| s.capacity == *c));
                PeriodWork {
                    period: *period,
                    start: period.start(),
                    end: period.end(),
                    mart_ready: self.mart.closed_through().is_some_and(|d| d >= period.end()),
                    extracts: self.extracts.values().filter(|e| e.as_at == period.end()).map(|e| e.kind.clone()).collect(),
                    reports_wanted: config
                        .iter()
                        .flat_map(|c| &c.checks)
                        .filter(|c| matches!(c.source, CheckSource::Reported { .. }) && unreported(&c.id))
                        .map(|c| c.id.clone())
                        .collect(),
                    aged_wanted: config
                        .iter()
                        .flat_map(|c| &c.aged)
                        .filter(|a| self.aged.get(&a.kind).is_none_or(|r| r.as_at < period.end()))
                        .map(|a| a.kind.clone())
                        .collect(),
                    blockers: checks
                        .iter()
                        .filter(|c| c.blocking && matches!(c.status, CheckStatus::Red { .. }))
                        .map(|c| c.id.clone())
                        .collect(),
                    sign_offs_needed: needed,
                }
            })
            .collect();
        Work { roots_wanted, closing }
    }

    /// Each tax and levy account's movement in a period by the posting
    /// type and product the core gave it, and whatever moved it under a
    /// posting type the close configuration does not list for it, or under
    /// none. These are control accounts, so only the core's feed moves them.
    /// The tax of one kind withheld in a closed period and owed to Inland
    /// Revenue: the net credit to withholding tax payable under the posting
    /// type `withholding_{kind}`, in minor units, and the day it is due. Each
    /// kind is taken to file monthly, due on the 20th of the next month (15
    /// January for December), moved to the next working day; that schedule is
    /// a best estimate until the tax review. The partitions hold the
    /// liability, so the split between them is the core's to answer.
    pub fn withholding_due(&self, kind: &str, period: PeriodId) -> Result<serde_json::Value, String> {
        let posting_type = format!("withholding_{kind}");
        let account = self
            .close_config
            .as_ref()
            .and_then(|c| c.tax.iter().find(|t| t.posting_types.contains(&posting_type)))
            .map(|t| t.account.clone())
            .ok_or_else(|| format!("no tax account takes posting type {posting_type}"))?;
        if period.is_adjustment() || !self.periods.get(&period).is_some_and(|p| p.closed.is_some()) {
            return Err(format!("{period} is not closed; withholding is paid on a closed month's figure"));
        }
        let moved: i128 = self
            .cube
            .iter()
            .filter(|(k, _)| k.period == period && k.account == account && k.dims.posting_type.as_deref() == Some(posting_type.as_str()))
            .map(|(_, amount)| *amount)
            .sum();
        let next = period.end().succ_opt().unwrap_or(period.end());
        let nominal = if next.month() == 1 { next.with_day(15) } else { next.with_day(20) }.unwrap_or(next);
        let holidays = self.close_config.as_ref().map(|c| c.holidays.clone()).unwrap_or_default();
        let due = working_days_after(nominal.pred_opt().unwrap_or(nominal), 1, &holidays);
        Ok(serde_json::json!({
            "kind": kind,
            "period": period,
            "account": account,
            "posting_type": posting_type,
            "amount_minor": -moved / ic_money::FINE_PER_MINOR,
            "due": due,
        }))
    }

    pub fn tax_reconciliation(&self, period: PeriodId) -> TaxReconciliation {
        let mut out = TaxReconciliation {
            period,
            accounts: Vec::new(),
            unexplained: Vec::new(),
        };
        if period.is_adjustment() {
            return out;
        }
        let declared = self.close_config.as_ref().map(|c| c.tax.as_slice()).unwrap_or_default();
        for decl in declared {
            let Some(account) = self.chart.as_ref().and_then(|c| c.account(&decl.account)) else {
                out.unexplained.push(format!("tax account {} is not in the chart", decl.account));
                continue;
            };
            let mut rows: BTreeMap<(Option<String>, Option<String>, ic_ledger::BalanceFamily), i128> = BTreeMap::new();
            for (key, amount) in self.cube.iter().filter(|(k, _)| k.period == period && k.account == decl.account) {
                *rows
                    .entry((key.dims.posting_type.clone(), key.dims.product.clone(), key.unit.clone()))
                    .or_insert(0) += amount;
            }
            let mut lines = Vec::new();
            for ((posting_type, product, unit), movement) in rows {
                let explained = posting_type.as_ref().is_some_and(|t| decl.posting_types.contains(t));
                if !explained && movement != 0 {
                    out.unexplained.push(format!(
                        "{} {} moved by {movement} in {unit:?} under {}, which is not one of {:?}",
                        decl.account,
                        account.name,
                        posting_type.as_ref().map_or("no posting type".to_string(), |t| format!("posting type {t}")),
                        decl.posting_types
                    ));
                }
                lines.push(TaxLine {
                    posting_type,
                    product,
                    unit: format!("{unit:?}"),
                    movement,
                    explained,
                });
            }
            out.accounts.push(TaxAccount {
                account: decl.account.clone(),
                name: account.name.clone(),
                posting_types: decl.posting_types.clone(),
                movement: lines.iter().map(|l| l.movement).sum(),
                lines,
            });
        }
        out
    }

    /// Money the core holds in suspense for each policy, as the feed has
    /// built it up: one item for each policy, oldest first. The balances are
    /// as at the latest day closed, which is the date the report carries
    /// when that is later than the date asked for.
    pub fn aged_suspense(&self, as_at: NaiveDate) -> serde_json::Value {
        let held = self.mart.suspense();
        let total: i64 = held.iter().map(|(_, h)| h.amount_minor).sum();
        let as_at = self.mart.latest_close().map_or(as_at, |closed| closed.max(as_at));
        let items: Vec<serde_json::Value> = held
            .iter()
            .filter(|(_, h)| h.since <= as_at)
            .take(crate::driver::MAX_AGED_ITEMS)
            .map(|(policy, h)| serde_json::json!({ "party": policy, "reason": "policy_suspense", "amount_minor": h.amount_minor, "since": h.since }))
            .collect();
        // How far the books have been fed: with no day fed, nothing has been reported to suspense yet,
        // which is not the same as nothing being there.
        serde_json::json!({ "as_at": as_at, "items": items, "total_minor": total, "count": held.len(), "fed_to": self.mart.latest_close() })
    }

    /// Lines `offset..offset + limit` of the extract of `kind` for a period,
    /// with the hash and totals of the whole file.
    pub fn extract(&self, kind: &str, period: PeriodId, offset: u64, limit: u64) -> Result<ExtractPage, String> {
        let (from, to) = (period.start(), period.end());
        let extract = match kind {
            crate::mart::IN_FORCE => self.mart.in_force(to),
            crate::mart::MOVEMENT => self.mart.movement(from, to),
            crate::mart::EXPERIENCE => self.mart.experience(from, to),
            crate::mart::CLAIMS_BY_CAUSE => self.mart.claims_by_cause(from, to),
            other => return Err(format!("there is no {other} extract")),
        }
        .map_err(|e| e.to_string())?;
        let first = offset.min(extract.lines.len() as u64) as usize;
        let last = (first + limit.clamp(1, MAX_EXTRACT_LINES) as usize).min(extract.lines.len());
        Ok(ExtractPage {
            id: extract.id(),
            name: extract.name(),
            kind: kind.to_string(),
            from: extract.from,
            as_at: extract.as_at,
            rows: extract.rows(),
            content_hash: extract.content_hash(),
            control: extract.control.clone(),
            reconciled: extract.reconciled,
            total_lines: extract.lines.len() as u64,
            offset: first as u64,
            lines: extract.lines[first..last].to_vec(),
        })
    }
}

/// A period's tax and levy accounts reconciled to posting types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaxReconciliation {
    pub period: PeriodId,
    pub accounts: Vec<TaxAccount>,
    /// What moved a tax account under a posting type not listed for it.
    pub unexplained: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaxAccount {
    pub account: String,
    pub name: String,
    pub posting_types: Vec<String>,
    /// The account's whole movement in the period, debits positive.
    pub movement: i128,
    pub lines: Vec<TaxLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaxLine {
    pub posting_type: Option<String>,
    pub product: Option<String>,
    pub unit: String,
    pub movement: i128,
    pub explained: bool,
}
