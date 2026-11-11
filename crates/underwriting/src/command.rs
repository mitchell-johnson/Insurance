//! Commands in, events and effects out.
//!
//! A command is the only way the module's state changes. Every command
//! carries an idempotency key, the identity of the driver that sent it, and
//! the time and business date the driver read: the state machine itself
//! never reads a clock.
//!
//! No command carries clinical content. Answers, evidence and notes are
//! sealed in the clinical store first; a command carries their references
//! and hashes. The log, and any workflow service that carries these
//! commands, therefore hold nothing clinical.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::command::IssuePolicy;
use ic_core::{GroupId, LifeId, PartyId, PolicyId};
use ic_product::{Frequency, Sex};
use serde::{Deserialize, Serialize};

use crate::authority::AuthorityProfile;
use crate::case::{CaseType, ChaserFrom, Escalation, Queue};
use crate::codes::{AppliedExclusion, ExclusionLibrary, ReasonLibrary};
use crate::config::{DelegatedAuthority, Settings};
use crate::evidence::{EvidenceKind, EvidenceStatus};
use crate::model::{AccessEnd, AdviserRef, ConsentKind, CoverTerms, Entry, ExistingCover, ProposalId, ProposedCover, ProposedLife, QuoteBinding, VersionRef};
use crate::questions::QuestionSet;
use crate::rules::{Assessment, RuleSet};
use crate::signing::SignedStep;

/// What goes into the module's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A duplicate changes nothing.
    pub key: String,
    /// The driver sending the command.
    pub caller: String,
    /// The time the driver read when it proposed the command.
    pub at: DateTime<Utc>,
    /// The business date.
    pub today: NaiveDate,
    pub command: Command,
}

/// Who signed off a configuration change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub author: String,
    pub approvers: Vec<String>,
    /// The change record the approval belongs to.
    pub change_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenProposal {
    pub proposal: ProposalId,
    pub entry: Entry,
    pub policy: PolicyId,
    pub policy_number: String,
    pub group: GroupId,
    pub policyholder: PartyId,
    pub product_code: String,
    pub frequency: Frequency,
    pub channel: String,
    #[serde(default)]
    pub adviser: Option<AdviserRef>,
    pub quote: QuoteBinding,
    pub lives: Vec<ProposedLife>,
    pub covers: Vec<ProposedCover>,
    /// Migration only: the date the legacy proposal was received, so the
    /// applicant keeps the terms and the time they were given.
    #[serde(default)]
    pub legacy_received_on: Option<NaiveDate>,
}

