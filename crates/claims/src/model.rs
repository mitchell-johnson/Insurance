//! The claim record and everything that hangs off it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::config::{Line, Match, Queue};
use crate::port::{CoverFacts, Offset};
use crate::StaffId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    Death,
    TerminalIllness,
    Trauma,
    TotalPermanentDisability,
    IncomeProtection,
    Health,
    PremiumWaiver,
}

impl ClaimType {
    pub fn line(self) -> Line {
        match self {
            ClaimType::Health => Line::Health,
            ClaimType::IncomeProtection | ClaimType::PremiumWaiver => Line::IncomeProtection,
            _ => Line::LumpSum,
        }
    }
    /// Paid as a regular benefit, not a lump sum.
    pub fn periodic(self) -> bool {
        matches!(self, ClaimType::IncomeProtection | ClaimType::PremiumWaiver)
    }
    pub fn code(self) -> &'static str {
        match self {
            ClaimType::Death => "death",
            ClaimType::TerminalIllness => "terminal_illness",
            ClaimType::Trauma => "trauma",
            ClaimType::TotalPermanentDisability => "total_permanent_disability",
            ClaimType::IncomeProtection => "income_protection",
            ClaimType::Health => "health",
            ClaimType::PremiumWaiver => "premium_waiver",
        }
    }
}

/// How the notification arrived. Every channel lands as the same command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    App,
    AdviserPortal,
    EmployerPortal,
    Provider,
    Phone,
    Letter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notifier {
    Policyholder,
    LifeInsured,
    Beneficiary,
    Executor,
    Employer,
    Adviser,
    Provider,
}

/// Features that change how a claim is routed and how fast it moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Flags {
    #[serde(default)]
    pub vulnerable: bool,
    #[serde(default)]
    pub hardship: bool,
    #[serde(default)]
    pub provider_waiting: bool,
    #[serde(default)]
    pub contested_cause: bool,
    #[serde(default)]
    pub non_disclosure_question: bool,
    #[serde(default)]
    pub multiple_policies: bool,
    #[serde(default)]
    pub overseas_event: bool,
    /// A group scheme claim: the employer sees status only.
    #[serde(default)]
    pub group: bool,
}

impl Flags {
    pub fn complex(&self) -> bool {
        self.contested_cause || self.non_disclosure_question || self.multiple_policies || self.overseas_event
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Notified and being assessed.
    Open,
    /// Admitted, with money still to pay.
    Admitted,
    /// A benefit in payment.
    InPayment,
    Paid,
    Declined,
    Withdrawn,
    /// A benefit in payment that has ended.
    Ended,
    /// An admission the insurer overturned.
    Reversed,
}

impl Status {
    pub fn is_open(self) -> bool {
        matches!(self, Status::Open | Status::Admitted | Status::InPayment)
    }
    pub fn code(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Admitted => "admitted",
            Status::InPayment => "in_payment",
            Status::Paid => "paid",
            Status::Declined => "declined",
            Status::Withdrawn => "withdrawn",
            Status::Ended => "benefit_ended",
            Status::Reversed => "admission_reversed",
        }
    }
}

/// The kinds of decision, for reason codes, sampling and reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Reserve,
    Admit,
    Decline,
    Withdraw,
    /// The insurer overturning its own admission. Reported apart from declines.
    ReverseAdmission,
    ExGratia,
    EndBenefit,
    RecoveryStatus,
    WriteOff,
}

/// What a second signer must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// Someone whose own profile covers the whole decision.
    Authority,
    ConductOfficer,
    Legal,
}

/// Why the earlier admission was wrong, read from the decision record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finding {
    /// The rules did not map the cause to the exclusion code.
    RulesUnmapped,
    /// The rules raised the exclusion and the assessor overrode it.
    AssessorOverride,
    /// The cause was recorded in a form the rules did not match.
    CauseMiscoded,
}

impl Finding {
    pub fn code(self) -> &'static str {
        match self {
            Finding::RulesUnmapped => "rules_unmapped",
            Finding::AssessorOverride => "assessor_override",
            Finding::CauseMiscoded => "cause_miscoded",
        }
    }
}

