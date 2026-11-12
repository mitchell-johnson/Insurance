//! What the module holds about each complaint and conduct case.
//!
//! No words of the complainant and no clinical detail are kept here: the
//! summary, letters and bundles are documents in the vault, and this state
//! holds their IDs. What is here is who, when, which policies and
//! decisions, how it was categorised, and what was done.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::{PartyId, PolicyId};
use serde::{Deserialize, Serialize};

/// Where the expression of dissatisfaction was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeChannel {
    App,
    Portal,
    Call,
    Letter,
    Email,
    SocialMedia,
    Adviser,
    /// A remark during a claim, collections or servicing conversation.
    InConversation,
    ExternalScheme,
}

/// Who is complaining. They need not be a customer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplainantKind {
    Policyholder,
    LifeInsured,
    Claimant,
    Beneficiary,
    SchemeMember,
    Employer,
    Adviser,
    ThirdParty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Process {
    Sales,
    Underwriting,
    Claims,
    Billing,
    Service,
    Privacy,
    AdviserConduct,
}

/// The controlled categorisation. It may be refined as the case is worked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    #[serde(default)]
    pub product: Option<String>,
    pub process: Process,
    /// A cause code from the configured list.
    #[serde(default)]
    pub cause: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Claim,
    Underwriting,
    Other,
}

/// A decision the complaint is about, and who made it under which rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionLink {
    pub decision: String,
    pub kind: DecisionKind,
    /// The assessor or underwriter who made it. They never handle the
    /// complaint.
    pub decided_by: String,
    /// The authority level it was made under. The reviewer's is no lower.
    pub authority: u8,
    #[serde(default)]
    pub rule_version: Option<String>,
    #[serde(default)]
    pub wording_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Links {
    #[serde(default)]
    pub policies: Vec<PolicyId>,
    /// Claims, proposals and other cases it concerns.
    #[serde(default)]
    pub cases: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<DecisionLink>,
    /// The adviser engagement it concerns, if any.
    #[serde(default)]
    pub adviser_engagement: Option<String>,
    /// Staff whose conduct or work it concerns. They never handle it.
    #[serde(default)]
    pub staff: Vec<String>,
    /// What the recorder was doing when they heard it.
    #[serde(default)]
    pub recorded_from: Option<String>,
}

/// Someone acting for or supporting the complainant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Representative {
    pub party: PartyId,
    /// `lawyer`, `family`, `support_person`, `advocate`.
    pub relationship: String,
    /// The document that records the complainant's authority.
    pub authority_document: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Upheld,
    PartlyUpheld,
    NotUpheld,
    Withdrawn,
    ResolvedByAgreement,
}

impl OutcomeKind {
    /// The complaint succeeded at least in part: the decision complained
    /// about was overturned or changed.
    pub fn overturns(self) -> bool {
        matches!(self, OutcomeKind::Upheld | OutcomeKind::PartlyUpheld)
    }

