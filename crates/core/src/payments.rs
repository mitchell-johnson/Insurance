//! The payments module's commands: money leaving through a payable.
//!
//! An outbound payment is owed before it is paid. The amount sits in a
//! payable account on the policy (benefit payable for a claim, surrender or
//! maturity; refund payable for premium going back) until the bank confirms
//! settlement, and only then moves to the ledger account that mirrors the
//! bank account it was paid from. A payment the bank sends back reverses to
//! the payable it came from, never to suspense, so what is owed and unpaid is
//! always one balance.

use std::collections::BTreeMap;

use ic_ledger::Ledger;
use ic_money::Money;
use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{PaymentRecord, Policy};
use crate::postings::PostingSet;

/// The payable a payment is made from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayableKind {
    /// Admitted claims, surrenders and maturities awaiting payment.
    Benefit,
    /// Premium on its way back to the payer, or with a claim to the estate.
    Refund,
}

impl PayableKind {
    fn account(self, policy: &Policy) -> ic_ledger::Account {
        match self {
            PayableKind::Benefit => accounts::benefit_payable(&policy.id, policy.currency),
            PayableKind::Refund => accounts::refund_payable(&policy.id, policy.currency),
        }
    }
    fn posting_type(self) -> &'static str {
        match self {
            PayableKind::Benefit => "benefit_payment",
            PayableKind::Refund => "premium_refund",
        }
    }
}

fn positive_in_policy_currency(policy: &Policy, amount: Money, what: &str) -> Result<(), Reject> {
    if amount.currency != policy.currency || amount.minor <= 0 {
        return Err(Reject::new(
            "invalid_amount",
            format!("{what} must be a positive amount in the policy's currency"),
        ));
    }
    Ok(())
}

/// Bank account codes become part of a ledger account ID.
fn valid_bank_account(code: &str) -> Result<(), Reject> {
    let ok = !code.is_empty() && code.len() <= 64 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(Reject::new(
            "invalid_bank_account",
            "a bank account code is 1 to 64 letters, digits, hyphens or underscores",
        ))
    }
}

/// Move a credit balance from suspense to refund payable: the refund is now
/// owed and waits for a payment run.
pub(crate) fn raise_refund_payable(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    refund: String,
    amount: Money,
    with_claim: Option<String>,
) -> Result<(), Reject> {
    // The same rule as an immediate refund: back to where the money came
    // from, unless the payer is the person a claim is for.
    crate::cash::check_refund_route(policy, with_claim.as_ref())?;
    positive_in_policy_currency(policy, amount, "a refund")?;
    if policy.refunds_raised.contains_key(&refund) || policy.payments.values().any(|p| p.refund.as_ref() == Some(&refund)) {
        return Err(Reject::new("duplicate_refund", format!("refund {refund} has already been raised")));
    }
    let c = policy.currency;
    if ledger.available(&accounts::policy_suspense(&policy.id, c).id) < amount.minor as i128 {
        return Err(Reject::new("insufficient_suspense", "the policy does not hold that much in suspense"));
    }
    let dims = accounts::dims(policy, None, PayableKind::Refund.posting_type());
    let mut set = PostingSet::new();
    set.debit(accounts::policy_suspense(&policy.id, c), amount.minor as i128, dims.clone());
    set.credit(accounts::refund_payable(&policy.id, c), amount.minor as i128, dims);
    let mut detail = BTreeMap::new();
    detail.insert("refund".to_string(), refund.clone());
    if let Some(claim) = &with_claim {
        detail.insert("with_claim".to_string(), claim.clone());
    }
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "refund_payable_raised", Some(policy), ctx.date, postings, detail)?;
    policy.refunds_raised.insert(refund.clone(), amount.minor);
    ctx.emit(Event::RefundPayableRaised {
        policy: policy.id.clone(),
        refund,
        amount_minor: amount.minor,
        with_claim,
    });
    Ok(())
}

/// A refund that will not be paid as raised (the route changed, or the
/// customer chose to leave the credit on the policy) goes back to suspense.
pub(crate) fn release_refund_payable(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, refund: &str) -> Result<(), Reject> {
    // What is still owed on it: a refund already paid has nothing to release
    // unless the bank sends the payment back.
    let amount = *policy
        .refunds_raised
        .get(refund)
        .ok_or_else(|| Reject::new("unknown_refund", format!("refund {refund} is not owed on this policy")))?;
    let c = policy.currency;
    let dims = accounts::dims(policy, None, PayableKind::Refund.posting_type());
    let mut set = PostingSet::new();
    set.debit(accounts::refund_payable(&policy.id, c), amount as i128, dims.clone());
    set.credit(accounts::policy_suspense(&policy.id, c), amount as i128, dims);
    let mut detail = BTreeMap::new();
    detail.insert("refund".to_string(), refund.to_string());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "refund_payable_released", Some(policy), ctx.date, postings, detail)?;
    policy.refunds_raised.remove(refund);
    ctx.emit(Event::RefundPayableReleased {
        policy: policy.id.clone(),
        refund: refund.to_string(),
        amount_minor: amount,
    });
    Ok(())
}

