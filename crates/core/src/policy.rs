//! The policy model: policy, cover, cover layer and the three kinds of life
//! record. Retail and group business share one structure.

use std::collections::{BTreeMap, BTreeSet};

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
    /// Avoided for non-disclosure: treated as never having been in force.
    Avoided,
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
    /// A scheme member's partner or child. A dependant has no cover the
    /// scheme gives automatically, only the covers chosen for them, and
    /// leaves the scheme when their member does.
    Dependant {
        /// The member they are on the scheme through.
        of: LifeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        party: Option<PartyId>,
    },
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
    /// Occupation class, for covers whose rate table is split by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupation_class: Option<String>,
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
    /// A cover a scheme member chose from the scheme's menu, for themselves
    /// or a dependant: a health plan, or a level of one.
    MemberChoice,
}

/// Who pays for an underwritten layer on a scheme member's cover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberPays {
    /// The employer pays, on the scheme invoice.
    #[default]
    Nothing,
    /// The member pays the loading; the standard rate stays on the scheme
    /// invoice. Used where a total of loadings would identify the few
    /// members who have one.
    Loading,
    /// The member pays for the whole layer: voluntary cover.
    All,
}

impl MemberPays {
    pub fn is_nothing(&self) -> bool {
        *self == MemberPays::Nothing
    }
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
    /// The date level premium takes the life's age from, when it is not the
    /// layer's own start. A layer that carries on an earlier layer's cover on
    /// changed terms keeps that layer's date, so the customer keeps their
    /// entry age.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rated_from: Option<NaiveDate>,
    /// The excess level chosen for this layer, where the cover offers a
    /// choice. A change of excess is a new layer, so the past keeps its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excess_option: Option<String>,
    /// What of this layer's premium the member pays for themselves.
    #[serde(default, skip_serializing_if = "MemberPays::is_nothing")]
    pub member_pays: MemberPays,
}

impl Layer {
    /// The date level premium takes the life's age from.
    pub fn rating_start(&self) -> NaiveDate {
        self.rated_from.unwrap_or(self.start)
    }
    pub fn in_force_on(&self, on: NaiveDate) -> bool {
        self.start <= on && self.end.is_none_or(|e| e > on)
    }
    /// Layers are merged when nothing distinguishes them.
    pub fn same_terms(&self, o: &Layer) -> bool {
        self.life == o.life
            && self.end == o.end
            && self.loadings == o.loadings
            && self.exclusions == o.exclusions
            && self.excess_option == o.excess_option
            && self.member_pays == o.member_pays
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
    /// Whether a layer is one scheme member's own: cover chosen from the
    /// scheme's menu, or underwritten cover above the automatic amount. It
    /// is priced on that member's line and ends when they leave.
    pub fn members_own(&self, layer: &Layer) -> bool {
        layer.origin == LayerOrigin::MemberChoice || (self.formula_driven && layer.origin == LayerOrigin::UnderwrittenExcess)
    }
    /// A cover on a scheme that members choose, one at a time.
    pub fn chosen_by_members(&self) -> bool {
        self.layers.iter().any(|l| l.origin == LayerOrigin::MemberChoice)
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
    /// The scheme member who owes this, for cover the member pays for
    /// themselves. Absent on everything the policyholder owes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<LifeId>,
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
    /// The scheme member who paid, for a receipt against the member's own
    /// account. What was not allocated went to the member's credit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<LifeId>,
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
    /// Underwriting's decision that the policy may be reinstated, and on
    /// what terms. It belongs to this lapse and goes with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ReinstatementApproval>,
    pub lapsed_on: NaiveDate,
    /// The lapse entry's postings and the receivables as they were, so that
    /// reinstatement can restore exactly what the lapse wrote off.
    pub postings: Vec<Posting>,
    pub receivables_before: Vec<Receivable>,
    /// What must be in suspense to reinstate.
    pub arrears_minor: i64,
}

/// Underwriting's decision on a lapsed policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReinstatementApproval {
    /// The underwriting decision.
    pub reference: String,
    /// The last day the decision can be relied on: a declaration of health
    /// goes stale.
    pub valid_until: NaiveDate,
    /// Terms that differ from the ones the cover lapsed on. Sensitive:
    /// never copied into an event.
    #[serde(default)]
    pub terms: Vec<LayerTerms>,
}

/// What becomes of the premiums paid on a policy that is avoided. The
/// approved non-disclosure case decides; the core does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PremiumOnAvoidance {
    /// Every premium paid since inception goes back to the policyholder.
    Refund,
    /// The insurer keeps what was paid.
    Retain,
}

