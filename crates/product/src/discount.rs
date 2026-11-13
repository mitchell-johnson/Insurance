//! Discounts as pricing components.
//!
//! The base premium comes from the rating function. A product then declares
//! a chain of discounts, each with a scope (the covers it applies to), a
//! basis, a condition and a place in the order. The chain is worked out in
//! full precision on annual amounts; rounding belongs to the bill.
//!
//! A step of the chain is one discount, which applies to what earlier steps
//! left; an additive set, whose members each take their share of the base
//! premium; or a best-of set, of which only the largest applies. A cap on
//! the total and a floor under the net premium apply last, and what they
//! take back comes off the last discount in the order first, so earlier
//! ones are shown in full. A discount never increases a premium.

use std::collections::BTreeMap;

use ic_money::{Dec, DecStr};
use serde::{Deserialize, Serialize};

use crate::Frequency;

/// One discount a product offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discount {
    pub code: String,
    /// The name a premium notice shows.
    pub name: String,
    /// The ledger posting type its line is posted under.
    pub posting_type: String,
    /// The covers whose premium it reduces. Empty: every cover.
    #[serde(default)]
    pub covers: Vec<String>,
    pub basis: DiscountBasis,
    pub condition: DiscountCondition,
    /// The most this discount gives in a year, in major currency units.
    #[serde(default)]
    pub max_annual: Option<DecStr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscountBasis {
    /// A percentage of the premium the step applies to.
    Percent { percent: DecStr },
    /// An amount a year, in major currency units, spread over the covers in
    /// scope in proportion to their premium.
    FixedAnnual { amount: DecStr },
    /// A percentage read from the tier of the entitlement the condition names.
    Tier { percent_by_tier: BTreeMap<String, DecStr> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscountCondition {
    Always,
    /// The policy has at least this many covers priced.
    MinCovers {
        count: u32,
    },
    /// The policy has at least this many lives with cover priced.
    MinLives {
        count: u32,
    },
    /// Premium is paid at one of these frequencies.
    Frequency {
        frequencies: Vec<Frequency>,
    },
    /// An entitlement with this code is held: state set by command from the
    /// module that judges the outside data behind it.
    Entitlement {
        code: String,
    },
}

/// One step of a product's chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscountStep {
    /// Applies to what the earlier steps left.
    One { discount: Discount },
    /// Each member applies to the base premium; together they take no more
    /// than what the earlier steps left.
    Additive { discounts: Vec<Discount> },
    /// Only the member worth most applies.
    BestOf { discounts: Vec<Discount> },
}

impl DiscountStep {
    pub fn discounts(&self) -> &[Discount] {
        match self {
            DiscountStep::One { discount } => std::slice::from_ref(discount),
            DiscountStep::Additive { discounts } | DiscountStep::BestOf { discounts } => discounts,
        }
    }
}

/// One priced line: a cover on a life. A bill that is not priced by life
/// uses an empty life.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Line {
    pub cover: String,
    pub life: String,
}

/// An entitlement held on the day a period is priced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub code: String,
    /// Empty where the entitlement has no tiers.
    pub tier: String,
    /// The life that holds it. Absent: the policy holds it, for every life.
    pub life: Option<String>,
}

/// What of the policy's state the chain's conditions read.
#[derive(Debug, Clone, Copy)]
pub struct PricingState<'a> {
    pub covers: u32,
    pub lives: u32,
    pub frequency: Frequency,
    pub entitlements: &'a [Held],
}

/// A discount that applied, with what it took off each line's annual premium.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub code: String,
    pub name: String,
    pub posting_type: String,
    pub by_line: BTreeMap<Line, Dec>,
}

impl Applied {
    pub fn total(&self) -> Dec {
        sum(&self.by_line)
    }
}

/// The limits a chain is held to once every step has run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits<'a> {
    /// The most all discounts together take, as a percentage of base premium.
    pub max_total_percent: Option<&'a DecStr>,
    /// The least the net annual premium can be, in major currency units.
    pub min_net_annual: Option<&'a DecStr>,
}

fn zero() -> Dec {
    Dec::from(0i64)
}

fn sum(amounts: &BTreeMap<Line, Dec>) -> Dec {
    amounts.values().fold(zero(), |a, b| a + *b)
}

/// The fraction a discount takes, or the fixed amount, where its condition holds.
enum Rate {
    Fraction(Dec),
    Fixed(Dec),
}

