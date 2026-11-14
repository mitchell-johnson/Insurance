//! The state machine: one logged command in, state changed or a refusal out.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, NaiveDateTime};
use ic_core::PolicyId;
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use ic_money::Currency;

use crate::model::{
    AccountDetails, Actor, Authority, Book, Drill, Exception, ExceptionKind, Id, NameCheck, NewItem, OutboxEntry, Payee, PayeeKind, Probe, Refund,
    RefundDestination, Reject, ReturnReason, Role, Run, RunStatus, RunType, StatementLine, Submission,
};

/// A command with who sent it and when. The proposer reads the clock, never
/// a replica: `at` is New Zealand wall-clock time, which is what bank
/// cutoffs and cooling-off periods are measured in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A repeat is answered with the original result.
    pub key: String,
    pub actor: Actor,
    pub at: NaiveDateTime,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Make a configuration version active. Versions rise by one and an
    /// active version never changes.
    ActivateConfig {
        version: u32,
        config: Box<Config>,
    },

    // --- direct debit authorities ---
    CreateAuthority {
        authority: Id,
        policy: PolicyId,
        party: String,
        /// Set for a scheme member who pays for their own cover.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        life: Option<ic_core::LifeId>,
        account: AccountDetails,
        sole_name: bool,
    },
    SuspendAuthority {
        authority: Id,
        reason: String,
    },
    ReinstateAuthority {
        authority: Id,
    },
    CancelAuthority {
        authority: Id,
        reason: String,
    },
    /// Mark or clear financial hardship on a payer's authority.
    MarkHardship {
        authority: Id,
        hardship: bool,
    },
    /// What a conversation with a payer in hardship decided about a
    /// dishonoured collection held for that conversation.
    RecordConversation {
        item: Id,
        outcome: crate::model::ConversationOutcome,
        note: String,
    },
    /// The core cancelled the payment authority on a policy because its
    /// payer has died (`Event::PaymentAuthorityCancelled`). Nothing more is
    /// collected from the payer's accounts, and nothing is refunded to an
    /// account that was in the payer's sole name.
    PayerDeceased {
        policy: PolicyId,
        claim: String,
    },
    /// The core applied part of a receipt to a policy in a statutory fund
    /// (`Event::ReceiptApplied` with its `fund`).
    ReceiptFunded {
        receipt: String,
        fund: String,
        amount_minor: i64,
    },
    /// The core reversed a receipt (`Event::ReceiptReversed`).
    ReceiptUnfunded {
        receipt: String,
        amount_minor: i64,
    },

    // --- a customer's own hold on money going out (SVC-022) ---
    /// What the customer master says about a party's own hold, as the pump
    /// last asked it. The master owns the hold; this module enforces it.
    RecordCustomerHold {
        party: String,
        held: bool,
    },
    /// The core recorded an insolvency interest naming `owner` as the
    /// policy owner (`InterestEvent::Recorded`, kind insolvency), or released
    /// one with effect from `released`.
    RecordInsolvency {
        owner: String,
        policy: String,
        interest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        released: Option<NaiveDate>,
    },

    // --- accounts that may be paid ---
    RegisterPayee {
        payee: Id,
        party: String,
        kind: PayeeKind,
        account: AccountDetails,
        name_check: NameCheck,
    },
    /// New account details for a payee. Every control starts again and the
    /// cooling-off period runs from now.
    ChangePayeeAccount {
        payee: Id,
        account: AccountDetails,
        name_check: NameCheck,
    },
    /// The details were confirmed with the payee through a second channel,
    /// by someone other than the person who entered them.
    ConfirmPayee {
        payee: Id,
    },
    /// The sanctions screening result.
    ScreenPayee {
        payee: Id,
        clear: bool,
    },
    /// Legal approves payment to an estate or a liquidator.
    ApprovePayee {
        payee: Id,
    },
    ClosePayee {
        payee: Id,
    },

    // --- runs ---
    PrepareRun {
        run: Id,
        run_type: RunType,
        fund: String,
        currency: Currency,
        value_date: NaiveDate,
        #[serde(default)]
        urgent: bool,
    },
    AddRunItems {
        run: Id,
        items: Vec<NewItem>,
    },
    /// Another module released a payment: keep it until a maker puts it in a run.
    OfferPayment {
        waiting: Box<crate::intake::Waiting>,
    },
    /// Claims submitted a run (`run_submitted`): the net total it released.
    ClaimsRunSubmitted {
        run: String,
        submitted_minor: i64,
    },
    /// Many collections failed together for a reason that is not the
    /// payers' doing (COL-024).
    DeclareIncident {
        incident: Id,
        authorities: BTreeSet<Id>,
        from: NaiveDate,
        to: NaiveDate,
        reason: String,
    },
    /// Claims admitted a claim (`claim_admitted`), for the funding forecast.
    ClaimAdmitted {
        claim: String,
        amount_minor: i64,
    },
    /// Claims closed a claim (`claim_closed`): nothing more is forecast.
    ClaimClosed {
        claim: String,
    },
    /// The core issued a policy to its owner (`PolicyIssued`).
    PolicyOwner {
        policy: String,
        owner: String,
    },
    /// The core applied a receipt to a policy (`ReceiptApplied`): check who
    /// paid it (`aml.rs`).
    PremiumApplied {
        policy: String,
        receipt: String,
    },
    /// Compliance closes a payer case, naming the customer master's record
    /// of the payer's relationship to the owner when there is one.
    ClearPayer {
        case: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relationship: Option<String>,
        note: String,
    },
    /// Whether the customer master confirmed a proposed clearance's
    /// relationship links the payer and the owner.
    ConfirmRelationship {
        case: String,
        confirmed: bool,
        #[serde(default)]
        reason: String,
    },
    /// A settled payment, or part of it, went into the wrong account
    /// (`recall.rs`): the bank is asked to recall it, the core holds what
    /// the holder owes, and the right person is owed again.
    ReportMisdirected {
        misdirection: Id,
        item: Id,
        amount_minor: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder: Option<String>,
        note: String,
    },
    /// The bank's answer to a recall.
    RecordRecallAnswer {
        recall: Id,
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// What will not come back, by an approver who did not report it.
    WriteOffMisdirected {
        misdirection: Id,
        amount_minor: i64,
        reason: String,
    },
    /// The core changed a payee debt (`PayeeDebtRaised`, `Recovered`,
    /// `WrittenOff`), with what is still owed.
    PayeeDebtChanged {
        policy: String,
        debt: String,
        outstanding_minor: i64,
    },
    /// What the customer master said about a party this module holds bank
    /// details for: still on its books, or erased (`erase.rs`).
    PartyChecked {
        party: String,
        erased: bool,
    },
    /// The core changed what is owed on a policy's loan (`PolicyLoanDrawn`,
    /// `PolicyLoanRepaid`, `PolicyLoanWrittenOff`).
    LoanChanged {
        policy: String,
        outstanding_minor: i64,
    },
    /// Claims changed an overpayment (`overpayment_*`), with what is still
    /// owed on it (`repay.rs`).
    OverpaymentChanged {
        claim: String,
        overpayment: String,
        policy: String,
        outstanding_minor: i64,
    },
    /// Someone holding a notice's role closes it with what they did.
    CloseNotice {
        notice: u64,
        action: String,
    },
    /// Prepare the day's fund settlement runs for what collection accounts
    /// owe other funds (the pump, each morning).
    ProposeFundSweep {
        as_at: NaiveDate,
    },
    /// Put released payments into a run.
    TakeWaiting {
        run: Id,
        payments: BTreeSet<Id>,
        /// The payee for a payment, where the release does not name one or
        /// the party has several.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        payees: BTreeMap<Id, Id>,
    },
    RemoveRunItem {
        run: Id,
        item: Id,
    },
    /// Run every control over the run's items.
    CheckRun {
        run: Id,
    },
    /// An approver lets a possible duplicate through.
    ReleaseItem {
        run: Id,
        item: Id,
    },
    ApproveRun {
        run: Id,
    },
    CancelRun {
        run: Id,
    },
    /// Send what is still unpaid in an approved run as a new submission.
    Resubmit {
        run: Id,
        value_date: NaiveDate,
    },
    CloseRun {
        run: Id,
    },
    /// Send a type of run through the second bank, or back to the first.
    RedirectRoute {
        run_type: RunType,
        secondary: bool,
    },

    // --- what the channel drivers report ---
    BeginSubmission {
        submission: Id,
        region: String,
    },
    RecordSubmitted {
        submission: Id,
        content_hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bank_reference: Option<String>,
    },
    /// The bank confirmed it does not hold the submission.
    RecordNotReceived {
        submission: Id,
        reason: String,
    },
    RecordAcknowledgement {
        submission: Id,
        /// The bank refused the whole submission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
        /// Items the bank refused, with its reason. The rest were accepted.
        #[serde(default)]
        rejected: BTreeMap<Id, String>,
    },
    RecordSettlement {
        submission: Id,
        settled_on: NaiveDate,
    },
    RecordReturn {
        item: Id,
        reason: ReturnReason,
        returned_on: NaiveDate,
    },

    // --- refunds ---
    RequestRefund {
        refund: Id,
        policy: PolicyId,
        amount_minor: i64,
        currency: Currency,
        destination: RefundDestination,
    },
    ReleaseRefund {
        refund: Id,
    },

    // --- reconciliation ---
    RecordStatement {
        account: Id,
        date: NaiveDate,
        opening_minor: i64,
        closing_minor: i64,
        lines: Vec<StatementLine>,
    },
    /// A driver has recorded every message a bank's channel has sent up to
    /// this one, so no driver need read them again.
    NoteMessagesRead {
        channel: Id,
        through: u64,
    },
    AssignException {
        exception: Id,
        owner: String,
    },
    /// Apply an unidentified receipt to one or more policies by hand; the
    /// parts add up to the receipt (COL-020, COL-022).
    ProposeReceiptMatch {
        exception: Id,
        parts: Vec<(PolicyId, i64)>,
    },
    /// A second reconciler approves a proposed match.
    ApproveReceiptMatch {
        exception: Id,
    },
    ResolveException {
        exception: Id,
        resolution: String,
    },
    /// A driver finished an outbox entry, or the other side refused it.
    CompleteInstruction {
        entry: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
    },

    // --- region drill ---
    StartDrill {
        drill: Id,
        region: String,
    },
    RecordDrillProbe {
        drill: Id,
        channel: Id,
        probe: Probe,
    },
    CloseDrill {
        drill: Id,
    },

    /// Forget results, closed runs and resolved exceptions older than this.
    Prune {
        before: NaiveDate,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ConfigActivated {
        version: u32,
    },
    AuthorityCreated {
        authority: Id,
        policy: PolicyId,
    },
    AuthoritySuspended {
        authority: Id,
        reason: String,
    },
    AuthorityReinstated {
        authority: Id,
    },
    /// The bank's window for a submission's value date had passed when a
    /// driver took it; it goes for the next window.
    SubmissionDeferred {
        submission: Id,
        from: NaiveDate,
        to: NaiveDate,
    },
    HardshipMarked {
        authority: Id,
        hardship: bool,
    },
    /// A dishonour from a payer in hardship waits for a conversation.
    ConversationNeeded {
        item: Id,
        authority: Id,
    },
    ConversationRecorded {
        item: Id,
        outcome: crate::model::ConversationOutcome,
    },
    AuthorityCancelled {
        authority: Id,
        policy: PolicyId,
        reason: String,
    },
    /// Nothing may be collected from or refunded to this account again.
    AccountBlocked {
        authority: Id,
    },
    PayeeRegistered {
        payee: Id,
        kind: PayeeKind,
    },
    PayeeAccountChanged {
        payee: Id,
        cooling_off_until: NaiveDateTime,
    },
    PayeeConfirmed {
        payee: Id,
        by: String,
    },
    PayeeScreened {
        payee: Id,
        clear: bool,
    },
    PayeeApproved {
        payee: Id,
        by: String,
    },
    PayeeClosed {
        payee: Id,
    },
    RunPrepared {
        run: Id,
        run_type: RunType,
    },
    RunItemsAdded {
        run: Id,
        count: usize,
    },
    PaymentOffered {
        payment: Id,
        source: String,
        amount_minor: i64,
    },
    RunItemRemoved {
        run: Id,
        item: Id,
    },
    RunChecked {
        run: Id,
        held: usize,
    },
    ItemReleased {
        run: Id,
        item: Id,
        by: String,
    },
    RunApproval {
        run: Id,
        by: String,
        approved: bool,
    },
    RunCancelled {
        run: Id,
    },
    RunClosed {
        run: Id,
    },
    RouteRedirected {
        run_type: RunType,
        secondary: bool,
    },
    SubmissionQueued {
        run: Id,
        submission: Id,
        channel: Id,
    },
    SubmissionBegun {
        submission: Id,
        region: String,
    },
    SubmissionSent {
        submission: Id,
        content_hash: String,
    },
    SubmissionFailed {
        submission: Id,
        reason: String,
    },
    SubmissionAcknowledged {
        submission: Id,
        accepted: usize,
        rejected: Vec<Id>,
    },
    SubmissionSettled {
        submission: Id,
        total_minor: i64,
    },
    /// The bank settled an item. Whoever asked for `source` to be paid
    /// reads this to know it was.
    ItemSettled {
        item: Id,
        source: String,
        amount_minor: i64,
    },
    ItemReturned {
        item: Id,
        source: String,
        amount_minor: i64,
        reason: ReturnReason,
    },
    RefundRequested {
        refund: Id,
        policy: PolicyId,
        amount_minor: i64,
    },
    RefundRaised {
        refund: Id,
    },
    RefundRefused {
        refund: Id,
        code: String,
    },
    RefundReleasing {
        refund: Id,
    },
    RefundReleased {
        refund: Id,
    },
    /// The refund will not go where it was going: the claim carries it.
    RefundRerouted {
        refund: Id,
        claim: String,
    },
    StatementReconciled {
        account: Id,
        date: NaiveDate,
        matched: usize,
        exceptions: usize,
    },
    ExceptionOpened {
        exception: Id,
        kind: ExceptionKind,
        amount_minor: i64,
    },
    ExceptionAssigned {
        exception: Id,
        owner: String,
    },
    ExceptionResolved {
        exception: Id,
        resolution: String,
    },
    InstructionCompleted {
        key: String,
    },
    /// A collection account owes another fund this much more (or less, when
    /// negative) for money applied to that fund's policies.
    FundOwed {
        account: Id,
        fund: String,
        amount_minor: i64,
    },
    NoticeClosed {
        notice: u64,
    },
    IncidentDeclared {
        incident: Id,
    },
    /// The customer master's answer about a party's hold changed.
    CustomerHoldRecorded {
        party: String,
        held: bool,
    },
    ReceiptMatchProposed {
        exception: Id,
    },
    ReceiptMatchApproved {
        exception: Id,
    },
    /// Premium was paid by someone other than the policy owner.
    PayerCaseOpened {
        case: String,
    },
    PayerCaseCleared {
        case: String,
    },
    MisdirectionReported {
        misdirection: Id,
        item: Id,
        amount_minor: i64,
    },
    RecallAnswered {
        misdirection: Id,
        accepted: bool,
    },
    /// Nothing more is owed on a misdirected payment.
    MisdirectionClosed {
        misdirection: Id,
    },
    /// The customer master erased a party, and its bank details here were
    /// forgotten.
    PartyErased {
        party: String,
    },
    /// Whether an insolvency interest stands against a policy owner changed.
    OwnerInsolvencyChanged {
        party: String,
        insolvent: bool,
    },
    DrillStarted {
        drill: Id,
        region: String,
    },
    DrillProbed {
        drill: Id,
        channel: Id,
        ok: bool,
    },
    DrillClosed {
        drill: Id,
        region: String,
        passed: bool,
        unreached: Vec<Id>,
    },
    Pruned {
        before: NaiveDate,
    },
}

/// What a command did, or why it was refused.
pub type Output = Result<Vec<Event>, Reject>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentsState {
    /// The latest time any command carried. Time never runs backwards here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<NaiveDateTime>,
    pub config_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,
    pub authorities: BTreeMap<Id, Authority>,
    /// Account numbers that were in a deceased payer's sole name.
    pub blocked_accounts: BTreeSet<String>,
    pub payees: BTreeMap<Id, Payee>,
    /// Policies whose payer has died, with the claim that said so.
    pub deceased_payers: BTreeMap<PolicyId, String>,
    /// Run types going through their second bank.
    pub redirected: BTreeSet<RunType>,
    pub runs: BTreeMap<Id, Run>,
    /// Every item ever taken, with its run. An item ID is never used twice.
    pub items: BTreeMap<Id, Id>,
    /// What is being paid right now, with the item paying it.
    pub sources: BTreeMap<String, Id>,
    pub submissions: BTreeMap<Id, Submission>,
    /// Dishonoured collections that may be presented once more.
    pub represent: BTreeSet<Id>,
    /// Dishonoured collections from payers in hardship, waiting for someone
    /// to talk to the payer before anything is presented again.
    #[serde(default)]
    pub awaiting_conversation: BTreeMap<Id, Id>,
    /// Items that went to the bank later than their run's value date, by
    /// item, so what the delay caused can be found and reversed.
    #[serde(default)]
    pub deferred: BTreeMap<Id, crate::model::Deferred>,
    pub refunds: BTreeMap<Id, Refund>,
    /// Payments other modules released, until they settle.
    #[serde(default)]
    pub waiting: BTreeMap<Id, crate::intake::Waiting>,
    /// Claims runs with payments still to settle.
    #[serde(default)]
    pub claims_runs: BTreeMap<String, crate::intake::ClaimsRun>,
    /// How far each channel's messages from the bank have been recorded.
    #[serde(default)]
    pub messages_read: BTreeMap<Id, u64>,
    pub books: BTreeMap<Id, Book>,
    /// Cash owed between statutory funds (PAY-010).
    #[serde(default)]
    pub sweeps: crate::sweep::Sweeps,
    /// Collection incidents (COL-024).
    #[serde(default)]
    pub incidents: BTreeMap<Id, crate::incident::Incident>,
    /// Unidentified receipts a person is applying by hand (`manual.rs`).
    #[serde(default)]
    pub manual_matches: BTreeMap<Id, crate::manual::Proposal>,
    /// Claims admitted and not yet released for payment (PAY-013).
    #[serde(default)]
    pub admitted: BTreeMap<String, crate::forecast::Admitted>,
    /// Work for people: every notice raised, until closed (`notices.rs`).
    #[serde(default)]
    pub notices: crate::notices::Notices,
    /// What the customer master last said about parties' own holds on
    /// money going out (SVC-022).
    #[serde(default)]
    pub customer_holds: BTreeMap<String, crate::hold::HoldRecord>,
    /// Claim overpayments still owed, under the reference a repayment
    /// quotes (`repay.rs`).
    #[serde(default)]
    pub repayables: crate::repay::Repayables,
    /// Who pays whose premium (`aml.rs`).
    #[serde(default)]
    pub payer_checks: crate::aml::PayerChecks,
    /// Policy owners with insolvency interests recorded by the core.
    #[serde(default)]
    pub insolvencies: BTreeMap<String, crate::insolvency::Insolvency>,
    /// Payments that went to the wrong account (`recall.rs`).
    #[serde(default)]
    pub misdirections: BTreeMap<Id, crate::recall::Misdirection>,
    /// Parties the customer master has erased, and when each was checked.
    #[serde(default)]
    pub erasure: crate::erase::Erasure,
    pub exceptions: BTreeMap<Id, Exception>,
    pub drills: BTreeMap<Id, Drill>,
    /// Work for the drivers, in order. Removed when its result is recorded.
    pub outbox: BTreeMap<u64, OutboxEntry>,
    pub next_outbox: u64,
    /// Results by idempotency key, with the day they were given.
    pub answered: BTreeMap<String, (NaiveDate, Output)>,
    /// Events so far and the hash that chains them, so the trail the drivers
    /// keep in the vault can be checked against the state.
    pub event_seq: u64,
    pub event_chain: String,
}

/// One command being applied.
pub(crate) struct Tx<'a> {
    pub actor: &'a Actor,
    pub now: NaiveDateTime,
    pub events: Vec<Event>,
}

