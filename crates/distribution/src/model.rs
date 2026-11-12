//! What the module keeps: schedules and the terms they give a layer, books,
//! the layers and policies it has seen, exposure items, what it has asked
//! the core to post, and each party's position as the core confirmed it.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use ic_core::counterparty::{CounterpartyPosting, PartyLedger};
use ic_core::{PartyId, PolicyId};
use ic_ledger::PartitionId;
use serde::{Deserialize, Serialize};

use crate::rules::{CommissionType, Component, HeldReason, Rule};
use crate::{BookId, EngagementId};

/// Below this many whole months in force, this share of initial commission
/// is clawed back. Steps are in ascending order of months.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClawbackStep {
    pub months_under: u32,
    pub ppm: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleOption {
    /// Initial commission as a share of the layer's annual premium.
    pub initial_ppm: u32,
    /// Renewal commission as a share of each premium received.
    pub renewal_ppm: u32,
    /// Renewal commission starts this many months after the layer does.
    pub renewal_from_month: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InitialEarned {
    /// When the first premium for the layer is received. The default.
    FirstPremium,
    AtIssue,
}

/// A commission schedule: how much. The distribution rule says who.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    /// The commission options on offer, by name.
    pub options: BTreeMap<String, ScheduleOption>,
    pub clawback: Vec<ClawbackStep>,
    /// A cancellation within this many days of the layer starting reverses
    /// all commission paid on it.
    pub cooling_off_days: u32,
    pub initial_earned: InitialEarned,
    /// Whether an indexation step earns initial commission.
    pub initial_on_indexation: bool,
    /// On replacement business, the unexpired clawback of the replaced
    /// policy carries over to the new one instead of being clawed back.
    pub replacement_carries_clawback: bool,
    /// Whether premium waived during a claim earns renewal commission, as
    /// premium received does. It does unless the schedule says otherwise.
    #[serde(default = "yes")]
    pub earns_on_waived_premium: bool,
}

fn yes() -> bool {
    true
}

impl Schedule {
    pub fn content_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"ic.distribution.schedule.v1");
        h.update(serde_json::to_vec(self).unwrap_or_default());
        hex::encode(h.finalize())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.options.is_empty() {
            return Err("a schedule offers at least one commission option".into());
        }
        if self.options.values().any(|o| o.renewal_ppm > crate::rules::WHOLE) {
            return Err("renewal commission cannot exceed the premium".into());
        }
        if self.clawback.windows(2).any(|w| w[0].months_under >= w[1].months_under) || self.clawback.iter().any(|s| s.ppm > crate::rules::WHOLE) {
            return Err("the clawback scale must rise in months and never exceed the whole".into());
        }
        Ok(())
    }
}

/// A layer's commission terms. Fixed when the layer is written; they travel
/// with it whoever is paid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerTerms {
    pub schedule: String,
    pub schedule_version: u32,
    pub option: String,
    pub rates: ScheduleOption,
    pub annual_premium_minor: i64,
    pub clawback: Vec<ClawbackStep>,
    pub cooling_off_days: u32,
    pub initial_earned: InitialEarned,
}

impl LayerTerms {
    pub fn from_schedule(s: &Schedule, option: &str, annual_premium_minor: i64) -> Option<Self> {
        Some(Self {
            schedule: s.id.clone(),
            schedule_version: s.version,
            option: option.to_string(),
            rates: *s.options.get(option)?,
            annual_premium_minor,
            clawback: s.clawback.clone(),
            cooling_off_days: s.cooling_off_days,
            initial_earned: s.initial_earned,
        })
    }
}

/// Whole months from `from` to `to`.
pub fn months_between(from: NaiveDate, to: NaiveDate) -> u32 {
    if to <= from {
        return 0;
    }
    let mut m = (to.year() - from.year()) * 12 + to.month() as i32 - from.month() as i32;
    if to.day() < from.day() {
        m -= 1;
    }
    m.max(0) as u32
}

pub fn clawback_ppm(scale: &[ClawbackStep], months_in_force: u32) -> u32 {
    scale.iter().find(|s| months_in_force < s.months_under).map_or(0, |s| s.ppm)
}

/// Which books a layer pays into, from a date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookAssignment {
    pub from: NaiveDate,
    pub shares: Vec<(BookId, u32)>,
}

