//! Commands: the only way claims state changes.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::config::{AuthorityBook, Rules};
use crate::model::{Channel, ClaimType, DecisionKind, EndReason, Flags, HealthItem, Notifier, OverpaymentCause, PayeeKind, RecoveryStatus, RunKind, TaskKind};
use crate::StaffId;

/// Who is acting and why. A purpose is declared on every call that reads
/// personal data or decides anything, and is recorded with what was done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub staff: StaffId,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub claim: String,
    pub policy: String,
    pub cover: String,
    #[serde(default)]
    pub life: Option<String>,
    pub claim_type: ClaimType,
    pub event_date: NaiveDate,
    pub channel: Channel,
    pub notifier: Notifier,
    #[serde(default)]
    pub claimant: Option<String>,
    #[serde(default)]
    pub claimed_minor: Option<i64>,
    #[serde(default)]
    pub flags: Flags,
    #[serde(default)]
    pub health: Option<HealthItem>,
    /// Who underwrote the life, from the underwriting module.
    #[serde(default)]
    pub underwriters: Vec<StaffId>,
    #[serde(default)]
    pub catastrophe_event: Option<String>,
    /// The cause, where the channel captured one (a provider's invoice does).
    /// Clinical: it stays in the claims module.
    #[serde(default)]
    pub cause_code: Option<String>,
}

/// The terms of a benefit in payment, set at admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Periodic {
    pub monthly_minor: i64,
    /// The first day benefit is payable for, after the waiting period.
    pub first_period: NaiveDate,
    /// The first day no benefit is payable for.
    pub end: NaiveDate,
    pub expected_end: NaiveDate,
}

/// A decision asked for. Each kind has its own authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    Reserve {
        amount_minor: i64,
    },
    Admit {
        /// The amount for a lump sum. Left out, it is the sum insured on the
        /// event date less earlier payments on the cover.
        #[serde(default)]
        amount_minor: Option<i64>,
        #[serde(default)]
        periodic: Option<Periodic>,
        /// A reason code for admitting although the rules raised an exclusion.
        #[serde(default)]
        exclusion_override: Option<String>,
        /// The assessor has read an exclusion the rules could not decide.
        #[serde(default)]
        exclusion_reviewed: bool,
        /// A death claim where the premium payer was the deceased: premium
        /// taken after the death is refunded with the claim.
        #[serde(default)]
        payer_deceased: bool,
    },
    Decline,
    Withdraw,
    /// Overturn the insurer's own admission. The benefit stops from the
    /// decision date and what was paid is recorded as an overpayment, held.
    ReverseAdmission {
        /// The admission rested on a false declaration by the claimant. The
        /// overpayment is then not the insurer's own error.
        #[serde(default)]
        false_declaration: bool,
    },
    ExGratia {
        amount_minor: i64,
        payment_ref: String,
    },
    EndBenefit {
        end: NaiveDate,
        reason: EndReason,
    },
    RecoveryStatus {
        overpayment: String,
        status: RecoveryStatus,
        #[serde(default)]
        offset_pct: u32,
        #[serde(default)]
        instalment_minor: i64,
    },
    WriteOff {
        overpayment: String,
    },
}

impl Request {
    pub fn kind(&self) -> DecisionKind {
        match self {
            Request::Reserve { .. } => DecisionKind::Reserve,
            Request::Admit { .. } => DecisionKind::Admit,
            Request::Decline => DecisionKind::Decline,
            Request::Withdraw => DecisionKind::Withdraw,
            Request::ReverseAdmission { .. } => DecisionKind::ReverseAdmission,
            Request::ExGratia { .. } => DecisionKind::ExGratia,
            Request::EndBenefit { .. } => DecisionKind::EndBenefit,
            Request::RecoveryStatus { .. } => DecisionKind::RecoveryStatus,
            Request::WriteOff { .. } => DecisionKind::WriteOff,
        }
    }
}

/// Why a closed claim is opened again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReopenBasis {
    /// An internal review overturned the decision.
    InternalReview,
    /// The external dispute scheme overturned it.
    DisputeScheme,
    /// The same condition came back inside the product's window.
    Recurrence,
    NewEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonDisclosureOutcome {
    NoAction,
    Vary,
    Avoid,
}

