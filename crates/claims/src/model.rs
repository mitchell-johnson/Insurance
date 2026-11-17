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
    /// A spouse, partner or other relative calling for the family, who
    /// holds no role on the policy.
    FamilyMember,
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
    /// The claimant carries a communication marker: they deal with one
    /// named owner, a handover is said to them, and the deadlines they have
    /// to meet are longer.
    #[serde(default)]
    pub communication_support: bool,
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
    /// No benefit while the life is in custody after sentence. Never a decline.
    SuspendBenefit,
    ResumeBenefit,
    /// Part of a death benefit advanced before the claim is decided.
    FuneralAdvance,
    /// A benefit built into the cover, within its own limit.
    AdmitBuiltIn,
    /// The Permanent Disablement Benefit starting or ending.
    PermanentDisablement,
}

/// What a second signer must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// Someone whose own profile covers the whole decision.
    Authority,
    /// The four-eyes check on a decision the proposer's own authority
    /// already covers: a different person accredited for the line who may
    /// themselves sign decisions of this kind at the two-signature level.
    Peer,
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
    /// The payee's resident withholding tax rate in basis points, from the
    /// customer master. Absent: none was given, and the no-declaration rate applies.
    #[serde(default)]
    pub withholding_rate_bp: Option<u32>,
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
    /// Of that interest, the resident withholding tax kept back for the
    /// revenue authority.
    #[serde(default)]
    pub interest_withheld_minor: i64,
    /// The interested party it went to, where it was not paid to the payee.
    #[serde(default)]
    pub interested_party: Option<String>,
}

impl Payment {
    /// The interest that reached the payee: less what they declined and the tax kept back.
    pub fn interest_paid_minor(&self) -> i64 {
        self.interest_minor - self.interest_declined_minor - self.interest_withheld_minor
    }

    /// What the bank was told to pay.
    pub fn sent_minor(&self) -> i64 {
        self.amount_minor + self.interest_paid_minor()
    }
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
    /// Tax kept back from the period's benefit.
    #[serde(default)]
    pub tax_withheld_minor: i64,
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
    /// Days no benefit is payable for while the life is in custody: from
    /// the first such day to the first day after it (`NaiveDate::MAX` while
    /// the suspension is open).
    #[serde(default)]
    pub suspended: BTreeMap<NaiveDate, NaiveDate>,
    /// The cover raises the benefit by the price index at each anniversary
    /// of the first period.
    #[serde(default)]
    pub indexed: bool,
    /// The monthly benefit after each indexation, by the first day it is
    /// payable for. Before the first, `monthly_minor`.
    #[serde(default)]
    pub index_steps: BTreeMap<NaiveDate, i64>,
    /// The Permanent Disablement Benefit: a third more each month, from the
    /// day it was accepted until the first day it is no longer paid.
    #[serde(default)]
    pub permanent_from: Option<NaiveDate>,
    #[serde(default)]
    pub permanent_until: Option<NaiveDate>,
    /// The Peak Season Upgrade: this much more for a period starting in one
    /// of the peak months (1 to 12).
    #[serde(default)]
    pub peak_addition_minor: i64,
    #[serde(default)]
    pub peak_months: Vec<u32>,
}

impl Schedule {
    /// The monthly benefit for a period starting on `start`: the latest
    /// indexation on or before it, or the benefit as admitted.
    pub fn monthly_on(&self, start: NaiveDate) -> i64 {
        self.index_steps.range(..=start).next_back().map_or(self.monthly_minor, |(_, v)| *v)
    }

    /// What is paid on top of the monthly benefit for a period starting on
    /// `start`, before other income is taken off the benefit: a third of it
    /// while the Permanent Disablement Benefit runs, and the peak season
    /// addition in a peak month.
    pub fn supplement_on(&self, start: NaiveDate) -> i64 {
        let permanent = self
            .permanent_from
            .is_some_and(|from| from <= start && self.permanent_until.is_none_or(|until| start < until));
        let peak = self.peak_months.contains(&chrono::Datelike::month(&start));
        (if permanent { self.monthly_on(start) / 3 } else { 0 }) + if peak { self.peak_addition_minor } else { 0 }
    }

    /// The suspension still open, by the day it began.
    pub fn open_suspension(&self) -> Option<NaiveDate> {
        self.suspended.iter().find(|(_, until)| **until == NaiveDate::MAX).map(|(from, _)| *from)
    }

