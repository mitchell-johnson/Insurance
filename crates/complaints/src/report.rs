//! The complaints register, the figures the conduct regulator, the dispute
//! scheme, the board and the conduct committee are given, and the views
//! each kind of reader is allowed.
//!
//! Everything here is computed from the register on request, so every
//! figure can be traced to the complaints that make it up.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::PartyId;
use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::state::{days, ComplaintsState};

/// One line of the register: what the regulator's return is built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRow {
    pub complaint: String,
    pub received_on: NaiveDate,
    pub channel: IntakeChannel,
    pub complainant_kind: ComplainantKind,
    pub process: Process,
    pub product: Option<String>,
    pub cause: Option<String>,
    pub status: Status,
    pub outcome: Option<OutcomeKind>,
    pub front_line: bool,
    pub vulnerable: bool,
    pub days_to_acknowledge: Option<i64>,
    pub days_to_decide: Option<i64>,
    pub closed_on: Option<NaiveDate>,
    pub deadlocked: bool,
    pub forum: Option<Forum>,
    pub forum_outcome: Option<OutcomeKind>,
    pub remediation_minor: i64,
    pub systemic_case: Option<String>,
    /// The timer configuration it ran under.
    pub timers_version: u32,
}

/// How often a decision maker's or a rule's decisions were complained
/// about, and how often the complaint overturned them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Overturns {
    pub complained_about: u64,
    pub overturned: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Report {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub received: u64,
    pub by_process: BTreeMap<Process, u64>,
    pub by_cause: BTreeMap<String, u64>,
    pub by_product: BTreeMap<String, u64>,
    pub by_channel: BTreeMap<IntakeChannel, u64>,
    pub acknowledged_on_time: u64,
    pub acknowledged_late: u64,
    pub decided_on_time: u64,
    pub decided_late: u64,
    pub outcomes: BTreeMap<OutcomeKind, u64>,
    pub resolved_on_the_spot: u64,
    pub deadlocks: u64,
    pub deadlocks_by_time: u64,
    pub referrals: BTreeMap<Forum, u64>,
    pub forum_outcomes: BTreeMap<OutcomeKind, u64>,
    pub remediation_paid_minor: BTreeMap<RemediationKind, i64>,
    pub systemic_cases: u64,
    /// Parties with more than one complaint on record, at least one in the period.
    pub repeat_complainants: u64,
    pub vulnerable: u64,
    /// Vulnerable complainants whose acknowledgement or decision was late.
    pub vulnerable_late: u64,
    pub by_decision_maker: BTreeMap<String, Overturns>,
    pub by_rule_version: BTreeMap<String, Overturns>,
    /// Complaints by adviser engagement, for distribution oversight.
    pub by_adviser_engagement: BTreeMap<String, u64>,
    /// Months in the period that have ended without a root cause review.
    pub months_without_review: Vec<String>,
    pub open_conduct_cases: u64,
    pub records_removed: u64,
}

/// What an adviser sees of a complaint about their service: that it exists
/// and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdviserView {
    pub complaint: String,
    pub received_on: NaiveDate,
    pub status: Status,
    pub outcome: Option<OutcomeKind>,
}

/// What the complainant is given under their right of access: the record
/// of their complaint, without the names of staff or internal escalations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectRecord {
    pub complaint: String,
    pub received_on: NaiveDate,
    pub status: Status,
    pub category: Category,
    pub policies: Vec<ic_core::PolicyId>,
    pub acknowledged_on: Option<NaiveDate>,
    pub updates: Vec<NaiveDate>,
    pub outcome: Option<OutcomeKind>,
    pub reason_codes: Vec<String>,
    /// Every document sent to or received from the complainant.
    pub documents: Vec<String>,
    pub remediation: Vec<(RemediationKind, i64)>,
    pub forum: Option<(Forum, String)>,
    pub closed_on: Option<NaiveDate>,
}

/// A complaint as it shows in the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub complaint: String,
    pub owner: Option<String>,
    pub process: Process,
    /// The adjustments to make. The reason for them is not shown.
    pub adjustments: Vec<String>,
    pub needs_trained_handler: bool,
    pub authority_needed: u8,
    /// The timer due soonest, and when.
    pub next_due: Option<(Timer, NaiveDate)>,
}

fn month_of(date: NaiveDate) -> String {
    date.format("%Y-%m").to_string()
}

