//! Billing: on the billing date the period's premium is posted to receivable
//! and unearned premium. It happens whether or not anyone pays.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{Dimensions, Ledger};
use ic_money::FINE_PER_MINOR;
use ic_product::rating::ModalPremium;
use ic_product::schedule::period_start;
use ic_product::ProductDefinition;

use crate::accounts::{self, dims};
use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus, Receivable, ReceivableKind};
use crate::postings::{div_half_even, scale_map, PostingSet};
use crate::premium::retail_period_premium;
use crate::{cash, group};

/// A bill in signed minor units: negative for a credit adjustment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Bill {
    /// Earned over the period, by cover type and `policy_fee`.
    pub net: BTreeMap<String, i64>,
    pub gst: i64,
    pub levies: BTreeMap<String, i64>,
}

impl Bill {
    pub fn from_modal(policy: &Policy, m: &ModalPremium) -> Bill {
        let mut net: BTreeMap<String, i64> = BTreeMap::new();
        for (code, amount) in &m.by_cover {
            let cover_type = policy.covers.get(code).map_or(code.as_str(), |c| c.cover_type.as_str());
            *net.entry(cover_type.to_string()).or_insert(0) += amount.minor;
        }
        if m.policy_fee.minor != 0 {
            net.insert("policy_fee".to_string(), m.policy_fee.minor);
        }
        Bill {
            net,
            gst: m.gst.values().map(|g| g.minor).sum(),
            levies: m.levies.iter().map(|(k, v)| (k.clone(), v.minor)).collect(),
        }
    }
    /// The bill scaled by `num / den`, each line rounded half-even.
    pub fn scaled(&self, num: i128, den: i128) -> Bill {
        Bill {
            net: scale_map(&self.net, num, den),
            gst: div_half_even(self.gst as i128 * num, den) as i64,
            levies: scale_map(&self.levies, num, den),
        }
    }
    pub fn minus(&self, other: &Bill) -> Bill {
        let mut out = self.clone();
        for (k, v) in &other.net {
            *out.net.entry(k.clone()).or_insert(0) -= v;
        }
        for (k, v) in &other.levies {
            *out.levies.entry(k.clone()).or_insert(0) -= v;
        }
        out.gst -= other.gst;
        out
    }
    pub fn total(&self) -> i64 {
        self.net.values().sum::<i64>() + self.gst + self.levies.values().sum::<i64>()
    }
    pub fn is_zero(&self) -> bool {
        self.gst == 0 && self.net.values().all(|v| *v == 0) && self.levies.values().all(|v| *v == 0)
    }
}

/// Post one receivable: debit premium receivable, credit unearned premium,
/// with tax and levies as separate lines in the same entry.
#[allow(clippy::too_many_arguments)]
pub(crate) fn raise_receivable(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    bill: &Bill,
    kind: ReceivableKind,
    period: (NaiveDate, NaiveDate),
    due: NaiveDate,
    entry_kind: &str,
) -> Result<Option<u32>, Reject> {
    if bill.is_zero() {
        return Ok(None);
    }
    let c = policy.currency;
    let mut set = PostingSet::new();
    set.debit(accounts::premium_receivable(&policy.id, c), bill.total() as i128, dims(policy, None, "premium"));
    for (cover_type, amount) in &bill.net {
        set.credit(
            accounts::unearned_premium(&policy.id, c),
            *amount as i128 * FINE_PER_MINOR,
            dims(policy, Some(cover_type), "premium"),
        );
    }
    set.credit(accounts::gst_payable(c), bill.gst as i128, dims(policy, None, "gst"));
    for (code, amount) in &bill.levies {
        set.credit(accounts::levy_payable(code, c), *amount as i128, dims(policy, None, "levy"));
    }
    let id = policy.next_receivable_id;
    let mut detail = BTreeMap::new();
    detail.insert("receivable".to_string(), id.to_string());
    detail.insert("period_start".to_string(), period.0.to_string());
    detail.insert("period_end".to_string(), period.1.to_string());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, entry_kind, Some(policy), period.0.min(ctx.date), postings, detail)?;
    policy.next_receivable_id += 1;
    policy.receivables.push(Receivable {
        id,
        kind,
        period_start: period.0,
        period_end: period.1,
        due,
        total_minor: bill.total(),
        outstanding_minor: bill.total(),
        gst_minor: bill.gst,
        levies_minor: bill.levies.clone(),
        net_fine: bill.net.iter().map(|(k, v)| (k.clone(), *v as i128 * FINE_PER_MINOR)).collect(),
        earned_fine: BTreeMap::new(),
        closed: false,
        entry: ctx.entries.last().map(|e| e.id),
    });
    settle_credits(ledger, policy, ctx)?;
    Ok(Some(id))
}