/// A decision as made: everything needed to reproduce and explain it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub no: u32,
    pub kind: DecisionKind,
    pub amount_minor: i64,
    /// Reason codes from the controlled list. Clinical: never leaves the module.
    pub reasons: Vec<String>,
    pub decided_on: NaiveDate,
    /// The proposer first, then each further signer. Empty for the rules.
    pub signers: Vec<StaffId>,
    pub straight_through: bool,
    pub rules_version: u32,
    pub authority_version: u32,
    /// The proposer's profile, as named in that authority version.
    pub profile: String,
    /// The cause code the decision was made on.
    pub cause_code: Option<String>,
    /// What the rules made of each exclusion on the cover.
    pub exclusion_outcomes: BTreeMap<String, Match>,
    /// The reason code given for admitting over a raised exclusion.
    pub exclusion_override: Option<String>,
    /// For a reversal: why the earlier admission was wrong.
    pub finding: Option<Finding>,
    /// The definition versions and facts relied on, from the cover check.
    pub definitions: Vec<String>,
    pub relied_on: Vec<String>,
}

/// The cover check as recorded: the facts and which of them were relied on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverCheck {
    pub checked_on: NaiveDate,
    pub facts: CoverFacts,
    pub relied_on: Vec<String>,
    pub by: StaffId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// The conversation with the claimant before a decline or a reversal.
    ClaimantConversation,
    CaseConference,
    MedicalOpinion,
    /// Evidence from the employer that a person was a member.
    MembershipEvidence,
    Rehabilitation,
    MedicalReview,
    IncomeDeclaration,
    ReturnToWork,
    PaymentFailure,
    SeniorReview,
    QualityReview,
    NonDisclosureReview,
    RepaymentArrangement,
    ReinsurerConsent,
    /// A hit from the exclusion-match control.
    ExclusionMatch,
    /// A death notified late: check the cadence of the death register feed.
    DeathRegisterCadence,
    /// The periodic check that a person paid for life is still alive.
    ProofOfLife,
}

impl TaskKind {
    pub fn review(self) -> bool {
        matches!(
            self,
            TaskKind::MedicalReview | TaskKind::IncomeDeclaration | TaskKind::ReturnToWork | TaskKind::ProofOfLife
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub kind: TaskKind,
    pub opened_on: NaiveDate,
    pub due: Option<NaiveDate>,
    pub completed_on: Option<NaiveDate>,
    pub completed_by: Option<StaffId>,
    /// A code for the outcome. Words stay in the clinical store.
    pub outcome: Option<String>,
    pub attendees: Vec<StaffId>,
    /// Where the question and the answer are held, for a medical opinion.
    pub vault_ref: Option<String>,
    /// Money spent within the claim on this task (rehabilitation).
    pub spend_minor: i64,
    pub overdue: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    /// What is asked for, as a code: `death_certificate`, `medical_report`.
    pub kind: String,
    pub from: String,
    pub requested_on: NaiveDate,
    pub due: NaiveDate,
    pub received_on: Option<NaiveDate>,
    pub vault_ref: Option<String>,
    pub cost_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerKind {
    Acknowledgement,
    FirstContact,
    Decision,
    Payment,
}

/// One service timer: when it is due and when it was met.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timer {
    pub due: NaiveDate,
    pub met_on: Option<NaiveDate>,
    /// How far it has been escalated: 1 owner, 2 queue lead, 3 claims manager.
    pub escalated: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayeeKind {
    Policyholder,
    Beneficiary,
    Estate,
    Trust,
    Assignee,
    Provider,
}

/// Who is paid, once verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payee {
    pub party: String,
    pub kind: PayeeKind,
    pub account_ref: String,
    /// The payee name check and the compliance screening that passed.
    pub name_check_ref: String,
    pub screening_ref: String,
    /// Proof of legal authority, for an estate above the simplified limit.
    pub authority_doc: Option<String>,
    pub verified_on: NaiveDate,
    pub verified_by: StaffId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub payment_ref: String,
    pub amount_minor: i64,
    pub paid_on: NaiveDate,
    pub ex_gratia: bool,
    pub returned: bool,
    pub released_by: Option<StaffId>,
    /// The payment run it went out in, if it was not released by hand.
    pub run: Option<String>,
    /// The evidence or rehabilitation task this pays for, where the payment
    /// is a cost of handling the claim and not a benefit.
    #[serde(default)]
    pub cost_item: Option<String>,
    /// Interest for late payment sent with it, on top of the amount.
    #[serde(default)]
    pub interest_minor: i64,
    /// Of that interest, what the payee declined: reversed, not paid.
    #[serde(default)]
    pub interest_declined_minor: i64,
    /// The interested party it went to, where it was not paid to the payee.
    #[serde(default)]
    pub interested_party: Option<String>,
}

/// What an interested party is owed from this claim's proceeds, fixed at
/// the admission from the interests then in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterestDue {
    pub party: String,
    pub priority: u32,
    pub owed_minor: i64,
    /// Released to them and not returned.
    pub paid_minor: i64,
    /// Owed as a nominated beneficiary's share, not as a lender's interest.
    #[serde(default)]
    pub nominated: bool,
}

/// One paid period of a benefit in payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    /// The first day after the period.
    pub end: NaiveDate,
    /// The benefit for the period, after income offsets.
    pub gross_minor: i64,
    /// Taken from it against an overpayment.
    pub offset_minor: i64,
    pub payment_ref: String,
    pub run: String,
    pub returned: bool,
    /// The part of `gross_minor` that made good a shortfall on earlier
    /// periods. It is not benefit for this period.
    #[serde(default)]
    pub arrears_minor: i64,
    /// Other income declared for the period (accident compensation, a
    /// part-time wage) that reduced the benefit, as known when it was paid.
    #[serde(default)]
    pub other_income_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Recovered,
    BenefitPeriodEnd,
    ExpiryAge,
    Death,
}

