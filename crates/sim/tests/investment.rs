//! The investment ledger: funds, unit prices, forward-priced unit orders,
//! their execution and switches.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::event::Event;
use ic_core::invest::{
    Direction, FundDefinition, InvestCommand, InvestEvent, InvestmentBook, NewOrder, OrderAmount, OrderKind, OrderState, PayoutReason, PayoutState, Question,
    Replayable, Reply, UnitPrice,
};
use ic_core::payments::PayableKind;
use ic_core::{Command, PolicyId, Reject};
use ic_money::{Currency::NZD, DecStr, FundId, Money, RoundingMode};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// A Monday.
fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn fund_id(name: &str) -> FundId {
    FundId(name.to_string())
}

fn fund(name: &str) -> FundDefinition {
    FundDefinition {
        id: fund_id(name),
        version: 1,
        content_hash: format!("hash-{name}-1"),
        name: name.to_string(),
        currency: NZD,
        cut_off: "14:00".to_string(),
        buy_spread_bps: 10,
        sell_spread_bps: 10,
        price_decimals: 4,
        unit_rounding: RoundingMode::HalfEven,
        non_pricing_days: [d(2027, 11, 17)].into(),
        open_to_new_money: true,
        pricing: BTreeMap::from([("movement_tolerance_bps".to_string(), "500".to_string())]),
    }
}

fn price(id: &str, name: &str, date: NaiveDate, buy: &str, sell: &str) -> UnitPrice {
    UnitPrice {
        price_id: id.to_string(),
        fund: fund_id(name),
        pricing_date: date,
        version: 1,
        buy: DecStr::parse(buy).unwrap(),
        sell: DecStr::parse(sell).unwrap(),
        mid: DecStr::parse(buy).unwrap(),
        inputs_hash: format!("inputs-{id}"),
        supersedes: None,
    }
}

fn order(id: &str, policy: &PolicyId, name: &str, direction: Direction, amount: OrderAmount, date: NaiveDate) -> NewOrder {
    NewOrder {
        order_id: id.to_string(),
        policy: policy.clone(),
        fund: fund_id(name),
        direction,
        amount,
        kind: if direction == Direction::Buy {
            OrderKind::PremiumAllocation
        } else {
            OrderKind::Withdrawal
        },
        pricing_date: date,
        switch_to: None,
    }
}

fn invest(c: &mut Cluster, caller: &str, command: InvestCommand) -> Result<(), Reject> {
    c.submit(0, caller, Command::Investment(Box::new(command)))
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

fn place(c: &mut Cluster, o: NewOrder) -> Result<(), Reject> {
    invest(c, "policy-servicing", InvestCommand::PlaceOrder(Box::new(o)))
}

fn execute(c: &mut Cluster, name: &str, date: NaiveDate) -> Result<(), Reject> {
    invest(
        c,
        "unit-orders",
        InvestCommand::ExecuteOrders {
            fund: fund_id(name),
            pricing_date: date,
            limit: 100,
        },
    )
}

/// One partition with the shipped products, two funds, and a policy holding
/// NZD 5,000 in pending investment cash.
fn invested() -> (Cluster, PolicyId) {
    let mut c = Cluster::in_memory(1, 11);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    for name in ["growth", "bond"] {
        invest(&mut c, "config", InvestCommand::ActivateFund(Box::new(fund(name)))).unwrap();
    }
    let issue = term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let policy = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let receipt = Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: "r1".to_string(),
        amount: Money::new(61_84 + 5_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    let allocate = InvestCommand::AllocateCash {
        policy: policy.clone(),
        reference: "alloc-1".to_string(),
        amount_minor: 5_000_00,
    };
    invest(&mut c, "policy-servicing", allocate).unwrap();
    (c, policy)
}

fn pending_cash(c: &Cluster, policy: &PolicyId) -> i128 {
    c.state(0).ledger.natural_balance(&accounts::pending_investment_cash(policy, NZD).id)
}

fn units(c: &Cluster, policy: &PolicyId, name: &str) -> i128 {
    c.state(0).ledger.natural_balance(&accounts::unit_account(policy, &fund_id(name)).id)
}

fn invest_events(c: &mut Cluster) -> Vec<InvestEvent> {
    c.take_events()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::Investment(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// Purpose: a buy order waits, with its cash in pending investment cash, until the price
/// for its pricing date is activated; it then executes at the buy price in one entry that
/// balances in dollars and in units, the units rounded by the fund's rule; a sell executes
/// at the sell price and returns cash to pending; executing again does nothing; and the
/// partition reconciles throughout, with the fund's unit position equal to the units held.
/// Why it matters: this is the whole of a unit-linked policy's money. Cash that buys units
/// before a price exists is money in the market without units; an order executed twice is
/// units given away; an entry that does not balance in units is a fund whose units on
/// issue no longer agree with the administrator.
#[test]
fn an_order_waits_for_its_price_and_executes_once() {
    let (mut c, policy) = invested();
    let today = start();
    place(
        &mut c,
        order("buy-1", &policy, "growth", Direction::Buy, OrderAmount::Cash { minor: 1_000_00 }, today),
    )
    .unwrap();
    // No price yet: nothing to execute at, and the cash still waits.
    assert_eq!(code(execute(&mut c, "growth", today)), "no_price");
    assert_eq!((pending_cash(&c, &policy), units(&c, &policy, "growth")), (5_000_00, 0));
    assert!(c.end_of_day().unwrap().breaks.is_empty());

    // The price is struck the next day for the order's date.
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("p-1", "growth", today, "2.0000", "1.9960"))),
    )
    .unwrap();
    let run = Command::Investment(Box::new(InvestCommand::ExecuteOrders {
        fund: fund_id("growth"),
        pricing_date: today,
        limit: 100,
    }));
    let applied = c.submit_keyed(0, "execute-growth-1", "unit-orders", run);
    assert_eq!(applied.result, Ok(()));
    // One entry, carrying the price it used.
    assert_eq!(applied.entries.len(), 1);
    let detail = &applied.entries[0].body.detail;
    assert_eq!(
        (detail["price_id"].as_str(), detail["price"].as_str(), detail["price_side"].as_str()),
        ("p-1", "2.0000", "buy")
    );
    assert_eq!((detail["pricing_date"].clone(), detail["order_id"].as_str()), (today.to_string(), "buy-1"));
    assert_eq!((pending_cash(&c, &policy), units(&c, &policy, "growth")), (4_000_00, 500_000_000));
    let position = c.state(0).ledger.balance(&accounts::fund_unit_position(&fund_id("growth")).id);
    assert_eq!(position, 500_000_000);
    let executed = c.state(0).investment.order("buy-1").unwrap();
    assert_eq!(
        executed.state,
        OrderState::Executed {
            price_id: "p-1".to_string(),
            cash_minor: 1_000_00,
            units_micro: 500_000_000,
            on: today.succ_opt().unwrap(),
        }
    );
    // Nothing is left pending, so a second run changes nothing.
    execute(&mut c, "growth", today).unwrap();
    assert_eq!(units(&c, &policy, "growth"), 500_000_000);

    // Sell 100 units for the next pricing date, at the sell price: 100 x 1.9960.
    let next = c.business_date().unwrap();
    place(
        &mut c,
        order("sell-1", &policy, "growth", Direction::Sell, OrderAmount::Units { micro: 100_000_000 }, next),
    )
    .unwrap();
    c.end_of_day().unwrap();
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("p-2", "growth", next, "2.0000", "1.9960"))),
    )
    .unwrap();
    execute(&mut c, "growth", next).unwrap();
    assert_eq!((pending_cash(&c, &policy), units(&c, &policy, "growth")), (4_000_00 + 199_60, 400_000_000));
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    let on_issue = c.state(0).investment.units_on_issue(&c.state(0).ledger, &fund_id("growth"));
    assert_eq!(
        (on_issue.units_micro, on_issue.executed_through, on_issue.pending.len()),
        (400_000_000, Some(next), 0)
    );
}

