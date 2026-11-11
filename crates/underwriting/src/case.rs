//! Cases: the human steps. A case has a type set by the rules, a queue by
//! skill, one owner at a time, and timers. Who may decide a case is the
//! authority module's question; a case only records what happened to it.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Queue {
    Medical,
    Financial,
    Occupational,
    TeleInterview,
    SeniorReview,
    ReinsurerLiaison,
}

impl Queue {
    pub fn key(self) -> &'static str {
        match self {
            Queue::Medical => "medical",
            Queue::Financial => "financial",
            Queue::Occupational => "occupational",
            Queue::TeleInterview => "tele_interview",
            Queue::SeniorReview => "senior_review",
            Queue::ReinsurerLiaison => "reinsurer_liaison",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseType {
    TeleInterview,
    MedicalEvidence,
    FinancialEvidence,
    Referral,
    ReinsurerReferral,
    /// The rules or an underwriter propose a decline: two signatures.
    DeclineReview,
    /// A decision waiting for its second signature.
    Countersign,
    /// Opened from a claim: was the disclosure complete and true.
    NonDisclosureReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Escalation {
    None,
    QueueLead,
    Manager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChaserFrom {
    Adviser,
    Customer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    pub from: Option<String>,
    pub to: String,
    pub at: DateTime<Utc>,
}

/// A medical officer's or other specialist's opinion, asked for as a task
/// inside the case. The opinion itself is in the vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opinion {
    pub asked_of: String,
    pub asked_at: DateTime<Utc>,
    pub vault_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub case_type: CaseType,
    pub queue: Queue,
    /// Why the case is where it is: the underwriter starts with the why.
    pub reasons: BTreeSet<String>,
    pub opened_at: DateTime<Utc>,
    pub first_touched_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub owner: Option<String>,
    pub handovers: Vec<Handover>,
    /// Everyone who has owned or signed anything on the case.
    pub touched_by: BTreeSet<String>,
    pub chasers: Vec<(ChaserFrom, DateTime<Utc>)>,
    pub escalation: Escalation,
    /// Each change of type and queue, in order.
    pub moves: Vec<(CaseType, Queue, DateTime<Utc>)>,
    pub opinions: Vec<Opinion>,
}

/// Service targets. Business settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timers {
    /// An untouched case goes to the queue lead after this long.
    pub first_touch_hours: i64,
    /// And to the underwriting manager after this long.
    pub manager_hours: i64,
    /// Cases one person may own at once.
    pub work_in_progress_limit: usize,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            first_touch_hours: 8,
            manager_hours: 24,
            work_in_progress_limit: 12,
        }
    }
}

impl Case {
    pub fn open(id: String, case_type: CaseType, queue: Queue, reasons: BTreeSet<String>, at: DateTime<Utc>) -> Self {
        Self {
            id,
            case_type,
            queue,
            reasons,
            opened_at: at,
            first_touched_at: None,
            closed_at: None,
            owner: None,
            handovers: vec![],
            touched_by: BTreeSet::new(),
            chasers: vec![],
            escalation: Escalation::None,
            moves: vec![(case_type, queue, at)],
            opinions: vec![],
        }
    }

    pub fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }

    /// Give the case to `to`. The first assignment is the first touch.
    pub fn assign(&mut self, to: &str, at: DateTime<Utc>) {
        self.handovers.push(Handover {
            from: self.owner.take(),
            to: to.to_string(),
            at,
        });
        self.owner = Some(to.to_string());
        self.touched_by.insert(to.to_string());
        self.first_touched_at.get_or_insert(at);
    }

    /// Change what the case needs. The owner is released: the next person
    /// must be someone the new queue can offer it to.
    pub fn move_to(&mut self, case_type: CaseType, queue: Queue, reasons: impl IntoIterator<Item = String>, at: DateTime<Utc>) {
        self.case_type = case_type;
        self.queue = queue;
        self.reasons.extend(reasons);
        self.owner = None;
        self.moves.push((case_type, queue, at));
    }

    pub fn close(&mut self, at: DateTime<Utc>) {
        self.closed_at.get_or_insert(at);
        self.owner = None;
    }

    /// Where an untouched case should have escalated to by `now`.
    pub fn escalation_due(&self, timers: &Timers, now: DateTime<Utc>) -> Escalation {
        if !self.is_open() || self.first_touched_at.is_some() {
            return self.escalation;
        }
        let waited = now - self.opened_at;
        if waited >= Duration::hours(timers.manager_hours) {
            Escalation::Manager
        } else if waited >= Duration::hours(timers.first_touch_hours) {
            Escalation::QueueLead.max(self.escalation)
        } else {
            self.escalation
        }
    }

    /// Higher is worked sooner. Sum insured, the age of the case, interim
    /// cover in force, chasers and escalation all raise it.
    pub fn priority(&self, sum_insured_minor: i64, interim_cover: bool, now: DateTime<Utc>) -> i64 {
        let age_hours = (now - self.opened_at).num_hours().max(0);
        let size = (sum_insured_minor / 100_000_00).min(50);
        age_hours
            + size
            + if interim_cover { 24 } else { 0 }
            + 12 * self.chasers.len() as i64
            + match self.escalation {
                Escalation::None => 0,
                Escalation::QueueLead => 48,
                Escalation::Manager => 96,
            }
    }
}
