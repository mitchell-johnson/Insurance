//! The reinsurance state machine: commands in, instructions for the core and
//! notices for other modules out.
//!
//! Like the core, it is a deterministic function of its own log. It reads no
//! clock: every command carries the business date it is made on. What it
//! works out goes back to the core as instructions with stable IDs, so the
//! core's postings stay replayable.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::event::EventRecord;
use ic_core::{LifeId, PartyId, PolicyId};
use ic_ledger::{Hash32, PartitionId};
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::account::{Bucket, LineKind, PcState, Statement};
use crate::capacity::{CapacityRequest, PendingTerms, Reservation};
use crate::cession::{timeline, Cession, Risk, Segment, Source, UnplacedReason};
use crate::claims::{ClaimBook, ClaimFact, DisputedItem};
use crate::ids::pairs;
use crate::premium::Period;
use crate::treaty::{AggregateTreaty, ExcessPath, ReinsurerRateTable, RiskFactors, Treaty};
use crate::{ClaimId, LifeKey, ReinsurerId, RiskKey, TreatyId, TreatyRef};

/// A command the module refused. Nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
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

/// What goes into the module's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A duplicate returns the original result.
    pub key: String,
    /// The module sending the command.
    pub caller: String,
    /// The business date the command is made on.
    pub on: NaiveDate,
    pub command: Command,
}