impl Tx<'_> {
    pub fn need(&self, role: Role) -> Result<(), Reject> {
        if self.actor.roles.contains(&role) {
            Ok(())
        } else {
            Err(Reject::new("not_permitted", format!("this needs the {} role", role_words(role))))
        }
    }
    pub fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
}

impl PaymentsState {
    pub(crate) fn cfg(&self) -> Result<&Config, Reject> {
        self.config.as_ref().ok_or_else(|| Reject::new("not_configured", "no configuration is active"))
    }

    pub(crate) fn push_outbox(&mut self, entry: OutboxEntry) {
        if let crate::model::Instruction::Notify { to, subject, detail } = &entry.instruction {
            self.raise_notice(*to, subject, detail);
        }
        self.outbox.insert(self.next_outbox, entry);
        self.next_outbox += 1;
    }

    fn activate_config(&mut self, tx: &mut Tx<'_>, version: u32, config: Config) -> Result<(), Reject> {
        tx.need(Role::ConnectivityAdmin)?;
        if version == self.config_version && self.config.as_ref() == Some(&config) {
            return Ok(());
        }
        if version <= self.config_version {
            return Err(Reject::new(
                "config_conflict",
                format!("version {version} is already active with other content"),
            ));
        }
        if version != self.config_version + 1 {
            return Err(Reject::new("config_gap", format!("the next version is {}", self.config_version + 1)));
        }
        config.validate(tx.now.date())?;
        self.config_version = version;
        self.config = Some(config);
        tx.emit(Event::ConfigActivated { version });
        Ok(())
    }

