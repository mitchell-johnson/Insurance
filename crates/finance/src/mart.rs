//! The data mart: the book of policies and covers as the core's events
//! describe it, kept by business date so that an extract cut as at a closed
//! date never changes.
//!
//! The mart is a read model. It holds nothing the events do not carry, and
//! it can be thrown away and rebuilt from the first event of each partition.
//! It knows a policy and a life only by their opaque IDs: no name, no party.
//!
//! Three extracts are cut from it:
//!
//! - `in_force`: one row for every cover in force as at a date.
//! - `movement`: every change to the in-force book over a period, with the
//!   premium billed, each row carrying the business date it was recorded on
//!   and the date it took effect.
//! - `experience`: days of exposure and the claims that ended a policy, by
//!   product and cover type.
//!
//! Something recorded after a date closed never changes that date's
//! extract. A backdated change is a movement of the period it was recorded
//! in, with its own effective date, as a backdated entry is in the ledger.
//!
//! The movement rows are worked out as the difference each event makes to
//! its policy. The in-force rows are worked out from the dates each cover
//! was known from and to. The two are independent, and an extract is
//! reconciled when opening plus movements equals closing.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::event::{Event, EventRecord, LayerInfo, LifeInfo, MemberCover};
use ic_core::{LayerOrigin, LifeId, PolicyId, PolicyStatus};
use ic_ledger::{sha256, AccountUnit, Hash32, PartitionId};
use ic_product::Sex;
use serde::{Deserialize, Serialize};

use crate::books::ExtractRecord;

/// Version of the extract layouts below. A change to a column is a new version.
pub const DEFINITION_VERSION: u32 = 3;

pub const IN_FORCE: &str = "in_force";
pub const MOVEMENT: &str = "movement";
pub const EXPERIENCE: &str = "experience";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MartError {
    #[error("there is no partition {0}")]
    Partition(PartitionId),
    #[error("expected event {expected} of partition {partition}, got {got}")]
    Gap { partition: PartitionId, expected: u64, got: u64 },
    #[error("partition {partition} has not closed {date}")]
    NotClosed { partition: PartitionId, date: NaiveDate },
    #[error("a period runs from its first day to its last: {from} is after {to}")]
    Range { from: NaiveDate, to: NaiveDate },
}

/// One cover on one life, with the business dates the mart knew it between.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Span {
    cover: String,
    cover_type: String,
    /// The layer, or none for a scheme member's formula-driven cover.
    layer: Option<u32>,
    life: LifeId,
    sum_insured_minor: i64,
    /// The date the cover started.
    start: NaiveDate,
    /// The business date it was recorded on.
    known_from: NaiveDate,
    /// The business date it was recorded as ended on.
    known_to: Option<NaiveDate>,
}

impl Span {
    fn known_at(&self, date: NaiveDate) -> bool {
        self.known_from <= date && self.known_to.is_none_or(|to| to > date)
    }
}

/// What a life was priced on, as the core last published it. The date of
/// birth stays in the mart: an extract carries the age, never the date.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Factors {
    date_of_birth: Option<NaiveDate>,
    sex: Option<Sex>,
    smoker: Option<bool>,
    #[serde(default)]
    occupation_class: Option<String>,
}

/// A life's factors with the business date each version was recorded on, in
/// order, so that an extract as at an earlier date keeps what was known then.
type Dated = Vec<(NaiveDate, Factors)>;

impl Factors {
    fn from(l: &LifeInfo) -> Self {
        Factors {
            date_of_birth: l.date_of_birth,
            sex: l.sex,
            smoker: l.smoker,
            occupation_class: l.occupation_class.clone(),
        }
    }

    fn of(lives: &[LifeInfo], date: NaiveDate) -> BTreeMap<LifeId, Dated> {
        lives.iter().map(|l| (l.life.clone(), vec![(date, Factors::from(l))])).collect()
    }

    /// The occupation class as a column: letters, digits, `-` and `_` only,
    /// so that a class typed with a comma cannot shift the columns after it.
    fn class(&self) -> String {
        let plain = |c: char| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' };
        self.occupation_class.as_deref().unwrap_or_default().chars().map(plain).collect()
    }

    /// Age last birthday on a date.
    fn age(&self, on: NaiveDate) -> Option<u32> {
        self.date_of_birth.and_then(|dob| on.years_since(dob))
    }

    /// The age, sex, smoker status and occupation class columns, each empty
    /// where not known.
    fn columns(&self, on: NaiveDate) -> String {
        format!("{},{},{},{}", opt(&self.age(on)), sex_name(self.sex), opt(&self.smoker), self.class())
    }
}

pub(crate) fn sex_name(sex: Option<Sex>) -> &'static str {
    match sex {
        Some(Sex::Female) => "female",
        Some(Sex::Male) => "male",
        None => "",
    }
}

