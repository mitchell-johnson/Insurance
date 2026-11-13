//! The finance ledger state machine.
//!
//! It is a second instance of the ledger engine, with one account per chart
//! account and unit where the core has one per policy. It is fed only by
//! the events the core publishes:
//!
//! * every posted entry arrives as lines by account role. The chart maps
//!   each role to a finance account, and the lines of one partition and
//!   business date are posted as one summarised journal when that day
//!   closes;
//! * each close arrives with the partition's balances by role, taken by the
//!   core from its own materialised balances. The books built here from the
//!   lines must equal them, and the chain head the totals were taken at must
//!   be the one the day's signed Merkle root covers. That is the daily
//!   control check.
//!
//! A backdated entry or a reversal is never a corrected copy of an old
//! entry. It is a new entry today with its original value date. Where that
//! date falls in an earlier period the line is kept apart as a prior-period
//! item under its original period, so the current period's own activity
//! reads on its own, and a closed period never changes.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate};
use ic_core::event::{Event, EventRecord, RoleTotal};
use ic_core::{PolicyId, Reject};
use ic_ledger::{
    merkle_root, Account, AccountClass, AccountId, AccountUnit, Amount, BalanceFamily, Dimensions, EntryDraft, EntryId, Hash32, JournalEntry, Ledger,
    PartitionId, Posting, Side,
};
use ic_log::StateMachine;
use ic_money::{AccrualAmount, Currency, UnitQty, FINE_PER_MINOR};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chart::Chart;
use crate::close::{
    BalanceRow, Capacity, CheckReport, CheckSource, CheckStatus, CloseConfig, ClosedPeriod, Outcome, PeriodState, PeriodStatus, SignOff, Waiver,
};
use crate::period::PeriodId;
use crate::returns::{ConductFact, ConductKey, FiledReturn, ReturnDefinition};

/// The partition number the finance ledger's own chain is kept under. The
/// core's partitions are numbered from zero and there are 64 of them.
pub const FINANCE_PARTITION: PartitionId = 9000;

/// Most events one ingest command may carry.
pub const MAX_INGEST_EVENTS: usize = 2_000;
/// Most lines a manual journal may carry.
pub const MAX_JOURNAL_LINES: usize = 500;
/// Most aged items one report may carry.
pub const MAX_AGED_ITEMS: usize = 2_000;

/// The caller recorded on every journal posted from the core's events,
/// whichever command happened to release them.
pub const FEED: &str = "feed";

pub const ROLE_OPERATOR: &str = "finance_operator";
pub const ROLE_APPROVER: &str = "finance_approver";
pub const ROLE_LEAD: &str = "finance_lead";
pub const ROLE_ACTUARY: &str = "appointed_actuary";

/// What goes into the finance ledger's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinEnvelope {
    /// Idempotency key. A duplicate returns the original result.
    pub key: String,
    /// The module or service sending the command.
    pub caller: String,
    /// The person acting, for commands a person takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The roles the gateway verified the actor holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    pub command: FinCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    Accrual,
    Prepayment,
    Intercompany,
    Tax,
    /// Results of the actuarial run. Carries the extract it was run from.
    Actuarial,
    /// Money moving between statutory funds. The only kind that may name
    /// more than one fund, and only the finance lead approves it.
    FundTransfer,
    /// Premiums of aggregate treaties, which are not per-policy postings.
    Reinsurance,
    OpeningBalance,
    Correction,
    /// Income and expense closed to retained earnings. Built by the ledger.
    YearEndClose,
}

/// One line of a manual journal: debits positive, in fine units (millionths
/// of a cent) of the currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalLine {
    pub account: String,
    pub currency: Currency,
    pub signed_fine: i128,
    #[serde(default)]
    pub fund: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub cover_type: Option<String>,
    #[serde(default)]
    pub posting_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalDraft {
    pub id: String,
    pub kind: JournalKind,
    /// The period it posts in. Open or closing, never closed.
    pub period: PeriodId,
    /// When it takes effect. A date in an earlier period makes it a
    /// prior-period item.
    pub value_date: NaiveDate,
    pub lines: Vec<JournalLine>,
    pub reason: String,
    /// References to the supporting evidence in the vault.
    pub evidence: Vec<String>,
    /// The frozen extract an actuarial journal was calculated from.
    #[serde(default)]
    pub extract_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JournalStatus {
    Proposed,
    Approved { checker: String, entry: EntryId },
    Rejected { checker: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub draft: JournalDraft,
    pub maker: String,
    pub status: JournalStatus,
    /// The appointed actuary's sign-off, for an actuarial journal.
    pub actuary: Option<String>,
}

/// An item another module is holding that should not be held for long:
/// money in suspense, a held commission balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgedItem {
    /// Opaque ID of the party or policy it is held for.
    pub party: String,
    /// Why it is held, for example `suspended_licence` or `dispute`.
    pub reason: String,
    pub amount_minor: i64,
    /// Held since.
    pub since: NaiveDate,
    #[serde(default)]
    pub owner: Option<String>,
    /// Reference to the documented action, if one has been recorded.
    #[serde(default)]
    pub action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgedReport {
    pub as_at: NaiveDate,
    /// The items old or large enough to matter, with or without an action.
    pub items: Vec<AgedItem>,
    /// Everything held, listed or not.
    pub total_minor: i64,
    pub count: u32,
    pub index: u64,
}

/// A frozen extract from the data mart, registered so that anything
/// calculated from it can be traced to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractRecord {
    pub id: String,
    /// `in_force`, `movement`, `claims`, `reinsurance`, `unit_holdings`, `experience`.
    pub kind: String,
    pub as_at: NaiveDate,
    /// SHA-256 of the extract file.
    pub content_hash: Hash32,
    pub rows: u64,
    /// The published Merkle root of the business date it was cut at.
    pub merkle_root: Hash32,
    /// Version of the extract definition that produced it.
    pub definition_version: u32,
    /// Where the file is kept in immutable storage.
    pub location: String,
    /// Control totals, for example `policies`, `sum_insured_minor`.
    #[serde(default)]
    pub control: BTreeMap<String, i128>,
    /// Whether opening plus movements equals closing.
    pub reconciled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinCommand {
    /// Set once, before anything else: how many partitions the core has and
    /// the month the financial year ends with.
    Configure {
        partitions: PartitionId,
        year_end_month: u32,
    },
    ProposeChart {
        chart: Chart,
    },
    ProposeCloseConfig {
        config: CloseConfig,
    },
    /// Approve a proposed chart or close configuration. Never by its proposer.
    ApproveConfig {
        id: String,
    },
    /// The next events of one partition's stream, in order.
    Ingest {
        partition: PartitionId,
        events: Vec<EventRecord>,
    },
    /// One event of a partition's stream, as the node hosting the ledger
    /// delivers them. Unlike `Ingest`, an event the books cannot take yet is
    /// kept, in order, until they can: it is never refused and lost.
    CoreEvent {
        record: Box<EventRecord>,
    },
    /// Try again to take in the events held for a partition.
    RetryFeed {
        partition: PartitionId,
    },
    /// The signed Merkle root published for a business date.
    RecordMerkleRoot {
        date: NaiveDate,
        root: Hash32,
        /// Where the signed root is kept.
        reference: String,
    },
    ProposeJournal {
        journal: JournalDraft,
    },
    ApproveJournal {
        id: String,
    },
    RejectJournal {
        id: String,
        reason: String,
    },
    SignActuarialJournal {
        id: String,
    },
    /// Build the journal that closes a financial year's income and expense
    /// to retained earnings, in the year's adjustment period.
    ProposeYearEndClose {
        id: String,
        year_end: PeriodId,
        evidence: Vec<String>,
    },
    ReportCheck {
        period: PeriodId,
        check: String,
        outcome: Outcome,
        #[serde(default)]
        evidence: Option<String>,
    },
    ReportAgedItems {
        kind: String,
        as_at: NaiveDate,
        items: Vec<AgedItem>,
        total_minor: i64,
        count: u32,
    },
    /// Record the action taken on one aged item: who owns it and where the
    /// action is written up. It stops the item turning the check red.
    DocumentAgedItem {
        kind: String,
        party: String,
        /// The date the item has been held since, as the report gives it.
        since: NaiveDate,
        action: String,
        owner: String,
    },
    WaiveCheck {
        period: PeriodId,
        check: String,
        reason: String,
        remediation_owner: String,
        due: NaiveDate,
    },
    SignOff {
        period: PeriodId,
        capacity: Capacity,
    },
    ClosePeriod {
        period: PeriodId,
    },
    RegisterExtract {
        extract: ExtractRecord,
    },
    /// Things that happened in claims or complaints, for the conduct measures.
    RecordConductFacts {
        facts: Vec<ConductFact>,
    },
    /// Work a return out from a closed period and store it as filed.
    FileReturn {
        id: String,
        definition: ReturnDefinition,
        period: PeriodId,
    },
    /// File a return again after a later posting restated its period.
    RestateReturn {
        id: String,
        definition: ReturnDefinition,
        reason: String,
    },
    /// Place every policy still without an accounting group by the rules
    /// of the close configuration now in force. A policy already placed is
    /// never moved.
    ClassifyPolicies,
}

impl FinCommand {
    /// The callers allowed to send each command.
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            FinCommand::Configure { .. } => &["operator"],
            FinCommand::Ingest { .. } | FinCommand::CoreEvent { .. } | FinCommand::RecordMerkleRoot { .. } => &["feed"],
            FinCommand::RetryFeed { .. } => &["operator", "finance-console"],
            FinCommand::RegisterExtract { .. } => &["reporting"],
            FinCommand::RecordConductFacts { .. } => &["claims", "complaints"],
            FinCommand::ClosePeriod { .. } => &["finance-console", "close-workflow"],
            // Who may report which check is declared in the close configuration.
            FinCommand::ReportCheck { .. } | FinCommand::ReportAgedItems { .. } => &[],
            _ => &["finance-console"],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            FinCommand::Configure { .. } => "configure",
            FinCommand::ProposeChart { .. } => "propose_chart",
            FinCommand::ProposeCloseConfig { .. } => "propose_close_config",
            FinCommand::ApproveConfig { .. } => "approve_config",
            FinCommand::Ingest { .. } => "ingest",
            FinCommand::CoreEvent { .. } => "core_event",
            FinCommand::RetryFeed { .. } => "retry_feed",
            FinCommand::RecordMerkleRoot { .. } => "record_merkle_root",
            FinCommand::ProposeJournal { .. } => "propose_journal",
            FinCommand::ApproveJournal { .. } => "approve_journal",
            FinCommand::RejectJournal { .. } => "reject_journal",
            FinCommand::SignActuarialJournal { .. } => "sign_actuarial_journal",
            FinCommand::ProposeYearEndClose { .. } => "propose_year_end_close",
            FinCommand::ReportCheck { .. } => "report_check",
            FinCommand::ReportAgedItems { .. } => "report_aged_items",
            FinCommand::DocumentAgedItem { .. } => "document_aged_item",
            FinCommand::WaiveCheck { .. } => "waive_check",
            FinCommand::SignOff { .. } => "sign_off",
            FinCommand::ClosePeriod { .. } => "close_period",
            FinCommand::RegisterExtract { .. } => "register_extract",
            FinCommand::RecordConductFacts { .. } => "record_conduct_facts",
            FinCommand::FileReturn { .. } => "file_return",
            FinCommand::RestateReturn { .. } => "restate_return",
            FinCommand::ClassifyPolicies => "classify_policies",
        }
    }
}

