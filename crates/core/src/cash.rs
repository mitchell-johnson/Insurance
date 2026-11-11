//! Applying money to what is owed. The cash matching service decides which
//! policy a receipt belongs to; the core allocates it across that policy's
//! receivables and records exactly what it did, so a dishonour can reverse it.

use std::collections::BTreeMap;

use ic_ledger::{Dimensions, Ledger};
use ic_money::Money;

use crate::accounts;
use crate::arrears;
use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus, ReceiptRecord};
use crate::postings::PostingSet;

/// Allocation order on a single policy: oldest receivable first.
fn allocate(policy: &Policy, mut amount: i64) -> BTreeMap<u32, i64> {
    let mut open: Vec<_> = policy.receivables.iter().filter(|r| r.outstanding_minor > 0 && !r.closed).collect();
    open.sort_by_key(|r| (r.due, r.id));
    let mut out = BTreeMap::new();
    for r in open {
        if amount == 0 {
            break;
        }
        let take = amount.min(r.outstanding_minor);
        out.insert(r.id, take);
        amount -= take;
    }
    out
}

fn apply_allocations(policy: &mut Policy, allocations: &BTreeMap<u32, i64>, sign: i64) {
    for r in &mut policy.receivables {
        if let Some(a) = allocations.get(&r.id) {
            r.outstanding_minor -= sign * a;
        }
    }
}

fn refresh_standing(catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>) {
    if let Some(product) = catalog.product(&policy.product_hash) {
        arrears::refresh(product, policy, ctx);
    }
}

pub(crate) fn apply_receipt(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    receipt: String,
    amount: Money,
) -> Result<(), Reject> {
    if amount.currency != policy.currency || amount.minor <= 0 {
        return Err(Reject::new("invalid_amount", "a receipt must be a positive amount in the policy's currency"));
    }
    if policy.receipts.contains_key(&receipt) {
        return Err(Reject::new("duplicate_receipt", format!("receipt {receipt} has already been applied")));
    }
    if matches!(policy.status, PolicyStatus::Cancelled | PolicyStatus::Expired | PolicyStatus::Terminated) && policy.outstanding_minor() == 0 {
        return Err(Reject::new("policy_ended", "the policy has ended; the money stays with cash matching"));
    }
    let c = policy.currency;
    let allocations = if policy.status == PolicyStatus::InForce {
        allocate(policy, amount.minor)
    } else {
        BTreeMap::new()
    };
    let allocated: i64 = allocations.values().sum();
    let to_suspense = amount.minor - allocated;
    let mut set = PostingSet::new();
    set.debit(accounts::settlement(c), amount.minor as i128, Dimensions::default());
    set.credit(accounts::premium_receivable(&policy.id, c), allocated as i128, Dimensions::default());
    set.credit(accounts::policy_suspense(&policy.id, c), to_suspense as i128, Dimensions::default());
    let mut detail = BTreeMap::new();
    detail.insert("receipt".to_string(), receipt.clone());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "premium_received", Some(policy), ctx.date, postings, detail)?;
    let net_by_cover_type = crate::counterparty::net_by_cover_type(&policy.receivables, &allocations);
    apply_allocations(policy, &allocations, 1);
    policy.receipts.insert(
        receipt.clone(),
        ReceiptRecord {
            received_on: ctx.date,
            amount_minor: amount.minor,
            allocations,
            to_suspense_minor: to_suspense,
            reversed: false,
        },
    );
    ctx.emit(Event::ReceiptApplied {
        policy: policy.id.clone(),
        receipt,
        allocated_minor: allocated,
        to_suspense_minor: to_suspense,
        net_by_cover_type,
    });
    refresh_standing(catalog, policy, ctx);
    Ok(())
}

