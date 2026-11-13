//! What the module keeps: the policies it follows, the notices and tasks it
//! has raised, hardship cases and the arrangements it asked the core for.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use ic_core::{PartyId, PolicyId, Standing};
use serde::{Deserialize, Serialize};

use crate::config::{Channel, StepKind};

/// What a person may do here, as the API layer established it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A collections officer: ordinary and financial classes only.
    Collections,
    /// The hardship team, who decide hardship and may see what an
    /// application discloses about health.
    Hardship,
    /// Collections team lead: arrangements beyond an officer's limit.
    TeamLead,
    /// Sets the dunning sequence of a product.
    Configuration,
}

/// A conversation's outcome. Each drives the next step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The customer has paid or will pay now: standing moves when it lands.
    Paid,
    /// An arrangement is being proposed.
    Arrangement,
    /// Referred to the hardship team: reminders wait for the application.
    HardshipReferral,
    /// The customer wants less cover or a cheaper premium: a servicing task.
    Reduction,
    /// The customer wants to cancel: a servicing task, after the
    /// alternatives were offered.
    CancellationRequested,
    /// Nobody answered: the task stays open and is tried again.
    NoContact,
    /// The customer disputes the arrears: reminders stop and the team lead
    /// looks at it. Mandatory notices still go.
    Disputed,
    /// The customer has died: the sequence stops and claims is told.
    DeceasedNotified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relief {
    /// No premium for the period.
    Holiday,
    /// A reduced premium for the period, as a percentage of the usual one.
    Reduced { percent: u8 },
}

/// Why a policy's reminders are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    /// Referred to hardship: reminders wait until the application, or until
    /// the referral runs out.
    HardshipReferral { until: NaiveDate },
    /// Disputed: reminders wait for the team lead.
    Disputed,
    /// The policyholder has died: nothing more is sent.
    Deceased,
}

impl Hold {
    /// The hold also stops the mandatory notices.
    pub fn stops_notices(self) -> bool {
        matches!(self, Hold::Deceased)
    }
}

/// The core's arrangement on a policy, as its events described it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub id: String,
    pub made_on: NaiveDate,
    pub arrears_minor: i64,
    /// The instalment dates, for the queue's arrangements due for review.
    pub dues: Vec<NaiveDate>,
}

/// Days the lapse clock stood still under a broken arrangement, for premium
/// that fell due before it was made. The same rule as the core's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    pub due_before: NaiveDate,
    pub days: i64,
}

/// A policy the module follows, from its issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracked {
    pub number: String,
    pub party: PartyId,
    pub product: String,
    pub partition: u16,
    /// The latest premium scaled to a year.
    pub annual_premium_minor: i64,
    pub standing: Option<Standing>,
    pub oldest_unpaid_due: Option<NaiveDate>,
    #[serde(default)]
    pub pause: Option<Pause>,
    /// The step days done in this spell of arrears.
    #[serde(default)]
    pub done: BTreeSet<u32>,
    #[serde(default)]
    pub kept: Option<Kept>,
    #[serde(default)]
    pub hold: Option<Hold>,
    /// The hardship case whose relief covers the policy now.
    #[serde(default)]
    pub relief: Option<String>,
    /// No longer in force: nothing more is done.
    #[serde(default)]
    pub ended: bool,
}

impl Tracked {
    /// Days in arrears on a date, net of any pause. `None` when nothing is
    /// overdue.
    pub fn age(&self, on: NaiveDate) -> Option<i64> {
        let due = self.oldest_unpaid_due?;
        let paused = self.pause.filter(|p| due < p.due_before).map_or(0, |p| p.days);
        let age = (on - due).num_days() - paused;
        (age > 0).then_some(age)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeState {
    /// Written here, not yet taken by the documents service.
    Due,
    Sent,
    Delivered,
    Undeliverable,
}

/// A notice to a customer. One notice covers every policy of the party that
/// reached the same step on the same day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub party: PartyId,
    pub kind: StepKind,
    pub channel: Channel,
    pub template: String,
    pub on: NaiveDate,
    pub policies: Vec<PolicyId>,
    pub proof_of_delivery: bool,
    pub state: NoticeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Call the customer.
    Call,
    /// A mandatory notice could not be delivered: find another way.
    UndeliverableNotice,
    /// A hardship application waits for the hardship team.
    HardshipDecision,
    /// The customer asked for a change or a cancellation: servicing does it.
    Servicing,
    /// Arrears disputed: the team lead looks at it.
    Dispute,
    /// The customer has died: claims takes it from here.
    Deceased,
    /// An arrangement the core refused, or that broke.
    Arrangement,
}

impl TaskKind {
    /// Who works the task.
    pub fn team(self) -> Role {
        match self {
            TaskKind::HardshipDecision => Role::Hardship,
            TaskKind::Dispute => Role::TeamLead,
            _ => Role::Collections,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub party: PartyId,
    pub kind: TaskKind,
    pub policies: Vec<PolicyId>,
    pub opened: NaiveDate,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub escalated: bool,
    #[serde(default)]
    pub closed: Option<(NaiveDate, Reason)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub by: String,
    pub on: NaiveDate,
    pub approved: bool,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardshipCase {
    pub id: String,
    pub party: PartyId,
    pub policies: Vec<PolicyId>,
    pub relief: Relief,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub applied_by: String,
    pub applied_on: NaiveDate,
    /// The application says something about health: only the hardship team
    /// reads the note.
    pub discloses_health: bool,
    pub note: String,
    #[serde(default)]
    pub decision: Option<Decision>,
    #[serde(default)]
    pub ended_early: Option<NaiveDate>,
}

impl HardshipCase {
    /// The last day of relief, if it was approved.
    pub fn last_day(&self) -> Option<NaiveDate> {
        let approved = self.decision.as_ref().is_some_and(|d| d.approved);
        approved.then(|| self.ended_early.and_then(|d| d.pred_opt()).unwrap_or(self.to))
    }

    /// Relief covers this day.
    pub fn covers(&self, on: NaiveDate) -> bool {
        self.last_day().is_some_and(|last| self.from <= on && on <= last)
    }
}

/// What the module asked the core to do, until the core answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    pub id: String,
    pub policy: PolicyId,
    pub command: ic_core::Command,
    pub by: String,
}
