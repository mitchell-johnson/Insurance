//! What the module tells the rest of the system. Everything it wants done
//! outside its own state goes through its outbox: commands for the core,
//! notices for reinsurance, requests for underwriting and for the customer
//! master, and events for everyone else. Events carry identifiers, counts
//! and the names of fields; never a name, a salary or anything about health.

use chrono::NaiveDate;
use ic_core::{GroupId, LifeId, PartyId, PolicyId};
use ic_product::Sex;
use serde::{Deserialize, Serialize};

use crate::diff::Ambiguity;
use crate::mapping::RowReject;
use crate::terms::ExcessPath;

/// What the reinsurance module is told. The shapes are its own inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReinsuranceNotice {
    SetSchemeTerms {
        policy: PolicyId,
        excess_path: ExcessPath,
        sites: Vec<(String, u32)>,
    },
    /// A member who may be a person already insured elsewhere.
    NotePossibleMatch {
        policy: PolicyId,
        life: LifeId,
        party: PartyId,
    },
    ClearPossibleMatch {
        policy: PolicyId,
        life: LifeId,
        party: PartyId,
    },
    RecordRiskFactors {
        policy: PolicyId,
        life: LifeId,
        date_of_birth: Option<NaiveDate>,
        sex: Option<Sex>,
        smoker: Option<bool>,
    },
}

/// What a file would do, in counts. This is the employer's preview.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    pub rows: u32,
    pub joiners: u32,
    pub leavers: u32,
    pub changes: u32,
    pub absences: u32,
    pub rekeys: u32,
    pub rejected: u32,
    pub ambiguities: u32,
    /// Changes dated further back than the employer may date them.
    pub needing_backdating_approval: u32,
    /// Joiners and changes whose formula cover is above the automatic amount.
    pub above_automatic_cover: u32,
    pub members_before: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExceptionKind {
    /// A member who may be someone already known.
    PossibleMatch,
    /// A file the safety check held.
    FileHeld,
    /// A row the core refused when the file was applied.
    RowRefused,
    /// The scheme cannot apply anything until someone acts.
    SchemeBlocked,
    /// A file not approved or applied within the scheme's limit.
    FileStale,
    /// A pool declaration that is overdue.
    DeclarationOverdue,
    /// The scheme keeps paying less than it is invoiced.
    PersistentShortPayment,
    /// Renewal terms not agreed by the renewal date.
    RenewalNotAgreed,
    /// Fewer members than the scheme's minimum are left in the employer's
    /// one total of loadings, so the total may show whose loading it is.
    LoadingTotalTooSmall,
    /// The commission module would not record the scheme's broker on a
    /// cover, so that cover earns no commission until someone acts.
    BrokerNotRecorded,
    /// A letter the member must have could not be asked of the documents
    /// module: the member is not linked to a person yet, or the module
    /// refused it. Staff send it another way. The subject is the request.
    LetterNotRequested,
}

/// A leaver's underwritten cover as underwriting's `group_continuation`
/// proposal: a layer on the continuation policy, on the earlier terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationExcess {
    /// `grp-cont:{scheme}:{offer}`: the proposal's name and key there.
    pub key: String,
    pub policy: PolicyId,
    pub group: GroupId,
    pub party: PartyId,
    pub product_code: String,
    pub cover: String,
    pub cover_type: String,
    pub date_of_birth: Option<NaiveDate>,
    pub sex: Option<Sex>,
    pub smoker: Option<bool>,
    pub amount_minor: i64,
    /// The continuation policy's first day, which the layer starts from.
    pub inception: NaiveDate,
    pub scheme_policy: PolicyId,
    pub scheme_life: LifeId,
    pub earlier: Vec<String>,
}

/// A letter the documents module issues to a member: its `request_letter`
/// with the facts, never wording. Kinds are the documents module's
/// (`absence_started`, `absence_limit_reached`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberLetter {
    /// Stable across retries: `grp:absence:{scheme}:{life}:{since}` with
    /// the kind, so a resend is one letter.
    pub request: String,
    pub kind: String,
    pub party: PartyId,
    pub policy: PolicyId,
    pub scheme: String,
    pub life: LifeId,
    pub since: NaiveDate,
    pub cover_ends: NaiveDate,
    pub business_date: NaiveDate,
}

