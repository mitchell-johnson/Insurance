//! The cession calculation: a pure function of the covers on one life and
//! the treaty versions they were written under.
//!
//! Retention is filled in a fixed order, by the date each cover starts and
//! then by its key, so the answer never depends on the order events arrived
//! in. The result is a timeline per cover: what was ceded from each date on
//! which anything about the life changed. A backdated change is the same
//! calculation with one more date in it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_ledger::PartitionId;
use serde::{Deserialize, Serialize};

use crate::math::{share_of, split_exact};
use crate::treaty::{Basis, ExcessPath, Treaty, UnnamedBasis};
use crate::{ReinsurerId, RiskKey, TreatyRef};

/// Where a risk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A layer of a cover.
    Layer,
    /// A scheme member's cover, written by a membership file.
    Member,
    /// Capacity held for a proposal that has not been issued.
    Reservation,
}

/// A reinsurer's terms for one cover, outside the treaty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facultative {
    pub reference: String,
    pub reinsurer: ReinsurerId,
    pub amount_minor: i64,
    /// Annual rate per thousand, in millionths.
    pub rate: i64,
    pub from: NaiveDate,
    /// Where the terms are filed. The terms themselves are not held here.
    pub terms_ref: String,
}

/// One cover on a life, with everything about it that moves a cession.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Risk {
    pub key: RiskKey,
    pub partition: PartitionId,
    pub product: String,
    pub cover_type: String,
    pub source: Source,
    /// Whether the cover was individually underwritten. A scheme member's
    /// formula cover is not.
    pub underwritten: bool,
    pub start: NaiveDate,
    #[serde(default)]
    pub end: Option<NaiveDate>,
    /// Sum at risk from each date, in date order.
    pub amounts: Vec<(NaiveDate, i64)>,
    /// When the policy was not in force: from, and until if it came back.
    #[serde(default)]
    pub suspensions: Vec<(NaiveDate, Option<NaiveDate>)>,
    /// The treaty version from each date. `None` is retained in full.
    pub treaties: Vec<(NaiveDate, Option<TreatyRef>)>,
    #[serde(default)]
    pub facultative: Vec<Facultative>,
    /// The treaty does not accept this risk automatically.
    #[serde(default)]
    pub excluded: bool,
    /// The scheme's own choice of path for excess cover on a member.
    #[serde(default)]
    pub excess_path: Option<ExcessPath>,
    /// A cession loaded from the legacy system. It stands, unrecalculated,
    /// for as long as the cover's amount and treaty are the ones it was
    /// loaded with.
    #[serde(default)]
    pub migrated: Option<Cession>,
}

impl Risk {
    pub fn amount_on(&self, on: NaiveDate) -> i64 {
        self.amounts.iter().rev().find(|(d, _)| *d <= on).map_or(0, |(_, a)| *a)
    }

    pub fn treaty_on(&self, on: NaiveDate) -> Option<&TreatyRef> {
        self.treaties.iter().rev().find(|(d, _)| *d <= on).and_then(|(_, t)| t.as_ref())
    }

    pub fn in_force_on(&self, on: NaiveDate) -> bool {
        self.start <= on && self.end.is_none_or(|e| on < e) && !self.suspensions.iter().any(|(from, to)| *from <= on && to.is_none_or(|t| on < t))
    }

    /// Every date on which something about this cover changes.
    pub fn change_dates(&self) -> BTreeSet<NaiveDate> {
        let mut out = BTreeSet::from([self.start]);
        out.extend(self.end);
        out.extend(self.amounts.iter().map(|(d, _)| *d));
        out.extend(self.treaties.iter().map(|(d, _)| *d));
        out.extend(self.facultative.iter().map(|f| f.from));
        for (from, to) in &self.suspensions {
            out.insert(*from);
            out.extend(*to);
        }
        out.retain(|d| *d >= self.start);
        out
    }

    /// The last date up to `on` on which this cover itself changed.
    fn last_change(&self, on: NaiveDate) -> NaiveDate {
        self.change_dates().range(..=on).next_back().copied().unwrap_or(self.start)
    }
}

/// A facultative share of a cession.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacPlacement {
    pub reference: String,
    pub reinsurer: ReinsurerId,
    pub amount_minor: i64,
    /// Annual rate per thousand, in millionths.
    pub rate: i64,
    /// Placed under a treaty's standing terms for group business, not under
    /// terms a reinsurer gave for this cover.
    #[serde(default)]
    pub standing: bool,
}

/// Why part of a cover has no reinsurance although it is over the retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnplacedReason {
    AboveAutomaticLimit,
    AboveFacultativeLimit,
    UnderwrittenExcessPending,
    ExcludedRisk,
}

/// What is ceded on one cover. The parts always add up to the sum at risk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cession {
    #[serde(default)]
    pub treaty: Option<TreatyRef>,
    pub sum_at_risk_minor: i64,
    pub retained_minor: i64,
    /// The treaty's share, by reinsurer on the panel.
    #[serde(default)]
    pub ceded: Vec<(ReinsurerId, i64)>,
    #[serde(default)]
    pub facultative: Vec<FacPlacement>,
    /// Over the retention with no reinsurance in place. Never silent: an
    /// amount here is always an open exception.
    #[serde(default)]
    pub unplaced_minor: i64,
    #[serde(default)]
    pub unplaced_reason: Option<UnplacedReason>,
}

