//! The dunning sequence for a product, and the rules it must keep.
//!
//! The days, channels and templates are the business's to set. What a
//! sequence may not do is drop or shorten the notices the law and the
//! conduct regime require: a suspension notice and a final notice, both sent
//! with proof of delivery, the final one at least the product's minimum
//! notice before the lapse day, and that minimum never below
//! [`NOTICE_FLOOR_DAYS`].

use serde::{Deserialize, Serialize};

use crate::machine::Reject;

/// The shortest notice of lapse any product may give. A product may set a
/// longer one. To be confirmed by legal and conduct review with the
/// sequence days themselves.
pub const NOTICE_FLOOR_DAYS: u32 = 14;

/// The most steps one sequence may have.
pub const MAX_STEPS: usize = 20;

/// The longest arrangement any product may allow, in instalments: the core
/// takes no more.
pub const MAX_ARRANGEMENT_INSTALMENTS: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// A reminder to the customer.
    Reminder,
    /// A task for a collections officer to call the customer. The customer's
    /// adviser is told at the same step.
    CallTask,
    /// The notice that cover is suspended. Mandatory.
    SuspensionNotice,
    /// The notice that the policy will lapse. Mandatory.
    FinalNotice,
}

impl StepKind {
    /// A notice the sequence may not leave out, sent with proof of delivery.
    pub fn mandatory(self) -> bool {
        matches!(self, StepKind::SuspensionNotice | StepKind::FinalNotice)
    }

