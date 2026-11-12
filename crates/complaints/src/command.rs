//! Commands, configuration and events of the complaints module.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::{PartyId, PolicyId};
use serde::{Deserialize, Serialize};

use crate::model::*;

/// What goes into the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A duplicate returns the original result.
    pub key: String,
    /// The service sending the command.
    pub caller: String,
    /// The person responsible. Separation of duties is checked on this.
    pub actor: String,
    /// The proposer's clock. Time moves only as commands carry it.
    pub at: DateTime<Utc>,
    pub command: Command,
}

/// The timers and limits, from the conduct regime and the external
/// scheme's terms of reference. Versioned and immutable: each complaint
/// runs under the version in force when it was received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timers {
    pub version: u32,
    pub acknowledge_days: u32,
    pub update_every_days: u32,
    pub decide_days: u32,
    /// The scheme's period after which an unresolved complaint is deadlocked.
    pub deadlock_days: u32,
    /// How long before a timer is due its warning is raised.
    pub warning_days: u32,
    /// How long after a timer is due it goes to the conduct officer.
    pub conduct_officer_after_days: u32,
    /// How long the complainant is given to answer a request.
    pub complainant_response_days: u32,
    /// The further time a complainant marked vulnerable is given. The
    /// insurer's own timers are never extended.
    pub vulnerable_extra_days: u32,
    /// What the person who hears a service complaint may spend to put it
    /// right on the spot.
    pub front_line_limit_minor: i64,
    /// The most an ex gratia payment may be at each authority level.
    pub ex_gratia_limits_minor: BTreeMap<u8, i64>,
    /// How long after a conduct case opens the regulator must be told,
    /// where it must be.
    pub regulator_notification_days: u32,
    /// How long a closed complaint is kept before its record is removed.
    pub retain_closed_days: u32,
    /// The cause codes a complaint may be closed with.
    pub cause_codes: BTreeSet<String>,
}