/// A benefit in payment: a schedule, like a loan in reverse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub monthly_minor: i64,
    /// The first day of the next period to pay.
    pub next_period: NaiveDate,
    /// The first day no benefit is payable for.
    pub end: NaiveDate,
    /// When the claim was expected to end, for the book view.
    pub expected_end: NaiveDate,
    /// Other income declared, by the first day it applies from.
    pub income: BTreeMap<NaiveDate, i64>,
    pub periods: BTreeMap<NaiveDate, Period>,
    /// The next review of each kind.
    pub reviews: BTreeMap<TaskKind, NaiveDate>,
    pub reviews_overdue: u32,
    pub case_manager: Option<StaffId>,
    /// The contractual end of the benefit period, whatever happens before it.
    pub benefit_end: NaiveDate,
    pub ended: Option<EndReason>,
    /// Owed to the claimant for periods already paid too little. Paid with
    /// the next benefit, through the same prepared and approved run.
    #[serde(default)]
    pub arrears_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    /// Recorded as a fact. Nothing is asked of anyone.
    Held,
    Pursued,
    Arrangement,
    Offset,
    WrittenOff,
    Recovered,
}

impl RecoveryStatus {
    pub fn code(self) -> &'static str {
        match self {
            RecoveryStatus::Held => "held",
            RecoveryStatus::Pursued => "pursued",
            RecoveryStatus::Arrangement => "arrangement",
            RecoveryStatus::Offset => "offset",
            RecoveryStatus::WrittenOff => "written_off",
            RecoveryStatus::Recovered => "recovered",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverpaymentCause {
    /// The insurer's own mistake, such as an admission made in error.
    InsurerError,
    /// Something the claimant told us late: income, a return to work.
    LateInformation,
    /// A false declaration. Recovery also goes to the compliance module.
    ClaimantFalseDeclaration,
    /// A death notified after benefits were paid past it. Recovered from the estate.
    LateDeathNotification,
    ProviderBilling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overpayment {
    pub id: String,
    pub raised_on: NaiveDate,
    pub amount_minor: i64,
    pub recovered_minor: i64,
    pub written_off_minor: i64,
    pub status: RecoveryStatus,
    pub cause: OverpaymentCause,
    /// The recipient acted in good faith.
    pub good_faith: bool,
    /// Share of each benefit taken, while the status is offset.
    pub offset_pct: u32,
    /// The agreed instalment, while the status is arrangement.
    pub instalment_minor: i64,
    /// Every status it has held, with the decision that set it.
    pub history: Vec<(NaiveDate, RecoveryStatus, u32)>,
    /// How many times its position has been shared with other partitions.
    #[serde(default)]
    pub shared_seq: u64,
    /// The position last shared: outstanding, offset share and what has
    /// been received from other partitions in all. Nothing is sent again
    /// until one of them changes.
    #[serde(default)]
    pub shared: Option<(i64, u32, i64)>,
    /// Offsets taken by other partitions and dealt with here, applied or
    /// returned, by partition: what each has been told is settled.
    #[serde(default)]
    pub remote_received: BTreeMap<u16, i64>,
}

impl Overpayment {
    pub fn outstanding_minor(&self) -> i64 {
        self.amount_minor - self.recovered_minor - self.written_off_minor
    }
    /// The insurer's own error and an innocent recipient: held by default,
    /// and only the conduct officer and legal together can change that.
    pub fn protected(&self) -> bool {
        self.cause == OverpaymentCause::InsurerError && self.good_faith
    }
}

/// A decision waiting for its further signatures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub request: crate::command::Request,
    pub reasons: Vec<String>,
    pub proposer: StaffId,
    pub proposed_on: NaiveDate,
    pub needs: Vec<Need>,
    pub signed: Vec<StaffId>,
}

/// What the reinsurance module has told claims about this claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Reinsurance {
    pub large_claim_notified: bool,
    /// Treaties under which a reinsurer must consent before admission.
    pub consent_required: BTreeSet<String>,
    /// Consents given, as `(reinsurer, reference)`.
    pub consents: Vec<(String, String)>,
    pub consent_refused: bool,
    /// Disputed items raised on reinsurers' accounts from decisions here.
    pub disputed_items: Vec<(String, String)>,
    /// Disputed items people have settled, with what the insurer repaid.
    #[serde(default)]
    pub disputes_settled: BTreeMap<String, i64>,
}

impl Reinsurance {
    /// Disputed items raised and not yet settled.
    pub fn disputes_open(&self) -> usize {
        self.disputed_items.iter().filter(|(_, r)| !self.disputes_settled.contains_key(r)).count()
    }
}

/// A health invoice or receipt, or a request to pre-approve one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthItem {
    /// The benefit category, an opaque code.
    pub category: String,
    pub invoice_minor: i64,
    /// Pay the provider directly.
    #[serde(default)]
    pub provider: Option<String>,
    /// This is a request for pre-approval, not an invoice.
    #[serde(default)]
    pub pre_approval_request: bool,
    /// The pre-approval this invoice settles.
    #[serde(default)]
    pub settles_pre_approval: Option<String>,
    /// The provider's code for the service billed, and the date it was
    /// given: what the duplicate billing rule compares.
    #[serde(default)]
    pub service_code: Option<String>,
    #[serde(default)]
    pub service_date: Option<NaiveDate>,
}