    pub fn name(self) -> &'static str {
        match self {
            StepKind::Reminder => "reminder",
            StepKind::CallTask => "call_task",
            StepKind::SuspensionNotice => "suspension_notice",
            StepKind::FinalNotice => "final_notice",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Email,
    Sms,
    Post,
    App,
    /// A task on the collections queue, for a call step.
    Queue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Days after the oldest unpaid premium fell due.
    pub day: u32,
    pub kind: StepKind,
    pub channel: Channel,
    /// The document template the notice is written from.
    pub template: String,
}

/// A call made earlier than the sequence's call step, for a policy whose
/// annual premium is at least this much.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EarlyCall {
    pub annual_premium_minor: i64,
    pub day: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sequence {
    pub product: String,
    /// The product's lapse day: days after the due date on which the core
    /// lapses an unpaid policy. It must agree with the product's arrears
    /// feature.
    pub lapse_day: u32,
    /// The least notice of lapse this product gives.
    pub minimum_notice_days: u32,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub early_call: Option<EarlyCall>,
    /// Days a call task may stay open before it goes to the team lead.
    pub call_escalate_days: u32,
    /// The most instalments an officer may agree without a team lead.
    pub max_arrangement_instalments: u32,
}

fn bad(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

impl Sequence {
    /// Refuse a sequence that would skip or shorten a mandatory notice, or
    /// that cannot be read the one way.
    pub fn validate(&self) -> Result<(), Reject> {
        let plain = !self.product.is_empty() && self.product.len() <= 32 && self.product.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !plain {
            return Err(bad("invalid_sequence", "a sequence names its product by the product code"));
        }
        if self.steps.is_empty() || self.steps.len() > MAX_STEPS {
            return Err(bad("invalid_sequence", format!("a sequence has between one and {MAX_STEPS} steps")));
        }
        if self.steps.windows(2).any(|w| w[0].day >= w[1].day) {
            return Err(bad("invalid_sequence", "steps are on different days, in ascending order"));
        }
        if let Some(step) = self.steps.iter().find(|s| s.day == 0 || s.day >= self.lapse_day) {
            return Err(bad(
                "invalid_sequence",
                format!("a step on day {} is not between the due date and the lapse day {}", step.day, self.lapse_day),
            ));
        }
        if self.steps.iter().any(|s| s.template.trim().is_empty()) {
            return Err(bad("invalid_sequence", "every step names its template"));
        }
        if self.steps.iter().any(|s| (s.kind == StepKind::CallTask) != (s.channel == Channel::Queue)) {
            return Err(bad("invalid_sequence", "a call step goes to the queue, and only a call step does"));
        }
        if self.minimum_notice_days < NOTICE_FLOOR_DAYS {
            return Err(bad(
                "notice_too_short",
                format!("the minimum notice of lapse is at least {NOTICE_FLOOR_DAYS} days"),
            ));
        }
        let only = |kind: StepKind| -> Result<&Step, Reject> {
            let mut found = self.steps.iter().filter(|s| s.kind == kind);
            match (found.next(), found.next()) {
                (Some(step), None) => Ok(step),
                (None, _) => Err(bad("missing_mandatory_notice", format!("a sequence must send the {}", kind.name()))),
                (Some(_), Some(_)) => Err(bad("invalid_sequence", format!("a sequence sends the {} once", kind.name()))),
            }
        };
        let suspension = only(StepKind::SuspensionNotice)?;
        let last = only(StepKind::FinalNotice)?;
        if suspension.day > last.day {
            return Err(bad("invalid_sequence", "the suspension notice comes before the final notice"));
        }
        if self.lapse_day - last.day < self.minimum_notice_days {
            return Err(bad(
                "notice_too_short",
                format!(
                    "the final notice on day {} gives {} days before lapse; the product's minimum is {}",
                    last.day,
                    self.lapse_day - last.day,
                    self.minimum_notice_days
                ),
            ));
        }
        if self
            .steps
            .iter()
            .any(|s| s.kind.mandatory() && !matches!(s.channel, Channel::Post | Channel::Email))
        {
            return Err(bad("invalid_sequence", "a mandatory notice goes by post or email, with proof of delivery"));
        }
        if let Some(early) = self.early_call {
            let call = self.steps.iter().find(|s| s.kind == StepKind::CallTask);
            if early.annual_premium_minor <= 0 || early.day == 0 || call.is_none_or(|c| early.day >= c.day) {
                return Err(bad(
                    "invalid_sequence",
                    "an early call is for a positive premium, before the sequence's own call step",
                ));
            }
        }
        if self.max_arrangement_instalments == 0 || self.max_arrangement_instalments > MAX_ARRANGEMENT_INSTALMENTS {
            return Err(bad(
                "invalid_sequence",
                format!("an arrangement allows between one and {MAX_ARRANGEMENT_INSTALMENTS} instalments"),
            ));
        }
        if self.call_escalate_days == 0 {
            return Err(bad("invalid_sequence", "a call task escalates after at least a day"));
        }
        Ok(())
    }

    /// The day a policy with this annual premium is first called, if the
    /// sequence calls at all.
    pub fn call_day(&self, annual_premium_minor: i64) -> Option<u32> {
        let own = self.steps.iter().find(|s| s.kind == StepKind::CallTask).map(|s| s.day)?;
        Some(match self.early_call {
            Some(early) if annual_premium_minor >= early.annual_premium_minor => early.day,
            _ => own,
        })
    }

    /// The day a step falls on for a policy with this annual premium: a
    /// call step moves earlier for a high premium.
    pub fn day_of(&self, step: &Step, annual_premium_minor: i64) -> u32 {
        if step.kind == StepKind::CallTask {
            self.call_day(annual_premium_minor).unwrap_or(step.day)
        } else {
            step.day
        }
    }
}

/// Best-estimate dunning sequences for every shipped product, as
/// `config/collections/sequences.json` holds them. Nobody has approved
/// them: the collections console offers one as the starting point when a
/// configuration person sets a product's sequence, and that person is the
/// one who sets it.
pub const SHIPPED_SEQUENCES: &str = include_str!("../../../config/collections/sequences.json");

/// The shipped best-estimate sequences, each checked as `SetSequence`
/// would check it.
pub fn shipped_sequences() -> Result<Vec<Sequence>, String> {
    parse_sequences(SHIPPED_SEQUENCES)
}

/// Sequences from a file shaped like `config/collections/sequences.json`
/// (`{"sequences": [...]}`), each checked as `SetSequence` would check it.
pub fn parse_sequences(text: &str) -> Result<Vec<Sequence>, String> {
    #[derive(Deserialize)]
    struct File {
        sequences: Vec<Sequence>,
    }
    let file: File = match serde_json::from_str(text) {
        Ok(file) => file,
        Err(e) => return Err(format!("not a file of sequences: {e}")),
    };
    for sequence in &file.sequences {
        if let Err(refused) = sequence.validate() {
            return Err(format!("the sequence for {}: {}", sequence.product, refused.message));
        }
    }
    Ok(file.sequences)
}