/// What a facultative cession attaches to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacTarget {
    Reservation(String),
    Risk(RiskKey),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacultativeTerms {
    pub target: FacTarget,
    pub reference: String,
    pub reinsurer: ReinsurerId,
    pub amount_minor: i64,
    /// Annual rate per thousand, in millionths.
    pub rate: i64,
    /// Where the reinsurer's terms are filed.
    pub terms_ref: String,
    /// The reinsurer's loading and exclusions for the offer, as opaque
    /// codes. Kept only until the proposal is issued or released.
    #[serde(default)]
    pub loading: Option<String>,
    #[serde(default)]
    pub exclusions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Externally tagged, like the core's commands.
#[serde(rename_all = "snake_case")]
pub enum Command {
    // --- configuration, activated through the log ---
    ActivateTreaty {
        treaty: Box<Treaty>,
    },
    ActivateAggregateTreaty {
        treaty: Box<AggregateTreaty>,
    },
    ActivateRateTable {
        table: Box<ReinsurerRateTable>,
    },

    // --- the core's event streams ---
    /// Events from the partitions, in order per partition. Duplicates are
    /// dropped on the sequence; a gap refuses the batch.
    CoreEvents {
        events: Vec<EventRecord>,
    },

    // --- the capacity step ---
    ReserveCapacity(Box<CapacityRequest>),
    ReleaseCapacity {
        reservation_id: String,
        reason: String,
    },
    ConfirmCapacity {
        reservation_id: String,
        policy: PolicyId,
        cover: String,
        layers: Vec<u32>,
    },
    RecordFacultativeTerms(Box<FacultativeTerms>),
    DeclineFacultative {
        reservation_id: String,
        reference: String,
    },
    /// A controlled release of evidence to a reinsurer. The documents are
    /// named, never copied here.
    LogEvidenceRelease {
        subject: String,
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        purpose: String,
        documents: Vec<String>,
    },
    RecordRiskFactors {
        policy: PolicyId,
        life: LifeId,
        factors: RiskFactors,
    },

    // --- group schemes ---
    SetSchemeTerms {
        policy: PolicyId,
        excess_path: ExcessPath,
        #[serde(default)]
        sites: Vec<(String, u32)>,
    },
    NotePossibleMatch {
        policy: PolicyId,
        life: LifeId,
        party: PartyId,
    },
    ClearPossibleMatch {
        policy: PolicyId,
        life: LifeId,
        party: PartyId,
    },

    // --- claims ---
    Claim(Box<ClaimFact>),
    ReinsurerConsent {
        claim: ClaimId,
        reinsurer: ReinsurerId,
        agreed: bool,
        reference: String,
    },
    ResolveDispute {
        reference: String,
        /// What the insurer repays the reinsurer: nothing, part or all.
        repay_minor: i64,
        agreement_ref: String,
        prepared_by: String,
        approved_by: String,
    },

    // --- treaty changes to cover in force ---
    /// Take cover back from a treaty from a date.
    Recapture {
        treaty: TreatyId,
        effective: NaiveDate,
        keys: Vec<RiskKey>,
    },
    /// A negotiated change: move cover to another treaty version from a date.
    MoveTreatyVersion {
        to: TreatyRef,
        effective: NaiveDate,
        keys: Vec<RiskKey>,
    },

    // --- reinsurer accounts ---
    CutStatement {
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        period_end: NaiveDate,
    },
    AgreeStatement {
        number: u32,
        reference: String,
    },
    HoldSettlement {
        number: u32,
        reason: String,
    },
    ReleaseHold {
        number: u32,
    },
    InstructSettlement {
        number: u32,
        prepared_by: String,
        approved_by: String,
    },
    RecordSettlement {
        number: u32,
        payment_ref: String,
        amount_minor: i64,
    },
    IssueSupplementaryListing {
        number: u32,
        reason: String,
    },
    ReviewProfitCommission {
        treaty: TreatyId,
        reinsurer: ReinsurerId,
    },

    // --- migration ---
    /// A cession as the legacy system held it. Nothing is recalculated.
    LoadMigratedCession {
        key: RiskKey,
        cession: Cession,
    },
    LoadOpeningBalance {
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        partition: PartitionId,
        /// Positive is owed to the reinsurer.
        amount_minor: i64,
        reference: String,
    },
}

impl Command {
    /// The modules allowed to send each command.
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            Command::ActivateTreaty { .. } | Command::ActivateAggregateTreaty { .. } | Command::ActivateRateTable { .. } => &["config"],
            Command::CoreEvents { .. } => &["event-feed"],
            Command::ReserveCapacity(_) | Command::ReleaseCapacity { .. } | Command::ConfirmCapacity { .. } => &["underwriting", "new-business", "group-admin"],
            Command::RecordFacultativeTerms(_) | Command::DeclineFacultative { .. } | Command::LogEvidenceRelease { .. } => {
                &["reinsurance-ops", "underwriting"]
            }
            Command::RecordRiskFactors { .. } => &["new-business", "group-admin", "migration"],
            Command::SetSchemeTerms { .. } | Command::NotePossibleMatch { .. } | Command::ClearPossibleMatch { .. } => &["group-admin"],
            Command::Claim(_) => &["claims"],
            Command::ReinsurerConsent { .. } | Command::Recapture { .. } | Command::MoveTreatyVersion { .. } => &["reinsurance-ops"],
            Command::ResolveDispute { .. } => &["reinsurance-ops", "finance"],
            Command::CutStatement { .. }
            | Command::AgreeStatement { .. }
            | Command::HoldSettlement { .. }
            | Command::ReleaseHold { .. }
            | Command::InstructSettlement { .. }
            | Command::RecordSettlement { .. }
            | Command::IssueSupplementaryListing { .. }
            | Command::ReviewProfitCommission { .. } => &["reinsurance-ops", "finance", "month-end"],
            Command::LoadMigratedCession { .. } | Command::LoadOpeningBalance { .. } => &["migration"],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Command::ActivateTreaty { .. } => "activate_treaty",
            Command::ActivateAggregateTreaty { .. } => "activate_aggregate_treaty",
            Command::ActivateRateTable { .. } => "activate_rate_table",
            Command::CoreEvents { .. } => "core_events",
            Command::ReserveCapacity(_) => "reserve_capacity",
            Command::ReleaseCapacity { .. } => "release_capacity",
            Command::ConfirmCapacity { .. } => "confirm_capacity",
            Command::RecordFacultativeTerms(_) => "record_facultative_terms",
            Command::DeclineFacultative { .. } => "decline_facultative",
            Command::LogEvidenceRelease { .. } => "log_evidence_release",
            Command::RecordRiskFactors { .. } => "record_risk_factors",
            Command::SetSchemeTerms { .. } => "set_scheme_terms",
            Command::NotePossibleMatch { .. } => "note_possible_match",
            Command::ClearPossibleMatch { .. } => "clear_possible_match",
            Command::Claim(_) => "claim",
            Command::ReinsurerConsent { .. } => "reinsurer_consent",
            Command::ResolveDispute { .. } => "resolve_dispute",
            Command::Recapture { .. } => "recapture",
            Command::MoveTreatyVersion { .. } => "move_treaty_version",
            Command::CutStatement { .. } => "cut_statement",
            Command::AgreeStatement { .. } => "agree_statement",
            Command::HoldSettlement { .. } => "hold_settlement",
            Command::ReleaseHold { .. } => "release_hold",
            Command::InstructSettlement { .. } => "instruct_settlement",
            Command::RecordSettlement { .. } => "record_settlement",
            Command::IssueSupplementaryListing { .. } => "issue_supplementary_listing",
            Command::ReviewProfitCommission { .. } => "review_profit_commission",
            Command::LoadMigratedCession { .. } => "load_migrated_cession",
            Command::LoadOpeningBalance { .. } => "load_opening_balance",
        }
    }
}