/// Purpose: an order is refused when it could not be honoured or priced fairly: for a fund
/// that is not active, closed to new money or in another state of the policy; for a date in
/// the past, on a day the fund does not price, or whose price is already struck; for more
/// cash than is waiting uncommitted or more units than are held uncommitted; under an ID
/// already used for something else. The same order sent twice is accepted once. An order
/// can be cancelled until its price is active and not after.
/// Why it matters: an order accepted after its price is known is a free option against the
/// other investors in the fund; two orders counting on the same cash or units leave one of
/// them to fail on the day; and a cancelled order that still executes moves a customer's
/// money against their instruction.
#[test]
fn orders_that_cannot_be_honoured_are_refused_when_placed() {
    let (mut c, policy) = invested();
    let today = start();
    let buy = |id: &str, minor: i64, date: NaiveDate| order(id, &policy, "growth", Direction::Buy, OrderAmount::Cash { minor }, date);

    let mut unknown = buy("o-unknown", 100_00, today);
    unknown.fund = fund_id("property");
    assert_eq!(code(place(&mut c, unknown)), "unknown_fund");
    assert_eq!(code(place(&mut c, buy("o/slash", 100_00, today))), "invalid_order");
    assert_eq!(code(place(&mut c, buy("o-past", 100_00, d(2027, 11, 12)))), "invalid_order");
    assert_eq!(code(place(&mut c, buy("o-weekend", 100_00, d(2027, 11, 20)))), "invalid_order");
    assert_eq!(code(place(&mut c, buy("o-holiday", 100_00, d(2027, 11, 17)))), "invalid_order");
    assert_eq!(code(place(&mut c, buy("o-zero", 0, today))), "invalid_order");
    let units_buy = order("o-units", &policy, "growth", Direction::Buy, OrderAmount::Units { micro: 5 }, today);
    assert_eq!(code(place(&mut c, units_buy)), "invalid_order");
    let mut switch_buy = buy("o-switch-buy", 100_00, today);
    switch_buy.switch_to = Some(fund_id("bond"));
    assert_eq!(code(place(&mut c, switch_buy)), "invalid_order");
    assert_eq!(code(place(&mut c, buy("o-big", 5_000_01, today))), "insufficient_pending_cash");
    let sell = |id: &str, amount: OrderAmount| order(id, &policy, "growth", Direction::Sell, amount, today);
    assert_eq!(code(place(&mut c, sell("o-none", OrderAmount::AllUnits))), "insufficient_units");
    assert_eq!(code(place(&mut c, sell("o-none-2", OrderAmount::Units { micro: 1 }))), "insufficient_units");
    assert_eq!(code(place(&mut c, sell("o-none-3", OrderAmount::Cash { minor: 100 }))), "insufficient_units");
    let stranger = order(
        "o-stranger",
        &PolicyId::from("nobody"),
        "growth",
        Direction::Buy,
        OrderAmount::Cash { minor: 1 },
        today,
    );
    assert_eq!(code(place(&mut c, stranger)), "unknown_policy");
    assert_eq!(
        code(invest(
            &mut c,
            "claims",
            InvestCommand::ExecuteOrders {
                fund: fund_id("growth"),
                pricing_date: today,
                limit: 1
            }
        )),
        "caller_not_allowed"
    );

    // Two orders cannot count on the same cash; the same order twice is one order.
    place(&mut c, buy("o-1", 3_000_00, today)).unwrap();
    place(&mut c, buy("o-1", 3_000_00, today)).unwrap();
    assert_eq!(code(place(&mut c, buy("o-1", 2_000_00, today))), "duplicate_order");
    assert_eq!(code(place(&mut c, buy("o-2", 2_000_01, today))), "insufficient_pending_cash");
    // Nor can committed cash go back to suspense, though the rest can.
    let back = |minor: i64, reference: &str| InvestCommand::AllocateCash {
        policy: policy.clone(),
        reference: reference.to_string(),
        amount_minor: minor,
    };
    assert_eq!(code(invest(&mut c, "policy-servicing", back(-2_000_01, "back-1"))), "cash_committed");
    assert_eq!(code(invest(&mut c, "policy-servicing", back(0, "back-0"))), "invalid_allocation");
    assert_eq!(code(invest(&mut c, "policy-servicing", back(9_999_999_00, "more"))), "insufficient_suspense");
    invest(&mut c, "policy-servicing", back(-2_000_00, "back-2")).unwrap();
    assert_eq!(pending_cash(&c, &policy), 3_000_00);

    // Cancelled before the price: the cash is free again, and cancelling twice is one cancel.
    let cancel = |id: &str| InvestCommand::CancelOrder { order_id: id.to_string() };
    assert_eq!(code(invest(&mut c, "policy-servicing", cancel("o-missing"))), "unknown_order");
    invest(&mut c, "policy-servicing", cancel("o-1")).unwrap();
    invest(&mut c, "policy-servicing", cancel("o-1")).unwrap();
    assert!(matches!(c.state(0).investment.order("o-1").unwrap().state, OrderState::Cancelled { .. }));
    place(&mut c, buy("o-3", 3_000_00, today)).unwrap();

    // Once the price is active the order can no longer be cancelled, nor a new one take that date.
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("p-1", "growth", today, "1.5000", "1.4970"))),
    )
    .unwrap();
    assert_eq!(code(invest(&mut c, "policy-servicing", cancel("o-3"))), "price_struck");
    assert_eq!(code(place(&mut c, buy("o-late", 1, today))), "price_struck");
    assert_eq!(
        code(invest(
            &mut c,
            "unit-orders",
            InvestCommand::ExecuteOrders {
                fund: fund_id("growth"),
                pricing_date: today,
                limit: 0
            }
        )),
        "invalid_chunk"
    );
    execute(&mut c, "growth", today).unwrap();
    assert_eq!(code(invest(&mut c, "policy-servicing", cancel("o-3"))), "order_executed");
    assert_eq!(units(&c, &policy, "growth"), 2_000_000_000);

    // Units promised to one sell cannot be sold again.
    let next = d(2027, 11, 16);
    c.end_of_day().unwrap();
    let sell = |id: &str, amount: OrderAmount| order(id, &policy, "growth", Direction::Sell, amount, next);
    place(&mut c, sell("s-1", OrderAmount::Units { micro: 1_500_000_000 })).unwrap();
    assert_eq!(
        code(place(&mut c, sell("s-2", OrderAmount::Units { micro: 500_000_001 }))),
        "insufficient_units"
    );
    assert_eq!(code(place(&mut c, sell("s-all", OrderAmount::AllUnits))), "insufficient_units");
    place(&mut c, sell("s-3", OrderAmount::Cash { minor: 100_00 })).unwrap();

    // A fund closed to new money takes no premium, and a policy that is not in force takes no order.
    let mut closed = fund("bond");
    closed.version = 2;
    closed.content_hash = "hash-bond-2".to_string();
    closed.open_to_new_money = false;
    invest(&mut c, "config", InvestCommand::ActivateFund(Box::new(closed))).unwrap();
    let bond = order("b-1", &policy, "bond", Direction::Buy, OrderAmount::Cash { minor: 1_00 }, next);
    assert_eq!(code(place(&mut c, bond)), "fund_closed");
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}

