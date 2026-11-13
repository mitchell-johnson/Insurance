//! Commands: the only way the module's state changes. Each carries an
//! idempotency key, who sent it, and the business date it was sent on. The
//! state machine never reads a clock.

use chrono::NaiveDate;
use ic_core::event::Event as CoreEvent;
use ic_core::{GroupId, LifeId, PartyId, PolicyId};
use ic_money::DecStr;
use ic_product::{Frequency, ProductDefinition};
use serde::{Deserialize, Serialize};

use crate::mapping::{FileKind, FileMapping, RowReject};
use crate::record::MemberRow;
use crate::terms::SchemeTerms;

/// The modules and channels that may send commands.
pub mod caller {
    /// Group administration staff.
    pub const STAFF: &str = "group-admin";
    /// The employer's administrators, through the portal or the payroll API.
    pub const EMPLOYER: &str = "employer-portal";
    /// A member, through the app, after a verified sign-in.
    pub const MEMBER: &str = "member-portal";
    /// The module's own driver: scans, matching, results from the core,
    /// the daily tick.
    pub const DRIVER: &str = "group-driver";
    pub const UNDERWRITING: &str = "underwriting";
    pub const REINSURANCE: &str = "reinsurance";
    pub const CASH: &str = "cash-matching";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A repeat returns the original result.
    pub key: String,
    /// The module or channel sending the command.
    pub caller: String,
    /// The person or service acting. Recorded, and compared where two
    /// people are required.
    pub actor: String,
    /// The business date.
    pub today: NaiveDate,
    pub command: GroupCommand,
}

