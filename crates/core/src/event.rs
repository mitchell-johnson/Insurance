//! Events: past-tense facts. Every state change produces one. Events carry
//! opaque IDs and never personal data. Underwriting terms (loadings and
//! exclusions) are sensitive and are not in events at all.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{EntryId, Hash32, PartitionId};
use serde::{Deserialize, Serialize};

use crate::payments::PayableKind;
use crate::policy::{LayerOrigin, PolicyStatus, Standing, UnwindCause};
use crate::transfer::TransferRecord;
use crate::{GroupId, LifeId, PartyId, PolicyId};

/// An event with its place in the partition's gapless stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// `partition-seq`: unique across the insurer. Consumers de-duplicate on it.
    pub id: String,
    pub partition: PartitionId,
    /// Gapless per partition. A consumer that sees a gap asks for the missing events.
    pub seq: u64,
    pub business_date: NaiveDate,
    /// Log index of the command that produced the event.
    pub log_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayableCover {
    pub cover: String,
    pub cover_type: String,
    pub sum_insured_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerInfo {
    pub cover: String,
    pub cover_type: String,
    pub layer: u32,
    pub life: LifeId,
    pub sum_insured_minor: i64,
    pub start: NaiveDate,
}

/// What a scheme member is covered for under a formula-driven cover: the
/// formula's result on the member's latest notified details. Scheme cover is
/// not held as layers, so this is how it reaches the life index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberCover {
    pub cover: String,
    pub cover_type: String,
    pub life: LifeId,
    pub sum_insured_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeInfo {
    pub life: LifeId,
    /// The party, where the life is named or has been identified.
    pub party: Option<PartyId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Externally tagged: internally tagged enums cannot carry 128-bit integers in JSON.
#[serde(rename_all = "snake_case")]
pub enum Event {
    ProductActivated {
        code: String,
        version: u32,
        content_hash: String,
    },
    RateTableActivated {
        id: String,
        version: u32,
        content_hash: String,
        scoped: bool,
    },
    DefinitionsActivated {
        count: u32,
    },
    IndexRateActivated {
        code: String,
        effective_from: NaiveDate,
    },
    BusinessDateAdvanced {
        from: Option<NaiveDate>,
        to: NaiveDate,
        closing_head: Hash32,
    },
    EntryPosted {
        entry: EntryId,
        kind: String,
        policy: Option<PolicyId>,
        hash: Hash32,
    },

    PolicyIssued {
        policy: PolicyId,
        number: String,
        group: GroupId,
        policyholder: PartyId,
        product_code: String,
        product_hash: String,
        inception: NaiveDate,
        lives: Vec<LifeInfo>,
        layers: Vec<LayerInfo>,
        /// Formula-driven cover on each member of a scheme.
        #[serde(default)]
        members: Vec<MemberCover>,
    },
    PolicyStatusChanged {
        policy: PolicyId,
        from: PolicyStatus,
        to: PolicyStatus,
    },
    StandingChanged {
        policy: PolicyId,
        from: Option<Standing>,
        to: Option<Standing>,
        oldest_unpaid_due: Option<NaiveDate>,
    },
    PolicyReinstated {
        policy: PolicyId,
    },
    AnniversaryReached {
        policy: PolicyId,
        number: u32,
        on: NaiveDate,
    },
    LayerAdded {
        policy: PolicyId,
        layer: LayerInfo,
        origin: LayerOrigin,
    },
    /// A layer stopped from a date: an underwritten layer given up, reduced
    /// or ended because the member left.
    LayerEnded {
        policy: PolicyId,
        cover: String,
        layer: u32,
        life: LifeId,
        effective: NaiveDate,
        sum_insured_minor: i64,
    },
    /// The part of a retrospective adjustment that is underwriting loading.
    /// It names no member: loadings reach the scheme only in aggregate.
    LoadingAdjusted {
        policy: PolicyId,
        receivable: u32,
        total_minor: i64,
    },
    LayersMerged {
        policy: PolicyId,
        cover: String,
        removed: Vec<u32>,
        into: u32,
        sum_insured_minor: i64,
    },
    CoverExpired {
        policy: PolicyId,
        cover: String,
        on: NaiveDate,
    },
    IndexationChoiceRecorded {
        policy: PolicyId,
        declined: bool,
    },

    PremiumDue {
        policy: PolicyId,
        receivable: u32,
        period_start: NaiveDate,
        period_end: NaiveDate,
        total_minor: i64,
        members: Option<u32>,
        /// The premium for the period by cover type, before tax and levies.
        /// Reinsurance priced on the original premium reads it.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        by_cover_type: BTreeMap<String, i64>,
    },
    PremiumAdjusted {
        policy: PolicyId,
        receivable: u32,
        life: LifeId,
        effective: NaiveDate,
        total_minor: i64,
    },
    /// Member-level breakdown of one chunk of a scheme invoice.
    SchemeInvoiceChunk {
        policy: PolicyId,
        period_index: u32,
        lines: Vec<(LifeId, i64)>,
        complete: bool,
        /// Underwriting loading billed in this chunk that is not shown on any
        /// member's line. The lines plus this is what the chunk added to the
        /// invoice.
        #[serde(default, skip_serializing_if = "is_zero")]
        loading_minor: i64,
    },
    PremiumEarned {
        policy: PolicyId,
        through: NaiveDate,
        amount_fine: i128,
    },
    ReceiptApplied {
        policy: PolicyId,
        receipt: String,
        allocated_minor: i64,
        to_suspense_minor: i64,
    },
    ReceiptReversed {
        policy: PolicyId,
        receipt: String,
        amount_minor: i64,
    },
    /// What the policy owes after a command changed it. Read models and cash
    /// matching follow this instead of re-deriving it from the lines above.
    PolicyBalance {
        policy: PolicyId,
        outstanding_minor: i64,
        oldest_unpaid_due: Option<NaiveDate>,
    },
    SuspenseApplied {
        policy: PolicyId,
        amount_minor: i64,
    },
    SuspenseRefunded {
        policy: PolicyId,
        amount_minor: i64,
        /// The claim the refund is paid with, where the payer is the person
        /// the claim is for. Absent for a refund to the source account.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        with_claim: Option<String>,
    },

    /// A layer that started after the unwind date now ends on the day it
    /// started: it existed for no days, and its history shows it was created
    /// and unwound.
    LayerUnwound {
        policy: PolicyId,
        cover: String,
        layer: u32,
        start: NaiveDate,
        reference: String,
    },
    /// A cover that expired after the unwind date is in force again.
    CoverExpiryUnwound {
        policy: PolicyId,
        cover: String,
        layers: Vec<LayerInfo>,
        reference: String,
    },
    /// Everything effective after `after` was reversed. The policy is held
    /// as at that date until it is terminated or resumed.
    PolicyUnwound {
        policy: PolicyId,
        after: NaiveDate,
        cause: UnwindCause,
        reference: String,
        /// The receivables whose billing was reversed.
        receivables: Vec<u32>,
    },
    PolicyUnwindResumed {
        policy: PolicyId,
        reference: String,
    },
    /// The policy ended with a claim, as at the insured event.
    PolicyTerminated {
        policy: PolicyId,
        effective: NaiveDate,
        claim: String,
        /// The sum insured the claim is assessed on, by cover: what was in
        /// force on the day of the event, plus any indexation step applied
        /// since where the product's wording pays it.
        payable: Vec<PayableCover>,
    },
    /// A refund moved from suspense to refund payable and waits to be paid.
    RefundPayableRaised {
        policy: PolicyId,
        refund: String,
        amount_minor: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        with_claim: Option<String>,
    },
    /// A raised refund went back to suspense unpaid.
    RefundPayableReleased {
        policy: PolicyId,
        refund: String,
        amount_minor: i64,
    },
    /// The bank confirmed a payment and the payable was discharged.
    PaymentSettled {
        policy: PolicyId,
        payment: String,
        payable: PayableKind,
        amount_minor: i64,
        bank_account: String,
    },
    /// The bank sent a payment back and it is owed again.
    PaymentReturned {
        policy: PolicyId,
        payment: String,
        payable: PayableKind,
        amount_minor: i64,
        bank_account: String,
    },
    /// Collection from the payer's account must stop: the payments module
    /// cancels the direct debit authority.
    PaymentAuthorityCancelled {
        policy: PolicyId,
        claim: String,
    },

    /// The reinsurance module recorded the cession on a cover from a date.
    CessionRecorded {
        policy: PolicyId,
        cover: String,
        layer: u32,
        life: LifeId,
        from: NaiveDate,
    },
    /// A reinsurance entry was posted. The finance ledger and the reinsurer
    /// exposure view read it.
    ReinsurancePosted {
        kind: crate::reinsurance::ReinsurancePosting,
        treaty: String,
        reinsurer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<PolicyId>,
        amount_minor: i64,
        value_date: NaiveDate,
        reference: String,
    },

    /// End-of-day could not process a policy. Becomes an exception item.
    PolicyException {
        policy: PolicyId,
        code: String,
        message: String,
    },

    MemberJoined {
        policy: PolicyId,
        life: LifeId,
        effective: NaiveDate,
        /// The member's cover on the details just notified.
        #[serde(default)]
        covers: Vec<MemberCover>,
        /// The person, where the member joined as a named life or is already
        /// identified.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        party: Option<PartyId>,
    },
    MemberLeft {
        policy: PolicyId,
        life: LifeId,
        effective: NaiveDate,
    },
    MemberChanged {
        policy: PolicyId,
        life: LifeId,
        effective: NaiveDate,
        /// The member's cover on the details just notified.
        #[serde(default)]
        covers: Vec<MemberCover>,
    },
    MemberIdentified {
        policy: PolicyId,
        life: LifeId,
        party: PartyId,
    },

    TransferStarted {
        policy: PolicyId,
        transfer_id: String,
        target_group: GroupId,
        target_partition: PartitionId,
    },
    TransferLanded {
        policy: PolicyId,
        transfer_id: String,
        group: GroupId,
        from_partition: PartitionId,
        lives: Vec<LifeInfo>,
        layers: Vec<LayerInfo>,
        #[serde(default)]
        members: Vec<MemberCover>,
        number: String,
        /// What a directory that first meets the policy here needs to list it.
        product_code: String,
        status: PolicyStatus,
        standing: Option<Standing>,
    },
    TransferRejected {
        policy: PolicyId,
        transfer_id: String,
        reason: String,
    },
    TransferCompleted {
        policy: PolicyId,
        transfer_id: String,
    },
    TransferAborted {
        policy: PolicyId,
        transfer_id: String,
    },
}

/// A message for another partition, delivered at least once by the relay.
/// The receiver treats the message ID as its idempotency key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub id: String,
    pub to_partition: PartitionId,
    pub body: MessageBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Externally tagged: internally tagged enums cannot carry 128-bit integers in JSON.
#[serde(rename_all = "snake_case")]
pub enum MessageBody {
    TransferRecord(Box<TransferRecord>),
    TransferLanded { policy: PolicyId, transfer_id: String },
    TransferRejected { policy: PolicyId, transfer_id: String, reason: String },
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}