/// Purpose: a switch sells in one fund and, when the sell executes, places a buy in the
/// other fund for the cash it fetched: for the same pricing date where that fund prices
/// that day, even if its price is already struck, and for its next pricing date where it
/// does not. A sell for a cash amount sells
/// the units that amount needs, or the whole holding if it is worth less. An order whose
/// units have gone by the time its price arrives is cancelled without stopping the rest.
/// Why it matters: a switch that bought before it sold would spend money the policy does
/// not have; one that lost its second half would leave a customer's savings as cash they
/// never asked to hold; and one bad order stopping the day's run would leave every other
/// investor's order unexecuted.
#[test]
fn a_switch_sells_then_buys_and_a_failed_order_does_not_stop_the_run() {
    let (mut c, policy) = invested();
    let monday = start();
    place(
        &mut c,
        order("buy-1", &policy, "growth", Direction::Buy, OrderAmount::Cash { minor: 4_000_00 }, monday),
    )
    .unwrap();
    c.end_of_day().unwrap();
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("g-mon", "growth", monday, "2.0000", "2.0000"))),
    )
    .unwrap();
    execute(&mut c, "growth", monday).unwrap();
    assert_eq!(units(&c, &policy, "growth"), 2_000_000_000);

    // Tuesday: switch 500 units to bond and sell NZD 300 worth. A switch to the same fund
    // is refused.
    let tuesday = d(2027, 11, 16);
    let mut switch = order("sw-1", &policy, "growth", Direction::Sell, OrderAmount::Units { micro: 500_000_000 }, tuesday);
    switch.kind = OrderKind::SwitchOut;
    switch.switch_to = Some(fund_id("bond"));
    let mut same = switch.clone();
    same.order_id = "sw-self".to_string();
    same.switch_to = Some(fund_id("growth"));
    assert_eq!(code(place(&mut c, same)), "invalid_order");
    place(&mut c, switch).unwrap();
    place(
        &mut c,
        order("cash-1", &policy, "growth", Direction::Sell, OrderAmount::Cash { minor: 300_00 }, tuesday),
    )
    .unwrap();
    // A second switch, of 100 units, to a fund that does not price on Tuesday or Wednesday.
    let mut income = fund("income");
    income.non_pricing_days = [tuesday, d(2027, 11, 17)].into();
    invest(&mut c, "config", InvestCommand::ActivateFund(Box::new(income))).unwrap();
    let mut second = order("sw-2", &policy, "growth", Direction::Sell, OrderAmount::Units { micro: 100_000_000 }, tuesday);
    second.kind = OrderKind::SwitchOut;
    second.switch_to = Some(fund_id("income"));
    place(&mut c, second).unwrap();
    // Bond struck Tuesday's price before growth did. The customer asked for the switch
    // before either price was known, so its buy still takes Tuesday's bond price.
    c.end_of_day().unwrap();
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("b-tue", "bond", tuesday, "1.0000", "1.0000"))),
    )
    .unwrap();
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("g-tue", "growth", tuesday, "3.0000", "3.0000"))),
    )
    .unwrap();
    execute(&mut c, "growth", tuesday).unwrap();
    // 500 units at 3.00 = 1,500.00 to bond; 300.00 needs exactly 100 units; 100 units to income.
    assert_eq!(units(&c, &policy, "growth"), 2_000_000_000 - 500_000_000 - 100_000_000 - 100_000_000);
    let second_half = c.state(0).investment.order("sw-1.in").expect("the switch placed its buy").clone();
    assert_eq!(second_half.order.amount, OrderAmount::Cash { minor: 1_500_00 });
    assert_eq!(
        (second_half.order.fund.clone(), second_half.order.pricing_date, second_half.order.kind),
        (fund_id("bond"), tuesday, OrderKind::SwitchIn)
    );
    assert_eq!(second_half.state, OrderState::Pending);
    // The other switch's buy waits for income's next pricing date, Thursday.
    let later = c.state(0).investment.order("sw-2.in").expect("the second switch placed its buy").clone();
    assert_eq!(
        (later.order.amount, later.order.pricing_date),
        (OrderAmount::Cash { minor: 300_00 }, d(2027, 11, 18))
    );
    assert_eq!(pending_cash(&c, &policy), 1_000_00 + 1_500_00 + 300_00 + 300_00);
    // The switch's cash is committed to its buy: it cannot be returned to suspense.
    let back = InvestCommand::AllocateCash {
        policy: policy.clone(),
        reference: "back".to_string(),
        amount_minor: -1_300_01,
    };
    assert_eq!(code(invest(&mut c, "policy-servicing", back)), "cash_committed");
    // Bond's Tuesday price is in force, so the buy is ready for the run at once.
    let todo = c.state(0).investment.executable();
    assert_eq!(todo.len(), 1);
    assert_eq!((todo[0].fund.clone(), todo[0].pricing_date, todo[0].pending), (fund_id("bond"), tuesday, 1));
    execute(&mut c, "bond", tuesday).unwrap();
    assert_eq!(units(&c, &policy, "bond"), 1_500_000_000);

    // Thursday: sell everything in growth twice over. The first takes it all for what it
    // fetches; the cash order asks for more than is left and is cancelled; income's buy executes.
    c.end_of_day().unwrap();
    let thursday = d(2027, 11, 18);
    assert_eq!(c.business_date(), Some(thursday));
    place(
        &mut c,
        order("a-cash", &policy, "growth", Direction::Sell, OrderAmount::Cash { minor: 9_999_00 }, thursday),
    )
    .unwrap();
    place(
        &mut c,
        order("z-cash", &policy, "growth", Direction::Sell, OrderAmount::Cash { minor: 50_00 }, thursday),
    )
    .unwrap();
    c.end_of_day().unwrap();
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("g-thu", "growth", thursday, "1.0000", "1.0000"))),
    )
    .unwrap();
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("i-thu", "income", thursday, "1.5000", "1.5000"))),
    )
    .unwrap();
    c.take_events();
    execute(&mut c, "growth", thursday).unwrap();
    execute(&mut c, "income", thursday).unwrap();
    // 1,300 units at 1.00: worth less than 9,999.00, so all of it is sold for 1,300.00.
    assert_eq!(units(&c, &policy, "growth"), 0);
    assert_eq!(units(&c, &policy, "income"), 200_000_000);
    assert!(matches!(c.state(0).investment.order("z-cash").unwrap().state, OrderState::Cancelled { .. }));
    let events = invest_events(&mut c);
    let executed: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            InvestEvent::UnitOrderExecuted { order_id, .. } => Some(order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(executed, ["a-cash", "sw-2.in"]);
    assert!(events
        .iter()
        .any(|e| matches!(e, InvestEvent::UnitOrderCancelled { order_id, .. } if order_id == "z-cash")));
    assert_eq!(pending_cash(&c, &policy), 1_000_00 + 300_00 + 1_300_00);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    // The cash has been waiting since the day it was first allocated.
    let waiting = c.state(0).investment.waiting_cash(&c.state(0).ledger, &c.state(0).policies);
    assert_eq!((waiting.len(), waiting[0].amount_minor, waiting[0].since), (1, 2_600_00, monday));
}

