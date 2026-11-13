//! Policy loans: the owner borrows against the policy's value.
//!
//! The loan is one ledger account on the policy, a debit balance. Money
//! borrowed is raised to benefit payable and paid like any payout. Interest
//! accrues each day at the rate in force that day and is added to what is
//! owed in whole cents, the fraction carried to the next day; on each
//! policy anniversary the interest accrued becomes principal and bears
//! interest itself. Repayment is at the owner's choice and meets interest
//! first. What is still owed is taken from a surrender or a claim before
//! anything is paid.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::Ledger;
use ic_money::{dec_i128, round_minor_to_fine, Dec, Money, RoundingMode, UnitQty, FINE_PER_MINOR};
use ic_product::schedule::{anniversary, policy_year};
use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::catalog::Catalog;
use crate::event::Event;
use crate::interest::{self, RestrictedAction};
use crate::invest::{InvestEvent, InvestmentBook, Payout, PayoutReason, PayoutState};
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::postings::PostingSet;
use crate::PolicyId;

/// What is owed on a policy's loan, and how it got there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loan {
    /// What was borrowed and not repaid, with interest capitalised so far.
    pub principal_minor: i64,
    /// Interest accrued since the last anniversary and not yet repaid.
    pub interest_minor: i64,
    /// The fraction of a cent of interest not yet posted.
    #[serde(default)]
    pub carry_fine: i128,
    /// The last day interest has been accrued for.
    pub accrued_through: Option<NaiveDate>,
    /// The policy year whose anniversary last capitalised interest.
    #[serde(default)]
    pub capitalised_year: u32,
    /// Each amount borrowed, by its reference.
    #[serde(default)]
    pub draws: BTreeMap<String, i64>,
    /// Each repayment, by its receipt.
    #[serde(default)]
    pub repayments: BTreeMap<String, i64>,
    /// The day the loan was first found to be more than the policy's value,
    /// until it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_value_since: Option<NaiveDate>,
}