    fn dispatch(&mut self, tx: &mut Tx<'_>, command: Command) -> Result<(), Reject> {
        match command {
            Command::ActivateConfig { version, config } => self.activate_config(tx, version, *config),
            Command::CreateAuthority {
                authority,
                policy,
                party,
                life,
                account,
                sole_name,
            } => self.create_authority(tx, authority, policy, party, life, account, sole_name),
            Command::SuspendAuthority { authority, reason } => self.suspend_authority(tx, &authority, reason),
            Command::ReinstateAuthority { authority } => self.reinstate_authority(tx, &authority),
            Command::CancelAuthority { authority, reason } => self.cancel_authority(tx, &authority, reason),
            Command::MarkHardship { authority, hardship } => self.mark_hardship(tx, &authority, hardship),
            Command::RecordConversation { item, outcome, note } => self.record_conversation(tx, &item, outcome, note),
            Command::PayerDeceased { policy, claim } => self.payer_deceased(tx, &policy, &claim),
            Command::ReceiptFunded { receipt, fund, amount_minor } => {
                tx.need(Role::Service)?;
                self.receipt_funded(tx, &receipt, &fund, amount_minor)
            }
            Command::ReceiptUnfunded { receipt, amount_minor } => {
                tx.need(Role::Service)?;
                self.receipt_unfunded(tx, &receipt, amount_minor)
            }
            Command::RecordCustomerHold { party, held } => self.record_customer_hold(tx, &party, held),
            Command::RecordInsolvency {
                owner,
                policy,
                interest,
                released,
            } => self.record_insolvency(tx, &owner, &policy, &interest, released),
            Command::DeclareIncident {
                incident,
                authorities,
                from,
                to,
                reason,
            } => self.declare_incident(tx, incident, authorities, from, to, reason),
            Command::ClaimAdmitted { claim, amount_minor } => self.claim_admitted(tx, &claim, amount_minor),
            Command::PolicyOwner { policy, owner } => self.policy_owner(tx, &policy, &owner),
            Command::PremiumApplied { policy, receipt } => self.premium_applied(tx, &policy, &receipt),
            Command::ClearPayer { case, relationship, note } => self.clear_payer(tx, &case, relationship, note),
            Command::ConfirmRelationship { case, confirmed, reason } => self.confirm_relationship(tx, &case, confirmed, reason),
            Command::ReportMisdirected {
                misdirection,
                item,
                amount_minor,
                holder,
                note,
            } => self.report_misdirected(tx, misdirection, &item, amount_minor, holder, note),
            Command::RecordRecallAnswer { recall, accepted, reason } => self.record_recall_answer(tx, &recall, accepted, reason),
            Command::WriteOffMisdirected {
                misdirection,
                amount_minor,
                reason,
            } => self.write_off_misdirected(tx, &misdirection, amount_minor, reason),
            Command::PayeeDebtChanged {
                policy,
                debt,
                outstanding_minor,
            } => self.payee_debt_changed(tx, &policy, &debt, outstanding_minor),
            Command::PartyChecked { party, erased } => self.party_checked(tx, &party, erased),
            Command::LoanChanged { policy, outstanding_minor } => self.loan_changed(tx, &policy, outstanding_minor),
            Command::OverpaymentChanged {
                claim,
                overpayment,
                policy,
                outstanding_minor,
            } => self.overpayment_changed(tx, &claim, &overpayment, &policy, outstanding_minor),
            Command::ClaimClosed { claim } => {
                tx.need(Role::Service)?;
                self.claim_left_forecast(&claim);
                Ok(())
            }
            Command::CloseNotice { notice, action } => self.close_notice(tx, notice, action),
            Command::ProposeFundSweep { as_at } => self.propose_fund_sweep(tx, as_at),
            Command::ClaimsRunSubmitted { run, submitted_minor } => self.claims_run_submitted(tx, &run, submitted_minor),
            Command::RegisterPayee {
                payee,
                party,
                kind,
                account,
                name_check,
            } => self.register_payee(tx, payee, party, kind, account, name_check),
            Command::ChangePayeeAccount { payee, account, name_check } => self.change_payee_account(tx, &payee, account, name_check),
            Command::ConfirmPayee { payee } => self.confirm_payee(tx, &payee),
            Command::ScreenPayee { payee, clear } => self.screen_payee(tx, &payee, clear),
            Command::ApprovePayee { payee } => self.approve_payee(tx, &payee),
            Command::ClosePayee { payee } => self.close_payee(tx, &payee),
            Command::PrepareRun {
                run,
                run_type,
                fund,
                currency,
                value_date,
                urgent,
            } => self.prepare_run(tx, run, run_type, fund, currency, value_date, urgent),
            Command::AddRunItems { run, items } => self.add_run_items(tx, &run, items),
            Command::OfferPayment { waiting } => self.offer_payment(tx, *waiting),
            Command::TakeWaiting { run, payments, payees } => self.take_waiting(tx, &run, payments, payees),
            Command::RemoveRunItem { run, item } => self.remove_run_item(tx, &run, &item),
            Command::CheckRun { run } => self.check_run(tx, &run),
            Command::ReleaseItem { run, item } => self.release_item(tx, &run, &item),
            Command::ApproveRun { run } => self.approve_run(tx, &run),
            Command::CancelRun { run } => self.cancel_run(tx, &run),
            Command::Resubmit { run, value_date } => self.resubmit(tx, &run, value_date),
            Command::CloseRun { run } => self.close_run(tx, &run),
            Command::RedirectRoute { run_type, secondary } => self.redirect_route(tx, run_type, secondary),
            Command::BeginSubmission { submission, region } => self.begin_submission(tx, &submission, region),
            Command::RecordSubmitted {
                submission,
                content_hash,
                bank_reference,
            } => self.record_submitted(tx, &submission, content_hash, bank_reference),
            Command::RecordNotReceived { submission, reason } => self.record_not_received(tx, &submission, reason),
            Command::RecordAcknowledgement { submission, refused, rejected } => self.record_acknowledgement(tx, &submission, refused, rejected),
            Command::RecordSettlement { submission, settled_on } => self.record_settlement(tx, &submission, settled_on),
            Command::RecordReturn { item, reason, returned_on } => self.record_return(tx, &item, reason, returned_on),
            Command::RequestRefund {
                refund,
                policy,
                amount_minor,
                currency,
                destination,
            } => self.request_refund(tx, refund, policy, amount_minor, currency, destination),
            Command::ReleaseRefund { refund } => self.release_refund(tx, &refund),
            Command::RecordStatement {
                account,
                date,
                opening_minor,
                closing_minor,
                lines,
            } => self.record_statement(tx, &account, date, opening_minor, closing_minor, lines),
            Command::AssignException { exception, owner } => self.assign_exception(tx, &exception, owner),
            Command::ProposeReceiptMatch { exception, parts } => self.propose_receipt_match(tx, &exception, parts),
            Command::ApproveReceiptMatch { exception } => self.approve_receipt_match(tx, &exception),
            Command::ResolveException { exception, resolution } => self.resolve_exception(tx, &exception, resolution),
            Command::CompleteInstruction { entry, refused } => self.complete_instruction(tx, entry, refused),
            Command::StartDrill { drill, region } => self.start_drill(tx, drill, region),
            Command::NoteMessagesRead { channel, through } => self.note_messages_read(tx, channel, through),
            Command::RecordDrillProbe { drill, channel, probe } => self.record_drill_probe(tx, &drill, channel, probe),
            Command::CloseDrill { drill } => self.close_drill(tx, &drill),
            Command::Prune { before } => self.prune(tx, before),
        }
    }