/// The kinds of entry the core posts for reinsurance. The amount is signed:
/// a negative amount reverses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostingKind {
    /// Debit reinsurance premium expense, credit the reinsurer's account.
    CededPremium,
    /// Debit the reinsurer's account, credit reinsurance commission.
    Commission,
    /// Debit reinsurance recoverable, credit reinsurance recoveries.
    Recoverable,
    /// A recovery now due: debit the reinsurer's account, credit reinsurance
    /// recoverable.
    RecoveryDue,
    /// Debit the reinsurer's account, credit reinsurance recoveries.
    AggregateRecovery,
    /// Debit the reinsurer's account, credit profit commission.
    ProfitCommission,
    /// A disputed recovery settled in the reinsurer's favour: debit
    /// reinsurance recoveries, credit the reinsurer's account.
    DisputeSettlement,
    /// Credit the reinsurer's account against the migration control account.
    OpeningBalance,
}

/// Something for the core to do, on one partition. The ID is the command's
/// idempotency key there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instruction {
    pub id: String,
    pub partition: PartitionId,
    pub body: InstructionBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionBody {
    /// Record this cession on this cover from this date. Replaces whatever
    /// the cover's record says from `from` onwards.
    RecordCession { key: RiskKey, from: NaiveDate, segments: Vec<Segment> },
    Post {
        kind: PostingKind,
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        policy: Option<PolicyId>,
        amount_minor: i64,
        value_date: NaiveDate,
        reference: String,
    },
    /// Move the partition's balance for the reinsurer, as it stood at the
    /// statement's cut, to settlement clearing. Positive is owed to the
    /// reinsurer.
    Sweep {
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        statement: u32,
        cut_seq: u64,
        amount_minor: i64,
    },
}

/// Something another module, or a person, needs to know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    ExceptionOpened {
        id: String,
        code: String,
        detail: String,
    },
    ExceptionClosed {
        id: String,
    },
    /// For group administration: start the underwritten-excess path.
    RequireUnderwrittenExcess {
        policy: PolicyId,
        life: LifeId,
        cover: String,
        excess_minor: i64,
        reference: String,
    },
    FacultativeTermsRecorded {
        reservation_id: String,
    },
    CapacityReleased {
        reservation_id: String,
        reason: String,
    },
    ReleaseLogged {
        subject: String,
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        purpose: String,
        documents: Vec<String>,
    },
    LargeClaimNotification {
        claim: ClaimId,
        treaty: TreatyId,
        reinsurers: Vec<ReinsurerId>,
    },
    ReinsurerConsentRequired {
        claim: ClaimId,
        treaty: TreatyId,
        due_by: NaiveDate,
    },
    ReinsurerConsent {
        claim: ClaimId,
        reinsurer: ReinsurerId,
        agreed: bool,
        reference: String,
    },
    DisputedItemRaised {
        claim: ClaimId,
        reinsurer: ReinsurerId,
        reference: String,
    },
    DisputeResolved {
        reference: String,
        repay_minor: i64,
    },
    /// A statement, listing or supplementary listing to file in write-once
    /// storage and send. `sha256` is the hash of `body`.
    Document {
        name: String,
        sha256: String,
        body: String,
    },
    SupplementaryListingDue {
        statement: u32,
    },
    /// For the payments module: one net payment. Positive is paid to the
    /// reinsurer, negative is collected from it.
    SettlementDue {
        statement: u32,
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        currency: ic_money::Currency,
        net_minor: i64,
    },
}

/// The result of applying one command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub index: u64,
    pub result: Result<(), Reject>,
    pub instructions: Vec<Instruction>,
    pub notices: Vec<Notice>,
    pub duplicate: bool,
}

/// An open item for a person to resolve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExceptionItem {
    pub code: String,
    pub detail: String,
    pub opened_on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyInfo {
    pub number: String,
    pub product: String,
    pub partition: PartitionId,
    pub lives: BTreeMap<LifeId, PartyId>,
    /// Since when the policy has not been in force, if it is not.
    #[serde(default)]
    pub out_of_force_from: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemeTerms {
    pub excess_path: ExcessPath,
    pub sites: Vec<(String, u32)>,
}

/// The covers on one life and the cessions recorded for them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeBook {
    #[serde(with = "pairs")]
    pub risks: BTreeMap<RiskKey, Risk>,
    #[serde(with = "pairs")]
    pub recorded: BTreeMap<RiskKey, Vec<Segment>>,
}

pub(crate) struct Ctx {
    pub index: u64,
    pub on: NaiveDate,
    pub instructions: Vec<Instruction>,
    pub notices: Vec<Notice>,
    /// Lives whose covers changed in this command.
    pub dirty: BTreeSet<LifeKey>,
}

impl Ctx {
    pub fn instruct(&mut self, partition: PartitionId, body: InstructionBody) {
        let id = format!("ri-{}-{}", self.index, self.instructions.len() + 1);
        self.instructions.push(Instruction { id, partition, body });
    }
}

