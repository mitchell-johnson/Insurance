//! Policy loans: the owner of a unit-linked policy borrows against its value,
//! is charged interest, repays when they choose, and has what is still owed
//! taken from the surrender value before anything is paid.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::event::Event;
use ic_core::interest::{InterestCommand, RestrictedAction};
use ic_core::invest::{Direction, FundDefinition, InvestCommand, InvestEvent, NewOrder, OrderAmount, OrderKind, PayoutReason, PayoutState, UnitPrice};
use ic_core::loan::LoanCommand;
use ic_core::owner_hold::SetOwnerHold;
use ic_core::payments::PayableKind;
use ic_core::policy::PolicyStatus;
use ic_core::{Command, PartyId, PolicyId, Reject};
use ic_money::{Currency::NZD, DecStr, FundId, Money, RoundingMode};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{invest_plan, issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// A Monday, and the policy's first day.
fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn growth() -> FundId {
    FundId("growth".to_string())
}

fn fund() -> FundDefinition {
    FundDefinition {
        id: growth(),
        version: 1,
        content_hash: "hash-growth-1".to_string(),
        name: "growth".to_string(),
        currency: NZD,
        cut_off: "14:00".to_string(),
        buy_spread_bps: 10,
        sell_spread_bps: 10,
        price_decimals: 4,
        unit_rounding: RoundingMode::HalfEven,
        non_pricing_days: Default::default(),
        open_to_new_money: true,
        pricing: BTreeMap::from([("movement_tolerance_bps".to_string(), "500".to_string())]),
    }
}

fn invest(c: &mut Cluster, caller: &str, command: InvestCommand) -> Result<(), Reject> {
    c.submit(0, caller, Command::Investment(Box::new(command)))
}

fn price(c: &mut Cluster, id: &str, date: NaiveDate, at: &str) {
    let price = UnitPrice {
        price_id: id.to_string(),
        fund: growth(),
        pricing_date: date,
        version: 1,
        buy: DecStr::parse(at).unwrap(),
        sell: DecStr::parse(at).unwrap(),
        mid: DecStr::parse(at).unwrap(),
        inputs_hash: format!("inputs-{id}"),
        supersedes: None,
    };
    invest(c, "unit-pricing", InvestCommand::ActivateUnitPrice(Box::new(price))).unwrap();
}

fn execute(c: &mut Cluster, date: NaiveDate) {
    let run = InvestCommand::ExecuteOrders {
        fund: growth(),
        pricing_date: date,
        limit: 100,
    };
    invest(c, "unit-orders", run).unwrap();
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

/// One partition with what is shipped (less whatever `keep` turns away), the
/// growth fund, and a policy holding 2,000 growth units bought at NZD 2.00 and
/// NZD 1,000 in cash: NZD 5,000 of value, of which the product lends half.
fn invested_with(keep: impl Fn(&Command) -> bool) -> (Cluster, PolicyId) {
    let mut c = Cluster::in_memory(1, 11);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap().into_iter().filter(|cmd| keep(cmd)) {
        c.activate_everywhere(cmd).unwrap();
    }
    invest(&mut c, "config", InvestCommand::ActivateFund(Box::new(fund()))).unwrap();
    let issue = invest_plan(1, start(), d(1990, 3, 10), Sex::Female);
    let policy = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let receipt = Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: "r1".to_string(),
        amount: Money::new(5_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    let allocate = InvestCommand::AllocateCash {
        policy: policy.clone(),
        reference: "alloc-1".to_string(),
        amount_minor: 5_000_00,
    };
    invest(&mut c, "policy-servicing", allocate).unwrap();
    let buy = NewOrder {
        order_id: "buy-1".to_string(),
        policy: policy.clone(),
        fund: growth(),
        direction: Direction::Buy,
        amount: OrderAmount::Cash { minor: 4_000_00 },
        kind: OrderKind::PremiumAllocation,
        pricing_date: start(),
        switch_to: None,
    };
    invest(&mut c, "policy-servicing", InvestCommand::PlaceOrder(Box::new(buy))).unwrap();
    price(&mut c, "g-1", start(), "2.0000");
    execute(&mut c, start());
    (c, policy)
}

fn invested() -> (Cluster, PolicyId) {
    invested_with(|_| true)
}

fn draw(c: &mut Cluster, policy: &PolicyId, reference: &str, amount_minor: i64) -> Result<(), Reject> {
    let command = LoanCommand::Draw {
        policy: policy.clone(),
        reference: reference.to_string(),
        amount_minor,
    };
    c.submit(0, "policy-servicing", Command::Loan(Box::new(command)))
}

fn repay(c: &mut Cluster, policy: &PolicyId, receipt: &str, amount: Money) -> Result<(), Reject> {
    let command = LoanCommand::Repay {
        policy: policy.clone(),
        receipt: receipt.to_string(),
        amount,
    };
    c.submit(0, "cash-matching", Command::Loan(Box::new(command)))
}

/// What the ledger says the policy owes on its loan.
fn lent(c: &Cluster, policy: &PolicyId) -> i128 {
    c.state(0).ledger.balance(&accounts::policy_loan(policy, NZD).id)
}

fn payable(c: &Cluster, policy: &PolicyId) -> i128 {
    c.state(0).ledger.natural_balance(&accounts::benefit_payable(policy, NZD).id)
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "{:?}", close.breaks);
    }
}

fn events(c: &mut Cluster) -> Vec<Event> {
    c.take_events().into_iter().map(|e| e.event).collect()
}

/// Purpose: the owner of a policy whose product lends against it can borrow up to the
/// product's share of what the policy is worth today (its units at the latest sell price
/// plus its uninvested cash), in one or several loans; the money is raised to benefit
/// payable under the loan's reference and settled by the payments module like any payout;
/// the same loan sent twice lends once. A loan is refused on a product that does not lend,
/// with no interest rate in force, above the limit, under a reference already used for
/// something else, while a recorded interest has not consented or the customer has a hold,
/// and a payout command cannot raise one.
/// Why it matters: a loan is the insurer's money secured only by the policy's value. Lent
/// above the limit, or twice for one request, it is unsecured lending nobody approved; paid
/// outside benefit payable it would skip the payee checks and the bank reconciliation; and
/// lent past an assignee's interest it takes value the owner had pledged to someone else.
#[test]
fn a_loan_is_lent_within_the_products_share_of_the_value_and_paid_like_any_payout() {
    let (mut c, policy) = invested();
    // Not on a product that does not lend, nor before there is a loan to repay.
    let term = term_life(2, start(), d(1988, 6, 2), Sex::Male, false, 400_000_00, None, Frequency::Monthly, None);
    let term_policy = term.policy.clone();
    c.submit(0, "new-business", issue_command(term)).unwrap();
    assert_eq!(code(draw(&mut c, &term_policy, "loan-1", 100_00)), "loans_not_offered");
    assert_eq!(code(repay(&mut c, &policy, "lr-1", Money::new(1_00, NZD))), "no_loan");
    assert_eq!(code(draw(&mut c, &policy, "loan 1", 100_00)), "invalid_loan");
    assert_eq!(code(draw(&mut c, &policy, "loan-1", 0)), "invalid_loan");
    // Only servicing lends and only cash matching applies a repayment.
    let as_other = Command::Loan(Box::new(LoanCommand::Draw {
        policy: policy.clone(),
        reference: "loan-1".to_string(),
        amount_minor: 100_00,
    }));
    assert!(c.submit(0, "cash-matching", as_other).is_err());

    // Half of NZD 5,000 and not a cent more.
    assert_eq!(code(draw(&mut c, &policy, "loan-1", 2_500_01)), "over_loan_limit");
    assert_eq!(lent(&c, &policy), 0);
    c.take_events();
    draw(&mut c, &policy, "loan-1", 2_000_00).unwrap();
    assert_eq!((lent(&c, &policy), payable(&c, &policy)), (2_000_00, 2_000_00));
    let raised = events(&mut c);
    assert!(raised.iter().any(|e| matches!(
        e,
        Event::PolicyLoanDrawn { reference, amount_minor: 2_000_00, outstanding_minor: 2_000_00, .. } if reference == "loan-1"
    )));
    assert!(raised.iter().any(|e| matches!(
        e,
        Event::Investment(InvestEvent::PayoutRaised { payout, amount_minor: 2_000_00, reason: PayoutReason::Loan, policyholder, .. })
            if payout == "loan-1" && *policyholder == PartyId("party-00000001".to_string())
    )));

    // Sent again it lends nothing more; with another amount it is refused.
    draw(&mut c, &policy, "loan-1", 2_000_00).unwrap();
    assert_eq!(lent(&c, &policy), 2_000_00);
    assert!(events(&mut c).is_empty());
    assert_eq!(code(draw(&mut c, &policy, "loan-1", 100_00)), "duplicate_loan");
    // The limit counts what is already owed, and the loan did not reduce the value.
    assert_eq!(code(draw(&mut c, &policy, "loan-2", 500_01)), "over_loan_limit");

    // A reference is a payment reference: one used by a withdrawal cannot be a loan.
    let withdrawal = InvestCommand::RaisePayout {
        policy: policy.clone(),
        payout: "wd-1".to_string(),
        amount_minor: 200_00,
        reason: PayoutReason::Withdrawal,
    };
    invest(&mut c, "policy-servicing", withdrawal).unwrap();
    assert_eq!(code(draw(&mut c, &policy, "wd-1", 100_00)), "duplicate_loan");
    // And a payout command cannot lend.
    let as_payout = InvestCommand::RaisePayout {
        policy: policy.clone(),
        payout: "loan-9".to_string(),
        amount_minor: 100_00,
        reason: PayoutReason::Loan,
    };
    assert_eq!(code(invest(&mut c, "policy-servicing", as_payout)), "invalid_payout");

    // The withdrawal took NZD 200 of value: half of 4,800 is 2,400, so 400 more.
    assert_eq!(code(draw(&mut c, &policy, "loan-2", 400_01)), "over_loan_limit");

    // An interest recorded on the policy must consent to a loan; a customer's hold stops it.
    let interest = |c: &mut Cluster, command: InterestCommand| c.submit(0, "policy-servicing", Command::Interest(Box::new(command)));
    let record = InterestCommand::Record {
        kind: Default::default(),
        policy: policy.clone(),
        interest: "int-1".to_string(),
        party: PartyId("party-bank".to_string()),
        notice_received: start(),
        extent_minor: None,
        effective_from: start(),
        notice: "vault/notice-1".to_string(),
    };
    interest(&mut c, record).unwrap();
    assert_eq!(code(draw(&mut c, &policy, "loan-2", 400_00)), "interest_consent_required");
    let consent = InterestCommand::Consent {
        policy: policy.clone(),
        interest: "int-1".to_string(),
        action: RestrictedAction::Loan,
        reference: "vault/c-1".to_string(),
    };
    interest(&mut c, consent).unwrap();
    let hold = |c: &mut Cluster, held: bool| {
        let command = SetOwnerHold {
            policy: policy.clone(),
            held,
            reference: "hold-1".to_string(),
        };
        c.submit(0, "customer-master", Command::SetOwnerHold(Box::new(command))).unwrap();
    };
    hold(&mut c, true);
    assert_eq!(code(draw(&mut c, &policy, "loan-2", 400_00)), "customer_hold");
    hold(&mut c, false);
    draw(&mut c, &policy, "loan-2", 400_00).unwrap();
    // The consent was for that one loan.
    assert_eq!(code(draw(&mut c, &policy, "loan-3", 1_00)), "interest_consent_required");
    let loan = c.state(0).policies[&policy].loan.clone().unwrap();
    assert_eq!((loan.principal_minor, loan.interest_minor, loan.outstanding_minor()), (2_400_00, 0, 2_400_00));
    assert_eq!(loan.draws, BTreeMap::from([("loan-1".to_string(), 2_000_00), ("loan-2".to_string(), 400_00)]));

    // The bank pays exactly the loan, under its reference.
    let settle = |c: &mut Cluster, payment: &str, minor: i64| {
        let command = Command::SettlePayable {
            policy: policy.clone(),
            payment: payment.to_string(),
            payable: PayableKind::Benefit,
            refund: None,
            amount: Money::new(minor, NZD),
            bank_account: "nz-main-benefits".to_string(),
        };
        c.submit(0, "payments", command)
    };
    assert!(settle(&mut c, "loan-1", 1_999_00).is_err());
    settle(&mut c, "loan-1", 2_000_00).unwrap();
    assert_eq!(c.state(0).policies[&policy].payouts["loan-1"].state, PayoutState::Settled);
    assert_eq!(c.state(0).policies[&policy].payouts["loan-2"].state, PayoutState::Owed);
    // Paid, the money is still owed on the loan.
    assert_eq!(lent(&c, &policy), 2_400_00);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // With no rate to charge, nothing is lent.
    let (mut bare, policy) = invested_with(|cmd| !matches!(cmd, Command::ActivateIndexRate { code, .. } if code == "policy-loan-rate"));
    assert_eq!(code(draw(&mut bare, &policy, "loan-1", 100_00)), "no_loan_rate");
}

/// Purpose: interest is charged for every day after the loan is lent, at the yearly rate
/// in force over 365, on the principal; whole cents are added to what is owed and to
/// interest income, and the fraction of a cent is carried so that nothing is lost over
/// time; on the policy anniversary the interest accrued becomes principal and bears
/// interest itself. A repayment meets interest first, is applied once however often its
/// receipt is sent, and is refused above what is owed, in another currency, or under a
/// receipt already used. The ledger's loan account equals the loan record every day.
/// Why it matters: loan interest is income the insurer reports and a debt the customer is
/// told to the cent. Interest rounded daily and the fraction dropped undercharges every
/// loan; a repayment applied twice from one receipt forgives debt nobody paid; and a loan
/// record that drifts from the ledger means the statement the customer sees is not what
/// the accounts say they owe.
#[test]
fn interest_accrues_daily_is_capitalised_on_the_anniversary_and_a_repayment_meets_interest_first() {
    let (mut c, policy) = invested();
    draw(&mut c, &policy, "loan-1", 2_000_00).unwrap();
    let income = accounts::policy_loan_interest("invest-plan", NZD);
    // 7.95% a year on NZD 2,000 is 43.5616 cents a day. Eleven closes charge the ten
    // days after the loan was lent: 435.616 cents, of which 435 are posted.
    days(&mut c, 11);
    let loan = c.state(0).policies[&policy].loan.clone().unwrap();
    assert_eq!((loan.principal_minor, loan.interest_minor), (2_000_00, 4_35));
    assert_eq!(loan.carry_fine, 616_440);
    assert_eq!(loan.accrued_through, Some(d(2027, 11, 25)));
    assert_eq!(lent(&c, &policy), 2_004_35);
    assert_eq!(c.state(0).ledger.natural_balance(&income.id), 4_35);

    // A repayment: not more than is owed, in the policy's currency, under a fresh receipt.
    assert_eq!(code(repay(&mut c, &policy, "lr-1", Money::new(2_004_36, NZD))), "over_repayment");
    assert_eq!(
        code(repay(&mut c, &policy, "lr-1", Money::new(500_00, ic_money::Currency::AUD))),
        "invalid_amount"
    );
    assert_eq!(code(repay(&mut c, &policy, "lr 1", Money::new(500_00, NZD))), "invalid_amount");
    assert_eq!(code(repay(&mut c, &policy, "r1", Money::new(500_00, NZD))), "duplicate_receipt");
    c.take_events();
    let settlement = |c: &Cluster| c.state(0).ledger.balance(&accounts::settlement(NZD).id);
    let before = settlement(&c);
    repay(&mut c, &policy, "lr-1", Money::new(500_00, NZD)).unwrap();
    assert_eq!(settlement(&c) - before, 500_00);
    let repaid = events(&mut c);
    assert!(
        repaid.iter().any(|e| matches!(
            e,
            Event::PolicyLoanRepaid { reference, amount_minor: 500_00, interest_minor: 4_35, principal_minor: 495_65, outstanding_minor: 1_504_35, from, .. }
                if reference == "lr-1" && from == "receipt"
        )),
        "{repaid:?}"
    );
    repay(&mut c, &policy, "lr-1", Money::new(500_00, NZD)).unwrap();
    assert_eq!(lent(&c, &policy), 1_504_35);
    assert_eq!(code(repay(&mut c, &policy, "lr-1", Money::new(400_00, NZD))), "duplicate_receipt");

    // To the day after the anniversary, 15 November 2028. From 26 November 2027 to 14
    // November 2028 is 355 days at 32.765979 cents on NZD 1,504.35: with the fraction
    // carried, 11,632 cents. On the anniversary that becomes principal, NZD 1,620.67,
    // and the anniversary's own interest of 35.2995 cents is charged on it.
    days(&mut c, 356);
    assert_eq!(c.business_date(), Some(d(2028, 11, 16)));
    let loan = c.state(0).policies[&policy].loan.clone().unwrap();
    assert_eq!((loan.principal_minor, loan.interest_minor), (1_620_67, 35));
    assert_eq!(loan.carry_fine, 838_510);
    assert_eq!(lent(&c, &policy), 1_621_02);
    assert_eq!(c.state(0).ledger.natural_balance(&income.id), 4_35 + 11_632 + 35);
    // Capitalised once: the next day adds a day's interest and moves no principal.
    days(&mut c, 1);
    let loan = c.state(0).policies[&policy].loan.clone().unwrap();
    assert_eq!((loan.principal_minor, loan.interest_minor), (1_620_67, 71));

    // Repaid in full, the loan is over: no interest the next day, and room to borrow again.
    repay(&mut c, &policy, "lr-2", Money::new(1_621_38, NZD)).unwrap();
    days(&mut c, 2);
    assert_eq!(lent(&c, &policy), 0);
    assert_eq!(
        c.state(0).policies[&policy].loan.as_ref().map(|l| (l.outstanding_minor(), l.carry_fine)),
        Some((0, 0))
    );
    draw(&mut c, &policy, "loan-2", 2_500_00).unwrap();
    assert_eq!(lent(&c, &policy), 2_500_00);
}

/// Purpose: when a policy with a loan is surrendered, what is owed is taken from the
/// surrender value before anything is paid, interest first, and only the rest is raised to
/// benefit payable. Where the value has fallen below the loan, the insurer is told once,
/// the first day it is so; the surrender then pays the owner nothing, the whole value goes
/// to the loan and what it does not cover is written off to its own expense account. The
/// policy ends with no loan balance either way, and cannot be borrowed against again.
/// Why it matters: the loan's only security is the policy's value. A surrender that paid
/// the full value would hand the customer the money twice, once as the loan and once as the
/// surrender. A loan left on a closed policy is an asset in the accounts that nobody will
/// ever repay, and a shortfall nobody is told about is found only when it is a loss.
#[test]
fn a_surrender_repays_the_loan_first_and_writes_off_what_the_value_does_not_cover() {
    let surrender = |c: &mut Cluster, policy: &PolicyId, date: NaiveDate| {
        let ask = InvestCommand::Surrender {
            policy: policy.clone(),
            surrender: "sur-1".to_string(),
            pricing_dates: [(growth(), date)].into(),
        };
        invest(c, "policy-servicing", ask).unwrap();
    };
    let complete = |c: &mut Cluster, policy: &PolicyId| {
        let finish = InvestCommand::CompleteSurrender {
            policy: policy.clone(),
            payout: "sur-pay".to_string(),
        };
        invest(c, "policy-servicing", finish).unwrap();
    };

    // The value covers the loan: NZD 5,000 less NZD 1,000 and a day's interest.
    let (mut c, policy) = invested();
    draw(&mut c, &policy, "loan-1", 1_000_00).unwrap();
    days(&mut c, 2);
    let today = c.business_date().unwrap();
    assert_eq!(lent(&c, &policy), 1_000_21);
    surrender(&mut c, &policy, today);
    price(&mut c, "g-2", today, "2.0000");
    execute(&mut c, today);
    c.take_events();
    complete(&mut c, &policy);
    let done = events(&mut c);
    assert!(done.iter().any(|e| matches!(
        e,
        Event::PolicyLoanRepaid { reference, amount_minor: 1_000_21, interest_minor: 21, principal_minor: 1_000_00, outstanding_minor: 0, from, .. }
            if reference == "sur-1" && from == "surrender"
    )));
    assert!(!done.iter().any(|e| matches!(e, Event::PolicyLoanWrittenOff { .. })));
    assert!(done.iter().any(|e| matches!(
        e,
        Event::Investment(InvestEvent::SurrenderCompleted { payout: Some(payout), value_minor: 5_000_00, .. }) if payout == "sur-pay"
    )));
    let record = &c.state(0).policies[&policy];
    assert_eq!(record.status, PolicyStatus::Surrendered);
    assert_eq!(record.payouts["sur-pay"].amount_minor, 5_000_00 - 1_000_21);
    assert_eq!(lent(&c, &policy), 0);
    // The loan itself is still owed to the customer's bank account, beside the surrender.
    assert_eq!(payable(&c, &policy), 1_000_00 + 3_999_79);
    assert_eq!(code(draw(&mut c, &policy, "loan-2", 1_00)), "not_in_force");
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // The value falls below the loan. NZD 2,500 is lent on NZD 5,000; the units then
    // price at 50 cents, and the policy is worth NZD 2,000.
    let (mut c, policy) = invested();
    draw(&mut c, &policy, "loan-1", 2_500_00).unwrap();
    days(&mut c, 1);
    assert!(!events(&mut c).iter().any(|e| matches!(e, Event::PolicyLoanOverValue { .. })));
    price(&mut c, "g-2", d(2027, 11, 16), "0.5000");
    days(&mut c, 1);
    let told = events(&mut c);
    assert!(
        told.iter().any(|e| matches!(
            e,
            Event::PolicyLoanOverValue { outstanding_minor: 2_500_54, value_minor: 2_000_00, on, .. } if *on == d(2027, 11, 17)
        )),
        "{told:?}"
    );
    assert_eq!(c.state(0).policies[&policy].loan.as_ref().unwrap().over_value_since, Some(d(2027, 11, 17)));
    // Said once, not every day it stays so.
    days(&mut c, 1);
    assert!(!events(&mut c).iter().any(|e| matches!(e, Event::PolicyLoanOverValue { .. })));
    let today = c.business_date().unwrap();
    assert_eq!(lent(&c, &policy), 2_501_08);
    surrender(&mut c, &policy, today);
    price(&mut c, "g-3", today, "0.5000");
    execute(&mut c, today);
    complete(&mut c, &policy);
    let done = events(&mut c);
    assert!(done.iter().any(|e| matches!(
        e,
        Event::PolicyLoanRepaid {
            amount_minor: 2_000_00,
            interest_minor: 1_08,
            principal_minor: 1_998_92,
            outstanding_minor: 501_08,
            ..
        }
    )));
    assert!(done.iter().any(|e| matches!(
        e,
        Event::PolicyLoanWrittenOff { reference, amount_minor: 501_08, cause, .. } if reference == "sur-1" && cause == "surrender"
    )));
    assert!(done.iter().any(|e| matches!(
        e,
        Event::Investment(InvestEvent::SurrenderCompleted {
            payout: None,
            value_minor: 2_000_00,
            ..
        })
    )));
    let record = &c.state(0).policies[&policy];
    assert_eq!(record.status, PolicyStatus::Surrendered);
    assert!(!record.payouts.contains_key("sur-pay"));
    assert_eq!(lent(&c, &policy), 0);
    let lost = accounts::policy_loan_written_off("invest-plan", NZD);
    assert_eq!(c.state(0).ledger.natural_balance(&lost.id), 501_08);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    // Nothing more is charged on a policy that has ended.
    days(&mut c, 2);
    assert_eq!(lent(&c, &policy), 0);
}
