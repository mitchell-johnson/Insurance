//! What the module holds about a proposal. None of it is clinical: answers,
//! evidence and notes are in the clinical store, and a proposal holds their
//! references and hashes. What is here is still sensitive, because a loading
//! or an exclusion code implies something about a person's health.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::{GroupId, LifeFactors, LifeId, PartyId, PolicyId};
use ic_product::Frequency;
use serde::{Deserialize, Serialize};

use crate::case::Case;
use crate::codes::AppliedExclusion;
use crate::evidence::{EvidenceKind, EvidenceRequest};

/// Opaque proposal ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProposalId(pub String);

impl fmt::Display for ProposalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl From<&str> for ProposalId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// A configuration version as recorded on a decision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VersionRef {
    pub id: String,
    pub version: u32,
    pub content_hash: String,
}

/// Why the proposal exists. The same flow serves every entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum Entry {
    NewBusiness,
    /// More cover on a policy in force: the increase is a new layer.
    Increase {
        policy: PolicyId,
    },
    /// A change of rating attribute, such as occupation: the old layer ends
    /// and a new one starts from the decision date.
    Alteration {
        policy: PolicyId,
    },
    Reinstatement {
        policy: PolicyId,
    },
    /// A scheme member's cover above the automatic acceptance limit. Only
    /// the excess is underwritten.
    GroupExcess {
        policy: PolicyId,
    },
}

impl Entry {
    pub fn policy(&self) -> Option<&PolicyId> {
        match self {
            Entry::NewBusiness => None,
            Entry::Increase { policy } | Entry::Alteration { policy } | Entry::Reinstatement { policy } | Entry::GroupExcess { policy } => Some(policy),
        }
    }
}

/// The quote the proposal is bound to: the terms offered are the terms
/// issued if nothing changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteBinding {
    pub product_hash: String,
    pub rate_tables: Vec<VersionRef>,
    /// The quoted premium for the first period at standard rates.
    pub premium_minor: i64,
    pub quoted_on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdviserRef {
    pub adviser: String,
    /// The financial advice provider the adviser acts under.
    pub fap: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedLife {
    pub life: LifeId,
    pub party: PartyId,
    pub factors: LifeFactors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedCover {
    pub code: String,
    pub cover_type: String,
    pub life: LifeId,
    pub sum_insured_minor: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentKind {
    /// The adviser may see and help with this life's answers, for this
    /// application only.
    AdviserAccess,
    /// The existing-cover question may be filled in from what the insurer
    /// already holds on this person.
    ExistingCoverPrefill,
    /// Records may be obtained from doctors and other sources.
    ObtainRecords,
    /// A scheme member agrees that their employer may see the loaded premium
    /// for their underwritten cover on an itemised invoice. A loading implies
    /// health, so without this the employer learns only that cover is in place.
    EmployerSeesLoading,
}

/// Cover the insurer already holds on the person, from the life index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistingCover {
    pub policy: PolicyId,
    pub partition: u16,
    pub cover_type: String,
    pub sum_insured_minor: i64,
}

/// A read of the life index that drove a decision: the rows and the event
/// sequence of each partition the read reflects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistingCoverRead {
    pub rows: Vec<ExistingCover>,
    pub reflects: BTreeMap<u16, u64>,
    pub read_at: DateTime<Utc>,
    /// Shown to the applicant as the answer to the existing-cover question.
    pub prefilled: bool,
}

impl ExistingCoverRead {
    pub fn total(&self, cover_type: &str) -> i64 {
        self.rows.iter().filter(|r| r.cover_type == cover_type).map(|r| r.sum_insured_minor).sum()
    }
}

/// The disclosure as the module holds it: where it is and what it hashes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureRef {
    pub hash: String,
    pub vault_ref: String,
    pub question_set: VersionRef,
    pub submitted_at: DateTime<Utc>,
    pub evidence_of_signing: Signing,
    /// The answers the adviser typed for the client, by count: each is
    /// marked in the record itself.
    pub entered_by_adviser: u32,
    /// The risk codes the answers stand for. Never clinical.
    #[serde(default)]
    pub risk_tags: BTreeSet<String>,
}