/// The module's whole replicated state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reinsurance {
    pub(crate) treaties: BTreeMap<TreatyId, BTreeMap<u32, Treaty>>,
    pub(crate) aggregates: BTreeMap<TreatyId, AggregateTreaty>,
    pub(crate) rate_tables: BTreeMap<String, BTreeMap<u32, ReinsurerRateTable>>,

    pub(crate) checkpoints: BTreeMap<PartitionId, u64>,
    pub(crate) policies: BTreeMap<PolicyId, PolicyInfo>,
    #[serde(with = "pairs")]
    pub(crate) lives: BTreeMap<LifeKey, LifeBook>,
    #[serde(with = "pairs")]
    pub(crate) risk_life: BTreeMap<RiskKey, LifeKey>,
    #[serde(with = "pairs")]
    pub(crate) factors: BTreeMap<(PolicyId, LifeId), RiskFactors>,
    pub(crate) schemes: BTreeMap<PolicyId, SchemeTerms>,
    #[serde(with = "pairs")]
    pub(crate) possible: BTreeMap<(PolicyId, LifeId), BTreeSet<PartyId>>,
    pub(crate) reservations: BTreeMap<String, Reservation>,

    #[serde(with = "pairs")]
    pub(crate) periods: BTreeMap<(PolicyId, NaiveDate), Period>,

    /// Facultative terms waiting for the cover they were given for.
    #[serde(with = "pairs")]
    pub(crate) pending_terms: BTreeMap<(PolicyId, String), PendingTerms>,
    /// Confirmed reservations standing in for layers not yet seen.
    #[serde(with = "pairs")]
    pub(crate) confirming: BTreeMap<(PolicyId, String), String>,
    pub(crate) held_until: BTreeSet<(NaiveDate, String)>,

    #[serde(with = "pairs")]
    pub(crate) pending: BTreeMap<Bucket, i64>,
    #[serde(with = "pairs")]
    pub(crate) booked: BTreeMap<(TreatyId, ReinsurerId, LineKind), i64>,
    pub(crate) statements: Vec<Statement>,
    /// Listing rows that changed after a statement was sent, as sent.
    #[serde(with = "pairs")]
    pub(crate) supplements: BTreeMap<(u32, RiskKey), Option<(i64, i64)>>,
    #[serde(with = "pairs")]
    pub(crate) profit_commission: BTreeMap<(TreatyId, ReinsurerId), PcState>,

    pub(crate) claims: BTreeMap<ClaimId, ClaimBook>,
    pub(crate) policy_claims: BTreeMap<PolicyId, BTreeSet<ClaimId>>,
    pub(crate) disputes: BTreeMap<String, DisputedItem>,
    #[serde(with = "pairs")]
    pub(crate) aggregate_net: BTreeMap<(TreatyId, String), (u32, i64)>,
    #[serde(with = "pairs")]
    pub(crate) aggregate_posted: BTreeMap<(TreatyId, String, ReinsurerId), i64>,

    pub(crate) exceptions: BTreeMap<String, ExceptionItem>,
    pub(crate) releases_logged: u64,
    pub(crate) idempotency: BTreeMap<String, Option<Reject>>,
}

impl Reinsurance {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hash of the whole state. Two replicas that applied the same log agree
    /// on it exactly.
    pub fn state_hash(&self) -> Hash32 {
        let mut h = Sha256::new();
        h.update(b"ic.reinsurance.state.v1");
        // Every map in the state has string keys or is written as pairs, so
        // this cannot fail.
        h.update(serde_json::to_vec(self).unwrap_or_else(|_| b"unencodable".to_vec()));
        Hash32(h.finalize().into())
    }

    // --- configuration ---

    pub fn treaty(&self, r: &TreatyRef) -> Option<&Treaty> {
        self.treaties.get(&r.id).and_then(|v| v.get(&r.version))
    }

    pub(crate) fn latest_treaty(&self, id: &TreatyId) -> Option<&Treaty> {
        self.treaties.get(id).and_then(|v| v.values().next_back())
    }

    /// The treaty version that cover of this kind starting on `on` is
    /// written under.
    pub fn select_treaty(&self, product: &str, cover_type: &str, on: NaiveDate) -> Option<&Treaty> {
        self.treaties
            .values()
            .filter_map(|versions| {
                versions
                    .values()
                    .filter(|t| t.covers(product, cover_type) && t.open_on(on))
                    .max_by_key(|t| (t.effective_from, t.version))
            })
            .max_by(|a, b| (a.effective_from, &b.id).cmp(&(b.effective_from, &a.id)))
    }

    pub(crate) fn rate_table(&self, id: &str, on: NaiveDate) -> Option<&ReinsurerRateTable> {
        self.rate_tables.get(id)?.values().rev().find(|t| t.effective_from <= on)
    }

    fn activate_treaty(&mut self, treaty: Treaty) -> Result<(), Reject> {
        treaty.validate().map_err(|e| Reject::new("invalid_treaty", e))?;
        let versions = self.treaties.entry(treaty.id.clone()).or_default();
        activate(versions, treaty.version, treaty, |t| &t.content_hash)
    }