/// One cover in force with the factors a rate table prices it on. It carries
/// ages, never the date of birth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatedCover {
    pub partition: PartitionId,
    pub policy: PolicyId,
    pub cover: String,
    pub layer: Option<u32>,
    /// Age last birthday as at the date asked.
    pub age: Option<u32>,
    /// Age last birthday when this cover started.
    pub age_at_start: Option<u32>,
    pub sex: Option<Sex>,
    pub smoker: Option<bool>,
    pub occupation_class: Option<String>,
    pub sum_insured_minor: i64,
}

/// The five-year band an age falls in, for example `30-34`.
pub(crate) fn age_band(age: Option<u32>) -> String {
    age.map(|a| format!("{}-{}", a / 5 * 5, a / 5 * 5 + 4)).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PolicyRow {
    product_code: String,
    inception: Option<NaiveDate>,
    /// Each status with the business date it was recorded on, in order.
    statuses: Vec<(NaiveDate, PolicyStatus)>,
    spans: Vec<Span>,
    /// What each receivable billed, so that an unwind can take it back.
    premiums: BTreeMap<u32, i64>,
    /// The rating factors of each life the core has published them for.
    #[serde(default)]
    lives: BTreeMap<LifeId, Dated>,
}

/// A policy counts towards the in-force book while cover is being provided.
fn provides_cover(status: PolicyStatus) -> bool {
    matches!(status, PolicyStatus::InForce | PolicyStatus::PaidUp)
}

type CoverKey = (String, Option<u32>, LifeId);

impl PolicyRow {
    /// A life's factors as they were recorded by the close of `date`.
    fn factors(&self, life: &LifeId, date: NaiveDate) -> Factors {
        let dated = self.lives.get(life).into_iter().flatten();
        dated.rev().find(|(on, _)| *on <= date).map(|(_, f)| f.clone()).unwrap_or_default()
    }

    /// Record a life's factors as published on `date`.
    fn learn(&mut self, date: NaiveDate, info: &Option<LifeInfo>) {
        if let Some(info) = info {
            let dated = self.lives.entry(info.life.clone()).or_default();
            let factors = Factors::from(info);
            if dated.last().is_none_or(|(_, last)| *last != factors) {
                dated.push((date, factors));
            }
        }
    }

    fn status_at(&self, date: NaiveDate) -> Option<PolicyStatus> {
        self.statuses.iter().rev().find(|(on, _)| *on <= date).map(|(_, s)| *s)
    }

    fn in_force_now(&self) -> bool {
        self.statuses.last().is_some_and(|(_, s)| provides_cover(*s))
    }

    /// What the policy adds to the in-force book now.
    fn contribution(&self) -> BTreeMap<CoverKey, (String, i64)> {
        if !self.in_force_now() {
            return BTreeMap::new();
        }
        self.spans
            .iter()
            .filter(|s| s.known_to.is_none())
            .map(|s| ((s.cover.clone(), s.layer, s.life.clone()), (s.cover_type.clone(), s.sum_insured_minor)))
            .collect()
    }

    fn open(&mut self, date: NaiveDate, cover: &str, cover_type: &str, layer: Option<u32>, life: &LifeId, sum_insured_minor: i64, start: NaiveDate) {
        // The same cover recorded twice replaces what was there.
        self.end(date, |s| s.cover == cover && s.layer == layer && s.life == *life);
        self.spans.push(Span {
            cover: cover.to_string(),
            cover_type: cover_type.to_string(),
            layer,
            life: life.clone(),
            sum_insured_minor,
            start,
            known_from: date,
            known_to: None,
        });
    }

    fn open_layer(&mut self, date: NaiveDate, l: &LayerInfo) {
        self.open(date, &l.cover, &l.cover_type, Some(l.layer), &l.life, l.sum_insured_minor, l.start);
    }

    fn open_member(&mut self, date: NaiveDate, m: &MemberCover, start: NaiveDate) {
        self.open(date, &m.cover, &m.cover_type, None, &m.life, m.sum_insured_minor, start);
    }

    fn end(&mut self, date: NaiveDate, which: impl Fn(&Span) -> bool) {
        for span in self.spans.iter_mut().filter(|s| s.known_to.is_none()) {
            if which(span) {
                span.known_to = Some(date);
            }
        }
    }

    /// The days from `from` to `to` on which the policy was in force, as
    /// runs of consecutive days.
    fn in_force_days(&self, from: NaiveDate, to: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
        let mut runs = Vec::new();
        for (i, (on, status)) in self.statuses.iter().enumerate() {
            if !provides_cover(*status) {
                continue;
            }
            let first = (*on).max(from);
            let last = match self.statuses.get(i + 1) {
                Some((next, _)) => match next.pred_opt() {
                    Some(day) => day.min(to),
                    None => continue,
                },
                None => to,
            };
            if first <= last {
                runs.push((first, last));
            }
        }
        runs
    }
}

/// One change to the in-force book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Movement {
    pub partition: PartitionId,
    pub policy: PolicyId,
    pub product_code: String,
    /// The business date it was recorded on.
    pub business_date: NaiveDate,
    /// The date it took effect, where the event says.
    pub effective: Option<NaiveDate>,
    /// What happened: `new_business`, `increase`, `indexation`, `lapsed`, ...
    pub kind: String,
    /// The cover, or empty for a row about the policy as a whole.
    pub cover: String,
    pub cover_type: String,
    pub layer: Option<u32>,
    pub life: Option<LifeId>,
    pub policies: i64,
    pub covers: i64,
    pub sum_insured_minor: i64,
    pub premium_minor: i64,
}