pub fn layer_key(policy: &PolicyId, cover: &str, layer: u32) -> String {
    format!("{policy}/{cover}/{layer}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerRecord {
    pub policy: PolicyId,
    pub cover: String,
    pub layer: u32,
    pub cover_type: String,
    pub start: NaiveDate,
    pub sum_insured_minor: i64,
    pub indexation: bool,
    /// The engagements under which the advice was given, with their split.
    /// A historical fact that never changes.
    pub writing: Vec<(EngagementId, u32)>,
    /// Ordered by date. Empty means the house book: no commission.
    pub assignments: Vec<BookAssignment>,
    pub terms: Option<LayerTerms>,
    /// The reference of the allocation that paid initial commission.
    pub initial_paid: Option<String>,
    pub ended: Option<NaiveDate>,
}

impl LayerRecord {
    pub fn key(&self) -> String {
        layer_key(&self.policy, &self.cover, self.layer)
    }
    pub fn books_on(&self, day: NaiveDate) -> &[(BookId, u32)] {
        self.assignments.iter().rev().find(|a| a.from <= day).map_or(&[], |a| a.shares.as_slice())
    }
    pub fn in_force(&self, day: NaiveDate) -> bool {
        self.start <= day && self.ended.is_none_or(|e| day < e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndKind {
    Lapsed,
    Cancelled,
    /// Ended because a claim was paid: never a clawback.
    Claim,
    Other,
}

/// What a new layer on the policy gets unless it is written otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultAssignment {
    pub writing: EngagementId,
    pub book: BookId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRecord {
    pub partition: PartitionId,
    pub product_code: String,
    pub inception: NaiveDate,
    pub ended: Option<(NaiveDate, EndKind)>,
    /// The engagement that looks after the customer now. It gives access.
    pub servicing: Option<EngagementId>,
    pub default: Option<DefaultAssignment>,
    pub layers: Vec<String>,
    /// The policy from the same insurer on the same life that this replaced.
    pub replaces: Option<PolicyId>,
    /// The clawback raised when it last lapsed or was cancelled.
    pub last_clawback: Option<String>,
    /// The setting version a customer's change of adviser was decided under.
    pub adviser_change_setting: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub id: BookId,
    pub name: String,
    /// The engagement whose business it is, if it is one adviser's.
    pub engagement: Option<EngagementId>,
    /// The party that sees everything in it: usually the provider.
    pub owner: Option<PartyId>,
    /// Orphan business: earns no commission.
    pub house: bool,
    /// New business is refused from this date.
    pub closed_to_new_from: Option<NaiveDate>,
    /// Activated versions, ordered by effective date then version.
    pub rules: Vec<Rule>,
    /// Replacement monitoring is raised on the book until this date.
    pub watch_until: Option<NaiveDate>,
}

impl Book {
    pub fn rule_on(&self, day: NaiveDate) -> Option<&Rule> {
        self.rules.iter().rev().find(|r| r.effective_from <= day)
    }
}

/// Initial commission that may yet be clawed back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    pub id: String,
    pub policy: PolicyId,
    pub layer: String,
    pub book: BookId,
    pub component: Component,
    pub paid_to: PartyId,
    /// Who bears clawback, with shares and guarantors, as the rule said on
    /// the day the commission was paid.
    pub liable: Vec<(PartyId, u32, Option<PartyId>)>,
    pub amount_minor: i64,
    pub paid_on: NaiveDate,
    pub layer_start: NaiveDate,
    pub scale: Vec<ClawbackStep>,
    pub cooling_off_days: u32,
    pub product: String,
    pub cover_type: String,
    pub open: bool,
    pub clawed_minor: i64,
}

impl Exposure {
    /// What is still at risk on a day.
    pub fn at_risk(&self, day: NaiveDate) -> i64 {
        if !self.open {
            return 0;
        }
        crate::rules::portion(self.amount_minor, clawback_ppm(&self.scale, months_between(self.layer_start, day)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dest {
    Payable,
    Held(HeldReason),
}

/// One line of commission detail: what a statement shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocLine {
    pub payee: PartyId,
    pub dest: Dest,
    /// Positive is owed to the payee; negative takes it back.
    pub amount_minor: i64,
    pub book: Option<BookId>,
    pub layer: Option<String>,
    pub component: Option<Component>,
    pub commission_type: Option<CommissionType>,
    pub earner: Option<EngagementId>,
    pub rule_version: Option<u32>,
    pub product: String,
    pub cover_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostingState {
    /// Asked of the core, not yet seen in its events.
    Pending,
    Posted {
        entry: String,
        seq: u64,
    },
    Rejected {
        code: String,
        message: String,
    },
}

/// Commission worked out for one cause and asked of the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allocation {
    pub reference: String,
    pub partition: PartitionId,
    pub policy: PolicyId,
    pub kind: String,
    /// The ID of the event that caused it.
    pub cause: String,
    pub date: NaiveDate,
    pub lines: Vec<AllocLine>,
    pub reverses: Option<String>,
    /// Exposure items this allocation created.
    pub exposures: Vec<String>,
    pub reversed_by: Option<String>,
}

/// One application of premium to a policy and the commission it earned, so
/// that premium later handed back can take the commission back with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PremiumRecord {
    /// The allocation the premium earned, where it earned any.
    pub reference: Option<String>,
    /// The premium that paid for cover, by cover type.
    pub net: BTreeMap<String, i64>,
    /// How much of it has since gone back to the policyholder.
    pub back: BTreeMap<String, i64>,
}

/// Something asked of the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requested {
    pub partition: PartitionId,
    pub posting: CounterpartyPosting,
    pub state: PostingState,
    /// The workbench action or statement it belongs to.
    pub owner: Option<String>,
}

/// One confirmed movement on one of a party's accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Movement {
    /// The module's own sequence: statements cut on it.
    pub seq: u64,
    pub party: PartyId,
    pub partition: PartitionId,
    pub ledger: PartyLedger,
    /// Positive is in the party's favour.
    pub amount_minor: i64,
    pub kind: String,
    pub reference: String,
    pub date: NaiveDate,
}

/// Part of a held balance, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldItem {
    pub party: PartyId,
    pub partition: PartitionId,
    pub book: Option<BookId>,
    pub reason: Option<HeldReason>,
    pub date: NaiveDate,
    pub reference: String,
    pub amount_minor: i64,
    pub remaining_minor: i64,
}

/// Where money for a payee is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayeeAccount {
    /// A reference to the account in the payments module. Never the number.
    pub account_ref: String,
    /// The result of the payee name check.
    pub name_check_ref: String,
    /// The signed step-up that authorised the change.
    pub step_up_ref: String,
    /// The first day a payment may go to it: the end of the cooling-off.
    pub usable_from: NaiveDate,
    /// The appointment document, where the account belongs to a liquidator
    /// or an executor acting for the party.
    pub appointment_ref: Option<String>,
    pub added_on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionKind {
    /// Commission assigned to a financier. Removed only with their consent.
    Assignment,
    /// A statutory deduction or court order. Entered by a restricted command.
    Statutory,
}

/// Pays part of each statement to someone else first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentInstruction {
    pub id: String,
    pub kind: InstructionKind,
    pub to_account_ref: String,
    /// A fixed amount per statement, or a share of it.
    pub amount_minor: Option<i64>,
    pub share_ppm: Option<u32>,
    pub from: NaiveDate,
    pub until: Option<NaiveDate>,
    pub document_ref: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payee {
    pub accounts: Vec<PayeeAccount>,
    pub instructions: Vec<PaymentInstruction>,
    /// Payments stop; commission keeps accruing and statements keep issuing.
    pub payment_hold: Option<String>,
    /// Earnings go to held from a date, for a reason other than licence.
    pub accrual_hold: Option<(NaiveDate, HeldReason, String)>,
    /// The agreement the payee is paid under.
    pub agreement_ref: Option<String>,
    /// The module sequence the last statement was cut at.
    pub statement_cut: u64,
    pub statements: u32,
    /// Advances are recovered at no more than this share of a statement.
    pub advance_recovery_ppm: u32,
}

impl Payee {
    pub fn account_on(&self, account_ref: &str, day: NaiveDate) -> Option<&PayeeAccount> {
        self.accounts.iter().find(|a| a.account_ref == account_ref && a.usable_from <= day)
    }
    pub fn default_account(&self, day: NaiveDate) -> Option<&PayeeAccount> {
        self.accounts.iter().rev().find(|a| a.usable_from <= day && a.appointment_ref.is_none())
    }
}