    fn activate_aggregate(&mut self, treaty: AggregateTreaty) -> Result<(), Reject> {
        treaty.validate().map_err(|e| Reject::new("invalid_treaty", e))?;
        match self.aggregates.get(&treaty.id) {
            Some(have) if have.version > treaty.version || (have.version == treaty.version && have.content_hash != treaty.content_hash) => Err(Reject::new(
                "config_conflict",
                format!(
                    "aggregate treaty {} is already active at version {} with other content",
                    treaty.id, have.version
                ),
            )),
            _ => {
                self.aggregates.insert(treaty.id.clone(), treaty);
                Ok(())
            }
        }
    }

    fn activate_rate_table(&mut self, table: ReinsurerRateTable) -> Result<(), Reject> {
        table.validate().map_err(|e| Reject::new("invalid_rate_table", e))?;
        let versions = self.rate_tables.entry(table.id.clone()).or_default();
        activate(versions, table.version, table, |t| &t.content_hash)
    }

    // --- exceptions ---

    /// Open, update or close an exception item so that it says `detail`.
    pub(crate) fn set_exception(&mut self, ctx: &mut Ctx, id: String, code: &str, detail: Option<String>) {
        match detail {
            Some(detail) => {
                if self.exceptions.get(&id).is_some_and(|e| e.detail == detail) {
                    return;
                }
                ctx.notices.push(Notice::ExceptionOpened {
                    id: id.clone(),
                    code: code.to_string(),
                    detail: detail.clone(),
                });
                let opened_on = self.exceptions.get(&id).map_or(ctx.on, |e| e.opened_on);
                self.exceptions.insert(
                    id,
                    ExceptionItem {
                        code: code.to_string(),
                        detail,
                        opened_on,
                    },
                );
            }
            None => {
                if self.exceptions.remove(&id).is_some() {
                    ctx.notices.push(Notice::ExceptionClosed { id });
                }
            }
        }
    }

    pub fn exceptions(&self) -> &BTreeMap<String, ExceptionItem> {
        &self.exceptions
    }

    // --- the per-life accumulation ---

    pub(crate) fn life_of(&self, policy: &PolicyId, life: &LifeId) -> LifeKey {
        match self.policies.get(policy).and_then(|p| p.lives.get(life)) {
            Some(party) => LifeKey::Party(party.clone()),
            None => LifeKey::Unmatched {
                policy: policy.clone(),
                life: life.clone(),
            },
        }
    }

    pub(crate) fn risk(&self, key: &RiskKey) -> Option<&Risk> {
        self.lives.get(self.risk_life.get(key)?)?.risks.get(key)
    }

    pub(crate) fn risk_mut(&mut self, ctx: &mut Ctx, key: &RiskKey) -> Option<&mut Risk> {
        let life = self.risk_life.get(key)?;
        ctx.dirty.insert(life.clone());
        self.lives.get_mut(life)?.risks.get_mut(key)
    }