/// The bank has confirmed settlement: the payable is discharged from the
/// ledger account that mirrors the bank account the money left.
pub(crate) fn settle_payable(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    payment: String,
    payable: PayableKind,
    refund: Option<String>,
    amount: Money,
    bank_account: String,
) -> Result<(), Reject> {
    positive_in_policy_currency(policy, amount, "a payment")?;
    valid_bank_account(&bank_account)?;
    if policy.payments.contains_key(&payment) {
        return Err(Reject::new("duplicate_payment", format!("payment {payment} has already been settled")));
    }
    // A refund payment names the refund it pays, so what is still owed on
    // each refund is always known.
    match (payable, &refund) {
        (PayableKind::Refund, Some(id)) => {
            let owed = policy.refunds_raised.get(id).copied().unwrap_or(0);
            if owed < amount.minor {
                return Err(Reject::new("insufficient_payable", format!("refund {id} does not owe that much")));
            }
        }
        (PayableKind::Refund, None) => return Err(Reject::new("refund_required", "a refund payment names the refund it pays")),
        (PayableKind::Benefit, Some(_)) => return Err(Reject::new("refund_not_expected", "a benefit payment does not name a refund")),
        (PayableKind::Benefit, None) => {}
    }
    let c = policy.currency;
    let from = payable.account(policy);
    if ledger.available(&from.id) < amount.minor as i128 {
        return Err(Reject::new(
            "insufficient_payable",
            format!("the policy does not owe that much in {}", from.role),
        ));
    }
    let dims = accounts::dims(policy, None, payable.posting_type());
    let mut set = PostingSet::new();
    set.debit(from, amount.minor as i128, dims.clone());
    set.credit(accounts::bank(&bank_account, c), amount.minor as i128, dims);
    let mut detail = BTreeMap::new();
    detail.insert("payment".to_string(), payment.clone());
    detail.insert("bank_account".to_string(), bank_account.clone());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "payment_settled", Some(policy), ctx.date, postings, detail)?;
    // `post` pushed the entry it made; its ID is what a return reverses.
    let entry = ctx.entries.last().map(|e| e.id);
    if let Some(id) = &refund {
        let fully_paid = policy.refunds_raised.get_mut(id).is_some_and(|owed| {
            *owed -= amount.minor;
            *owed == 0
        });
        if fully_paid {
            policy.refunds_raised.remove(id);
        }
    }
    policy.payments.insert(
        payment.clone(),
        PaymentRecord {
            payable,
            refund,
            amount_minor: amount.minor,
            bank_account: bank_account.clone(),
            settled_on: ctx.date,
            entry,
            returned: false,
        },
    );
    ctx.emit(Event::PaymentSettled {
        policy: policy.id.clone(),
        payment,
        payable,
        amount_minor: amount.minor,
        bank_account,
    });
    Ok(())
}

/// The bank sent a settled payment back (a closed account, a wrong number).
/// The settlement is reversed by a new entry with the original value date
/// that names the entry it reverses, and the money is owed again.
pub(crate) fn return_payment(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, payment: &str) -> Result<(), Reject> {
    let rec = policy
        .payments
        .get(payment)
        .ok_or_else(|| Reject::new("unknown_payment", format!("payment {payment} has not been settled on this policy")))?
        .clone();
    if rec.returned {
        return Err(Reject::new("already_returned", format!("payment {payment} has already been returned")));
    }
    // Money a wrong payee owes back is recovered against the debt, not by
    // reversing the settlement a second time.
    if policy.payee_debts.values().any(|d| d.payment == payment) {
        return Err(Reject::new("payment_has_debt", format!("payment {payment} has a payee debt raised against it")));
    }
    let c = policy.currency;
    let dims = accounts::dims(policy, None, rec.payable.posting_type());
    let mut set = PostingSet::new();
    set.debit(accounts::bank(&rec.bank_account, c), rec.amount_minor as i128, dims.clone());
    set.credit(rec.payable.account(policy), rec.amount_minor as i128, dims);
    let mut detail = BTreeMap::new();
    detail.insert("payment".to_string(), payment.to_string());
    detail.insert("bank_account".to_string(), rec.bank_account.clone());
    if let Some(entry) = rec.entry {
        detail.insert("reverses".to_string(), entry.to_string());
    }
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "payment_returned", Some(policy), rec.settled_on, postings, detail)?;
    if let Some(r) = policy.payments.get_mut(payment) {
        r.returned = true;
    }
    if let Some(id) = &rec.refund {
        *policy.refunds_raised.entry(id.clone()).or_insert(0) += rec.amount_minor;
    }
    ctx.emit(Event::PaymentReturned {
        policy: policy.id.clone(),
        payment: payment.to_string(),
        payable: rec.payable,
        amount_minor: rec.amount_minor,
        bank_account: rec.bank_account,
    });
    Ok(())
}