/// Facts the finance ledger publishes. They carry IDs and amounts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinEvent {
    Configured {
        partitions: PartitionId,
        year_end_month: u32,
    },
    ConfigProposed {
        id: String,
        by: String,
    },
    ChartActivated {
        version: u32,
        content_hash: String,
    },
    CloseConfigActivated {
        version: u32,
        content_hash: String,
    },
    /// One partition's business day was posted and checked against the core.
    DayControlled {
        partition: PartitionId,
        date: NaiveDate,
        equal: bool,
        entries: u64,
    },
    /// A backdated entry or a reversal reached the books.
    BackdatedEntryReceived {
        entry: EntryId,
        period: PeriodId,
        origin: Option<PeriodId>,
    },
    MerkleRootRecorded {
        date: NaiveDate,
        anchored: bool,
    },
    PeriodClosing {
        period: PeriodId,
    },
    JournalProposed {
        id: String,
        kind: JournalKind,
        period: PeriodId,
        maker: String,
    },
    JournalApproved {
        id: String,
        checker: String,
        entry: EntryId,
    },
    JournalRejected {
        id: String,
        checker: String,
    },
    ActuarialJournalSigned {
        id: String,
        by: String,
    },
    CheckReported {
        period: PeriodId,
        check: String,
        green: bool,
    },
    AgedItemsReported {
        kind: String,
        as_at: NaiveDate,
        total_minor: i64,
        count: u32,
    },
    AgedItemDocumented {
        kind: String,
        party: String,
        action: String,
        owner: String,
        by: String,
    },
    CheckWaived {
        period: PeriodId,
        check: String,
        by: String,
        remediation_owner: String,
        due: NaiveDate,
    },
    SignedOff {
        period: PeriodId,
        capacity: Capacity,
        by: String,
    },
    /// A posting landed after sign-off, so the period must be signed again.
    SignOffsCleared {
        period: PeriodId,
    },
    PeriodClosed {
        period: PeriodId,
        final_seq: u64,
        trial_balance_hash: Hash32,
    },
    ExtractRegistered {
        id: String,
        kind: String,
        as_at: NaiveDate,
        content_hash: Hash32,
    },
    ConductFactsRecorded {
        count: u32,
    },
    /// Events from a partition are being held because the books cannot take
    /// the next one. Nothing is lost; the feed resumes when the cause is fixed.
    FeedStalled {
        partition: PartitionId,
        code: String,
    },
    FeedResumed {
        partition: PartitionId,
    },
    ReturnFiled {
        id: String,
        period: PeriodId,
        version: u32,
        hash: Hash32,
    },
    PoliciesClassified {
        placed: u64,
        still_ungrouped: u64,
        rule_version: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinEventRecord {
    /// Gapless.
    pub seq: u64,
    pub log_index: u64,
    pub event: FinEvent,
}

/// What applying one command produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinApplied {
    pub index: u64,
    pub result: Result<(), Reject>,
    pub events: Vec<FinEventRecord>,
    /// Entries added to the finance ledger's own hash chain, for the cold tier.
    pub entries: Vec<JournalEntry>,
    /// The key had been seen before: this is the original result.
    pub duplicate: bool,
}

/// The dimensions of a line, in a form that orders.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Dims {
    pub fund: Option<String>,
    pub product: Option<String>,
    pub cover_type: Option<String>,
    pub posting_type: Option<String>,
}

impl From<&Dimensions> for Dims {
    fn from(d: &Dimensions) -> Self {
        Self {
            fund: d.statutory_fund.clone(),
            product: d.product.clone(),
            cover_type: d.cover_type.clone(),
            posting_type: d.posting_type.clone(),
        }
    }
}

impl From<&Dims> for Dimensions {
    fn from(d: &Dims) -> Self {
        Self {
            statutory_fund: d.fund.clone(),
            product: d.product.clone(),
            cover_type: d.cover_type.clone(),
            posting_type: d.posting_type.clone(),
        }
    }
}

/// One cell of the books: what moved in a period on an account, by unit and
/// dimension. `origin` is set on a prior-period item: the earlier period
/// its value date falls in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CubeKey {
    pub period: PeriodId,
    pub account: String,
    pub unit: BalanceFamily,
    pub dims: Dims,
    pub origin: Option<PeriodId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct PendingKey {
    origin: Option<PeriodId>,
    account: String,
    unit: BalanceFamily,
    dims: Dims,
}

type RoleKey = (String, AccountClass, AccountUnit);

/// Where the feed from one partition has reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Feed {
    next_event_seq: u64,
    next_entry_seq: u64,
    /// Head of the partition's chain after the last entry received.
    head: Hash32,
    /// The partition's open business date.
    date: Option<NaiveDate>,
    /// The business date closed before the open one. The end-of-day batch
    /// for a date posts on the next business date with the closed date as
    /// its value date.
    previous: Option<NaiveDate>,
    /// A day whose close has been seen and whose control totals are due next.
    awaiting: Option<NaiveDate>,
    /// Lines of the open business date, not yet posted.
    #[serde(with = "crate::pairs")]
    pending: BTreeMap<PendingKey, i128>,
    day_first_seq: Option<u64>,
    /// The partition's balances by role, built from every line received.
    #[serde(with = "crate::pairs")]
    roles: BTreeMap<RoleKey, i128>,
}

impl Feed {
    fn new(partition: PartitionId) -> Self {
        Self {
            next_event_seq: 1,
            next_entry_seq: 1,
            head: Ledger::genesis(partition),
            date: None,
            previous: None,
            awaiting: None,
            pending: BTreeMap::new(),
            day_first_seq: None,
            roles: BTreeMap::new(),
        }
    }
}

/// A role whose balance in the core differs from the books built here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlDifference {
    pub role: String,
    pub class: AccountClass,
    pub unit: AccountUnit,
    pub core: i128,
    pub finance: i128,
}

/// One partition's closed business day as the finance ledger holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRecord {
    /// The partition's chain head when the day closed.
    pub closing_head: Hash32,
    /// The core entries the day's journals summarise.
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    /// The finance ledger entries that carry the day.
    pub journals: Vec<EntryId>,
    /// Empty when the books equal the core.
    pub differences: Vec<ControlDifference>,
}

/// A backdated entry or a reversal, kept by ID so a restated figure can be
/// walked back to the posting it reverses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackdatedItem {
    /// The period it was posted in.
    pub period: PeriodId,
    /// The earlier period its value date falls in, if it does.
    pub origin: Option<PeriodId>,
    pub entry: EntryId,
    pub kind: String,
    pub business_date: NaiveDate,
    pub value_date: NaiveDate,
    /// The entry it reverses.
    pub reverses: Option<String>,
    pub policy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootRecord {
    pub root: Hash32,
    pub reference: String,
}

