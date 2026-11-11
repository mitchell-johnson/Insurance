//! Distribution rules: who is paid what share of each commission component
//! on a book, and who bears clawback.
//!
//! A rule is a set of dated lines. Tails are shares of the whole component;
//! the ongoing parties share what is left, so a tail that reaches its end
//! date flows to them with no new rule. Overrides and fixed fees are paid on
//! top of base commission by lines of their own, usually in a fragment shared
//! by every book under the same provider or group.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::PartyId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{BookId, EngagementId, FragmentId};

/// Shares are in parts per million: 1,000,000 is the whole.
pub const WHOLE: u32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    /// Commission from the layer's own terms.
    Base,
    /// Paid on top of base to a party above the adviser, named by level.
    Override(String),
    /// A flat amount per event, such as a referral fee.
    FixedFee(String),
}

impl Component {
    pub fn label(&self) -> String {
        match self {
            Component::Base => "base".to_string(),
            Component::Override(l) => format!("override:{l}"),
            Component::FixedFee(l) => format!("fee:{l}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommissionType {
    Initial,
    Renewal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Share {
    /// A share of the whole component: a tail.
    OfComponent(u32),
    /// A share of what earlier lines leave.
    OfRemainder(u32),
    /// A rate on the premium that earned the event. Overrides only.
    RateOnPremium(u32),
    /// A fixed amount in minor units per event. Fixed fees only.
    Fixed(i64),
}

/// Why money is kept in a held account instead of being paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldReason {
    SuspendedLicence,
    Dispute,
    MissingPaymentDetails,
    LegalHold,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub payee: PartyId,
    /// The engagement that earns the line, where it is one person's work.
    #[serde(default)]
    pub earner: Option<EngagementId>,
    pub component: Component,
    /// None applies to both initial and renewal commission.
    #[serde(default)]
    pub commission_type: Option<CommissionType>,
    pub share: Share,
    pub from: NaiveDate,
    /// The last day the line applies.
    #[serde(default)]
    pub until: Option<NaiveDate>,
    /// The share is disputed: it is held under the payee until resolved.
    #[serde(default)]
    pub disputed: bool,
    /// The one line of its component that takes the odd cent.
    #[serde(default)]
    pub absorbs_rounding: bool,
}

impl Line {
    pub fn on(&self, day: NaiveDate) -> bool {
        self.from <= day && self.until.is_none_or(|u| day <= u)
    }
    fn applies(&self, component: &Component, ctype: CommissionType, day: NaiveDate) -> bool {
        &self.component == component && self.commission_type.is_none_or(|t| t == ctype) && self.on(day)
    }
}

/// Who bears clawback of a component, and who stands behind them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiabilityLine {
    pub component: Component,
    pub liable: PartyId,
    pub share: u32,
    #[serde(default)]
    pub guarantor: Option<PartyId>,
    pub from: NaiveDate,
    #[serde(default)]
    pub until: Option<NaiveDate>,
}

impl LiabilityLine {
    pub fn on(&self, day: NaiveDate) -> bool {
        self.from <= day && self.until.is_none_or(|u| day <= u)
    }
}

/// Lines shared by every book that references them: a provider's standard
/// lines, a group's overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fragment {
    pub id: FragmentId,
    pub version: u32,
    pub effective_from: NaiveDate,
    pub lines: Vec<Line>,
    #[serde(default)]
    pub liability: Vec<LiabilityLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub book: BookId,
    pub version: u32,
    pub effective_from: NaiveDate,
    /// The lines specific to this book.
    pub lines: Vec<Line>,
    #[serde(default)]
    pub liability: Vec<LiabilityLine>,
    /// Shared fragments this book follows.
    #[serde(default)]
    pub fragments: Vec<FragmentId>,
    /// A clawback caused by an alteration falls on the payees of the
    /// engagement that submitted it. Off unless an agreement turns it on.
    #[serde(default)]
    pub clawback_by_cause: bool,
    /// The signed instructions that support the rule.
    pub instructions_ref: String,
}

fn hash_of<T: Serialize>(tag: &[u8], value: &T) -> String {
    let mut h = Sha256::new();
    h.update(tag);
    // Structs with fixed field order and no maps: the JSON form is canonical.
    h.update(serde_json::to_vec(value).unwrap_or_default());
    hex::encode(h.finalize())
}

impl Rule {
    pub fn content_hash(&self) -> String {
        hash_of(b"ic.distribution.rule.v1", self)
    }
}

impl Fragment {
    pub fn content_hash(&self) -> String {
        hash_of(b"ic.distribution.fragment.v1", self)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuleError {
    #[error("{0}")]
    Invalid(String),
}

fn invalid<T>(m: impl Into<String>) -> Result<T, RuleError> {
    Err(RuleError::Invalid(m.into()))
}

/// One amount for one payee out of one component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allocated {
    pub payee: PartyId,
    pub earner: Option<EngagementId>,
    pub component: Component,
    pub disputed: bool,
    pub amount_minor: i64,
}

/// `amount * ppm / WHOLE`, rounded half to even, exact in integers.
pub fn portion(amount: i64, ppm: u32) -> i64 {
    let n = amount as i128 * ppm as i128;
    let d = WHOLE as i128;
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    let rounded = match (2 * r).cmp(&d) {
        std::cmp::Ordering::Less => q,
        std::cmp::Ordering::Greater => q + 1,
        std::cmp::Ordering::Equal => q + (q & 1),
    };
    rounded as i64
}

/// A rule together with the fragments it follows: the lines in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed<'a> {
    pub lines: Vec<&'a Line>,
    pub liability: Vec<&'a LiabilityLine>,
}

impl<'a> Composed<'a> {
    pub fn of(rule: &'a Rule, fragments: &[&'a Fragment]) -> Self {
        let mut lines: Vec<&Line> = rule.lines.iter().collect();
        let mut liability: Vec<&LiabilityLine> = rule.liability.iter().collect();
        // A book's own lines for a component replace a shared fragment's
        // lines for it: that is how negotiated exceptions are written.
        let own: BTreeSet<&Component> = rule.lines.iter().map(|l| &l.component).collect();
        let own_liability: BTreeSet<&Component> = rule.liability.iter().map(|l| &l.component).collect();
        for f in fragments {
            lines.extend(f.lines.iter().filter(|l| !own.contains(&l.component)));
            liability.extend(f.liability.iter().filter(|l| !own_liability.contains(&l.component)));
        }
        Self { lines, liability }
    }

