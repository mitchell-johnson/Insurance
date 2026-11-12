//! The policy model: policy, cover, cover layer and the three kinds of life
//! record. Retail and group business share one structure.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{EntryId, PartitionId, Posting};
use ic_money::{Currency, DecStr};
use ic_product::rating::ModalPremium;
use ic_product::{DefinitionBinding, Frequency, Sex};
use serde::{Deserialize, Serialize};

use crate::bitemporal::Bitemporal;
use crate::{GroupId, LifeId, PartyId, PolicyId};

/// The lifecycle. Reinstatement is an event, not a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyStatus {
    Quoted,
    Proposed,
    InForce,
    Lapsed,
    PaidUp,
    Surrendered,
    Matured,
    Expired,
    Cancelled,
    /// Ended by a claim that ends the policy, as at the insured event.
    Terminated,
    /// Moved to another group. History stays on this partition's chain.
    Transferred,
}

/// Derived from unpaid premium, never set by hand. Only a policy in force has
/// a standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Paid,
    /// Cover continues in full.
    InGrace,
    /// Cover is not in force for new events.
    Suspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverStatus {
    InForce,
    Expired,
    Ended,
}

/// What a cover attaches to. Three bases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifeKind {
    /// A person in the customer master.
    Named { party: PartyId },
    /// A member under a group policy carrying only what is needed to rate and
    /// administer cover. Identified (linked to a party) when it is needed;
    /// the member record is never deleted.
    UnnamedMember { member_ref: String, linked_party: Option<PartyId> },
    /// Cover expressed in aggregate for a group.
    Pool { headcount: u32, total_salary_minor: i64 },
}

/// Rating attributes of a life. These are insurance facts, not identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeFactors {
    #[serde(default)]
    pub date_of_birth: Option<NaiveDate>,
    #[serde(default)]
    pub sex: Option<Sex>,
    #[serde(default)]
    pub smoker: Option<bool>,
    /// Group scheme benefit category.
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub salary_minor: Option<i64>,
    /// A ceiling on this member's automatic (formula-driven) cover, below
    /// the category maximum and the automatic acceptance limit. Set where a
    /// reinsurer or the scheme's terms require the excess to be underwritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_cover_cap_minor: Option<i64>,
    /// The declared aggregate of a pool life. A pool with a declaration is
    /// billed on it; a new declaration is a membership change like any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolAggregate>,
}

/// What an employer declares for a pool: how many people and their total pay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolAggregate {
    pub headcount: u32,
    pub total_salary_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeRecord {
    pub id: LifeId,
    pub kind: LifeKind,
    pub factors: Bitemporal<LifeFactors>,
    pub joined: NaiveDate,
    pub left: Option<NaiveDate>,
}