/// Whether a date's control check is tied to the day's published root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Anchored,
    NoRoot,
    Mismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConfigItem {
    Chart(Chart),
    Close(CloseConfig),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Proposal {
    item: ConfigItem,
    by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Settings {
    partitions: PartitionId,
    year_end_month: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredResult {
    index: u64,
    error: Option<Reject>,
}

/// A line ready to post: debits positive, in fine units or micro-units.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    account: String,
    unit: BalanceFamily,
    dims: Dims,
    signed: i128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinanceState {
    settings: Option<Settings>,
    pub chart: Option<Chart>,
    pub close_config: Option<CloseConfig>,
    proposals: BTreeMap<String, Proposal>,
    pub ledger: Ledger,
    #[serde(with = "crate::pairs")]
    pub(crate) cube: BTreeMap<CubeKey, i128>,
    feeds: BTreeMap<PartitionId, Feed>,
    #[serde(with = "crate::pairs")]
    pub(crate) days: BTreeMap<(NaiveDate, PartitionId), DayRecord>,
    pub(crate) roots: BTreeMap<NaiveDate, RootRecord>,
    pub periods: BTreeMap<PeriodId, PeriodState>,
    pub journals: BTreeMap<String, Journal>,
    pub backdated: Vec<BackdatedItem>,
    pub aged: BTreeMap<String, AgedReport>,
    pub extracts: BTreeMap<String, ExtractRecord>,
    #[serde(default, with = "crate::pairs")]
    pub(crate) conduct: BTreeMap<ConductKey, u64>,
    /// The conduct facts counted in periods still open, by ID, so that one
    /// sent twice is counted once. A closed period takes no more facts, so
    /// its IDs are let go when it closes.
    #[serde(default)]
    pub(crate) conduct_seen: BTreeMap<String, PeriodId>,
    /// Every version of every return filed, oldest first.
    #[serde(default)]
    pub returns: BTreeMap<String, Vec<FiledReturn>>,
    next_event_seq: u64,
    /// Events delivered by the host and not yet taken into the books.
    #[serde(default)]
    inbox: BTreeMap<PartitionId, Vec<EventRecord>>,
    /// Why each stalled partition's next event was refused.
    #[serde(default)]
    pub stalled: BTreeMap<PartitionId, Reject>,
    /// The book of policies and covers, for the extracts. Fed from the same
    /// events as the books, and needs no chart to take them.
    #[serde(default)]
    pub mart: crate::mart::Mart,
    /// Everything the core posted to each account in each period, debits
    /// and credits added together. The books hold a day's entries netted, so
    /// this is the only record that an amount went through an account and
    /// came back out.
    #[serde(default, with = "crate::pairs")]
    pub(crate) turnover: BTreeMap<(PeriodId, String, BalanceFamily), i128>,
    /// Every rate table version the core has activated or recorded a
    /// validation for, by table, version and content hash.
    #[serde(default, with = "crate::pairs")]
    pub(crate) rate_tables: BTreeMap<(String, u32, String), TableSeen>,
    /// Each policy's accounting group, set once when it is issued and never
    /// changed.
    #[serde(default, with = "crate::pairs")]
    pub accounting_groups: BTreeMap<(PartitionId, PolicyId), AccountingGroup>,
    /// Policies issued when no rule named their product, until classified.
    #[serde(default, with = "crate::pairs")]
    pub(crate) ungrouped: BTreeMap<(PartitionId, PolicyId), Ungrouped>,
    idempotency: BTreeMap<String, StoredResult>,
}

/// The group of insurance contracts a policy belongs to: its portfolio,
/// the financial year it was issued in, and its profitability band.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingGroup {
    pub portfolio: String,
    /// The financial year of inception, named by the year it ends in: "FY2028".
    pub cohort: String,
    pub profitability: String,
    /// The version of the close configuration whose rule placed it.
    pub rule_version: u32,
    /// The business date it was placed.
    pub assigned_on: NaiveDate,
}

impl AccountingGroup {
    /// The group's name, as a dimension: `portfolio/cohort/profitability`.
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.portfolio, self.cohort, self.profitability)
    }
}

/// A policy waiting for a rule that names its product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ungrouped {
    pub product: String,
    pub inception: NaiveDate,
    pub issued_on: NaiveDate,
}

/// What the core has said of one version of a rate table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSeen {
    /// The first business date a partition activated it on.
    pub activated: Option<NaiveDate>,
    /// Whether a validation of this content has been recorded.
    pub validated: bool,
}

impl Default for FinanceState {
    fn default() -> Self {
        Self::new()
    }
}

struct Ctx<'a> {
    index: u64,
    key: &'a str,
    caller: &'a str,
    actor: Option<&'a str>,
    roles: &'a [String],
    events: Vec<FinEvent>,
    entries: Vec<JournalEntry>,
}

impl Ctx<'_> {
    /// The person acting, who must hold one of the roles.
    fn actor_with(&self, roles: &[&str]) -> Result<String, Reject> {
        let actor = self.actor.ok_or_else(|| Reject::new("actor", "this command needs the person acting"))?;
        if !roles.iter().any(|r| self.roles.iter().any(|held| held == r)) {
            return Err(Reject::new("authority", format!("this needs one of the roles: {}", roles.join(", "))));
        }
        Ok(actor.to_string())
    }
}

fn reject<T>(code: &str, message: impl Into<String>) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

fn family_name(unit: &BalanceFamily) -> String {
    match unit {
        BalanceFamily::Currency(c) => c.to_string(),
        BalanceFamily::Fund(f) => format!("units/{}", f.0),
    }
}

fn amount_of(unit: &BalanceFamily, abs: i128) -> Result<Amount, Reject> {
    Ok(match unit {
        BalanceFamily::Currency(c) => Amount::Accrual(AccrualAmount::new(abs, *c)),
        BalanceFamily::Fund(f) => Amount::Units(UnitQty::new(
            i64::try_from(abs).map_err(|_| Reject::new("overflow", "unit amount too large"))?,
            f.clone(),
        )),
    })
}

impl FinanceState {
    pub fn new() -> Self {
        Self {
            settings: None,
            chart: None,
            close_config: None,
            proposals: BTreeMap::new(),
            ledger: Ledger::new(FINANCE_PARTITION),
            cube: BTreeMap::new(),
            feeds: BTreeMap::new(),
            days: BTreeMap::new(),
            roots: BTreeMap::new(),
            periods: BTreeMap::new(),
            journals: BTreeMap::new(),
            backdated: Vec::new(),
            aged: BTreeMap::new(),
            extracts: BTreeMap::new(),
            conduct: BTreeMap::new(),
            conduct_seen: BTreeMap::new(),
            returns: BTreeMap::new(),
            next_event_seq: 1,
            inbox: BTreeMap::new(),
            stalled: BTreeMap::new(),
            mart: crate::mart::Mart::default(),
            turnover: BTreeMap::new(),
            rate_tables: BTreeMap::new(),
            accounting_groups: BTreeMap::new(),
            ungrouped: BTreeMap::new(),
            idempotency: BTreeMap::new(),
        }
    }

    /// Hash of the whole replicated state. Two replicas that applied the
    /// same log must agree on it exactly.
    pub fn state_hash(&self) -> Hash32 {
        let mut h = Sha256::new();
        h.update(b"ic.finance.state.v1");
        // Every map in the state has string or integer keys, or is saved as
        // pairs, so this encoding is canonical and cannot fail.
        h.update(serde_json::to_vec(self).unwrap_or_else(|_| b"unencodable".to_vec()));
        Hash32(h.finalize().into())
    }

    pub fn partitions(&self) -> PartitionId {
        self.settings.as_ref().map_or(0, |s| s.partitions)
    }

    pub fn year_end_month(&self) -> u32 {
        self.settings.as_ref().map_or(12, |s| s.year_end_month)
    }

    pub fn last_event_seq(&self) -> u64 {
        self.next_event_seq - 1
    }

    /// The next event of a partition's stream the feed should send.
    pub fn next_event_seq(&self, partition: PartitionId) -> u64 {
        self.feeds.get(&partition).map_or(1, |f| f.next_event_seq)
    }

    /// The first business date received from the core.
    pub fn first_date(&self) -> Option<NaiveDate> {
        self.days.keys().next().map(|(date, _)| *date)
    }

    /// The latest business date closed on any partition.
    pub fn last_date(&self) -> Option<NaiveDate> {
        self.days.keys().next_back().map(|(date, _)| *date)
    }

    pub fn day(&self, date: NaiveDate, partition: PartitionId) -> Option<&DayRecord> {
        self.days.get(&(date, partition))
    }

    /// Whether the chain heads checked here for a date produce the Merkle
    /// root published for it. Call only once every partition has the date.
    pub fn anchor(&self, date: NaiveDate) -> Anchor {
        let Some(record) = self.roots.get(&date) else {
            return Anchor::NoRoot;
        };
        let heads: BTreeMap<PartitionId, Hash32> = self
            .days
            .range((date, 0)..=(date, PartitionId::MAX))
            .map(|((_, p), d)| (*p, d.closing_head))
            .collect();
        if heads.len() == usize::from(self.partitions()) && merkle_root(date, &heads) == record.root {
            Anchor::Anchored
        } else {
            Anchor::Mismatch
        }
    }

    pub fn apply_envelope(&mut self, index: u64, env: FinEnvelope) -> FinApplied {
        // The feed is idempotent by its own cursor, so its keys are not kept:
        // there would be one for every batch of events for ever.
        let keyed = !matches!(env.command, FinCommand::Ingest { .. } | FinCommand::CoreEvent { .. });
        if keyed {
            if let Some(stored) = self.idempotency.get(&env.key) {
                return FinApplied {
                    index: stored.index,
                    result: stored.error.clone().map_or(Ok(()), Err),
                    events: vec![],
                    entries: vec![],
                    duplicate: true,
                };
            }
        }
        let mut ctx = Ctx {
            index,
            key: &env.key,
            caller: &env.caller,
            actor: env.actor.as_deref(),
            roles: &env.roles,
            events: vec![],
            entries: vec![],
        };
        let allowed = env.command.allowed_callers();
        let result = if !allowed.is_empty() && !allowed.contains(&env.caller.as_str()) {
            reject("caller", format!("{} may not send {}", env.caller, env.command.name()))
        } else {
            self.dispatch(&mut ctx, env.command)
        };
        let Ctx { events, entries, .. } = ctx;
        debug_assert!(result.is_ok() || (events.is_empty() && entries.is_empty()), "a refused command left a trace");
        if keyed {
            self.idempotency.insert(
                env.key,
                StoredResult {
                    index,
                    error: result.clone().err(),
                },
            );
        }
        let events = events
            .into_iter()
            .map(|event| {
                let seq = self.next_event_seq;
                self.next_event_seq += 1;
                FinEventRecord { seq, log_index: index, event }
            })
            .collect();
        FinApplied {
            index,
            result,
            events,
            entries,
            duplicate: false,
        }
    }