    /// Forget what is finished and older than `before`. The trail of events
    /// is in the vault; this is only what the replicas carry.
    fn prune(&mut self, tx: &mut Tx<'_>, before: NaiveDate) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        self.answered.retain(|_, (day, _)| *day >= before);
        let gone: Vec<Id> = self
            .runs
            .iter()
            .filter(|(_, r)| matches!(r.status, RunStatus::Closed | RunStatus::Cancelled) && r.value_date < before)
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            if let Some(run) = self.runs.remove(&id) {
                for submission in &run.submissions {
                    self.submissions.remove(submission);
                }
                for item in run.items.keys() {
                    self.items.remove(item);
                    self.represent.remove(item);
                }
            }
        }
        self.exceptions.retain(|_, e| e.resolution.is_none() || e.opened_on >= before);
        self.drills.retain(|_, d| d.passed.is_none() || d.started.date() >= before);
        tx.emit(Event::Pruned { before });
        Ok(())
    }

    fn chain(&mut self, events: &[Event]) {
        for event in events {
            self.event_seq += 1;
            let mut h = Sha256::new();
            h.update(b"ic.payments.event.v1");
            h.update(self.event_chain.as_bytes());
            h.update(self.event_seq.to_be_bytes());
            // An event is plain data: it always has a JSON form.
            h.update(serde_json::to_vec(event).unwrap_or_default());
            self.event_chain = hex::encode(h.finalize());
        }
    }
}