/// How the person stood behind a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum Signing {
    /// Signed by the person's device key.
    DeviceKey { key_id: String },
    /// Carried over from a legacy system with its original evidence. Not
    /// signed by a device, because none existed.
    Migrated { legacy_ref: String },
}

/// What one decision says about one cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CoverOutcome {
    /// Accept. Standard rates when there is no loading and no exclusion.
    Accept {
        #[serde(default)]
        loading_percent: u32,
        #[serde(default)]
        exclusions: Vec<AppliedExclusion>,
        /// The amount accepted, where it is less than the amount proposed.
        #[serde(default)]
        revised_sum_insured_minor: Option<i64>,
    },
    Decline,
    /// Not now: the person may apply again after the date.
    Defer {
        until: NaiveDate,
    },
    /// Migrated with no evidence of the original decision. Never defaulted
    /// to standard rates.
    UnknownTerms,
}

impl CoverOutcome {
    pub fn standard() -> Self {
        CoverOutcome::Accept {
            loading_percent: 0,
            exclusions: vec![],
            revised_sum_insured_minor: None,
        }
    }
    pub fn is_accept(&self) -> bool {
        matches!(self, CoverOutcome::Accept { .. })
    }
    pub fn is_standard(&self) -> bool {
        *self == Self::standard()
    }
    pub fn key(&self) -> &'static str {
        match self {
            CoverOutcome::Accept { .. } if self.is_standard() => "standard",
            CoverOutcome::Accept { .. } => "terms",
            CoverOutcome::Decline => "decline",
            CoverOutcome::Defer { .. } => "defer",
            CoverOutcome::UnknownTerms => "unknown_terms",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverTerms {
    pub cover: String,
    pub outcome: CoverOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum DecisionSource {
    /// The rules alone: straight through.
    Rules,
    Underwriter,
    /// Decided by a delegate under a delegated authority.
    Delegated {
        authority: String,
        authority_version: u32,
        delegate_ref: String,
        consent_ref: String,
        decided_by: String,
    },
    /// Terms that arrived as data from a legacy system.
    Migrated {
        legacy_ref: String,
        legacy_underwriter: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// Waiting for the second signature.
    AwaitingCountersign,
    /// Signed; waiting for reinsurance capacity.
    AwaitingCapacity,
    /// The terms that stand.
    Effective,
    /// Replaced by a later decision, or refused at countersign. Kept on file.
    Superseded,
}

/// One person's signature on a decision, with their own reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub underwriter: String,
    /// The authority profile version the signature was made under.
    pub profile_version: u32,
    pub reasons: BTreeSet<String>,
    /// Reasons the customer may not be told, under the stricter access class.
    pub withheld_reasons: BTreeSet<String>,
    pub at: DateTime<Utc>,
}

/// The versions a decision can be reproduced from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    pub rule_set: Option<VersionRef>,
    pub question_set: Option<VersionRef>,
    pub rate_tables: Vec<VersionRef>,
    pub exclusions: Option<VersionRef>,
    pub reasons: Option<VersionRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerFinding {
    pub reviewer: String,
    pub agrees: bool,
    pub reasons: BTreeSet<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// The reference the core stores on the layer.
    pub reference: String,
    pub source: DecisionSource,
    pub status: DecisionStatus,
    pub terms: Vec<CoverTerms>,
    pub versions: Versions,
    /// Reasons the decision carries from its source: the rules that fired,
    /// or the marker of a migrated or delegated decision.
    pub reasons: BTreeSet<String>,
    pub signatures: Vec<Signature>,
    /// The kinds of evidence relied on.
    pub evidence: BTreeSet<EvidenceKind>,
    /// Where the underwriter's notes are, in the clinical store.
    pub notes_ref: Option<String>,
    /// The reinsurer's reference where facultative terms were placed.
    pub facultative_ref: Option<String>,
    pub made_at: DateTime<Utc>,
    pub effective_at: Option<DateTime<Utc>>,
    pub peer_review: Option<PeerFinding>,
    /// Drawn into a peer review sample, for the named period.
    pub sampled_for: Option<String>,
    /// A non-disclosure finding at claim, linked back here.
    pub non_disclosure: Option<String>,
}

impl Decision {
    /// The amount accepted on a cover, given the amount proposed.
    pub fn accepted_sum(&self, cover: &str, proposed_minor: i64) -> Option<i64> {
        match self.terms_for(cover)? {
            CoverOutcome::Accept { revised_sum_insured_minor, .. } => Some(revised_sum_insured_minor.unwrap_or(proposed_minor)),
            _ => None,
        }
    }
    pub fn terms_for(&self, cover: &str) -> Option<&CoverOutcome> {
        self.terms.iter().find(|t| t.cover == cover).map(|t| &t.outcome)
    }
    pub fn all_standard(&self) -> bool {
        self.terms.iter().all(|t| t.outcome.is_standard())
    }
    pub fn any_accept(&self) -> bool {
        self.terms.iter().any(|t| t.outcome.is_accept())
    }
    pub fn any_decline(&self) -> bool {
        self.terms.iter().any(|t| matches!(t.outcome, CoverOutcome::Decline))
    }
    pub fn signers(&self) -> impl Iterator<Item = &str> {
        self.signatures.iter().map(|s| s.underwriter.as_str())
    }
    /// Every reason on the decision: its own and each signer's.
    pub fn all_reasons(&self) -> BTreeSet<String> {
        self.reasons
            .iter()
            .chain(self.signatures.iter().flat_map(|s| s.reasons.iter()))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    Open,
    Accepted,
    Declined,
    /// The customer asked for the terms to be looked at again.
    Countered,
    Expired,
    /// The proposal ended some other way while the offer was open.
    Withdrawn,
}

/// Terms other than standard, offered back to the applicant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub id: String,
    pub decision: String,
    pub status: OfferStatus,
    pub made_at: DateTime<Utc>,
    pub expires_on: NaiveDate,
    pub responded_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    AwaitingDisclosure,
    AwaitingEvidence,
    /// A person is needed: a case is open.
    InAssessment,
    AwaitingCountersign,
    AwaitingCapacity,
    AwaitingFacultative,
    OfferOpen,
    /// Terms stand and, where they needed accepting, are accepted.
    Accepted,
    /// Every cover on the life was declined or deferred.
    Declined,
    OfferDeclined,
}

impl Stage {
    pub fn is_final(self) -> bool {
        matches!(self, Stage::Accepted | Stage::Declined | Stage::OfferDeclined)
    }
    pub fn key(self) -> &'static str {
        match self {
            Stage::AwaitingDisclosure => "awaiting_disclosure",
            Stage::AwaitingEvidence => "awaiting_evidence",
            Stage::InAssessment => "in_assessment",
            Stage::AwaitingCountersign => "awaiting_countersign",
            Stage::AwaitingCapacity => "awaiting_capacity",
            Stage::AwaitingFacultative => "awaiting_facultative",
            Stage::OfferOpen => "offer_open",
            Stage::Accepted => "accepted",
            Stage::Declined => "declined",
            Stage::OfferDeclined => "offer_declined",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapacityState {
    /// Asked; no answer yet.
    Pending,
    /// The treaty takes it automatically.
    Automatic,
    /// Needs the reinsurer's own terms before any offer.
    FacultativeRequired,
    /// The reinsurer's terms are placed.
    Placed {
        reference: String,
    },
    FacultativeDeclined,
    /// The treaty excludes the risk.
    Excluded,
}

/// Reinsurance capacity reserved for one cover for the life of the proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub key: String,
    pub amount_minor: i64,
    pub state: CapacityState,
}

/// A tele-underwriting interview. The answers are the underwriter's notes
/// until the customer confirms them with a signed step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interview {
    pub scheduled_for: DateTime<Utc>,
    pub booked_by: String,
    pub underwriter: Option<String>,
    /// The structured answers as recorded, in the clinical store.
    pub notes_ref: Option<String>,
    pub notes_hash: Option<String>,
    pub recording_ref: Option<String>,
    /// Interpreters and support people present, by role.
    pub present: Vec<String>,
    pub adviser_present: bool,
    pub confirmed_at: Option<DateTime<Utc>>,
}

/// Everything about one life on the proposal. Each life has its own
/// disclosure, evidence and decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeFile {
    pub life: LifeId,
    pub party: PartyId,
    pub factors: LifeFactors,
    pub stage: Stage,
    pub consents: BTreeMap<ConsentKind, DateTime<Utc>>,
    pub existing_cover: Option<ExistingCoverRead>,
    pub disclosures: Vec<DisclosureRef>,
    pub evidence: Vec<EvidenceRequest>,
    pub cases: Vec<Case>,
    pub decisions: Vec<Decision>,
    pub offers: Vec<Offer>,
    pub interviews: Vec<Interview>,
    /// What the rules said for each cover, and under which rule set: the
    /// underwriter starts with the why.
    pub assessments: BTreeMap<String, crate::rules::Assessment>,
    pub rule_set: Option<VersionRef>,
    /// Capacity by cover code.
    pub capacity: BTreeMap<String, Reservation>,
    pub reservations_made: u32,
    /// Set when the adviser's access to this life's answers has ended, with
    /// why: the decision was made, consent was withdrawn, or the adviser's
    /// licence was suspended.
    pub adviser_access_ended: Option<AccessEnd>,
    /// A delegate's file obtained under audit access: vault reference and hash.
    pub delegate_file: Option<(String, String)>,
    /// Another life on the proposal dropped out after this one applied: the
    /// contract is no longer the one applied for, and this life is asked
    /// whether they still want it before anything is issued.
    pub reconfirm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessEnd {
    DecisionMade,
    ConsentWithdrawn,
    LicenceSuspended,
    ProposalEnded,
}

impl LifeFile {
    pub fn new(life: LifeId, party: PartyId, factors: LifeFactors, stage: Stage) -> Self {
        Self {
            life,
            party,
            factors,
            stage,
            consents: BTreeMap::new(),
            existing_cover: None,
            disclosures: vec![],
            evidence: vec![],
            cases: vec![],
            decisions: vec![],
            offers: vec![],
            interviews: vec![],
            assessments: BTreeMap::new(),
            rule_set: None,
            capacity: BTreeMap::new(),
            reservations_made: 0,
            adviser_access_ended: None,
            delegate_file: None,
            reconfirm: false,
        }
    }
    pub fn effective_decision(&self) -> Option<&Decision> {
        self.decisions.iter().rev().find(|d| d.status == DecisionStatus::Effective)
    }
    /// The underwriting case open on the life. A non-disclosure review
    /// opened from a claim is a separate matter.
    pub fn open_case(&self) -> Option<&Case> {
        self.cases
            .iter()
            .find(|c| c.is_open() && c.case_type != crate::case::CaseType::NonDisclosureReview)
    }
    pub fn open_offer(&self) -> Option<&Offer> {
        self.offers.iter().find(|o| o.status == OfferStatus::Open)
    }
    pub fn consented(&self, kind: ConsentKind) -> bool {
        self.consents.contains_key(&kind)
    }
    /// Everyone who has owned a case or signed a decision on this life.
    pub fn underwriters(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self.cases.iter().flat_map(|c| c.touched_by.iter().cloned()).collect();
        out.extend(self.decisions.iter().flat_map(|d| d.signers().map(str::to_string)));
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProposalStatus {
    Open,
    /// Every life is settled; the issue command is with the core.
    Issuing,
    /// The core refused the issue. A person or a new quote is needed.
    IssueFailed {
        code: String,
    },
    Issued {
        at: DateTime<Utc>,
    },
    /// Ran out of time, with the reason recorded.
    Lapsed {
        reason: String,
        at: DateTime<Utc>,
    },
    /// Nothing on the proposal was accepted.
    Declined {
        at: DateTime<Utc>,
    },
    Withdrawn {
        at: DateTime<Utc>,
    },
}

impl ProposalStatus {
    pub fn is_open(&self) -> bool {
        matches!(self, ProposalStatus::Open | ProposalStatus::Issuing | ProposalStatus::IssueFailed { .. })
    }
    pub fn key(&self) -> &'static str {
        match self {
            ProposalStatus::Open => "open",
            ProposalStatus::Issuing => "issuing",
            ProposalStatus::IssueFailed { .. } => "issue_failed",
            ProposalStatus::Issued { .. } => "issued",
            ProposalStatus::Lapsed { .. } => "lapsed",
            ProposalStatus::Declined { .. } => "declined",
            ProposalStatus::Withdrawn { .. } => "withdrawn",
        }
    }
}

/// Accidental death cover while the proposal is in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterimCover {
    pub policy: PolicyId,
    pub issued: bool,
    pub ended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    pub entry: Entry,
    /// The policy the proposal becomes, or changes.
    pub policy: PolicyId,
    pub policy_number: String,
    pub group: GroupId,
    pub policyholder: PartyId,
    pub product_code: String,
    pub frequency: Frequency,
    pub channel: String,
    pub adviser: Option<AdviserRef>,
    pub quote: QuoteBinding,
    /// The contract date: which question set, rules and remedies apply.
    pub received_on: NaiveDate,
    pub received_at: DateTime<Utc>,
    /// The last day of the proposal's life.
    pub expires_on: NaiveDate,
    pub status: ProposalStatus,
    pub lives: BTreeMap<LifeId, LifeFile>,
    pub covers: Vec<ProposedCover>,
    pub interim: Option<InterimCover>,
    /// Came across from a legacy system part-way through.
    pub migrated: bool,
    /// How many times the issue command has been sent to the core.
    pub issue_attempts: u32,
    /// For a group excess case: offers to move the loading on terms already
    /// in place from the employer to the member, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reoffers: Vec<Reoffer>,
}

/// An offer to a scheme member to pay the loading on their excess cover
/// themselves, on the terms already accepted. Staff ask for it when the
/// scheme can no longer carry the loading in the employer's total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reoffer {
    pub id: String,
    pub life: LifeId,
    /// Why staff asked, in their words. Never clinical.
    pub reason: String,
    pub made_at: DateTime<Utc>,
    pub expires_on: NaiveDate,
    pub status: OfferStatus,
    pub responded_at: Option<DateTime<Utc>>,
    /// The terms with the member paying have been written to the core.
    #[serde(default)]
    pub written: bool,
}

impl Proposal {
    /// The offer to move a loading to the member that is waiting for them.
    pub fn open_reoffer(&self) -> Option<&Reoffer> {
        self.reoffers.last().filter(|r| r.status == OfferStatus::Open)
    }
    /// The member agreed to pay their own loading: every issue from then on
    /// says so.
    pub fn member_pays_loading(&self) -> bool {
        self.reoffers.iter().any(|r| r.status == OfferStatus::Accepted)
    }
    pub fn covers_on<'a>(&'a self, life: &'a LifeId) -> impl Iterator<Item = &'a ProposedCover> {
        self.covers.iter().filter(move |c| &c.life == life)
    }
    pub fn cover(&self, life: &LifeId, code: &str) -> Option<&ProposedCover> {
        self.covers.iter().find(|c| &c.life == life && c.code == code)
    }
    /// The largest sum insured proposed on the life, for priority and for
    /// the second-signature threshold.
    pub fn largest_sum_on(&self, life: &LifeId) -> i64 {
        self.covers_on(life).map(|c| c.sum_insured_minor).max().unwrap_or(0)
    }
    pub fn interim_in_force(&self) -> bool {
        self.interim.as_ref().is_some_and(|i| i.issued && !i.ended)
    }
}
