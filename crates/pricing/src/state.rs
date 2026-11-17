//! The unit pricing state machine.
//!
//! Deterministic: it changes only by applying a logged [`Envelope`], reads no
//! clock (time comes in the envelope) and keeps every map ordered, so every
//! member of the module's group holds the same state and the same hash.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Days, NaiveDate, Utc};
use ic_ledger::PartitionId;
use ic_money::{DecStr, FundId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::command::*;
use crate::fund::{FundDefinition, FundRules, PricingTerms};
use crate::strike::{error_size, strike, CheckKind, Previous, Struck};
use crate::valuation::Valuation;

/// How long an idempotency key is remembered. A retry comes within minutes;
/// this is far beyond any.
pub const KEY_RETENTION_DAYS: u64 = 35;
/// How many published events are kept for readers to catch up on.
pub const EVENT_RETENTION: usize = 50_000;
/// How long a movement ID is remembered after its pricing date, so the same
/// movement offered again is not counted twice.
pub const MOVEMENT_RETENTION_DAYS: u64 = 62;
/// The furthest back the late price question looks.
pub const LATE_LOOKBACK_DAYS: u64 = 60;

/// The key for one fund on one pricing date.
pub fn slot(fund: &FundId, date: NaiveDate) -> String {
    format!("{}:{date}", fund.0)
}

/// One valuation as it was received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValuationRecord {
    pub revision: u32,
    pub file_id: String,
    pub administrator: String,
    pub received_at: DateTime<Utc>,
    pub valuation: Valuation,
}

/// One partition's units on issue of a fund, as the core last reported them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreUnits {
    pub executed_through: Option<NaiveDate>,
    pub units_micro: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceStatus {
    /// Every check passed: waiting for an approver.
    Prepared,
    /// A check failed: an approver may accept each failure with a reason.
    Held,
    /// Units on issue do not agree: nobody may approve it.
    Blocked,
    /// Approved and on its way to every partition.
    Activating,
    Active,
    /// Replaced by a corrected version.
    Superseded,
    Rejected,
    /// A newer draft for the same fund and date replaced it.
    Replaced,
    /// A corrected price whose error was below the materiality threshold:
    /// recorded, never activated, and the wrong price stands.
    Immaterial,
}

/// A price, from preparation to activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub id: String,
    pub fund: FundId,
    pub pricing_date: NaiveDate,
    pub definition_version: u32,
    pub valuation_revision: u32,
    pub previous: Option<Previous>,
    pub core_units_micro: Option<i64>,
    pub struck: Struck,
    pub status: PriceStatus,
    pub prepared_by: String,
    pub prepared_at: DateTime<Utc>,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub approved_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub accepted: BTreeMap<CheckKind, String>,
    #[serde(default)]
    pub rejected: Option<String>,
    #[serde(default)]
    pub version: Option<u32>,
    #[serde(default)]
    pub supersedes: Option<String>,
    /// The price error this price corrects.
    #[serde(default)]
    pub corrects: Option<String>,
    #[serde(default)]
    pub deliveries: BTreeMap<PartitionId, Delivery>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub activated_at: Option<DateTime<Utc>>,
}

/// A copy of a price waiting to be carried to one partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    pub id: String,
    pub seq: u64,
    pub partition: PartitionId,
    pub price: String,
    pub fund: FundId,
    pub pricing_date: NaiveDate,
    pub version: u32,
    pub buy: DecStr,
    pub sell: DecStr,
    pub mid: DecStr,
    pub inputs_hash: String,
    pub supersedes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suspension {
    pub from: NaiveDate,
    #[serde(default)]
    pub to: Option<NaiveDate>,
    pub reason: String,
    pub by: String,
}

/// How a wrong price was put right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correction {
    pub price: String,
    /// The error's size as a fraction of the correct price.
    pub size: DecStr,
    /// The fund's threshold when the correction was approved.
    pub threshold: DecStr,
    pub material: bool,
}

/// What the replay of orders at the corrected prices did, from the core's events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remediation {
    pub policies: BTreeSet<String>,
    /// Orders replayed, compensated or not.
    pub movements: u64,
    /// Orders whose difference was under the compensation minimum.
    pub uncompensated: u64,
    pub units_micro: i64,
    pub cash_minor: i64,
    /// What the insurer paid the fund to make it whole.
    pub borne_minor: i64,
}

/// A price error and its correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceError {
    pub id: String,
    pub fund: FundId,
    pub cause: String,
    pub detected_on: NaiveDate,
    pub raised_by: String,
    pub raised_at: DateTime<Utc>,
    /// Each wrong price and, once approved, its correction.
    pub prices: BTreeMap<String, Option<Correction>>,
    #[serde(default)]
    pub corrected_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub remediation: Remediation,
    #[serde(default)]
    pub closed: Option<(String, DateTime<Utc>, String)>,
}

impl PriceError {
    fn open(&self) -> bool {
        self.closed.is_none()
    }

    /// Every wrong price has a correction that is in force (active or immaterial).
    pub fn corrected(&self) -> bool {
        self.corrected_at.is_some()
    }
}

/// The day's applications and redemptions of a fund.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flow {
    pub applications_minor: i64,
    pub redemptions_minor: i64,
    pub orders: u64,
}

/// A holdings extract and net flow instruction for one fund and date, due
/// once every partition has executed that date's orders. The holdings are
/// taken then, and kept only until the file is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extract {
    pub id: String,
    pub fund: FundId,
    pub pricing_date: NaiveDate,
    pub price: Option<String>,
    pub holdings: BTreeMap<String, i64>,
    pub units_on_issue_micro: i64,
    pub holders: u64,
    pub flow: Flow,
    #[serde(default)]
    pub written: Option<(String, String)>,
}