impl Loan {
    pub fn outstanding_minor(&self) -> i64 {
        self.principal_minor.saturating_add(self.interest_minor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoanCommand {
    /// Lend the owner an amount against the policy. It is raised to benefit
    /// payable under the reference, which is its payment reference.
    Draw { policy: PolicyId, reference: String, amount_minor: i64 },
    /// Money from the owner towards the loan.
    Repay { policy: PolicyId, receipt: String, amount: Money },
}

impl LoanCommand {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            LoanCommand::Draw { .. } => &["policy-servicing"],
            LoanCommand::Repay { .. } => &["cash-matching"],
        }
    }
    pub fn policy(&self) -> &PolicyId {
        match self {
            LoanCommand::Draw { policy, .. } | LoanCommand::Repay { policy, .. } => policy,
        }
    }
}

fn bad(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

/// What the policy is worth today: its units at the latest price a seller
/// would get, and its cash waiting to be invested or paid that no pending
/// order is counting on. None where a fund it holds has never been priced.
pub(crate) fn value_minor(ledger: &Ledger, book: &InvestmentBook, policy: &Policy, on: NaiveDate) -> Option<i64> {
    let c = policy.currency;
    let mut total = -ledger.balance(&accounts::pending_investment_cash(&policy.id, c).id) - book.committed_cash_of(&policy.id);
    for fund in book.fund_ids() {
        let units = -ledger.balance(&accounts::unit_account(&policy.id, &fund).id);
        if units == 0 {
            continue;
        }
        let price = book.latest_price(&fund, on)?;
        let micro = i64::try_from(units).ok()?;
        total += UnitQty::new(micro, fund.clone()).value_at(price.sell.get(), c, RoundingMode::Down).ok()?.minor as i128;
    }
    i64::try_from(total.max(0)).ok()
}

/// The most the owner may owe today, by the product's share of the value.
fn limit_minor(share_percent: Dec, value_minor: i64, c: ic_money::Currency) -> i64 {
    let scale = dec_i128(10i128.pow(c.minor_digits()));
    let major = dec_i128(value_minor as i128) * share_percent / Dec::from(100i64) / scale;
    ic_money::round_to_money(major, c, RoundingMode::Down).map_or(0, |m| m.minor)
}

fn post(ledger: &mut Ledger, policy: &Policy, ctx: &mut Ctx<'_>, kind: &str, set: PostingSet, detail: BTreeMap<String, String>) -> Result<(), Reject> {
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, kind, Some(policy), ctx.date, postings, detail)
}

pub(crate) fn draw(
    ledger: &mut Ledger,
    catalog: &Catalog,
    book: &mut InvestmentBook,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    reference: String,
    amount_minor: i64,
) -> Result<(), Reject> {
    if !interest::plain(&reference) || amount_minor <= 0 {
        return Err(bad("invalid_loan", "a loan has a plain reference and an amount above zero"));
    }
    if let Some(existing) = policy.loan.as_ref().and_then(|l| l.draws.get(&reference)) {
        return if *existing == amount_minor {
            Ok(())
        } else {
            Err(bad("duplicate_loan", format!("{reference} was borrowed with another amount")))
        };
    }
    if policy.status != PolicyStatus::InForce {
        return Err(bad("not_in_force", "only a policy in force can be borrowed against"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| bad("unknown_product", "product version not active"))?;
    let (share, rate_code) = product
        .policy_loans()
        .ok_or_else(|| bad("loans_not_offered", "this product does not lend against its policies"))?;
    if catalog.index_rate_on(rate_code, ctx.date).is_none() {
        return Err(bad("no_loan_rate", format!("no {rate_code} rate is in force today")));
    }
    if policy.payments.contains_key(&reference) || policy.payouts.contains_key(&reference) || policy.payouts.values().any(|p| p.payment == reference) {
        return Err(bad("duplicate_loan", format!("{reference} is already a payment reference on this policy")));
    }
    crate::owner_hold::permit(policy)?;
    interest::permit(policy, ctx.date, RestrictedAction::Loan)?;
    let value =
        value_minor(ledger, book, policy, ctx.date).ok_or_else(|| bad("no_value", "the policy cannot be valued today: a fund it holds has no price"))?;
    let owed = policy.loan.as_ref().map_or(0, Loan::outstanding_minor);
    let room = limit_minor(share.get(), value, policy.currency).saturating_sub(owed);
    if amount_minor > room {
        return Err(bad(
            "over_loan_limit",
            format!("the most that can be borrowed today is {room} in minor units, on a value of {value}"),
        ));
    }
    let held = interest::insolvency(policy, ctx.date).cloned();
    let c = policy.currency;
    let dims = accounts::dims(policy, None, PayoutReason::Loan.posting_type());
    let mut set = PostingSet::new();
    set.debit(accounts::policy_loan(&policy.id, c), amount_minor as i128, dims.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), amount_minor as i128, dims);
    post(
        ledger,
        policy,
        ctx,
        "policy_loan_drawn",
        set,
        BTreeMap::from([("loan".to_string(), reference.clone())]),
    )?;
    let loan = policy.loan.get_or_insert_with(Loan::default);
    loan.principal_minor += amount_minor;
    loan.draws.insert(reference.clone(), amount_minor);
    // Interest runs from the day after the money is lent.
    loan.accrued_through.get_or_insert(ctx.date);
    if loan.capitalised_year == 0 {
        loan.capitalised_year = policy_year(policy.inception, ctx.date);
    }
    let outstanding_minor = loan.outstanding_minor();
    policy.payouts.insert(
        reference.clone(),
        Payout {
            amount_minor,
            reason: PayoutReason::Loan,
            payment: reference.clone(),
            state: if held.is_some() { PayoutState::Held } else { PayoutState::Owed },
            raised_on: ctx.date,
            directed: None,
        },
    );
    interest::consume(policy, ctx, RestrictedAction::Loan);
    ctx.emit(Event::PolicyLoanDrawn {
        policy: policy.id.clone(),
        reference: reference.clone(),
        amount_minor,
        outstanding_minor,
    });
    ctx.emit(Event::Investment(match held {
        Some(interest) => InvestEvent::PayoutHeld {
            policy: policy.id.clone(),
            payout: reference,
            amount_minor,
            reason: PayoutReason::Loan,
            interest,
        },
        None => InvestEvent::PayoutRaised {
            policy: policy.id.clone(),
            policyholder: policy.policyholder.clone(),
            payout: reference,
            amount_minor,
            reason: PayoutReason::Loan,
        },
    }));
    Ok(())
}

/// Take an amount off the loan, interest first. Returns what went to
/// interest and what to principal.
fn reduce(loan: &mut Loan, amount_minor: i64) -> (i64, i64) {
    let to_interest = amount_minor.min(loan.interest_minor);
    let to_principal = amount_minor - to_interest;
    loan.interest_minor -= to_interest;
    loan.principal_minor -= to_principal;
    if loan.outstanding_minor() == 0 {
        loan.carry_fine = 0;
        loan.over_value_since = None;
    }
    (to_interest, to_principal)
}

pub(crate) fn repay(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, receipt: String, amount: Money) -> Result<(), Reject> {
    if !interest::plain(&receipt) || amount.currency != policy.currency || amount.minor <= 0 {
        return Err(bad(
            "invalid_amount",
            "a repayment has a plain receipt and a positive amount in the policy's currency",
        ));
    }
    let Some(loan) = policy.loan.as_mut() else {
        return Err(bad("no_loan", "nothing is owed on a loan against this policy"));
    };
    if let Some(existing) = loan.repayments.get(&receipt) {
        return if *existing == amount.minor {
            Ok(())
        } else {
            Err(bad("duplicate_receipt", format!("receipt {receipt} was applied with another amount")))
        };
    }
    if policy.receipts.contains_key(&receipt) {
        return Err(bad("duplicate_receipt", format!("receipt {receipt} has already been applied to premium")));
    }
    if amount.minor > loan.outstanding_minor() {
        return Err(bad(
            "over_repayment",
            format!("{} is owed on the loan; a repayment is for that or less", loan.outstanding_minor()),
        ));
    }
    let (to_interest, to_principal) = reduce(loan, amount.minor);
    loan.repayments.insert(receipt.clone(), amount.minor);
    let outstanding_minor = loan.outstanding_minor();
    let c = policy.currency;
    let dims = accounts::dims(policy, None, "policy_loan_repaid");
    let mut set = PostingSet::new();
    set.debit(accounts::settlement(c), amount.minor as i128, dims.clone());
    set.credit(accounts::policy_loan(&policy.id, c), amount.minor as i128, dims);
    post(
        ledger,
        policy,
        ctx,
        "policy_loan_repaid",
        set,
        BTreeMap::from([("receipt".to_string(), receipt.clone())]),
    )?;
    ctx.emit(Event::PolicyLoanRepaid {
        policy: policy.id.clone(),
        reference: receipt,
        amount_minor: amount.minor,
        interest_minor: to_interest,
        principal_minor: to_principal,
        outstanding_minor,
        from: "receipt".to_string(),
    });
    Ok(())
}

/// Repay as much of the loan as an amount covers out of money the policy
/// is about to pay: a surrender value or a claim. The caller's own entry
/// has already put the money in `from`; this moves it to the loan. Returns
/// what was taken.
pub(crate) fn deduct(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    from: ic_ledger::Account,
    available_minor: i64,
    source: &str,
    reference: &str,
) -> Result<i64, Reject> {
    let Some(loan) = policy.loan.as_mut().filter(|l| l.outstanding_minor() > 0) else {
        return Ok(0);
    };
    let take = available_minor.min(loan.outstanding_minor());
    if take <= 0 {
        return Ok(0);
    }
    let (to_interest, to_principal) = reduce(loan, take);
    let outstanding_minor = loan.outstanding_minor();
    let c = policy.currency;
    let dims = accounts::dims(policy, None, "policy_loan_repaid");
    let mut set = PostingSet::new();
    set.debit(from, take as i128, dims.clone());
    set.credit(accounts::policy_loan(&policy.id, c), take as i128, dims);
    let detail = BTreeMap::from([(source.to_string(), reference.to_string())]);
    post(ledger, policy, ctx, "policy_loan_repaid", set, detail)?;
    ctx.emit(Event::PolicyLoanRepaid {
        policy: policy.id.clone(),
        reference: reference.to_string(),
        amount_minor: take,
        interest_minor: to_interest,
        principal_minor: to_principal,
        outstanding_minor,
        from: source.to_string(),
    });
    Ok(take)
}

/// The policy has ended and paid what it could: what is still owed on the
/// loan is lost. Returns what was written off.
pub(crate) fn write_off(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, cause: &str, reference: &str) -> Result<i64, Reject> {
    let Some(loan) = policy.loan.as_mut().filter(|l| l.outstanding_minor() > 0) else {
        return Ok(0);
    };
    let lost = loan.outstanding_minor();
    reduce(loan, lost);
    let c = policy.currency;
    let dims = accounts::dims(policy, None, "policy_loan_written_off");
    let mut set = PostingSet::new();
    set.debit(accounts::policy_loan_written_off(&policy.product_code, c), lost as i128, dims.clone());
    set.credit(accounts::policy_loan(&policy.id, c), lost as i128, dims);
    let detail = BTreeMap::from([(cause.to_string(), reference.to_string())]);
    post(ledger, policy, ctx, "policy_loan_written_off", set, detail)?;
    ctx.emit(Event::PolicyLoanWrittenOff {
        policy: policy.id.clone(),
        reference: reference.to_string(),
        amount_minor: lost,
        cause: cause.to_string(),
    });
    Ok(lost)
}

/// The end-of-day step: accrue interest for each day up to the day just
/// closed, capitalise it on an anniversary, and say so the first day the
/// loan is found to be more than the policy is worth.
pub(crate) fn end_of_day(ledger: &mut Ledger, catalog: &Catalog, book: &InvestmentBook, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    let Some(mut loan) = policy.loan.clone().filter(|l| l.outstanding_minor() > 0) else {
        return Ok(());
    };
    let Some(closed) = ctx.date.pred_opt() else {
        return Ok(());
    };
    let rate_code = catalog
        .product(&policy.product_hash)
        .and_then(|p| p.policy_loans())
        .map(|(_, code)| code.to_string());
    let c = policy.currency;
    let mut posted = 0i64;
    let mut day = loan.accrued_through.unwrap_or(closed);
    // Bounded per command, like billing: a policy catching up takes several
    // end-of-day steps.
    for _ in 0..45 {
        let Some(next) = day.succ_opt().filter(|n| *n <= closed) else {
            break;
        };
        day = next;
        ctx.work += 1;
        // Interest accrued to the anniversary becomes principal on it.
        let year = policy_year(policy.inception, day);
        if year > loan.capitalised_year && anniversary(policy.inception, year) == Some(day) {
            loan.principal_minor += loan.interest_minor;
            loan.interest_minor = 0;
        }
        loan.capitalised_year = loan.capitalised_year.max(year);
        let rate = rate_code.as_deref().and_then(|code| catalog.index_rate_on(code, day)).map(|r| r.get());
        if let Some(rate) = rate {
            let daily = dec_i128(loan.principal_minor as i128) * rate / Dec::from(365i64);
            loan.carry_fine += round_minor_to_fine(daily, c).map(|a| a.fine)?;
            let whole = (loan.carry_fine / FINE_PER_MINOR) as i64;
            loan.carry_fine -= whole as i128 * FINE_PER_MINOR;
            loan.interest_minor += whole;
            posted += whole;
        }
        loan.accrued_through = Some(day);
    }
    if posted > 0 {
        let dims = accounts::dims(policy, None, "policy_loan_interest");
        let mut set = PostingSet::new();
        set.debit(accounts::policy_loan(&policy.id, c), posted as i128, dims.clone());
        set.credit(accounts::policy_loan_interest(&policy.product_code, c), posted as i128, dims);
        let postings = set.into_postings(ledger)?;
        ctx.post(ledger, "policy_loan_interest", Some(policy), closed, postings, BTreeMap::new())?;
    }
    let value = value_minor(ledger, book, policy, ctx.date);
    let over = value.is_some_and(|v| loan.outstanding_minor() > v);
    if over && loan.over_value_since.is_none() {
        loan.over_value_since = Some(ctx.date);
        ctx.emit(Event::PolicyLoanOverValue {
            policy: policy.id.clone(),
            outstanding_minor: loan.outstanding_minor(),
            value_minor: value.unwrap_or(0),
            on: ctx.date,
        });
    } else if !over {
        loan.over_value_since = None;
    }
    policy.loan = Some(loan);
    Ok(())
}

/// What the ledger must hold for the policy's loan.
pub(crate) fn expected_balance(policy: &Policy) -> i128 {
    policy.loan.as_ref().map_or(0, |l| l.outstanding_minor() as i128)
}