/// What the reinsurance module tells claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReinsuranceNotice {
    LargeClaimNotification {
        claim: String,
        treaty: String,
    },
    ReinsurerConsentRequired {
        claim: String,
        treaty: String,
        due_by: NaiveDate,
    },
    ReinsurerConsent {
        claim: String,
        reinsurer: String,
        agreed: bool,
        reference: String,
        /// The treaty the consent answers. Left out, it answers every one.
        #[serde(default)]
        treaty: Option<String>,
    },
    DisputedItemRaised {
        claim: String,
        reinsurer: String,
        reference: String,
    },
    /// People settled a disputed item under the treaty. `repay_minor` is
    /// what the insurer repays the reinsurer, which may be nothing.
    DisputeResolved {
        claim: String,
        reference: String,
        repay_minor: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    // --- configuration ---
    ActivateAuthority {
        book: Box<AuthorityBook>,
    },
    ActivateRules {
        rules: Box<Rules>,
    },

    // --- intake ---
    Notify(Box<Notification>),
    /// Load a claim from the system before this one, at the step it had
    /// reached: open, in payment, closed or declined. Sent by the migration
    /// load only. Health limits already used travel with a closed claim.
    LoadMigrated {
        claim: Box<crate::Claim>,
        #[serde(default)]
        health_used: std::collections::BTreeMap<String, i64>,
    },
    Acknowledge {
        claim: String,
        /// Proof of delivery from the notifications service.
        delivery_ref: String,
    },
    Assign {
        claim: String,
        to: StaffId,
        actor: Actor,
    },
    RecordContact {
        claim: String,
        actor: Actor,
    },
    /// Move a claim to the senior queue.
    Escalate {
        claim: String,
        actor: Actor,
    },
    /// Identification at claim: say who an unnamed scheme member is.
    IdentifyLife {
        claim: String,
        party: String,
        actor: Actor,
    },

    // --- assessment ---
    RecordCause {
        claim: String,
        cause_code: String,
        actor: Actor,
    },
    CoverCheck {
        claim: String,
        relied_on: Vec<String>,
        actor: Actor,
    },
    RequestEvidence {
        claim: String,
        evidence: String,
        kind: String,
        from: String,
        actor: Actor,
    },
    ReceiveEvidence {
        claim: String,
        evidence: String,
        vault_ref: String,
        #[serde(default)]
        cost_minor: i64,
    },
    OpenTask {
        claim: String,
        task: String,
        kind: TaskKind,
        #[serde(default)]
        due: Option<NaiveDate>,
        actor: Actor,
    },
    CompleteTask {
        claim: String,
        task: String,
        outcome: String,
        #[serde(default)]
        attendees: Vec<StaffId>,
        #[serde(default)]
        vault_ref: Option<String>,
        #[serde(default)]
        spend_minor: i64,
        actor: Actor,
    },
    /// Open the clinical file. Every view is logged with its purpose.
    OpenFile {
        claim: String,
        actor: Actor,
    },

    // --- decisions ---
    Decide {
        claim: String,
        request: Request,
        #[serde(default)]
        reasons: Vec<String>,
        actor: Actor,
    },
    Approve {
        claim: String,
        actor: Actor,
    },
    Refuse {
        claim: String,
        actor: Actor,
    },
    Reopen {
        claim: String,
        basis: ReopenBasis,
        reserve_minor: i64,
        actor: Actor,
    },

    // --- paying ---
    VerifyPayee {
        claim: String,
        party: String,
        kind: PayeeKind,
        account_ref: String,
        name_check_ref: String,
        screening_ref: String,
        #[serde(default)]
        authority_doc: Option<String>,
        actor: Actor,
    },
    Pay {
        claim: String,
        payment_ref: String,
        amount_minor: i64,
        /// The returned payment this one replaces.
        #[serde(default)]
        reissue_of: Option<String>,
        actor: Actor,
    },
    /// Pay the recorded cost of a piece of evidence or a rehabilitation
    /// task to whoever provided it. Paid again under a new reference if the
    /// bank sent the first payment back.
    PayCost {
        claim: String,
        payment_ref: String,
        /// The evidence ID or the rehabilitation task ID.
        item: String,
        /// Who is paid, as the payments module knows them.
        provider: String,
        actor: Actor,
    },
    /// Pay a lender or assignee what their recorded interest takes of the
    /// proceeds. They are paid before the claimant, in priority order.
    PayInterest {
        claim: String,
        payment_ref: String,
        party: String,
        actor: Actor,
    },
    /// Start waiving premiums on the claim's policy while its benefit is in
    /// payment, where the cover includes waiver of premium.
    StartWaiver {
        claim: String,
        /// Premiums for periods starting on or after this date are waived.
        from: NaiveDate,
        actor: Actor,
    },
    PaymentReturned {
        claim: String,
        payment_ref: String,
    },
    DeclareIncome {
        claim: String,
        from: NaiveDate,
        other_income_minor: i64,
        actor: Actor,
    },
    PrepareRun {
        run: String,
        kind: RunKind,
        benefit_date: NaiveDate,
        actor: Actor,
    },
    ApproveRun {
        run: String,
        actor: Actor,
    },
    CancelRun {
        run: String,
        actor: Actor,
    },
    /// The bank statement's total for a submitted run.
    ConfirmRun {
        run: String,
        settled_minor: i64,
    },

    // --- overpayments ---
    RaiseOverpayment {
        claim: String,
        overpayment: String,
        amount_minor: i64,
        cause: OverpaymentCause,
        good_faith: bool,
        actor: Actor,
    },
    RecordRepayment {
        claim: String,
        overpayment: String,
        amount_minor: i64,
    },

    // --- controls and other modules ---
    /// The standing control: claims in payment whose recorded cause matches
    /// an exclusion on the paying cover. Run in chunks, on a schedule.
    ExclusionControl {
        #[serde(default)]
        after: Option<String>,
    },
    /// A person has died. Benefits paid past the date are overpayments.
    RecordDeath {
        party: String,
        date_of_death: NaiveDate,
    },
    Reinsurance(ReinsuranceNotice),
    NonDisclosureOutcome {
        claim: String,
        review_ref: String,
        outcome: NonDisclosureOutcome,
    },
    /// Work through whatever has fallen due and not yet been handled.
    DrainDue,
}