    fn dispatch(&mut self, ctx: &mut Ctx<'_>, command: FinCommand) -> Result<(), Reject> {
        match command {
            FinCommand::Configure { partitions, year_end_month } => {
                if self.settings.is_some() {
                    return reject("configured", "the finance ledger is already configured");
                }
                if partitions == 0 || !(1..=12).contains(&year_end_month) {
                    return reject("settings", "needs at least one partition and a year-end month from 1 to 12");
                }
                self.settings = Some(Settings { partitions, year_end_month });
                self.mart.set_partitions(partitions);
                ctx.events.push(FinEvent::Configured { partitions, year_end_month });
                self.drain_all(ctx);
                Ok(())
            }
            FinCommand::ProposeChart { chart } => {
                chart.validate().map_err(|e| Reject::new("chart", e.to_string()))?;
                if let Some(current) = &self.chart {
                    chart.may_replace(current).map_err(|e| Reject::new("chart", e.to_string()))?;
                }
                let id = format!("chart:{}:{}", chart.version, chart.content_hash);
                self.propose(ctx, id, ConfigItem::Chart(chart))
            }
            FinCommand::ProposeCloseConfig { config } => {
                config.validate().map_err(|e| Reject::new("close_config", e.to_string()))?;
                if self.close_config.as_ref().is_some_and(|c| config.version != c.version + 1) {
                    return reject("close_config", "a close configuration must be the next version");
                }
                let id = format!("close:{}:{}", config.version, config.content_hash);
                self.propose(ctx, id, ConfigItem::Close(config))
            }
            FinCommand::ApproveConfig { id } => {
                self.approve_config(ctx, &id)?;
                // A new chart may place the role the feed was waiting on.
                self.drain_all(ctx);
                Ok(())
            }
            FinCommand::CoreEvent { record } => {
                let partition = record.partition;
                let queue = self.inbox.get(&partition);
                let expected = queue.and_then(|q| q.last()).map_or(self.next_event_seq(partition), |last| last.seq + 1);
                if record.seq < expected {
                    return Ok(());
                }
                if record.seq > expected {
                    return reject("gap", format!("expected event {expected} of partition {partition}, got {}", record.seq));
                }
                // The mart takes the event as it arrives: it needs no chart.
                self.mart.apply(&record).map_err(|e| Reject::new("mart", e.to_string()))?;
                self.group_on_issue(&record);
                self.inbox.entry(partition).or_default().push(*record);
                // While stalled, events queue until the cause is fixed.
                if !self.stalled.contains_key(&partition) {
                    self.drain(ctx, partition);
                }
                Ok(())
            }
            FinCommand::RetryFeed { partition } => {
                self.drain(ctx, partition);
                Ok(())
            }
            FinCommand::Ingest { partition, events } => {
                self.ingest(ctx, partition, events.clone())?;
                for record in &events {
                    // In step with the feed just checked: the order cannot be wrong here.
                    self.mart.apply(record).ok();
                    self.group_on_issue(record);
                }
                Ok(())
            }
            FinCommand::RecordMerkleRoot { date, root, reference } => {
                match self.roots.get(&date) {
                    Some(existing) if existing.root == root => return Ok(()),
                    Some(_) => return reject("root_conflict", format!("a different Merkle root is already recorded for {date}")),
                    None => {}
                }
                self.roots.insert(date, RootRecord { root, reference });
                ctx.events.push(FinEvent::MerkleRootRecorded {
                    date,
                    anchored: self.anchor(date) == Anchor::Anchored,
                });
                Ok(())
            }
            FinCommand::ProposeJournal { journal } => {
                let maker = ctx.actor_with(&[ROLE_OPERATOR, ROLE_LEAD])?;
                if journal.kind == JournalKind::YearEndClose {
                    return reject("journal", "the year-end closing journal is built by the ledger, not entered");
                }
                self.propose_journal(ctx, journal, maker)
            }
            FinCommand::ApproveJournal { id } => self.approve_journal(ctx, &id),
            FinCommand::RejectJournal { id, reason } => {
                let checker = ctx.actor_with(&[ROLE_APPROVER, ROLE_LEAD])?;
                let journal = self.proposed_journal(&id)?;
                if reason.trim().is_empty() {
                    return reject("journal", "a rejection needs a reason");
                }
                journal.status = JournalStatus::Rejected {
                    checker: checker.clone(),
                    reason,
                };
                ctx.events.push(FinEvent::JournalRejected { id, checker });
                Ok(())
            }
            FinCommand::SignActuarialJournal { id } => {
                let by = ctx.actor_with(&[ROLE_ACTUARY])?;
                let Some(journal) = self.journals.get_mut(&id) else {
                    return reject("journal", format!("there is no journal {id}"));
                };
                if journal.draft.kind != JournalKind::Actuarial || matches!(journal.status, JournalStatus::Rejected { .. }) {
                    return reject("journal", "only an actuarial journal that has not been rejected can be signed");
                }
                if journal.maker == by {
                    return reject("separation", "the actuary cannot sign a journal they entered");
                }
                if journal.actuary.is_none() {
                    journal.actuary = Some(by.clone());
                    ctx.events.push(FinEvent::ActuarialJournalSigned { id, by });
                }
                Ok(())
            }
            FinCommand::ProposeYearEndClose { id, year_end, evidence } => self.propose_year_end(ctx, id, year_end, evidence),
            FinCommand::ReportCheck {
                period,
                check,
                outcome,
                evidence,
            } => self.report_check(ctx, period, check, outcome, evidence),
            FinCommand::ReportAgedItems {
                kind,
                as_at,
                items,
                total_minor,
                count,
            } => {
                let Some(decl) = self.close_config.as_ref().and_then(|c| c.aged.iter().find(|a| a.kind == kind)) else {
                    return reject("aged", format!("the close configuration declares no aged items called {kind}"));
                };
                if decl.by != ctx.caller {
                    return reject("caller", format!("{kind} items are reported by {}, not {}", decl.by, ctx.caller));
                }
                if items.len() > MAX_AGED_ITEMS || items.iter().any(|i| i.since > as_at) {
                    return reject("aged", format!("a report lists at most {MAX_AGED_ITEMS} items, none held since after its date"));
                }
                if self.aged.get(&kind).is_some_and(|r| r.as_at > as_at) {
                    return reject("aged", "a later report is already recorded");
                }
                let index = ctx.index;
                // An action documented here against an item stays with it
                // while the same holding is reported again without one.
                let mut items = items;
                if let Some(earlier) = self.aged.get(&kind) {
                    for item in items.iter_mut().filter(|i| i.action.is_none()) {
                        if let Some(was) = earlier
                            .items
                            .iter()
                            .find(|e| e.party == item.party && e.since == item.since && e.action.is_some())
                        {
                            item.action = was.action.clone();
                            item.owner = was.owner.clone();
                        }
                    }
                }
                self.aged.insert(
                    kind.clone(),
                    AgedReport {
                        as_at,
                        items,
                        total_minor,
                        count,
                        index,
                    },
                );
                ctx.events.push(FinEvent::AgedItemsReported {
                    kind,
                    as_at,
                    total_minor,
                    count,
                });
                Ok(())
            }
            FinCommand::DocumentAgedItem {
                kind,
                party,
                since,
                action,
                owner,
            } => {
                let by = ctx.actor_with(&[ROLE_OPERATOR, ROLE_LEAD])?;
                if action.trim().is_empty() || owner.trim().is_empty() {
                    return reject("aged_action", "a documented action needs a reference and an owner");
                }
                let item = self
                    .aged
                    .get_mut(&kind)
                    .and_then(|r| r.items.iter_mut().find(|i| i.party == party && i.since == since));
                let Some(item) = item else {
                    return reject("aged_item", format!("the latest {kind} report lists nothing held for {party} since {since}"));
                };
                item.action = Some(action.clone());
                item.owner = Some(owner.clone());
                ctx.events.push(FinEvent::AgedItemDocumented {
                    kind,
                    party,
                    action,
                    owner,
                    by,
                });
                Ok(())
            }
            FinCommand::WaiveCheck {
                period,
                check,
                reason,
                remediation_owner,
                due,
            } => self.waive(ctx, period, check, reason, remediation_owner, due),
            FinCommand::SignOff { period, capacity } => self.sign_off(ctx, period, capacity),
            FinCommand::ClosePeriod { period } => self.close_period(ctx, period),
            FinCommand::RegisterExtract { extract } => {
                match self.extracts.get(&extract.id) {
                    Some(existing) if *existing == extract => return Ok(()),
                    Some(_) => return reject("extract_conflict", format!("extract {} is frozen and cannot be replaced", extract.id)),
                    None => {}
                }
                if extract.id.is_empty() || extract.kind.is_empty() || extract.location.is_empty() {
                    return reject("extract", "an extract needs an ID, a kind and a location");
                }
                // An extract is tied to the published root of its business date.
                if self.roots.get(&extract.as_at).map(|r| r.root) != Some(extract.merkle_root) {
                    return reject(
                        "extract_anchor",
                        format!("extract {} does not carry the Merkle root published for {}", extract.id, extract.as_at),
                    );
                }
                ctx.events.push(FinEvent::ExtractRegistered {
                    id: extract.id.clone(),
                    kind: extract.kind.clone(),
                    as_at: extract.as_at,
                    content_hash: extract.content_hash,
                });
                self.extracts.insert(extract.id.clone(), extract);
                Ok(())
            }
            FinCommand::RecordConductFacts { facts } => {
                let count = self.record_conduct(ctx.caller, facts)?;
                if count > 0 {
                    ctx.events.push(FinEvent::ConductFactsRecorded { count });
                }
                Ok(())
            }
            FinCommand::FileReturn { id, definition, period } => {
                let by = ctx.actor_with(&[ROLE_OPERATOR, ROLE_LEAD])?;
                let filed = self.file_return(&id, &definition, period, by, None, ctx.index)?;
                let (version, hash) = (filed.version, filed.hash);
                ctx.events.push(FinEvent::ReturnFiled { id, period, version, hash });
                Ok(())
            }
            FinCommand::RestateReturn { id, definition, reason } => {
                let by = ctx.actor_with(&[ROLE_LEAD])?;
                let Some(period) = self.returns.get(&id).and_then(|v| v.last()).map(|r| r.period) else {
                    return reject("return", format!("return {id} has not been filed, so it cannot be restated"));
                };
                let filed = self.file_return(&id, &definition, period, by, Some(reason), ctx.index)?;
                let (version, hash) = (filed.version, filed.hash);
                ctx.events.push(FinEvent::ReturnFiled { id, period, version, hash });
                Ok(())
            }
            FinCommand::ClassifyPolicies => {
                ctx.actor_with(&[ROLE_OPERATOR, ROLE_LEAD])?;
                let Some(rule_version) = self.close_config.as_ref().map(|c| c.version) else {
                    return reject("close_config", "no close configuration is in force, so there are no grouping rules");
                };
                let waiting: Vec<_> = self.ungrouped.keys().cloned().collect();
                let mut placed = 0u64;
                for key in waiting {
                    let Some(u) = self.ungrouped.get(&key).cloned() else { continue };
                    let today = self.mart.closed_through().map_or(u.issued_on, |d| d.max(u.issued_on));
                    if let Some(group) = self.group_for(&u.product, u.inception, today) {
                        self.ungrouped.remove(&key);
                        self.accounting_groups.insert(key, group);
                        placed += 1;
                    }
                }
                ctx.events.push(FinEvent::PoliciesClassified {
                    placed,
                    still_ungrouped: self.ungrouped.len() as u64,
                    rule_version,
                });
                Ok(())
            }
        }
    }