/// Something that falls due on a claim. The business date reaching it is
/// what makes it happen: nobody has to remember.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Due {
    /// A service timer reaching an escalation level (0, 1 or 2).
    Timer(TimerKind, u8),
    /// A review on the plan of a claim in payment.
    Review(TaskKind),
    ReviewOverdue(TaskKind),
    Evidence(String),
    /// The date the claim was expected to have ended by.
    ExpectedEnd,
    PreApprovalExpiry,
    /// The daily look for premiums to waive while a waiver runs.
    Waiver,
}

/// A line of the claim's own history: who did what, when and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub on: NaiveDate,
    pub log_index: u64,
    pub by: Option<StaffId>,
    pub purpose: Option<String>,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: String,
    pub policy: String,
    pub cover: String,
    pub life: Option<String>,
    pub claim_type: ClaimType,
    pub event_date: NaiveDate,
    pub notified_on: NaiveDate,
    pub channel: Channel,
    pub notifier: Notifier,
    /// The claimant, who need not be a customer.
    pub claimant: Option<String>,
    pub claimed_minor: i64,
    pub flags: Flags,
    pub health: Option<HealthItem>,
    pub status: Status,
    pub queue: Queue,
    pub owner: Option<StaffId>,
    pub priority: u32,
    /// People who underwrote the life: never offered the claim.
    pub underwriters: BTreeSet<StaffId>,
    /// Everyone who has acted on the claim: never its quality reviewer.
    pub touched_by: BTreeSet<StaffId>,
    /// Cause of claim, an opaque code. Clinical: never leaves the module.
    pub cause_code: Option<String>,
    /// Every cause code the claim has carried, with the date it was set.
    pub cause_history: Vec<(NaiveDate, String)>,
    pub cover_check: Option<CoverCheck>,
    /// The policyholder and the person the life is, from the policy record.
    pub policyholder: String,
    pub life_party: Option<String>,
    pub reserve_minor: i64,
    pub admitted_minor: i64,
    /// Of what was admitted, the policy loan it repaid.
    #[serde(default)]
    pub loan_repaid_minor: i64,
    /// Admitted and not yet paid: mirrors the claim's share of benefit payable.
    pub payable_minor: i64,
    /// The part of `payable_minor` that is a handling cost the bank sent
    /// back, waiting to be paid to its provider again.
    #[serde(default)]
    pub cost_payable_minor: i64,
    /// The payment run this claim is waiting in.
    pub in_run: Option<String>,
    pub decisions: Vec<Decision>,
    pub pending: Option<Pending>,
    pub tasks: Vec<Task>,
    pub evidence: Vec<Evidence>,
    pub timers: BTreeMap<TimerKind, Timer>,
    pub payee: Option<Payee>,
    pub payments: Vec<Payment>,
    pub schedule: Option<Schedule>,
    pub overpayments: Vec<Overpayment>,
    pub reinsurance: Reinsurance,
    pub catastrophe_event: Option<String>,
    /// Times an earlier decision on this claim was overturned on review.
    pub overturns: u32,
    pub closed_on: Option<NaiveDate>,
    /// Brought in from the system before this one, with the state it had there.
    #[serde(default)]
    pub migrated: bool,
    /// The restricted case this claim is in, while it is open. Never shown
    /// outside the investigators' own view.
    #[serde(default)]
    pub investigation: Option<String>,
    /// On a continuation policy: the scheme policy and member it carries on
    /// from, so the scheme's claims history and reinsurance follow the life.
    #[serde(default)]
    pub continued_from: Option<crate::port::Continuation>,
    /// Lenders and assignees paid before the claimant, in priority order.
    #[serde(default)]
    pub interests: Vec<InterestDue>,
    /// Late payment interest as last offered to the payee: the amount and
    /// the day they were told.
    #[serde(default)]
    pub interest_offer: Option<(i64, NaiveDate)>,
    /// The payee, having been told, declined late payment interest: the
    /// day and the vault reference of their recorded decision. Until the
    /// claim closes they can change their mind.
    #[serde(default)]
    pub interest_declined: Option<(NaiveDate, String)>,
    /// Interest declined on earlier payments and owed again because the
    /// payee changed their mind: it goes out with the next payment.
    #[serde(default)]
    pub interest_owed_again_minor: i64,
    /// Premiums for periods starting on or after this date are paid from
    /// claims expense while the benefit runs.
    #[serde(default)]
    pub waiver_from: Option<NaiveDate>,
    /// Things that happen to this claim on a date, whoever remembers.
    pub due: BTreeSet<(NaiveDate, Due)>,
    pub history: Vec<HistoryItem>,
}

