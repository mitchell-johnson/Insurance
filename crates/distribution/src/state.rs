//! The distribution state machine: commands in, events and core postings out.
//!
//! State changes only by applying a logged [`Envelope`]. Nothing here reads a
//! clock: "today" is the latest business date seen on the core's events.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyPosting, PartyLedger};
use ic_core::{EventRecord, PartyId, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authority::{Ask, Authority, AuthorityError, DecisionRecord, Role, Staff};
use crate::config::{Change, ChangeSet, Config};
use crate::model::*;
use crate::rules::HeldReason;
use crate::statement::Statement;
use crate::structure::{CeasedReason, Engagement, LicenceRecord, LicenceState, PartyKind, Relation, RelationKind, StructureError};
use crate::workbench::{Action, ActionRecord, ControlSnapshot, DocumentRecord, PositionNote, RecoveryStatus};
use crate::{BookId, EngagementId};

/// Who sent a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// A person, as the gateway's internal token describes them.
    Staff(Box<Staff>),
    /// Another module or a driver of this one, by name.
    Module(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A duplicate returns the original result.
    pub key: String,
    pub actor: Actor,
    pub command: Command,
}

/// How a layer is written: who advised, which books, on what terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteLayer {
    pub policy: PolicyId,
    pub cover: String,
    pub layer: u32,
    pub writing: Vec<(EngagementId, u32)>,
    pub books: Vec<(BookId, u32)>,
    pub schedule: String,
    pub option: String,
    pub annual_premium_minor: i64,
    /// The layer replaces cover the same insurer already had on the life: it
    /// earns trail only, and the original exposure clock is not reset.
    #[serde(default)]
    pub replacement: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    // --- the core's events, relayed in order per partition ---
    Ingest(Box<EventRecord>),
    /// The core refused a posting this module asked for.
    PostingRefused {
        reference: String,
        code: String,
        message: String,
    },

    // --- structure ---
    AddParty {
        party: PartyId,
        kind: PartyKind,
    },
    Relate(Relation),
    EndRelation {
        child: PartyId,
        parent: PartyId,
        kind: RelationKind,
        on: NaiveDate,
    },
    StartEngagement {
        id: EngagementId,
        engagement: Engagement,
    },
    EndEngagement {
        id: EngagementId,
        on: NaiveDate,
    },
    /// A licence-status event from the regulator's register or the insurer's own records.
    RecordLicence {
        party: PartyId,
        state: LicenceState,
        effective: NaiveDate,
        source_ref: String,
    },
    MarkCeased {
        party: PartyId,
        on: NaiveDate,
        reason: CeasedReason,
    },

    // --- payees ---
    RegisterPayee {
        party: PartyId,
        agreement_ref: String,
        advance_recovery_ppm: u32,
    },
    AddPayeeAccount {
        party: PartyId,
        account_ref: String,
        name_check_ref: String,
        step_up_ref: String,
        appointment_ref: Option<String>,
    },
    SetPaymentHold {
        party: PartyId,
        reason: Option<String>,
    },
    SetAccrualHold {
        party: PartyId,
        hold: Option<(NaiveDate, HeldReason, String)>,
    },
    AddPaymentInstruction {
        party: PartyId,
        instruction: PaymentInstruction,
    },
    RemovePaymentInstruction {
        party: PartyId,
        id: String,
        consent_ref: String,
    },

    // --- policies ---
    WriteLayer(Box<WriteLayer>),
    /// The customer appoints or removes their servicing adviser. Access follows the customer.
    AppointAdviser {
        policy: PolicyId,
        engagement: Option<EngagementId>,
        book: Option<BookId>,
    },
    LinkReplacement {
        policy: PolicyId,
        replaces: PolicyId,
    },

    // --- configuration: proposed, approved twice, then active ---
    ProposeChange {
        id: String,
        changes: Vec<Change>,
        instructions_ref: String,
    },
    ApproveChange {
        id: String,
        compliance: bool,
    },
    RejectChange {
        id: String,
        reason: String,
    },

    // --- statements and payment ---
    CutStatement {
        payee: PartyId,
    },
    PaymentSettled {
        reference: String,
    },
    PaymentReturned {
        reference: String,
        reason: String,
    },

    // --- the held balance workbench ---
    ProposeAction {
        id: String,
        party: PartyId,
        action: Action,
        reason_code: String,
        document_ref: String,
    },
    ApproveAction {
        id: String,
        as_role: Role,
    },
    RejectAction {
        id: String,
        reason: String,
    },
    AttachDocument {
        party: PartyId,
        kind: String,
        document_ref: String,
    },
    /// Another module says what a receivable it posted is.
    DescribePosition {
        party: PartyId,
        reference: String,
        note: PositionNote,
    },
    RecordDailyControl,
    CompleteTask {
        id: String,
        note: String,
    },
    CloseCase {
        id: String,
        outcome: String,
    },
}