/// A dishonour or chargeback. The allocation is reversed with a new entry and
/// each receivable goes back to owing from its original due date, so a
/// bounced payment does not reset the lapse clock.
pub(crate) fn reverse_receipt(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, receipt: &str) -> Result<(), Reject> {
    let rec = policy
        .receipts
        .get(receipt)
        .ok_or_else(|| Reject::new("unknown_receipt", format!("receipt {receipt} is not on this policy")))?
        .clone();
    if rec.reversed {
        return Err(Reject::new("already_reversed", format!("receipt {receipt} has already been reversed")));
    }
    let c = policy.currency;
    // Money that went to suspense may since have been consumed by later
    // billings or refunded. If it is no longer there, a person decides.
    if ledger.natural_balance(&accounts::policy_suspense(&policy.id, c).id) < rec.to_suspense_minor as i128 {
        return Err(Reject::new(
            "suspense_consumed",
            "part of this receipt has since been applied or refunded; raise an exception item",
        ));
    }
    if rec.allocations.keys().any(|id| !policy.receivables.iter().any(|r| r.id == *id && !r.closed)) {
        return Err(Reject::new(
            "receivable_closed",
            "a receivable this receipt paid has since been closed; raise an exception item",
        ));
    }
    let allocated: i64 = rec.allocations.values().sum();
    let mut set = PostingSet::new();
    set.credit(accounts::settlement(c), rec.amount_minor as i128, Dimensions::default());
    set.debit(accounts::premium_receivable(&policy.id, c), allocated as i128, Dimensions::default());
    set.debit(accounts::policy_suspense(&policy.id, c), rec.to_suspense_minor as i128, Dimensions::default());
    let mut detail = BTreeMap::new();
    detail.insert("receipt".to_string(), receipt.to_string());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "premium_receipt_reversed", Some(policy), ctx.date, postings, detail)?;
    apply_allocations(policy, &rec.allocations, -1);
    if let Some(r) = policy.receipts.get_mut(receipt) {
        r.reversed = true;
    }
    ctx.emit(Event::ReceiptReversed {
        policy: policy.id.clone(),
        receipt: receipt.to_string(),
        amount_minor: rec.amount_minor,
    });
    refresh_standing(catalog, policy, ctx);
    Ok(())
}

/// Consume a credit balance in suspense against open receivables.
pub(crate) fn apply_suspense(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Ok(());
    }
    let c = policy.currency;
    let balance = ledger.available(&accounts::policy_suspense(&policy.id, c).id);
    if balance <= 0 || policy.outstanding_minor() <= 0 {
        return Ok(());
    }
    let allocations = allocate(policy, i64::try_from(balance).unwrap_or(i64::MAX));
    let allocated: i64 = allocations.values().sum();
    if allocated == 0 {
        return Ok(());
    }
    let mut set = PostingSet::new();
    set.debit(accounts::policy_suspense(&policy.id, c), allocated as i128, Dimensions::default());
    set.credit(accounts::premium_receivable(&policy.id, c), allocated as i128, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "suspense_applied", Some(policy), ctx.date, postings, BTreeMap::new())?;
    let net_by_cover_type = crate::counterparty::net_by_cover_type(&policy.receivables, &allocations);
    apply_allocations(policy, &allocations, 1);
    ctx.emit(Event::SuspenseApplied {
        policy: policy.id.clone(),
        amount_minor: allocated,
        net_by_cover_type,
    });
    Ok(())
}

pub(crate) fn refund_suspense(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, amount: Money, with_claim: Option<String>) -> Result<(), Reject> {
    // Refunds go back to where the money came from, unless the payer is the
    // person a claim is for: then they are paid with that claim and only so.
    if policy.refund_with_claim != with_claim {
        return Err(match &policy.refund_with_claim {
            Some(claim) => Reject::new(
                "refund_with_claim",
                format!("the payer is the person claim {claim} is for; this refund is paid with that claim, not to the source account"),
            ),
            None => Reject::new("refund_to_source", "refunds on this policy go back to the source account; no claim takes them"),
        });
    }
    let c = policy.currency;
    if amount.currency != c || amount.minor <= 0 {
        return Err(Reject::new("invalid_amount", "a refund must be a positive amount in the policy's currency"));
    }
    if ledger.available(&accounts::policy_suspense(&policy.id, c).id) < amount.minor as i128 {
        return Err(Reject::new("insufficient_suspense", "the policy does not hold that much in suspense"));
    }
    let mut set = PostingSet::new();
    set.debit(accounts::policy_suspense(&policy.id, c), amount.minor as i128, Dimensions::default());
    set.credit(accounts::settlement(c), amount.minor as i128, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    let mut detail = BTreeMap::new();
    if let Some(claim) = &with_claim {
        detail.insert("with_claim".to_string(), claim.clone());
    }
    ctx.post(ledger, "suspense_refunded", Some(policy), ctx.date, postings, detail)?;
    ctx.emit(Event::SuspenseRefunded {
        policy: policy.id.clone(),
        amount_minor: amount.minor,
        with_claim,
    });
    Ok(())
}