    // ---- accounting groups ---------------------------------------------

    /// The group the rules now in force give a policy of `product` that
    /// incepted on `inception`, or none when no rule names the product.
    fn group_for(&self, product: &str, inception: NaiveDate, today: NaiveDate) -> Option<AccountingGroup> {
        let config = self.close_config.as_ref()?;
        let rule = config.grouping.iter().find(|g| g.product == product)?;
        let year_end_month = self.year_end_month();
        // The financial year is named by the calendar year it ends in.
        let ends_in = if inception.month() > year_end_month {
            inception.year() + 1
        } else {
            inception.year()
        };
        Some(AccountingGroup {
            portfolio: rule.portfolio.clone(),
            cohort: format!("FY{ends_in}"),
            profitability: rule.profitability.clone(),
            rule_version: config.version,
            assigned_on: today,
        })
    }

    /// Place a policy in its accounting group on the event that issues it.
    /// It is placed once: a policy already placed or waiting is left alone.
    fn group_on_issue(&mut self, record: &EventRecord) {
        let Event::PolicyIssued {
            policy,
            product_code,
            inception,
            ..
        } = &record.event
        else {
            return;
        };
        let key = (record.partition, policy.clone());
        if self.accounting_groups.contains_key(&key) || self.ungrouped.contains_key(&key) {
            return;
        }
        match self.group_for(product_code, *inception, record.business_date) {
            Some(group) => {
                self.accounting_groups.insert(key, group);
            }
            None => {
                self.ungrouped.insert(
                    key,
                    Ungrouped {
                        product: product_code.clone(),
                        inception: *inception,
                        issued_on: record.business_date,
                    },
                );
            }
        }
    }

    /// Policies by accounting group, and how many have none.
    pub fn accounting_group_counts(&self) -> serde_json::Value {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for g in self.accounting_groups.values() {
            *counts.entry(g.id()).or_insert(0) += 1;
        }
        let groups: Vec<serde_json::Value> = counts
            .into_iter()
            .map(|(group, policies)| serde_json::json!({ "group": group, "policies": policies }))
            .collect();
        serde_json::json!({ "groups": groups, "ungrouped": self.ungrouped.len() })
    }

    // ---- configuration -------------------------------------------------

    fn propose(&mut self, ctx: &mut Ctx<'_>, id: String, item: ConfigItem) -> Result<(), Reject> {
        let by = ctx.actor_with(&[ROLE_OPERATOR, ROLE_LEAD])?;
        self.proposals.insert(id.clone(), Proposal { item, by: by.clone() });
        ctx.events.push(FinEvent::ConfigProposed { id, by });
        Ok(())
    }

    fn approve_config(&mut self, ctx: &mut Ctx<'_>, id: &str) -> Result<(), Reject> {
        let approver = ctx.actor_with(&[ROLE_LEAD])?;
        let Some(proposal) = self.proposals.get(id) else {
            return reject("config", format!("nothing is proposed under {id}"));
        };
        if proposal.by == approver {
            return reject("separation", "a configuration change cannot be approved by the person who proposed it");
        }
        // Something else may have been activated since this was proposed.
        match &proposal.item {
            ConfigItem::Chart(chart) => {
                if let Some(current) = &self.chart {
                    chart.may_replace(current).map_err(|e| Reject::new("chart", e.to_string()))?;
                }
            }
            ConfigItem::Close(config) => {
                if self.close_config.as_ref().is_some_and(|c| config.version != c.version + 1) {
                    return reject("close_config", "a close configuration must be the next version");
                }
            }
        }
        if let Some(proposal) = self.proposals.remove(id) {
            match proposal.item {
                ConfigItem::Chart(chart) => {
                    ctx.events.push(FinEvent::ChartActivated {
                        version: chart.version,
                        content_hash: chart.content_hash.clone(),
                    });
                    self.chart = Some(chart);
                }
                ConfigItem::Close(config) => {
                    ctx.events.push(FinEvent::CloseConfigActivated {
                        version: config.version,
                        content_hash: config.content_hash.clone(),
                    });
                    self.close_config = Some(config);
                }
            }
        }
        Ok(())
    }

    // ---- posting -------------------------------------------------------

    /// Post balanced lines to a ledger and return the entry with the lines
    /// as posted, for the cube. Lines to the same account, unit and
    /// dimensions are netted; `None` if everything nets to nothing.
    fn post_lines(
        chart: &Chart,
        ledger: &mut Ledger,
        kind: &str,
        business_date: NaiveDate,
        value_date: NaiveDate,
        lines: &[Line],
        detail: BTreeMap<String, String>,
        caller: &str,
        salt: &str,
    ) -> Result<Option<(JournalEntry, Vec<Line>)>, Reject> {
        let mut netted: BTreeMap<(String, BalanceFamily, Dims), i128> = BTreeMap::new();
        for line in lines {
            let cell = netted.entry((line.account.clone(), line.unit.clone(), line.dims.clone())).or_insert(0);
            *cell = cell.checked_add(line.signed).ok_or_else(|| Reject::new("overflow", "amount too large"))?;
        }
        let mut postings = Vec::new();
        let mut posted = Vec::new();
        for ((code, unit, dims), signed) in netted {
            if signed == 0 {
                continue;
            }
            let Some(account) = chart.account(&code) else {
                return reject("account", format!("the chart has no account {code}"));
            };
            let id = AccountId(format!("{code}/{}", family_name(&unit)));
            ledger.open_account(Account {
                id: id.clone(),
                unit: match &unit {
                    BalanceFamily::Currency(c) => AccountUnit::Fine(*c),
                    BalanceFamily::Fund(f) => AccountUnit::Units(f.clone()),
                },
                class: account.class,
                role: code.clone(),
            })?;
            postings.push(Posting {
                account: id,
                side: if signed > 0 { Side::Debit } else { Side::Credit },
                amount: amount_of(&unit, signed.abs())?,
                dims: (&dims).into(),
            });
            posted.push(Line {
                account: code,
                unit,
                dims,
                signed,
            });
        }
        if postings.is_empty() {
            return Ok(None);
        }
        let draft = EntryDraft {
            kind: kind.to_string(),
            value_date,
            postings,
            product_hash: None,
            caller: caller.to_string(),
            policy: None,
            detail,
        };
        let entry = ledger.post(draft, business_date, salt)?;
        Ok(Some((entry, posted)))
    }

    fn add_to_cube(&mut self, period: PeriodId, origin: Option<PeriodId>, lines: Vec<Line>) {
        for line in lines {
            *self
                .cube
                .entry(CubeKey {
                    period,
                    account: line.account,
                    unit: line.unit,
                    dims: line.dims,
                    origin,
                })
                .or_insert(0) += line.signed;
        }
    }

    /// The business date a posting made now is recorded under: the latest
    /// day closed on any partition, or the period's last day before any has.
    fn recording_date(&self, period: PeriodId) -> NaiveDate {
        self.last_date().unwrap_or(period.end())
    }

    // ---- the feed ------------------------------------------------------

    /// How many events the host has delivered that the books have not yet taken.
    pub fn held_events(&self, partition: PartitionId) -> usize {
        self.inbox.get(&partition).map_or(0, Vec::len)
    }

    fn drain_all(&mut self, ctx: &mut Ctx<'_>) {
        for partition in self.inbox.keys().copied().collect::<Vec<_>>() {
            self.drain(ctx, partition);
        }
    }