/// Purpose: a fund or a price is activated once and is then immutable: the same content
/// again is accepted and changes nothing, other content under the same version or ID is
/// refused, as are a fund that changes currency, an older version, a malformed definition,
/// a price for an unknown fund, a non-pricing day or a date not yet reached, a price that
/// is not above zero, and a second price for a date that does not name the one it corrects.
/// A correction is taken whether or not orders have executed at the price it replaces, and
/// those that have are left as they are for the replay.
/// Why it matters: every replica must value every order identically for ever. A price or
/// a fund's rounding rule that could change under an ID already used would make a replayed
/// log produce different units from the ones customers were told they hold.
#[test]
fn funds_and_prices_are_immutable_once_active() {
    let (mut c, policy) = invested();
    let today = start();
    let activate = |c: &mut Cluster, f: FundDefinition| invest(c, "config", InvestCommand::ActivateFund(Box::new(f)));
    activate(&mut c, fund("growth")).unwrap();
    let mut other = fund("growth");
    other.name = "Another name".to_string();
    assert_eq!(code(activate(&mut c, other)), "invalid_fund");
    let variations: Vec<Box<dyn Fn(&mut FundDefinition)>> = vec![
        Box::new(|f| f.id = fund_id("a/b")),
        Box::new(|f| f.content_hash.clear()),
        Box::new(|f| f.version = 0),
        Box::new(|f| f.price_decimals = 9),
        Box::new(|f| f.cut_off = "2pm".to_string()),
        Box::new(|f| f.buy_spread_bps = 10_001),
        Box::new(|f| {
            f.version = 2;
            f.currency = ic_money::Currency::AUD;
        }),
    ];
    for vary in variations {
        let mut f = fund("growth");
        vary(&mut f);
        assert_eq!(code(activate(&mut c, f.clone())), "invalid_fund", "{f:?}");
    }
    let mut third = fund("growth");
    third.version = 3;
    third.content_hash = "hash-growth-3".to_string();
    activate(&mut c, third).unwrap();
    let mut second = fund("growth");
    second.version = 2;
    second.content_hash = "hash-growth-2".to_string();
    assert_eq!(code(activate(&mut c, second)), "invalid_fund");
    assert_eq!(
        code(invest(&mut c, "unit-pricing", InvestCommand::ActivateFund(Box::new(fund("x"))))),
        "caller_not_allowed"
    );

    let strike = |c: &mut Cluster, p: UnitPrice| invest(c, "unit-pricing", InvestCommand::ActivateUnitPrice(Box::new(p)));
    assert_eq!(code(strike(&mut c, price("p-x", "property", today, "1", "1"))), "unknown_fund");
    assert_eq!(code(strike(&mut c, price("p x", "growth", today, "1", "1"))), "invalid_unit_price");
    assert_eq!(
        code(strike(&mut c, price("p-future", "growth", d(2027, 11, 16), "1", "1"))),
        "invalid_unit_price"
    );
    assert_eq!(code(strike(&mut c, price("p-zero", "growth", today, "0", "1"))), "invalid_unit_price");
    assert_eq!(code(strike(&mut c, price("p-neg", "growth", today, "1", "-1"))), "invalid_unit_price");
    let mut second_version = price("p-v2", "growth", today, "1", "1");
    second_version.version = 2;
    assert_eq!(code(strike(&mut c, second_version)), "invalid_unit_price");
    c.end_of_day().unwrap();
    c.end_of_day().unwrap();
    assert_eq!(code(strike(&mut c, price("p-hol", "growth", d(2027, 11, 17), "1", "1"))), "invalid_unit_price");

    place(
        &mut c,
        order("buy-1", &policy, "growth", Direction::Buy, OrderAmount::Cash { minor: 100_00 }, d(2027, 11, 18)),
    )
    .unwrap();
    c.end_of_day().unwrap();
    let thursday = d(2027, 11, 18);
    strike(&mut c, price("p-1", "growth", thursday, "2.0000", "2.0000")).unwrap();
    strike(&mut c, price("p-1", "growth", thursday, "2.0000", "2.0000")).unwrap();
    assert_eq!(code(strike(&mut c, price("p-1", "growth", thursday, "2.5000", "2.0000"))), "invalid_unit_price");
    assert_eq!(
        code(strike(&mut c, price("p-again", "growth", thursday, "2.5000", "2.5000"))),
        "invalid_unit_price"
    );
    // A correction names what it corrects and is taken while nothing has executed.
    let mut corrected = price("p-2", "growth", thursday, "2.5000", "2.5000");
    corrected.version = 2;
    corrected.supersedes = Some("p-wrong".to_string());
    assert_eq!(code(strike(&mut c, corrected.clone())), "invalid_unit_price");
    corrected.supersedes = Some("p-1".to_string());
    strike(&mut c, corrected).unwrap();
    assert_eq!(c.state(0).investment.price_for(&fund_id("growth"), thursday).unwrap().price_id, "p-2");
    execute(&mut c, "growth", thursday).unwrap();
    assert_eq!(units(&c, &policy, "growth"), 40_000_000);
    // After execution a further correction is still taken; the executed order waits for the replay.
    let mut late = price("p-3", "growth", thursday, "2.6000", "2.6000");
    late.version = 3;
    late.supersedes = Some("p-2".to_string());
    strike(&mut c, late).unwrap();
    assert_eq!(c.state(0).investment.price_for(&fund_id("growth"), thursday).unwrap().price_id, "p-3");
    assert_eq!(c.state(0).investment.replayable().len(), 1);
    assert_eq!(units(&c, &policy, "growth"), 40_000_000);
    let seen = invest_events(&mut c);
    assert!(seen
        .iter()
        .any(|e| matches!(e, InvestEvent::UnitPriceActivated { price_id, pending_orders: 1, .. } if price_id == "p-1")));
    assert!(seen.iter().any(|e| matches!(e, InvestEvent::FundActivated { version: 3, .. })));
    assert!(c.state(0).investment.price("p-1").is_some());
}