/// One command creates the complaint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intake {
    pub complaint: String,
    pub channel: IntakeChannel,
    pub complainant: PartyId,
    pub complainant_kind: ComplainantKind,
    #[serde(default)]
    pub links: Links,
    pub category: Category,
    pub summary_document: String,
    #[serde(default)]
    pub vulnerable: bool,
    #[serde(default)]
    pub adjustments: Vec<String>,
    /// Resolved by the person who heard it. Recorded all the same, because
    /// the pattern matters.
    #[serde(default)]
    pub resolved_on_the_spot: Option<FrontLine>,
    /// Migration only: the date it was first received, so its timers
    /// continue from there.
    #[serde(default)]
    pub received_on: Option<NaiveDate>,
    /// Migration only: the acknowledgement already sent.
    #[serde(default)]
    pub acknowledged_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontLine {
    pub outcome: OutcomeKind,
    pub reason_codes: Vec<String>,
    pub cause: String,
    /// What was spent putting it right, and the command that did it.
    #[serde(default)]
    pub put_right: Option<(String, i64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseDetail {
    pub cause: String,
    #[serde(default)]
    pub product_version: Option<String>,
    #[serde(default)]
    pub rule_version: Option<String>,
    #[serde(default)]
    pub template_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    ActivateTimers(Timers),

    // --- complaints ---
    RecordComplaint(Box<Intake>),
    RefineCategory {
        complaint: String,
        category: Category,
    },
    AddRepresentative {
        complaint: String,
        representative: Representative,
    },
    /// Set or clear the vulnerability marker. Always a person's decision.
    SetVulnerability {
        complaint: String,
        vulnerable: bool,
        adjustments: Vec<String>,
    },
    Assign {
        complaint: String,
        handler: String,
        /// The handler's authority level, from the staff directory.
        authority: u8,
        /// The handler is trained to work with vulnerable customers.
        trained_for_vulnerability: bool,
    },
    RecordAcknowledgement {
        complaint: String,
        document: String,
    },
    RecordUpdate {
        complaint: String,
        document: String,
    },
    /// Ask the complainant for something. Their time to answer is longer
    /// if they are marked vulnerable; the insurer's timers do not move.
    AskComplainant {
        complaint: String,
    },
    RecordComplainantResponse {
        complaint: String,
    },
    /// Give someone access to the file for this case and a stated purpose.
    GrantAccess {
        complaint: String,
        to: String,
        classes: BTreeSet<DataClass>,
        purpose: String,
    },
    RevokeAccess {
        complaint: String,
        from: String,
    },
    Decide {
        complaint: String,
        outcome: OutcomeKind,
        reason_codes: Vec<String>,
        /// The written outcome. Required unless the complaint was withdrawn.
        document: Option<String>,
    },
    RecordOutcomeDelivered {
        complaint: String,
        on: NaiveDate,
    },
    RecordRemediation {
        complaint: String,
        kind: RemediationKind,
        reference: String,
        amount_minor: i64,
        /// Who authorised it, and at what level.
        authorised_by: String,
        authority: u8,
    },
    CompleteRemediation {
        complaint: String,
        reference: String,
    },
    /// The cause affects others: a remediation case is opened for the
    /// population and the conduct officer is told.
    RecordSystemicFinding {
        complaint: String,
        remediation_case: String,
    },
    /// The insurer decides it cannot resolve the complaint.
    DeclareDeadlock {
        complaint: String,
    },
    RecordDeadlockNotice {
        complaint: String,
        document: String,
    },
    Refer {
        complaint: String,
        forum: Forum,
        reference: String,
        response_due: NaiveDate,
        documents_due: Option<NaiveDate>,
    },
    RecordForumResponse {
        complaint: String,
    },
    ReleaseBundle {
        complaint: String,
        bundle: String,
        documents: Vec<String>,
        reviewed_by: String,
    },
    RecordForumDecision {
        complaint: String,
        outcome: OutcomeKind,
        binding: bool,
        remediation_due: Option<NaiveDate>,
    },
    CompleteForumRemediation {
        complaint: String,
    },
    Close {
        complaint: String,
        detail: CloseDetail,
    },
    RecordRootCauseReview {
        /// `2027-11`.
        month: String,
        actions: Vec<Action>,
    },

    // --- conduct cases ---
    OpenConductCase {
        case: String,
        kind: ConductKind,
        owner: String,
        summary_document: Option<String>,
        /// The regulator must be told of this matter.
        notify_regulator: bool,
    },
    /// One chunk of the affected population.
    AttachPopulation {
        case: String,
        customers: Vec<PartyId>,
        advisers: Vec<PartyId>,
        policies: Vec<PolicyId>,
    },
    CompletePopulation {
        case: String,
        customers: u32,
        policies: u32,
    },
    RecordCustomerNotified {
        case: String,
        customer: PartyId,
        document: String,
    },
    ExcludeCustomer {
        case: String,
        customer: PartyId,
        reason: String,
    },
    /// Draw the advice files to review. The draw is a function of the case
    /// and the policies, so it can be shown not to have been chosen.
    DrawSample {
        case: String,
        size: u32,
    },
    RecordFileReview {
        case: String,
        policy: PolicyId,
        finding: ReviewFinding,
    },
    RecordRegulatorNotification {
        case: String,
        reference: String,
        document: String,
    },
    RecordRegulatorRequest {
        case: String,
        request: String,
        due: NaiveDate,
        document: String,
    },
    RespondToRegulator {
        case: String,
        request: String,
        document: String,
    },
    LinkComplaint {
        case: String,
        complaint: String,
    },
    CloseConductCase {
        case: String,
        findings_document: String,
    },

    // --- time and the stream ---
    /// Let time pass: raise warnings and escalations, reach deadlock,
    /// remove records past their retention. Bounded per command.
    Tick,
    MarkPublished {
        through_seq: u64,
    },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::ActivateTimers(_) => "activate_timers",
            Command::RecordComplaint(_) => "record_complaint",
            Command::RefineCategory { .. } => "refine_category",
            Command::AddRepresentative { .. } => "add_representative",
            Command::SetVulnerability { .. } => "set_vulnerability",
            Command::Assign { .. } => "assign",
            Command::RecordAcknowledgement { .. } => "record_acknowledgement",
            Command::RecordUpdate { .. } => "record_update",
            Command::AskComplainant { .. } => "ask_complainant",
            Command::RecordComplainantResponse { .. } => "record_complainant_response",
            Command::GrantAccess { .. } => "grant_access",
            Command::RevokeAccess { .. } => "revoke_access",
            Command::Decide { .. } => "decide",
            Command::RecordOutcomeDelivered { .. } => "record_outcome_delivered",
            Command::RecordRemediation { .. } => "record_remediation",
            Command::CompleteRemediation { .. } => "complete_remediation",
            Command::RecordSystemicFinding { .. } => "record_systemic_finding",
            Command::DeclareDeadlock { .. } => "declare_deadlock",
            Command::RecordDeadlockNotice { .. } => "record_deadlock_notice",
            Command::Refer { .. } => "refer",
            Command::RecordForumResponse { .. } => "record_forum_response",
            Command::ReleaseBundle { .. } => "release_bundle",
            Command::RecordForumDecision { .. } => "record_forum_decision",
            Command::CompleteForumRemediation { .. } => "complete_forum_remediation",
            Command::Close { .. } => "close",
            Command::RecordRootCauseReview { .. } => "record_root_cause_review",
            Command::OpenConductCase { .. } => "open_conduct_case",
            Command::AttachPopulation { .. } => "attach_population",
            Command::CompletePopulation { .. } => "complete_population",
            Command::RecordCustomerNotified { .. } => "record_customer_notified",
            Command::ExcludeCustomer { .. } => "exclude_customer",
            Command::DrawSample { .. } => "draw_sample",
            Command::RecordFileReview { .. } => "record_file_review",
            Command::RecordRegulatorNotification { .. } => "record_regulator_notification",
            Command::RecordRegulatorRequest { .. } => "record_regulator_request",
            Command::RespondToRegulator { .. } => "respond_to_regulator",
            Command::LinkComplaint { .. } => "link_complaint",
            Command::CloseConductCase { .. } => "close_conduct_case",
            Command::Tick => "tick",
            Command::MarkPublished { .. } => "mark_published",
        }
    }

    /// The services allowed to send each command.
    ///
    /// `intake` is any service a complaint can be heard through: the app,
    /// the portals, the claims and servicing workbenches. `complaints` is
    /// the complaints team's workbench, which reports outside the
    /// functions it reviews. `conduct` and `legal` are theirs alone.
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            Command::ActivateTimers(_) => &["config"],
            Command::RecordComplaint(_) => &["intake", "complaints", "migration"],
            Command::RefineCategory { .. }
            | Command::AddRepresentative { .. }
            | Command::SetVulnerability { .. }
            | Command::Assign { .. }
            | Command::RecordAcknowledgement { .. }
            | Command::RecordUpdate { .. }
            | Command::AskComplainant { .. }
            | Command::RecordComplainantResponse { .. }
            | Command::GrantAccess { .. }
            | Command::RevokeAccess { .. }
            | Command::Decide { .. }
            | Command::RecordRemediation { .. }
            | Command::CompleteRemediation { .. }
            | Command::RecordSystemicFinding { .. }
            | Command::DeclareDeadlock { .. }
            | Command::RecordDeadlockNotice { .. }
            | Command::Close { .. }
            | Command::RecordRootCauseReview { .. } => &["complaints"],
            Command::RecordOutcomeDelivered { .. } => &["documents", "complaints"],
            Command::Refer { .. }
            | Command::RecordForumResponse { .. }
            | Command::ReleaseBundle { .. }
            | Command::RecordForumDecision { .. }
            | Command::CompleteForumRemediation { .. } => &["complaints", "legal"],
            Command::OpenConductCase { .. } => &["conduct", "distribution", "complaints"],
            Command::AttachPopulation { .. } | Command::CompletePopulation { .. } => &["conduct", "distribution"],
            Command::RecordCustomerNotified { .. } => &["conduct", "documents"],
            Command::ExcludeCustomer { .. }
            | Command::DrawSample { .. }
            | Command::RecordFileReview { .. }
            | Command::RecordRegulatorNotification { .. }
            | Command::RecordRegulatorRequest { .. }
            | Command::RespondToRegulator { .. }
            | Command::LinkComplaint { .. }
            | Command::CloseConductCase { .. } => &["conduct", "legal"],
            Command::Tick => &["scheduler"],
            Command::MarkPublished { .. } => &["relay"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// `cmp-<seq>`: unique. Consumers de-duplicate on it.
    pub id: String,
    /// Gapless.
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub log_index: u64,
    pub event: Event,
}

/// What the module tells other services. No personal detail beyond IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    TimersActivated {
        version: u32,
    },
    ComplaintRecorded {
        complaint: String,
        complainant: PartyId,
        process: Process,
        vulnerable: bool,
    },
    ComplaintAssigned {
        complaint: String,
        handler: String,
    },
    /// A document is due to the complainant: the documents service issues it.
    AcknowledgementDue {
        complaint: String,
        by: NaiveDate,
    },
    Acknowledged {
        complaint: String,
    },
    ComplainantUpdated {
        complaint: String,
    },
    AccessGranted {
        complaint: String,
        to: String,
        clinical: bool,
        purpose: String,
    },
    AccessRevoked {
        complaint: String,
        from: String,
    },
    TimerEscalated {
        subject: String,
        timer: Timer,
        level: Level,
        due: NaiveDate,
    },
    Decided {
        complaint: String,
        outcome: OutcomeKind,
        overturned: Vec<String>,
    },
    OutcomeDelivered {
        complaint: String,
        on: NaiveDate,
    },
    RemediationRecorded {
        complaint: String,
        kind: RemediationKind,
        reference: String,
        amount_minor: i64,
    },
    /// The conduct officer is told.
    SystemicFindingRaised {
        complaint: String,
        remediation_case: String,
    },
    /// A deadlock notice must go to the complainant, with their right to
    /// refer the matter to the scheme.
    DeadlockReached {
        complaint: String,
        automatic: bool,
    },
    DeadlockNoticeSent {
        complaint: String,
    },
    Referred {
        complaint: String,
        forum: Forum,
        reference: String,
    },
    BundleReleased {
        complaint: String,
        bundle: String,
        documents: u32,
    },
    ForumDecided {
        complaint: String,
        outcome: OutcomeKind,
        binding: bool,
    },
    ComplaintClosed {
        complaint: String,
        cause: String,
        adviser_engagement: Option<String>,
    },
    ComplaintRecordRemoved {
        complaint: String,
    },
    RootCauseReviewed {
        month: String,
        actions: u32,
    },
    ConductCaseOpened {
        case: String,
        owner: String,
        licence_event: Option<String>,
    },
    PopulationAttached {
        case: String,
        customers: u32,
        policies: u32,
    },
    /// Every affected customer is attached: communication can begin.
    PopulationComplete {
        case: String,
        customers: u32,
    },
    SampleDrawn {
        case: String,
        files: u32,
    },
    FileReviewed {
        case: String,
        policy: PolicyId,
        remediation_case: Option<String>,
    },
    RegulatorNotified {
        case: String,
        reference: String,
    },
    RegulatorRequestReceived {
        case: String,
        request: String,
        due: NaiveDate,
    },
    RegulatorAnswered {
        case: String,
        request: String,
    },
    ConductCaseClosed {
        case: String,
    },
}
