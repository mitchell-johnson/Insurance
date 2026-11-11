//! Views of the claims state, each cut for its audience.
//!
//! The claimant sees their own claim. An adviser or an employer sees status,
//! never cause. Cause-of-claim analysis exists only in [`Book`], which stays
//! inside the claims module.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::config::Queue;
use crate::model::{ClaimType, DecisionKind, RecoveryStatus, RunStatus, Status, TaskKind, TimerKind};
use crate::state::ClaimsState;
use crate::{ClaimId, StaffId};

/// What an adviser or an employer may see: that a claim exists and where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusView {
    pub claim: ClaimId,
    pub status: Status,
    pub notified_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
}

/// The claimant's own view of their claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimantView {
    pub claim: ClaimId,
    pub claim_type: ClaimType,
    pub status: Status,
    /// One owner at a time, visible to the claimant.
    pub owner: Option<StaffId>,
    pub notified_on: NaiveDate,
    pub decision_due: Option<NaiveDate>,
    /// Evidence still awaited: its ID, what it is and when it is due.
    pub evidence_outstanding: Vec<(String, String, NaiveDate)>,
    pub admitted_minor: i64,
    pub paid_minor: i64,
    pub next_benefit: Option<NaiveDate>,
    /// Only what is actually being asked for. A held overpayment asks
    /// nothing of the claimant, so it is not shown as owing.
    pub repayment_asked_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRow {
    pub claim: ClaimId,
    pub priority: u32,
    pub age_days: i64,
    pub owner: Option<StaffId>,
}

/// Claims in payment as a portfolio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Book {
    pub in_payment: u32,
    pub monthly_minor: i64,
    pub reserve_minor: i64,
    /// Months in payment so far: `0-6`, `6-12`, `12-24`, `24+`.
    pub by_duration: BTreeMap<String, u32>,
    /// By root of the cause tag tree. Never leaves the claims module.
    pub by_cause: BTreeMap<String, u32>,
    pub expected_end_by_year: BTreeMap<i32, u32>,
    pub reviews_open: u32,
    pub reviews_overdue: u32,
    pub return_to_work: BTreeMap<String, u32>,
    pub caseload: BTreeMap<StaffId, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Report {
    pub notified: BTreeMap<ClaimType, u32>,
    /// Straight-through admissions, by health benefit category or claim type.
    pub straight_through: BTreeMap<String, u32>,
    pub decisions: BTreeMap<DecisionKind, u32>,
    /// Decisions by the person who proposed them, for consistency reporting.
    pub by_assessor: BTreeMap<StaffId, BTreeMap<DecisionKind, u32>>,
    pub partial_admissions: u32,
    /// Declines overturned at internal review or by the dispute scheme.
    pub overturns: u32,
    /// The insurer overturning its own admission, reported apart from declines.
    pub admissions_reversed: u32,
    pub days_to_decision: i64,
    pub decisions_timed: u32,
    /// Service timers met on time and missed, by kind.
    pub timers: BTreeMap<TimerKind, (u32, u32)>,
    pub overpaid_minor: i64,
    pub recovered_minor: i64,
    pub written_off_minor: i64,
    pub held_minor: i64,
    pub evidence_cost_minor: i64,
    pub workload: BTreeMap<StaffId, u32>,
    pub runs_unreconciled: u32,
}

/// The figures the daily control compares with the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Totals {
    pub reserve_minor: i64,
    pub payable_minor: i64,
    pub receivable_minor: i64,
}

impl ClaimsState {
    pub fn status_view(&self, claim: &str) -> Option<StatusView> {
        self.claims.get(claim).map(|c| StatusView {
            claim: c.id.clone(),
            status: c.status,
            notified_on: c.notified_on,
            closed_on: c.closed_on,
        })
    }

    pub fn claimant_view(&self, claim: &str) -> Option<ClaimantView> {
        let c = self.claims.get(claim)?;
        let asked = |o: &&crate::Overpayment| matches!(o.status, RecoveryStatus::Pursued | RecoveryStatus::Arrangement | RecoveryStatus::Offset);
        Some(ClaimantView {
            claim: c.id.clone(),
            claim_type: c.claim_type,
            status: c.status,
            owner: c.owner.clone(),
            notified_on: c.notified_on,
            decision_due: c.timers.get(&TimerKind::Decision).filter(|t| t.met_on.is_none()).map(|t| t.due),
            evidence_outstanding: c
                .evidence
                .iter()
                .filter(|e| e.received_on.is_none())
                .map(|e| (e.id.clone(), e.kind.clone(), e.due))
                .collect(),
            admitted_minor: c.admitted_minor,
            paid_minor: c.paid_minor(false) + c.paid_minor(true),
            next_benefit: c
                .schedule
                .as_ref()
                .filter(|s| c.status == Status::InPayment && s.next_period < s.end)
                .map(|s| s.next_period),
            repayment_asked_minor: c.overpayments.iter().filter(asked).map(|o| o.outstanding_minor()).sum(),
        })
    }

    /// A queue's open claims, most urgent first and then oldest first, so
    /// the oldest claim is always visible.
    pub fn queue(&self, queue: Queue, today: NaiveDate) -> Vec<QueueRow> {
        let mut rows: Vec<QueueRow> = self
            .claims
            .values()
            .filter(|c| c.queue == queue && c.status.is_open())
            .map(|c| QueueRow {
                claim: c.id.clone(),
                priority: c.priority + (today - c.event_date).num_days().clamp(0, 30) as u32,
                age_days: (today - c.notified_on).num_days(),
                owner: c.owner.clone(),
            })
            .collect();
        rows.sort_by(|a, b| b.priority.cmp(&a.priority).then(b.age_days.cmp(&a.age_days)));
        rows
    }