impl StateMachine for PaymentsState {
    type Command = Envelope;
    type Output = Output;

    fn apply(&mut self, _index: u64, envelope: Envelope) -> Output {
        if let Some((_, original)) = self.answered.get(&envelope.key) {
            return original.clone();
        }
        let now = self.clock.map_or(envelope.at, |c| c.max(envelope.at));
        self.clock = Some(now);
        let mut tx = Tx {
            actor: &envelope.actor,
            now,
            events: Vec::new(),
        };
        // The people who release money never also decide who may be paid or
        // how the bank is reached.
        let roles = &envelope.actor.roles;
        let output = if envelope.key.is_empty() || envelope.actor.id.is_empty() {
            Err(Reject::new("invalid_envelope", "a command carries an idempotency key and names who sent it"))
        } else if roles.contains(&Role::Approver) && (roles.contains(&Role::PayeeMaintainer) || roles.contains(&Role::ConnectivityAdmin)) {
            Err(Reject::new("role_conflict", "an approver does not maintain payees or bank connectivity"))
        } else {
            self.dispatch(&mut tx, envelope.command).map(|()| tx.events)
        };
        if let Ok(events) = &output {
            self.chain(events);
        }
        self.answered.insert(envelope.key, (now.date(), output.clone()));
        output
    }
}

/// A role as a person reads it: "payee maintainer", not "PayeeMaintainer".
pub fn role_words(role: Role) -> String {
    let name = format!("{role:?}");
    let mut words = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            words.push(' ');
        }
        words.push(c.to_ascii_lowercase());
    }
    words
}
