//! Ceded premium: what each reinsurer is paid for each billing period of
//! each policy, worked out from the cession record.
//!
//! The charge for a period is a pure function of the cession timeline, the
//! treaty version and the rates in force when the period starts. Whenever a
//! cession changes, including for a date in the past, the periods it touches
//! are worked out again and only the difference is posted, with the period's
//! own value date. Nothing already posted is edited.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::PolicyId;
use serde::{Deserialize, Serialize};

use crate::account::LineKind;
use crate::ids::pairs;
use crate::machine::{Ctx, Reinsurance};
use crate::math::{div_half_even, share_of};
use crate::treaty::PremiumBasis;
use crate::{ReinsurerId, RiskKey, TreatyId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeKind {
    /// Premium for the treaty's share.
    Premium,
    /// Reinsurance commission on that premium.
    Commission,
    /// Premium for a facultative share.
    Facultative,
}

impl ChargeKind {
    fn line(self) -> LineKind {
        match self {
            ChargeKind::Premium => LineKind::CededPremium,
            ChargeKind::Commission => LineKind::Commission,
            ChargeKind::Facultative => LineKind::FacultativePremium,
        }
    }
}

/// What one charge is for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChargeKey {
    pub risk: RiskKey,
    pub treaty: TreatyId,
    pub reinsurer: ReinsurerId,
    pub kind: ChargeKind,
}

/// One billing period of one policy, and what has been charged for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    /// The first day after the period.
    pub end: NaiveDate,
    /// The core's receivable for the period.
    #[serde(default)]
    pub receivable: u32,
    /// The billing was reversed by an unwind: nothing is charged for it.
    #[serde(default)]
    pub reversed: bool,
    /// The customer's premium for the period by cover type before
    /// discounts, where the core supplied it ([`before_discounts`]). Needed
    /// by treaties priced on the original premium.
    #[serde(default)]
    pub by_cover_type: BTreeMap<String, i64>,
    #[serde(with = "pairs")]
    pub charged: BTreeMap<ChargeKey, i64>,
}

/// The premium by cover type before discounts: the core bills each cover
/// type after discounts and names the discounts for the whole policy, so
/// each discount is given back to the cover types in proportion to their
/// premium, the remainder to the largest. Exact for a flat percentage off
/// every cover, the only kind shipped. With nothing billed to share it by,
/// the premium is left as billed.
pub fn before_discounts(by_cover_type: BTreeMap<String, i64>, discounts_minor: i64) -> BTreeMap<String, i64> {
    let billed: i64 = by_cover_type.values().sum();
    if discounts_minor == 0 || billed <= 0 {
        return by_cover_type;
    }
    let mut given = 0;
    let mut gross: BTreeMap<String, i64> = by_cover_type
        .iter()
        .map(|(k, v)| {
            let share = (discounts_minor as i128 * *v as i128).div_euclid(billed as i128) as i64;
            given += share;
            (k.clone(), v + share)
        })
        .collect();
    if let Some(largest) = by_cover_type
        .iter()
        .max_by_key(|(k, v)| (**v, std::cmp::Reverse((*k).clone())))
        .map(|(k, _)| k.clone())
    {
        *gross.entry(largest).or_default() += discounts_minor - given;
    }
    gross
}

/// An annual rate per thousand, in millionths, applied to an amount for a
/// number of days.
pub fn rated(amount_minor: i64, rate: i64, days: i64) -> i64 {
    div_half_even(amount_minor as i128 * rate as i128 * days as i128, 1_000_000_000 * 365) as i64
}