impl Cession {
    pub fn treaty_ceded(&self) -> i64 {
        self.ceded.iter().map(|(_, a)| *a).sum()
    }
    pub fn facultative_ceded(&self) -> i64 {
        self.facultative.iter().map(|f| f.amount_minor).sum()
    }
    fn standing_facultative(&self) -> i64 {
        self.facultative.iter().filter(|f| f.standing).map(|f| f.amount_minor).sum()
    }
    /// The amount ceded to one reinsurer, under the treaty and facultatively.
    pub fn ceded_to(&self, reinsurer: &ReinsurerId) -> i64 {
        self.ceded.iter().filter(|(r, _)| r == reinsurer).map(|(_, a)| *a).sum::<i64>()
            + self
                .facultative
                .iter()
                .filter(|f| &f.reinsurer == reinsurer)
                .map(|f| f.amount_minor)
                .sum::<i64>()
    }
    /// Whether the parts add up to the sum at risk.
    pub fn balanced(&self) -> bool {
        self.retained_minor + self.treaty_ceded() + self.facultative_ceded() + self.unplaced_minor == self.sum_at_risk_minor
    }
}

/// A cession from a date. `None` is no cession: the cover is not in force.
pub type Segment = (NaiveDate, Option<Cession>);

/// What the covers ordered before this one already use of a treaty on a life.
#[derive(Default)]
struct Pool {
    retained: i64,
    ceded: i64,
    standing_facultative: i64,
}

struct Calc<'a, F: Fn(&TreatyRef) -> Option<&'a Treaty>> {
    risks: Vec<&'a Risk>,
    treaty: F,
    matched: bool,
    memo: BTreeMap<(usize, NaiveDate), Cession>,
}

impl<'a, F: Fn(&TreatyRef) -> Option<&'a Treaty>> Calc<'a, F> {
    fn pool(&mut self, i: usize, tref: &TreatyRef, treaty: &Treaty, at: NaiveDate) -> Pool {
        let mut pool = Pool::default();
        for j in 0..i {
            let other = self.risks[j];
            if !other.in_force_on(at) {
                continue;
            }
            let Some(theirs) = other.treaty_on(at) else { continue };
            let same = theirs.id == tref.id;
            if !same && (self.treaty)(theirs).is_none_or(|t| t.retention_key() != treaty.retention_key()) {
                continue;
            }
            let c = self.eval(j, at);
            // The retention is the insurer's, per life, across the treaties
            // that share it. What a treaty has accepted is its own.
            pool.retained += c.retained_minor;
            if same {
                pool.ceded += c.treaty_ceded();
                pool.standing_facultative += c.standing_facultative();
            }
        }
        pool
    }

    /// The cession on risk `i` as at `on`.
    fn eval(&mut self, i: usize, on: NaiveDate) -> Cession {
        if let Some(done) = self.memo.get(&(i, on)) {
            return done.clone();
        }
        let risk = self.risks[i];
        let sar = risk.amount_on(on);
        let retained_in_full = Cession {
            sum_at_risk_minor: sar,
            retained_minor: sar,
            ..Cession::default()
        };
        let out = match risk.treaty_on(on).and_then(|r| (self.treaty)(r).map(|t| (r, t))) {
            None => retained_in_full,
            Some((tref, _))
                if risk
                    .migrated
                    .as_ref()
                    .is_some_and(|m| m.sum_at_risk_minor == sar && m.treaty.as_ref() == Some(tref)) =>
            {
                risk.migrated.clone().unwrap_or(retained_in_full)
            }
            Some((tref, treaty)) => {
                // Where freed retention is not reallocated, a cover's cession
                // is settled when the cover itself last changed.
                let at = if treaty.reallocate_freed_retention { on } else { risk.last_change(on) };
                let pool = self.pool(i, tref, treaty, at);
                split(risk, treaty, tref, sar, on, &pool, self.matched)
            }
        };
        self.memo.insert((i, on), out.clone());
        out
    }
}