/// A scheme cover as the commission module's `write_layer`: its one layer,
/// numbered 0, written by the scheme's broker into the broker's book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommissionLayer {
    pub policy: PolicyId,
    pub cover: String,
    pub engagement: String,
    pub book: String,
    pub schedule: String,
    pub option: String,
    pub annual_premium_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GroupEvent {
    SchemeRegistered {
        scheme: String,
        policy: PolicyId,
        terms_version: u32,
    },
    TermsAmended {
        scheme: String,
        terms_version: u32,
    },
    /// A member chose a health plan and tier. The tier is the employer's to
    /// see; dependants are never named.
    HealthPlanChosen {
        scheme: String,
        life: LifeId,
        plan: String,
        tier: crate::terms::HealthTier,
        effective: NaiveDate,
    },
    /// The core refused a step of a member's health plan; the rest was not
    /// sent. What was in place before stays, and staff are told.
    HealthPlanRefused {
        scheme: String,
        life: LifeId,
        code: String,
    },
    /// A member's letter could not be asked for; staff are told by the
    /// exception `letter_not_requested`.
    LetterNotRequested {
        scheme: String,
        request: String,
        code: String,
    },
    /// The commission module refused the scheme's broker on a cover.
    BrokerNotRecorded {
        scheme: String,
        cover: String,
        code: String,
    },
    MappingSet {
        scheme: String,
        mapping: String,
        version: u32,
        content_hash: String,
    },
    SchemeFrozen {
        scheme: String,
        reason: String,
    },
    SchemeUnfrozen {
        scheme: String,
    },
    SchemeBlocked {
        scheme: String,
        code: String,
    },
    SchemeResumed {
        scheme: String,
    },

    FileQuarantined {
        scheme: String,
        file: String,
    },
    FileRefused {
        scheme: String,
        file: String,
        code: String,
    },
    FileScanned {
        scheme: String,
        file: String,
    },
    /// The preview: what applying the file would do.
    FileCompared {
        scheme: String,
        file: String,
        summary: FileSummary,
        rejected: Vec<RowReject>,
        ambiguities: Vec<Ambiguity>,
    },
    FileHeld {
        scheme: String,
        file: String,
        leavers: u32,
        members: u32,
    },
    FileAwaitingApproval {
        scheme: String,
        file: String,
    },
    FileAwaitingBackdatingDecision {
        scheme: String,
        file: String,
        changes: u32,
    },
    FileApproved {
        scheme: String,
        file: String,
        by: String,
    },
    FileWithdrawn {
        scheme: String,
        file: String,
    },
    FileApplied {
        scheme: String,
        file: String,
        applied: u32,
        refused: u32,
        scheduled: u32,
    },
    RowRefused {
        scheme: String,
        file: Option<String>,
        employee_id: String,
        change: String,
        code: String,
    },

    MemberLinked {
        scheme: String,
        life: LifeId,
        party: PartyId,
        how: String,
    },
    PossibleMatchRaised {
        scheme: String,
        life: LifeId,
        candidates: u32,
    },
    PossibleMatchResolved {
        scheme: String,
        life: LifeId,
    },
    MembersReinstated {
        scheme: String,
        count: u32,
        reason: String,
    },

    /// Cover above the automatic amount needs a decision. The amount is the
    /// formula's result on salary and category; it says nothing of health.
    ExcessUnderwritingRequested {
        scheme: String,
        case: u64,
        life: LifeId,
        cover: String,
        excess_minor: i64,
        path: ExcessPath,
        reference: Option<String>,
    },
    /// The employer-visible outcome: in place or not, never why.
    ExcessCoverDecided {
        scheme: String,
        case: u64,
        life: LifeId,
        in_place: bool,
    },
    /// Underwriting offered a member the loading to pay themselves and it
    /// was not taken up. Nothing changed; staff who asked for it are told.
    LoadingMoveDeclined {
        scheme: String,
        case: u64,
    },
    ExcessCoverEnded {
        scheme: String,
        case: u64,
        life: LifeId,
    },

    /// A member went on a leave of absence (an overseas posting, say). The
    /// cover continues to `cover_ends` unless they come back before then,
    /// which is what the member is told.
    AbsenceStarted {
        scheme: String,
        life: LifeId,
        since: NaiveDate,
        cover_ends: NaiveDate,
    },
    AbsenceLimitReached {
        scheme: String,
        life: LifeId,
        cover_ends: NaiveDate,
    },
    ContinuationOffered {
        scheme: String,
        offer: u64,
        life: LifeId,
        expires: NaiveDate,
    },
    ContinuationTakenUp {
        offer: u64,
        scheme: String,
        life: LifeId,
        new_policy: PolicyId,
        new_group: GroupId,
    },
    /// Underwriting decided on the underwritten part of a continuation:
    /// `issued`, or the outcome that ended it.
    ContinuedExcessDecided {
        scheme: String,
        offer: u64,
        outcome: String,
    },
    ContinuationRefused {
        scheme: String,
        offer: u64,
        code: String,
    },
    ContinuationExpired {
        scheme: String,
        offer: u64,
    },

    PoolDeclared {
        scheme: String,
        pool: String,
        effective: NaiveDate,
        headcount: u32,
    },

    InvoiceIssued {
        scheme: String,
        period_index: u32,
        total_minor: i64,
        members: u32,
    },
    ShortPaymentRecorded {
        scheme: String,
        in_a_row: u32,
        shortfall_minor: i64,
    },
    JoinersSuspended {
        scheme: String,
    },
    JoinersResumed {
        scheme: String,
    },

    RenewalOpened {
        scheme: String,
        renewal_date: NaiveDate,
        members: u32,
    },
    RenewalAgreed {
        scheme: String,
        rate_table: String,
        terms_version: u32,
    },
    SchemeRenewed {
        scheme: String,
        scheme_year_start: NaiveDate,
        terms_version: u32,
    },
    RenewalRunningOnOldRates {
        scheme: String,
        until: NaiveDate,
    },
    TerminationStarted {
        scheme: String,
        effective: NaiveDate,
        reason: String,
    },
    SchemeTerminated {
        scheme: String,
        effective: NaiveDate,
    },
    /// Staff began moving members of a scheme to a new one.
    SplitStarted {
        scheme: String,
        into: String,
        new_policy: PolicyId,
        effective: NaiveDate,
        members: u32,
    },
    /// The core moved a part of the members. The new scheme exists from the
    /// first part; `moved` counts every member moved so far.
    MembersMoved {
        scheme: String,
        into: String,
        moved: u32,
    },
    SplitCompleted {
        scheme: String,
        into: String,
        effective: NaiveDate,
        members: u32,
    },
    /// The core refused a part of the split. The members moved before it
    /// stay on the new scheme; the rest stay where they were.
    SplitRefused {
        scheme: String,
        into: String,
        moved: u32,
        code: String,
    },

    ExceptionRaised {
        id: u64,
        scheme: String,
        kind: ExceptionKind,
        subject: String,
    },
    ExceptionCleared {
        id: u64,
        scheme: String,
        kind: ExceptionKind,
    },
}