    /// Days in `[start, end)` on which the benefit is suspended.
    pub fn suspended_days(&self, start: NaiveDate, end: NaiveDate) -> i64 {
        self.suspended
            .iter()
            .map(|(from, until)| ((*until).min(end) - (*from).max(start)).num_days().max(0))
            .sum()
    }
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
    /// Letters asking the payee to repay, in the order they were asked for.
    #[serde(default)]
    pub letters: Vec<RepaymentLetter>,
}

/// A letter asking the payee to repay, which the documents service issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepaymentLetter {
    /// The documents request it goes out under: `cl-op:<overpayment>:<n>`.
    pub request: String,
    pub asked_on: NaiveDate,
    /// What was outstanding when it was asked for.
    pub amount_minor: i64,
    /// The documents service has taken the request.
    pub requested: bool,
}

impl Overpayment {
    /// Ask for a letter, unless one is already waiting to be taken.
    pub(crate) fn ask_letter(&mut self, on: NaiveDate) -> Option<RepaymentLetter> {
        if self.letters.iter().any(|l| !l.requested) || self.outstanding_minor() <= 0 {
            return None;
        }
        let letter = RepaymentLetter {
            request: format!("cl-op:{}:{}", self.id, self.letters.len() + 1),
            asked_on: on,
            amount_minor: self.outstanding_minor(),
            requested: false,
        };
        self.letters.push(letter.clone());
        Some(letter)
    }

    /// Whether the payee is being asked to repay: pursued or under an arrangement, with money owed.
    pub fn asking(&self) -> bool {
        matches!(self.status, RecoveryStatus::Pursued | RecoveryStatus::Arrangement) && self.outstanding_minor() > 0
    }
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
    /// The amount the decision was measured at when proposed, so the work
    /// view can say who could sign it. Absent on proposals recorded before.
    #[serde(default)]
    pub amount_minor: Option<i64>,
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
    /// A claim anniversary: the benefit rises by the price index.
    Indexation,
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
    /// Of what was admitted, what was advanced before the claim was decided.
    /// The admission counts it as part of the benefit.
    #[serde(default)]
    pub advanced_minor: i64,
    /// Of what was admitted, what was admitted under each benefit built
    /// into the cover, by its code.
    #[serde(default)]
    pub built_in: BTreeMap<String, i64>,
    /// The qualifying period was waived, with the evidence for it.
    #[serde(default)]
    pub waiting_waived: Option<String>,
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
    /// Admitted while an insolvency interest stood on the policy: what is
    /// payable to the owner or the estate is held until a legal direction.
    #[serde(default)]
    pub insolvency_hold: bool,
    /// The party the legal direction says to pay, with its vault reference.
    #[serde(default)]
    pub directed_to: Option<(String, String)>,
    /// Premiums for periods starting on or after this date are paid from
    /// claims expense while the benefit runs.
    #[serde(default)]
    pub waiver_from: Option<NaiveDate>,
    /// Things that happen to this claim on a date, whoever remembers.
    pub due: BTreeSet<(NaiveDate, Due)>,
    pub history: Vec<HistoryItem>,
}

impl Claim {
    /// Whether proceeds may go to `party`: always, unless an insolvency
    /// interest holds them, and then only to whom a legal direction names.
    pub fn directed(&self, party: &str) -> bool {
        !self.insolvency_hold || self.directed_to.as_ref().is_some_and(|(p, _)| p == party)
    }

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
    /// Tax kept back from an income benefit period.
    #[serde(default)]
    pub tax_withheld_minor: i64,
}

impl RunItem {
    /// What the bank pays: the benefit less any offset and tax kept back.
    pub fn net_minor(&self) -> i64 {
        self.gross_minor - self.offset.as_ref().map_or(0, |o| o.amount_minor) - self.tax_withheld_minor
    }
    /// The payment's reference, which the bank carries as its ISO 20022
    /// end-to-end id: at most 35 printable characters with no spaces. The
    /// readable `{run}/{claim}/{period}` is kept when it fits; otherwise the
    /// period's date and a digest of the readable form, which stays unique
    /// to the run, the claim and the period.
    pub fn payment_ref(&self, run: &str) -> String {
        const MAX_END_TO_END: usize = 35;
        let readable = format!("{run}/{}/{}", self.claim, self.period_start);
        let fits = readable.len() <= MAX_END_TO_END && readable.chars().all(|c| c.is_ascii_graphic());
        if fits {
            return readable;
        }
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(readable.as_bytes()));
        // 9 for the date and its dash, 26 hex digits (104 bits) of digest.
        format!("{}-{}", self.period_start.format("%Y%m%d"), &digest[..MAX_END_TO_END - 9])
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
