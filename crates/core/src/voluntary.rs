//! Cover a scheme member pays for themselves.
//!
//! A member-paid layer, or the loading on a layer where the member pays only
//! that, is billed to the member and not to the employer: it has its own
//! receivables on the scheme policy, marked with the member as payer, and
//! the member's own standing. Money the member pays is set only against what
//! the member owes, and what is left over is held as the member's credit in
//! an account of its own. Nothing of it reaches the scheme invoice, the
//! scheme's balance or the scheme's standing.

use std::collections::{BTreeMap, BTreeSet};

use ic_ledger::{Account, Dimensions, Ledger};
use ic_money::{AccrualAmount, Money};
use ic_product::ProductDefinition;

use crate::accounts::{self, dims};
use crate::arrears;
use crate::cash::{allocate, apply_allocations};
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus, ReceiptRecord};
use crate::postings::PostingSet;
use crate::LifeId;

/// The account holding a member's own money, noted on the policy so that a
/// transfer knows it exists.
fn credit_account(policy: &mut Policy, life: &LifeId) -> Account {
    policy.member_accounts.insert(life.clone());
    accounts::member_suspense(&policy.id, life, policy.currency)
}

/// A member's credit balance.
pub fn credit_minor(ledger: &Ledger, policy: &Policy, life: &LifeId) -> i128 {
    ledger.available(&accounts::member_suspense(&policy.id, life, policy.currency).id)
}

/// Whether any member has money of their own held on the policy.
pub(crate) fn holds_credit(ledger: &Ledger, policy: &Policy) -> bool {
    policy.member_accounts.iter().any(|life| credit_minor(ledger, policy, life) != 0)
}

fn member(policy: &Policy, life: &LifeId) -> Result<(), Reject> {
    if policy.lives.contains_key(life) {
        Ok(())
    } else {
        Err(Reject::new("unknown_member", format!("{life} is not a member of this scheme")))
    }
}

/// Money from a member for cover the member pays for themselves.
pub(crate) fn apply_receipt(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, life: LifeId, receipt: String, amount: Money) -> Result<(), Reject> {
    member(policy, &life)?;
    if amount.currency != policy.currency || amount.minor <= 0 {
        return Err(Reject::new("invalid_amount", "a receipt must be a positive amount in the policy's currency"));
    }
    if policy.receipts.contains_key(&receipt) {
        return Err(Reject::new("duplicate_receipt", format!("receipt {receipt} has already been applied")));
    }
    let c = policy.currency;
    let allocations = if policy.status == PolicyStatus::InForce {
        allocate(policy, amount.minor, Some(&life))
    } else {
        BTreeMap::new()
    };
    let allocated: i64 = allocations.values().sum();
    let to_credit = amount.minor - allocated;
    let mut set = PostingSet::new();
    set.debit(accounts::settlement(c), amount.minor as i128, Dimensions::default());
    set.credit(accounts::premium_receivable(&policy.id, c), allocated as i128, Dimensions::default());
    if to_credit > 0 {
        set.credit(credit_account(policy, &life), to_credit as i128, Dimensions::default());
    }
    let mut detail = BTreeMap::new();
    detail.insert("receipt".to_string(), receipt.clone());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "member_premium_received", Some(policy), ctx.date, postings, detail)?;
    let net_by_cover_type = crate::counterparty::net_by_cover_type(&policy.receivables, &allocations);
    apply_allocations(policy, &allocations, 1);
    policy.receipts.insert(
        receipt.clone(),
        ReceiptRecord {
            received_on: ctx.date,
            amount_minor: amount.minor,
            allocations,
            to_suspense_minor: to_credit,
            reversed: false,
            member: Some(life.clone()),
        },
    );
    ctx.emit(Event::MemberReceiptApplied {
        policy: policy.id.clone(),
        life,
        receipt,
        allocated_minor: allocated,
        to_credit_minor: to_credit,
        net_by_cover_type,
    });
    Ok(())
}