/// Purpose: the investment ledger answers what its readers need (units on issue with what
/// is still pending, cash waiting and since when, one order, one policy's holdings, and
/// what the execution run has left to do), and its reconciliation names each way the book
/// and the ledger can disagree: a fund's units on issue against the units policies hold,
/// its cash against its executed orders, a pending order missing from the execution index,
/// and buy orders counting on more cash than is waiting.
/// Why it matters: the unit pricing module blocks a price when units on issue disagree
/// with the administrator, so a wrong figure here stops a fund or lets a bad price through;
/// finance ages the waiting cash at the close; and the reconciliation is what stops the
/// line before a customer is told they hold units the ledger does not have.
#[test]
fn the_book_answers_its_readers_and_reconciles_to_the_ledger() {
    let (mut c, policy) = invested();
    let monday = start();
    place(
        &mut c,
        order("buy-1", &policy, "growth", Direction::Buy, OrderAmount::Cash { minor: 1_000_00 }, monday),
    )
    .unwrap();
    let tuesday = d(2027, 11, 16);
    place(
        &mut c,
        order("buy-2", &policy, "growth", Direction::Buy, OrderAmount::Cash { minor: 500_00 }, tuesday),
    )
    .unwrap();
    let ask = |c: &Cluster, question: Question| {
        let state = c.state(0);
        state.investment.answer(&state.ledger, &state.policies, &question)
    };
    assert_eq!(ask(&c, Question::Executable), Reply::Executable(vec![]));
    c.end_of_day().unwrap();
    invest(
        &mut c,
        "unit-pricing",
        InvestCommand::ActivateUnitPrice(Box::new(price("p-1", "growth", monday, "2.0000", "2.0000"))),
    )
    .unwrap();
    let Reply::Executable(todo) = ask(&c, Question::Executable) else {
        unreachable!()
    };
    assert_eq!(todo.len(), 1);
    assert_eq!(
        (
            todo[0].fund.clone(),
            todo[0].pricing_date,
            todo[0].price_id.as_str(),
            todo[0].pending,
            todo[0].first.as_str()
        ),
        (fund_id("growth"), monday, "p-1", 1, "buy-1")
    );
    // Before execution Monday is not yet "executed through".
    let Reply::UnitsOnIssue(before) = ask(&c, Question::UnitsOnIssue { fund: fund_id("growth") }) else {
        unreachable!()
    };
    assert_eq!(
        (before.units_micro, before.executed_through, before.pending.clone()),
        (0, None, BTreeMap::from([(monday, 1), (tuesday, 1)]))
    );
    execute(&mut c, "growth", monday).unwrap();
    let Reply::UnitsOnIssue(after) = ask(&c, Question::UnitsOnIssue { fund: fund_id("growth") }) else {
        unreachable!()
    };
    assert_eq!(
        (after.units_micro, after.executed_through, after.pending),
        (500_000_000, Some(monday), BTreeMap::from([(tuesday, 1)]))
    );

    let Reply::WaitingCash(waiting) = ask(&c, Question::WaitingCash) else {
        unreachable!()
    };
    assert_eq!(waiting.len(), 1);
    assert_eq!(
        (waiting[0].policy.clone(), waiting[0].amount_minor, waiting[0].currency, waiting[0].since),
        (policy.clone(), 4_000_00, NZD, monday)
    );
    let Reply::Order(Some(found)) = ask(&c, Question::Order { order_id: "buy-2".to_string() }) else {
        unreachable!()
    };
    assert_eq!(found.state, OrderState::Pending);
    assert_eq!(
        ask(
            &c,
            Question::Order {
                order_id: "nothing".to_string()
            }
        ),
        Reply::Order(None)
    );
    let Reply::Holdings(Some(holdings)) = ask(&c, Question::Holdings { policy: policy.clone() }) else {
        unreachable!()
    };
    assert_eq!(holdings.units_micro, BTreeMap::from([("growth".to_string(), 500_000_000)]));
    assert_eq!(
        (holdings.pending_cash_minor, holdings.pending_orders.len(), holdings.currency),
        (4_000_00, 1, NZD)
    );
    assert_eq!(
        ask(
            &c,
            Question::Holdings {
                policy: PolicyId::from("nobody")
            }
        ),
        Reply::Holdings(None)
    );

    // The book as it stands reconciles. Each way it could disagree with the ledger is named.
    let state = c.state(0);
    assert!(state.investment.breaks(&state.ledger, &state.policies).is_empty());
    let tampered = |edit: &dyn Fn(&mut serde_json::Value)| -> Vec<&'static str> {
        let mut book = serde_json::to_value(&state.investment).unwrap();
        edit(&mut book);
        let book: InvestmentBook = serde_json::from_value(book).unwrap();
        book.breaks(&state.ledger, &state.policies).into_iter().map(|(check, _)| check).collect()
    };
    assert_eq!(
        tampered(&|b| b["orders"]["buy-1"]["state"]["executed"]["cash_minor"] = 999.into()),
        ["fund_cash_position"]
    );
    assert_eq!(tampered(&|b| b["pending"] = serde_json::json!([])), ["unit_orders_pending"]);
    assert_eq!(
        tampered(&|b| b["orders"]["buy-2"]["order"]["amount"]["cash"]["minor"] = 4_000_01.into()),
        ["pending_investment_cash"]
    );
    // An executed order filed under the wrong fund puts both funds' cash out.
    assert_eq!(
        tampered(&|b| {
            let growth = b["funds"]["growth"].clone();
            b["funds"]["bond"] = growth;
            b["orders"]["buy-1"]["order"]["fund"] = "bond".into();
        }),
        ["fund_cash_position", "fund_cash_position"]
    );
    // The cancelled order's cash goes back to suspense, and nothing is left waiting.
    invest(&mut c, "policy-servicing", InvestCommand::CancelOrder { order_id: "buy-2".to_string() }).unwrap();
    let back = InvestCommand::AllocateCash {
        policy: policy.clone(),
        reference: "back".to_string(),
        amount_minor: -4_000_00,
    };
    invest(&mut c, "policy-servicing", back).unwrap();
    assert_eq!(ask(&c, Question::WaitingCash), Reply::WaitingCash(vec![]));
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}