impl Claim {
    /// The person the claim is about, for anyone deciding who may see it.
    /// A claim on a living person's health is about the life insured, even
    /// where a company owns the policy and is paid the benefit; a death claim
    /// is about the claimant (the estate or a beneficiary). Failing those, the
    /// life, else the policyholder.
    pub fn subject_party(&self) -> &str {
        let first = if self.claim_type == ClaimType::Death {
            self.claimant.as_deref().or(self.life_party.as_deref())
        } else {
            self.life_party.as_deref().or(self.claimant.as_deref())
        };
        first.unwrap_or(&self.policyholder)
    }

    pub fn line(&self) -> Line {
        self.claim_type.line()
    }
    pub fn task(&self, kind: TaskKind) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(move |t| t.kind == kind)
    }
    pub fn has_open_task(&self, kind: TaskKind) -> bool {
        self.task(kind).any(|t| t.completed_on.is_none())
    }
    /// A task of this kind was completed on or after `since`.
    pub fn task_done_since(&self, kind: TaskKind, since: NaiveDate) -> bool {
        self.task(kind).any(|t| t.completed_on.is_some_and(|d| d >= since))
    }
    /// Admitted benefit not yet released. A handling cost that came back
    /// from the bank is owed to its provider, not to the claimant.
    pub fn benefit_payable_minor(&self) -> i64 {
        self.payable_minor - self.cost_payable_minor
    }
    /// Payments are held, not cancelled, while a proof of life is overdue.
    pub fn held_for_proof_of_life(&self) -> bool {
        self.tasks
            .iter()
            .any(|t| t.kind == TaskKind::ProofOfLife && t.completed_on.is_none() && t.overdue)
    }
    /// Proceeds interested parties are still to be paid before the claimant.
    pub fn interests_unpaid_minor(&self) -> i64 {
        self.interests.iter().map(|i| i.owed_minor - i.paid_minor).sum()
    }
    /// What a piece of evidence or a rehabilitation task cost, once recorded.
    pub fn cost_of(&self, item: &str) -> Option<i64> {
        let evidence = self.evidence.iter().find(|e| e.id == item && e.received_on.is_some()).map(|e| e.cost_minor);
        let task = self
            .tasks
            .iter()
            .find(|t| t.id == item && t.kind == TaskKind::Rehabilitation && t.completed_on.is_some())
            .map(|t| t.spend_minor);
        evidence.or(task).filter(|c| *c > 0)
    }
    /// Money paid and not returned, contractual and ex gratia apart.
    /// Handling costs are neither.
    pub fn paid_minor(&self, ex_gratia: bool) -> i64 {
        let lump: i64 = self
            .payments
            .iter()
            .filter(|p| !p.returned && p.ex_gratia == ex_gratia && p.cost_item.is_none())
            .map(|p| p.amount_minor)
            .sum();
        let periods: i64 = match (&self.schedule, ex_gratia) {
            (Some(s), false) => s.periods.values().filter(|p| !p.returned).map(|p| p.gross_minor).sum(),
            _ => 0,
        };
        lump + periods
    }
    /// The admission in force: the last admit not followed by a reversal.
    pub fn admission(&self) -> Option<&Decision> {
        self.decisions.iter().rev().find(|d| d.kind == DecisionKind::Admit)
    }
    pub fn overpayment(&self, id: &str) -> Option<&Overpayment> {
        self.overpayments.iter().find(|o| o.id == id)
    }
}