/// A credit adjustment reduces what is owed on the oldest open receivables;
/// anything left over becomes a credit balance in policy suspense.
fn settle_credits(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    let mut credit: i64 = -policy
        .receivables
        .iter()
        .filter(|r| r.outstanding_minor < 0)
        .map(|r| r.outstanding_minor)
        .sum::<i64>();
    if credit == 0 {
        return Ok(());
    }
    let owed: i64 = policy.receivables.iter().filter(|r| r.outstanding_minor > 0).map(|r| r.outstanding_minor).sum();
    let to_suspense = (credit - owed).max(0);
    if to_suspense > 0 {
        let c = policy.currency;
        let mut set = PostingSet::new();
        set.debit(accounts::premium_receivable(&policy.id, c), to_suspense as i128, Dimensions::default());
        set.credit(accounts::policy_suspense(&policy.id, c), to_suspense as i128, Dimensions::default());
        let postings = set.into_postings(ledger)?;
        ctx.post(ledger, "premium_credit_to_suspense", Some(policy), ctx.date, postings, BTreeMap::new())?;
    }
    for r in policy.receivables.iter_mut().filter(|r| r.outstanding_minor < 0) {
        r.outstanding_minor = 0;
    }
    credit -= to_suspense;
    let mut open: Vec<&mut Receivable> = policy.receivables.iter_mut().filter(|r| r.outstanding_minor > 0).collect();
    open.sort_by_key(|r| (r.due, r.id));
    for r in open {
        let take = credit.min(r.outstanding_minor);
        r.outstanding_minor -= take;
        credit -= take;
        if credit == 0 {
            break;
        }
    }
    Ok(())
}

/// Bill every period whose first day has arrived. Premium is in advance: a
/// period is due on its first day. Returns false if a scheme invoice is still
/// being built and another step is needed.
pub(crate) fn bill_due_periods(
    ledger: &mut Ledger,
    catalog: &Catalog,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
) -> Result<bool, Reject> {
    let scheme = policy.covers.values().any(|c| c.formula_driven);
    // Bounded per command; a policy catching up after a transfer or an outage
    // takes several end-of-day steps.
    for _ in 0..3 {
        if policy.status != PolicyStatus::InForce {
            break;
        }
        let n = policy.periods_billed;
        let (Some(start), Some(end)) = (
            period_start(policy.inception, policy.frequency, n),
            period_start(policy.inception, policy.frequency, n + 1),
        ) else {
            return Err(Reject::new("date", "billing period out of range"));
        };
        if start > ctx.date {
            break;
        }
        if scheme {
            if !group::bill_scheme_step(ledger, catalog, product, policy, ctx, (start, end))? {
                return Ok(false);
            }
        } else {
            let premium = retail_period_premium(catalog, product, policy, start, &mut ctx.work)?;
            let bill = Bill::from_modal(policy, &premium);
            let id = raise_receivable(ledger, policy, ctx, &bill, ReceivableKind::Period, (start, end), start, "premium_due")?;
            policy.periods_billed += 1;
            if let Some(id) = id {
                ctx.emit(Event::PremiumDue {
                    policy: policy.id.clone(),
                    receivable: id,
                    period_start: start,
                    period_end: end,
                    total_minor: bill.total(),
                    members: None,
                    by_cover_type: bill
                        .net
                        .iter()
                        .filter(|(k, _)| k.as_str() != "policy_fee")
                        .map(|(k, v)| (k.clone(), *v))
                        .collect(),
                });
            }
        }
        // Money paid ahead is consumed by the new billing.
        cash::apply_suspense(ledger, policy, ctx)?;
    }
    Ok(true)
}