/// A claim that ended a policy, by cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EndingClaim {
    business_date: NaiveDate,
    product_code: String,
    cover_type: String,
    sum_insured_minor: i64,
    /// The insured life's age at the event, sex and smoker status, where
    /// the cover is on one life whose factors are known.
    #[serde(default)]
    age: Option<u32>,
    #[serde(default)]
    sex: Option<Sex>,
    #[serde(default)]
    smoker: Option<bool>,
    #[serde(default)]
    occupation_class: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PartitionFeed {
    next_seq: u64,
    /// The last business date the partition closed.
    closed: Option<NaiveDate>,
}

/// The mart. Feed it every event of every partition, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mart {
    /// How many partitions the insurer has. Zero until it is known: events
    /// are taken from any partition, and no extract is cut.
    partitions: u16,
    feeds: BTreeMap<PartitionId, PartitionFeed>,
    #[serde(with = "crate::pairs")]
    policies: BTreeMap<(PartitionId, PolicyId), PolicyRow>,
    movements: Vec<Movement>,
    claims: Vec<EndingClaim>,
    /// Money held in suspense for each policy, and since when.
    #[serde(default, with = "crate::pairs")]
    suspense: BTreeMap<(PartitionId, PolicyId), Held>,
}

/// Money held for a policy: what the core owes back or has yet to apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub amount_minor: i64,
    /// The business date the balance last rose from nothing.
    pub since: NaiveDate,
}

/// The in-force book's totals: policies, covers and sum insured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Totals {
    policies: i128,
    covers: i128,
    sum_insured_minor: i128,
}

/// A frozen file cut from the mart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extract {
    pub kind: &'static str,
    /// First day of the period, for an extract that covers one.
    pub from: Option<NaiveDate>,
    pub as_at: NaiveDate,
    /// The file: a header line, then one line for each row.
    pub lines: Vec<String>,
    pub control: BTreeMap<String, i128>,
    /// Whether opening plus movements equals closing.
    pub reconciled: bool,
}

impl Extract {
    /// The file as it is stored: comma-separated, one line per row.
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = self.lines.join("\n").into_bytes();
        out.push(b'\n');
        out
    }

    pub fn rows(&self) -> u64 {
        self.lines.len() as u64 - 1
    }

    pub fn content_hash(&self) -> Hash32 {
        sha256(&[&self.bytes()])
    }

    pub fn id(&self) -> String {
        format!("{}:{}:v{DEFINITION_VERSION}", self.kind, self.as_at)
    }

    /// The name the file is stored under. An extract is never rewritten, so
    /// the name carries everything that identifies it.
    pub fn name(&self) -> String {
        format!("extracts/{}/{}-v{DEFINITION_VERSION}.csv", self.as_at, self.kind)
    }

    /// What the finance ledger registers: the file's hash and totals, tied
    /// to the Merkle root published for its date.
    pub fn record(&self, merkle_root: Hash32, location: &str) -> ExtractRecord {
        ExtractRecord {
            id: self.id(),
            kind: self.kind.to_string(),
            as_at: self.as_at,
            content_hash: self.content_hash(),
            rows: self.rows(),
            merkle_root,
            definition_version: DEFINITION_VERSION,
            location: location.to_string(),
            control: self.control.clone(),
            reconciled: self.reconciled,
        }
    }
}

fn status_name(status: PolicyStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn origin_name(origin: LayerOrigin) -> &'static str {
    match origin {
        LayerOrigin::Original => "new_cover",
        LayerOrigin::Increase => "increase",
        LayerOrigin::Indexation => "indexation",
        LayerOrigin::UnderwrittenExcess => "underwritten_excess",
    }
}

fn opt<T: ToString>(value: &Option<T>) -> String {
    value.as_ref().map(T::to_string).unwrap_or_default()
}

impl Default for Mart {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Mart {
    pub fn new(partitions: u16) -> Self {
        Self {
            partitions,
            feeds: BTreeMap::new(),
            policies: BTreeMap::new(),
            movements: Vec::new(),
            claims: Vec::new(),
            suspense: BTreeMap::new(),
        }
    }

