//! What the payments module holds: authorities to collect, accounts it may
//! pay, runs and their items, refunds, the bank's statements and the
//! exceptions reconciliation raises.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, NaiveDateTime};
use ic_core::{PayableKind, PolicyId};
use ic_money::Currency;
use serde::{Deserialize, Serialize};

/// IDs are chosen by whoever asks, so a retry names the same thing.
pub type Id = String;

/// A refusal. Nothing changed.
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

/// What a person or service is allowed to do here. The host authenticates
/// the caller and states the roles; the module enforces who may do what and
/// that the same person never does both halves of a control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Prepares runs and requests refunds.
    Maker,
    /// Approves runs. Never also maintains payees or connectivity.
    Approver,
    /// Registers and changes authorities and payee accounts.
    PayeeMaintainer,
    /// Activates configuration and redirects routes between banks.
    ConnectivityAdmin,
    /// Approves payment to an estate or a liquidator.
    Legal,
    /// Works reconciliation exceptions.
    Reconciler,
    /// Funds the bank accounts and hears of large payments in advance.
    Treasury,
    /// The module's own drivers: bank feedback, core results, core events.
    Service,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub id: String,
    pub roles: BTreeSet<Role>,
}

/// A bank account as the bank needs it. Held only here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountDetails {
    pub name: String,
    pub number: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityStatus {
    Active,
    Suspended,
    Cancelled,
}

/// A direct debit authority: permission to collect a policy's premium.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authority {
    pub policy: PolicyId,
    pub party: String,
    /// The scheme member this authority collects for, where a member pays
    /// for their own cover. What it collects is then that member's money
    /// and is never applied to the employer's bill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life: Option<ic_core::LifeId>,
    pub account: AccountDetails,
    /// The account is in the payer's sole name. If the payer dies, nothing
    /// more may be collected from it or refunded to it.
    pub sole_name: bool,
    pub status: AuthorityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Dishonours in a row. A collection still settled when its run closes
    /// resets it.
    pub dishonours: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayeeKind {
    Customer,
    Beneficiary,
    /// An executor or administrator receiving for an estate.
    Estate,
    /// A liquidator or receiver receiving for a party that has failed.
    Liquidator,
    Provider,
    Adviser,
    Reinsurer,
    /// A tax authority, a levy body, the unclaimed money authority.
    Authority,
    /// One of the insurer's own accounts, for transfers between funds.
    Own,
}

impl PayeeKind {
    /// Paying someone who stands in for the person owed always needs legal's
    /// approval and always waits out the cooling-off period.
    pub fn needs_legal(self) -> bool {
        matches!(self, PayeeKind::Estate | PayeeKind::Liquidator)
    }
}

/// What the bank's account name check said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameCheck {
    Match,
    Close,
    NoMatch,
    Unavailable,
}

/// An account the module may pay once every control on it has passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payee {
    pub party: String,
    pub kind: PayeeKind,
    pub account: AccountDetails,
    pub name_check: NameCheck,
    /// Who last entered or changed the details.
    pub maintained_by: String,
    /// Who confirmed them with the payee through a second channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_by: Option<String>,
    /// `Some(true)` once sanctions screening came back clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screened_clear: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_approved_by: Option<String>,
    /// No payment before this moment: the cooling-off period on new details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooling_off_until: Option<NaiveDateTime>,
    /// A payment came back from this account: it needs new details.
    #[serde(default)]
    pub suspect: bool,
    #[serde(default)]
    pub closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunType {
    /// Direct debit collection: the one run type that brings money in.
    Collection,
    LumpSumClaims,
    BenefitPayments,
    ProviderPayments,
    PremiumRefunds,
    Commission,
    Reinsurer,
    FundSettlement,
    TaxAndLevies,
    UnclaimedMoney,
    FundTransfer,
    ExGratia,
}

/// What a bank account is for. One account per bank, statutory fund,
/// purpose and currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    PremiumCollection,
    ClaimsDisbursement,
    ThirdPartyDisbursement,
    Refunds,
    Operating,
}

