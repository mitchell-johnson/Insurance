//! Money paid to the wrong person.
//!
//! A payment that settled into an account that was not the payee's has left
//! the bank, so the payable it paid is discharged, yet the right person has
//! not been paid. Raising a payee debt puts both right: the right person is
//! owed again in the payable the payment came from, and whoever holds the
//! money owes it back, in an account of its own on the policy. The debt
//! falls as money is recovered (a bank recall, a repayment) and what will
//! never come back is written off by a person's decision, to its own
//! expense account. Nothing here chases anyone: the payments module does.

use std::collections::BTreeMap;

use ic_ledger::Ledger;
use ic_money::Money;
use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::event::Event;
use crate::interest::plain;
use crate::machine::{Ctx, Reject};
use crate::payments::PayableKind;
use crate::policy::Policy;
use crate::postings::PostingSet;
use crate::{PartyId, PolicyId};

/// What one wrong payee owes, and how it got there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayeeDebt {
    /// The settled payment that went to the wrong account.
    pub payment: String,
    /// Who holds the money, where known. An opaque ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub party: Option<PartyId>,
    pub raised_minor: i64,
    pub outstanding_minor: i64,
    /// Each recovery, by its receipt.
    #[serde(default)]
    pub recoveries: BTreeMap<String, i64>,
    pub written_off_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayeeDebtCommand {
    /// A settled payment, or part of it, went to the wrong account: the
    /// right payee is owed it again and the holder owes it back.
    Raise {
        policy: PolicyId,
        debt: String,
        payment: String,
        amount: Money,
        #[serde(default)]
        party: Option<PartyId>,
    },
    /// Money came back from the holder or the bank.
    Recover {
        policy: PolicyId,
        debt: String,
        receipt: String,
        amount: Money,
    },
    /// What will not be recovered, by a person's decision.
    WriteOff {
        policy: PolicyId,
        debt: String,
        amount: Money,
        reason: String,
    },
}

impl PayeeDebtCommand {
    pub fn policy(&self) -> &PolicyId {
        match self {
            PayeeDebtCommand::Raise { policy, .. } | PayeeDebtCommand::Recover { policy, .. } | PayeeDebtCommand::WriteOff { policy, .. } => policy,
        }
    }
}

fn amount_ok(policy: &Policy, amount: Money) -> Result<(), Reject> {
    if amount.currency != policy.currency || amount.minor <= 0 {
        return Err(Reject::new("invalid_amount", "the amount is positive and in the policy's currency"));
    }
    Ok(())
}

fn post(ledger: &mut Ledger, policy: &Policy, ctx: &mut Ctx<'_>, kind: &str, set: PostingSet, debt: &str, extra: (&str, &str)) -> Result<(), Reject> {
    let detail = BTreeMap::from([("debt".to_string(), debt.to_string()), (extra.0.to_string(), extra.1.to_string())]);
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, kind, Some(policy), ctx.date, postings, detail)
}

/// What the ledger's payee debt account should hold for the policy.
pub(crate) fn expected_balance(policy: &Policy) -> i128 {
    policy.payee_debts.values().map(|d| d.outstanding_minor as i128).sum()
}

pub(crate) fn apply(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, command: PayeeDebtCommand) -> Result<(), Reject> {
    match command {
        PayeeDebtCommand::Raise {
            debt, payment, amount, party, ..
        } => raise(ledger, policy, ctx, debt, payment, amount, party),
        PayeeDebtCommand::Recover { debt, receipt, amount, .. } => recover(ledger, policy, ctx, debt, receipt, amount),
        PayeeDebtCommand::WriteOff { debt, amount, reason, .. } => write_off(ledger, policy, ctx, debt, amount, reason),
    }
}