/// That a policy was avoided, and on what decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvoidanceRecord {
    /// The non-disclosure case that decided it.
    pub case: String,
    pub avoided_on: NaiveDate,
    pub premium: PremiumOnAvoidance,
    /// What went back to policy suspense for the policyholder.
    pub refunded_minor: i64,
}

/// Terms a layer carried before a non-disclosure case varied them. The
/// layer now reads as if written on the varied terms from its start; this
/// is the record of what it was written on. Sensitive, like the terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variation {
    pub case: String,
    pub recorded_on: NaiveDate,
    pub cover: String,
    pub layer: u32,
    pub loadings_before: Vec<DecStr>,
    pub exclusions_before: Vec<String>,
}

/// What receivables removed from the policy's state had billed and earned,
/// so that an avoidance years later can still undo all of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrunedPremium {
    /// Earned premium by cover type (and `policy_fee`), in fine units.
    #[serde(default)]
    pub earned_fine: BTreeMap<String, i128>,
    #[serde(default)]
    pub gst_minor: i64,
    #[serde(default)]
    pub levies_minor: BTreeMap<String, i64>,
}

impl PrunedPremium {
    pub fn is_empty(&self) -> bool {
        self.earned_fine.values().all(|v| *v == 0) && self.gst_minor == 0 && self.levies_minor.values().all(|v| *v == 0)
    }
}

/// The underwriting terms on one life's cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerTerms {
    pub cover: String,
    pub life: LifeId,
    #[serde(default)]
    pub loadings: Vec<DecStr>,
    #[serde(default)]
    pub exclusions: Vec<String>,
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
    /// What of it was waived on each receivable, by receivable ID, so that
    /// a waiver can be billed back period by period.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub waived: BTreeMap<u32, i64>,
    /// Cover this claim's lump sums took off the policy, layer by layer,
    /// until the admission is reversed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<ClaimCut>,
}

