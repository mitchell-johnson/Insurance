//! Standing, grace, suspension and lapse. Standing is derived from the age of
//! the oldest unpaid receivable; nobody sets it by hand.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{Dimensions, Ledger};
use ic_money::{AccrualAmount, FINE_PER_MINOR};
use ic_product::{ArrearsRules, ProductDefinition};

use crate::accounts::{self, dims};
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{LapseRecord, Policy, PolicyStatus, Receivable, Standing};
use crate::postings::{div_half_even, PostingSet};
use crate::voluntary;

pub(crate) enum Derived {
    Standing(Standing),
    Lapse,
}

/// Pure: the standing a policy's receivables imply on a date.
pub(crate) fn derive(policy: &Policy, rules: ArrearsRules, on: NaiveDate) -> Derived {
    let Some(due) = policy.oldest_unpaid_due() else {
        return Derived::Standing(Standing::Paid);
    };
    let age = (on - due).num_days();
    if age <= 0 {
        Derived::Standing(Standing::Paid)
    } else if age <= rules.grace_days as i64 {
        Derived::Standing(Standing::InGrace)
    } else if age <= rules.lapse_days as i64 {
        Derived::Standing(if rules.suspend_after_grace { Standing::Suspended } else { Standing::InGrace })
    } else {
        Derived::Lapse
    }
}

pub(crate) fn set_standing(policy: &mut Policy, ctx: &mut Ctx<'_>, to: Option<Standing>) {
    if policy.standing != to {
        ctx.emit(Event::StandingChanged {
            policy: policy.id.clone(),
            from: policy.standing,
            to,
            oldest_unpaid_due: policy.oldest_unpaid_due(),
        });
        policy.standing = to;
    }
}

/// Re-derive standing after a payment or a reversal, without lapsing: lapse
/// happens only in the end-of-day arrears run.
pub(crate) fn refresh(product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>) {
    if policy.status != PolicyStatus::InForce {
        return;
    }
    if let Some(rules) = product.arrears() {
        let to = match derive(policy, rules, ctx.date) {
            Derived::Standing(s) => s,
            // Overdue beyond the lapse period: stays as it is until end-of-day.
            Derived::Lapse => policy.standing.unwrap_or(Standing::InGrace),
        };
        set_standing(policy, ctx, Some(to));
    }
}

/// The end-of-day arrears run for one policy.
pub(crate) fn run(ledger: &mut Ledger, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Ok(());
    }
    let Some(rules) = product.arrears() else {
        return Ok(());
    };
    match derive(policy, rules, ctx.date) {
        Derived::Standing(s) => {
            set_standing(policy, ctx, Some(s));
            Ok(())
        }
        // A policy does not lapse before its lapse notice, as last proven
        // delivered, allows it to.
        Derived::Lapse if policy.notices.get("lapse").is_some_and(|n| n.earliest_effect > ctx.date) => Ok(()),
        Derived::Lapse => lapse(ledger, product, policy, ctx),
    }
}

/// The postings that close open receivables as a lapse does: what is owed
/// is written off, against unearned premium first and the rest to the lapse
/// write-off account, with the tax on the unpaid part reversed. Returns what
/// was owed and the premium that was paid but not yet earned, which the
/// caller returns to whoever paid it.
pub(crate) fn write_off(policy: &Policy, set: &mut PostingSet, mine: impl Fn(&Receivable) -> bool) -> (i64, i128) {
    let c = policy.currency;
    let mut arrears: i64 = 0;
    let mut leftover_fine: i128 = 0;
    for r in policy.receivables.iter().filter(|r| !r.closed && mine(r)) {
        let outstanding = r.outstanding_minor.max(0);
        let unearned = r.unearned_fine();
        let (gst_rev, levies_rev): (i64, BTreeMap<String, i64>) = if r.total_minor != 0 {
            (
                div_half_even(r.gst_minor as i128 * outstanding as i128, r.total_minor as i128) as i64,
                r.levies_minor
                    .iter()
                    .map(|(k, v)| (k.clone(), div_half_even(*v as i128 * outstanding as i128, r.total_minor as i128) as i64))
                    .collect(),
            )
        } else {
            (0, BTreeMap::new())
        };
        let net_out_fine = (outstanding - gst_rev - levies_rev.values().sum::<i64>()) as i128 * FINE_PER_MINOR;
        let from_unearned = unearned.clamp(0, net_out_fine.max(0));
        set.debit(accounts::unearned_premium(&policy.id, c), unearned, dims(policy, None, "lapse"));
        set.credit(accounts::premium_receivable(&policy.id, c), outstanding as i128, dims(policy, None, "lapse"));
        set.debit(accounts::gst_payable(c), gst_rev as i128, dims(policy, None, "gst"));
        for (code, v) in &levies_rev {
            set.debit(accounts::levy_payable(code, c), *v as i128, dims(policy, None, "levy"));
        }
        set.debit(
            accounts::lapse_write_off(&policy.product_code, c),
            net_out_fine - from_unearned,
            dims(policy, None, "lapse"),
        );
        leftover_fine += unearned - from_unearned;
        arrears += outstanding;
    }
    (arrears, leftover_fine)
}

/// Lapse: cover ends. Open receivables are written off against unearned
/// premium (reversing the unearned part); earned-but-unpaid premium goes to
/// the lapse write-off account; tax on the unpaid part is reversed; premium
/// that was paid but not yet earned is returned to policy suspense.
fn lapse(ledger: &mut Ledger, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    voluntary::close_all(ledger, product, policy, ctx)?;
    let c = policy.currency;
    let mut set = PostingSet::new();
    let (arrears, leftover_fine) = write_off(policy, &mut set, |_| true);
    let (refund, rem) = AccrualAmount::new(leftover_fine, c).round_to_money(product.rounding)?;
    set.credit(accounts::policy_suspense(&policy.id, c), refund.minor as i128, Dimensions::default());
    set.credit(accounts::rounding(c), rem.fine, dims(policy, None, "rounding"));

    let postings = set.into_postings(ledger)?;
    if !postings.is_empty() {
        ctx.post(ledger, "policy_lapsed", Some(policy), ctx.date, postings.clone(), BTreeMap::new())?;
    }
    let before = policy.receivables.clone();
    for r in policy.receivables.iter_mut().filter(|r| !r.closed) {
        r.outstanding_minor = 0;
        r.closed = true;
    }
    policy.lapse = Some(LapseRecord {
        approval: None,
        lapsed_on: ctx.date,
        postings,
        receivables_before: before,
        arrears_minor: arrears + refund.minor,
    });
    set_standing(policy, ctx, None);
    ctx.emit(Event::PolicyStatusChanged {
        policy: policy.id.clone(),
        from: policy.status,
        to: PolicyStatus::Lapsed,
    });
    policy.status = PolicyStatus::Lapsed;
    Ok(())
}