fn split(risk: &Risk, treaty: &Treaty, tref: &TreatyRef, sar: i64, on: NaiveDate, pool: &Pool, matched: bool) -> Cession {
    let mut c = Cession {
        treaty: Some(tref.clone()),
        sum_at_risk_minor: sar,
        ..Cession::default()
    };
    let retention = match &treaty.basis {
        Basis::Surplus { retention_minor, .. } | Basis::ExcessOfLoss { retention_minor, .. } => Some(*retention_minor),
        Basis::QuotaShare { .. } => None,
    };
    let mut ceded = 0;
    let mut over;
    let mut reason = UnplacedReason::AboveAutomaticLimit;
    match (&treaty.group, retention) {
        // A member not linked to a party cannot be accumulated per life, so
        // the scheme basis applies.
        (Some(group), _) if !matched => {
            over = 0;
            match group.unnamed {
                UnnamedBasis::SchemeShare { share } => {
                    ceded = share_of(sar, share);
                    c.retained_minor = sar - ceded;
                }
                UnnamedBasis::ExcessPerMember { limit_minor } => {
                    c.retained_minor = sar.min((limit_minor - pool.retained).max(0));
                    ceded = sar - c.retained_minor;
                }
            }
        }
        // Cover a membership file wrote without underwriting: the retention
        // is filled like anyone's, and what is over it follows the group
        // rules. It is never left retained.
        (Some(group), Some(retention)) if risk.source == Source::Member && !risk.underwritten => {
            c.retained_minor = sar.min((retention - pool.retained).max(0));
            over = sar - c.retained_minor;
            match risk.excess_path.unwrap_or(group.excess_path) {
                ExcessPath::Facultative => {
                    reason = UnplacedReason::AboveFacultativeLimit;
                    let placed = over.min((group.facultative_limit_minor - pool.standing_facultative).max(0));
                    if placed > 0 {
                        c.facultative.push(FacPlacement {
                            reference: format!("FACOB/{tref}/{}", risk.key),
                            reinsurer: group.facultative_reinsurer.clone(),
                            amount_minor: placed,
                            rate: group.facultative_rate,
                            standing: true,
                        });
                        over -= placed;
                    }
                }
                ExcessPath::UnderwrittenExcess => reason = UnplacedReason::UnderwrittenExcessPending,
            }
        }
        _ if risk.excluded => {
            over = sar;
            reason = UnplacedReason::ExcludedRisk;
        }
        _ => match &treaty.basis {
            Basis::QuotaShare { share, automatic_limit_minor } => {
                let within = sar.min((automatic_limit_minor - pool.retained - pool.ceded).max(0));
                ceded = share_of(within, *share);
                c.retained_minor = within - ceded;
                over = sar - within;
            }
            Basis::Surplus {
                retention_minor,
                automatic_limit_minor: limit,
            }
            | Basis::ExcessOfLoss {
                retention_minor,
                limit_minor: limit,
            } => {
                c.retained_minor = sar.min((retention_minor - pool.retained).max(0));
                let excess = sar - c.retained_minor;
                ceded = excess.min((limit - pool.ceded).max(0));
                over = excess - ceded;
            }
        },
    }
    // Terms a reinsurer gave for this cover take what the treaty did not.
    let mut terms: Vec<&Facultative> = risk.facultative.iter().filter(|f| f.from <= on).collect();
    terms.sort_by(|a, b| a.reference.cmp(&b.reference));
    for f in terms {
        let placed = over.min(f.amount_minor);
        if placed > 0 {
            c.facultative.push(FacPlacement {
                reference: f.reference.clone(),
                reinsurer: f.reinsurer.clone(),
                amount_minor: placed,
                rate: f.rate,
                standing: false,
            });
            over -= placed;
        }
    }
    if ceded > 0 {
        let weights: Vec<i64> = treaty.panel.iter().map(|p| p.share as i64).collect();
        c.ceded = treaty.panel.iter().map(|p| p.reinsurer.clone()).zip(split_exact(ceded, &weights)).collect();
    }
    if over > 0 {
        c.unplaced_minor = over;
        c.unplaced_reason = Some(reason);
    }
    c
}

/// The fixed order retention is filled in.
pub fn allocation_order(a: &Risk, b: &Risk) -> std::cmp::Ordering {
    (a.start, &a.key).cmp(&(b.start, &b.key))
}

/// The cession on every cover of one life, from every date on which it
/// changes. `matched` says whether the life is linked to a party.
pub fn timeline<'a>(
    risks: impl IntoIterator<Item = &'a Risk>,
    treaty: impl Fn(&TreatyRef) -> Option<&'a Treaty>,
    matched: bool,
) -> BTreeMap<RiskKey, Vec<Segment>> {
    let mut risks: Vec<&Risk> = risks.into_iter().collect();
    risks.sort_by(|a, b| allocation_order(a, b));
    let dates: BTreeSet<NaiveDate> = risks.iter().flat_map(|r| r.change_dates()).collect();
    let mut calc = Calc {
        risks,
        treaty,
        matched,
        memo: BTreeMap::new(),
    };
    let mut out: BTreeMap<RiskKey, Vec<Segment>> = BTreeMap::new();
    for i in 0..calc.risks.len() {
        let risk = calc.risks[i];
        let mut segments: Vec<Segment> = Vec::new();
        for on in dates.range(risk.start..) {
            let now = risk.in_force_on(*on).then(|| calc.eval(i, *on));
            if segments.last().map(|(_, c)| c) != Some(&now) {
                segments.push((*on, now));
            }
        }
        out.insert(risk.key.clone(), segments);
    }
    out
}

/// The cession in a timeline on a date.
pub fn cession_on(segments: &[Segment], on: NaiveDate) -> Option<&Cession> {
    segments.iter().rev().find(|(d, _)| *d <= on).and_then(|(_, c)| c.as_ref())
}
