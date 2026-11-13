//! Commands: the only way the module's state changes.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::{EventRecord, PartyId, PolicyId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::kinds::DocumentKind;
use crate::model::{AttemptOutcome, Channel, DefectCause, LetterRequest, Recipient, RetentionRule};
use crate::template::{AlternateFormat, Part, Template, TemplateRef};

/// What goes into the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A duplicate returns the original result.
    pub key: String,
    /// The service sending the command.
    pub caller: String,
    /// The person or process responsible, for approvals and the record.
    pub actor: String,
    /// The proposer's clock. The state machine never reads one: time moves
    /// only as commands carry it, and never backwards.
    pub at: DateTime<Utc>,
    pub command: Command,
}

/// Settings that are the business's to choose. Versioned and immutable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    /// The highest school reading grade a template may have, times ten.
    pub max_reading_grade_x10: u32,
    /// How long an electronic send may go unconfirmed before a document
    /// that must be proven falls back to the next channel.
    pub electronic_confirm_hours: u32,
    /// Days after lodgement at which post is deemed delivered.
    pub post_deemed_days: u32,
    /// Default days a choice stays reopenable after a corrected notice.
    pub reopen_grace_days: u32,
    /// Core events older than this many days are not acted on, so a replay
    /// of the stream cannot send a flood of old documents.
    pub live_event_days: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 0,
            max_reading_grade_x10: 120,
            electronic_confirm_hours: 72,
            post_deemed_days: 5,
            reopen_grace_days: 30,
            live_event_days: 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRole {
    Legal,
    Product,
    Compliance,
}

/// One rendering test: a persona's data for a product version, and the
/// hash the rendered document must have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderCase {
    pub product_code: String,
    pub product_version: u32,
    pub persona: String,
    pub language: String,
    /// Synthetic data, never a real customer's.
    pub data: Value,
    /// Every field the product version supplies, in the renderer's path
    /// notation. The template may not require a field outside this list.
    pub product_fields: Vec<String>,
    pub expected_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeSpec {
    pub purpose: String,
    /// For a corrected notice: the defect, and the notice it corrects.
    #[serde(default)]
    pub corrects: Option<crate::model::Correction>,
}