impl LifeRecord {
    pub fn in_force_on(&self, on: NaiveDate) -> bool {
        self.joined <= on && self.left.is_none_or(|l| l > on)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerOrigin {
    Original,
    Increase,
    Indexation,
    /// A scheme member's cover above the automatic amount, accepted by
    /// underwriting and written as its own layer on the scheme's cover.
    UnderwrittenExcess,
}

/// One tranche of a cover with its own start date. Frozen forever at the end
/// of the spine phase. Reinsurance cession, commission book and clawback
/// exposure attach here in later phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    pub id: u32,
    pub life: LifeId,
    pub start: NaiveDate,
    pub end: Option<NaiveDate>,
    pub sum_insured_minor: i64,
    pub origin: LayerOrigin,
    /// Underwriting terms: loadings as fractions and exclusions as opaque
    /// codes. Sensitive: they imply health, so they never appear in public
    /// events or telemetry.
    #[serde(default)]
    pub loadings: Vec<DecStr>,
    #[serde(default)]
    pub exclusions: Vec<String>,
    /// The member has agreed that the employer may see this layer's premium
    /// with its loading on the member's own invoice line. Otherwise loadings
    /// reach the employer only inside one scheme total.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loading_itemised: bool,
}

impl Layer {
    pub fn in_force_on(&self, on: NaiveDate) -> bool {
        self.start <= on && self.end.is_none_or(|e| e > on)
    }
    /// Layers are merged when nothing distinguishes them.
    pub fn same_terms(&self, o: &Layer) -> bool {
        self.life == o.life && self.end == o.end && self.loadings == o.loadings && self.exclusions == o.exclusions
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cover {
    pub code: String,
    pub cover_type: String,
    pub status: CoverStatus,
    pub start: NaiveDate,
    /// The definition versions this cover is bound to. Frozen forever.
    pub definitions: Vec<DefinitionBinding>,
    /// Individually written amounts. Empty for formula-driven group cover,
    /// whose amount moves with the member's data.
    pub layers: Vec<Layer>,
    pub formula_driven: bool,
}

impl Cover {
    /// A cover's sum insured on a date is the sum of its layers in force.
    pub fn sum_insured_on(&self, on: NaiveDate) -> i64 {
        self.layers.iter().filter(|l| l.in_force_on(on)).map(|l| l.sum_insured_minor).sum()
    }
}

/// One billed period (or a retrospective adjustment to one). Nothing already
/// billed is ever edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receivable {
    pub id: u32,
    pub kind: ReceivableKind,
    /// First day covered.
    pub period_start: NaiveDate,
    /// First day not covered.
    pub period_end: NaiveDate,
    /// The age of the receivable is always counted from here, so a bounced
    /// payment does not reset the lapse clock.
    pub due: NaiveDate,
    /// Signed total in minor units: negative for a credit adjustment.
    pub total_minor: i64,
    pub outstanding_minor: i64,
    pub gst_minor: i64,
    pub levies_minor: BTreeMap<String, i64>,
    /// What is earned over the period, by cover type (and `policy_fee`), in
    /// fine units. Signed.
    pub net_fine: BTreeMap<String, i128>,
    pub earned_fine: BTreeMap<String, i128>,
    /// Closed by lapse, cancellation or transfer: no longer earns.
    pub closed: bool,
    /// The journal entry that raised it, so a reversal can name what it
    /// reverses. Absent on receivables raised before this was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryId>,
    /// The business date on which nothing was left owing on it, however that
    /// came about: a payment, suspense applied, a credit, a write-off. Absent
    /// while anything is outstanding, and cleared when a reversal reopens it.
    /// The claims cover check reads it to know whether a premium was unpaid
    /// on an event date that has since been paid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceivableKind {
    Period,
    Adjustment,
}

impl Receivable {
    pub fn net_total_fine(&self) -> i128 {
        self.net_fine.values().sum()
    }
    pub fn earned_total_fine(&self) -> i128 {
        self.earned_fine.values().sum()
    }
    pub fn unearned_fine(&self) -> i128 {
        if self.closed {
            0
        } else {
            self.net_total_fine() - self.earned_total_fine()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRecord {
    pub received_on: NaiveDate,
    pub amount_minor: i64,
    /// Receivable ID -> amount allocated.
    pub allocations: BTreeMap<u32, i64>,
    pub to_suspense_minor: i64,
    pub reversed: bool,
}

/// A scheme invoice being built across several commands, so that no single
/// command exceeds the apply budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvoice {
    pub period_index: u32,
    pub cursor: Option<LifeId>,
    pub members: u32,
    pub premium: ModalPremium,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LapseRecord {
    pub lapsed_on: NaiveDate,
    /// The lapse entry's postings and the receivables as they were, so that
    /// reinstatement can restore exactly what the lapse wrote off.
    pub postings: Vec<Posting>,
    pub receivables_before: Vec<Receivable>,
    /// What must be in suspense to reinstate.
    pub arrears_minor: i64,
}

/// Why a policy was unwound to a date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnwindCause {
    /// An insured event was notified after the policy had carried on past it.
    Claim,
    /// The insurer's own error is being corrected under a remediation case.
    Remediation,
}

/// A policy unwound to a date is held there: end-of-day earns what is still
/// billed and does nothing else, so the anniversary and the billing that
/// were just reversed are not applied again the same night. The hold ends
/// when the policy is terminated by the claim, or when the remediation case
/// resumes it and end-of-day replays from the date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnwindHold {
    /// Everything effective after this date was reversed.
    pub after: NaiveDate,
    pub cause: UnwindCause,
    /// The claim or remediation case.
    pub reference: String,
    /// The business date the unwind was applied.
    pub applied_on: NaiveDate,
}

/// What the ledger holds for one claim on a policy. The claims module owns
/// the claim; this is the core's own record of its money, so the two can be
/// reconciled against each other every day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimLedger {
    pub cover: String,
    pub open: bool,
    pub reserve_minor: i64,
    pub admitted_minor: i64,
    /// Owed and not yet settled at the bank: this claim's share of benefit
    /// payable.
    pub payable_minor: i64,
    /// The part of it released for payment and waiting for the bank.
    #[serde(default)]
    pub instructed_minor: i64,
    /// Overpaid and not yet recovered or written off.
    pub receivable_minor: i64,
    pub payments: BTreeMap<String, ClaimPayment>,
    /// Premium paid from claims expense under this claim's waiver.
    #[serde(default)]
    pub waived_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimPayment {
    pub amount_minor: i64,
    pub ex_gratia: bool,
    pub state: ClaimPaymentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPaymentState {
    /// Released by claims; the payments module has it to pay.
    Instructed,
    /// The bank confirmed it and benefit payable was discharged.
    Settled,
    /// It failed or came back. The money is owed again.
    Returned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferOut {
    pub transfer_id: String,
    pub target_group: GroupId,
    pub target_partition: PartitionId,
    pub postings: Vec<Posting>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub id: PolicyId,
    /// The customer-facing policy number.
    pub number: String,
    pub group: GroupId,
    pub policyholder: PartyId,
    pub product_code: String,
    /// The product version this policy is on. Grandfathered until changed.
    pub product_hash: String,
    pub currency: Currency,
    pub statutory_fund: String,
    pub inception: NaiveDate,
    pub frequency: Frequency,
    pub status: PolicyStatus,
    pub standing: Option<Standing>,
    pub lives: BTreeMap<LifeId, LifeRecord>,
    pub covers: BTreeMap<String, Cover>,
    pub indexation_declined: bool,
    /// Number of billing periods billed so far.
    pub periods_billed: u32,
    pub next_receivable_id: u32,
    pub receivables: Vec<Receivable>,
    pub receipts: BTreeMap<String, ReceiptRecord>,
    pub anniversaries_processed: u32,
    pub pending_invoice: Option<PendingInvoice>,
    pub lapse: Option<LapseRecord>,
    pub transfer_out: Option<TransferOut>,
    /// Where earlier history was written, if the policy arrived by transfer.
    pub history: Vec<(PartitionId, GroupId)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unwind: Option<UnwindHold>,
    /// Set when the person who paid the premiums is the person the claim is
    /// for: refunds are then paid with this claim, to the estate or the
    /// beneficiary, never back to the account the money came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund_with_claim: Option<String>,
    /// The money side of each claim on the policy, by claim ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub claims: BTreeMap<String, ClaimLedger>,
    /// Periods in which choices made on a defective notice can be made
    /// again, by reopen reference.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reopen_windows: BTreeMap<String, ReopenWindow>,
    /// The latest proven delivery of each mandatory notice, by purpose.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notices: BTreeMap<String, NoticeRecord>,
    /// Written on another system and brought in by the migration load.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub migrated: bool,
    /// What a cancellation wrote, so that one set aside is undone exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<CancellationRecord>,
    /// The anniversaries that passed with indexation declined. A decline
    /// set aside is taken out again, so it does not count against the
    /// policyholder's right to indexation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declined_anniversaries: Vec<u32>,
    /// What is still owed on each refund raised to refund payable, by refund ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub refunds_raised: BTreeMap<String, i64>,
    /// Unit-linked money raised to benefit payable (a withdrawal, a
    /// surrender, cash owed after a price error), by payout ID, until the
    /// bank has settled it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub payouts: BTreeMap<String, crate::invest::Payout>,
    /// Lenders' and assignees' interests in the policy, by interest ID.
    /// Released ones are kept, so the past can still be asked about.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub interests: BTreeMap<String, crate::interest::Interest>,
    /// Payments settled from a payable, by payment ID, so a payment the
    /// bank sends back can be reversed exactly.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub payments: BTreeMap<String, PaymentRecord>,
    /// The cession recorded on each layer or member cover, by
    /// `cover/layer/life`, as the reinsurance module worked it out.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cessions: BTreeMap<String, Vec<crate::reinsurance::CessionSegment>>,
}

/// A period in which a choice made on a defective notice can be made again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReopenWindow {
    /// What the notice was for: `renewal`, `lapse`, `premium_change`.
    pub purpose: String,
    pub from: NaiveDate,
    /// The last day a choice can be made again.
    pub until: NaiveDate,
    /// The choices already made again under this reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used: Vec<ReopenedChoice>,
}

/// A choice that can be made again under a reopen reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReopenedChoice {
    /// A decline of indexation is set aside: indexation applies, from the
    /// anniversary it would have applied at.
    IndexationAccepted,
    /// A cancellation is set aside: the policy is in force as if it had
    /// never been cancelled.
    CancellationSetAside,
}

/// Proof that a mandatory notice reached the policyholder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeRecord {
    pub notice: String,
    pub delivered_on: NaiveDate,
    /// The first day what the notice warns of may take effect.
    pub earliest_effect: NaiveDate,
}

/// A cancellation as it was applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationRecord {
    pub cancelled_on: NaiveDate,
    /// The cancellation entry and its postings, if it posted anything.
    pub entry: Option<EntryId>,
    pub postings: Vec<Posting>,
    pub receivables_before: Vec<Receivable>,
    pub covers_before: BTreeMap<String, Cover>,
    pub standing_before: Option<Standing>,
}

/// A payment settled from one of the policy's payable accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRecord {
    pub payable: crate::payments::PayableKind,
    /// The refund this payment paid, for a refund payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refund: Option<String>,
    pub amount_minor: i64,
    /// The payments module's code for the bank account the money left.
    pub bank_account: String,
    pub settled_on: NaiveDate,
    /// The entry that settled it: what a return reverses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<EntryId>,
    pub returned: bool,
}

