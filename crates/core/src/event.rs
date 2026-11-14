//! Events: past-tense facts. Every state change produces one. Events carry
//! opaque IDs and never personal data. Underwriting terms (loadings and
//! exclusions) are sensitive and are not in events at all.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{AccountClass, AccountUnit, Dimensions, EntryId, Hash32, PartitionId};
use ic_product::Sex;
use serde::{Deserialize, Serialize};

use crate::payments::PayableKind;
use crate::policy::{LayerOrigin, PolicyStatus, PremiumOnAvoidance, ReopenedChoice, Standing, UnwindCause};
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

/// One line of a posted entry as the finance ledger needs it: the account's
/// role and class in place of the account itself, so the line says what kind
/// of money moved without naming a policy's account. Postings of one entry to
/// accounts of the same role, class, unit and dimensions are one line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostedLine {
    pub role: String,
    pub class: AccountClass,
    pub unit: AccountUnit,
    /// Debits positive, in `unit`.
    pub signed: i128,
    #[serde(default)]
    pub dims: Dimensions,
}

/// The balance of every account of one role, class and unit on a partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleTotal {
    pub role: String,
    pub class: AccountClass,
    pub unit: AccountUnit,
    /// Debits positive, in `unit`.
    pub balance: i128,
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
    /// The rating factors the life was priced on, as last recorded: what an
    /// in-force, movement or experience extract groups by. Absent for a
    /// pool of unnamed members and wherever a factor was never given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_of_birth: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sex: Option<Sex>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoker: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupation_class: Option<String>,
    /// The scheme benefit category the member is in, where the scheme has
    /// them: what a category rate table prices on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

impl LifeInfo {
    /// A life with no rating factors given.
    pub fn new(life: LifeId, party: Option<PartyId>) -> Self {
        Self {
            life,
            party,
            date_of_birth: None,
            sex: None,
            smoker: None,
            occupation_class: None,
            category: None,
        }
    }