/// A request to the underwriting module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnderwritingRequest {
    pub scheme: String,
    pub case: u64,
    pub policy: PolicyId,
    pub life: LifeId,
    /// Who the member is. None until the member is linked to a person:
    /// underwriting cannot ask a nobody for a disclosure, so the driver
    /// holds the request until the link is made.
    pub party: Option<PartyId>,
    /// The scheme policy's billing group, policyholder and product, which
    /// underwriting records on its proposal.
    pub group: GroupId,
    pub policyholder: PartyId,
    pub product_code: String,
    pub product_hash: String,
    pub cover: String,
    pub cover_type: String,
    /// What the employer's file says of the member. Salary and category
    /// stay here: underwriting is told the amount, not how it was reached.
    pub date_of_birth: Option<NaiveDate>,
    pub sex: Option<Sex>,
    pub smoker: Option<bool>,
    pub excess_minor: i64,
    pub effective: NaiveDate,
    /// The reinsurer decides (facultative) or our underwriters do.
    pub path: ExcessPath,
    /// Cover the previous insurer had accepted is honoured without evidence
    /// up to this amount, under the scheme's takeover terms.
    pub honour_without_evidence_minor: Option<i64>,
    pub reference: Option<String>,
    /// The delegated authority that decides, where the scheme has one and
    /// reinsurance did not ask for our own underwriting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_authority: Option<String>,
}

/// What the core made of accepted terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueOutcome {
    Issued { life: LifeId, cover: String, layer: u32 },
    Rejected { code: String },
}

/// One thing the driver must do. It stays in the module's state until the
/// driver reports it done, so a crash between the two loses nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outbound {
    /// Ask the documents module for a letter to a member.
    Letter(MemberLetter),
    /// Tell the commission module who placed a scheme cover, so that the
    /// premium the scheme pays for it earns the broker's commission.
    Commission {
        key: String,
        layer: CommissionLayer,
    },
    /// Send to the core as `group-admin`, to the partition of `group`, and
    /// report the answer with `CoreResult`.
    Core {
        key: String,
        group: GroupId,
        command: Box<ic_core::Command>,
    },
    Reinsurance {
        key: String,
        notice: ReinsuranceNotice,
    },
    Underwriting {
        key: String,
        request: UnderwritingRequest,
    },
    /// Ask underwriting to continue a leaver's underwritten cover on the
    /// continuation policy, on the terms it accepted before.
    ContinueExcess(ContinuationExcess),
    /// A request no longer wanted: the member left, or the cover fell back
    /// under the automatic amount, before underwriting answered.
    WithdrawUnderwriting {
        key: String,
        /// The key the request went under, which is its name there.
        request_key: String,
    },
    /// What became of the terms underwriting accepted: the layer the core
    /// wrote, so that underwriting can confirm reinsurance capacity against
    /// it, or the core's refusal, so that a person there sees it.
    UnderwritingOutcome {
        key: String,
        /// The key the request went under, which is its name there.
        request_key: String,
        outcome: IssueOutcome,
    },
    /// Match these joiners of a file against the customer master and
    /// answer with `PartiesResolved`. The driver holds the identities; the
    /// state holds only who is waiting.
    ResolveParties {
        scheme: String,
        file: String,
        employee_ids: Vec<String>,
    },
    Publish {
        seq: u64,
        date: NaiveDate,
        event: GroupEvent,
    },
}
