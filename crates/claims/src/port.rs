//! The seam between the claims module and the core.
//!
//! The module reads the policy record and sends instructions through
//! [`Core`]. In production the core implements it against the partition's
//! ledger; tests use [`crate::testing::FakeCore`].

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// A refused command. Nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Reject {
    pub code: String,
    pub message: String,
}

impl Reject {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// What the two-timeline policy record says about a cover on the event
/// date. Facts, never a verdict: the assessor decides what they mean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CoverFacts {
    /// The policy's status today, as the core names it.
    pub policy_status: String,
    /// Cover was in force for an event on the event date.
    pub in_force: bool,
    /// `paid`, `in_grace` or `suspended`, where the policy has a standing.
    pub standing: Option<String>,
    /// Premium owing today. A claim in grace is payable less this.
    pub outstanding_premium_minor: i64,
    /// Premium that was due and unpaid on the event date and has been paid
    /// since. The standing above already counts it as unpaid on that date.
    #[serde(default)]
    pub premium_paid_since_event_minor: i64,
    pub cover_type: String,
    pub cover_start: Option<NaiveDate>,
    /// The sum insured on the event date. A layer that starts after the
    /// event date is not in it, so indexation applied after an unknown death
    /// never raises what the claim pays.
    pub sum_insured_minor: i64,
    /// Exclusion codes on the layers in force for this life on the event date.
    pub exclusions: Vec<String>,
    /// The definition versions the cover is bound to.
    pub definitions: Vec<String>,
    /// The life is on the policy and was a member on the event date.
    pub life_on_policy: bool,
    /// The life is linked to a person in the customer master.
    pub life_party: Option<String>,
    /// The first day of the policy year the event date falls in. Health
    /// limits per policy year are counted from it.
    #[serde(default)]
    pub policy_year_start: Option<NaiveDate>,
    /// This life's death ends the whole policy: an individual policy on one
    /// life. Never true for a scheme or a policy with another life on it.
    #[serde(default)]
    pub ends_on_death: bool,
    pub policyholder: String,
    pub product_code: String,
    pub product_version: String,
}

/// An instruction to the core's ledger. Every one names the claim, carries
/// amounts in minor units and nothing clinical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instruction {
    /// Set the case reserve to this amount (the first estimate or an adjustment).
    SetReserve {
        claim: String,
        policy: String,
        cover: String,
        amount_minor: i64,
        reason: String,
    },
    /// Admit: move the admitted amount to benefit payable. A lump sum
    /// releases the reserve; a claim in payment keeps it for the periods ahead.
    Admit {
        claim: String,
        policy: String,
        cover: String,
        amount_minor: i64,
        release_reserve: bool,
        /// Outstanding premium taken from the claim (a claim in grace).
        deduct_premium_minor: i64,
        /// A death claim: end the policy as at the event date, in the same step.
        terminate: Option<Termination>,
    },
    /// Release a payment from benefit payable to the payments module, and
    /// close the claim if that was the last of it.
    Pay {
        claim: String,
        policy: String,
        payment_ref: String,
        amount_minor: i64,
        payee: String,
        close: bool,
        /// Interest for late payment, expensed now and paid with it.
        #[serde(default)]
        interest_minor: i64,
    },
    /// One period of a benefit in payment: admitted and paid in one entry,
    /// less any agreed offset against an overpayment.
    PayBenefit {
        claim: String,
        policy: String,
        cover: String,
        payment_ref: String,
        period_start: NaiveDate,
        period_end: NaiveDate,
        gross_minor: i64,
        offset: Option<Offset>,
        payee: String,
        /// This was the last period: close the claim in the same step.
        close: bool,
    },
    /// A payment failed or came back: the money returns to benefit payable.
    PaymentReturned { claim: String, policy: String, payment_ref: String },
    /// A goodwill payment, never mixed with contractual ones.
    ExGratia {
        claim: String,
        policy: String,
        cover: String,
        payment_ref: String,
        amount_minor: i64,
        payee: String,
    },
    /// A cost of handling the claim owed to a third party: a report fee, a
    /// rehabilitation provider. Its own expense account, never claims incurred.
    /// Pay the premiums billed for periods starting in a window from claims
    /// expense. Nothing is posted where nothing is owing.
    WaivePremium {
        claim: String,
        policy: String,
        cover: String,
        from: NaiveDate,
        /// The first day premiums are no longer waived.
        until: NaiveDate,
    },
    HandlingCost {
        claim: String,
        policy: String,
        cover: String,
        payment_ref: String,
        amount_minor: i64,
        payee: String,
        /// `evidence` or `rehabilitation`.
        kind: String,
    },
    /// Close the claim, releasing whatever reserve and unpaid payable remain,
    /// and in the same step raise what was overpaid as a receivable.
    Close {
        claim: String,
        policy: String,
        cover: String,
        outcome: String,
        overpayment: Option<(String, i64)>,
    },
    /// Reopen a closed claim with a new reserve.
    Reopen { claim: String, policy: String, reserve_minor: i64 },
    RaiseOverpayment {
        claim: String,
        policy: String,
        cover: String,
        overpayment: String,
        amount_minor: i64,
    },
    /// Money received from the debtor against an overpayment.
    RecoverOverpayment {
        claim: String,
        policy: String,
        overpayment: String,
        amount_minor: i64,
    },
    WriteOffOverpayment {
        claim: String,
        policy: String,
        overpayment: String,
        amount_minor: i64,
    },
    /// Identification at claim: link an unnamed scheme member to a person.
    IdentifyMember { policy: String, life: String, party: String },
}

impl Instruction {
    /// The policy the instruction posts to.
    pub fn policy(&self) -> &str {
        match self {
            Instruction::SetReserve { policy, .. }
            | Instruction::Admit { policy, .. }
            | Instruction::Pay { policy, .. }
            | Instruction::PayBenefit { policy, .. }
            | Instruction::PaymentReturned { policy, .. }
            | Instruction::ExGratia { policy, .. }
            | Instruction::HandlingCost { policy, .. }
            | Instruction::WaivePremium { policy, .. }
            | Instruction::Close { policy, .. }
            | Instruction::Reopen { policy, .. }
            | Instruction::RaiseOverpayment { policy, .. }
            | Instruction::RecoverOverpayment { policy, .. }
            | Instruction::WriteOffOverpayment { policy, .. }
            | Instruction::IdentifyMember { policy, .. } => policy,
        }
    }
}

/// Part of a benefit kept back against an overpayment, which may sit on
/// another claim of the same person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offset {
    pub claim: String,
    pub policy: String,
    pub overpayment: String,
    pub amount_minor: i64,
}

/// Ending a policy on a death claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Termination {
    pub event_date: NaiveDate,
    /// The payer was the deceased: later premium is refunded with the claim,
    /// never back to the source account.
    pub payer_deceased: bool,
}

/// What the claims module needs from the core while applying one command.
pub trait Core {
    fn business_date(&self) -> NaiveDate;
    /// The cover check: the facts about a cover and a life on a date.
    fn cover_facts(&self, policy: &str, cover: &str, life: Option<&str>, event_date: NaiveDate) -> Result<CoverFacts, Reject>;
    /// Post to the ledger. On a reject nothing was posted.
    fn execute(&mut self, instruction: Instruction) -> Result<(), Reject>;
    /// Publish a fact on the partition's event stream. Never clinical.
    fn notify(&mut self, kind: &str, claim: &str, detail: BTreeMap<String, String>);
    /// Count work against the partition's apply budget.
    fn work(&mut self, units: u32);
}