impl Reinsurance {
    /// What should be charged for a period as the cession record stands,
    /// and the covers that could not be priced.
    fn charges(&self, policy: &PolicyId, start: NaiveDate, period: &Period) -> (BTreeMap<ChargeKey, i64>, Vec<String>) {
        let mut out: BTreeMap<ChargeKey, i64> = BTreeMap::new();
        let mut unpriced = Vec::new();
        let period_days = (period.end - start).num_days();
        let keys = self.policy_risks(policy);
        for key in &keys {
            let Some(risk) = self.risk(key) else { continue };
            let segments = self.cession_record(key);
            for (i, (from, cession)) in segments.iter().enumerate() {
                let Some(c) = cession else { continue };
                let until = segments.get(i + 1).map_or(NaiveDate::MAX, |(d, _)| *d);
                let first = (*from).max(start);
                let days = (until.min(period.end) - first).num_days();
                let Some(treaty) = c.treaty.as_ref().and_then(|t| self.treaty(t)) else {
                    continue;
                };
                if days <= 0 {
                    continue;
                }
                let mut add = |reinsurer: &ReinsurerId, kind: ChargeKind, amount: i64| {
                    let k = ChargeKey {
                        risk: key.clone(),
                        treaty: treaty.id.clone(),
                        reinsurer: reinsurer.clone(),
                        kind,
                    };
                    *out.entry(k).or_default() += amount;
                };
                for (reinsurer, ceded) in &c.ceded {
                    let premium = match &treaty.premium {
                        PremiumBasis::RateTable { table } => self
                            .rate_table(table, start)
                            .and_then(|t| t.rate(self.factors.get(&(policy.clone(), key.life.clone())), start))
                            .map(|rate| rated(*ceded, rate, days)),
                        PremiumBasis::OriginalPremium { .. } => {
                            // The ceded proportion of the customer's premium
                            // for this cover type, for the days ceded.
                            let cover_total: i64 = keys
                                .iter()
                                .filter_map(|k| self.risk(k))
                                .filter(|r| r.cover_type == risk.cover_type && r.in_force_on(first))
                                .map(|r| r.amount_on(first))
                                .sum();
                            period
                                .by_cover_type
                                .get(&risk.cover_type)
                                .filter(|_| cover_total > 0 && period_days > 0)
                                .map(|original| {
                                    div_half_even(*original as i128 * *ceded as i128 * days as i128, cover_total as i128 * period_days as i128) as i64
                                })
                        }
                    };
                    // A rate is never guessed: a cover that cannot be priced
                    // is charged nothing and raised for a person.
                    let Some(premium) = premium else {
                        unpriced.push(key.to_string());
                        continue;
                    };
                    add(reinsurer, ChargeKind::Premium, premium);
                    add(reinsurer, ChargeKind::Commission, share_of(premium, treaty.commission));
                }
                for f in &c.facultative {
                    add(&f.reinsurer, ChargeKind::Facultative, rated(f.amount_minor, f.rate, days));
                }
            }
        }
        out.retain(|_, v| *v != 0);
        unpriced.sort();
        unpriced.dedup();
        (out, unpriced)
    }

    /// Bring what is charged for one period into line with the cession
    /// record, posting only the difference.
    pub(crate) fn reprice(&mut self, ctx: &mut Ctx, policy: &PolicyId, start: NaiveDate) {
        let at = (policy.clone(), start);
        let Some(period) = self.periods.get(&at) else { return };
        let (now, unpriced) = if period.reversed {
            (BTreeMap::new(), vec![])
        } else {
            self.charges(policy, start, period)
        };
        let partition = self.policies.get(policy).map_or(0, |p| p.partition);
        let mut deltas: Vec<(ChargeKey, i64)> = Vec::new();
        for (key, amount) in &now {
            let delta = amount - period.charged.get(key).copied().unwrap_or(0);
            if delta != 0 {
                deltas.push((key.clone(), delta));
            }
        }
        for (key, amount) in &period.charged {
            if !now.contains_key(key) {
                deltas.push((key.clone(), -amount));
            }
        }
        for (key, delta) in deltas {
            let reference = format!("{}/{start}", key.risk);
            self.book(
                ctx,
                key.kind.line(),
                &key.treaty,
                &key.reinsurer,
                partition,
                Some(policy.clone()),
                start,
                delta,
                reference,
            );
        }
        if let Some(period) = self.periods.get_mut(&at) {
            period.charged = now;
        }
        let detail = (!unpriced.is_empty()).then(|| {
            format!(
                "no reinsurance premium could be worked out for {}: a rate or the customer's premium is missing",
                unpriced.join(", ")
            )
        });
        self.set_exception(ctx, format!("unpriced:{policy}/{start}"), "unpriced_cession", detail);
    }

    /// Reprice every period of a policy that ends after `from`.
    pub(crate) fn reprice_from(&mut self, ctx: &mut Ctx, policy: &PolicyId, from: NaiveDate) {
        let starts: Vec<NaiveDate> = self
            .periods
            .range((policy.clone(), NaiveDate::MIN)..)
            .take_while(|((p, _), _)| p == policy)
            .filter(|(_, period)| period.end > from)
            .map(|((_, start), _)| *start)
            .collect();
        for start in starts {
            self.reprice(ctx, policy, start);
        }
    }

    /// What has been charged for a period, for reconciliation and tests.
    pub fn charged(&self, policy: &PolicyId, start: NaiveDate) -> Option<&BTreeMap<ChargeKey, i64>> {
        self.periods.get(&(policy.clone(), start)).map(|p| &p.charged)
    }
}