/// The whole state of the module.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingState {
    pub partitions: Option<PartitionId>,
    pub funds: BTreeMap<FundId, Vec<FundDefinition>>,
    /// The date each fund's first version was activated: it prices from then.
    pub fund_activated: BTreeMap<FundId, NaiveDate>,
    pub valuations: BTreeMap<String, Vec<ValuationRecord>>,
    pub refused_files: BTreeMap<String, String>,
    /// Fund definitions the core activated that this module refused, by
    /// `fund:version`, until a later version of the fund is taken.
    #[serde(default)]
    pub refused_definitions: BTreeMap<String, String>,
    pub core_units: BTreeMap<FundId, BTreeMap<PartitionId, CoreUnits>>,
    pub prices: BTreeMap<String, Price>,
    /// Drafts prepared for each slot, for numbering.
    pub drafts: BTreeMap<String, u32>,
    /// Active versions of each slot, oldest first. The last is in force.
    pub active: BTreeMap<String, Vec<String>>,
    pub suspensions: BTreeMap<FundId, Vec<Suspension>>,
    pub errors: BTreeMap<String, PriceError>,
    pub publications: BTreeMap<String, Publication>,
    pub publication_seq: u64,
    pub holdings: BTreeMap<FundId, BTreeMap<String, i64>>,
    pub flows: BTreeMap<String, Flow>,
    pub movement_ids: BTreeMap<String, NaiveDate>,
    pub executed: BTreeMap<String, BTreeSet<PartitionId>>,
    pub extracts: BTreeMap<String, Extract>,
    pub events: Vec<EventRecord>,
    pub event_seq: u64,
    pub applied: BTreeMap<String, (NaiveDate, Output)>,
}

type Step = Result<Output, Output>;

fn refuse(code: &str, message: impl Into<String>) -> Output {
    Output::refused(code, message)
}

impl ic_log::StateMachine for PricingState {
    type Command = Envelope;
    type Output = Output;

    fn apply(&mut self, _index: u64, envelope: Envelope) -> Output {
        self.apply_envelope(envelope)
    }
}