/// Set a member's credit against what the member owes, oldest first.
pub(crate) fn apply_credit(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce || !policy.member_accounts.contains(life) {
        return Ok(());
    }
    let balance = credit_minor(ledger, policy, life);
    if balance <= 0 {
        return Ok(());
    }
    let allocations = allocate(policy, i64::try_from(balance).unwrap_or(i64::MAX), Some(life));
    let allocated: i64 = allocations.values().sum();
    if allocated == 0 {
        return Ok(());
    }
    let c = policy.currency;
    let mut set = PostingSet::new();
    set.debit(accounts::member_suspense(&policy.id, life, c), allocated as i128, Dimensions::default());
    set.credit(accounts::premium_receivable(&policy.id, c), allocated as i128, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "member_credit_applied", Some(policy), ctx.date, postings, BTreeMap::new())?;
    let net_by_cover_type = crate::counterparty::net_by_cover_type(&policy.receivables, &allocations);
    apply_allocations(policy, &allocations, 1);
    ctx.emit(Event::MemberCreditApplied {
        policy: policy.id.clone(),
        life: life.clone(),
        amount_minor: allocated,
        net_by_cover_type,
    });
    Ok(())
}

/// Pay a member's credit back to the member.
pub(crate) fn refund_credit(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, life: LifeId, amount: Money) -> Result<(), Reject> {
    member(policy, &life)?;
    let c = policy.currency;
    if amount.currency != c || amount.minor <= 0 {
        return Err(Reject::new("invalid_amount", "a refund must be a positive amount in the policy's currency"));
    }
    if credit_minor(ledger, policy, &life) < amount.minor as i128 {
        return Err(Reject::new("insufficient_credit", "the member does not hold that much in credit"));
    }
    let mut set = PostingSet::new();
    set.debit(accounts::member_suspense(&policy.id, &life, c), amount.minor as i128, Dimensions::default());
    set.credit(accounts::settlement(c), amount.minor as i128, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "member_credit_refunded", Some(policy), ctx.date, postings, BTreeMap::new())?;
    ctx.emit(Event::MemberCreditRefunded {
        policy: policy.id.clone(),
        life,
        amount_minor: amount.minor,
    });
    Ok(())
}

/// Close what one member owes, as a lapse closes a policy's receivables:
/// what is unpaid is written off, and premium the member paid for days not
/// yet covered goes back to the member's credit. Returns what was owed.
pub(crate) fn close(ledger: &mut Ledger, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId) -> Result<i64, Reject> {
    let c = policy.currency;
    let theirs = |r: &crate::policy::Receivable| r.payer.as_ref() == Some(life);
    if !policy.receivables.iter().any(|r| !r.closed && theirs(r)) {
        return Ok(0);
    }
    let mut set = PostingSet::new();
    let (owed, leftover_fine) = arrears::write_off(policy, &mut set, theirs);
    let (refund, rem) = AccrualAmount::new(leftover_fine, c).round_to_money(product.rounding)?;
    if refund.minor != 0 {
        set.credit(credit_account(policy, life), refund.minor as i128, Dimensions::default());
    }
    set.credit(accounts::rounding(c), rem.fine, dims(policy, None, "rounding"));
    let postings = set.into_postings(ledger)?;
    if !postings.is_empty() {
        ctx.post(ledger, "member_cover_closed", Some(policy), ctx.date, postings, BTreeMap::new())?;
    }
    for r in policy.receivables.iter_mut().filter(|r| !r.closed && r.payer.as_ref() == Some(life)) {
        r.outstanding_minor = 0;
        r.closed = true;
    }
    Ok(owed)
}

/// Close what every member owes, before the scheme itself lapses or ends:
/// the scheme's close-out then deals only with the employer's money.
pub(crate) fn close_all(ledger: &mut Ledger, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    let payers: BTreeSet<LifeId> = policy.receivables.iter().filter(|r| !r.closed).filter_map(|r| r.payer.clone()).collect();
    for life in payers {
        close(ledger, product, policy, ctx, &life)?;
    }
    Ok(())
}
