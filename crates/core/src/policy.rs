//! The policy model: policy, cover, cover layer and the three kinds of life
//! record. Retail and group business share one structure.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{PartitionId, Posting};
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
    /// Receivables that are fully settled and fully earned carry no further
    /// information the state machine needs: their history is in the log. This
    /// keeps policy state bounded over decades.
    pub fn prune_settled(&mut self, keep_from: NaiveDate) {
        self.receivables
            .retain(|r| r.outstanding_minor != 0 || r.unearned_fine() != 0 || r.period_end > keep_from);
        self.receipts.retain(|_, r| r.received_on > keep_from);
    }
}