    /// A life with the rating factors given.
    pub fn with_factors(life: LifeId, party: Option<PartyId>, factors: Option<&crate::policy::LifeFactors>) -> Self {
        Self {
            life,
            party,
            date_of_birth: factors.and_then(|f| f.date_of_birth),
            sex: factors.and_then(|f| f.sex),
            smoker: factors.and_then(|f| f.smoker),
            occupation_class: factors.and_then(|f| f.occupation_class.clone()),
            category: factors.and_then(|f| f.category.clone()),
        }
    }
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
    /// A version of a rate table was recorded as having passed validation.
    RateTableValidated {
        id: String,
        version: u32,
        content_hash: String,
        report_hash: String,
        schema_version: u32,
        by: String,
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
        /// The entry's value date. A value date before the business date
        /// marks a backdated entry; the finance ledger reports it as a
        /// prior-period item when it falls in an earlier period.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value_date: Option<NaiveDate>,
        /// The entry this one reverses, where it is a reversal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reverses: Option<String>,
        /// The entry's postings by role, class, unit and dimensions.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lines: Vec<PostedLine>,
    },
    /// The partition's materialised balances by role when a business day
    /// closed, at the chain head the day's Merkle root covers. The finance
    /// ledger checks what it built from the day's entries against this.
    ControlTotals {
        date: NaiveDate,
        closing_head: Hash32,
        totals: Vec<RoleTotal>,
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
    /// Underwriting's decision to reinstate a lapsed policy was recorded,
    /// or replaced by a later one. Reinstatement on payment can go ahead
    /// while it can be relied on.
    ReinstatementApproved {
        policy: PolicyId,
        reference: String,
        valid_until: NaiveDate,
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
        /// The layer this one takes the place of when underwriting changed
        /// its terms and nothing else: the same cover carried on, not new
        /// business.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continues: Option<u32>,
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
    /// A layer was cut in part: it ends on `effective` and the part kept
    /// carries on as a new layer. Published before the `LayerEnded` and
    /// `LayerAdded` that record the two halves, so that a reader that keeps
    /// something per layer (commission terms, clawback exposure) can carry
    /// the kept share across instead of treating it as cover given up and
    /// new cover written.
    LayerReduced {
        policy: PolicyId,
        cover: String,
        layer: u32,
        continued_as: u32,
        effective: NaiveDate,
        /// The layer's sum insured before the cut.
        sum_insured_minor: i64,
        /// What carries on in the new layer.
        kept_minor: i64,
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
        /// Each discount already taken off the premium for the period, in
        /// the product's order. The premium before discounts is the premium
        /// by cover type plus these.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        discounts: Vec<DiscountGiven>,
    },
    PremiumAdjusted {
        policy: PolicyId,
        receivable: u32,
        life: LifeId,
        effective: NaiveDate,
        total_minor: i64,
        /// The change to each discount on the premium adjusted: negative
        /// where a credit hands back premium a discount had reduced.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        discounts: Vec<DiscountGiven>,
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
        /// The part that paid for cover, by cover type, without the policy
        /// fee, GST and levies. Commission is earned on it.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        net_by_cover_type: BTreeMap<String, i64>,
        /// The statutory fund the money belongs to: the one stamped on the
        /// entry's postings. Payments adds receipts up by fund to move cash
        /// between the funds' bank accounts. Absent on older events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fund: Option<String>,
    },
    ReceiptReversed {
        policy: PolicyId,
        receipt: String,
        amount_minor: i64,
    },
    /// A scheme member was billed for cover the member pays for themselves:
    /// a period's premium, or an adjustment to periods already billed.
    /// Sensitive: the amount can include an underwriting loading, so it
    /// goes to the member and never to the employer.
    MemberPremiumBilled {
        policy: PolicyId,
        life: LifeId,
        receivable: u32,
        period_start: NaiveDate,
        period_end: NaiveDate,
        total_minor: i64,
        adjustment: bool,
    },
    /// The policy was avoided for non-disclosure: it is treated as never
    /// having been in force, and every layer ends on the day it started.
    /// Commission paid on it and reinsurance ceded on it follow.
    PolicyAvoided {
        policy: PolicyId,
        /// The non-disclosure case that decided it.
        case: String,
        premium: PremiumOnAvoidance,
        /// What went back to policy suspense for the policyholder.
        refunded_minor: i64,
    },
    /// A non-disclosure case varied the terms of layers, which now read as
    /// written on those terms from their start. The terms themselves are
    /// sensitive and are not here.
    TermsVaried {
        policy: PolicyId,
        case: String,
        /// Cover, life and layer number of each layer varied.
        layers: Vec<(String, LifeId, u32)>,
    },
    /// The standing of a scheme member on cover the member pays for
    /// themselves. It says nothing about the scheme's own standing.
    MemberStandingChanged {
        policy: PolicyId,
        life: LifeId,
        from: Option<Standing>,
        to: Option<Standing>,
        oldest_unpaid_due: Option<NaiveDate>,
    },
    /// An arrangement to clear overdue premium by instalments was made.
    /// While it is kept, standing is held and the policy does not lapse.
    ArrangementMade {
        policy: PolicyId,
        arrangement: String,
        arrears_minor: i64,
        instalments: Vec<crate::arrangement::Instalment>,
    },
    /// The overdue premium the arrangement was for has been cleared.
    ArrangementCompleted {
        policy: PolicyId,
        arrangement: String,
    },
    /// An instalment was missed, premium billed since went past its grace,
    /// or collections ended it. The lapse clock carries on from where it
    /// stood when the arrangement was made.
    ArrangementBroken {
        policy: PolicyId,
        arrangement: String,
        /// What is still owed of the premium the arrangement was for.
        outstanding_minor: i64,
    },
    /// A hardship override was applied: `share_bps` of the premium is
    /// billed for the days from `from` up to `to`. `adjusted_minor` is the
    /// credit on the period already billed, zero or negative.
    HardshipGranted {
        policy: PolicyId,
        hardship: String,
        from: NaiveDate,
        to: NaiveDate,
        share_bps: u32,
        adjusted_minor: i64,
    },
    /// A hardship override was ended before its planned end. The full
    /// premium is billed from `on`; `adjusted_minor` is the charge for the
    /// rest of the period already billed, zero or positive.
    HardshipEnded {
        policy: PolicyId,
        hardship: String,
        on: NaiveDate,
        planned_to: NaiveDate,
        adjusted_minor: i64,
    },
    /// A member's own cover ended because the member did not pay for it.
    /// Each layer also has its `LayerEnded`. The cover the employer pays
    /// for is untouched.
    MemberCoverLapsed {
        policy: PolicyId,
        life: LifeId,
        /// Cover code and layer number of each layer ended.
        layers: Vec<(String, u32)>,
        effective: NaiveDate,
        /// What the member owed and was written off.
        arrears_minor: i64,
    },
    /// Money from a scheme member, set against what the member owes; the
    /// rest is the member's credit.
    MemberReceiptApplied {
        policy: PolicyId,
        life: LifeId,
        receipt: String,
        allocated_minor: i64,
        to_credit_minor: i64,
        /// As on `ReceiptApplied`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        net_by_cover_type: BTreeMap<String, i64>,
    },
    MemberReceiptReversed {
        policy: PolicyId,
        life: LifeId,
        receipt: String,
        amount_minor: i64,
    },
    /// A member moved to another scheme in a split, and the credit they
    /// held on the old scheme went with them.
    MemberCreditMoved {
        policy: PolicyId,
        new_policy: PolicyId,
        life: LifeId,
        amount_minor: i64,
    },
    /// The owner suspended the policy: no premium for the days from `from`
    /// up to `to`, and nothing that happens in them is covered. The premium
    /// side is announced by `HardshipGranted` under the same name.
    CoverSuspended {
        policy: PolicyId,
        reference: String,
        from: NaiveDate,
        to: NaiveDate,
    },
    /// A suspended policy came back on cover on `on`, ahead of `planned_to`.
    CoverResumed {
        policy: PolicyId,
        reference: String,
        on: NaiveDate,
        planned_to: NaiveDate,
    },
    /// A limited-pay cover was made paid up for a life: no more premium,
    /// and the reduced sum insured for life. The layers it rewrote are
    /// announced by `LayerReduced`, `LayerEnded` and `LayerAdded`.
    CoverMadePaidUp {
        policy: PolicyId,
        cover: String,
        life: LifeId,
        reference: String,
        effective: NaiveDate,
        sum_insured_minor: i64,
        paid_up_minor: i64,
    },
    /// A settled payment went to the wrong person. The payable it paid is
    /// owed again and the holder owes the amount back.
    PayeeDebtRaised {
        policy: PolicyId,
        debt: String,
        payment: String,
        payable: crate::payments::PayableKind,
        /// The refund owed again, for a refund payment.
        refund: Option<String>,
        party: Option<PartyId>,
        amount_minor: i64,
        outstanding_minor: i64,
    },
    PayeeDebtRecovered {
        policy: PolicyId,
        debt: String,
        receipt: String,
        amount_minor: i64,
        outstanding_minor: i64,
    },
    PayeeDebtWrittenOff {
        policy: PolicyId,
        debt: String,
        amount_minor: i64,
        outstanding_minor: i64,
        reason: String,
    },
    /// Cover on a policy takes the place of cover on another policy for the
    /// same person. Reinsurance carries the old cession on for the replaced
    /// amount; the old policy's own ending is announced as it always is.
    CoverReplaced {
        policy: PolicyId,
        cover: String,
        life: LifeId,
        replaced_policy: PolicyId,
        replaced_cover: String,
        replaced_life: LifeId,
        replaced_sum_insured_minor: i64,
        /// The new cover's sum insured for the life on the day recorded.
        sum_insured_minor: i64,
    },
    /// A life used its cover's insurability option. The layer it wrote is
    /// announced by `LayerAdded` as any increase is.
    InsurabilityOptionExercised {
        policy: PolicyId,
        cover: String,
        life: LifeId,
        event: String,
        event_date: NaiveDate,
        layer: u32,
        sum_insured_minor: i64,
        /// How many times the life has now used the option on this cover.
        uses: u32,
        reference: String,
    },
    /// Money was lent to the owner against the policy and raised to benefit
    /// payable under the reference.
    PolicyLoanDrawn {
        policy: PolicyId,
        reference: String,
        amount_minor: i64,
        outstanding_minor: i64,
    },
    /// The loan was reduced: by a receipt, or out of a surrender value or a
    /// claim (`from` says which, and `reference` is that thing's).
    PolicyLoanRepaid {
        policy: PolicyId,
        reference: String,
        amount_minor: i64,
        interest_minor: i64,
        principal_minor: i64,
        outstanding_minor: i64,
        from: String,
    },
    /// The loan was found to be more than the policy is worth. Said once,
    /// the first day it is so.
    PolicyLoanOverValue {
        policy: PolicyId,
        outstanding_minor: i64,
        value_minor: i64,
        on: NaiveDate,
    },
    /// The policy ended owing more on its loan than it was worth, and the
    /// rest was written off.
    PolicyLoanWrittenOff {
        policy: PolicyId,
        reference: String,
        amount_minor: i64,
        cause: String,
    },
    /// A member's credit was set against what the member owes.
    MemberCreditApplied {
        policy: PolicyId,
        life: LifeId,
        amount_minor: i64,
        /// As on `ReceiptApplied`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        net_by_cover_type: BTreeMap<String, i64>,
    },
    MemberCreditRefunded {
        policy: PolicyId,
        life: LifeId,
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
        /// As on `ReceiptApplied`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        net_by_cover_type: BTreeMap<String, i64>,
    },
    /// Premium already paid for periods whose billing was reversed is the
    /// policyholder's money again, in suspense. The amounts are the part that
    /// had paid for cover, by cover type, as on `ReceiptApplied`: commission
    /// earned on it is taken back.
    PremiumUnapplied {
        policy: PolicyId,
        reference: String,
        net_by_cover_type: BTreeMap<String, i64>,
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
    /// Choices made on a defective notice can be made again until `until`.
    ReopenWindowOpened {
        policy: PolicyId,
        reopen_reference: String,
        purpose: String,
        from: NaiveDate,
        until: NaiveDate,
    },
    NoticeDeliveryRecorded {
        policy: PolicyId,
        purpose: String,
        notice: String,
        delivered_on: NaiveDate,
        earliest_effect: NaiveDate,
    },
    /// A choice made on a defective notice was made again. `effective` is
    /// the date the new choice applies from.
    ChoiceReopened {
        policy: PolicyId,
        reopen_reference: String,
        choice: ReopenedChoice,
        effective: NaiveDate,
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
        /// The member's rating factors as last recorded, for the extracts.
        /// Sensitive like any life's factors; absent on older events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        info: Option<LifeInfo>,
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
        /// The member's rating factors as last recorded, for the extracts.
        /// Sensitive like any life's factors; absent on older events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        info: Option<LifeInfo>,
    },
    MemberIdentified {
        policy: PolicyId,
        life: LifeId,
        party: PartyId,
    },

    /// A fact published by a module that lives on the partition. `kind` is
    /// part of that module's published interface. Like every event it
    /// carries opaque IDs, amounts and codes, never personal or clinical data.
    Module {
        module: String,
        kind: String,
        /// What the fact is about: a claim ID, for the claims module.
        subject: String,
        #[serde(default)]
        detail: std::collections::BTreeMap<String, String>,
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

    /// Something happened in the investment ledger: a fund or a unit price
    /// was activated, or a unit order was placed, cancelled or executed.
    Investment(crate::invest::InvestEvent),

    /// A lender's interest in a policy was recorded, consented for, or
    /// released.
    Interest(crate::interest::InterestEvent),

    /// The owner's own hold on money going out began or ended on a policy.
    OwnerHoldChanged {
        policy: PolicyId,
        held: bool,
        reference: String,
    },
    /// Members of a scheme moved to a new scheme from a date, with their
    /// history and underwritten cover: a sale or a demerger. They did not
    /// leave. `layers` names each underwritten layer that moved, as life,
    /// cover and layer ID, the same on both schemes.
    /// The covers a scheme member chose from the scheme's menu, for
    /// themselves or a dependant, as they stand from a date. The layers
    /// that ended and started are in `LayerEnded` and `LayerAdded` before it.
    MemberCoversSet {
        policy: PolicyId,
        life: LifeId,
        /// Who is billed for the covers the member pays for: the life, or
        /// for a dependant their member.
        payer: LifeId,
        effective: NaiveDate,
        covers: Vec<crate::group::ChosenCover>,
        reference: String,
    },
    SchemeSplit {
        policy: PolicyId,
        new_policy: PolicyId,
        new_number: String,
        lives: Vec<LifeId>,
        effective: NaiveDate,
        layers: Vec<(LifeId, String, u32)>,
    },
    /// What a life or the policy holds that a discount is given for changed
    /// from a date: a tier awarded, changed or lost. A tier code only; the
    /// data behind it stays with the module that judged it.
    EntitlementChanged {
        policy: PolicyId,
        code: String,
        tier: Option<String>,
        life: Option<LifeId>,
        held: bool,
        from: NaiveDate,
        until: Option<NaiveDate>,
        reference: String,
    },
    /// An approved discount was given on a policy, or its end brought
    /// forward: reported with its size, reason and approvers.
    DiscountOverrideSet {
        policy: PolicyId,
        id: String,
        percent: ic_money::DecStr,
        reason: String,
        from: NaiveDate,
        until: NaiveDate,
        approver: String,
        second_approver: Option<String>,
    },
    /// A nomination for a death benefit was recorded: a first one, a change
    /// or a removal (no nominees). Parties are opaque IDs.
    NominationRecorded {
        policy: PolicyId,
        nomination: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        life: Option<LifeId>,
        nominees: Vec<crate::nomination::Nominee>,
        effective_from: NaiveDate,
        /// Servicing held a conversation with the owner before the change.
        conversation_held: bool,
    },

    /// A balanced entry on counterparty accounts. Carries the lines, so the
    /// module that keeps each party's position builds it from this alone.
    CounterpartyPosted {
        entry: EntryId,
        reference: String,
        kind: String,
        currency: ic_money::Currency,
        value_date: NaiveDate,
        policy: Option<PolicyId>,
        reverses: Option<String>,
        lines: Vec<crate::counterparty::CounterpartyLine>,
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
    TransferLanded {
        policy: PolicyId,
        transfer_id: String,
    },
    TransferRejected {
        policy: PolicyId,
        transfer_id: String,
        reason: String,
    },
    /// A claims command one partition sends another, for work on one person
    /// whose claims sit on both: an overpayment on one recovered from a
    /// benefit on the other. The relay delivers it as caller `partition`, so
    /// only a command the claims module allows that caller is applied.
    Claims(Box<ic_claims::Command>),
}

impl MessageBody {
    /// The command the receiving partition applies, and the caller the
    /// relay delivers it as. One place, so every relay delivers alike.
    /// A message between the claims held on two partitions.
    pub fn is_claims(&self) -> bool {
        matches!(self, MessageBody::Claims(_))
    }

    pub fn into_delivery(self) -> (&'static str, crate::Command) {
        match self {
            MessageBody::TransferRecord(record) => ("transfer", crate::Command::LandTransfer { record }),
            MessageBody::TransferLanded { policy, transfer_id } => ("transfer", crate::Command::CompleteTransfer { policy, transfer_id }),
            MessageBody::TransferRejected { policy, transfer_id, .. } => ("transfer", crate::Command::AbortTransfer { policy, transfer_id }),
            MessageBody::Claims(command) => ("partition", crate::Command::Claims(command)),
        }
    }
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// One discount on a bill: its code, the name a notice shows, the posting
/// type its ledger lines carry and what it took off. Negative on a credit
/// adjustment that hands back premium the discount had reduced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscountGiven {
    pub code: String,
    pub name: String,
    pub posting_type: String,
    pub amount_minor: i64,
}