    /// Take the events held for a partition into the books, in order, as
    /// far as they will go. What cannot be taken stays held.
    fn drain(&mut self, ctx: &mut Ctx<'_>, partition: PartitionId) {
        let mut held = self.inbox.remove(&partition).unwrap_or_default();
        let mut failure = None;
        while !held.is_empty() {
            let take = held.len().min(MAX_INGEST_EVENTS);
            match self.ingest(ctx, partition, held[..take].to_vec()) {
                Ok(()) => {
                    held.drain(..take);
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        if !held.is_empty() {
            self.inbox.insert(partition, held);
        }
        match failure {
            Some(e) => {
                if self.stalled.get(&partition).map(|was| &was.code) != Some(&e.code) {
                    ctx.events.push(FinEvent::FeedStalled {
                        partition,
                        code: e.code.clone(),
                    });
                }
                self.stalled.insert(partition, e);
            }
            None => {
                if self.stalled.remove(&partition).is_some() {
                    ctx.events.push(FinEvent::FeedResumed { partition });
                }
            }
        }
    }

    fn ingest(&mut self, ctx: &mut Ctx<'_>, partition: PartitionId, events: Vec<EventRecord>) -> Result<(), Reject> {
        let Some(chart) = &self.chart else {
            return reject("chart", "no chart of accounts is active");
        };
        if partition >= self.partitions() {
            return reject("partition", format!("there is no partition {partition}"));
        }
        if events.len() > MAX_INGEST_EVENTS {
            return reject("batch", format!("an ingest carries at most {MAX_INGEST_EVENTS} events"));
        }
        struct Close {
            date: NaiveDate,
            head: Hash32,
            first_seq: Option<u64>,
            last_seq: Option<u64>,
            pending: BTreeMap<PendingKey, i128>,
            differences: Vec<ControlDifference>,
        }
        let mut feed = self.feeds.get(&partition).cloned().unwrap_or_else(|| Feed::new(partition));
        let mut closes: Vec<Close> = Vec::new();
        let mut backdated: Vec<BackdatedItem> = Vec::new();
        let mut turnover: Vec<((PeriodId, String, BalanceFamily), i128)> = Vec::new();
        let mut tables: Vec<((String, u32, String), Option<NaiveDate>)> = Vec::new();
        for record in &events {
            if record.partition != partition {
                return reject("partition", format!("event {} is not from partition {partition}", record.id));
            }
            if record.seq < feed.next_event_seq {
                continue;
            }
            if record.seq > feed.next_event_seq {
                return reject(
                    "gap",
                    format!("expected event {} of partition {partition}, got {}", feed.next_event_seq, record.seq),
                );
            }
            feed.next_event_seq += 1;
            if feed.awaiting.is_some() && !matches!(record.event, Event::ControlTotals { .. }) {
                return reject(
                    "control_missing",
                    format!("partition {partition} closed a day without publishing its control totals"),
                );
            }
            match &record.event {
                Event::BusinessDateAdvanced { from, to, closing_head } => {
                    if *from != feed.date || *closing_head != feed.head {
                        return reject(
                            "head_mismatch",
                            format!(
                                "partition {partition} closed {from:?} at {closing_head}, but the entries received end at {} on {:?}",
                                feed.head, feed.date
                            ),
                        );
                    }
                    feed.awaiting = *from;
                    feed.previous = *from;
                    feed.date = Some(*to);
                }
                Event::EntryPosted {
                    entry,
                    kind,
                    policy,
                    hash,
                    value_date,
                    reverses,
                    lines,
                } => {
                    if entry.partition != partition || entry.seq != feed.next_entry_seq {
                        return reject(
                            "entry_gap",
                            format!("expected entry {} of partition {partition}, got {entry}", feed.next_entry_seq),
                        );
                    }
                    if Some(record.business_date) != feed.date {
                        return reject(
                            "date_mismatch",
                            format!("entry {entry} is dated {} but the partition is on {:?}", record.business_date, feed.date),
                        );
                    }
                    let (Some(value_date), false) = (*value_date, lines.is_empty()) else {
                        return reject("no_lines", format!("entry {entry} was published without its value date and lines"));
                    };
                    let period = PeriodId::of(record.business_date);
                    // The end-of-day batch of the date just closed is the new
                    // date's own activity, at month end as on any other day.
                    // Anything dated before that is backdated.
                    let routine = feed.previous.is_some_and(|previous| value_date >= previous);
                    let origin = Some(PeriodId::of(value_date)).filter(|o| *o < period && !routine);
                    for line in lines {
                        let Some(account) = chart.account_for_role(&line.role) else {
                            return reject("unmapped_role", format!("the chart maps no account for role {} (entry {entry})", line.role));
                        };
                        if account.class != line.class {
                            return reject(
                                "class_mismatch",
                                format!(
                                    "role {} is {:?} in the core and {:?} on account {}",
                                    line.role, line.class, account.class, account.code
                                ),
                            );
                        }
                        let overflow = || Reject::new("overflow", "amount too large");
                        let amount = match &line.unit {
                            AccountUnit::Minor(_) => line.signed.checked_mul(FINE_PER_MINOR).ok_or_else(overflow)?,
                            AccountUnit::Fine(_) | AccountUnit::Units(_) => line.signed,
                        };
                        turnover.push(((period, account.code.clone(), line.unit.family()), amount.saturating_abs()));
                        let key = PendingKey {
                            origin,
                            account: account.code.clone(),
                            unit: line.unit.family(),
                            dims: (&line.dims).into(),
                        };
                        let cell = feed.pending.entry(key).or_insert(0);
                        *cell = cell.checked_add(amount).ok_or_else(overflow)?;
                        let total = feed.roles.entry((line.role.clone(), line.class, line.unit.clone())).or_insert(0);
                        *total = total.checked_add(line.signed).ok_or_else(overflow)?;
                    }
                    if origin.is_some() || reverses.is_some() {
                        backdated.push(BackdatedItem {
                            period,
                            origin,
                            entry: *entry,
                            kind: kind.clone(),
                            business_date: record.business_date,
                            value_date,
                            reverses: reverses.clone(),
                            policy: policy.as_ref().map(|p| p.0.clone()),
                        });
                    }
                    feed.day_first_seq.get_or_insert(entry.seq);
                    feed.next_entry_seq += 1;
                    feed.head = *hash;
                }
                Event::ControlTotals { date, closing_head, totals } => {
                    if feed.awaiting != Some(*date) || *closing_head != feed.head {
                        return reject(
                            "control_mismatch",
                            format!("control totals for {date} at {closing_head} do not follow that day's close"),
                        );
                    }
                    feed.awaiting = None;
                    let first_seq = feed.day_first_seq.take();
                    closes.push(Close {
                        date: *date,
                        head: feed.head,
                        first_seq,
                        last_seq: first_seq.map(|_| feed.next_entry_seq - 1),
                        pending: std::mem::take(&mut feed.pending),
                        differences: differences(totals, &feed.roles),
                    });
                }
                Event::RateTableActivated { id, version, content_hash, .. } => {
                    tables.push(((id.clone(), *version, content_hash.clone()), Some(record.business_date)))
                }
                Event::RateTableValidated { id, version, content_hash, .. } => tables.push(((id.clone(), *version, content_hash.clone()), None)),
                _ => {}
            }
        }

        // Everything is checked on a copy of the ledger before anything is kept.
        let mut ledger = self.ledger.clone();
        let mut posted: Vec<(NaiveDate, DayRecord, Vec<(Option<PeriodId>, JournalEntry, Vec<Line>)>)> = Vec::new();
        for close in closes {
            let period = PeriodId::of(close.date);
            if self.periods.get(&period).is_some_and(|p| p.status == PeriodStatus::Closed) {
                return reject(
                    "period_closed",
                    format!("{} is closed; partition {partition} cannot post {} into it", period, close.date),
                );
            }
            let mut groups: BTreeMap<Option<PeriodId>, Vec<Line>> = BTreeMap::new();
            for (key, signed) in close.pending {
                groups.entry(key.origin).or_default().push(Line {
                    account: key.account,
                    unit: key.unit,
                    dims: key.dims,
                    signed,
                });
            }
            let mut journals = Vec::new();
            for (origin, lines) in groups {
                let mut detail = BTreeMap::new();
                detail.insert("partition".to_string(), partition.to_string());
                detail.insert("business_date".to_string(), close.date.to_string());
                detail.insert("closing_head".to_string(), close.head.to_hex());
                if let (Some(first), Some(last)) = (close.first_seq, close.last_seq) {
                    detail.insert("entries".to_string(), format!("{first}-{last}"));
                }
                if let Some(origin) = origin {
                    detail.insert("origin_period".to_string(), origin.to_string());
                }
                let kind = if origin.is_some() { "core_day_prior_period" } else { "core_day" };
                // A prior-period journal takes effect on the last day of the period it belongs to.
                let value_date = origin.map_or(close.date, PeriodId::end);
                let salt = format!("feed:{partition}:{}:{}", close.date, journals.len());
                if let Some((entry, lines)) = Self::post_lines(chart, &mut ledger, kind, close.date, value_date, &lines, detail, FEED, &salt)
                    .map_err(|e| Reject::new("feed_unbalanced", format!("partition {partition} on {}: {}", close.date, e.message)))?
                {
                    journals.push((origin, entry, lines));
                }
            }
            let record = DayRecord {
                closing_head: close.head,
                first_seq: close.first_seq,
                last_seq: close.last_seq,
                journals: journals.iter().map(|(_, e, _)| e.id).collect(),
                differences: close.differences,
            };
            posted.push((close.date, record, journals));
        }

        self.ledger = ledger;
        self.feeds.insert(partition, feed);
        for (key, activated) in tables {
            let seen = self.rate_tables.entry(key).or_default();
            match activated {
                // Every partition activates a table: the first date stands.
                Some(date) => seen.activated = Some(seen.activated.map_or(date, |first| first.min(date))),
                None => seen.validated = true,
            }
        }
        for (key, amount) in turnover {
            let cell = self.turnover.entry(key).or_insert(0);
            *cell = cell.saturating_add(amount);
        }
        for item in backdated {
            ctx.events.push(FinEvent::BackdatedEntryReceived {
                entry: item.entry,
                period: item.period,
                origin: item.origin,
            });
            self.backdated.push(item);
        }
        for (date, record, journals) in posted {
            let period = PeriodId::of(date);
            ctx.events.push(FinEvent::DayControlled {
                partition,
                date,
                equal: record.differences.is_empty(),
                entries: record.first_seq.zip(record.last_seq).map_or(0, |(f, l)| l - f + 1),
            });
            self.days.insert((date, partition), record);
            // No sign-off can exist yet: a period is signed only after its
            // last day has closed on every partition.
            for (origin, entry, lines) in journals {
                self.add_to_cube(period, origin, lines);
                ctx.entries.push(entry);
            }
            let state = self.periods.entry(period).or_insert_with(|| PeriodState::new(PeriodStatus::Open));
            // The period moves to closing once its last day has closed everywhere.
            if date == period.end()
                && state.status == PeriodStatus::Open
                && (0..self.settings.as_ref().map_or(0, |s| s.partitions)).all(|p| self.days.contains_key(&(date, p)))
            {
                state.status = PeriodStatus::Closing;
                ctx.events.push(FinEvent::PeriodClosing { period });
            }
        }
        Ok(())
    }

    /// A posting landed in the period: any sign-off given before it no
    /// longer covers the books. Returns whether there was one.
    fn clear_sign_offs(&mut self, period: PeriodId) -> bool {
        match self.periods.get_mut(&period) {
            Some(state) if !state.sign_offs.is_empty() => {
                state.sign_offs.clear();
                true
            }
            _ => false,
        }
    }

    // ---- manual journals -----------------------------------------------

    fn proposed_journal(&mut self, id: &str) -> Result<&mut Journal, Reject> {
        match self.journals.get_mut(id) {
            Some(journal) if journal.status == JournalStatus::Proposed => Ok(journal),
            Some(_) => reject("journal", format!("journal {id} has already been decided")),
            None => reject("journal", format!("there is no journal {id}")),
        }
    }

    /// The period a journal may post in: one that exists and is not closed,
    /// or the adjustment period of a year-end month that has closed.
    fn journal_period(&self, period: PeriodId) -> Result<(), Reject> {
        match self.periods.get(&period).map(|p| p.status) {
            Some(PeriodStatus::Closed) => reject(
                "period_closed",
                format!("{period} is closed; post the correction in the current period with its original value date"),
            ),
            Some(_) => Ok(()),
            None if period.is_adjustment() => {
                let month = PeriodId::of(period.end());
                let closed = self.periods.get(&month).is_some_and(|p| p.status == PeriodStatus::Closed);
                if closed && month.is_year_end(self.year_end_month()) {
                    Ok(())
                } else {
                    reject("period", format!("{period} opens only once the year-end month {month} has closed"))
                }
            }
            None => reject("period", format!("{period} has not started")),
        }
    }

    fn propose_journal(&mut self, ctx: &mut Ctx<'_>, journal: JournalDraft, maker: String) -> Result<(), Reject> {
        let Some(chart) = &self.chart else {
            return reject("chart", "no chart of accounts is active");
        };
        let system = journal.kind == JournalKind::YearEndClose;
        if journal.id.is_empty() || self.journals.contains_key(&journal.id) {
            return reject("journal", format!("journal ID `{}` is empty or already used", journal.id));
        }
        self.journal_period(journal.period)?;
        if journal.reason.trim().is_empty() || journal.evidence.is_empty() || journal.evidence.iter().any(|e| e.trim().is_empty()) {
            return reject("journal", "a journal needs a reason and a reference to its supporting evidence");
        }
        if journal.lines.len() < 2 || (!system && journal.lines.len() > MAX_JOURNAL_LINES) {
            return reject("journal", format!("a journal has between 2 and {MAX_JOURNAL_LINES} lines"));
        }
        if journal.value_date > journal.period.end() {
            return reject("journal", "a journal cannot take effect after the period it posts in");
        }
        let mut by_currency: BTreeMap<Currency, i128> = BTreeMap::new();
        let mut by_fund: BTreeMap<(Option<&str>, Currency), i128> = BTreeMap::new();
        for line in &journal.lines {
            let Some(account) = chart.account(&line.account) else {
                return reject("account", format!("the chart has no account {}", line.account));
            };
            if account.control && !system {
                return reject(
                    "locked",
                    format!("account {} is a control account: only the feed from the core posts to it", account.code),
                );
            }
            if line.signed_fine == 0 {
                return reject("journal", "a journal line cannot be zero");
            }
            let overflow = || Reject::new("overflow", "amount too large");
            let net = by_currency.entry(line.currency).or_insert(0);
            *net = net.checked_add(line.signed_fine).ok_or_else(overflow)?;
            *by_fund.entry((line.fund.as_deref(), line.currency)).or_insert(0) += line.signed_fine;
        }
        if let Some((currency, off)) = by_currency.iter().find(|(_, net)| **net != 0) {
            return reject("unbalanced", format!("the journal is off by {off} in {currency}"));
        }
        let funds: BTreeSet<Option<&str>> = journal.lines.iter().map(|l| l.fund.as_deref()).collect();
        // Money stays in its fund. Each fund's side of a journal balances on
        // its own, and only a fund transfer journal may touch more than one.
        if by_fund.values().any(|net| *net != 0) {
            return reject("fund", "the journal does not balance within each statutory fund");
        }
        if journal.kind == JournalKind::FundTransfer {
            if funds.len() < 2 || funds.contains(&None) {
                return reject("fund_transfer", "a transfer between funds names a fund on every line and at least two funds");
            }
        } else if funds.len() > 1 && !system {
            return reject(
                "fund",
                "a journal posts within one statutory fund; a movement between funds is a fund transfer journal",
            );
        }
        if journal.kind == JournalKind::Actuarial && !journal.extract_id.as_ref().is_some_and(|id| self.extracts.contains_key(id)) {
            return reject(
                "extract",
                "an actuarial journal carries the ID of the registered extract it was calculated from",
            );
        }
        ctx.events.push(FinEvent::JournalProposed {
            id: journal.id.clone(),
            kind: journal.kind,
            period: journal.period,
            maker: maker.clone(),
        });
        self.journals.insert(
            journal.id.clone(),
            Journal {
                draft: journal,
                maker,
                status: JournalStatus::Proposed,
                actuary: None,
            },
        );
        Ok(())
    }

    fn approve_journal(&mut self, ctx: &mut Ctx<'_>, id: &str) -> Result<(), Reject> {
        let checker = ctx.actor_with(&[ROLE_APPROVER, ROLE_LEAD])?;
        let journal = self.proposed_journal(id)?.clone();
        if journal.draft.kind == JournalKind::FundTransfer && !ctx.roles.iter().any(|r| r == ROLE_LEAD) {
            return reject("authority", "a transfer between statutory funds is approved by the finance lead");
        }
        if journal.maker == checker {
            return reject("separation", "the person who posts a journal does not approve it");
        }
        let Some(chart) = &self.chart else {
            return reject("chart", "no chart of accounts is active");
        };
        let draft = &journal.draft;
        self.journal_period(draft.period)?;
        let lines: Vec<Line> = draft
            .lines
            .iter()
            .map(|l| Line {
                account: l.account.clone(),
                unit: BalanceFamily::Currency(l.currency),
                dims: Dims {
                    fund: l.fund.clone(),
                    product: l.product.clone(),
                    cover_type: l.cover_type.clone(),
                    posting_type: l.posting_type.clone(),
                },
                signed: l.signed_fine,
            })
            .collect();
        let mut detail = BTreeMap::new();
        detail.insert("journal".to_string(), draft.id.clone());
        detail.insert("maker".to_string(), journal.maker.clone());
        detail.insert("checker".to_string(), checker.clone());
        detail.insert("period".to_string(), draft.period.to_string());
        detail.insert("evidence".to_string(), draft.evidence.join(" "));
        if let Some(extract) = &draft.extract_id {
            detail.insert("extract".to_string(), extract.clone());
        }
        let origin = Some(PeriodId::of(draft.value_date)).filter(|o| *o < draft.period && !draft.period.is_adjustment());
        if let Some(origin) = origin {
            detail.insert("origin_period".to_string(), origin.to_string());
        }
        let kind = format!(
            "journal_{}",
            serde_json::to_value(draft.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        );
        let business_date = self.recording_date(draft.period);
        let mut ledger = self.ledger.clone();
        let salt = format!("{}:journal", ctx.key);
        let Some((entry, posted)) = Self::post_lines(chart, &mut ledger, &kind, business_date, draft.value_date, &lines, detail, ctx.caller, &salt)? else {
            return reject("journal", "the journal nets to nothing on every account");
        };
        self.ledger = ledger;
        let period = draft.period;
        self.periods
            .entry(period)
            .or_insert_with(|| PeriodState::new(if period.is_adjustment() { PeriodStatus::Closing } else { PeriodStatus::Open }));
        let cleared = self.clear_sign_offs(period);
        self.add_to_cube(period, origin, posted);
        if let Some(j) = self.journals.get_mut(id) {
            j.status = JournalStatus::Approved {
                checker: checker.clone(),
                entry: entry.id,
            };
        }
        ctx.events.push(FinEvent::JournalApproved {
            id: id.to_string(),
            checker,
            entry: entry.id,
        });
        if cleared {
            ctx.events.push(FinEvent::SignOffsCleared { period });
        }
        ctx.entries.push(entry);
        Ok(())
    }

    fn propose_year_end(&mut self, ctx: &mut Ctx<'_>, id: String, year_end: PeriodId, evidence: Vec<String>) -> Result<(), Reject> {
        let maker = ctx.actor_with(&[ROLE_OPERATOR, ROLE_LEAD])?;
        let Some(chart) = &self.chart else {
            return reject("chart", "no chart of accounts is active");
        };
        if year_end.is_adjustment() || !year_end.is_year_end(self.year_end_month()) {
            return reject("period", format!("{year_end} is not the last month of a financial year"));
        }
        let adjustment = year_end.adjustment();
        let first = year_end.year_start(self.year_end_month());
        // Income and expense of the year, by every dimension, so each cell closes to nothing.
        let mut cells: BTreeMap<(String, Currency, Dims), i128> = BTreeMap::new();
        for (key, amount) in self.cube.range(
            CubeKey {
                period: first,
                account: String::new(),
                unit: BalanceFamily::Currency(Currency::NZD),
                dims: Dims::default(),
                origin: None,
            }..,
        ) {
            if key.period > adjustment {
                break;
            }
            let (Some(account), BalanceFamily::Currency(currency)) = (chart.account(&key.account), &key.unit) else {
                continue;
            };
            if matches!(account.class, AccountClass::Income | AccountClass::Expense) {
                *cells.entry((key.account.clone(), *currency, key.dims.clone())).or_insert(0) += amount;
            }
        }
        let mut lines = Vec::new();
        let mut retained: BTreeMap<(Currency, Option<String>), i128> = BTreeMap::new();
        for ((account, currency, dims), balance) in cells {
            if balance == 0 {
                continue;
            }
            *retained.entry((currency, dims.fund.clone())).or_insert(0) += balance;
            lines.push(JournalLine {
                account,
                currency,
                signed_fine: -balance,
                fund: dims.fund,
                product: dims.product,
                cover_type: dims.cover_type,
                posting_type: dims.posting_type,
            });
        }
        for ((currency, fund), balance) in retained {
            if balance != 0 {
                lines.push(JournalLine {
                    account: chart.retained_earnings.clone(),
                    currency,
                    signed_fine: balance,
                    fund,
                    product: None,
                    cover_type: None,
                    posting_type: Some("year_end_close".to_string()),
                });
            }
        }
        let draft = JournalDraft {
            id,
            kind: JournalKind::YearEndClose,
            period: adjustment,
            value_date: year_end.end(),
            lines,
            reason: format!("Close income and expense for the year ended {} to retained earnings", year_end.end()),
            evidence,
            extract_id: None,
        };
        self.propose_journal(ctx, draft, maker)
    }

    // ---- the close -----------------------------------------------------

    fn open_period(&mut self, period: PeriodId) -> Result<&mut PeriodState, Reject> {
        match self.periods.get_mut(&period) {
            Some(state) if state.status == PeriodStatus::Closed => reject("period_closed", format!("{period} is closed")),
            Some(state) => Ok(state),
            None => reject("period", format!("{period} has not started")),
        }
    }

    fn report_check(&mut self, ctx: &mut Ctx<'_>, period: PeriodId, check: String, outcome: Outcome, evidence: Option<String>) -> Result<(), Reject> {
        let Some(decl) = self.close_config.as_ref().and_then(|c| c.checks.iter().find(|c| c.id == check)) else {
            return reject("check", format!("the close configuration declares no check called {check}"));
        };
        let CheckSource::Reported { by } = &decl.source else {
            return reject("check", format!("{check} is worked out by the finance ledger and cannot be reported"));
        };
        if by != ctx.caller {
            return reject("caller", format!("{check} is reported by {by}, not {}", ctx.caller));
        }
        // A person's report carries the person; a module's carries the module.
        let actor = if by == "finance-console" {
            Some(ctx.actor_with(&[ROLE_OPERATOR, ROLE_LEAD])?)
        } else {
            ctx.actor.map(str::to_string)
        };
        if matches!(&outcome, Outcome::Red { items } if items.is_empty()) {
            return reject("check", "a red result lists what is wrong");
        }
        let green = outcome == Outcome::Green;
        let report = CheckReport {
            outcome,
            by: ctx.caller.to_string(),
            actor,
            evidence,
            index: ctx.index,
        };
        self.open_period(period)?.reports.insert(check.clone(), report);
        ctx.events.push(FinEvent::CheckReported { period, check, green });
        Ok(())
    }

    fn waive(&mut self, ctx: &mut Ctx<'_>, period: PeriodId, check: String, reason: String, remediation_owner: String, due: NaiveDate) -> Result<(), Reject> {
        let by = ctx.actor_with(&[ROLE_LEAD])?;
        self.open_period(period)?;
        if reason.trim().is_empty() || remediation_owner.trim().is_empty() {
            return reject("waiver", "a waiver needs a reason and a remediation owner");
        }
        if remediation_owner == by {
            return reject("separation", "the person who waives a check does not own its remediation");
        }
        if due <= period.end() {
            return reject("waiver", "the remediation date must be after the period end");
        }
        let Some(view) = self.checks(period).into_iter().find(|c| c.id == check) else {
            return reject("check", format!("the close configuration declares no check called {check}"));
        };
        let CheckStatus::Red { items } = view.status else {
            return reject("waiver", format!("{check} is not red, so there is nothing to waive"));
        };
        if !view.blocking {
            return reject("waiver", format!("{check} does not block sign-off, so it needs no waiver"));
        }
        let index = ctx.index;
        self.open_period(period)?.waivers.insert(
            check.clone(),
            Waiver {
                check: check.clone(),
                by: by.clone(),
                reason,
                remediation_owner: remediation_owner.clone(),
                due,
                items,
                index,
            },
        );
        ctx.events.push(FinEvent::CheckWaived {
            period,
            check,
            by,
            remediation_owner,
            due,
        });
        Ok(())
    }

    fn closing(&self, period: PeriodId) -> Result<(), Reject> {
        if self.close_config.is_none() {
            return reject("close_config", "no close configuration is active");
        }
        match self.periods.get(&period).map(|p| p.status) {
            Some(PeriodStatus::Closing) => {}
            Some(PeriodStatus::Open) => return reject("period", format!("{period} has not ended on every partition")),
            Some(PeriodStatus::Closed) => return reject("period_closed", format!("{period} is closed")),
            None => return reject("period", format!("{period} has not started")),
        }
        let blockers = self.blockers(period);
        if !blockers.is_empty() {
            let names: Vec<&str> = blockers.iter().map(|c| c.id.as_str()).collect();
            return reject("blocked", format!("{period} has blocking checks that are red: {}", names.join(", ")));
        }
        Ok(())
    }

    fn sign_off(&mut self, ctx: &mut Ctx<'_>, period: PeriodId, capacity: Capacity) -> Result<(), Reject> {
        let by = ctx.actor_with(&[match capacity {
            Capacity::FinanceLead => ROLE_LEAD,
            Capacity::AppointedActuary => ROLE_ACTUARY,
        }])?;
        self.closing(period)?;
        let index = ctx.index;
        let state = self.open_period(period)?;
        if !state.sign_offs.iter().any(|s| s.capacity == capacity) {
            state.sign_offs.push(SignOff {
                capacity,
                by: by.clone(),
                index,
            });
            ctx.events.push(FinEvent::SignedOff { period, capacity, by });
        }
        Ok(())
    }

    fn close_period(&mut self, ctx: &mut Ctx<'_>, period: PeriodId) -> Result<(), Reject> {
        self.closing(period)?;
        // Months close in order. An adjustment period stays open for the
        // audit while later months close, so it neither waits nor is waited for.
        if !period.is_adjustment() {
            if let Some((earlier, _)) = self
                .periods
                .range(..period)
                .find(|(p, s)| !p.is_adjustment() && s.status != PeriodStatus::Closed)
            {
                return reject("order", format!("{earlier} must close before {period}"));
            }
        }
        let quarter_end = !period.is_adjustment() && period.is_quarter_end(self.year_end_month());
        let signed: BTreeSet<Capacity> = self
            .periods
            .get(&period)
            .map(|p| p.sign_offs.iter().map(|s| s.capacity).collect())
            .unwrap_or_default();
        if !signed.contains(&Capacity::FinanceLead) {
            return reject("sign_off", format!("{period} is not signed off by the finance lead"));
        }
        if quarter_end && !signed.contains(&Capacity::AppointedActuary) {
            return reject("sign_off", format!("{period} ends a quarter and is not signed off by the appointed actuary"));
        }
        let trial_balance = self.trial_balance(period, None);
        let trial_balance_hash = hash_rows(&trial_balance);
        let closed = ClosedPeriod {
            final_seq: self.ledger.entry_count(),
            final_head: self.ledger.head(),
            trial_balance,
            trial_balance_hash,
            by: ctx.actor.map(str::to_string),
            index: ctx.index,
        };
        ctx.events.push(FinEvent::PeriodClosed {
            period,
            final_seq: closed.final_seq,
            trial_balance_hash,
        });
        let state = self.open_period(period)?;
        state.status = PeriodStatus::Closed;
        state.closed = Some(closed);
        // A closed period's extracts are stored and registered. The mart
        // keeps the year behind it, for a file that has to be cut again, and
        // lets the rest go so that the state does not grow for ever.
        if let Some(keep_from) = period.start().checked_sub_months(chrono::Months::new(MART_KEEPS_MONTHS)) {
            self.mart.trim(keep_from);
        }
        if !period.is_adjustment() {
            self.conduct_seen.retain(|_, counted_in| *counted_in > period);
        }
        if let Some(keep_from) = period.start().checked_sub_months(chrono::Months::new(LEDGER_KEEPS_MONTHS)) {
            self.trim(keep_from);
        }
        Ok(())
    }
}

impl FinanceState {
    /// Let go of the working records dated before `keep_from`: each day's
    /// control record and Merkle root, the backdated entries posted before
    /// it, and the keys of commands applied before the last period that
    /// ended before it closed. The books themselves, every closed period's
    /// trial balance, the journals, the registered extracts and the filed
    /// returns are kept for ever.
    fn trim(&mut self, keep_from: NaiveDate) {
        self.days.retain(|(date, _), _| *date >= keep_from);
        self.roots.retain(|date, _| *date >= keep_from);
        self.backdated.retain(|item| item.period.end() >= keep_from);
        let before = self
            .periods
            .iter()
            .filter(|(p, _)| p.end() < keep_from)
            .filter_map(|(_, state)| state.closed.as_ref().map(|c| c.index))
            .max();
        if let Some(before) = before {
            self.idempotency.retain(|_, stored| stored.index > before);
        }
    }
}

/// How many months of working records before a period the ledger keeps
/// once that period has closed.
pub const LEDGER_KEEPS_MONTHS: u32 = 24;

/// How many months of history before a period the mart keeps once that
/// period has closed.
pub const MART_KEEPS_MONTHS: u32 = 12;

/// The hash a closed trial balance is known by.
pub fn hash_rows(rows: &[BalanceRow]) -> Hash32 {
    let mut h = Sha256::new();
    h.update(b"ic.finance.trial_balance.v1");
    h.update(serde_json::to_vec(rows).unwrap_or_default());
    Hash32(h.finalize().into())
}

fn differences(core: &[RoleTotal], finance: &BTreeMap<RoleKey, i128>) -> Vec<ControlDifference> {
    let mut all: BTreeMap<RoleKey, (i128, i128)> = BTreeMap::new();
    for t in core {
        all.entry((t.role.clone(), t.class, t.unit.clone())).or_default().0 += t.balance;
    }
    for (key, balance) in finance {
        all.entry(key.clone()).or_default().1 = *balance;
    }
    all.into_iter()
        .filter(|(_, (core, finance))| core != finance)
        .map(|((role, class, unit), (core, finance))| ControlDifference {
            role,
            class,
            unit,
            core,
            finance,
        })
        .collect()
}

impl StateMachine for FinanceState {
    type Command = FinEnvelope;
    type Output = FinApplied;
    fn apply(&mut self, index: u64, command: FinEnvelope) -> FinApplied {
        self.apply_envelope(index, command)
    }
}