fn raise(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    debt: String,
    payment: String,
    amount: Money,
    party: Option<PartyId>,
) -> Result<(), Reject> {
    amount_ok(policy, amount)?;
    if !plain(&debt) || party.as_ref().is_some_and(|p| !plain(&p.0)) {
        return Err(Reject::new("invalid_debt", "a debt has a plain ID, and a plain party ID where it names one"));
    }
    if let Some(known) = policy.payee_debts.get(&debt) {
        return if known.payment == payment && known.raised_minor == amount.minor && known.party == party {
            Ok(())
        } else {
            Err(Reject::new("duplicate_debt", format!("debt {debt} was raised with other details")))
        };
    }
    let record = policy
        .payments
        .get(&payment)
        .filter(|r| !r.returned)
        .ok_or_else(|| Reject::new("unknown_payment", format!("payment {payment} is not a settled payment on this policy")))?
        .clone();
    let already: i64 = policy.payee_debts.values().filter(|d| d.payment == payment).map(|d| d.raised_minor).sum();
    if amount.minor > record.amount_minor - already {
        return Err(Reject::new(
            "over_payment",
            format!("payment {payment} was for {}, of which {already} is already a debt", record.amount_minor),
        ));
    }
    let c = policy.currency;
    let payable = match record.payable {
        PayableKind::Benefit => accounts::benefit_payable(&policy.id, c),
        PayableKind::Refund => accounts::refund_payable(&policy.id, c),
    };
    let dims = accounts::dims(policy, None, "payee_debt_raised");
    let mut set = PostingSet::new();
    set.debit(accounts::payee_debt(&policy.id, c), amount.minor as i128, dims.clone());
    set.credit(payable, amount.minor as i128, dims);
    post(ledger, policy, ctx, "payee_debt_raised", set, &debt, ("payment", &payment))?;
    // The refund the payment paid is owed again, to the right person.
    if let Some(refund) = &record.refund {
        *policy.refunds_raised.entry(refund.clone()).or_insert(0) += amount.minor;
    }
    policy.payee_debts.insert(
        debt.clone(),
        PayeeDebt {
            payment: payment.clone(),
            party: party.clone(),
            raised_minor: amount.minor,
            outstanding_minor: amount.minor,
            recoveries: BTreeMap::new(),
            written_off_minor: 0,
        },
    );
    ctx.emit(Event::PayeeDebtRaised {
        policy: policy.id.clone(),
        debt,
        payment,
        payable: record.payable,
        refund: record.refund,
        party,
        amount_minor: amount.minor,
        outstanding_minor: amount.minor,
    });
    Ok(())
}

fn owed<'a>(policy: &'a mut Policy, debt: &str) -> Result<&'a mut PayeeDebt, Reject> {
    policy
        .payee_debts
        .get_mut(debt)
        .ok_or_else(|| Reject::new("unknown_debt", format!("no debt {debt} on this policy")))
}

fn recover(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, debt: String, receipt: String, amount: Money) -> Result<(), Reject> {
    amount_ok(policy, amount)?;
    if !plain(&receipt) {
        return Err(Reject::new("invalid_debt", "a recovery has a plain receipt"));
    }
    if policy.receipts.contains_key(&receipt) {
        return Err(Reject::new(
            "duplicate_receipt",
            format!("receipt {receipt} has already been applied to premium"),
        ));
    }
    let record = owed(policy, &debt)?;
    if let Some(existing) = record.recoveries.get(&receipt) {
        return if *existing == amount.minor {
            Ok(())
        } else {
            Err(Reject::new("duplicate_receipt", format!("receipt {receipt} was applied with another amount")))
        };
    }
    if amount.minor > record.outstanding_minor {
        return Err(Reject::new(
            "over_recovery",
            format!("{} is outstanding on debt {debt}", record.outstanding_minor),
        ));
    }
    record.outstanding_minor -= amount.minor;
    record.recoveries.insert(receipt.clone(), amount.minor);
    let outstanding_minor = record.outstanding_minor;
    let c = policy.currency;
    let dims = accounts::dims(policy, None, "payee_debt_recovered");
    let mut set = PostingSet::new();
    set.debit(accounts::settlement(c), amount.minor as i128, dims.clone());
    set.credit(accounts::payee_debt(&policy.id, c), amount.minor as i128, dims);
    post(ledger, policy, ctx, "payee_debt_recovered", set, &debt, ("receipt", &receipt))?;
    ctx.emit(Event::PayeeDebtRecovered {
        policy: policy.id.clone(),
        debt,
        receipt,
        amount_minor: amount.minor,
        outstanding_minor,
    });
    Ok(())
}

fn write_off(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, debt: String, amount: Money, reason: String) -> Result<(), Reject> {
    amount_ok(policy, amount)?;
    if !plain(&reason) {
        return Err(Reject::new("invalid_debt", "a write-off gives a plain reason code"));
    }
    let record = owed(policy, &debt)?;
    if amount.minor > record.outstanding_minor {
        return Err(Reject::new(
            "over_recovery",
            format!("{} is outstanding on debt {debt}", record.outstanding_minor),
        ));
    }
    record.outstanding_minor -= amount.minor;
    record.written_off_minor += amount.minor;
    let outstanding_minor = record.outstanding_minor;
    let c = policy.currency;
    let dims = accounts::dims(policy, None, "payee_debt_written_off");
    let mut set = PostingSet::new();
    set.debit(accounts::payee_debt_written_off(&policy.product_code, c), amount.minor as i128, dims.clone());
    set.credit(accounts::payee_debt(&policy.id, c), amount.minor as i128, dims);
    post(ledger, policy, ctx, "payee_debt_written_off", set, &debt, ("reason", &reason))?;
    ctx.emit(Event::PayeeDebtWrittenOff {
        policy: policy.id.clone(),
        debt,
        amount_minor: amount.minor,
        outstanding_minor,
        reason,
    });
    Ok(())
}