/// Cover a claim's lump sum took off one layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCut {
    pub cover: String,
    pub life: LifeId,
    /// The layer that was ended or reduced.
    pub layer: u32,
    pub amount_minor: i64,
    /// The business date the cover came off.
    pub on: NaiveDate,
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
    /// Scheme members who have, or have had, money of their own held on
    /// this policy: each has a member suspense account in the ledger.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub member_accounts: BTreeSet<LifeId>,
    /// The standing of each scheme member who is billed for cover of their
    /// own, derived from the member's oldest unpaid bill. Separate from the
    /// scheme's standing in both directions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub member_standing: BTreeMap<LifeId, Standing>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avoidance: Option<AvoidanceRecord>,
    /// The arrangement to clear overdue premium, the latest one made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrangement: Option<crate::arrangement::Arrangement>,
    /// Days the lapse clock stood still under a broken arrangement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrears_pause: Option<crate::arrangement::ArrearsPause>,
    /// The hardship override, the latest one granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardship: Option<crate::hardship::HardshipOverride>,
    /// A full surrender of a unit-linked policy that has been asked for.
    /// While it is set no new money goes in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surrender: Option<crate::invest::SurrenderRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variations: Vec<Variation>,
    #[serde(default, skip_serializing_if = "PrunedPremium::is_empty")]
    pub pruned: PrunedPremium,
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
    /// A group scheme: its lives are members, billed together on one
    /// invoice. A scheme whose product has only covers that members choose
    /// (a health-only employer plan) has no formula cover to know it by.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scheme: bool,
    /// The scheme this one was split from, and the date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_from: Option<crate::group::SplitFrom>,
    /// Every entitlement a discount is given for that the policy or a life
    /// on it has held, with its dates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entitlements: Vec<crate::entitlement::Entitlement>,
    /// Every approved discount given on the policy, with its dates, reason
    /// and approvers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discount_overrides: Vec<crate::discount_override::DiscountOverride>,
    /// The owner's own hold on money going out, while it stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_hold: Option<crate::owner_hold::OwnerHold>,
    /// Every nomination for a death benefit ever recorded, in the order
    /// recorded. Nothing is edited or removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nominations: Vec<crate::nomination::Nomination>,
    /// The scheme membership this policy carries on from, where it was
    /// issued under a leaver's continuation option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_of: Option<crate::command::ContinuationOf>,
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
    /// Whether this is a group scheme rather than a retail policy.
    pub fn is_scheme(&self) -> bool {
        self.scheme || self.covers.values().any(|c| c.formula_driven)
    }
    /// Who is billed for cover on a life that the member pays for: the life
    /// itself, or for a dependant the member they are on the scheme through.
    pub fn payer_of(&self, life: &LifeId) -> LifeId {
        match self.lives.get(life).map(|l| &l.kind) {
            Some(LifeKind::Dependant { of, .. }) => of.clone(),
            _ => life.clone(),
        }
    }
    /// The dependants on the scheme through a member, whether or not they
    /// have left.
    pub fn dependants_of<'a>(&'a self, member: &'a LifeId) -> impl Iterator<Item = &'a LifeRecord> + 'a {
        self.lives
            .values()
            .filter(move |l| matches!(&l.kind, LifeKind::Dependant { of, .. } if of == member))
    }
    pub fn is_frozen(&self) -> bool {
        self.transfer_out.is_some()
    }
    pub fn outstanding_minor(&self) -> i64 {
        self.receivables.iter().map(|r| r.outstanding_minor).sum()
    }
    pub fn unearned_fine(&self) -> i128 {
        self.receivables.iter().map(Receivable::unearned_fine).sum()
    }
    /// Due date of the oldest receivable the policyholder still owes money
    /// on. What a scheme member owes for their own cover does not count
    /// against the scheme.
    pub fn oldest_unpaid_due(&self) -> Option<NaiveDate> {
        self.receivables
            .iter()
            .filter(|r| r.outstanding_minor > 0 && r.payer.is_none())
            .map(|r| r.due)
            .min()
    }
    /// Due date of the oldest receivable a member still owes money on.
    pub fn member_oldest_unpaid_due(&self, life: &LifeId) -> Option<NaiveDate> {
        self.receivables
            .iter()
            .filter(|r| r.outstanding_minor > 0 && r.payer.as_ref() == Some(life))
            .map(|r| r.due)
            .min()
    }
    /// What the policyholder owes, leaving out what scheme members owe for
    /// cover they pay for themselves.
    pub fn policyholder_outstanding_minor(&self) -> i64 {
        self.receivables.iter().filter(|r| r.payer.is_none()).map(|r| r.outstanding_minor).sum()
    }
    /// What a member owes for cover they pay for themselves.
    pub fn member_outstanding_minor(&self, life: &LifeId) -> i64 {
        self.receivables
            .iter()
            .filter(|r| r.payer.as_ref() == Some(life))
            .map(|r| r.outstanding_minor)
            .sum()
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
        let keep = |r: &Receivable| r.outstanding_minor != 0 || r.unearned_fine() != 0 || r.period_end > keep_from;
        // What goes is remembered in total: an avoidance undoes every
        // premium since inception, long after the receivables are gone.
        for r in self.receivables.iter().filter(|r| !keep(r) && r.payer.is_none()) {
            for (cover_type, fine) in &r.earned_fine {
                *self.pruned.earned_fine.entry(cover_type.clone()).or_insert(0) += fine;
            }
            self.pruned.gst_minor += r.gst_minor;
            for (code, v) in &r.levies_minor {
                *self.pruned.levies_minor.entry(code.clone()).or_insert(0) += v;
            }
        }
        self.receivables.retain(keep);
        self.receipts.retain(|_, r| r.received_on > keep_from);
        self.payments.retain(|_, p| p.settled_on > keep_from);
    }
}