    /// Something may be put right after this outcome.
    pub fn allows_remediation(self) -> bool {
        self.overturns() || self == OutcomeKind::ResolvedByAgreement
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub kind: OutcomeKind,
    pub reason_codes: Vec<String>,
    /// The written outcome, in the vault. Sent with proof of delivery.
    pub document: Option<String>,
    pub decided_on: NaiveDate,
    pub decided_by: String,
    pub delivered_on: Option<NaiveDate>,
    /// Resolved by the person who heard it, within their small authority.
    pub front_line: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationKind {
    /// The claim or proposal is reopened in its own module.
    DecisionReopened,
    /// Posted through the payments module.
    Refund,
    /// A goodwill payment, with its own authority.
    ExGratia,
    /// A correction event on a record.
    RecordCorrection,
}

/// Something put right, through the ordinary command of the module that
/// owns it. This is the link to that command, never the adjustment itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remediation {
    pub kind: RemediationKind,
    /// The ID of the command or case in the owning module.
    pub reference: String,
    pub amount_minor: i64,
    pub authorised_by: String,
    pub recorded_on: NaiveDate,
    pub completed_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Forum {
    /// The external dispute resolution scheme the insurer belongs to.
    Scheme,
    Court,
    PrivacyRegulator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub id: String,
    /// The vault documents released.
    pub documents: Vec<String>,
    /// Who checked what may be shared.
    pub reviewed_by: String,
    pub released_by: String,
    pub released_on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForumDecision {
    pub on: NaiveDate,
    pub outcome: OutcomeKind,
    /// A decision that binds the insurer is implemented as remediation.
    pub binding: bool,
    pub remediation_due: Option<NaiveDate>,
    pub remediation_completed_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Referral {
    pub forum: Forum,
    /// The forum's own reference.
    pub reference: String,
    pub referred_on: NaiveDate,
    pub response_due: NaiveDate,
    pub responded_on: Option<NaiveDate>,
    pub documents_due: Option<NaiveDate>,
    pub bundles: Vec<Bundle>,
    pub decision: Option<ForumDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deadlock {
    pub on: NaiveDate,
    /// Reached by the passing of time, not declared by the insurer.
    pub automatic: bool,
    /// The deadlock notice, once issued.
    pub notice: Option<String>,
}

/// What a closed complaint was found to be caused by, and what it concerned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closure {
    pub on: NaiveDate,
    pub by: String,
    pub cause: String,
    pub product_version: Option<String>,
    pub rule_version: Option<String>,
    pub template_version: Option<String>,
    pub adviser_engagement: Option<String>,
    pub staff: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Timer {
    Acknowledge,
    Update,
    Decide,
    Deadlock,
    ForumResponse,
    ForumDocuments,
    ForumRemediation,
    ComplainantResponse,
    RegulatorNotification,
    RegulatorRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Warning,
    /// Due and not done: to the complaints lead.
    Lead,
    /// Still not done: to the conduct officer.
    ConductOfficer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    Personal,
    Financial,
    InsuranceSensitive,
    Clinical,
}

/// Access to the file, granted by the case and for a stated purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub classes: BTreeSet<DataClass>,
    pub purpose: String,
    pub granted_by: String,
    pub on: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    /// An outcome is recorded; remediation or delivery may be outstanding.
    Decided,
    Deadlocked,
    Referred,
    Closed,
}

/// Something asked of the complainant, and when an answer is hoped for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Awaiting {
    pub asked_on: NaiveDate,
    pub due: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Complaint {
    pub id: String,
    pub received_on: NaiveDate,
    pub recorded_by: String,
    pub channel: IntakeChannel,
    pub complainant: PartyId,
    pub complainant_kind: ComplainantKind,
    pub representatives: Vec<Representative>,
    pub links: Links,
    pub category: Category,
    /// The summary in the complainant's words, in the vault.
    pub summary_document: String,
    /// Set by a person, never inferred. Routes the complaint to trained
    /// staff and gives the complainant longer to respond.
    pub vulnerable: bool,
    /// The adjustments to make, which staff see instead of the reason.
    pub adjustments: Vec<String>,
    /// The timer configuration the complaint runs under, for its life.
    pub timers_version: u32,
    pub owner: Option<String>,
    pub owner_authority: u8,
    pub status: Status,
    pub acknowledged_on: Option<NaiveDate>,
    pub acknowledgement: Option<String>,
    /// Dates the complainant was updated.
    pub updates: Vec<NaiveDate>,
    pub awaiting: Option<Awaiting>,
    pub outcome: Option<Outcome>,
    pub remediation: Vec<Remediation>,
    /// The remediation case opened for others affected by the same cause.
    pub systemic_case: Option<String>,
    pub deadlock: Option<Deadlock>,
    pub referral: Option<Referral>,
    pub closure: Option<Closure>,
    /// The highest level each timer has been escalated to.
    pub escalated: BTreeMap<Timer, Level>,
    pub grants: BTreeMap<String, Grant>,
    pub migrated: bool,
}

impl Complaint {
    /// Everyone who may not handle the complaint: whoever made a decision
    /// it is about, and the staff it concerns.
    pub fn involved(&self) -> BTreeSet<&str> {
        self.links
            .decisions
            .iter()
            .map(|d| d.decided_by.as_str())
            .chain(self.links.staff.iter().map(String::as_str))
            .collect()
    }

    /// The authority a reviewer needs: no lower than any claims or
    /// underwriting decision under review.
    pub fn authority_needed(&self) -> u8 {
        self.links
            .decisions
            .iter()
            .filter(|d| d.kind != DecisionKind::Other)
            .map(|d| d.authority)
            .max()
            .unwrap_or(0)
    }

    pub fn concerns_clinical_decision(&self) -> bool {
        self.links.decisions.iter().any(|d| d.kind != DecisionKind::Other)
    }
}

// --- conduct cases ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConductKind {
    /// A licence or authorisation was revoked, suspended or lapsed.
    LicenceEvent {
        licence_event: String,
        /// The FAP, authorised body or adviser whose licence it is.
        party: PartyId,
        /// `revoked`, `suspended`, `lapsed`.
        status: String,
        effective: NaiveDate,
    },
    Breach,
    Incident,
    AdviserPattern,
    SystemicFinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFinding {
    Satisfactory,
    /// The advice needs putting right, in the named remediation case.
    RemediationNeeded {
        remediation_case: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReview {
    pub reviewer: String,
    pub on: NaiveDate,
    pub finding: ReviewFinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegulatorRequest {
    pub received_on: NaiveDate,
    pub due: NaiveDate,
    /// The request as received, in the vault.
    pub document: String,
    pub response: Option<String>,
    pub responded_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegulatorNotification {
    pub due: NaiveDate,
    pub reference: Option<String>,
    pub document: Option<String>,
    pub notified_on: Option<NaiveDate>,
}

/// How one affected customer was dealt with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Contact {
    Outstanding,
    /// Told, by the named document.
    Notified {
        document: String,
        on: NaiveDate,
    },
    Excluded {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConductCase {
    pub id: String,
    pub kind: ConductKind,
    /// The conduct officer. Legal for a matter before a court.
    pub owner: String,
    pub opened_on: NaiveDate,
    pub opened_by: String,
    pub summary_document: Option<String>,
    /// The affected customers, and how each was told.
    pub customers: BTreeMap<PartyId, Contact>,
    pub advisers: BTreeSet<PartyId>,
    /// The policies whose advice files are in scope.
    pub policies: BTreeSet<PolicyId>,
    /// The population is attached in chunks; this says it is all there.
    pub population_complete: bool,
    /// The advice files drawn for review, and what was found.
    pub sample: BTreeMap<PolicyId, Option<FileReview>>,
    pub regulator: Option<RegulatorNotification>,
    pub requests: BTreeMap<String, RegulatorRequest>,
    pub complaints: BTreeSet<String>,
    pub escalated: BTreeMap<String, Level>,
    pub timers_version: u32,
    pub closed_on: Option<NaiveDate>,
    pub findings_document: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    ProductChange,
    RuleChange,
    TemplateChange,
    Training,
    AdviserConductCase,
}

/// An action the monthly root cause review produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub kind: ActionKind,
    pub cause: String,
    /// The change record, training record or conduct case.
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCauseReview {
    pub reviewed_on: NaiveDate,
    pub by: String,
    pub actions: Vec<Action>,
}