fn correction(id: &str, date: NaiveDate, at: &str, version: u32, supersedes: &str) -> UnitPrice {
    let mut p = price(id, "growth", date, at, at);
    p.version = version;
    p.supersedes = Some(supersedes.to_string());
    p
}

fn replay(c: &mut Cluster, superseded: &str, limit: u32, materiality_minor: i64) -> Result<(), Reject> {
    invest(
        c,
        "unit-orders",
        InvestCommand::ReplayOrders {
            superseded: superseded.to_string(),
            limit,
            materiality_minor,
        },
    )
}

/// What each replayed order was given, as (order, units, cash, borne by the insurer, compensated).
fn replayed(c: &mut Cluster) -> Vec<(String, i64, i64, i64, bool)> {
    invest_events(c)
        .into_iter()
        .filter_map(|e| match e {
            InvestEvent::UnitOrderReplayed {
                order_id,
                units_delta_micro,
                cash_delta_minor,
                borne_minor,
                compensated,
                ..
            } => Some((order_id, units_delta_micro, cash_delta_minor, borne_minor, compensated)),
            _ => None,
        })
        .collect()
}

/// Purpose: when a price is corrected after orders executed at it, the replay puts each of
/// those orders on the corrected price. The side the customer fixed stays fixed: a buy
/// keeps its cash and its units change, a sell of units keeps its units and its cash
/// changes, a sell for cash keeps its cash and its units change. A policy that is owed gets
/// units, or cash once it is no longer in force. A policy that was given too much has units
/// taken back only as far as it still holds them and never has cash taken back; the rest
/// the insurer pays the fund through the pricing error account. A difference below the
/// materiality threshold is recorded and not paid. The run works in chunks, a repeated
/// chunk does nothing, the adjustment carries the original pricing date as its value date,
/// and the book reconciles to the ledger after every step.
/// Why it matters: a pricing error is the event a unit-linked insurer is judged on. Each
/// customer must end where the right price would have put them, no customer may be sent a
/// bill for the insurer's mistake, and the other investors in the fund must not carry the
/// cost. A replay that paid twice on a retry, or left the fund's cash unexplained, would
/// turn one error into a second one.
#[test]
fn a_corrected_price_is_replayed_over_the_orders_executed_at_it() {
    let (mut c, policy) = invested();
    let monday = start();
    let tuesday = d(2027, 11, 16);
    let strike = |c: &mut Cluster, p: UnitPrice| invest(c, "unit-pricing", InvestCommand::ActivateUnitPrice(Box::new(p)));
    let to_replay = |c: &Cluster| {
        let state = c.state(0);
        match state.investment.answer(&state.ledger, &state.policies, &Question::ToReplay) {
            Reply::ToReplay(list) => list,
            other => panic!("{other:?}"),
        }
    };
    let position = |c: &Cluster| (units(c, &policy, "growth"), pending_cash(c, &policy));
    let borne = |c: &Cluster| c.state(0).ledger.balance(&accounts::pricing_error(&fund_id("growth"), NZD).id);

    // Monday: two buys at 2.0000 give 500 and 1,000 units.
    for (id, minor) in [("buy-1", 1_000_00), ("buy-2", 2_000_00)] {
        place(&mut c, order(id, &policy, "growth", Direction::Buy, OrderAmount::Cash { minor }, monday)).unwrap();
    }
    c.end_of_day().unwrap();
    strike(&mut c, price("p-m", "growth", monday, "2.0000", "2.0000")).unwrap();
    execute(&mut c, "growth", monday).unwrap();
    // Tuesday: a sell for NZD 300 takes 150 units and a sell of 200 units fetches NZD 400.
    place(
        &mut c,
        order("sell-c", &policy, "growth", Direction::Sell, OrderAmount::Cash { minor: 300_00 }, tuesday),
    )
    .unwrap();
    place(
        &mut c,
        order("sell-u", &policy, "growth", Direction::Sell, OrderAmount::Units { micro: 200_000_000 }, tuesday),
    )
    .unwrap();
    c.end_of_day().unwrap();
    strike(&mut c, price("p-t", "growth", tuesday, "2.0000", "2.0000")).unwrap();
    execute(&mut c, "growth", tuesday).unwrap();
    assert_eq!(position(&c), (1_150_000_000, 2_700_00));
    assert!(to_replay(&c).is_empty());
    invest_events(&mut c);

    // Tuesday's price should have been 2.5000: the policy sold too cheaply.
    assert_eq!(code(replay(&mut c, "p-t", 100, 0)), "not_superseded");
    strike(&mut c, correction("p-t2", tuesday, "2.5000", 2, "p-t")).unwrap();
    assert_eq!(
        to_replay(&c),
        [Replayable {
            superseded: "p-t".to_string(),
            fund: fund_id("growth"),
            pricing_date: tuesday,
            orders: 2,
            compensation_minimum_minor: 0,
        }]
    );
    assert_eq!(code(replay(&mut c, "p-t", 0, 0)), "invalid_chunk");
    assert_eq!(code(replay(&mut c, "p-t", 100, -1)), "invalid_chunk");
    assert_eq!(code(replay(&mut c, "p-none", 100, 0)), "unknown_price");
    let wrong_caller = InvestCommand::ReplayOrders {
        superseded: "p-t".to_string(),
        limit: 100,
        materiality_minor: 0,
    };
    assert_eq!(code(invest(&mut c, "policy-servicing", wrong_caller)), "caller_not_allowed");
    // One order per chunk. The sell for cash needed only 120 units, so 30 come back.
    replay(&mut c, "p-t", 1, 0).unwrap();
    assert_eq!(position(&c), (1_180_000_000, 2_700_00));
    assert_eq!(to_replay(&c)[0].orders, 1);
    // The sell of units should have fetched NZD 500, so NZD 100 more is paid.
    let run = Command::Investment(Box::new(InvestCommand::ReplayOrders {
        superseded: "p-t".to_string(),
        limit: 100,
        materiality_minor: 0,
    }));
    let applied = c.submit_keyed(0, "replay-p-t-2", "unit-orders", run);
    assert_eq!(applied.result, Ok(()));
    assert_eq!(applied.entries.len(), 1);
    let entry = &applied.entries[0].body;
    assert_eq!((entry.kind.as_str(), entry.value_date), ("unit_price_correction", tuesday));
    assert_eq!(
        (
            entry.detail["order_id"].as_str(),
            entry.detail["from_price"].as_str(),
            entry.detail["price_id"].as_str()
        ),
        ("sell-u", "p-t", "p-t2")
    );
    assert_eq!(position(&c), (1_180_000_000, 2_800_00));
    assert!(to_replay(&c).is_empty());
    // A repeated chunk finds nothing left on the old price.
    replay(&mut c, "p-t", 100, 0).unwrap();
    assert_eq!(position(&c), (1_180_000_000, 2_800_00));
    assert_eq!(
        replayed(&mut c),
        [("sell-c".to_string(), 30_000_000, 0, 0, true), ("sell-u".to_string(), 0, 100_00, 0, true)]
    );
    assert_eq!(borne(&c), 0);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // Corrected back to 2.0000: now the policy has been given too much. The 30 units are
    // taken back; the NZD 100 is not, and the insurer pays it to the fund.
    strike(&mut c, correction("p-t3", tuesday, "2.0000", 3, "p-t2")).unwrap();
    replay(&mut c, "p-t2", 100, 0).unwrap();
    assert_eq!(position(&c), (1_150_000_000, 2_800_00));
    assert_eq!(
        replayed(&mut c),
        [("sell-c".to_string(), -30_000_000, 0, 0, true), ("sell-u".to_string(), 0, 0, 100_00, true)]
    );
    assert_eq!(borne(&c), 100_00);
    let stands = c.state(0).investment.order("sell-u").unwrap();
    assert_eq!(
        stands.state,
        OrderState::Executed {
            price_id: "p-t3".to_string(),
            cash_minor: 400_00,
            units_micro: 200_000_000,
            on: c.business_date().unwrap(),
        }
    );
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // Monday's price moves by a tenth of a cent: about 50 cents and a dollar, under a
    // threshold of NZD 5. Recorded, not paid, and the orders move to the new price.
    strike(&mut c, correction("p-m2", monday, "2.0010", 2, "p-m")).unwrap();
    replay(&mut c, "p-m", 100, 5_00).unwrap();
    assert_eq!(position(&c), (1_150_000_000, 2_800_00));
    assert_eq!(replayed(&mut c), [("buy-1".to_string(), 0, 0, 0, false), ("buy-2".to_string(), 0, 0, 0, false)]);
    assert!(to_replay(&c).is_empty());

    // Monday's price should have been 20.0000: the buys were worth 50 and 100 units, not
    // 500 and 1,000. The first gives back 450. The second owes 900 and only 700 are left,
    // so the insurer pays the fund for the other 200 at 20.0000.
    strike(&mut c, correction("p-m3", monday, "20.0000", 3, "p-m2")).unwrap();
    replay(&mut c, "p-m2", 100, 0).unwrap();
    assert_eq!(position(&c), (0, 2_800_00));
    assert_eq!(
        replayed(&mut c),
        [
            ("buy-1".to_string(), -450_000_000, 0, 0, true),
            ("buy-2".to_string(), -700_000_000, 0, 4_000_00, true)
        ]
    );
    assert_eq!(borne(&c), 4_100_00);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // The policy is cancelled, then Monday's price is corrected to 10.0000: the buys were
    // worth 100 and 200 units. A policyholder who has left is paid the value, not units.
    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: policy.clone() }).unwrap();
    strike(&mut c, correction("p-m4", monday, "10.0000", 4, "p-m3")).unwrap();
    replay(&mut c, "p-m3", 100, 0).unwrap();
    assert_eq!(position(&c), (0, 4_300_00));
    assert_eq!(
        replayed(&mut c),
        [("buy-1".to_string(), 0, 500_00, 0, true), ("buy-2".to_string(), 0, 1_000_00, 0, true)]
    );
    assert_eq!(borne(&c), 4_100_00);
    assert!(to_replay(&c).is_empty());
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}