/// A discount's rate on one line, where the line is in its scope and its
/// condition holds there. An entitlement held by a life reaches that life's
/// lines only; one held by the policy reaches them all.
fn rate(d: &Discount, state: &PricingState<'_>, line: &Line) -> Option<Rate> {
    if !(d.covers.is_empty() || d.covers.contains(&line.cover)) {
        return None;
    }
    let held = |code: &String| {
        state
            .entitlements
            .iter()
            .find(|h| &h.code == code && h.life.as_ref().is_none_or(|l| l == &line.life))
    };
    let holds = match &d.condition {
        DiscountCondition::Always => true,
        DiscountCondition::MinCovers { count } => state.covers >= *count,
        DiscountCondition::MinLives { count } => state.lives >= *count,
        DiscountCondition::Frequency { frequencies } => frequencies.contains(&state.frequency),
        DiscountCondition::Entitlement { code } => held(code).is_some(),
    };
    if !holds {
        return None;
    }
    let hundred = Dec::from(100i64);
    match (&d.basis, &d.condition) {
        (DiscountBasis::Percent { percent }, _) => Some(Rate::Fraction(percent.get() / hundred)),
        (DiscountBasis::FixedAnnual { amount }, _) => Some(Rate::Fixed(amount.get())),
        (DiscountBasis::Tier { percent_by_tier }, DiscountCondition::Entitlement { code }) => {
            percent_by_tier.get(&held(code)?.tier).map(|p| Rate::Fraction(p.get() / hundred))
        }
        (DiscountBasis::Tier { .. }, _) => None,
    }
}

/// What a discount takes off each line: a fraction of `of`, or its fixed
/// amount spread over what is `left` on the lines it reaches; never more
/// than is left on a line, and never more in all than the discount's own
/// cap. `None` where it reaches no line.
fn reduction(d: &Discount, state: &PricingState<'_>, of: &BTreeMap<Line, Dec>, left: &BTreeMap<Line, Dec>) -> Option<BTreeMap<Line, Dec>> {
    let rates: Vec<(&Line, &Dec, Rate)> = left.iter().filter_map(|(line, v)| rate(d, state, line).map(|r| (line, v, r))).collect();
    if rates.is_empty() {
        return None;
    }
    let reach = rates.iter().fold(zero(), |a, (_, v, _)| a + **v);
    let mut out = BTreeMap::new();
    for (line, remaining, rate) in &rates {
        let wanted = match rate {
            Rate::Fraction(f) => of.get(*line).copied().unwrap_or_else(zero) * *f,
            Rate::Fixed(_) if reach <= zero() => zero(),
            Rate::Fixed(amount) => *amount * **remaining / reach,
        };
        let taken = wanted.min(**remaining).max(zero());
        if taken > zero() {
            out.insert((*line).clone(), taken);
        }
    }
    if let Some(cap) = d.max_annual.as_ref().map(DecStr::get) {
        let total = sum(&out);
        if total > cap {
            out.values_mut().for_each(|v| *v = *v * cap / total);
        }
    }
    Some(out)
}

fn take(left: &mut BTreeMap<Line, Dec>, taken: &BTreeMap<Line, Dec>) {
    for (line, amount) in taken {
        if let Some(v) = left.get_mut(line) {
            *v -= *amount;
        }
    }
}

fn applied(d: &Discount, by_line: BTreeMap<Line, Dec>) -> Applied {
    Applied {
        code: d.code.clone(),
        name: d.name.clone(),
        posting_type: d.posting_type.clone(),
        by_line,
    }
}

/// Run a chain over the annual base premium of each line. Returns what is
/// left of each line's premium and the discounts that applied, in order.
pub fn apply(chain: &[DiscountStep], limits: Limits<'_>, base: &BTreeMap<Line, Dec>, state: &PricingState<'_>) -> (BTreeMap<Line, Dec>, Vec<Applied>) {
    let mut left = base.clone();
    let mut out: Vec<Applied> = Vec::new();
    for step in chain {
        match step {
            DiscountStep::One { discount } => {
                if let Some(taken) = reduction(discount, state, &left, &left) {
                    take(&mut left, &taken);
                    out.push(applied(discount, taken));
                }
            }
            DiscountStep::Additive { discounts } => {
                for d in discounts {
                    if let Some(taken) = reduction(d, state, base, &left) {
                        take(&mut left, &taken);
                        out.push(applied(d, taken));
                    }
                }
            }
            DiscountStep::BestOf { discounts } => {
                let mut best: Option<(&Discount, BTreeMap<Line, Dec>)> = None;
                for d in discounts {
                    if let Some(taken) = reduction(d, state, &left, &left) {
                        if best.as_ref().is_none_or(|(_, b)| sum(&taken) > sum(b)) {
                            best = Some((d, taken));
                        }
                    }
                }
                if let Some((d, taken)) = best {
                    take(&mut left, &taken);
                    out.push(applied(d, taken));
                }
            }
        }
    }
    // The cap on the total and the floor under the net, taken back from the
    // last discount first.
    let gross = sum(base);
    let mut allowed = gross;
    if let Some(cap) = limits.max_total_percent {
        allowed = allowed.min(gross * cap.get() / Dec::from(100i64));
    }
    if let Some(floor) = limits.min_net_annual {
        allowed = allowed.min((gross - floor.get()).max(zero()));
    }
    let mut excess = out.iter().fold(zero(), |a, d| a + d.total()) - allowed;
    for d in out.iter_mut().rev() {
        let total = d.total();
        if excess <= zero() || total <= zero() {
            continue;
        }
        let back = excess.min(total);
        for (line, amount) in d.by_line.iter_mut() {
            let returned = *amount * back / total;
            *amount -= returned;
            if let Some(v) = left.get_mut(line) {
                *v += returned;
            }
        }
        excess -= back;
    }
    out.retain(|d| d.total() > zero());
    (left, out)
}