    /// The components other than base that pay on a day.
    pub fn extra_components(&self, ctype: CommissionType, day: NaiveDate) -> BTreeSet<Component> {
        self.lines
            .iter()
            .filter(|l| l.component != Component::Base && l.commission_type.is_none_or(|t| t == ctype) && l.on(day))
            .map(|l| l.component.clone())
            .collect()
    }

    /// Split a base component of `gross` among its payees on a day. The
    /// pieces always sum to `gross` to the cent.
    pub fn allocate_base(&self, gross: i64, ctype: CommissionType, day: NaiveDate) -> Result<Vec<Allocated>, RuleError> {
        let lines: Vec<&&Line> = self.lines.iter().filter(|l| l.applies(&Component::Base, ctype, day)).collect();
        if lines.is_empty() {
            return invalid(format!("no line pays base {ctype:?} commission on {day}"));
        }
        let mut out = Vec::with_capacity(lines.len());
        let mut tails = 0i64;
        for l in &lines {
            if let Share::OfComponent(ppm) = l.share {
                let a = portion(gross, ppm);
                tails += a;
                out.push((**l, a));
            }
        }
        let remainder = gross - tails;
        for l in &lines {
            if let Share::OfRemainder(ppm) = l.share {
                out.push((**l, portion(remainder, ppm)));
            }
        }
        let paid: i64 = out.iter().map(|(_, a)| a).sum();
        let odd = gross - paid;
        if odd != 0 {
            let absorber = out
                .iter_mut()
                .find(|(l, _)| l.absorbs_rounding)
                .ok_or_else(|| RuleError::Invalid(format!("no line absorbs rounding for base {ctype:?} commission on {day}")))?;
            absorber.1 += odd;
        }
        Ok(out
            .into_iter()
            .map(|(l, amount_minor)| Allocated {
                payee: l.payee.clone(),
                earner: l.earner.clone(),
                component: Component::Base,
                disputed: l.disputed,
                amount_minor,
            })
            .collect())
    }

    /// The lines of an override or fee component on a day, each paid in full
    /// on top of base. `premium` is what the event's premium was for the
    /// layer; `events` is 1 when the event pays fixed fees and 0 otherwise.
    pub fn allocate_extra(&self, component: &Component, premium: i64, pays_fees: bool, ctype: CommissionType, day: NaiveDate) -> Vec<Allocated> {
        self.lines
            .iter()
            .filter(|l| l.applies(component, ctype, day))
            .filter_map(|l| {
                let amount_minor = match l.share {
                    Share::RateOnPremium(ppm) => portion(premium, ppm),
                    Share::Fixed(a) if pays_fees => a,
                    _ => 0,
                };
                (amount_minor != 0).then(|| Allocated {
                    payee: l.payee.clone(),
                    earner: l.earner.clone(),
                    component: component.clone(),
                    disputed: l.disputed,
                    amount_minor,
                })
            })
            .collect()
    }