fn payout(c: &mut Cluster, policy: &PolicyId, id: &str, amount_minor: i64, reason: PayoutReason) -> Result<(), Reject> {
    invest(
        c,
        "policy-servicing",
        InvestCommand::RaisePayout {
            policy: policy.clone(),
            payout: id.to_string(),
            amount_minor,
            reason,
        },
    )
}

fn pay(c: &mut Cluster, policy: &PolicyId, payment: &str, minor: i64) -> Result<(), Reject> {
    let settle = Command::SettlePayable {
        policy: policy.clone(),
        payment: payment.to_string(),
        payable: PayableKind::Benefit,
        refund: None,
        amount: Money::new(minor, NZD),
        bank_account: "nz-main-benefits".to_string(),
    };
    c.submit(0, "payments", settle)
}

/// Purpose: unit-linked money leaves through the policy's benefit payable. Cash no buy
/// order is counting on is raised from pending investment cash to the payable under a
/// payout ID, once however often it is sent; the payments module can settle exactly that
/// payout, for exactly that amount, under that ID and nothing else; a payment the bank
/// sends back makes the payout owed again, and it can be paid only after it is given a
/// new payment reference. A policy that is no longer in force can still be paid what it
/// is owed. The claims reconciliation of benefit payable stays clean throughout.
/// Why it matters: a withdrawal is a customer's own money and it must leave once, to the
/// bank, for the amount asked. Cash paid straight out of the investment ledger would skip
/// the payee checks and the bank reconciliation every other payment goes through, and a
/// payable that any payment reference could draw on would let one withdrawal be paid twice.
#[test]
fn a_withdrawal_is_paid_through_benefit_payable() {
    let (mut c, policy) = invested();
    let payable = |c: &Cluster| c.state(0).ledger.natural_balance(&accounts::benefit_payable(&policy, NZD).id);
    let state_of = |c: &Cluster, id: &str| c.state(0).policies[&policy].payouts[id].state;
    let reinstruct = |c: &mut Cluster, id: &str, payment: &str| {
        invest(
            c,
            "policy-servicing",
            InvestCommand::ReinstructPayout {
                policy: policy.clone(),
                payout: id.to_string(),
                payment: payment.to_string(),
            },
        )
    };
    // NZD 1,000 of the 5,000 is waiting for a buy order.
    place(
        &mut c,
        order("buy-1", &policy, "growth", Direction::Buy, OrderAmount::Cash { minor: 1_000_00 }, start()),
    )
    .unwrap();
    assert_eq!(code(payout(&mut c, &policy, "wd-1", 4_000_01, PayoutReason::Withdrawal)), "cash_committed");
    assert_eq!(code(payout(&mut c, &policy, "wd-1", 0, PayoutReason::Withdrawal)), "invalid_payout");
    assert_eq!(code(payout(&mut c, &policy, "wd 1", 500_00, PayoutReason::Withdrawal)), "invalid_payout");
    let by_payments = InvestCommand::RaisePayout {
        policy: policy.clone(),
        payout: "wd-1".to_string(),
        amount_minor: 500_00,
        reason: PayoutReason::Withdrawal,
    };
    assert_eq!(code(invest(&mut c, "payments", by_payments)), "caller_not_allowed");

    let raise = Command::Investment(Box::new(InvestCommand::RaisePayout {
        policy: policy.clone(),
        payout: "wd-1".to_string(),
        amount_minor: 500_00,
        reason: PayoutReason::Withdrawal,
    }));
    let applied = c.submit_keyed(0, "raise-wd-1", "policy-servicing", raise);
    assert_eq!(applied.result, Ok(()));
    let entry = &applied.entries[0].body;
    assert_eq!((entry.kind.as_str(), entry.detail["payout"].as_str()), ("investment_payout_raised", "wd-1"));
    assert!(entry.postings.iter().all(|p| p.dims.posting_type.as_deref() == Some("unit_withdrawal")));
    assert_eq!((pending_cash(&c, &policy), payable(&c)), (4_500_00, 500_00));
    // Sent again it changes nothing; with other content it is refused.
    payout(&mut c, &policy, "wd-1", 500_00, PayoutReason::Withdrawal).unwrap();
    assert_eq!((pending_cash(&c, &policy), payable(&c)), (4_500_00, 500_00));
    assert_eq!(code(payout(&mut c, &policy, "wd-1", 600_00, PayoutReason::Withdrawal)), "duplicate_payout");
    assert_eq!(code(payout(&mut c, &policy, "wd-1", 500_00, PayoutReason::Surrender)), "duplicate_payout");
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // Only that payout, for that amount, can be settled.
    assert_eq!(code(pay(&mut c, &policy, "wd-1", 400_00)), "payment_not_instructed");
    assert_eq!(code(pay(&mut c, &policy, "someone-else", 500_00)), "payment_not_instructed");
    assert_eq!(code(reinstruct(&mut c, "wd-1", "wd-1b")), "payout_not_returned");
    reinstruct(&mut c, "wd-1", "wd-1").unwrap();
    pay(&mut c, &policy, "wd-1", 500_00).unwrap();
    assert_eq!((payable(&c), state_of(&c, "wd-1")), (0, PayoutState::Settled));
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    // Its reference is spent: a second payout cannot take it.
    assert_eq!(code(payout(&mut c, &policy, "wd-1", 500_00, PayoutReason::Withdrawal)), "accepted");
    assert_eq!(pending_cash(&c, &policy), 4_500_00);

    // The bank sends it back. It is owed again, and payable only under a new reference.
    c.submit(
        0,
        "payments",
        Command::ReturnPayment {
            policy: policy.clone(),
            payment: "wd-1".to_string(),
        },
    )
    .unwrap();
    assert_eq!((payable(&c), state_of(&c, "wd-1")), (500_00, PayoutState::Returned));
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    assert_eq!(code(pay(&mut c, &policy, "wd-1b", 500_00)), "payment_not_instructed");
    assert_eq!(code(reinstruct(&mut c, "wd-none", "wd-1b")), "unknown_payout");
    assert_eq!(code(reinstruct(&mut c, "wd-1", "wd-1")), "duplicate_payment");
    assert_eq!(code(reinstruct(&mut c, "wd-1", "wd 1b")), "duplicate_payment");
    reinstruct(&mut c, "wd-1", "wd-1b").unwrap();
    // A new payout cannot take a reference another payout is to be paid under.
    assert_eq!(code(payout(&mut c, &policy, "wd-1b", 10_00, PayoutReason::Withdrawal)), "duplicate_payout");
    pay(&mut c, &policy, "wd-1b", 500_00).unwrap();
    assert_eq!((payable(&c), state_of(&c, "wd-1")), (0, PayoutState::Settled));

    // A policy that has ended is still paid what it holds in cash.
    invest(&mut c, "policy-servicing", InvestCommand::CancelOrder { order_id: "buy-1".to_string() }).unwrap();
    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: policy.clone() }).unwrap();
    payout(&mut c, &policy, "sur-1", 4_500_00, PayoutReason::Surrender).unwrap();
    assert_eq!((pending_cash(&c, &policy), payable(&c)), (0, 4_500_00));
    let Reply::WaitingCash(waiting) = c.state(0).investment.answer(&c.state(0).ledger, &c.state(0).policies, &Question::WaitingCash) else {
        unreachable!()
    };
    assert!(waiting.is_empty());
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    let seen = invest_events(&mut c);
    let has = |want: &dyn Fn(&InvestEvent) -> bool| seen.iter().any(want);
    assert!(has(
        &|e| matches!(e, InvestEvent::PayoutRaised { payout, amount_minor: 500_00, reason: PayoutReason::Withdrawal, .. } if payout == "wd-1")
    ));
    assert!(has(
        &|e| matches!(e, InvestEvent::PayoutReturned { payout, payment, amount_minor: 500_00, .. } if payout == "wd-1" && payment == "wd-1")
    ));
    assert!(has(
        &|e| matches!(e, InvestEvent::PayoutReinstructed { payout, payment, .. } if payout == "wd-1" && payment == "wd-1b")
    ));
    assert!(has(&|e| matches!(
        e,
        InvestEvent::PayoutRaised {
            reason: PayoutReason::Surrender,
            ..
        }
    )));
    assert_eq!(seen.iter().filter(|e| matches!(e, InvestEvent::PayoutRaised { .. })).count(), 2);
    // Each request to pay names the policy's owner, so it can be paid to no one else.
    let owner = c.state(0).policies[&policy].policyholder.clone();
    assert!(seen.iter().all(|e| match e {
        InvestEvent::PayoutRaised { policyholder, .. } | InvestEvent::PayoutReinstructed { policyholder, .. } => *policyholder == owner,
        _ => true,
    }));
}