impl Policy {
    pub fn is_frozen(&self) -> bool {
        self.transfer_out.is_some()
    }
    pub fn outstanding_minor(&self) -> i64 {
        self.receivables.iter().map(|r| r.outstanding_minor).sum()
    }
    pub fn unearned_fine(&self) -> i128 {
        self.receivables.iter().map(Receivable::unearned_fine).sum()
    }
    /// Due date of the oldest receivable with money still owed on it.
    pub fn oldest_unpaid_due(&self) -> Option<NaiveDate> {
        self.receivables.iter().filter(|r| r.outstanding_minor > 0).map(|r| r.due).min()
    }
    /// Record when each receivable stopped being owed, and forget it for
    /// one that is owed again. Run after every command that touches the
    /// policy, so no path that settles or reopens a receivable is missed.
    pub fn stamp_settled(&mut self, today: NaiveDate) {
        for r in &mut self.receivables {
            if r.outstanding_minor != 0 {
                r.settled_on = None;
            } else {
                r.settled_on.get_or_insert(today);
            }
        }
    }
    /// Receivables that are fully settled and fully earned carry no further
    /// information the state machine needs: their history is in the log. This
    /// keeps policy state bounded over decades.
    pub fn prune_settled(&mut self, keep_from: NaiveDate) {
        self.receivables
            .retain(|r| r.outstanding_minor != 0 || r.unearned_fine() != 0 || r.period_end > keep_from);
        self.receipts.retain(|_, r| r.received_on > keep_from);
        self.payments.retain(|_, p| p.settled_on > keep_from);
    }
}