impl PricingState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state_hash(&self) -> String {
        // Every map key is a string or a number, so this cannot fail.
        hex::encode(Sha256::digest(serde_json::to_vec(self).unwrap_or_default()))
    }

    /// Apply one envelope. A repeated key returns the first result and changes nothing.
    pub fn apply_envelope(&mut self, envelope: Envelope) -> Output {
        if let Some((_, output)) = self.applied.get(&envelope.key) {
            return output.clone();
        }
        let today = envelope.at.date_naive();
        let output = match self.authorise(&envelope) {
            Err(refusal) => refusal,
            // Every command checks everything before it writes, so a
            // refusal leaves no trace (the tests check this for each one).
            Ok(()) => self.execute(&envelope).unwrap_or_else(|refusal| refusal),
        };
        if let Some(cutoff) = today.checked_sub_days(Days::new(KEY_RETENTION_DAYS)) {
            self.applied.retain(|_, (day, _)| *day >= cutoff);
        }
        if let Command::ActivateFund(def) = &envelope.command {
            match &output {
                Output::Refused { code, message } if envelope.caller == CALLER_EVENTS => {
                    self.refused_definitions
                        .insert(format!("{}:{}", def.id.0, def.version), format!("{code}: {message}"));
                }
                Output::Done => {
                    let prefix = format!("{}:", def.id.0);
                    self.refused_definitions.retain(|k, _| !k.starts_with(&prefix));
                }
                _ => {}
            }
        }
        self.applied.insert(envelope.key, (today, output.clone()));
        output
    }

    fn authorise(&self, e: &Envelope) -> Result<(), Output> {
        if e.key.is_empty() || e.actor.trim().is_empty() {
            return Err(refuse("unauthorised", "a command needs a key and an actor"));
        }
        if !e.command.allowed_callers().contains(&e.caller.as_str()) {
            return Err(refuse("unauthorised", format!("{} may not send {}", e.caller, e.command.name())));
        }
        if let Some(role) = e.command.role() {
            if !e.roles.iter().any(|r| r == role) {
                return Err(refuse("unauthorised", format!("{} needs the role {role}", e.command.name())));
            }
        }
        Ok(())
    }

    fn execute(&mut self, e: &Envelope) -> Step {
        match &e.command {
            Command::ActivateFund(def) => self.activate_fund(def, e.at.date_naive()),
            Command::RecordPartitions { count } => self.record_partitions(*count),
            Command::ReceiveValuations {
                administrator,
                file_id,
                valuations,
            } => self.receive(e.at, administrator, file_id, valuations),
            Command::RefuseValuationFile { file_id, reason } => {
                self.refused_files.insert(file_id.clone(), reason.clone());
                Ok(Output::Done)
            }
            Command::RecordCoreUnits {
                fund,
                partition,
                executed_through,
                units_micro,
            } => {
                self.partition_known(*partition)?;
                self.definition(fund)?;
                self.core_units.entry(fund.clone()).or_default().insert(
                    *partition,
                    CoreUnits {
                        executed_through: *executed_through,
                        units_micro: *units_micro,
                    },
                );
                if let Some(through) = executed_through {
                    self.record_executed_through(fund, *partition, *through)?;
                }
                Ok(Output::Done)
            }
            Command::PreparePrice { fund, pricing_date } => self.prepare(e, fund, *pricing_date),
            Command::ApprovePrice { price, accepted } => self.approve(e, price, accepted),
            Command::RejectPrice { price, reason } => {
                let p = self.price_mut(price)?;
                if !matches!(p.status, PriceStatus::Prepared | PriceStatus::Held | PriceStatus::Blocked) {
                    return Err(refuse("wrong_status", format!("{price} is {:?}", p.status)));
                }
                if reason.trim().is_empty() {
                    return Err(refuse("reason_required", "a rejection needs a reason"));
                }
                p.status = PriceStatus::Rejected;
                p.rejected = Some(format!("{}: {reason}", e.actor));
                Ok(Output::Done)
            }
            Command::SuspendFund { fund, from, reason } => self.suspend(e, fund, *from, reason),
            Command::ResumeFund { fund, from, reason } => self.resume(e, fund, *from, reason),
            Command::RaisePriceError {
                error,
                prices,
                cause,
                detected_on,
            } => self.raise_error(e, error, prices, cause, *detected_on),
            Command::CloseError { error, summary } => {
                let err = self.errors.get_mut(error).ok_or_else(|| refuse("unknown_error", error.clone()))?;
                if !err.open() {
                    return Err(refuse("closed", format!("{error} is closed")));
                }
                if !err.corrected() {
                    return Err(refuse("not_corrected", "every wrong price needs a correction in force first"));
                }
                if summary.trim().is_empty() {
                    return Err(refuse("reason_required", "closing an error needs a summary"));
                }
                err.closed = Some((e.actor.clone(), e.at, summary.clone()));
                self.publish(e.at, Event::PriceErrorClosed { error: error.clone() });
                Ok(Output::Done)
            }
            Command::Republish { price } => self.republish(price),
            Command::RecordDelivery { publication, delivery } => self.record_delivery(e.at, publication, delivery),
            Command::RecordMovement(m) => self.record_movement(m),
            Command::RecordExtractWritten { extract, name, sha256 } => {
                let x = self.extracts.get_mut(extract).ok_or_else(|| refuse("unknown_extract", extract.clone()))?;
                if x.written.is_some() {
                    return Err(refuse("written", format!("{extract} is already written")));
                }
                x.written = Some((name.clone(), sha256.clone()));
                // The file holds the holdings now; the state keeps the totals.
                x.holdings.clear();
                Ok(Output::Done)
            }
        }
    }

    // --- funds ---

    /// The fund's definitions, newest last.
    fn definition(&self, fund: &FundId) -> Result<&FundDefinition, Output> {
        self.funds.get(fund).and_then(|v| v.last()).ok_or_else(|| self.no_such_fund(fund))
    }

    /// A fund nobody defined, with the funds there are, so a mistyped name
    /// is not answered as if the fund existed (F2-06-5).
    fn no_such_fund(&self, fund: &FundId) -> Output {
        let known: Vec<&str> = self.funds.keys().map(|f| f.0.as_str()).collect();
        refuse("unknown_fund", format!("there is no fund '{}'; the funds are {}", fund.0, known.join(", ")))
    }

    /// A suspension in words: from when, why, who placed it, and who can lift it.
    fn suspension_words(fund: &FundId, s: &Suspension) -> String {
        format!(
            "{} is suspended from {} by {}: {}; a unit pricing lead can resume it",
            fund.0, s.from, s.by, s.reason
        )
    }

    /// The definition in force: a fund's latest version.
    pub fn definition_of(&self, fund: &FundId) -> Option<&FundDefinition> {
        self.funds.get(fund)?.last()
    }

    fn activate_fund(&mut self, def: &FundDefinition, today: NaiveDate) -> Step {
        def.terms().map_err(|m| refuse("invalid_definition", m))?;
        let versions = self.funds.get(&def.id).map(Vec::as_slice).unwrap_or_default();
        if let Some(same) = versions.iter().find(|v| v.version == def.version) {
            return if same == def {
                Ok(Output::Done)
            } else {
                Err(refuse(
                    "config_conflict",
                    format!("{} version {} is active with other content", def.id.0, def.version),
                ))
            };
        }
        match versions.last() {
            Some(previous) => def.may_follow(previous).map_err(|m| refuse("invalid_definition", m))?,
            None if def.version != 1 => return Err(refuse("invalid_definition", "a fund's first version is 1")),
            None => {}
        }
        self.funds.entry(def.id.clone()).or_default().push(def.clone());
        self.fund_activated.entry(def.id.clone()).or_insert(today);
        Ok(Output::Done)
    }

    fn record_partitions(&mut self, count: PartitionId) -> Step {
        if count == 0 || count > ic_ledger::PARTITION_COUNT {
            return Err(refuse("invalid", format!("the ledger has 1 to {} partitions", ic_ledger::PARTITION_COUNT)));
        }
        match self.partitions {
            Some(n) if n != count => Err(refuse("partitions_fixed", format!("the ledger has {n} partitions; that never changes"))),
            _ => {
                self.partitions = Some(count);
                Ok(Output::Done)
            }
        }
    }

    fn partition_known(&self, p: PartitionId) -> Result<PartitionId, Output> {
        match self.partitions {
            None => Err(refuse("no_partitions", "the number of partitions is not yet recorded")),
            Some(n) if p >= n => Err(refuse("unknown_partition", format!("partition {p} of {n}"))),
            Some(n) => Ok(n),
        }
    }

    pub fn suspended_on(&self, fund: &FundId, date: NaiveDate) -> Option<&Suspension> {
        self.suspensions.get(fund)?.iter().find(|s| s.from <= date && s.to.is_none_or(|to| date < to))
    }

    fn suspend(&mut self, e: &Envelope, fund: &FundId, from: NaiveDate, reason: &str) -> Step {
        self.definition(fund)?;
        if reason.trim().is_empty() {
            return Err(refuse("reason_required", "a suspension needs a reason"));
        }
        let list = self.suspensions.get(fund).map(Vec::as_slice).unwrap_or_default();
        if list.iter().any(|s| s.to.is_none()) {
            return Err(refuse("suspended", format!("{} is already suspended", fund.0)));
        }
        // A withdrawn suspension covers no day, so it is no obstacle.
        if list.iter().any(|s| s.to.is_some_and(|to| s.from < to && from < to)) {
            return Err(refuse("overlap", "a suspension cannot start inside an earlier one"));
        }
        // A price prepared for a day the suspension covers can never be
        // approved: it leaves the work list, saying why (F2-06-2).
        for p in self.prices.values_mut() {
            if p.fund == *fund && p.pricing_date >= from && matches!(p.status, PriceStatus::Prepared | PriceStatus::Held | PriceStatus::Blocked) {
                p.status = PriceStatus::Rejected;
                p.rejected = Some(format!("{}: the fund was suspended from {from}: {reason}", e.actor));
            }
        }
        self.suspensions.entry(fund.clone()).or_default().push(Suspension {
            from,
            to: None,
            reason: reason.to_string(),
            by: e.actor.clone(),
        });
        self.publish(
            e.at,
            Event::FundSuspended {
                fund: fund.clone(),
                from,
                reason: reason.to_string(),
            },
        );
        Ok(Output::Done)
    }

    fn resume(&mut self, e: &Envelope, fund: &FundId, from: NaiveDate, reason: &str) -> Step {
        self.definition(fund)?;
        if reason.trim().is_empty() {
            return Err(refuse("reason_required", "resuming needs a reason"));
        }
        let open = self
            .suspensions
            .get_mut(fund)
            .and_then(|l| l.iter_mut().find(|s| s.to.is_none()))
            .ok_or_else(|| refuse("not_suspended", format!("{} is not suspended", fund.0)))?;
        if from < open.from {
            return Err(refuse(
                "invalid",
                format!(
                    "{} was suspended from {}: resume from that day to withdraw the suspension, or from a later day",
                    fund.0, open.from
                ),
            ));
        }
        // Resumed from the day it was suspended from, the suspension is
        // withdrawn: it covers no day, and stays on record with why (F2-06-1).
        open.to = Some(from);
        let how = if from == open.from { "withdrawn" } else { "resumed" };
        open.reason = format!("{}; {how} by {}: {reason}", open.reason, e.actor);
        self.publish(e.at, Event::FundResumed { fund: fund.clone(), from });
        Ok(Output::Done)
    }

    // --- valuations and prices ---

    fn receive(&mut self, at: DateTime<Utc>, administrator: &str, file_id: &str, valuations: &[Valuation]) -> Step {
        if valuations.is_empty() {
            return Err(refuse("invalid", "a file with no valuations"));
        }
        let mut accepted = Vec::new();
        let mut refused = Vec::new();
        for v in valuations {
            let key = slot(&v.fund, v.pricing_date);
            if self.definition_of(&v.fund).is_none() {
                refused.push((key, "no definition of the fund is in force on that date".to_string()));
                continue;
            }
            if self.active.contains_key(&key) && !v.restated {
                refused.push((key, "a price is active for this date; a new valuation must be marked restated".to_string()));
                continue;
            }
            let list = self.valuations.entry(key.clone()).or_default();
            if list.iter().any(|r| r.file_id == file_id) {
                refused.push((key, "this file has already delivered this valuation".to_string()));
                continue;
            }
            list.push(ValuationRecord {
                revision: list.len() as u32 + 1,
                file_id: file_id.to_string(),
                administrator: administrator.to_string(),
                received_at: at,
                valuation: v.clone(),
            });
            accepted.push(key);
        }
        Ok(Output::Valuations { accepted, refused })
    }

    /// The latest active price of a fund before a date.
    fn previous_active(&self, fund: &FundId, before: NaiveDate) -> Option<&Price> {
        let prefix = format!("{}:", fund.0);
        self.active
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter_map(|(_, ids)| ids.last().and_then(|id| self.prices.get(id)))
            .filter(|p| p.pricing_date < before)
            .max_by_key(|p| p.pricing_date)
    }

    fn latest_active_date(&self, fund: &FundId) -> Option<NaiveDate> {
        self.previous_active(fund, NaiveDate::MAX).map(|p| p.pricing_date)
    }

    /// The next date the fund is priced for: the pricing day after its
    /// latest price, or its first pricing day. Prices are struck in order,
    /// so this is the only date a price can be prepared for.
    pub fn next_to_price(&self, fund: &FundId) -> Option<NaiveDate> {
        let def = self.definition_of(fund)?;
        let mut next = match self.latest_active_date(fund) {
            Some(last) => def.following_pricing_day(last).ok(),
            None => def.next_pricing_day(*self.fund_activated.get(fund)?).ok(),
        }?;
        // A day a suspension covers is never priced, so it is not next (F2-06-3).
        // An open suspension covers every day from its start: none is next.
        while self.suspended_on(fund, next).is_some() {
            if self.suspensions.get(fund)?.iter().any(|s| s.to.is_none() && s.from <= next) {
                return None;
            }
            next = def.following_pricing_day(next).ok()?;
        }
        Some(next)
    }

    /// The core's units on issue at `date`'s cut-off: every partition's
    /// position once the orders of the previous active price have executed.
    fn core_units_for(&self, fund: &FundId, date: NaiveDate) -> Option<i64> {
        let n = self.partitions?;
        let required = self.previous_active(fund, date).map(|p| p.pricing_date);
        let reports = self.core_units.get(fund)?;
        let mut sum = 0i64;
        for p in 0..n {
            let r = reports.get(&p)?;
            if r.executed_through != required {
                return None;
            }
            sum = sum.checked_add(r.units_micro)?;
        }
        Some(sum)
    }

    /// The open price error holding a fund's active price, if any.
    fn error_over(&self, price: &str) -> Option<&PriceError> {
        self.errors.values().find(|e| e.open() && e.prices.contains_key(price))
    }

    /// The core strikes no price for a day after the books' business date,
    /// so none is prepared or approved for one: it would be refused by every
    /// partition after approval, out of sight (F2-06-4).
    fn not_before_its_day(e: &Envelope, fund: &FundId, date: NaiveDate) -> Result<(), Output> {
        match e.today {
            Some(today) if date > today => Err(refuse(
                "not_yet",
                format!("{} cannot be priced for {date} before that day: the business date is {today}", fund.0),
            )),
            _ => Ok(()),
        }
    }

    fn prepare(&mut self, e: &Envelope, fund: &FundId, date: NaiveDate) -> Step {
        let def = self
            .definition_of(fund)
            .ok_or_else(|| refuse("unknown_fund", format!("no definition of {} is in force on {date}", fund.0)))?
            .clone();
        let terms = def.terms().map_err(|m| refuse("invalid_definition", m))?;
        if !def.prices_on(date) {
            return Err(refuse("not_a_pricing_day", format!("{} does not price on {date}", fund.0)));
        }
        Self::not_before_its_day(e, fund, date)?;
        // Said before a missing valuation, which no valuation would cure.
        if let Some(s) = self.suspended_on(fund, date) {
            return Err(refuse("suspended", Self::suspension_words(fund, s)));
        }
        let key = slot(fund, date);
        let record = self
            .valuations
            .get(&key)
            .and_then(|v| v.last())
            .ok_or_else(|| refuse("no_valuation", format!("no valuation of {key} has been received")))?
            .clone();
        // A correction: the date already has a price in force, held by an error.
        let in_force = self.active.get(&key).and_then(|v| v.last()).cloned();
        let (previous, core_units) = match &in_force {
            Some(wrong) => {
                if self.error_over(wrong).is_none() {
                    return Err(refuse(
                        "already_priced",
                        format!("{wrong} is in force; a correction needs a price error raised over it"),
                    ));
                }
                let w = &self.prices[wrong];
                (w.previous.clone(), w.core_units_micro)
            }
            None => {
                if self.latest_active_date(fund).is_some_and(|d| d > date) {
                    return Err(refuse("out_of_order", format!("{} already has a price after {date}", fund.0)));
                }
                let previous = self.previous_active(fund, date).map(|p| Previous {
                    price_id: p.id.clone(),
                    pricing_date: p.pricing_date,
                    mid: p.struck.mid.clone(),
                    nav: self.valuation_of(p).nav.clone(),
                });
                (previous, self.core_units_for(fund, date))
            }
        };
        let struck = strike(&def, &terms, &record.valuation, previous.as_ref(), core_units).map_err(|m| refuse("cannot_strike", m))?;
        let status = if struck.blocked() {
            PriceStatus::Blocked
        } else if struck.held().is_empty() {
            PriceStatus::Prepared
        } else {
            PriceStatus::Held
        };
        // Any open draft for the slot is replaced by this one.
        for p in self.prices.values_mut() {
            if p.fund == *fund && p.pricing_date == date && matches!(p.status, PriceStatus::Prepared | PriceStatus::Held | PriceStatus::Blocked) {
                p.status = PriceStatus::Replaced;
            }
        }
        let n = self.drafts.entry(key.clone()).or_insert(0);
        *n += 1;
        let id = format!("{key}.{n}");
        self.prices.insert(
            id.clone(),
            Price {
                id: id.clone(),
                fund: fund.clone(),
                pricing_date: date,
                definition_version: def.version,
                valuation_revision: record.revision,
                previous,
                core_units_micro: core_units,
                struck,
                status,
                prepared_by: e.actor.clone(),
                prepared_at: e.at,
                approved_by: None,
                approved_at: None,
                accepted: BTreeMap::new(),
                rejected: None,
                version: None,
                supersedes: None,
                corrects: in_force.as_ref().and_then(|w| self.error_over(w)).map(|err| err.id.clone()),
                deliveries: BTreeMap::new(),
                attempt: 0,
                activated_at: None,
            },
        );
        Ok(Output::Prepared { price: id, status })
    }

    /// The valuation a price was struck from.
    pub fn valuation_of(&self, p: &Price) -> &Valuation {
        let records = &self.valuations[&slot(&p.fund, p.pricing_date)];
        &records[p.valuation_revision as usize - 1].valuation
    }

    fn price_mut(&mut self, id: &str) -> Result<&mut Price, Output> {
        self.prices.get_mut(id).ok_or_else(|| refuse("unknown_price", id.to_string()))
    }

    fn approve(&mut self, e: &Envelope, id: &str, accepted: &BTreeMap<CheckKind, String>) -> Step {
        let p = self.prices.get(id).ok_or_else(|| refuse("unknown_price", id.to_string()))?.clone();
        match p.status {
            PriceStatus::Prepared | PriceStatus::Held => {}
            PriceStatus::Blocked => return Err(refuse("blocked", "units on issue do not agree; the price cannot be approved")),
            other => return Err(refuse("wrong_status", format!("{id} is {other:?}"))),
        }
        if e.actor == p.prepared_by {
            return Err(refuse("separation", "the person who prepared a price cannot approve it"));
        }
        let held: BTreeSet<CheckKind> = p.struck.held().into_iter().collect();
        let given: BTreeSet<CheckKind> = accepted.keys().copied().collect();
        if held != given {
            return Err(refuse(
                "checks",
                format!("each held check must be accepted with a reason, and only those: held {held:?}"),
            ));
        }
        if accepted.values().any(|r| r.trim().is_empty()) {
            return Err(refuse("reason_required", "each accepted check needs a reason"));
        }
        let key = slot(&p.fund, p.pricing_date);
        if let Some(other) = self
            .prices
            .values()
            .find(|x| x.status == PriceStatus::Activating && x.fund == p.fund && x.pricing_date == p.pricing_date)
        {
            return Err(refuse("in_flight", format!("{} is being activated for this date", other.id)));
        }
        if self.valuations.get(&key).map_or(0, |v| v.len() as u32) != p.valuation_revision {
            return Err(refuse("stale", "a newer valuation has arrived; prepare the price again"));
        }
        // No draft is open for a suspended day: none is prepared for one, and a
        // suspension rejects the drafts it covers.
        Self::not_before_its_day(e, &p.fund, p.pricing_date)?;
        let def = self.definition_of(&p.fund).ok_or_else(|| refuse("unknown_fund", p.fund.0.clone()))?.clone();
        let terms: PricingTerms = def.terms().map_err(|m| refuse("invalid_definition", m))?;
        let in_force = self.active.get(&key).and_then(|v| v.last()).cloned();
        let n = self
            .partitions
            .ok_or_else(|| refuse("no_partitions", "the number of partitions is not yet recorded"))?;

        let mut version = 1;
        let mut material = true;
        match (&p.corrects, &in_force) {
            (Some(error), Some(wrong)) => {
                if !self.errors.get(error).is_some_and(|x| x.open() && x.prices.contains_key(wrong)) {
                    return Err(refuse("not_held", format!("{wrong} is no longer held by {error}")));
                }
                let w = &self.prices[wrong];
                let size = error_size(&w.struck.mid, &p.struck.mid);
                material = size >= terms.error_threshold.get();
                version = w.version.unwrap_or(1) + 1;
                let correction = Correction {
                    price: id.to_string(),
                    size: crate::strike::fixed(size, 8, ic_money::RoundingMode::HalfEven).map_err(|m| refuse("cannot_strike", m))?,
                    threshold: terms.error_threshold.clone(),
                    material,
                };
                let wrong = wrong.clone();
                let error = error.clone();
                self.errors.get_mut(&error).map(|x| x.prices.insert(wrong, Some(correction)));
            }
            (Some(_), None) | (None, Some(_)) => return Err(refuse("stale", "the price in force has changed since this was prepared; prepare again")),
            (None, None) => {
                // A new date: the core's units must still be the ones it was struck against.
                if self.core_units_for(&p.fund, p.pricing_date) != p.core_units_micro {
                    return Err(refuse("stale", "the core's units on issue have changed; prepare the price again"));
                }
                let previous = self.previous_active(&p.fund, p.pricing_date).map(|x| x.id.as_str());
                if previous != p.previous.as_ref().map(|x| x.price_id.as_str()) {
                    return Err(refuse("stale", "the previous price has been corrected since; prepare the price again"));
                }
                if let Some(err) = self.errors.values().find(|x| x.open() && x.fund == p.fund && !x.corrected()) {
                    let waiting: Vec<&str> = err.prices.iter().filter(|(_, c)| c.is_none()).map(|(wrong, _)| wrong.as_str()).collect();
                    return Err(refuse(
                        "held_by_error",
                        format!(
                            "{} is held by the price error raised on {} against {}; prepare and approve the correction of {} first, then this price",
                            p.fund.0,
                            err.detected_on,
                            waiting.join(", "),
                            if waiting.len() == 1 { "that price" } else { "each of them" }
                        ),
                    ));
                }
                // Every pricing day since the last price has one, or was suspended.
                if let Some(last) = self.latest_active_date(&p.fund) {
                    let mut d = def.following_pricing_day(last).map_err(|m| refuse("invalid_definition", m))?;
                    while d < p.pricing_date {
                        if self.suspended_on(&p.fund, d).is_none() {
                            return Err(refuse("gap", format!("{} has no price for {d}; price dates in order", p.fund.0)));
                        }
                        d = def.following_pricing_day(d).map_err(|m| refuse("invalid_definition", m))?;
                    }
                }
            }
        }

        let price = self.price_mut(id)?;
        price.approved_by = Some(e.actor.clone());
        price.approved_at = Some(e.at);
        price.accepted = accepted.clone();
        if !material {
            price.status = PriceStatus::Immaterial;
            self.settle_error(e.at, id);
            return Ok(Output::Done);
        }
        price.status = PriceStatus::Activating;
        price.version = Some(version);
        price.supersedes = in_force;
        price.attempt = 1;
        let price = price.clone();
        for partition in 0..n {
            self.queue(&price, partition);
        }
        Ok(Output::Done)
    }

    fn queue(&mut self, p: &Price, partition: PartitionId) {
        self.publication_seq += 1;
        let id = format!("{}/{partition}/{}", p.id, p.attempt);
        self.publications.insert(
            id.clone(),
            Publication {
                id,
                seq: self.publication_seq,
                partition,
                price: p.id.clone(),
                fund: p.fund.clone(),
                pricing_date: p.pricing_date,
                version: p.version.unwrap_or(1),
                buy: p.struck.buy.clone(),
                sell: p.struck.sell.clone(),
                mid: p.struck.mid.clone(),
                inputs_hash: p.struck.inputs_hash.clone(),
                supersedes: p.supersedes.clone(),
            },
        );
    }

    fn republish(&mut self, id: &str) -> Step {
        let p = self.price_mut(id)?;
        if p.status != PriceStatus::Activating {
            return Err(refuse("wrong_status", format!("{id} is {:?}", p.status)));
        }
        let refused: Vec<PartitionId> = p
            .deliveries
            .iter()
            .filter(|(_, d)| matches!(d, Delivery::Refused { .. }))
            .map(|(k, _)| *k)
            .collect();
        if refused.is_empty() {
            return Err(refuse("nothing_refused", format!("no partition has refused {id}")));
        }
        p.attempt += 1;
        for k in &refused {
            p.deliveries.remove(k);
        }
        let p = p.clone();
        for partition in refused {
            self.queue(&p, partition);
        }
        Ok(Output::Done)
    }

    fn record_delivery(&mut self, at: DateTime<Utc>, publication: &str, delivery: &Delivery) -> Step {
        let Some(item) = self.publications.remove(publication) else {
            // Heard before: the item is gone. Nothing changes.
            return Ok(Output::Done);
        };
        let n = self.partitions.unwrap_or(0);
        let price = self.price_mut(&item.price)?;
        price.deliveries.insert(item.partition, delivery.clone());
        let all_applied = (0..n).all(|k| matches!(price.deliveries.get(&k), Some(Delivery::Applied)));
        if !all_applied {
            return Ok(Output::Done);
        }
        price.status = PriceStatus::Active;
        price.activated_at = Some(at);
        let p = price.clone();
        let key = slot(&p.fund, p.pricing_date);
        if let Some(old) = &p.supersedes {
            if let Some(o) = self.prices.get_mut(old) {
                o.status = PriceStatus::Superseded;
            }
        }
        self.active.entry(key).or_default().push(p.id.clone());
        self.publish(
            at,
            Event::PriceActivated {
                price: p.id.clone(),
                fund: p.fund.clone(),
                pricing_date: p.pricing_date,
                version: p.version.unwrap_or(1),
                mid: p.struck.mid.clone(),
                buy: p.struck.buy.clone(),
                sell: p.struck.sell.clone(),
            },
        );
        if p.corrects.is_some() {
            self.settle_error(at, &p.id);
        }
        Ok(Output::Done)
    }

    /// A correction is in force: mark the error corrected when all its prices are.
    fn settle_error(&mut self, at: DateTime<Utc>, correction: &str) {
        // Only a price with an error to correct reaches here.
        let error = self.prices.get(correction).and_then(|p| p.corrects.clone()).unwrap_or_default();
        let prices = &self.prices;
        let in_force = |c: &Option<Correction>| {
            c.as_ref()
                .and_then(|c| prices.get(&c.price))
                .is_some_and(|p| matches!(p.status, PriceStatus::Active | PriceStatus::Immaterial))
        };
        let settled = self
            .errors
            .get_mut(&error)
            .filter(|err| err.corrected_at.is_none() && err.prices.values().all(in_force))
            .map(|err| {
                err.corrected_at = Some(at);
                err.prices.values().flatten().any(|c| c.material)
            });
        if let Some(material) = settled {
            self.publish(at, Event::PriceErrorCorrected { error, material });
        }
    }

    fn raise_error(&mut self, e: &Envelope, id: &str, prices: &[String], cause: &str, detected_on: NaiveDate) -> Step {
        if id.trim().is_empty() || self.errors.contains_key(id) {
            return Err(refuse("invalid", format!("price error {id:?} exists or has no ID")));
        }
        if cause.trim().is_empty() {
            return Err(refuse("reason_required", "a price error needs its cause"));
        }
        let first = prices.first().ok_or_else(|| refuse("invalid", "a price error names the prices it affects"))?;
        let fund = self
            .prices
            .get(first)
            .map(|p| p.fund.clone())
            .ok_or_else(|| refuse("unknown_price", first.clone()))?;
        for id in prices {
            let p = self.prices.get(id).ok_or_else(|| refuse("unknown_price", id.clone()))?;
            if p.fund != fund {
                return Err(refuse("invalid", "one price error covers one fund"));
            }
            if p.status != PriceStatus::Active {
                return Err(refuse("not_active", format!("{id} is {:?}; only a price in force can be wrong", p.status)));
            }
            if let Some(other) = self.error_over(id) {
                return Err(refuse("already_raised", format!("{id} is already in price error {}", other.id)));
            }
        }
        self.errors.insert(
            id.to_string(),
            PriceError {
                id: id.to_string(),
                fund: fund.clone(),
                cause: cause.to_string(),
                detected_on,
                raised_by: e.actor.clone(),
                raised_at: e.at,
                prices: prices.iter().map(|p| (p.clone(), None)).collect(),
                corrected_at: None,
                remediation: Remediation::default(),
                closed: None,
            },
        );
        self.publish(
            e.at,
            Event::PriceErrorRaised {
                error: id.to_string(),
                fund,
                prices: prices.to_vec(),
            },
        );
        Ok(Output::Done)
    }

    // --- the core's unit events ---

    fn record_movement(&mut self, m: &UnitMovement) -> Step {
        self.partition_known(m.partition)?;
        if self.movement_ids.contains_key(&m.id) {
            return Ok(Output::Done);
        }
        self.movement_ids.insert(m.id.clone(), m.pricing_date);
        if m.kind == MovementKind::Correction {
            let error = m.price_id.as_ref().and_then(|p| self.prices.get(p)).and_then(|p| p.corrects.clone());
            if let Some(err) = error.and_then(|id| self.errors.get_mut(&id)) {
                let r = &mut err.remediation;
                r.policies.insert(m.policy.clone());
                r.movements += 1;
                r.borne_minor = r.borne_minor.saturating_add(m.borne_minor);
                if m.uncompensated {
                    r.uncompensated += 1;
                } else {
                    r.units_micro = r.units_micro.saturating_add(m.units_micro);
                    r.cash_minor = r.cash_minor.saturating_add(m.cash_minor);
                }
            }
            if m.uncompensated {
                // Nothing was posted: holdings are as they were.
                return Ok(Output::Done);
            }
        }
        let holdings = self.holdings.entry(m.fund.clone()).or_default();
        let units = holdings.entry(m.policy.clone()).or_insert(0);
        *units = units.saturating_add(m.units_micro);
        if *units == 0 {
            holdings.remove(&m.policy);
        }
        match m.kind {
            MovementKind::Buy | MovementKind::Sell => {
                let flow = self.flows.entry(slot(&m.fund, m.pricing_date)).or_default();
                if m.cash_minor >= 0 {
                    flow.applications_minor = flow.applications_minor.saturating_add(m.cash_minor);
                } else {
                    flow.redemptions_minor = flow.redemptions_minor.saturating_add(-m.cash_minor);
                }
                flow.orders += 1;
            }
            MovementKind::Correction | MovementKind::Opening => {}
        }
        Ok(Output::Done)
    }
    /// A partition has executed every order of a fund through `through`:
    /// mark each priced date up to then that has no extract yet.
    fn record_executed_through(&mut self, fund: &FundId, partition: PartitionId, through: NaiveDate) -> Result<(), Output> {
        let prefix = format!("{}:", fund.0);
        let dates: Vec<NaiveDate> = self
            .active
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter(|(k, _)| !self.extracts.contains_key(*k))
            .filter_map(|(_, ids)| ids.last().and_then(|id| self.prices.get(id)))
            .map(|p| p.pricing_date)
            .filter(|d| *d <= through)
            .collect();
        for date in dates {
            self.record_executed(fund, partition, date)?;
        }
        Ok(())
    }

    fn record_executed(&mut self, fund: &FundId, partition: PartitionId, date: NaiveDate) -> Step {
        let n = self.partition_known(partition)?;
        let key = slot(fund, date);
        let done = self.executed.entry(key.clone()).or_default();
        done.insert(partition);
        if done.len() == n as usize && !self.extracts.contains_key(&key) {
            let holdings = self.holdings.get(fund).cloned().unwrap_or_default();
            let flow = self.flows.remove(&key).unwrap_or_default();
            self.executed.remove(&key);
            self.extracts.insert(
                key.clone(),
                Extract {
                    id: key.clone(),
                    fund: fund.clone(),
                    pricing_date: date,
                    price: self.active.get(&key).and_then(|v| v.last()).cloned(),
                    units_on_issue_micro: holdings.values().sum(),
                    holders: holdings.len() as u64,
                    holdings,
                    flow,
                    written: None,
                },
            );
            if let Some(cutoff) = date.checked_sub_days(Days::new(MOVEMENT_RETENTION_DAYS)) {
                self.movement_ids.retain(|_, d| *d >= cutoff);
            }
        }
        Ok(Output::Done)
    }

    // --- events ---

    fn publish(&mut self, at: DateTime<Utc>, event: Event) {
        self.event_seq += 1;
        self.events.push(EventRecord {
            seq: self.event_seq,
            at,
            event,
        });
        trim(&mut self.events, EVENT_RETENTION);
    }

    // --- questions ---

    /// Events after a sequence number, oldest first.
    pub fn events_after(&self, after: u64, limit: usize) -> Vec<&EventRecord> {
        let start = self.events.partition_point(|e| e.seq <= after);
        self.events[start..].iter().take(limit).collect()
    }

    /// The oldest publications still to be carried to the core.
    pub fn outbox(&self, limit: usize) -> Vec<&Publication> {
        let mut items: Vec<&Publication> = self.publications.values().collect();
        items.sort_by_key(|p| p.seq);
        items.truncate(limit);
        items
    }

    /// Prices waiting for a person: prepared, held, blocked, or with a partition refusing.
    pub fn work(&self) -> Vec<&Price> {
        self.prices
            .values()
            .filter(|p| match p.status {
                PriceStatus::Prepared | PriceStatus::Held | PriceStatus::Blocked => true,
                PriceStatus::Activating => p.deliveries.values().any(|d| matches!(d, Delivery::Refused { .. })),
                _ => false,
            })
            .collect()
    }

    /// Pricing dates on or before `as_at` whose price is overdue by the fund's
    /// lag and not explained by a suspension.
    pub fn late(&self, as_at: NaiveDate) -> Vec<(FundId, NaiveDate)> {
        let mut late = Vec::new();
        let floor = as_at.checked_sub_days(Days::new(LATE_LOOKBACK_DAYS)).unwrap_or(as_at);
        for fund in self.funds.keys() {
            let Some(def) = self.definition_of(fund) else { continue };
            let Ok(terms) = def.terms() else { continue };
            let start = match self.latest_active_date(fund) {
                Some(last) => def.following_pricing_day(last),
                None => def.next_pricing_day(self.fund_activated.get(fund).copied().unwrap_or(floor)),
            };
            let Ok(mut d) = start else { continue };
            d = d.max(def.next_pricing_day(floor).unwrap_or(d));
            let mut next = Some(d).filter(|d| *d <= as_at);
            while let Some(d) = next {
                let due = def.price_due_by(d, terms.pricing_lag_days).unwrap_or(d);
                if due < as_at && self.suspended_on(fund, d).is_none() && !self.active.contains_key(&slot(fund, d)) {
                    late.push((fund.clone(), d));
                }
                next = def.following_pricing_day(d).ok().filter(|d| *d <= as_at);
            }
        }
        late
    }

    /// Extracts due and not yet written, oldest first.
    pub fn extracts_due(&self, limit: usize) -> Vec<&Extract> {
        let mut due: Vec<&Extract> = self.extracts.values().filter(|x| x.written.is_none()).collect();
        due.sort_by_key(|x| (x.pricing_date, x.fund.clone()));
        due.truncate(limit);
        due
    }
}

/// Keep the newest `limit` items of a list kept oldest first.
fn trim<T>(items: &mut Vec<T>, limit: usize) {
    let excess = items.len().saturating_sub(limit);
    items.drain(..excess);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: trimming keeps the newest items up to the limit, and leaves a
    /// shorter list alone.
    /// Why it matters: the published event stream is bounded by this; an
    /// off-by-one would drop an event a reader has not yet taken.
    #[test]
    fn trim_keeps_the_newest() {
        let mut v: Vec<u32> = (1..=10).collect();
        trim(&mut v, 3);
        assert_eq!(v, vec![8, 9, 10]);
        trim(&mut v, 5);
        assert_eq!(v, vec![8, 9, 10]);
    }
}