impl RunType {
    pub fn inbound(self) -> bool {
        self == RunType::Collection
    }
    pub fn purpose(self) -> Purpose {
        match self {
            RunType::Collection => Purpose::PremiumCollection,
            RunType::LumpSumClaims | RunType::BenefitPayments | RunType::ProviderPayments | RunType::ExGratia => Purpose::ClaimsDisbursement,
            RunType::Commission | RunType::Reinsurer | RunType::TaxAndLevies | RunType::UnclaimedMoney => Purpose::ThirdPartyDisbursement,
            RunType::PremiumRefunds => Purpose::Refunds,
            RunType::FundSettlement | RunType::FundTransfer => Purpose::Operating,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Prepared,
    Checked,
    Approved,
    Closed,
    Cancelled,
}

/// Who an item pays, or collects from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Counterparty {
    Payee(Id),
    /// The account of a direct debit authority: a collection, or a refund
    /// going back to where the premium came from.
    Authority(Id),
}

/// Why the bank sent an item back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReturnReason {
    InsufficientFunds,
    AccountClosed,
    AuthorityCancelled,
    InvalidAccount,
    Other(String),
}

impl ReturnReason {
    /// The account itself is no good, whatever is tried next.
    pub fn account_unusable(&self) -> bool {
        matches!(
            self,
            ReturnReason::AccountClosed | ReturnReason::AuthorityCancelled | ReturnReason::InvalidAccount
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    /// A control stopped it. The code says which.
    Held(String),
    /// In a submission handed to the channel.
    Submitted,
    Acknowledged,
    /// The bank refused it at acknowledgement.
    Rejected(String),
    Settled,
    Returned(ReturnReason),
}

impl ItemStatus {
    /// Nothing more will happen to the item in this run.
    pub fn terminal(&self) -> bool {
        matches!(self, ItemStatus::Rejected(_) | ItemStatus::Settled | ItemStatus::Returned(_))
    }
}

/// One payment or collection as the requester states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewItem {
    /// The end-to-end reference at the bank. Never reused.
    pub item: Id,
    /// What is being paid (a claim payment, a commission statement, an
    /// invoice). The same source is never in two live items.
    pub source: String,
    pub counterparty: Counterparty,
    pub amount_minor: i64,
    /// The policy whose ledger the money settles on, if it is policy money.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyId>,
    /// The payable an outbound policy item discharges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payable: Option<PayableKind>,
    /// The refund a refund item pays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund: Option<Id>,
    /// The dishonoured collection this one presents again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub represents: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    #[serde(flatten)]
    pub new: NewItem,
    pub status: ItemStatus,
    /// The account as it was when the run was checked: what the file says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<Id>,
    /// Settled at some point, so the core has heard of it.
    #[serde(default)]
    pub was_settled: bool,
    /// An approver looked at a possible duplicate and let it through.
    #[serde(default)]
    pub released: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    /// Waiting for the channel driver.
    Queued,
    /// A driver has taken it. Until the bank says what it holds, nobody
    /// knows whether it arrived, and it is never sent again under this ID.
    Sending,
    Sent,
    Acknowledged,
    /// The bank refused the whole file, or confirmed it never arrived.
    Failed(String),
}

/// One file or API batch: the unit the bank acknowledges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub run: Id,
    pub channel: Id,
    /// The module's code for the bank account, which is also its code in
    /// the core's ledger.
    pub account: Id,
    pub items: Vec<Id>,
    pub status: SubmissionStatus,
    /// The region whose endpoint the driver used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// When the driver took it. A submission that has been with a driver
    /// for long is one whose fate has to be asked of the bank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begun_at: Option<NaiveDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub run_type: RunType,
    pub fund: String,
    pub currency: Currency,
    pub value_date: NaiveDate,
    /// Out of hours, by API, with two on-call approvers.
    pub urgent: bool,
    pub status: RunStatus,
    /// Everyone who prepared or changed the run. None of them may approve it.
    pub makers: BTreeSet<String>,
    pub approvers: BTreeSet<String>,
    pub items: BTreeMap<Id, Item>,
    pub submissions: Vec<Id>,
}