/// How the customer master answered for one joiner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Resolution {
    /// Exactly one high-confidence match: the member is that person.
    Linked { party: PartyId },
    /// No candidate: a new person was created.
    Created { party: PartyId },
    /// One or more people the member might be. The member joins unlinked
    /// and the candidates go to a person to decide.
    Possible { candidates: Vec<PartyId> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ExcessDecision {
    Accepted {
        sum_insured_minor: i64,
        /// Underwriting terms. They pass through to the core's layer: this
        /// module holds them only until the core has accepted the layer, and
        /// never puts them in an event.
        #[serde(default)]
        loadings: Vec<DecStr>,
        #[serde(default)]
        exclusions: Vec<String>,
        /// The member agreed that the employer may see the loaded premium.
        #[serde(default)]
        member_consents_to_itemising: bool,
        /// The member has agreed to pay the loading themselves. On a case
        /// whose cover is already in place this is underwriting's reissue
        /// of the terms: the layer is replaced by one the member pays the
        /// loading of.
        #[serde(default)]
        member_pays_loading: bool,
    },
    /// Underwriting offered the member the loading to pay and the member
    /// declined, or the offer ran out. The terms in place stand.
    ReofferDeclined,
    Declined,
    /// The member did not complete underwriting.
    NotProceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum MatchDecision {
    /// The member is this existing person.
    SamePerson { party: PartyId },
    /// The member is nobody we knew: this person was created for them.
    DifferentPerson { party: PartyId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupCommand {
    // --- schemes ---
    RegisterScheme {
        scheme: String,
        policy: PolicyId,
        group: GroupId,
        employer: PartyId,
        inception: NaiveDate,
        /// The product version the scheme policy is on.
        product: Box<ProductDefinition>,
        terms: Box<SchemeTerms>,
        /// The members the policy was issued with.
        #[serde(default)]
        founding: Vec<crate::model::Member>,
        /// On a pool scheme, the names of the pools the policy was issued
        /// with (each is the life `pool:{name}` in the core).
        #[serde(default)]
        pools: Vec<String>,
    },
    AmendTerms {
        scheme: String,
        terms: Box<SchemeTerms>,
    },
    SetMapping {
        scheme: String,
        mapping: Box<FileMapping>,
    },
    /// Stop taking files while a mistake is put right.
    Freeze {
        scheme: String,
        reason: String,
    },
    Unfreeze {
        scheme: String,
    },
    /// Try again after the cause of a block has been dealt with.
    Resume {
        scheme: String,
    },

    // --- membership files ---
    ReceiveFile {
        scheme: String,
        file: String,
        content_hash: String,
    },
    RecordScan {
        scheme: String,
        file: String,
        clean: bool,
    },
    /// The file could not be read or mapped at all.
    RefuseFile {
        scheme: String,
        file: String,
        code: String,
    },
    /// Mapped rows, in parts so that no command is too large.
    SubmitRows {
        scheme: String,
        file: String,
        mapping_hash: String,
        kind: FileKind,
        /// The date the file speaks for.
        file_date: NaiveDate,
        rows: Vec<MemberRow>,
        rejected: Vec<RowReject>,
        last: bool,
    },
    /// A person confirms a file the safety check held.
    ConfirmHeld {
        scheme: String,
        file: String,
    },
    ApproveFile {
        scheme: String,
        file: String,
    },
    /// Staff decide the changes dated further back than the employer may.
    DecideBackdating {
        scheme: String,
        file: String,
        approve: bool,
    },
    WithdrawFile {
        scheme: String,
        file: String,
    },
    PartiesResolved {
        scheme: String,
        file: String,
        /// Employee ID and the customer master's answer.
        resolutions: Vec<(String, Resolution)>,
        last: bool,
    },
    /// Put back members a file removed in error, from the day they were
    /// removed, with no gap.
    Reinstate {
        scheme: String,
        employee_ids: Vec<String>,
        reason: String,
    },
    /// Ask underwriting again for a member's cover above the automatic
    /// amount, at the member's request: after cover they paid for lapsed,
    /// or after a decline once their circumstances changed. Underwriting
    /// decides, with its own rules on reinstating without evidence.
    RequestExcessAgain {
        scheme: String,
        employee_id: String,
        reason: String,
    },

    // --- people ---
    ResolveMatch {
        scheme: String,
        employee_id: String,
        decision: MatchDecision,
    },
    /// A member signed in with verified identity and claimed the membership.
    SelfRegister {
        scheme: String,
        employee_id: String,
        party: PartyId,
    },
    /// A member's choice from the scheme's health menu: a plan and who it
    /// covers, from a date.
    ChooseHealthPlan {
        scheme: String,
        employee_id: String,
        plan: String,
        tier: crate::terms::HealthTier,
        effective: NaiveDate,
    },

    // --- cover above the automatic amount ---
    RecordExcessDecision {
        scheme: String,
        case: u64,
        decision: ExcessDecision,
    },
    /// The reinsurance module needs part of a member's automatic cover
    /// underwritten: accumulation across the member's policies is over the
    /// retention.
    RequireUnderwrittenExcess {
        policy: PolicyId,
        life: LifeId,
        cover: String,
        excess_minor: i64,
        reference: String,
    },

    // --- pools ---
    DeclarePool {
        scheme: String,
        pool: String,
        category: String,
        effective: NaiveDate,
        headcount: u32,
        total_salary_minor: i64,
    },

    // --- leavers ---
    TakeUpContinuation {
        scheme: String,
        offer: u64,
        policy: PolicyId,
        number: String,
        group: GroupId,
        product_code: String,
        cover: String,
        frequency: Frequency,
        sum_insured_minor: i64,
    },

    // --- renewal and ending ---
    AgreeRenewal {
        scheme: String,
        /// The terms for the new scheme year.
        terms: Box<SchemeTerms>,
        /// The rate table version agreed, for the record.
        rate_table: String,
    },
    Terminate {
        scheme: String,
        effective: NaiveDate,
        reason: String,
    },

    // --- money ---
    RecordShortPayment {
        scheme: String,
        shortfall_minor: i64,
    },

    // --- from the driver ---
    /// What the core answered to a command this module sent.
    CoreResult {
        key: String,
        /// The core's reject code, if it refused.
        error: Option<String>,
    },
    /// One event from a partition's stream. The host keeps the cursor and
    /// fills gaps; the key is the event's ID, so a repeat changes nothing.
    ObserveCore {
        event: Box<CoreEvent>,
    },
    /// The commission module refused a scheme's broker for good: the entry
    /// leaves the outbox and staff are told.
    CommissionRefused {
        key: String,
        code: String,
    },
    /// The documents module refused a member's letter for good: the entry
    /// leaves the outbox and staff are told.
    LetterRefused {
        request: String,
        code: String,
    },
    /// Underwriting's outcome for the underwritten part of a continuation,
    /// read from its outbox.
    RecordContinuedExcess {
        scheme: String,
        offer: u64,
        outcome: String,
    },
    /// Outbox entries that were delivered.
    Ack {
        ids: Vec<u64>,
    },
    /// The business date has moved on.
    Tick,
}

impl GroupCommand {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        use caller::*;
        use GroupCommand::*;
        match self {
            RegisterScheme { .. }
            | AmendTerms { .. }
            | SetMapping { .. }
            | Freeze { .. }
            | Unfreeze { .. }
            | Resume { .. }
            | ConfirmHeld { .. }
            | DecideBackdating { .. }
            | Reinstate { .. }
            | RequestExcessAgain { .. }
            | ResolveMatch { .. }
            | AgreeRenewal { .. }
            | Terminate { .. } => &[STAFF],
            ReceiveFile { .. } | ApproveFile { .. } | WithdrawFile { .. } | DeclarePool { .. } => &[EMPLOYER, STAFF],
            RecordScan { .. }
            | RefuseFile { .. }
            | SubmitRows { .. }
            | PartiesResolved { .. }
            | CoreResult { .. }
            | CommissionRefused { .. }
            | LetterRefused { .. }
            | ObserveCore { .. }
            | Ack { .. }
            | Tick => &[DRIVER],
            SelfRegister { .. } | TakeUpContinuation { .. } => &[MEMBER],
            ChooseHealthPlan { .. } => &[MEMBER, EMPLOYER, STAFF],
            // The driver carries the underwriting module's answer from its outbox.
            RecordExcessDecision { .. } | RecordContinuedExcess { .. } => &[UNDERWRITING, DRIVER],
            RequireUnderwrittenExcess { .. } => &[REINSURANCE],
            RecordShortPayment { .. } => &[CASH],
        }
    }

    /// The command's name as it is written in JSON.
    pub fn name(&self) -> String {
        match serde_json::to_value(self).unwrap_or_default() {
            serde_json::Value::Object(fields) => fields.keys().next().cloned().unwrap_or_default(),
            other => other.as_str().unwrap_or_default().to_string(),
        }
    }
}