    /// Who bears clawback of a component paid on a day, with shares that sum
    /// to the whole. Where the rule names nobody, whoever was paid bears it.
    pub fn liable(&self, component: &Component, day: NaiveDate) -> Vec<(PartyId, u32, Option<PartyId>)> {
        self.liability
            .iter()
            .filter(|l| &l.component == component && l.on(day))
            .map(|l| (l.liable.clone(), l.share, l.guarantor.clone()))
            .collect()
    }

    /// Every date on which the set of lines in force changes, from `from`.
    fn boundaries(&self, from: NaiveDate) -> BTreeSet<NaiveDate> {
        let mut days = BTreeSet::from([from]);
        for l in &self.lines {
            if l.from > from {
                days.insert(l.from);
            }
            if let Some(next) = l.until.and_then(|u| u.succ_opt()).filter(|n| *n > from) {
                days.insert(next);
            }
        }
        for l in &self.liability {
            if l.from > from {
                days.insert(l.from);
            }
            if let Some(next) = l.until.and_then(|u| u.succ_opt()).filter(|n| *n > from) {
                days.insert(next);
            }
        }
        days
    }

    /// The checks made before a rule is activated: on every date from its
    /// effective date, base commission of both types is allocated in full,
    /// exactly one line absorbs rounding, shares are of a kind their
    /// component allows, and liability shares sum to the whole.
    pub fn validate(&self, effective_from: NaiveDate) -> Result<(), RuleError> {
        for l in &self.lines {
            if l.until.is_some_and(|u| u < l.from) {
                return invalid(format!("a line for {} ends before it starts", l.payee));
            }
            let ok = match (&l.component, l.share) {
                (Component::Base, Share::OfComponent(p) | Share::OfRemainder(p)) => p > 0 && p <= WHOLE,
                (Component::Override(_), Share::RateOnPremium(p)) => p > 0 && p <= WHOLE,
                (Component::FixedFee(_), Share::Fixed(a)) => a > 0,
                _ => false,
            };
            if !ok {
                return invalid(format!("the share on the {} line for {} does not fit its component", l.component.label(), l.payee));
            }
        }
        for day in self.boundaries(effective_from) {
            for ctype in [CommissionType::Initial, CommissionType::Renewal] {
                let lines: Vec<&&Line> = self.lines.iter().filter(|l| l.applies(&Component::Base, ctype, day)).collect();
                if lines.is_empty() {
                    return invalid(format!("base {ctype:?} commission is paid to nobody from {day}"));
                }
                let tails: u64 = lines.iter().filter_map(|l| if let Share::OfComponent(p) = l.share { Some(p as u64) } else { None }).sum();
                let rest: u64 = lines.iter().filter_map(|l| if let Share::OfRemainder(p) = l.share { Some(p as u64) } else { None }).sum();
                let whole = WHOLE as u64;
                let complete = if rest == 0 { tails == whole } else { rest == whole && tails < whole };
                if !complete {
                    return invalid(format!(
                        "base {ctype:?} commission is not allocated in full from {day}: tails {tails}, shares of the remainder {rest}"
                    ));
                }
                if lines.iter().filter(|l| l.absorbs_rounding).count() != 1 {
                    return invalid(format!("exactly one base {ctype:?} line must absorb rounding from {day}"));
                }
            }
            let mut by_component: BTreeMap<&Component, u64> = BTreeMap::new();
            for l in self.liability.iter().filter(|l| l.on(day)) {
                *by_component.entry(&l.component).or_insert(0) += l.share as u64;
            }
            if let Some((c, total)) = by_component.into_iter().find(|(_, t)| *t != WHOLE as u64) {
                return invalid(format!("clawback liability for {} sums to {total} from {day}, not the whole", c.label()));
            }
        }
        Ok(())
    }

    pub fn payees(&self) -> BTreeSet<PartyId> {
        self.lines
            .iter()
            .map(|l| l.payee.clone())
            .chain(self.liability.iter().map(|l| l.liable.clone()))
            .chain(self.liability.iter().filter_map(|l| l.guarantor.clone()))
            .collect()
    }
}