    pub fn book(&self, today: NaiveDate) -> Book {
        let mut book = Book::default();
        for c in self.in_payment.iter().filter_map(|id| self.claims.get(id)) {
            let Some(s) = c.schedule.as_ref() else { continue };
            book.in_payment += 1;
            book.monthly_minor += s.monthly_minor;
            book.reserve_minor += c.reserve_minor;
            let since = c.admission().map_or(c.notified_on, |d| d.decided_on);
            let band = match (today - since).num_days() {
                d if d < 183 => "0-6",
                d if d < 365 => "6-12",
                d if d < 730 => "12-24",
                _ => "24+",
            };
            *book.by_duration.entry(band.to_string()).or_default() += 1;
            let tags = c.cause_code.as_ref().and_then(|code| self.rules.cause_codes.get(code));
            let root = tags.and_then(|t| t.first()).and_then(|t| t.split('.').next()).unwrap_or("unknown");
            *book.by_cause.entry(root.to_string()).or_default() += 1;
            *book.expected_end_by_year.entry(chrono::Datelike::year(&s.expected_end)).or_default() += 1;
            book.reviews_open += c.tasks.iter().filter(|t| t.kind.review() && t.completed_on.is_none()).count() as u32;
            book.reviews_overdue += s.reviews_overdue;
            for t in c.task(TaskKind::ReturnToWork) {
                *book.return_to_work.entry(t.outcome.clone().unwrap_or_else(|| "open".into())).or_default() += 1;
            }
            if let Some(m) = &s.case_manager {
                *book.caseload.entry(m.clone()).or_default() += 1;
            }
        }
        book
    }

    pub fn report(&self) -> Report {
        let mut r = Report::default();
        for c in self.claims.values() {
            *r.notified.entry(c.claim_type).or_default() += 1;
            r.overturns += c.overturns;
            for d in &c.decisions {
                *r.decisions.entry(d.kind).or_default() += 1;
                if let Some(who) = d.signers.first() {
                    *r.by_assessor.entry(who.clone()).or_default().entry(d.kind).or_default() += 1;
                }
                match d.kind {
                    DecisionKind::Admit if d.straight_through => {
                        let category = c.health.as_ref().map_or_else(|| c.claim_type.code().to_string(), |h| h.category.clone());
                        *r.straight_through.entry(category).or_default() += 1;
                    }
                    DecisionKind::Admit if d.amount_minor < c.claimed_minor && c.schedule.is_none() => r.partial_admissions += 1,
                    DecisionKind::ReverseAdmission => r.admissions_reversed += 1,
                    _ => {}
                }
            }
            if let Some(d) = c.decisions.iter().find(|d| matches!(d.kind, DecisionKind::Admit | DecisionKind::Decline)) {
                r.days_to_decision += (d.decided_on - c.notified_on).num_days();
                r.decisions_timed += 1;
            }
            for (kind, t) in &c.timers {
                let entry = r.timers.entry(*kind).or_default();
                match t.met_on {
                    Some(on) if on <= t.due => entry.0 += 1,
                    Some(_) => entry.1 += 1,
                    None => entry.1 += u32::from(t.escalated > 0),
                }
            }
            for o in &c.overpayments {
                r.overpaid_minor += o.amount_minor;
                r.recovered_minor += o.recovered_minor;
                r.written_off_minor += o.written_off_minor;
                if o.status == RecoveryStatus::Held {
                    r.held_minor += o.outstanding_minor();
                }
            }
            r.evidence_cost_minor += c.evidence.iter().map(|e| e.cost_minor).sum::<i64>();
        }
        r.workload = self
            .owned
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.len() as u32))
            .collect();
        r.runs_unreconciled = self.runs.values().filter(|run| run.status == RunStatus::Unreconciled).count() as u32;
        r
    }

    /// Open reserves, unpaid admissions and overpayments outstanding, by
    /// policy. The daily control: these equal the ledger's balances.
    pub fn totals_by_policy(&self) -> BTreeMap<String, Totals> {
        let mut totals: BTreeMap<String, Totals> = BTreeMap::new();
        for c in self.claims.values() {
            let receivable: i64 = c.overpayments.iter().map(|o| o.outstanding_minor()).sum();
            if c.reserve_minor != 0 || c.payable_minor != 0 || receivable != 0 {
                let t = totals.entry(c.policy.clone()).or_default();
                t.reserve_minor += c.reserve_minor;
                t.payable_minor += c.payable_minor;
                t.receivable_minor += receivable;
            }
        }
        totals
    }

    /// The remittance advice for a run: what each payee is paid, and for
    /// how many claims.
    pub fn remittance(&self, run: &str) -> BTreeMap<String, (u32, i64)> {
        let mut advice: BTreeMap<String, (u32, i64)> = BTreeMap::new();
        let failed = |r: &crate::Run, item: &crate::RunItem| r.exceptions.iter().any(|(p, _)| p == &item.payment_ref(&r.id));
        let Some(r) = self.runs.get(run) else { return advice };
        for item in r.items.iter().filter(|i| !failed(r, i)) {
            let entry = advice.entry(item.payee.clone()).or_default();
            entry.0 += 1;
            entry.1 += item.net_minor();
        }
        advice
    }
}