impl Run {
    /// The total of the items that are still going to the bank or are there.
    pub fn live_total(&self) -> i64 {
        self.items
            .values()
            .filter(|i| !matches!(i.status, ItemStatus::Rejected(_) | ItemStatus::Returned(_)))
            .map(|i| i.new.amount_minor)
            .sum()
    }
}

/// Where a refund goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefundDestination {
    /// Back to the account the premium was collected from. The default.
    Source { authority: Id },
    /// A verified account, where there is no source account to go back to.
    Payee { payee: Id },
    /// With the claim payment, to the estate or the beneficiary: the payer
    /// has died, or the source account was in the deceased's sole name.
    WithClaim { claim: String, payee: Id },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefundStatus {
    /// Asked of the core, not yet owed in the ledger.
    Requested,
    /// Owed: in refund payable, waiting for a run.
    Raised,
    InRun(Id),
    Paid,
    /// Going back to suspense.
    Releasing,
    Released,
    /// The core refused to raise it.
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refund {
    pub policy: PolicyId,
    pub amount_minor: i64,
    pub currency: Currency,
    pub destination: RefundDestination,
    pub status: RefundStatus,
}

/// Work for a driver: a command for the core, a receipt to match, or a
/// notice to people. Done when the driver reports the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Instruction {
    /// Send this command to the partition that holds the policy. The key
    /// of the outbox entry is the idempotency key.
    Core { caller: String, command: Box<ic_core::Command> },
    /// Tell another hosted module something, in that module's own words.
    Module { module: String, command: serde_json::Value },
    /// A command that does not itself name its policy (one for the claims
    /// module): send it to the partition that holds this policy.
    CoreAt {
        policy: PolicyId,
        caller: String,
        command: Box<ic_core::Command>,
    },
    /// A credit nobody was expecting: hand it to cash matching.
    Match {
        exception: Id,
        account: Id,
        amount_minor: i64,
        currency: Currency,
        reference: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payer_account: Option<String>,
        received_on: NaiveDate,
    },
    /// Tell people something they must act on.
    Notify { to: Role, subject: String, detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxEntry {
    /// Stable for the life of the entry: the idempotency key downstream.
    pub key: String,
    pub instruction: Instruction,
    /// What the instruction is about, so its result finds its way back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<About>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum About {
    RefundRaise(Id),
    RefundRelease(Id),
    Item(Id),
    Exception(Id),
}

/// One line of a bank statement. Credits are positive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementLine {
    pub id: Id,
    pub amount_minor: i64,
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer_account: Option<String>,
}

/// A movement the module expects to see on a statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expected {
    pub amount_minor: i64,
    pub since: NaiveDate,
}

/// The module's view of one bank account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement_date: Option<NaiveDate>,
    pub closing_minor: i64,
    /// By the reference the bank will quote: a file ID for the bulk entry of
    /// a settled file, an item ID for a single return.
    pub expected: BTreeMap<String, Expected>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExceptionKind {
    /// A credit on a collection account that matched nothing expected.
    UnidentifiedReceipt,
    UnexplainedCredit,
    UnexplainedDebit,
    /// The reference was expected, the amount was not.
    AmountMismatch,
    /// The statement does not open where the last one closed.
    BalanceBreak,
    /// The core refused an instruction.
    CoreRefused,
    /// Money went, or may have gone, to an account it should not have.
    PaidToBlockedAccount,
}

/// A reconciliation break. Open until someone accountable resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exception {
    pub kind: ExceptionKind,
    pub account: Id,
    pub amount_minor: i64,
    pub reference: String,
    pub opened_on: NaiveDate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

/// One attempt to reach a bank channel from a region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub ok: bool,
    /// Hash of the evidence held in the vault: what was sent, what came back.
    pub evidence_hash: String,
    pub detail: String,
}

/// An evacuation drill: a low-value file and a low-value API payment sent
/// from one region through every channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drill {
    pub region: String,
    pub started: NaiveDateTime,
    pub probes: BTreeMap<Id, Probe>,
    /// `Some(true)` once closed with every channel reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
}