/// A controlled batch of payments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub kind: RunKind,
    pub benefit_date: NaiveDate,
    pub items: Vec<RunItem>,
    /// The control total and count the approver signs.
    pub control_total_minor: i64,
    pub count: u32,
    pub prepared_by: StaffId,
    pub approved_by: Option<StaffId>,
    pub status: RunStatus,
    /// What was actually posted from benefit payable.
    pub submitted_minor: i64,
    /// Items the core refused, with the reason code. Each raised a task.
    pub exceptions: Vec<(String, String)>,
    /// Payments from this run that came back.
    pub returned_minor: i64,
    /// The bank's settled total, once confirmed.
    pub settled_minor: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    /// Benefits in payment, per benefit date.
    Benefit,
    /// Health claims paid to providers and members on a cycle.
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Prepared,
    Submitted,
    Settled,
    /// The bank's total differs from what was submitted.
    Unreconciled,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunItem {
    pub claim: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub gross_minor: i64,
    pub offset: Option<Offset>,
    /// Who is paid: the provider for a remittance advice, or the payee.
    pub payee: String,
}

impl RunItem {
    pub fn net_minor(&self) -> i64 {
        self.gross_minor - self.offset.as_ref().map_or(0, |o| o.amount_minor)
    }
    pub fn payment_ref(&self, run: &str) -> String {
        format!("{run}/{}/{}", self.claim, self.period_start)
    }
}

/// An amount held against health limits for an approved procedure, like a
/// card authorisation: replicated state, not a posting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreApproval {
    pub claim: String,
    pub policy: String,
    pub life: String,
    pub category: String,
    pub amount_minor: i64,
    pub expires: NaiveDate,
}