impl ComplaintsState {
    fn row(&self, c: &Complaint) -> RegisterRow {
        RegisterRow {
            complaint: c.id.clone(),
            received_on: c.received_on,
            channel: c.channel,
            complainant_kind: c.complainant_kind,
            process: c.category.process,
            product: c.category.product.clone(),
            cause: c.category.cause.clone(),
            status: c.status,
            outcome: c.outcome.as_ref().map(|o| o.kind),
            front_line: c.outcome.as_ref().is_some_and(|o| o.front_line),
            vulnerable: c.vulnerable,
            days_to_acknowledge: c.acknowledged_on.map(|d| (d - c.received_on).num_days()),
            days_to_decide: c.outcome.as_ref().map(|o| (o.decided_on - c.received_on).num_days()),
            closed_on: c.closure.as_ref().map(|x| x.on),
            deadlocked: c.deadlock.is_some(),
            forum: c.referral.as_ref().map(|r| r.forum),
            forum_outcome: c.referral.as_ref().and_then(|r| r.decision.as_ref()).map(|d| d.outcome),
            remediation_minor: c.remediation.iter().map(|r| r.amount_minor).sum(),
            systemic_case: c.systemic_case.clone(),
            timers_version: c.timers_version,
        }
    }

    fn in_period(&self, from: NaiveDate, to: NaiveDate) -> impl Iterator<Item = &Complaint> {
        self.complaints.values().filter(move |c| c.received_on >= from && c.received_on <= to)
    }

    /// The register for complaints received in a period.
    pub fn register(&self, from: NaiveDate, to: NaiveDate) -> Vec<RegisterRow> {
        self.in_period(from, to).map(|c| self.row(c)).collect()
    }

    /// The figures for complaints received in a period, as they stand on
    /// `to`: a timer not met by then counts as late.
    pub fn report(&self, from: NaiveDate, to: NaiveDate) -> Report {
        let mut r = Report {
            from: Some(from),
            to: Some(to),
            open_conduct_cases: self.open_conduct.len() as u64,
            records_removed: self.removed,
            ..Report::default()
        };
        let mut complainants = BTreeSet::new();
        for c in self.in_period(from, to) {
            let Some(t) = self.timers(c.timers_version) else { continue };
            r.received += 1;
            *r.by_process.entry(c.category.process).or_default() += 1;
            *r.by_channel.entry(c.channel).or_default() += 1;
            if let Some(cause) = &c.category.cause {
                *r.by_cause.entry(cause.clone()).or_default() += 1;
            }
            if let Some(product) = &c.category.product {
                *r.by_product.entry(product.clone()).or_default() += 1;
            }
            // Met, missed, or not yet due.
            let met = |done: Option<NaiveDate>, due: NaiveDate| match done {
                Some(on) => Some(on <= due),
                None if to > due => Some(false),
                None => None,
            };
            let acknowledged = met(c.acknowledged_on, c.received_on + days(t.acknowledge_days));
            let decided = met(c.outcome.as_ref().map(|o| o.decided_on), c.received_on + days(t.decide_days));
            match acknowledged {
                Some(true) => r.acknowledged_on_time += 1,
                Some(false) => r.acknowledged_late += 1,
                None => {}
            }
            match decided {
                Some(true) => r.decided_on_time += 1,
                Some(false) => r.decided_late += 1,
                None => {}
            }
            if c.vulnerable {
                r.vulnerable += 1;
                if acknowledged == Some(false) || decided == Some(false) {
                    r.vulnerable_late += 1;
                }
            }
            if let Some(o) = &c.outcome {
                *r.outcomes.entry(o.kind).or_default() += 1;
                r.resolved_on_the_spot += u64::from(o.front_line);
            }
            if let Some(d) = &c.deadlock {
                r.deadlocks += 1;
                r.deadlocks_by_time += u64::from(d.automatic);
            }
            if let Some(referral) = &c.referral {
                *r.referrals.entry(referral.forum).or_default() += 1;
                if let Some(d) = &referral.decision {
                    *r.forum_outcomes.entry(d.outcome).or_default() += 1;
                }
            }
            for paid in c.remediation.iter().filter(|x| x.completed_on.is_some() && x.amount_minor > 0) {
                *r.remediation_paid_minor.entry(paid.kind).or_default() += paid.amount_minor;
            }
            r.systemic_cases += u64::from(c.systemic_case.is_some());
            let overturned = c.outcome.as_ref().is_some_and(|o| o.kind.overturns())
                || c.referral.as_ref().and_then(|x| x.decision.as_ref()).is_some_and(|d| d.outcome.overturns());
            for d in &c.links.decisions {
                let count = |o: &mut Overturns| {
                    o.complained_about += 1;
                    o.overturned += u64::from(overturned);
                };
                count(r.by_decision_maker.entry(d.decided_by.clone()).or_default());
                if let Some(rule) = &d.rule_version {
                    count(r.by_rule_version.entry(rule.clone()).or_default());
                }
            }
            if let Some(engagement) = &c.links.adviser_engagement {
                *r.by_adviser_engagement.entry(engagement.clone()).or_default() += 1;
            }
            complainants.insert(&c.complainant);
        }
        r.repeat_complainants = complainants
            .iter()
            .filter(|p| self.by_complainant.get(**p).is_some_and(|ids| ids.len() > 1))
            .count() as u64;
        // Every month of the period that is over should have had its review.
        let mut month = from;
        while month_of(month) < month_of(to) {
            if !self.reviews.contains_key(&month_of(month)) {
                r.months_without_review.push(month_of(month));
            }
            month = month.checked_add_months(chrono::Months::new(1)).unwrap_or(to);
        }
        r
    }