    /// The keys of every cover on a policy, reservations excluded.
    pub(crate) fn policy_risks(&self, policy: &PolicyId) -> Vec<RiskKey> {
        let from = RiskKey {
            policy: policy.clone(),
            cover: String::new(),
            layer: 0,
            life: LifeId(String::new()),
        };
        self.risk_life
            .range(from..)
            .take_while(|(k, _)| &k.policy == policy)
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub(crate) fn put_risk(&mut self, ctx: &mut Ctx, risk: Risk) {
        let life = self.life_of(&risk.key.policy, &risk.key.life);
        self.put_risk_on(ctx, life, risk);
    }

    pub(crate) fn put_risk_on(&mut self, ctx: &mut Ctx, life: LifeKey, risk: Risk) {
        ctx.dirty.insert(life.clone());
        self.risk_life.insert(risk.key.clone(), life.clone());
        self.lives.entry(life).or_default().risks.insert(risk.key.clone(), risk);
    }

    /// Move a member's covers to the life they have been linked to.
    pub(crate) fn relink(&mut self, ctx: &mut Ctx, policy: &PolicyId, life: &LifeId) {
        let to = self.life_of(policy, life);
        let keys: Vec<RiskKey> = self.policy_risks(policy).into_iter().filter(|k| &k.life == life).collect();
        for key in keys {
            let Some(from) = self.risk_life.get(&key).cloned().filter(|f| f != &to) else {
                continue;
            };
            let Some(book) = self.lives.get_mut(&from) else { continue };
            let risk = book.risks.remove(&key);
            let recorded = book.recorded.remove(&key);
            if book.risks.is_empty() {
                self.lives.remove(&from);
            } else {
                ctx.dirty.insert(from);
            }
            let target = self.lives.entry(to.clone()).or_default();
            if let Some(recorded) = recorded {
                target.recorded.insert(key.clone(), recorded);
            }
            if let Some(risk) = risk {
                target.risks.insert(key.clone(), risk);
            }
            self.risk_life.insert(key, to.clone());
            ctx.dirty.insert(to.clone());
        }
    }

    /// Recalculate the cessions on a life and send the core whatever
    /// changed, from the first date it changed.
    fn recompute(&mut self, ctx: &mut Ctx, life: &LifeKey) {
        let Some(book) = self.lives.get(life) else { return };
        let matched = matches!(life, LifeKey::Party(_));
        let real = book.risks.values().filter(|r| r.source != Source::Reservation);
        let new = timeline(real, |r| self.treaty(r), matched);
        let mut changed: Vec<(RiskKey, NaiveDate, Vec<Segment>)> = Vec::new();
        for (key, segments) in &new {
            let old = book.recorded.get(key).map_or(&[][..], Vec::as_slice);
            if let Some(from) = first_difference(old, segments) {
                changed.push((key.clone(), from, old.to_vec()));
            }
        }
        let positions: Vec<(RiskKey, PartitionId, Option<(i64, UnplacedReason)>)> = new
            .iter()
            .filter_map(|(key, segments)| {
                let risk = book.risks.get(key)?;
                let unplaced = segments
                    .last()
                    .and_then(|(_, c)| c.as_ref())
                    .and_then(|c| c.unplaced_reason.map(|r| (c.unplaced_minor, r)));
                Some((key.clone(), risk.partition, unplaced))
            })
            .collect();
        for (key, from, _) in &changed {
            let segments: Vec<Segment> = new[key].iter().filter(|(d, _)| d >= from).cloned().collect();
            let partition = positions.iter().find(|(k, _, _)| k == key).map_or(0, |(_, p, _)| *p);
            ctx.instruct(
                partition,
                InstructionBody::RecordCession {
                    key: key.clone(),
                    from: *from,
                    segments,
                },
            );
        }
        if let Some(book) = self.lives.get_mut(life) {
            book.recorded = new;
        }
        // Never silently over-retained: anything over the retention with no
        // reinsurance in place is an open item until it is placed.
        for (key, _, unplaced) in positions {
            let id = format!("over_retention:{key}");
            let was = self.exceptions.get(&id).map(|e| e.detail.clone());
            let detail = unplaced.map(|(amount, reason)| format!("{amount} over the retention on {life} has no reinsurance: {reason:?}"));
            if let (Some((amount, UnplacedReason::UnderwrittenExcessPending)), true) = (unplaced, was != detail) {
                ctx.notices.push(Notice::RequireUnderwrittenExcess {
                    policy: key.policy.clone(),
                    life: key.life.clone(),
                    cover: key.cover.clone(),
                    excess_minor: amount,
                    reference: format!("UWX/{key}/{amount}"),
                });
            }
            self.set_exception(ctx, id, "over_retention", detail);
        }
        for (key, from, was) in changed {
            self.reprice_from(ctx, &key.policy, from);
            self.resync_claims(ctx, &key, from);
            self.flag_supplementary(ctx, &key, from, &was);
        }
    }

    /// The cession recorded for a cover on a date.
    pub fn cession(&self, key: &RiskKey, on: NaiveDate) -> Option<&Cession> {
        let segments = self.lives.get(self.risk_life.get(key)?)?.recorded.get(key)?;
        crate::cession::cession_on(segments, on)
    }

    /// The whole recorded timeline for a cover.
    pub fn cession_record(&self, key: &RiskKey) -> &[Segment] {
        self.risk_life
            .get(key)
            .and_then(|l| self.lives.get(l))
            .and_then(|b| b.recorded.get(key))
            .map_or(&[], Vec::as_slice)
    }

    pub fn checkpoint(&self, partition: PartitionId) -> u64 {
        self.checkpoints.get(&partition).copied().unwrap_or(0)
    }

    fn recapture(&mut self, ctx: &mut Ctx, treaty: &TreatyId, to: Option<&TreatyRef>, effective: NaiveDate, keys: &[RiskKey]) -> Result<(), Reject> {
        for key in keys {
            let risk = self.risk(key).ok_or_else(|| Reject::new("unknown_risk", format!("no cover {key}")))?;
            let current = risk
                .treaty_on(effective)
                .ok_or_else(|| Reject::new("not_ceded", format!("{key} is not under a treaty on {effective}")))?;
            if &current.id != treaty || effective < risk.start {
                return Err(Reject::new("not_ceded", format!("{key} is not under treaty {treaty} on {effective}")));
            }
            if to.is_none() {
                let years = self.treaty(current).and_then(|t| t.recapture_after_years);
                let earliest = years.and_then(|y| risk.start.checked_add_months(chrono::Months::new(12 * y)));
                if earliest.is_none_or(|e| effective < e) {
                    return Err(Reject::new(
                        "recapture_not_allowed",
                        format!("the treaty does not allow {key} to be recaptured on {effective}"),
                    ));
                }
            }
        }
        for key in keys {
            if let Some(risk) = self.risk_mut(ctx, key) {
                risk.treaties.retain(|(d, _)| *d < effective);
                risk.treaties.push((effective, to.cloned()));
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, ctx: &mut Ctx, command: Command) -> Result<(), Reject> {
        match command {
            Command::ActivateTreaty { treaty } => self.activate_treaty(*treaty),
            Command::ActivateAggregateTreaty { treaty } => self.activate_aggregate(*treaty),
            Command::ActivateRateTable { table } => self.activate_rate_table(*table),
            Command::CoreEvents { events } => self.core_events(ctx, events),
            Command::ReserveCapacity(request) => self.reserve(ctx, *request),
            Command::ReleaseCapacity { reservation_id, reason } => self.release(ctx, &reservation_id, &reason),
            Command::ConfirmCapacity {
                reservation_id,
                policy,
                cover,
                layers,
            } => self.confirm(ctx, &reservation_id, policy, cover, layers),
            Command::RecordFacultativeTerms(terms) => self.record_facultative(ctx, *terms),
            Command::DeclineFacultative { reservation_id, reference } => self.decline_facultative(&reservation_id, reference),
            Command::LogEvidenceRelease {
                subject,
                treaty,
                reinsurer,
                purpose,
                documents,
            } => {
                if purpose.is_empty() || documents.is_empty() || self.latest_treaty(&treaty).is_none() {
                    return Err(Reject::new(
                        "invalid_release",
                        "a release needs a purpose, the documents released and the treaty that authorises it",
                    ));
                }
                self.log_release(ctx, subject, treaty, reinsurer, purpose, documents);
                Ok(())
            }
            Command::RecordRiskFactors { policy, life, factors } => {
                self.factors.insert((policy.clone(), life), factors);
                self.reprice_from(ctx, &policy, NaiveDate::MIN);
                Ok(())
            }
            Command::SetSchemeTerms { policy, excess_path, sites } => {
                for key in self.policy_risks(&policy) {
                    if let Some(risk) = self.risk_mut(ctx, &key) {
                        risk.excess_path = Some(excess_path);
                    }
                }
                self.schemes.insert(policy, SchemeTerms { excess_path, sites });
                Ok(())
            }
            Command::NotePossibleMatch { policy, life, party } => {
                let id = format!("possible_match:{policy}/{life}/{party}");
                let detail =
                    format!("member {life} of {policy} may be party {party}; their cover is not counted against that party's retention until someone confirms");
                self.set_exception(ctx, id, "possible_match", Some(detail));
                self.possible.entry((policy, life)).or_default().insert(party);
                Ok(())
            }
            Command::ClearPossibleMatch { policy, life, party } => {
                self.clear_possible(ctx, &policy, &life, Some(&party));
                Ok(())
            }
            Command::Claim(fact) => self.claim_fact(ctx, *fact),
            Command::ReinsurerConsent {
                claim,
                reinsurer,
                agreed,
                reference,
            } => self.reinsurer_consent(ctx, claim, reinsurer, agreed, reference),
            Command::ResolveDispute {
                reference,
                repay_minor,
                agreement_ref,
                prepared_by,
                approved_by,
            } => self.resolve_dispute(ctx, &reference, repay_minor, agreement_ref, &prepared_by, &approved_by),
            Command::Recapture { treaty, effective, keys } => self.recapture(ctx, &treaty, None, effective, &keys),
            Command::MoveTreatyVersion { to, effective, keys } => {
                if self.treaty(&to).is_none() {
                    return Err(Reject::new("unknown_treaty", format!("treaty {to} is not active")));
                }
                self.recapture(ctx, &to.id.clone(), Some(&to), effective, &keys)
            }
            Command::CutStatement { treaty, reinsurer, period_end } => self.cut_statement(ctx, treaty, reinsurer, period_end),
            Command::AgreeStatement { number, reference } => self.agree_statement(number, reference),
            Command::HoldSettlement { number, reason } => self.hold_settlement(number, Some(reason)),
            Command::ReleaseHold { number } => self.hold_settlement(number, None),
            Command::InstructSettlement {
                number,
                prepared_by,
                approved_by,
            } => self.instruct_settlement(ctx, number, &prepared_by, &approved_by),
            Command::RecordSettlement {
                number,
                payment_ref,
                amount_minor,
            } => self.record_settlement(ctx, number, payment_ref, amount_minor),
            Command::IssueSupplementaryListing { number, reason } => self.issue_supplementary(ctx, number, reason),
            Command::ReviewProfitCommission { treaty, reinsurer } => self.review_profit_commission(ctx, treaty, reinsurer),
            Command::LoadMigratedCession { key, cession } => {
                if !cession.balanced() {
                    return Err(Reject::new("unbalanced_cession", "the parts of the cession do not add up to its sum at risk"));
                }
                let risk = self.risk_mut(ctx, &key).ok_or_else(|| Reject::new("unknown_risk", format!("no cover {key}")))?;
                if let Some(treaty) = &cession.treaty {
                    risk.treaties = vec![(risk.start, Some(treaty.clone()))];
                }
                risk.migrated = Some(cession);
                Ok(())
            }
            Command::LoadOpeningBalance {
                treaty,
                reinsurer,
                partition,
                amount_minor,
                reference,
            } => {
                if self.latest_treaty(&treaty).is_none() {
                    return Err(Reject::new("unknown_treaty", format!("treaty {treaty} is not active")));
                }
                let on = ctx.on;
                self.book(ctx, LineKind::OpeningBalance, &treaty, &reinsurer, partition, None, on, amount_minor, reference);
                Ok(())
            }
        }
    }

    pub(crate) fn clear_possible(&mut self, ctx: &mut Ctx, policy: &PolicyId, life: &LifeId, party: Option<&PartyId>) {
        let key = (policy.clone(), life.clone());
        let Some(parties) = self.possible.get_mut(&key) else { return };
        let cleared: Vec<PartyId> = parties.iter().filter(|p| party.is_none_or(|q| q == *p)).cloned().collect();
        parties.retain(|p| !cleared.contains(p));
        if parties.is_empty() {
            self.possible.remove(&key);
        }
        for p in cleared {
            self.set_exception(ctx, format!("possible_match:{policy}/{life}/{p}"), "possible_match", None);
        }
    }

    pub(crate) fn log_release(&mut self, ctx: &mut Ctx, subject: String, treaty: TreatyId, reinsurer: ReinsurerId, purpose: String, documents: Vec<String>) {
        self.releases_logged += 1;
        ctx.notices.push(Notice::ReleaseLogged {
            subject,
            treaty,
            reinsurer,
            purpose,
            documents,
        });
    }

    /// Apply one logged command.
    pub fn apply_envelope(&mut self, index: u64, env: Envelope) -> Applied {
        if let Some(stored) = self.idempotency.get(&env.key) {
            return Applied {
                index,
                result: stored.clone().map_or(Ok(()), Err),
                instructions: vec![],
                notices: vec![],
                duplicate: true,
            };
        }
        let mut ctx = Ctx {
            index,
            on: env.on,
            instructions: vec![],
            notices: vec![],
            dirty: BTreeSet::new(),
        };
        let result = if env.command.allowed_callers().contains(&env.caller.as_str()) {
            self.expire_reservations(&mut ctx);
            // A refused command changes nothing: every handler checks before
            // it changes anything, and what it had queued is dropped.
            let kept = (ctx.instructions.len(), ctx.notices.len());
            let result = self.dispatch(&mut ctx, env.command);
            if result.is_err() {
                ctx.instructions.truncate(kept.0);
                ctx.notices.truncate(kept.1);
            }
            while let Some(life) = ctx.dirty.pop_first() {
                self.recompute(&mut ctx, &life);
            }
            result
        } else {
            Err(Reject::new(
                "caller_not_allowed",
                format!("module {} may not send {}", env.caller, env.command.name()),
            ))
        };
        self.idempotency.insert(env.key, result.clone().err());
        Applied {
            index,
            result,
            instructions: ctx.instructions,
            notices: ctx.notices,
            duplicate: false,
        }
    }
}

/// Activate one version of versioned configuration. An active version is
/// immutable: the same content again is accepted, other content is refused.
fn activate<T>(versions: &mut BTreeMap<u32, T>, version: u32, item: T, hash: impl Fn(&T) -> &String) -> Result<(), Reject> {
    match versions.get(&version) {
        Some(have) if hash(have) != hash(&item) => Err(Reject::new(
            "config_conflict",
            format!("version {version} is already active with other content"),
        )),
        Some(_) => Ok(()),
        None => {
            versions.insert(version, item);
            Ok(())
        }
    }
}

/// The first date from which two timelines differ.
fn first_difference(old: &[Segment], new: &[Segment]) -> Option<NaiveDate> {
    let mut i = 0;
    while i < old.len() && i < new.len() && old[i] == new[i] {
        i += 1;
    }
    match (old.get(i), new.get(i)) {
        (None, None) => None,
        (Some((a, _)), Some((b, _))) => Some(*a.min(b)),
        (Some((a, _)), None) | (None, Some((a, _))) => Some(*a),
    }
}

impl StateMachine for Reinsurance {
    type Command = Envelope;
    type Output = Applied;
    fn apply(&mut self, index: u64, command: Envelope) -> Applied {
        self.apply_envelope(index, command)
    }
}