/// The record of a document the driver has rendered and put in the vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub document: String,
    pub kind: DocumentKind,
    pub template: TemplateRef,
    #[serde(default)]
    pub policy: Option<PolicyId>,
    #[serde(default)]
    pub other_policies: Vec<PolicyId>,
    #[serde(default)]
    pub data_class: crate::kinds::DataClass,
    #[serde(default)]
    pub lives: Vec<String>,
    /// Ask for proof of delivery for a document whose kind does not
    /// already require it, such as a claim acknowledgement.
    #[serde(default)]
    pub prove_delivery: bool,
    pub owner: PartyId,
    pub recipients: Vec<Recipient>,
    pub language: String,
    #[serde(default)]
    pub format: Option<AlternateFormat>,
    pub content_sha256: String,
    pub data_sha256: String,
    pub blob: String,
    pub wrapped_key: String,
    pub bytes: u64,
    #[serde(default)]
    pub trigger_event: Option<String>,
    #[serde(default)]
    pub case: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub wordings: Vec<TemplateRef>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub notice: Option<NoticeSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyImport {
    pub document: String,
    pub kind: DocumentKind,
    pub policy: Option<PolicyId>,
    #[serde(default)]
    pub data_class: crate::kinds::DataClass,
    pub owner: PartyId,
    pub issued_on: NaiveDate,
    pub language: String,
    pub content_sha256: String,
    pub blob: String,
    pub wrapped_key: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldScope {
    Document(String),
    Party(PartyId),
    Policy(PolicyId),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    // --- configuration ---
    ActivateSettings(Settings),
    ActivateRetention {
        version: u32,
        rules: BTreeMap<DocumentKind, RetentionRule>,
    },
    RegisterPart(Part),
    SubmitTemplate(Box<Template>),
    ApproveTemplate {
        template: TemplateRef,
        role: ApprovalRole,
    },
    /// Render a sample of personas and compare with the expected output.
    /// The result is recorded whether it passes or fails.
    RunRenderingTests {
        template: TemplateRef,
        cases: Vec<RenderCase>,
    },
    ActivateTemplate {
        template: TemplateRef,
        effective_from: NaiveDate,
    },
    /// Containment when a template is found to be wrong: nothing more is
    /// issued from this version.
    DisableTemplate {
        template: TemplateRef,
        reason: String,
        case: String,
    },

    // --- documents ---
    /// The owner's key, wrapped by the Key Vault key. The first one stands.
    RegisterPartyKey {
        party: PartyId,
        wrapped_key: String,
    },
    IssueDocument(Box<Issue>),
    ImportLegacy(Box<LegacyImport>),

    // --- delivery ---
    RecordSend {
        document: String,
        party: PartyId,
        channel: Channel,
        masked_destination: String,
        provider_reference: String,
    },
    RecordOutcome {
        document: String,
        party: PartyId,
        channel: Channel,
        outcome: AttemptOutcome,
    },
    /// Start delivery again for a recipient whose document was
    /// undeliverable, once their contact details are corrected.
    RetryDelivery {
        document: String,
        party: PartyId,
        channels: Vec<Channel>,
    },
    /// Let time pass: unconfirmed sends fall back, lodged post is deemed
    /// delivered, retention periods end. Does a bounded amount per command.
    Tick,

    // --- notice defects ---
    RaiseNoticeDefect {
        defect: String,
        case: String,
        description: String,
        cause: DefectCause,
        from: NaiveDate,
        to: NaiveDate,
        #[serde(default)]
        grace_days: Option<u32>,
    },
    /// One chunk of the population.
    AttachDefectNotices {
        defect: String,
        notices: Vec<String>,
    },
    ExcludeFromDefect {
        defect: String,
        notice: String,
        reason: String,
    },
    CloseNoticeDefect {
        defect: String,
        /// The control that should have caught it, and now will.
        control: String,
    },

    // --- the core ---
    /// The next events of one core partition, in order.
    ObserveCoreEvents {
        partition: u16,
        records: Vec<EventRecord>,
    },
    DismissTrigger {
        trigger: String,
        reason: String,
    },
    AckCoreInstructions {
        ids: Vec<String>,
    },
    MarkPublished {
        through_seq: u64,
    },

    // --- retention ---
    PlaceLegalHold {
        scope: HoldScope,
        case: String,
    },
    ReleaseLegalHold {
        scope: HoldScope,
    },
    /// Crypto-shred a party: destroy what may be destroyed now, and the
    /// rest as each retention period ends.
    /// The channel rows of these documents were written or taken down.
    AckListing {
        documents: Vec<(String, bool)>,
    },
    /// A letter another module asks for (`model::REQUESTED_KINDS`).
    RequestLetter(LetterRequest),
    /// Drop a requested letter that is still waiting.
    CancelLetterRequest {
        request: String,
        reason: String,
    },
    ShredParty {
        party: PartyId,
    },
    AckDeleted {
        documents: Vec<String>,
    },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::ActivateSettings(_) => "activate_settings",
            Command::ActivateRetention { .. } => "activate_retention",
            Command::RegisterPart(_) => "register_part",
            Command::SubmitTemplate(_) => "submit_template",
            Command::ApproveTemplate { .. } => "approve_template",
            Command::RunRenderingTests { .. } => "run_rendering_tests",
            Command::ActivateTemplate { .. } => "activate_template",
            Command::DisableTemplate { .. } => "disable_template",
            Command::RegisterPartyKey { .. } => "register_party_key",
            Command::IssueDocument(_) => "issue_document",
            Command::ImportLegacy(_) => "import_legacy",
            Command::RequestLetter(_) => "request_letter",
            Command::AckListing { .. } => "ack_listing",
            Command::CancelLetterRequest { .. } => "cancel_letter_request",
            Command::RecordSend { .. } => "record_send",
            Command::RecordOutcome { .. } => "record_outcome",
            Command::RetryDelivery { .. } => "retry_delivery",
            Command::Tick => "tick",
            Command::RaiseNoticeDefect { .. } => "raise_notice_defect",
            Command::AttachDefectNotices { .. } => "attach_defect_notices",
            Command::ExcludeFromDefect { .. } => "exclude_from_defect",
            Command::CloseNoticeDefect { .. } => "close_notice_defect",
            Command::ObserveCoreEvents { .. } => "observe_core_events",
            Command::DismissTrigger { .. } => "dismiss_trigger",
            Command::AckCoreInstructions { .. } => "ack_core_instructions",
            Command::MarkPublished { .. } => "mark_published",
            Command::PlaceLegalHold { .. } => "place_legal_hold",
            Command::ReleaseLegalHold { .. } => "release_legal_hold",
            Command::ShredParty { .. } => "shred_party",
            Command::AckDeleted { .. } => "ack_deleted",
        }
    }

    /// The services allowed to send each command.
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            Command::ActivateSettings(_)
            | Command::ActivateRetention { .. }
            | Command::RegisterPart(_)
            | Command::SubmitTemplate(_)
            | Command::ApproveTemplate { .. }
            | Command::RunRenderingTests { .. }
            | Command::ActivateTemplate { .. } => &["config"],
            Command::DisableTemplate { .. } => &["config", "remediation"],
            Command::RegisterPartyKey { .. } | Command::IssueDocument(_) => &["documents", "remediation", "complaints"],
            Command::ImportLegacy(_) => &["migration"],
            Command::RecordSend { .. } | Command::RecordOutcome { .. } | Command::Tick => &["delivery"],
            Command::RetryDelivery { .. } => &["delivery", "servicing"],
            Command::RaiseNoticeDefect { .. } | Command::AttachDefectNotices { .. } | Command::ExcludeFromDefect { .. } | Command::CloseNoticeDefect { .. } => {
                &["remediation"]
            }
            Command::ObserveCoreEvents { .. } | Command::AckCoreInstructions { .. } | Command::MarkPublished { .. } | Command::AckListing { .. } => &["relay"],
            Command::DismissTrigger { .. } => &["documents"],
            Command::PlaceLegalHold { .. } | Command::ReleaseLegalHold { .. } => &["legal"],
            Command::ShredParty { .. } => &["privacy"],
            Command::RequestLetter(_) => &["underwriting", "reinsurance"],
            Command::CancelLetterRequest { .. } => &["underwriting", "reinsurance", "servicing"],
            Command::AckDeleted { .. } => &["retention"],
        }
    }
}