    /// Say how many partitions there are, once it is known.
    pub fn set_partitions(&mut self, partitions: u16) {
        self.partitions = partitions;
    }

    fn holds(&self, partition: PartitionId) -> bool {
        self.partitions == 0 || partition < self.partitions
    }

    /// Money held in suspense, by policy, oldest first.
    pub fn suspense(&self) -> Vec<(&PolicyId, &Held)> {
        let mut held: Vec<(&PolicyId, &Held)> = self.suspense.iter().map(|((_, policy), h)| (policy, h)).collect();
        held.sort_by(|a, b| (a.1.since, a.0).cmp(&(b.1.since, b.0)));
        held
    }

    /// The latest business date any partition has closed.
    pub fn latest_close(&self) -> Option<NaiveDate> {
        self.feeds.values().filter_map(|f| f.closed).max()
    }

    /// The next event wanted from a partition.
    pub fn next_seq(&self, partition: PartitionId) -> Option<u64> {
        self.holds(partition).then(|| self.feeds.get(&partition).map_or(1, |f| f.next_seq))
    }

    /// The last date every partition has closed.
    pub fn closed_through(&self) -> Option<NaiveDate> {
        (0..self.partitions).map(|p| self.feeds.get(&p).and_then(|f| f.closed)).min().flatten()
    }