impl Command {
    /// The command's name as it appears on the wire.
    pub fn name(&self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(fields)) => fields.keys().next().cloned().unwrap_or_default(),
            Ok(serde_json::Value::String(name)) => name,
            _ => String::new(),
        }
    }

    /// The modules that may send a command no person sends.
    fn modules(&self) -> &'static [&'static str] {
        match self {
            Command::Ingest(_) | Command::PostingRefused { .. } => &["event-relay"],
            Command::WriteLayer(_) | Command::LinkReplacement { .. } => &["new-business", "policy-servicing", "migration"],
            Command::AppointAdviser { .. } => &["customer-app", "policy-servicing"],
            Command::PaymentSettled { .. } | Command::PaymentReturned { .. } => &["payments"],
            Command::DescribePosition { .. } => &["claims", "reinsurance", "group-admin", "collections", "payments"],
            Command::RecordDailyControl => &["end-of-day"],
            Command::RecordLicence { .. } => &["regulator-register"],
            Command::AddParty { .. } | Command::Relate(_) | Command::StartEngagement { .. } => &["migration", "customer-master"],
            _ => &[],
        }
    }

    /// The staff action a person needs for it, and the record it concerns.
    fn staff_action(&self) -> Option<(&'static str, String)> {
        let any = || "*".to_string();
        Some(match self {
            Command::AddParty { .. }
            | Command::Relate(_)
            | Command::EndRelation { .. }
            | Command::StartEngagement { .. }
            | Command::EndEngagement { .. }
            | Command::MarkCeased { .. } => ("structure.manage", any()),
            Command::RecordLicence { .. } => ("licence.record", any()),
            Command::RegisterPayee { party, .. }
            | Command::AddPayeeAccount { party, .. }
            | Command::SetPaymentHold { party, .. }
            | Command::SetAccrualHold { party, .. }
            | Command::RemovePaymentInstruction { party, .. } => ("payee.manage", party.0.clone()),
            Command::AddPaymentInstruction { party, instruction } => match instruction.kind {
                InstructionKind::Statutory => ("payee.restricted_instruction", party.0.clone()),
                InstructionKind::Assignment => ("payee.manage", party.0.clone()),
            },
            Command::WriteLayer(_) | Command::LinkReplacement { .. } => ("policy.assign", any()),
            Command::ProposeChange { .. } | Command::RejectChange { .. } => ("config.propose", any()),
            Command::CutStatement { payee } => ("statement.cut", payee.0.clone()),
            Command::AttachDocument { party, .. } => ("workbench.document", party.0.clone()),
            Command::CompleteTask { .. } => ("workbench.view", any()),
            Command::CloseCase { .. } => ("case.manage", any()),
            // Checked where they are handled, because what they need depends on state.
            Command::ApproveChange { .. } | Command::ProposeAction { .. } | Command::ApproveAction { .. } | Command::RejectAction { .. } => return None,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseKind {
    /// Opened by a licence event, owned by the conduct officer.
    Conduct,
    Dispute,
    /// A debt aged past the threshold, needing a repayment arrangement.
    Debt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub kind: CaseKind,
    pub owner: Role,
    pub subject: PartyId,
    pub opened: NaiveDate,
    pub source_ref: String,
    /// The affected population.
    pub engagements: BTreeSet<EngagementId>,
    pub payees: BTreeSet<PartyId>,
    pub policies: BTreeSet<PolicyId>,
    /// What the case drives: communication, file review, reporting.
    pub workstreams: Vec<String>,
    pub closed: Option<(NaiveDate, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub subject: String,
    pub due: NaiveDate,
    pub detail: String,
    pub done: Option<String>,
}

/// Past-tense facts for other modules, the portals and the data mart. Opaque
/// IDs only, never personal data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    PartyAdded {
        party: PartyId,
        kind: PartyKind,
    },
    StructureChanged {
        child: PartyId,
        parent: PartyId,
    },
    EngagementStarted {
        id: EngagementId,
        adviser: PartyId,
        with: PartyId,
        from: NaiveDate,
    },
    EngagementEnded {
        id: EngagementId,
        on: NaiveDate,
    },
    /// A licence-status event and what it cascaded to. Portals drop the
    /// engagements to read-only; the app shows the adviser as unavailable.
    AuthorisationChanged {
        party: PartyId,
        state: LicenceState,
        effective: NaiveDate,
        engagements: Vec<EngagementId>,
        suspended_payees: Vec<PartyId>,
        policies_to_service_directly: u32,
    },
    PartyCeased {
        party: PartyId,
        on: NaiveDate,
        reason: CeasedReason,
    },
    CaseOpened {
        case: String,
        kind: CaseKind,
        subject: PartyId,
    },
    CaseClosed {
        case: String,
    },
    TaskCreated {
        task: String,
        kind: String,
        due: NaiveDate,
    },
    PayeeChanged {
        party: PartyId,
        what: String,
    },
    LayerAssigned {
        policy: PolicyId,
        layer: String,
        books: Vec<(BookId, u32)>,
        from: NaiveDate,
    },
    /// Customers are told when their servicing adviser changes.
    ServicingChanged {
        policy: PolicyId,
        from: Option<EngagementId>,
        to: Option<EngagementId>,
        on: NaiveDate,
    },
    ChangeProposed {
        id: String,
        hash: String,
    },
    ChangeApproved {
        id: String,
        by: PartyId,
        compliance: bool,
    },
    ChangeActivated {
        id: String,
        hash: String,
    },
    ChangeRejected {
        id: String,
    },
    BookCreated {
        book: BookId,
    },
    RuleActivated {
        book: BookId,
        version: u32,
        hash: String,
    },
    CommissionEarned {
        reference: String,
        policy: PolicyId,
        total_minor: i64,
    },
    CommissionReversed {
        reference: String,
        reverses: String,
    },
    ClawbackRaised {
        reference: String,
        policy: PolicyId,
        total_minor: i64,
    },
    AmountHeld {
        party: PartyId,
        amount_minor: i64,
        reason: Option<HeldReason>,
        reference: String,
    },
    AmountReleased {
        party: PartyId,
        amount_minor: i64,
        reference: String,
    },
    /// Commission could not be worked out. Becomes an exception item.
    CommissionException {
        policy: PolicyId,
        cause: String,
        message: String,
    },
    StatementIssued {
        payee: PartyId,
        number: u32,
        closing_minor: i64,
        hash: String,
    },
    /// For the payments module: pay this amount to this account reference.
    PaymentRequested {
        reference: String,
        party: PartyId,
        account_ref: String,
        amount_minor: i64,
        currency: ic_money::Currency,
    },
    PaymentCompleted {
        reference: String,
    },
    PaymentFailed {
        reference: String,
        reason: String,
    },
    ActionProposed {
        id: String,
        party: PartyId,
        kind: String,
        amount_minor: i64,
    },
    ActionApproved {
        id: String,
        by: PartyId,
        role: Role,
    },
    ActionRejected {
        id: String,
    },
    ActionCompleted {
        id: String,
    },
    DocumentAttached {
        party: PartyId,
        kind: String,
    },
    ControlRecorded {
        date: NaiveDate,
        held_total_minor: i64,
        parties: u32,
    },
    PostingRefused {
        reference: String,
        code: String,
    },
}

/// What applying one command produced. The host writes the events and the
/// postings to its outbox in the same atomic step as the state; a driver
/// sends each posting to the core with its reference as the idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Output {
    pub index: u64,
    pub result: Result<(), Reject>,
    pub events: Vec<Event>,
    pub postings: Vec<(PartitionId, CounterpartyPosting)>,
    pub duplicate: bool,
}

/// Working context for one command.
pub(crate) struct Ctx {
    pub events: Vec<Event>,
    pub postings: Vec<(PartitionId, CounterpartyPosting)>,
    /// What the staff decision relied on, where a person sent the command.
    pub decision: Option<DecisionRecord>,
}

impl Ctx {
    pub fn emit(&mut self, e: Event) {
        self.events.push(e);
    }
}

pub(crate) fn reject<T>(code: &str, message: impl Into<String>) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

impl From<StructureError> for RejectFrom {
    fn from(e: StructureError) -> Self {
        RejectFrom(Reject::new("structure", e.to_string()))
    }
}
/// Local wrapper so foreign errors convert to the core's refusal type.
pub(crate) struct RejectFrom(pub Reject);

pub(crate) fn structure<T>(r: Result<T, StructureError>) -> Result<T, Reject> {
    r.map_err(|e| RejectFrom::from(e).0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredResult {
    index: u64,
    error: Option<Reject>,
}

/// Business settings of the module. Changed by configuration, not by code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Days between a payee account being added and the first payment to it.
    pub account_cooling_off_days: u32,
    /// A rule may be activated this far into the past and no further.
    pub max_backdate_days: u32,
    /// A workbench action at or above this needs two approvers and legal.
    pub two_approver_threshold_minor: i64,
    /// A write-off at or above this needs a second approver.
    pub write_off_second_approver_minor: i64,
    /// A held balance may be written off below this floor or beyond this age.
    pub write_off_floor_minor: i64,
    pub write_off_age_days: u32,
    /// A debit balance older than this becomes a case.
    pub debt_case_after_days: u32,
    /// Months a book is watched for replacement patterns after a move.
    pub watch_months_after_move: u32,
    /// Month-end: a party's held balance older than these days and above
    /// this amount blocks sign-off.
    pub month_end_held_age_days: u32,
    pub month_end_held_threshold_minor: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            account_cooling_off_days: 5,
            max_backdate_days: 365,
            two_approver_threshold_minor: 100_000_00,
            write_off_second_approver_minor: 5_000_00,
            write_off_floor_minor: 50_00,
            write_off_age_days: 730,
            debt_case_after_days: 90,
            watch_months_after_move: 12,
            month_end_held_age_days: 90,
            month_end_held_threshold_minor: 10_000_00,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionState {
    pub settings: Settings,
    /// The latest business date seen on the core's events.
    pub today: Option<NaiveDate>,
    /// The module's own sequence: one per applied command.
    pub seq: u64,
    /// Structure, schedules, fragments and books: what change sets activate.
    pub config: Config,
    pub payees: BTreeMap<PartyId, Payee>,
    pub policies: BTreeMap<PolicyId, PolicyRecord>,
    pub layers: BTreeMap<String, LayerRecord>,
    pub exposures: BTreeMap<String, Exposure>,
    pub allocations: BTreeMap<String, Allocation>,
    pub(crate) allocations_by_date: BTreeMap<NaiveDate, Vec<String>>,
    /// `policy/receipt` to the allocation it earned.
    pub(crate) receipts: BTreeMap<String, String>,
    /// Everything asked of the core, by reference.
    pub requested: BTreeMap<String, Requested>,
    pub movements: Vec<Movement>,
    /// Party, partition, account: positive is in the party's favour.
    pub positions: BTreeMap<PartyId, BTreeMap<PartitionId, BTreeMap<PartyLedger, i64>>>,
    pub held: Vec<HeldItem>,
    pub change_sets: BTreeMap<String, ChangeSet>,
    pub statements: BTreeMap<String, Statement>,
    /// Payments asked of the payments module and not yet settled or returned.
    pub(crate) payments: BTreeMap<String, crate::statement::PaymentRecord>,
    pub actions: BTreeMap<String, ActionRecord>,
    pub documents: BTreeMap<PartyId, Vec<DocumentRecord>>,
    pub notes: BTreeMap<String, (PartyId, PositionNote)>,
    pub controls: Vec<ControlSnapshot>,
    pub cases: BTreeMap<String, Case>,
    pub tasks: BTreeMap<String, Task>,
    /// The last event sequence taken from each partition.
    pub checkpoints: BTreeMap<PartitionId, u64>,
    /// The module sequence at which each party last changed: its position,
    /// its actions, its documents, its licence or its payee record. What a
    /// publisher reads to send on only what moved.
    #[serde(default)]
    pub changed: BTreeMap<PartyId, u64>,
    idempotency: BTreeMap<String, StoredResult>,
}

impl Default for DistributionState {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

thread_local! {
    // The shipped policy text is part of the binary, so every replica running
    // the same build evaluates the same policy. Parsed once per thread.
    static AUTHORITY: Result<Authority, AuthorityError> = Authority::shipped();
}

impl DistributionState {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            today: None,
            seq: 0,
            config: Config::default(),
            payees: BTreeMap::new(),
            policies: BTreeMap::new(),
            layers: BTreeMap::new(),
            exposures: BTreeMap::new(),
            allocations: BTreeMap::new(),
            allocations_by_date: BTreeMap::new(),
            receipts: BTreeMap::new(),
            requested: BTreeMap::new(),
            movements: vec![],
            positions: BTreeMap::new(),
            held: vec![],
            change_sets: BTreeMap::new(),
            statements: BTreeMap::new(),
            payments: BTreeMap::new(),
            actions: BTreeMap::new(),
            documents: BTreeMap::new(),
            notes: BTreeMap::new(),
            controls: vec![],
            cases: BTreeMap::new(),
            tasks: BTreeMap::new(),
            checkpoints: BTreeMap::new(),
            changed: BTreeMap::new(),
            idempotency: BTreeMap::new(),
        }
    }

    /// Hash of the whole replicated state. Two replicas that applied the same
    /// log must agree on it exactly.
    pub fn state_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"ic.distribution.state.v1");
        // Every map in the state has string or integer keys and every
        // collection is ordered, so the JSON form is canonical.
        h.update(serde_json::to_vec(self).unwrap_or_else(|_| b"unencodable".to_vec()));
        hex::encode(h.finalize())
    }

    pub(crate) fn date(&self) -> Result<NaiveDate, Reject> {
        self.today
            .ok_or_else(|| Reject::new("business_date", "no business date has been seen from the core yet"))
    }

    /// Ask the four questions of a person for an action on a record.
    pub(crate) fn authorise(&self, staff: &Staff, ask: &Ask<'_>) -> Result<DecisionRecord, Reject> {
        let day = self.date()?;
        // Someone covering for a colleague inherits that colleague's paths.
        let covered: BTreeSet<String> = staff
            .grants
            .iter()
            .filter(|g| g.active(day))
            .filter_map(|g| match &g.kind {
                crate::authority::GrantKind::Cover { of } => Some(of),
                _ => None,
            })
            .flat_map(|of| self.config.staff_paths.get(of).into_iter().flatten().cloned())
            .collect();
        AUTHORITY.with(|a| match a {
            Ok(a) => a.decide(staff, ask, day, &covered).map_err(|e| match e {
                AuthorityError::Denied(why) => Reject::new("not_authorised", why),
                other => Reject::new("authority", other.to_string()),
            }),
            Err(e) => Err(Reject::new("authority", e.to_string())),
        })
    }

    pub(crate) fn task(&mut self, ctx: &mut Ctx, kind: &str, subject: &str, due: NaiveDate, detail: String) {
        let id = format!("task-{}-{}", self.seq, self.tasks.len() + 1);
        ctx.emit(Event::TaskCreated {
            task: id.clone(),
            kind: kind.to_string(),
            due,
        });
        self.tasks.insert(
            id.clone(),
            Task {
                id,
                kind: kind.to_string(),
                subject: subject.to_string(),
                due,
                detail,
                done: None,
            },
        );
    }

    /// Record a posting to ask of the core, and put it in the outbox.
    pub(crate) fn request(&mut self, ctx: &mut Ctx, partition: PartitionId, posting: CounterpartyPosting, owner: Option<String>) {
        self.requested.insert(
            posting.reference.clone(),
            Requested {
                partition,
                posting: posting.clone(),
                state: PostingState::Pending,
                owner,
            },
        );
        ctx.postings.push((partition, posting));
    }

    /// Postings asked of the core and not yet confirmed: what a driver that
    /// lost its outbox sends again.
    pub fn unconfirmed(&self) -> Vec<(PartitionId, CounterpartyPosting)> {
        self.requested
            .values()
            .filter(|r| r.state == PostingState::Pending)
            .map(|r| (r.partition, r.posting.clone()))
            .collect()
    }

    fn check_actor(&self, env: &Envelope, ctx: &mut Ctx) -> Result<(), Reject> {
        match &env.actor {
            Actor::Module(name) => {
                if env.command.modules().contains(&name.as_str()) {
                    Ok(())
                } else {
                    reject("caller_not_allowed", format!("module {name} may not send {}", env.command.name()))
                }
            }
            Actor::Staff(staff) => {
                if !env.command.modules().is_empty() && env.command.staff_action().is_none() {
                    return reject("caller_not_allowed", format!("{} is sent by a module, not a person", env.command.name()));
                }
                if let Some((action, record)) = env.command.staff_action() {
                    ctx.decision = Some(self.authorise(staff, &Ask::new(action, &record))?);
                }
                Ok(())
            }
        }
    }

    fn dispatch(&mut self, ctx: &mut Ctx, actor: &Actor, command: Command) -> Result<(), Reject> {
        match command {
            Command::Ingest(record) => self.ingest(ctx, *record),
            Command::PostingRefused { reference, code, message } => self.posting_refused(ctx, &reference, code, message),

            Command::AddParty { party, kind } => {
                structure(self.config.structure.add_party(party.clone(), kind))?;
                ctx.emit(Event::PartyAdded { party, kind });
                Ok(())
            }
            Command::Relate(relation) => {
                let (child, parent) = (relation.child.clone(), relation.parent.clone());
                structure(self.config.structure.relate(relation))?;
                ctx.emit(Event::StructureChanged { child, parent });
                Ok(())
            }
            Command::EndRelation { child, parent, kind, on } => {
                structure(self.config.structure.end_relation(&child, &parent, kind, on))?;
                ctx.emit(Event::StructureChanged { child, parent });
                Ok(())
            }
            Command::StartEngagement { id, engagement } => {
                let e = Event::EngagementStarted {
                    id: id.clone(),
                    adviser: engagement.adviser.clone(),
                    with: engagement.with.clone(),
                    from: engagement.from,
                };
                structure(self.config.structure.start_engagement(id, engagement))?;
                ctx.emit(e);
                Ok(())
            }
            Command::EndEngagement { id, on } => {
                structure(self.config.structure.end_engagement(&id, on))?;
                ctx.emit(Event::EngagementEnded { id, on });
                Ok(())
            }
            Command::RecordLicence {
                party,
                state,
                effective,
                source_ref,
            } => self.record_licence(ctx, party, LicenceRecord { state, effective, source_ref }),
            Command::MarkCeased { party, on, reason } => {
                structure(self.config.structure.mark_ceased(&party, on, reason))?;
                ctx.emit(Event::PartyCeased { party, on, reason });
                Ok(())
            }

            Command::RegisterPayee {
                party,
                agreement_ref,
                advance_recovery_ppm,
            } => {
                if !self.config.structure.parties.contains_key(&party) {
                    return reject("unknown_party", format!("{party} is not in the structure"));
                }
                if agreement_ref.trim().is_empty() || advance_recovery_ppm > crate::rules::WHOLE {
                    return reject(
                        "invalid_payee",
                        "a payee has a place in an agreement, and a recovery share no more than the whole",
                    );
                }
                let p = self.payees.entry(party.clone()).or_default();
                p.agreement_ref = Some(agreement_ref);
                p.advance_recovery_ppm = advance_recovery_ppm;
                ctx.emit(Event::PayeeChanged {
                    party,
                    what: "registered".into(),
                });
                Ok(())
            }
            Command::AddPayeeAccount {
                party,
                account_ref,
                name_check_ref,
                step_up_ref,
                appointment_ref,
            } => {
                let today = self.date()?;
                let cooling = self.settings.account_cooling_off_days;
                let payee = self.payee_mut(&party)?;
                if account_ref.trim().is_empty() || name_check_ref.trim().is_empty() || step_up_ref.trim().is_empty() {
                    return reject(
                        "invalid_account",
                        "a payee account needs its reference, the payee name check and the signed step-up",
                    );
                }
                if payee.accounts.iter().any(|a| a.account_ref == account_ref) {
                    return reject("duplicate_account", "that account is already on the payee");
                }
                // Every account waits out the cooling-off before its first
                // payment, a liquidator's or an executor's included:
                // redirected commission is a fraud target.
                let usable_from = today.checked_add_days(chrono::Days::new(cooling as u64)).unwrap_or(today);
                payee.accounts.push(PayeeAccount {
                    account_ref,
                    name_check_ref,
                    step_up_ref,
                    usable_from,
                    appointment_ref,
                    added_on: today,
                });
                ctx.emit(Event::PayeeChanged {
                    party,
                    what: "account_added".into(),
                });
                Ok(())
            }
            Command::SetPaymentHold { party, reason } => {
                if reason.as_ref().is_some_and(|r| r.trim().is_empty()) {
                    return reject("invalid_hold", "a payment hold records why");
                }
                self.payee_mut(&party)?.payment_hold = reason;
                ctx.emit(Event::PayeeChanged {
                    party,
                    what: "payment_hold".into(),
                });
                Ok(())
            }
            Command::SetAccrualHold { party, hold } => {
                if let Some((_, reason, reference)) = &hold {
                    if !matches!(reason, HeldReason::MissingPaymentDetails | HeldReason::LegalHold) || reference.trim().is_empty() {
                        return reject(
                            "invalid_hold",
                            "an accrual hold is for missing payment details or a legal hold, with a reference",
                        );
                    }
                }
                self.payee_mut(&party)?.accrual_hold = hold;
                ctx.emit(Event::PayeeChanged {
                    party,
                    what: "accrual_hold".into(),
                });
                Ok(())
            }
            Command::AddPaymentInstruction { party, instruction } => {
                let payee = self.payee_mut(&party)?;
                let sized = matches!((instruction.amount_minor, instruction.share_ppm), (Some(a), None) if a > 0)
                    || matches!((instruction.amount_minor, instruction.share_ppm), (None, Some(s)) if s > 0 && s <= crate::rules::WHOLE);
                if !sized || instruction.document_ref.trim().is_empty() || instruction.to_account_ref.trim().is_empty() {
                    return reject(
                        "invalid_instruction",
                        "an instruction pays a set amount or a share to a named account, with its document",
                    );
                }
                if payee.instructions.iter().any(|i| i.id == instruction.id) {
                    return reject("duplicate_instruction", "that instruction is already on the payee");
                }
                payee.instructions.push(instruction);
                // Legal priority first, then in the order they were given.
                payee.instructions.sort_by_key(|i| i.kind != InstructionKind::Statutory);
                ctx.emit(Event::PayeeChanged {
                    party,
                    what: "instruction_added".into(),
                });
                Ok(())
            }
            Command::RemovePaymentInstruction { party, id, consent_ref } => {
                let payee = self.payee_mut(&party)?;
                let Some(at) = payee.instructions.iter().position(|i| i.id == id) else {
                    return reject("unknown_instruction", "no such instruction on the payee");
                };
                if payee.instructions[at].kind == InstructionKind::Statutory {
                    return reject(
                        "restricted_instruction",
                        "an instruction with legal priority ends on its own terms, not by removal",
                    );
                }
                if consent_ref.trim().is_empty() {
                    return reject("consent_required", "an assignment is removed only with the lender's recorded consent");
                }
                payee.instructions.remove(at);
                ctx.emit(Event::PayeeChanged {
                    party,
                    what: "instruction_removed".into(),
                });
                Ok(())
            }

            Command::WriteLayer(w) => self.write_layer(ctx, *w),
            Command::AppointAdviser { policy, engagement, book } => self.appoint_adviser(ctx, policy, engagement, book),
            Command::LinkReplacement { policy, replaces } => {
                if !self.policies.contains_key(&replaces) {
                    return reject("unknown_policy", format!("policy {replaces} is not known to distribution"));
                }
                let p = self
                    .policies
                    .get_mut(&policy)
                    .ok_or_else(|| Reject::new("unknown_policy", format!("policy {policy} is not known to distribution")))?;
                p.replaces = Some(replaces);
                Ok(())
            }

            Command::ProposeChange { id, changes, instructions_ref } => self.propose_change(ctx, actor, id, changes, instructions_ref),
            Command::ApproveChange { id, compliance } => self.approve_change(ctx, actor, &id, compliance),
            Command::RejectChange { id, reason } => self.reject_change(ctx, &id, reason),

            Command::CutStatement { payee } => self.cut_statement(ctx, &payee),
            Command::PaymentSettled { reference } => self.payment_settled(ctx, &reference),
            Command::PaymentReturned { reference, reason } => self.payment_returned(ctx, &reference, reason),

            Command::ProposeAction {
                id,
                party,
                action,
                reason_code,
                document_ref,
            } => self.propose_action(ctx, actor, id, party, action, reason_code, document_ref),
            Command::ApproveAction { id, as_role } => self.approve_action(ctx, actor, &id, as_role),
            Command::RejectAction { id, reason } => self.reject_action(ctx, actor, &id, reason),
            Command::AttachDocument { party, kind, document_ref } => {
                if kind.trim().is_empty() || document_ref.trim().is_empty() {
                    return reject("invalid_document", "a document has a kind and a reference in the vault");
                }
                let on = self.date()?;
                self.documents.entry(party.clone()).or_default().push(DocumentRecord {
                    kind: kind.clone(),
                    document_ref,
                    on,
                });
                ctx.emit(Event::DocumentAttached { party, kind });
                Ok(())
            }
            Command::DescribePosition { party, reference, note } => {
                if let Some((_, existing)) = self.notes.get(&reference) {
                    if existing.recovery != note.recovery {
                        return reject("recovery_status", "a recovery status changes through the workbench, with its approvals");
                    }
                }
                if note.recovery != RecoveryStatus::Held && !self.notes.contains_key(&reference) {
                    return reject("recovery_status", "an overpayment is raised as held; moving it on is a workbench decision");
                }
                self.notes.insert(reference, (party, note));
                Ok(())
            }
            Command::RecordDailyControl => self.record_daily_control(ctx),
            Command::CompleteTask { id, note } => {
                let t = self
                    .tasks
                    .get_mut(&id)
                    .ok_or_else(|| Reject::new("unknown_task", format!("task {id} is not known")))?;
                if t.done.is_some() {
                    return reject("task_done", "the task is already complete");
                }
                t.done = Some(note);
                Ok(())
            }
            Command::CloseCase { id, outcome } => {
                let on = self.date()?;
                let c = self
                    .cases
                    .get_mut(&id)
                    .ok_or_else(|| Reject::new("unknown_case", format!("case {id} is not known")))?;
                if c.closed.is_some() || outcome.trim().is_empty() {
                    return reject("invalid_case", "a case closes once, with its outcome");
                }
                c.closed = Some((on, outcome));
                ctx.emit(Event::CaseClosed { case: id });
                Ok(())
            }
        }
    }

    fn payee_mut(&mut self, party: &PartyId) -> Result<&mut Payee, Reject> {
        self.payees
            .get_mut(party)
            .ok_or_else(|| Reject::new("unknown_payee", format!("{party} is not a registered payee")))
    }

    /// A licence-status event. Revocation, suspension and lapse cascade
    /// through the dated structure from the effective date: engagements that
    /// depend on the licence can no longer advise, payees under it accrue to
    /// held, anything they were paid since the effective date is moved to
    /// held, and a conduct case opens with the affected population attached.
    fn record_licence(&mut self, ctx: &mut Ctx, party: PartyId, record: LicenceRecord) -> Result<(), Reject> {
        let today = self.date()?;
        if record.effective > today {
            return reject("future_licence_event", "a licence status is recorded once it is in effect");
        }
        if self.config.structure.own_licence(&party, record.effective).is_some_and(|r| *r == record) {
            return Ok(());
        }
        structure(self.config.structure.record_licence(&party, record.clone()))?;
        let (engagements, payees) = self.config.structure.affected_by(&party, today);
        let mut policies = BTreeSet::new();
        if record.state != LicenceState::Active {
            for (id, p) in &self.policies {
                if p.servicing.as_ref().is_some_and(|e| engagements.contains(e)) {
                    policies.insert(id.clone());
                }
            }
            self.move_paid_to_held(ctx, record.effective);
            let case = format!("conduct-{}-{}", party, record.effective);
            if !self.cases.contains_key(&case) {
                self.cases.insert(
                    case.clone(),
                    Case {
                        id: case.clone(),
                        kind: CaseKind::Conduct,
                        owner: Role::ConductOfficer,
                        subject: party.clone(),
                        opened: today,
                        source_ref: record.source_ref.clone(),
                        engagements: engagements.clone(),
                        payees: payees.clone(),
                        policies: policies.clone(),
                        workstreams: vec![
                            "customer_communication".into(),
                            "advice_file_sample_review".into(),
                            "regulatory_reporting".into(),
                        ],
                        closed: None,
                    },
                );
                ctx.emit(Event::CaseOpened {
                    case,
                    kind: CaseKind::Conduct,
                    subject: party.clone(),
                });
            }
        }
        ctx.emit(Event::AuthorisationChanged {
            party,
            state: record.state,
            effective: record.effective,
            engagements: engagements.into_iter().collect(),
            suspended_payees: payees.into_iter().collect(),
            policies_to_service_directly: policies.len() as u32,
        });
        Ok(())
    }

    fn posting_refused(&mut self, ctx: &mut Ctx, reference: &str, code: String, message: String) -> Result<(), Reject> {
        let today = self.date()?;
        let r = self
            .requested
            .get_mut(reference)
            .ok_or_else(|| Reject::new("unknown_reference", format!("{reference} was not asked of the core")))?;
        if r.state != PostingState::Pending {
            return reject("posting_settled", "the posting has already been confirmed or refused");
        }
        r.state = PostingState::Rejected {
            code: code.clone(),
            message: message.clone(),
        };
        let owner = r.owner.clone();
        if let Some(a) = owner.as_ref().and_then(|o| self.actions.get_mut(o)) {
            a.failed = Some(format!("{code}: {message}"));
        }
        self.task(ctx, "posting_refused", reference, today, format!("{code}: {message}"));
        ctx.emit(Event::PostingRefused {
            reference: reference.to_string(),
            code,
        });
        Ok(())
    }

    /// The parties an event is about, for the change cursor.
    fn parties_of(&self, event: &Event) -> Vec<PartyId> {
        let of_action = |id: &String| self.actions.get(id).map(|a| a.party.clone()).into_iter().collect();
        match event {
            Event::PartyAdded { party, .. }
            | Event::PartyCeased { party, .. }
            | Event::PayeeChanged { party, .. }
            | Event::AmountHeld { party, .. }
            | Event::AmountReleased { party, .. }
            | Event::PaymentRequested { party, .. }
            | Event::ActionProposed { party, .. }
            | Event::DocumentAttached { party, .. } => vec![party.clone()],
            Event::StatementIssued { payee, .. } => vec![payee.clone()],
            Event::CaseOpened { subject, .. } => vec![subject.clone()],
            Event::StructureChanged { child, parent } => vec![child.clone(), parent.clone()],
            Event::EngagementStarted { adviser, with, .. } => vec![adviser.clone(), with.clone()],
            Event::EngagementEnded { id, .. } => self
                .config
                .structure
                .engagements
                .get(id)
                .map(|e| vec![e.adviser.clone(), e.with.clone()])
                .unwrap_or_default(),
            Event::AuthorisationChanged {
                party,
                engagements,
                suspended_payees,
                ..
            } => {
                let advisers = engagements
                    .iter()
                    .filter_map(|id| self.config.structure.engagements.get(id))
                    .map(|e| e.adviser.clone());
                std::iter::once(party.clone()).chain(suspended_payees.iter().cloned()).chain(advisers).collect()
            }
            Event::ActionApproved { id, .. } | Event::ActionRejected { id } | Event::ActionCompleted { id } => of_action(id),
            _ => vec![],
        }
    }

    /// Apply one logged command.
    pub fn apply_envelope(&mut self, index: u64, env: Envelope) -> Output {
        if let Some(stored) = self.idempotency.get(&env.key) {
            return Output {
                index: stored.index,
                result: stored.error.clone().map_or(Ok(()), Err),
                events: vec![],
                postings: vec![],
                duplicate: true,
            };
        }
        self.seq += 1;
        let mut ctx = Ctx {
            events: vec![],
            postings: vec![],
            decision: None,
        };
        let actor = env.actor.clone();
        // Handlers check everything before they write, so a rejected
        // command leaves no trace but its key.
        let result = self.check_actor(&env, &mut ctx).and_then(|()| self.dispatch(&mut ctx, &actor, env.command));
        if result.is_err() {
            ctx.events.clear();
            ctx.postings.clear();
        }
        let touched: Vec<PartyId> = ctx.events.iter().flat_map(|e| self.parties_of(e)).collect();
        for party in touched {
            self.changed.insert(party, self.seq);
        }
        self.idempotency.insert(
            env.key,
            StoredResult {
                index,
                error: result.clone().err(),
            },
        );
        Output {
            index,
            result,
            events: ctx.events,
            postings: ctx.postings,
            duplicate: false,
        }
    }
}

impl StateMachine for DistributionState {
    type Command = Envelope;
    type Output = Output;
    fn apply(&mut self, index: u64, command: Envelope) -> Output {
        self.apply_envelope(index, command)
    }
}