    /// Complaints about an adviser engagement's service: existence and
    /// outcome, never the contents.
    pub fn adviser_view(&self, engagement: &str) -> Vec<AdviserView> {
        self.complaints
            .values()
            .filter(|c| c.links.adviser_engagement.as_deref() == Some(engagement))
            .map(|c| AdviserView {
                complaint: c.id.clone(),
                received_on: c.received_on,
                status: c.status,
                outcome: c.outcome.as_ref().map(|o| o.kind),
            })
            .collect()
    }

    /// The record a complainant is given under their right of access.
    pub fn subject_record(&self, complaint: &str) -> Option<SubjectRecord> {
        let c = self.complaints.get(complaint)?;
        let mut documents = vec![c.summary_document.clone()];
        documents.extend(c.acknowledgement.clone());
        documents.extend(c.outcome.as_ref().and_then(|o| o.document.clone()));
        documents.extend(c.deadlock.as_ref().and_then(|d| d.notice.clone()));
        Some(SubjectRecord {
            complaint: c.id.clone(),
            received_on: c.received_on,
            status: c.status,
            category: c.category.clone(),
            policies: c.links.policies.clone(),
            acknowledged_on: c.acknowledged_on,
            updates: c.updates.clone(),
            outcome: c.outcome.as_ref().map(|o| o.kind),
            reason_codes: c.outcome.as_ref().map(|o| o.reason_codes.clone()).unwrap_or_default(),
            documents,
            remediation: c.remediation.iter().map(|r| (r.kind, r.amount_minor)).collect(),
            forum: c.referral.as_ref().map(|r| (r.forum, r.reference.clone())),
            closed_on: c.closure.as_ref().map(|x| x.on),
        })
    }

    /// The classes of the complaint file a person may open now. Access is
    /// by the case: nobody holds it standing, and it ends when the case does.
    pub fn access(&self, complaint: &str, actor: &str) -> BTreeSet<DataClass> {
        self.complaints
            .get(complaint)
            .and_then(|c| c.grants.get(actor))
            .map(|g| g.classes.clone())
            .unwrap_or_default()
    }

    /// Every complaint a party has on record, for spotting a repeat.
    pub fn complaints_of(&self, party: &PartyId) -> Vec<String> {
        self.by_complainant.get(party).into_iter().flatten().cloned().collect()
    }

    /// The open complaints, those marked vulnerable first, then by the
    /// timer due soonest.
    pub fn queue(&self, limit: usize) -> Vec<QueueItem> {
        let mut items: Vec<QueueItem> = self
            .open
            .iter()
            .filter_map(|id| self.complaints.get(id))
            .map(|c| QueueItem {
                complaint: c.id.clone(),
                owner: c.owner.clone(),
                process: c.category.process,
                adjustments: c.adjustments.clone(),
                needs_trained_handler: c.vulnerable,
                authority_needed: c.authority_needed(),
                next_due: self.running(c).into_iter().min_by_key(|(_, due)| *due),
            })
            .collect();
        // A complaint with nothing due comes after every one that has.
        items.sort_by_key(|i| {
            (
                !i.needs_trained_handler,
                i.next_due.is_none(),
                i.next_due.map(|(_, due)| due),
                i.complaint.clone(),
            )
        });
        items.truncate(limit);
        items
    }
}
