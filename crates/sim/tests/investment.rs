//! The investment ledger: funds, unit prices, forward-priced unit orders,
//! their execution and switches.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::event::Event;
use ic_core::invest::{Direction, FundDefinition, InvestCommand, InvestEvent, NewOrder, OrderAmount, OrderKind, OrderState, UnitPrice};
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
/// other fund for the cash it fetched: for the same pricing date where that fund has not
/// yet struck, and for its next pricing date where it has. A sell for a cash amount sells
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
    // Bond struck Tuesday's price before growth did, so the switch's buy takes bond's next
    // pricing date: Thursday, since Wednesday is a holiday for these funds.
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
    // 500 units at 3.00 = 1,500.00 to bond; 300.00 needs exactly 100 units.
    assert_eq!(units(&c, &policy, "growth"), 2_000_000_000 - 500_000_000 - 100_000_000);
    let second_half = c.state(0).investment.order("sw-1.in").expect("the switch placed its buy").clone();
    assert_eq!(second_half.order.amount, OrderAmount::Cash { minor: 1_500_00 });
    assert_eq!(
        (second_half.order.fund.clone(), second_half.order.pricing_date, second_half.order.kind),
        (fund_id("bond"), d(2027, 11, 18), OrderKind::SwitchIn)
    );
    assert_eq!(second_half.state, OrderState::Pending);
    assert_eq!(pending_cash(&c, &policy), 1_000_00 + 1_500_00 + 300_00);
    // The switch's cash is committed to its buy: it cannot be returned to suspense.
    let back = InvestCommand::AllocateCash {
        policy: policy.clone(),
        reference: "back".to_string(),
        amount_minor: -1_300_01,
    };
    assert_eq!(code(invest(&mut c, "policy-servicing", back)), "cash_committed");

    // Thursday: sell everything in growth twice over. The first takes it all for what it
    // fetches; the cash order asks for more than is left and is cancelled; bond's buy executes.
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
        InvestCommand::ActivateUnitPrice(Box::new(price("b-thu", "bond", thursday, "1.2500", "1.2500"))),
    )
    .unwrap();
    c.take_events();
    execute(&mut c, "growth", thursday).unwrap();
    execute(&mut c, "bond", thursday).unwrap();
    // 1,400 units at 1.00: worth less than 9,999.00, so all of it is sold for 1,400.00.
    assert_eq!(units(&c, &policy, "growth"), 0);
    assert_eq!(units(&c, &policy, "bond"), 1_200_000_000);
    assert!(matches!(c.state(0).investment.order("z-cash").unwrap().state, OrderState::Cancelled { .. }));
    let events = invest_events(&mut c);
    let executed: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            InvestEvent::UnitOrderExecuted { order_id, .. } => Some(order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(executed, ["a-cash", "sw-1.in"]);
    assert!(events
        .iter()
        .any(|e| matches!(e, InvestEvent::UnitOrderCancelled { order_id, .. } if order_id == "z-cash")));
    assert_eq!(pending_cash(&c, &policy), 1_000_00 + 300_00 + 1_400_00);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    // The cash has been waiting since the day it was first allocated.
    let waiting = c.state(0).investment.waiting_cash(&c.state(0).ledger, &c.state(0).policies);
    assert_eq!((waiting.len(), waiting[0].amount_minor, waiting[0].since), (1, 2_700_00, monday));
}

/// Purpose: a fund or a price is activated once and is then immutable: the same content
/// again is accepted and changes nothing, other content under the same version or ID is
/// refused, as are a fund that changes currency, an older version, a malformed definition,
/// a price for an unknown fund, a non-pricing day or a date not yet reached, a price that
/// is not above zero, and a second price for a date that does not name the one it corrects.
/// A correction is taken while no order has executed at the price it replaces.
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
    // After execution a further correction needs the replay, which says so.
    let mut late = price("p-3", "growth", thursday, "2.6000", "2.6000");
    late.version = 3;
    late.supersedes = Some("p-2".to_string());
    assert_eq!(code(strike(&mut c, late)), "replay_required");
    let seen = invest_events(&mut c);
    assert!(seen
        .iter()
        .any(|e| matches!(e, InvestEvent::UnitPriceActivated { price_id, pending_orders: 1, .. } if price_id == "p-1")));
    assert!(seen.iter().any(|e| matches!(e, InvestEvent::FundActivated { version: 3, .. })));
    assert!(c.state(0).investment.price("p-1").is_some());
}