    /// Take one event in. An event already taken is ignored; one that skips
    /// ahead is refused, so the caller asks for what is missing.
    pub fn apply(&mut self, record: &EventRecord) -> Result<(), MartError> {
        let partition = record.partition;
        let expected = self.next_seq(partition).ok_or(MartError::Partition(partition))?;
        if record.seq < expected {
            return Ok(());
        }
        if record.seq > expected {
            return Err(MartError::Gap {
                partition,
                expected,
                got: record.seq,
            });
        }
        let feed = self.feeds.entry(partition).or_insert(PartitionFeed { next_seq: 1, closed: None });
        feed.next_seq += 1;
        let date = record.business_date;
        match &record.event {
            Event::BusinessDateAdvanced { from, .. } => {
                feed.closed = (*from).or(feed.closed);
            }
            Event::EntryPosted {
                policy: Some(policy), lines, ..
            } => {
                // A liability: a credit, which is negative, is money held.
                let moved: i128 = lines
                    .iter()
                    .filter(|l| l.role == ic_core::accounts::role::POLICY_SUSPENSE)
                    .map(|l| match l.unit {
                        AccountUnit::Minor(_) => -l.signed,
                        AccountUnit::Fine(_) => -l.signed / ic_money::FINE_PER_MINOR,
                        AccountUnit::Units(_) => 0,
                    })
                    .sum();
                if moved != 0 {
                    let key = (partition, policy.clone());
                    let was = self.suspense.get(&key).map_or(0, |h| h.amount_minor);
                    let now = was.saturating_add(i64::try_from(moved).unwrap_or(i64::MAX));
                    if now == 0 {
                        self.suspense.remove(&key);
                    } else {
                        let since = self.suspense.get(&key).filter(|_| was != 0).map_or(date, |h| h.since);
                        self.suspense.insert(key, Held { amount_minor: now, since });
                    }
                }
            }
            Event::PolicyIssued {
                policy,
                product_code,
                inception,
                lives,
                layers,
                members,
                ..
            } => {
                let mut row = PolicyRow {
                    product_code: product_code.clone(),
                    inception: Some(*inception),
                    statuses: vec![(date, PolicyStatus::InForce)],
                    spans: Vec::new(),
                    premiums: BTreeMap::new(),
                    lives: Factors::of(lives, date),
                };
                for l in layers {
                    row.open_layer(date, l);
                }
                for m in members {
                    row.open_member(date, m, *inception);
                }
                self.arrive(partition, policy, row, date, Some(*inception), "new_business");
            }
            Event::TransferLanded {
                policy,
                product_code,
                status,
                lives,
                layers,
                members,
                ..
            } => {
                let mut row = PolicyRow {
                    product_code: product_code.clone(),
                    inception: None,
                    statuses: vec![(date, *status)],
                    spans: Vec::new(),
                    premiums: BTreeMap::new(),
                    lives: Factors::of(lives, date),
                };
                for l in layers {
                    row.open_layer(date, l);
                }
                for m in members {
                    row.open_member(date, m, date);
                }
                self.arrive(partition, policy, row, date, None, "transfer_in");
            }
            Event::PolicyStatusChanged { policy, to, .. } => {
                let kind = match to {
                    PolicyStatus::Transferred => "transfer_out".to_string(),
                    other => status_name(*other),
                };
                self.change(partition, policy, date, None, &kind, |row| row.statuses.push((date, *to)));
            }
            Event::LayerAdded { policy, layer, origin } => {
                self.change(partition, policy, date, Some(layer.start), origin_name(*origin), |row| {
                    row.open_layer(date, layer)
                });
            }
            Event::LayerEnded {
                policy,
                cover,
                layer,
                life,
                effective,
                ..
            } => {
                self.change(partition, policy, date, Some(*effective), "cover_ended", |row| {
                    row.end(date, |s| s.cover == *cover && s.layer == Some(*layer) && s.life == *life)
                });
            }
            Event::LayersMerged {
                policy,
                cover,
                removed,
                into,
                sum_insured_minor,
            } => {
                self.change(partition, policy, date, None, "layers_merged", |row| {
                    row.end(date, |s| s.cover == *cover && s.layer.is_some_and(|l| removed.contains(&l)));
                    let kept = row
                        .spans
                        .iter()
                        .find(|s| s.known_to.is_none() && s.cover == *cover && s.layer == Some(*into))
                        .cloned();
                    if let Some(kept) = kept {
                        row.open(date, cover, &kept.cover_type, Some(*into), &kept.life, *sum_insured_minor, kept.start);
                    }
                });
            }
            Event::CoverExpired { policy, cover, on } => {
                self.change(partition, policy, date, Some(*on), "cover_expired", |row| row.end(date, |s| s.cover == *cover));
            }
            Event::LayerUnwound {
                policy, cover, layer, start, ..
            } => {
                self.change(partition, policy, date, Some(*start), "unwound", |row| {
                    row.end(date, |s| s.cover == *cover && s.layer == Some(*layer))
                });
            }
            Event::CoverExpiryUnwound { policy, layers, .. } => {
                self.change(partition, policy, date, None, "expiry_unwound", |row| {
                    for l in layers {
                        row.open_layer(date, l);
                    }
                });
            }
            Event::MemberJoined {
                policy,
                effective,
                covers,
                info,
                ..
            } => {
                self.change(partition, policy, date, Some(*effective), "member_joined", |row| {
                    row.learn(date, info);
                    for m in covers {
                        row.open_member(date, m, *effective);
                    }
                });
            }
            Event::MemberChanged {
                policy,
                life,
                effective,
                covers,
                info,
            } => {
                self.change(partition, policy, date, Some(*effective), "member_changed", |row| {
                    row.learn(date, info);
                    row.end(date, |s| s.layer.is_none() && s.life == *life);
                    for m in covers {
                        row.open_member(date, m, *effective);
                    }
                });
            }
            Event::MemberLeft { policy, life, effective } => {
                self.change(partition, policy, date, Some(*effective), "member_left", |row| {
                    row.end(date, |s| s.layer.is_none() && s.life == *life)
                });
            }
            Event::PremiumDue {
                policy,
                receivable,
                period_start,
                total_minor,
                ..
            } => self.premium(partition, policy, date, Some(*period_start), "premium_due", Some(*receivable), *total_minor),
            Event::PremiumAdjusted {
                policy,
                receivable,
                effective,
                total_minor,
                ..
            } => self.premium(partition, policy, date, Some(*effective), "premium_adjusted", Some(*receivable), *total_minor),
            Event::LoadingAdjusted {
                policy,
                receivable,
                total_minor,
            } => self.premium(partition, policy, date, None, "loading_adjusted", Some(*receivable), *total_minor),
            Event::PolicyUnwound {
                policy, after, receivables, ..
            } => {
                let reversed: i64 = match self.policies.get_mut(&(partition, policy.clone())) {
                    Some(row) => receivables.iter().filter_map(|r| row.premiums.remove(r)).sum(),
                    None => 0,
                };
                if reversed != 0 {
                    self.premium(partition, policy, date, Some(*after), "premium_unwound", None, -reversed);
                }
            }
            Event::PolicyTerminated {
                policy, effective, payable, ..
            } => {
                if let Some(row) = self.policies.get(&(partition, policy.clone())) {
                    for p in payable {
                        // The life the cover is on, where it is on one life only.
                        let mut on: Vec<&LifeId> = row.spans.iter().filter(|s| s.cover == p.cover).map(|s| &s.life).collect();
                        on.dedup();
                        let f = match on.as_slice() {
                            [life] => row.factors(life, date),
                            _ => Factors::default(),
                        };
                        self.claims.push(EndingClaim {
                            business_date: date,
                            product_code: row.product_code.clone(),
                            cover_type: p.cover_type.clone(),
                            sum_insured_minor: p.sum_insured_minor,
                            age: f.age(*effective),
                            sex: f.sex,
                            smoker: f.smoker,
                            occupation_class: f.class(),
                        });
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A policy the partition has not held before.
    fn arrive(&mut self, partition: PartitionId, policy: &PolicyId, row: PolicyRow, date: NaiveDate, effective: Option<NaiveDate>, kind: &str) {
        let key = (partition, policy.clone());
        let before = self.policies.get(&key).map(PolicyRow::contribution);
        let product = row.product_code.clone();
        let after = row.contribution();
        let was = self.policies.get(&key).is_some_and(PolicyRow::in_force_now);
        let is = row.in_force_now();
        self.policies.insert(key, row);
        self.record(partition, policy, &product, date, effective, kind, before.unwrap_or_default(), after, was, is);
    }

    fn change(&mut self, partition: PartitionId, policy: &PolicyId, date: NaiveDate, effective: Option<NaiveDate>, kind: &str, f: impl FnOnce(&mut PolicyRow)) {
        let Some(row) = self.policies.get_mut(&(partition, policy.clone())) else {
            return;
        };
        let (before, was) = (row.contribution(), row.in_force_now());
        f(row);
        let (after, is) = (row.contribution(), row.in_force_now());
        let product = row.product_code.clone();
        self.record(partition, policy, &product, date, effective, kind, before, after, was, is);
    }

    /// Write the difference between two states of one policy as movements.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        partition: PartitionId,
        policy: &PolicyId,
        product: &str,
        date: NaiveDate,
        effective: Option<NaiveDate>,
        kind: &str,
        before: BTreeMap<CoverKey, (String, i64)>,
        mut after: BTreeMap<CoverKey, (String, i64)>,
        was: bool,
        is: bool,
    ) {
        let blank = |policies: i64| Movement {
            partition,
            policy: policy.clone(),
            product_code: product.to_string(),
            business_date: date,
            effective,
            kind: kind.to_string(),
            cover: String::new(),
            cover_type: String::new(),
            layer: None,
            life: None,
            policies,
            covers: 0,
            sum_insured_minor: 0,
            premium_minor: 0,
        };
        if was != is {
            self.movements.push(blank(i64::from(is) - i64::from(was)));
        }
        let mut cover_row = |key: CoverKey, cover_type: String, covers: i64, sum: i64| {
            self.movements.push(Movement {
                cover: key.0,
                cover_type,
                layer: key.1,
                life: Some(key.2),
                covers,
                sum_insured_minor: sum,
                ..blank(0)
            });
        };
        for (key, (cover_type, sum)) in before {
            match after.remove(&key) {
                Some((_, now)) if now == sum => {}
                Some((_, now)) => cover_row(key, cover_type, 0, now - sum),
                None => cover_row(key, cover_type, -1, -sum),
            }
        }
        for (key, (cover_type, sum)) in after {
            cover_row(key, cover_type, 1, sum);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn premium(
        &mut self,
        partition: PartitionId,
        policy: &PolicyId,
        date: NaiveDate,
        effective: Option<NaiveDate>,
        kind: &str,
        receivable: Option<u32>,
        minor: i64,
    ) {
        let Some(row) = self.policies.get_mut(&(partition, policy.clone())) else {
            return;
        };
        if let Some(receivable) = receivable {
            *row.premiums.entry(receivable).or_insert(0) += minor;
        }
        self.movements.push(Movement {
            partition,
            policy: policy.clone(),
            product_code: row.product_code.clone(),
            business_date: date,
            effective,
            kind: kind.to_string(),
            cover: String::new(),
            cover_type: String::new(),
            layer: None,
            life: None,
            policies: 0,
            covers: 0,
            sum_insured_minor: 0,
            premium_minor: minor,
        });
    }

    /// An extract is cut only as at a date every partition has closed.
    fn closed(&self, date: NaiveDate) -> Result<(), MartError> {
        let behind = |p: &PartitionId| self.feeds.get(p).and_then(|f| f.closed).is_none_or(|c| c < date);
        match (0..self.partitions.max(1)).find(behind) {
            Some(partition) => Err(MartError::NotClosed { partition, date }),
            None => Ok(()),
        }
    }

    fn totals_at(&self, date: NaiveDate) -> Totals {
        let mut t = Totals::default();
        for row in self.policies.values() {
            if !row.status_at(date).is_some_and(provides_cover) {
                continue;
            }
            t.policies += 1;
            for span in row.spans.iter().filter(|s| s.known_at(date)) {
                t.covers += 1;
                t.sum_insured_minor += i128::from(span.sum_insured_minor);
            }
        }
        t
    }

    /// The covers in force as at the close of `as_at` on the named cover
    /// codes, each with what a rate table prices it on.
    pub fn rated_covers(&self, as_at: NaiveDate, covers: &BTreeSet<String>) -> Result<Vec<RatedCover>, MartError> {
        self.closed(as_at)?;
        let mut rated = Vec::new();
        for ((partition, policy), row) in &self.policies {
            if !row.status_at(as_at).is_some_and(provides_cover) {
                continue;
            }
            let mut spans: Vec<&Span> = row.spans.iter().filter(|s| s.known_at(as_at) && covers.contains(&s.cover)).collect();
            spans.sort_by(|a, b| (&a.cover, a.layer, &a.life).cmp(&(&b.cover, b.layer, &b.life)));
            for s in spans {
                let factors = row.factors(&s.life, as_at);
                rated.push(RatedCover {
                    partition: *partition,
                    policy: policy.clone(),
                    cover: s.cover.clone(),
                    layer: s.layer,
                    age: factors.age(as_at),
                    age_at_start: factors.age(s.start),
                    sex: factors.sex,
                    smoker: factors.smoker,
                    occupation_class: factors.occupation_class,
                    sum_insured_minor: s.sum_insured_minor,
                });
            }
        }
        Ok(rated)
    }

    /// Every cover in force as at the close of `as_at`.
    pub fn in_force(&self, as_at: NaiveDate) -> Result<Extract, MartError> {
        self.closed(as_at)?;
        let mut lines =
            vec!["partition,policy,product,inception,status,cover,cover_type,layer,life,age,sex,smoker,occupation_class,start,sum_insured_minor".to_string()];
        for ((partition, policy), row) in &self.policies {
            let Some(status) = row.status_at(as_at).filter(|s| provides_cover(*s)) else {
                continue;
            };
            let mut spans: Vec<&Span> = row.spans.iter().filter(|s| s.known_at(as_at)).collect();
            spans.sort_by(|a, b| (&a.cover, a.layer, &a.life).cmp(&(&b.cover, b.layer, &b.life)));
            for s in spans {
                lines.push(format!(
                    "{partition},{policy},{},{},{},{},{},{},{},{},{},{}",
                    row.product_code,
                    opt(&row.inception),
                    status_name(status),
                    s.cover,
                    s.cover_type,
                    opt(&s.layer),
                    s.life,
                    row.factors(&s.life, as_at).columns(as_at),
                    s.start,
                    s.sum_insured_minor
                ));
            }
        }
        let totals = self.totals_at(as_at);
        // The book as at the date is everything that has moved up to it.
        let moved = self.moved(None, as_at);
        Ok(Extract {
            kind: IN_FORCE,
            from: None,
            as_at,
            lines,
            control: control(totals, None),
            reconciled: moved == totals,
        })
    }

    fn moved(&self, from: Option<NaiveDate>, to: NaiveDate) -> Totals {
        let mut t = Totals::default();
        for m in self.in_period(from, to) {
            t.policies += i128::from(m.policies);
            t.covers += i128::from(m.covers);
            t.sum_insured_minor += i128::from(m.sum_insured_minor);
        }
        t
    }

    fn in_period(&self, from: Option<NaiveDate>, to: NaiveDate) -> impl Iterator<Item = &Movement> {
        self.movements
            .iter()
            .filter(move |m| from.is_none_or(|f| m.business_date >= f) && m.business_date <= to)
    }

    /// The age on the day of the movement, sex and smoker status of the life
    /// a movement is about. Empty for a row about the policy as a whole.
    fn movement_factors(&self, m: &Movement) -> String {
        let factors = m
            .life
            .as_ref()
            .and_then(|life| {
                self.policies
                    .get(&(m.partition, m.policy.clone()))
                    .map(|row| row.factors(life, m.business_date))
            })
            .unwrap_or_default();
        factors.columns(m.business_date)
    }

    /// Every change recorded from `from` to `to`, both days included.
    pub fn movement(&self, from: NaiveDate, to: NaiveDate) -> Result<Extract, MartError> {
        if from > to {
            return Err(MartError::Range { from, to });
        }
        self.closed(to)?;
        let mut rows: Vec<&Movement> = self.in_period(Some(from), to).collect();
        // Stable: movements of one policy on one day keep the order they happened in.
        rows.sort_by(|a, b| (a.business_date, a.partition, &a.policy).cmp(&(b.business_date, b.partition, &b.policy)));
        let mut lines = vec![
            "business_date,partition,policy,product,kind,effective,cover,cover_type,layer,life,age,sex,smoker,occupation_class,policies,covers,sum_insured_minor,premium_minor"
                .to_string(),
        ];
        let mut premium: i128 = 0;
        for m in rows {
            premium += i128::from(m.premium_minor);
            lines.push(format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                m.business_date,
                m.partition,
                m.policy,
                m.product_code,
                m.kind,
                opt(&m.effective),
                m.cover,
                m.cover_type,
                opt(&m.layer),
                opt(&m.life),
                self.movement_factors(m),
                m.policies,
                m.covers,
                m.sum_insured_minor,
                m.premium_minor
            ));
        }
        let opening = from.pred_opt().map(|day| self.totals_at(day)).unwrap_or_default();
        let closing = self.totals_at(to);
        let moved = self.moved(Some(from), to);
        let mut control = control(closing, Some(opening));
        control.insert("premium_minor".to_string(), premium);
        Ok(Extract {
            kind: MOVEMENT,
            from: Some(from),
            as_at: to,
            lines,
            control,
            reconciled: opening.policies + moved.policies == closing.policies
                && opening.covers + moved.covers == closing.covers
                && opening.sum_insured_minor + moved.sum_insured_minor == closing.sum_insured_minor,
        })
    }

    /// Days of exposure from `from` to `to` and the claims that ended a
    /// policy, by product and cover type.
    pub fn experience(&self, from: NaiveDate, to: NaiveDate) -> Result<Extract, MartError> {
        if from > to {
            return Err(MartError::Range { from, to });
        }
        self.closed(to)?;
        #[derive(Default)]
        struct Cell {
            cover_days: i128,
            sum_insured_days_minor: i128,
            claims: i128,
            claim_sum_insured_minor: i128,
        }
        // Product, cover type, age band at the end of the period, sex, smoker,
        // occupation class.
        type CellKey = (String, String, String, &'static str, String, String);
        let mut cells: BTreeMap<CellKey, Cell> = BTreeMap::new();
        for row in self.policies.values() {
            let runs = row.in_force_days(from, to);
            for span in &row.spans {
                // The last day the cover was known: the day before it was recorded as ended.
                let last = match span.known_to {
                    Some(end) => match end.pred_opt() {
                        Some(day) => day,
                        None => continue,
                    },
                    None => to,
                };
                let days: i64 = runs
                    .iter()
                    .map(|(a, b)| ((*b).min(last) - (*a).max(span.known_from)).num_days() + 1)
                    .filter(|d| *d > 0)
                    .sum();
                if days > 0 {
                    let f = row.factors(&span.life, to);
                    let key = (
                        row.product_code.clone(),
                        span.cover_type.clone(),
                        age_band(f.age(to)),
                        sex_name(f.sex),
                        opt(&f.smoker),
                        f.class(),
                    );
                    let cell = cells.entry(key).or_default();
                    cell.cover_days += i128::from(days);
                    cell.sum_insured_days_minor += i128::from(days) * i128::from(span.sum_insured_minor);
                }
            }
        }
        for claim in self.claims.iter().filter(|c| c.business_date >= from && c.business_date <= to) {
            let key = (
                claim.product_code.clone(),
                claim.cover_type.clone(),
                age_band(claim.age),
                sex_name(claim.sex),
                opt(&claim.smoker),
                claim.occupation_class.clone(),
            );
            let cell = cells.entry(key).or_default();
            cell.claims += 1;
            cell.claim_sum_insured_minor += i128::from(claim.sum_insured_minor);
        }
        let mut lines = vec![
            "product,cover_type,age_band,sex,smoker,occupation_class,cover_days,sum_insured_days_minor,ending_claims,ending_claim_sum_insured_minor"
                .to_string(),
        ];
        let mut control: BTreeMap<String, i128> = BTreeMap::new();
        for ((product, cover_type, band, sex, smoker, class), c) in &cells {
            lines.push(format!(
                "{product},{cover_type},{band},{sex},{smoker},{class},{},{},{},{}",
                c.cover_days, c.sum_insured_days_minor, c.claims, c.claim_sum_insured_minor
            ));
            *control.entry("cover_days".to_string()).or_insert(0) += c.cover_days;
            *control.entry("sum_insured_days_minor".to_string()).or_insert(0) += c.sum_insured_days_minor;
            *control.entry("ending_claims".to_string()).or_insert(0) += c.claims;
        }
        // Exposure worked out day by day from the in-force book must agree.
        let mut daily = Totals::default();
        let mut day = from;
        while day <= to {
            let t = self.totals_at(day);
            daily.covers += t.covers;
            daily.sum_insured_minor += t.sum_insured_minor;
            match day.succ_opt() {
                Some(next) => day = next,
                None => break,
            }
        }
        let reconciled = control.get("cover_days").copied().unwrap_or(0) == daily.covers
            && control.get("sum_insured_days_minor").copied().unwrap_or(0) == daily.sum_insured_minor;
        Ok(Extract {
            kind: EXPERIENCE,
            from: Some(from),
            as_at: to,
            lines,
            control,
            reconciled,
        })
    }
}

fn control(closing: Totals, opening: Option<Totals>) -> BTreeMap<String, i128> {
    let mut out = BTreeMap::new();
    out.insert("policies".to_string(), closing.policies);
    out.insert("covers".to_string(), closing.covers);
    out.insert("sum_insured_minor".to_string(), closing.sum_insured_minor);
    if let Some(opening) = opening {
        out.insert("opening_policies".to_string(), opening.policies);
        out.insert("opening_covers".to_string(), opening.covers);
        out.insert("opening_sum_insured_minor".to_string(), opening.sum_insured_minor);
    }
    out
}
