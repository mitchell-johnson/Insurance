//! Earning: each day a day's share of every billed period is released from
//! unearned premium to earned premium income, in fine units.
//!
//! The release is cumulative: after `n` of `d` days, exactly
//! `round_half_even(net * n / d)` has been earned, and the whole amount after
//! the last day. A late run, a backdated adjustment or a reinstatement
//! therefore catches up in one entry, and a period can never leave a residue.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::Ledger;

use crate::accounts::{self, dims};
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, Receivable};
use crate::postings::{div_half_even, PostingSet};

/// What should have been earned on a receivable, by cover type, once every
/// day up to and including `through` has been earned.
pub(crate) fn earned_target(r: &Receivable, through: NaiveDate) -> BTreeMap<String, i128> {
    let total_days = (r.period_end - r.period_start).num_days().max(1) as i128;
    let elapsed = ((through.succ_opt().unwrap_or(through).min(r.period_end)) - r.period_start)
        .num_days()
        .clamp(0, total_days as i64) as i128;
    r.net_fine
        .iter()
        .map(|(k, net)| {
            (
                k.clone(),
                if elapsed == total_days {
                    *net
                } else {
                    div_half_even(net * elapsed, total_days)
                },
            )
        })
        .collect()
}

/// Earn everything due up to and including `through`. One entry per policy.
pub(crate) fn earn_through(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, through: NaiveDate) -> Result<(), Reject> {
    let c = policy.currency;
    let mut set = PostingSet::new();
    let mut updates: Vec<(usize, BTreeMap<String, i128>)> = Vec::new();
    let mut total: i128 = 0;
    for (i, r) in policy.receivables.iter().enumerate() {
        if r.closed || r.period_start > through {
            continue;
        }
        let target = earned_target(r, through);
        let mut changed = false;
        for (cover_type, t) in &target {
            let delta = t - r.earned_fine.get(cover_type).copied().unwrap_or(0);
            if delta != 0 {
                changed = true;
                total += delta;
                set.debit(accounts::unearned_premium(&policy.id, c), delta, dims(policy, None, "premium"));
                set.credit(
                    accounts::earned_premium(&policy.product_code, cover_type, c),
                    delta,
                    dims(policy, Some(cover_type), "premium"),
                );
            }
        }
        if changed {
            updates.push((i, target));
        }
    }
    if updates.is_empty() {
        return Ok(());
    }
    if !set.is_empty() {
        let postings = set.into_postings(ledger)?;
        ctx.post(ledger, "premium_earned", Some(policy), through, postings, BTreeMap::new())?;
    }
    for (i, target) in updates {
        policy.receivables[i].earned_fine = target;
    }
    ctx.emit(Event::PremiumEarned {
        policy: policy.id.clone(),
        through,
        amount_fine: total,
    });
    Ok(())
}