impl Command {
    /// The modules allowed to send each command.
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            Command::ActivateAuthority { .. } | Command::ActivateRules { .. } => &["config"],
            Command::PaymentReturned { .. } | Command::ConfirmRun { .. } | Command::RecordRepayment { .. } => &["payments", "claims"],
            Command::Decide { .. } | Command::Approve { .. } | Command::Refuse { .. } => &["claims", "held-balances"],
            Command::Reinsurance(_) => &["reinsurance"],
            Command::NonDisclosureOutcome { .. } => &["underwriting"],
            Command::ExclusionControl { .. } | Command::DrainDue => &["claims", "end-of-day"],
            Command::RecordDeath { .. } => &["claims", "customer-master"],
            Command::LoadMigrated { .. } => &["migration"],
            _ => &["claims"],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Command::ActivateAuthority { .. } => "activate_authority",
            Command::ActivateRules { .. } => "activate_rules",
            Command::Notify(_) => "notify",
            Command::Acknowledge { .. } => "acknowledge",
            Command::Assign { .. } => "assign",
            Command::RecordContact { .. } => "record_contact",
            Command::Escalate { .. } => "escalate",
            Command::IdentifyLife { .. } => "identify_life",
            Command::RecordCause { .. } => "record_cause",
            Command::CoverCheck { .. } => "cover_check",
            Command::RequestEvidence { .. } => "request_evidence",
            Command::ReceiveEvidence { .. } => "receive_evidence",
            Command::OpenTask { .. } => "open_task",
            Command::CompleteTask { .. } => "complete_task",
            Command::OpenFile { .. } => "open_file",
            Command::Decide { .. } => "decide",
            Command::Approve { .. } => "approve",
            Command::Refuse { .. } => "refuse",
            Command::Reopen { .. } => "reopen",
            Command::VerifyPayee { .. } => "verify_payee",
            Command::Pay { .. } => "pay",
            Command::PayCost { .. } => "pay_cost",
            Command::StartWaiver { .. } => "start_waiver",
            Command::PayInterest { .. } => "pay_interest",
            Command::PaymentReturned { .. } => "payment_returned",
            Command::DeclareIncome { .. } => "declare_income",
            Command::PrepareRun { .. } => "prepare_run",
            Command::ApproveRun { .. } => "approve_run",
            Command::CancelRun { .. } => "cancel_run",
            Command::ConfirmRun { .. } => "confirm_run",
            Command::RaiseOverpayment { .. } => "raise_overpayment",
            Command::RecordRepayment { .. } => "record_repayment",
            Command::ExclusionControl { .. } => "exclusion_control",
            Command::RecordDeath { .. } => "record_death",
            Command::LoadMigrated { .. } => "load_migrated",
            Command::Reinsurance(_) => "reinsurance",
            Command::NonDisclosureOutcome { .. } => "non_disclosure_outcome",
            Command::DrainDue => "drain_due",
        }
    }
}