/// How a record was signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum SigningProof {
    Device(SignedStep),
    /// Migration only: the legacy evidence of who gave the disclosure.
    Migrated {
        legacy_ref: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitDisclosure {
    pub proposal: ProposalId,
    pub life: LifeId,
    /// SHA-256 of the disclosure record sealed in the clinical store.
    pub hash: String,
    pub vault_ref: String,
    pub question_set: VersionRef,
    pub rule_set: VersionRef,
    pub signing: SigningProof,
    /// How many answers the adviser entered for the client.
    #[serde(default)]
    pub entered_by_adviser: u32,
    /// What the rules said, per cover code on this life.
    pub assessments: BTreeMap<String, Assessment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferResponse {
    Accept,
    Decline,
    /// Ask for the terms to be looked at again, with more to say.
    Counter,
}

impl OfferResponse {
    pub fn key(self) -> &'static str {
        match self {
            OfferResponse::Accept => "accept",
            OfferResponse::Decline => "decline",
            OfferResponse::Counter => "counter",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum CapacityAnswer {
    Automatic,
    FacultativeRequired,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "terms", rename_all = "snake_case")]
pub enum FacultativeOutcome {
    Offered { reference: String },
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenceStatus {
    Active,
    Suspended,
    Revoked,
    Lapsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NonDisclosureOutcome {
    NoAction,
    Vary,
    Avoid,
}

impl NonDisclosureOutcome {
    pub fn key(self) -> &'static str {
        match self {
            NonDisclosureOutcome::NoAction => "no_action",
            NonDisclosureOutcome::Vary => "vary",
            NonDisclosureOutcome::Avoid => "avoid",
        }
    }
}

/// Terms for one life as they arrive from a legacy system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigratedLife {
    pub life: LifeId,
    pub terms: Vec<CoverTerms>,
    /// The legacy underwriting file, loaded into the vault as received.
    pub file_ref: Option<String>,
    pub file_hash: Option<String>,
    pub legacy_underwriter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigratedPolicy {
    pub open: OpenProposal,
    /// The original commencement date.
    pub commencement: NaiveDate,
    pub original_decision_date: NaiveDate,
    pub legacy_ref: String,
    pub lives: Vec<MigratedLife>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedDecision {
    pub proposal: ProposalId,
    pub life: LifeId,
    pub authority: String,
    pub terms: Vec<CoverTerms>,
    pub effective: NaiveDate,
    /// The rule version the delegate used, where the authority names one.
    #[serde(default)]
    pub rule_version: Option<u32>,
    pub evidence: BTreeSet<EvidenceKind>,
    /// The deciding underwriter or system on the delegate's side.
    pub decided_by: String,
    pub delegate_ref: String,
    /// The member's consent to the delegate's underwriting and our audit access.
    pub consent_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Externally tagged, like the core's commands.
#[serde(rename_all = "snake_case")]
pub enum Command {
    // --- configuration, each with two approvers ---
    ActivateExclusions {
        library: Box<ExclusionLibrary>,
        approval: Approval,
    },
    ActivateReasons {
        library: Box<ReasonLibrary>,
        approval: Approval,
    },
    ActivateQuestionSet {
        set: Box<QuestionSet>,
        approval: Approval,
    },
    /// A changed rule set carries the digest of the backtest its approvers saw.
    ActivateRuleSet {
        set: Box<RuleSet>,
        backtest_digest: Option<String>,
        approval: Approval,
    },
    ActivateProfile {
        profile: Box<AuthorityProfile>,
        approval: Approval,
    },
    ActivateSettings {
        settings: Box<Settings>,
        approval: Approval,
    },
    ActivatePolicies {
        cedar: String,
        approval: Approval,
    },
    ActivateDelegatedAuthority {
        authority: Box<DelegatedAuthority>,
        approval: Approval,
    },

    // --- the applicant's side ---
    OpenProposal(Box<OpenProposal>),
    /// Consent to adviser access is given with a signed step.
    RecordConsent {
        proposal: ProposalId,
        life: LifeId,
        kind: ConsentKind,
        granted: bool,
        signed: Option<SignedStep>,
    },
    /// Record what the life index holds on the person. With `for_applicant`
    /// it fills the existing-cover question on the form: only with the
    /// life's consent, and only where the person is matched in the customer
    /// master. Without it, the read is for the underwriter and the rules.
    PrefillExistingCover {
        proposal: ProposalId,
        life: LifeId,
        matched: bool,
        for_applicant: bool,
        rows: Vec<ExistingCover>,
        reflects: BTreeMap<u16, u64>,
    },
    SubmitDisclosure(Box<SubmitDisclosure>),
    RespondToOffer {
        proposal: ProposalId,
        life: LifeId,
        offer: String,
        response: OfferResponse,
        signed: SignedStep,
    },
    Withdraw {
        proposal: ProposalId,
    },
    Chaser {
        proposal: ProposalId,
        life: LifeId,
        from: ChaserFrom,
    },
    /// A new quote accepted by the applicant after the core refused the
    /// issue because the product or prices had changed.
    Requote {
        proposal: ProposalId,
        quote: QuoteBinding,
        signed: SignedStep,
    },
    /// The adviser opens the application. Logged, with its purpose.
    AdviserLookup {
        proposal: ProposalId,
        adviser: String,
        purpose: String,
    },

    // --- evidence ---
    EvidenceAcknowledged {
        request: String,
        provider_ref: String,
    },
    EvidenceReceived {
        request: String,
        vault_ref: String,
        cost_minor: i64,
    },
    EvidenceUnobtainable {
        request: String,
    },

    // --- case work, by a named underwriter ---
    DeclareConflict {
        underwriter: String,
        party: PartyId,
    },
    TakeCase {
        case: String,
        underwriter: String,
    },
    HandOver {
        case: String,
        by: String,
        to: String,
    },
    MoveCase {
        case: String,
        underwriter: String,
        case_type: CaseType,
        queue: Queue,
        reasons: BTreeSet<String>,
    },
    RequestEvidence {
        case: String,
        underwriter: String,
        kinds: BTreeSet<EvidenceKind>,
    },
    AskOpinion {
        case: String,
        underwriter: String,
        of: String,
    },
    RecordOpinion {
        case: String,
        of: String,
        vault_ref: String,
    },
    ScheduleInterview {
        proposal: ProposalId,
        life: LifeId,
        scheduled_for: DateTime<Utc>,
        booked_by: String,
    },
    RecordInterview {
        case: String,
        underwriter: String,
        notes_ref: String,
        notes_hash: String,
        recording_ref: Option<String>,
        present: Vec<String>,
        adviser_present: bool,
    },
    /// The life confirms the recorded interview answers: now they are disclosure.
    ConfirmInterview(Box<SubmitDisclosure>),
    ProposeDecision {
        case: String,
        underwriter: String,
        terms: Vec<CoverTerms>,
        reasons: BTreeSet<String>,
        withheld_reasons: BTreeSet<String>,
        notes_ref: Option<String>,
        evidence: BTreeSet<EvidenceKind>,
    },
    Countersign {
        case: String,
        underwriter: String,
        agree: bool,
        reasons: BTreeSet<String>,
        withheld_reasons: BTreeSet<String>,
    },
    /// The reinsurer liaison releases named documents for a facultative
    /// referral. References only: nothing is copied to the reinsurance module.
    ReleaseToReinsurer {
        case: String,
        underwriter: String,
        treaty: String,
        documents: Vec<String>,
    },
    RetryIssue {
        proposal: ProposalId,
        underwriter: String,
    },

    // --- reinsurance capacity ---
    CapacityAnswered {
        reservation: String,
        answer: CapacityAnswer,
    },
    FacultativeTermsRecorded {
        reservation: String,
        outcome: FacultativeOutcome,
    },

    // --- the core's replies ---
    InterimIssued {
        proposal: ProposalId,
    },
    PolicyIssued {
        proposal: ProposalId,
    },
    IssueRejected {
        proposal: ProposalId,
        code: String,
    },

    // --- time ---
    /// Lapse what has run out of time, chase evidence, escalate untouched
    /// cases. Works through at most `max` proposals after `after`.
    Sweep {
        after: Option<ProposalId>,
        max: u32,
    },

    // --- delivery ---
    /// The driver has delivered every effect up to and including `through`,
    /// and the receivers answered. They leave the outbox.
    AcknowledgeEffects {
        through: u64,
    },

    // --- adviser licensing ---
    LicenceChanged {
        subject: String,
        status: LicenceStatus,
        effective: NaiveDate,
        source_ref: String,
    },

    // --- claims ---
    OpenFileForClaim {
        claim_ref: String,
        policy: PolicyId,
        life: LifeId,
        assessor: String,
    },
    OpenNonDisclosureReview {
        claim_ref: String,
        policy: PolicyId,
        life: LifeId,
        opened_by: String,
    },
    RecordNonDisclosureOutcome {
        review: String,
        outcome: NonDisclosureOutcome,
        decided_by: String,
        countersigned_by: Option<String>,
        reasons: BTreeSet<String>,
        /// For a variation: the terms that would have been offered.
        varied_terms: Vec<CoverTerms>,
    },

    // --- decisions that arrive as data ---
    DelegatedDecision(Box<DelegatedDecision>),
    RequestDelegateFile {
        proposal: ProposalId,
        life: LifeId,
        underwriter: String,
        purpose: String,
    },
    DelegateFileReceived {
        proposal: ProposalId,
        life: LifeId,
        vault_ref: String,
        hash: String,
    },
    MigratedPolicy(Box<MigratedPolicy>),

    // --- quality ---
    DrawPeerReviewSample {
        period: String,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        seed: u64,
    },
    RecordPeerFinding {
        decision: String,
        reviewer: String,
        agrees: bool,
        reasons: BTreeSet<String>,
    },
}

impl Command {
    /// The drivers allowed to send each command.
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        use Command::*;
        match self {
            ActivateExclusions { .. }
            | ActivateReasons { .. }
            | ActivateQuestionSet { .. }
            | ActivateRuleSet { .. }
            | ActivateProfile { .. }
            | ActivateSettings { .. }
            | ActivatePolicies { .. }
            | ActivateDelegatedAuthority { .. } => &["config"],
            OpenProposal(_) | SubmitDisclosure(_) => &["new-business", "migration"],
            RecordConsent { .. }
            | PrefillExistingCover { .. }
            | RespondToOffer { .. }
            | Withdraw { .. }
            | Chaser { .. }
            | Requote { .. }
            | ConfirmInterview(_) => &["new-business"],
            AdviserLookup { .. } => &["adviser-portal"],
            EvidenceAcknowledged { .. } | EvidenceReceived { .. } | EvidenceUnobtainable { .. } => &["evidence", "migration"],
            ScheduleInterview { .. } => &["new-business", "underwriting-console"],
            DeclareConflict { .. }
            | TakeCase { .. }
            | HandOver { .. }
            | MoveCase { .. }
            | RequestEvidence { .. }
            | AskOpinion { .. }
            | RecordOpinion { .. }
            | RecordInterview { .. }
            | ProposeDecision { .. }
            | Countersign { .. }
            | ReleaseToReinsurer { .. }
            | RetryIssue { .. }
            | RecordNonDisclosureOutcome { .. }
            | RequestDelegateFile { .. }
            | DrawPeerReviewSample { .. }
            | RecordPeerFinding { .. } => &["underwriting-console"],
            CapacityAnswered { .. } | FacultativeTermsRecorded { .. } => &["reinsurance"],
            InterimIssued { .. } | PolicyIssued { .. } | IssueRejected { .. } => &["issuer"],
            Sweep { .. } => &["timer"],
            AcknowledgeEffects { .. } => &["effects-driver"],
            LicenceChanged { .. } => &["adviser-licensing"],
            OpenFileForClaim { .. } | OpenNonDisclosureReview { .. } => &["claims"],
            DelegatedDecision(_) | DelegateFileReceived { .. } => &["delegate-api"],
            MigratedPolicy(_) => &["migration"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Who a notice goes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum Recipient {
    Party { party: PartyId },
    Adviser { adviser: String },
    Underwriter { underwriter: String },
    QueueLead { queue: Queue },
    UnderwritingManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    OfferMade,
    /// Some or all cover declined or deferred: the reasons the person may be
    /// told, and how to ask for review.
    Declined,
    ProposalLapsed,
    ProposalWithdrawn,
    PolicyIssued,
    /// The adviser can no longer act: the insurer serves the customer
    /// directly and the customer may appoint a new adviser.
    AdviserUnavailable,
    EvidenceArrived,
    CaseEscalated,
    IssueFailed,
    InterviewToConfirm,
}

/// Things for drivers to do outside the module. Every effect is safe to
/// repeat: each carries a key that is stable for the proposal and step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    ReserveCapacity {
        reservation: String,
        party: PartyId,
        product_code: String,
        cover_code: String,
        cover_type: String,
        sum_insured_minor: i64,
        commencement: NaiveDate,
        proposal_expires: NaiveDate,
    },
    ReleaseCapacity {
        reservation: String,
        reason: String,
    },
    ConfirmCapacity {
        reservation: String,
        policy: PolicyId,
        life: LifeId,
        cover: String,
    },
    /// Rating factors for the reinsurer's rate tables and listing.
    RecordRiskFactors {
        policy: PolicyId,
        life: LifeId,
        date_of_birth: Option<NaiveDate>,
        sex: Option<Sex>,
        smoker: Option<bool>,
    },
    RequestEvidence {
        request: String,
        kind: EvidenceKind,
        party: PartyId,
    },
    ChaseEvidence {
        request: String,
    },
    CancelEvidence {
        request: String,
    },
    /// Ask the delegate for a file under the audit access the agreement gives.
    RequestDelegateFile {
        proposal: ProposalId,
        life: LifeId,
        delegate_ref: String,
    },
    /// One command to the core for the policy. The driver first checks the
    /// core still has the product version the quote was bound to.
    IssuePolicy {
        key: String,
        proposal: ProposalId,
        expected_product_hash: String,
        migrated: bool,
        command: Box<IssuePolicy>,
    },
    /// Terms for a policy already in force: a new layer, a replaced layer or
    /// a reinstatement, as the entry point says.
    ApplyTerms {
        key: String,
        proposal: ProposalId,
        entry: Entry,
        policy: PolicyId,
        effective: NaiveDate,
        covers: Vec<ic_core::command::CoverSpec>,
    },
    IssueInterim {
        key: String,
        proposal: ProposalId,
        command: Box<IssuePolicy>,
    },
    EndInterim {
        key: String,
        proposal: ProposalId,
        policy: PolicyId,
    },
    /// A non-disclosure remedy, approved in its own case: a command to the
    /// core, never an edit.
    NonDisclosureRemedy {
        key: String,
        policy: PolicyId,
        life: LifeId,
        outcome: NonDisclosureOutcome,
        review: String,
        decision: Option<String>,
    },
    /// A notice for the correspondence module. The reasons are plain words
    /// the recipient may be told; they imply health, so a notice is not an
    /// event and goes only to its recipient.
    Notify {
        to: Recipient,
        kind: NoticeKind,
        proposal: ProposalId,
        life: Option<LifeId>,
        reasons: Vec<String>,
    },
}

/// What happened. Events carry no health data: no reasons, no terms, no
/// answers. An event that records access to a file names who and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    ConfigActivated {
        kind: String,
        id: String,
        version: u32,
        content_hash: String,
        author: String,
        approvers: Vec<String>,
    },
    ProposalReceived {
        proposal: ProposalId,
        product_code: String,
        channel: String,
        lives: u32,
        expires_on: NaiveDate,
    },
    ConsentRecorded {
        proposal: ProposalId,
        life: LifeId,
        kind: ConsentKind,
        granted: bool,
    },
    ExistingCoverPrefilled {
        proposal: ProposalId,
        life: LifeId,
    },
    DisclosureSubmitted {
        proposal: ProposalId,
        life: LifeId,
        hash: String,
    },
    EvidenceRequested {
        proposal: ProposalId,
        life: LifeId,
        request: String,
        kind: EvidenceKind,
        deadline: DateTime<Utc>,
    },
    EvidenceChased {
        proposal: ProposalId,
        life: LifeId,
        request: String,
        chasers: u32,
    },
    EvidenceSettled {
        proposal: ProposalId,
        life: LifeId,
        request: String,
        status: EvidenceStatus,
    },
    Referred {
        proposal: ProposalId,
        life: LifeId,
        case: String,
        case_type: CaseType,
        queue: Queue,
    },
    CaseMoved {
        case: String,
        case_type: CaseType,
        queue: Queue,
    },
    CaseAssigned {
        case: String,
        owner: String,
        from: Option<String>,
    },
    /// A case went back to its queue with no owner.
    CaseReleased {
        case: String,
        from: String,
    },
    CaseEscalated {
        case: String,
        to: Escalation,
    },
    CaseClosed {
        case: String,
    },
    ChaserRecorded {
        proposal: ProposalId,
        life: LifeId,
        from: ChaserFrom,
    },
    OpinionAsked {
        case: String,
        of: String,
    },
    OpinionRecorded {
        case: String,
        of: String,
    },
    InterviewScheduled {
        proposal: ProposalId,
        life: LifeId,
        scheduled_for: DateTime<Utc>,
    },
    InterviewRecorded {
        proposal: ProposalId,
        life: LifeId,
    },
    InterviewConfirmed {
        proposal: ProposalId,
        life: LifeId,
    },
    DecisionProposed {
        proposal: ProposalId,
        life: LifeId,
        decision: String,
        needs_second_signature: bool,
    },
    CountersignRefused {
        proposal: ProposalId,
        life: LifeId,
        decision: String,
    },
    CapacityRequested {
        proposal: ProposalId,
        life: LifeId,
        reservation: String,
    },
    FacultativeReferral {
        proposal: ProposalId,
        life: LifeId,
    },
    DecisionMade {
        proposal: ProposalId,
        life: LifeId,
        decision: String,
    },
    OfferMade {
        proposal: ProposalId,
        life: LifeId,
        offer: String,
        expires_on: NaiveDate,
    },
    OfferResponded {
        proposal: ProposalId,
        life: LifeId,
        offer: String,
        response: OfferResponse,
    },
    ProposalLapsed {
        proposal: ProposalId,
    },
    ProposalDeclined {
        proposal: ProposalId,
    },
    ProposalWithdrawn {
        proposal: ProposalId,
    },
    IssueRequested {
        proposal: ProposalId,
        policy: PolicyId,
    },
    PolicyIssued {
        proposal: ProposalId,
        policy: PolicyId,
    },
    IssueFailed {
        proposal: ProposalId,
        code: String,
    },
    InterimCoverRequested {
        proposal: ProposalId,
        policy: PolicyId,
    },
    InterimCoverEnded {
        proposal: ProposalId,
        policy: PolicyId,
    },
    AdviserAccessEnded {
        proposal: ProposalId,
        life: LifeId,
        adviser: String,
        why: AccessEnd,
    },
    LicenceRecorded {
        subject: String,
        status: LicenceStatus,
        effective: NaiveDate,
    },
    /// Someone read, or was given, part of an underwriting file.
    FileAccessed {
        who: String,
        role: String,
        purpose: String,
        proposal: ProposalId,
        life: Option<LifeId>,
        documents: Vec<String>,
    },
    NonDisclosureReviewOpened {
        review: String,
        claim_ref: String,
    },
    NonDisclosureReviewClosed {
        review: String,
        claim_ref: String,
        outcome: NonDisclosureOutcome,
        decision: String,
    },
    DelegatedDecisionRejected {
        proposal: ProposalId,
        life: LifeId,
        delegate_ref: String,
        why: String,
    },
    PeerReviewSampled {
        decision: String,
        period: String,
    },
    PeerReviewRecorded {
        decision: String,
        agrees: bool,
    },
}

/// What an adviser sees of one life on an application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdviserLifeView {
    pub life: LifeId,
    pub stage: String,
    /// Evidence progress: what was asked and where it has got to. Never content.
    pub evidence: Vec<(EvidenceKind, EvidenceStatus)>,
    /// The outcome, once there is one: `standard`, `terms`, `decline`, `defer`.
    pub outcome: Option<Vec<(String, String)>>,
    /// Where this life's answers are, only when the life has consented and
    /// no decision has been made.
    pub answers_ref: Option<String>,
}

/// The decision as the claims module is shown it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFile {
    pub proposal: ProposalId,
    pub decision: String,
    pub terms: Vec<CoverTerms>,
    pub versions: crate::model::Versions,
    /// Disclosure records: vault reference, hash and whether migrated.
    pub disclosures: Vec<(String, String, bool)>,
    pub underwriters: BTreeSet<String>,
}

/// What a command hands back to its caller, beyond events and effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Adviser {
        status: String,
        lives: Vec<AdviserLifeView>,
    },
    ClaimFiles(Vec<ClaimFile>),
    Review {
        review: String,
        regime: crate::questions::DisclosureRegime,
    },
    NonDisclosure {
        decision: String,
    },
    Sweep {
        next: Option<ProposalId>,
        lapsed: u32,
        chased: u32,
        escalated: u32,
    },
    Delegated {
        accepted: bool,
        why: Option<String>,
    },
    Sample {
        decisions: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub events: Vec<Event>,
    pub effects: Vec<Effect>,
    pub view: Option<View>,
    /// The key had been applied before: nothing changed this time.
    pub duplicate: bool,
}

/// The terms on a cover as the claims module needs them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermsOnCover {
    pub decision: String,
    pub loading_percent: u32,
    pub exclusions: Vec<AppliedExclusion>,
    /// Migrated with no evidence of the original terms: an assessor decides.
    pub unknown_terms: bool,
}
