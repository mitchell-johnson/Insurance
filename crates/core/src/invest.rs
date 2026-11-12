//! The investment ledger: funds, unit prices, unit orders and their execution.
//!
//! The core holds each policy's units per fund, holds pending orders and
//! converts between cash and units at an activated price. It never looks a
//! price up and never works one out: a price arrives as a logged command from
//! the unit pricing module, so every replica values identically.
//!
//! Pricing is forward. An order is replicated state, not a posting: it waits,
//! with its cash in the policy's pending investment cash, until the price for
//! its pricing date is activated and the order is executed. Money is never in
//! the market without units and never in units without a price.
//!
//! An executed order posts one entry that balances in currency and in the
//! fund's units separately. Buying NZD 1,000 at 2.0000: debit the policy's
//! pending cash 1,000, credit the fund's cash position 1,000, debit the
//! fund's unit position 500 units, credit the policy's unit account 500
//! units. Selling is the reverse. The cash side of an order is exact and the
//! unit side is rounded to micro-units by the fund's rule, so no residual is
//! left to carry.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate, Weekday};
use ic_ledger::{Dimensions, Ledger};
use ic_money::{Currency, DecStr, FundId, Money, RoundingMode, UnitQty};
use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::postings::PostingSet;
use crate::PolicyId;

/// The most orders one execution command works through.
pub const MAX_EXECUTION_CHUNK: u32 = 500;
/// How far ahead an order's pricing date may be.
const MAX_PRICING_DAYS_AHEAD: i64 = 400;

/// A fund, as versioned configuration. Activated on every partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundDefinition {
    pub id: FundId,
    pub version: u32,
    /// Hash of the definition's source, set where it is loaded.
    pub content_hash: String,
    pub name: String,
    /// Frozen once the fund is in use, like its unit type.
    pub currency: Currency,
    /// The time of day, as HH:MM in the fund's place of business, after which
    /// an order takes the next pricing date. Applied at the edge, where the
    /// clock is; the core only checks the date it is given.
    pub cut_off: String,
    #[serde(default)]
    pub buy_spread_bps: u32,
    #[serde(default)]
    pub sell_spread_bps: u32,
    pub price_decimals: u32,
    /// How a fraction of a micro-unit is rounded when an order executes.
    pub unit_rounding: RoundingMode,
    /// Weekdays the fund does not price. Weekends never price.
    #[serde(default)]
    pub non_pricing_days: BTreeSet<NaiveDate>,
    pub open_to_new_money: bool,
    /// Settings of the unit pricing module (tolerances, benchmark). The core
    /// keeps them with the version and does not read them.
    #[serde(default)]
    pub pricing: BTreeMap<String, String>,
}

impl FundDefinition {
    /// Whether the fund strikes a price for this date.
    pub fn prices_on(&self, date: NaiveDate) -> bool {
        !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) && !self.non_pricing_days.contains(&date)
    }
}

/// One fund's price for one pricing date, as the unit pricing module struck
/// and approved it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitPrice {
    pub price_id: String,
    pub fund: FundId,
    pub pricing_date: NaiveDate,
    /// 1 for the first price of a date. A higher version corrects an error.
    pub version: u32,
    /// What a buyer pays for a unit.
    pub buy: DecStr,
    /// What a seller receives for a unit.
    pub sell: DecStr,
    pub mid: DecStr,
    /// Hash of the valuation and units on issue the price was struck from.
    pub inputs_hash: String,
    /// The price this one corrects. Required above version 1.
    #[serde(default)]
    pub supersedes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Buy,
    Sell,
}

/// How much an order is for. A buy is always for cash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderAmount {
    Cash {
        minor: i64,
    },
    Units {
        micro: i64,
    },
    /// Everything the policy holds in the fund when the order executes.
    AllUnits,
}

/// Why the order was raised. Carried on the entry and the events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderKind {
    PremiumAllocation,
    Withdrawal,
    RegularWithdrawal,
    SwitchOut,
    SwitchIn,
    FeeDeduction,
    InsuranceCharge,
    TaxDeduction,
    FeeRebate,
    Surrender,
    Claim,
}

impl OrderKind {
    fn name(self) -> &'static str {
        match self {
            OrderKind::PremiumAllocation => "premium_allocation",
            OrderKind::Withdrawal => "withdrawal",
            OrderKind::RegularWithdrawal => "regular_withdrawal",
            OrderKind::SwitchOut => "switch_out",
            OrderKind::SwitchIn => "switch_in",
            OrderKind::FeeDeduction => "fee_deduction",
            OrderKind::InsuranceCharge => "insurance_charge",
            OrderKind::TaxDeduction => "tax_deduction",
            OrderKind::FeeRebate => "fee_rebate",
            OrderKind::Surrender => "surrender",
            OrderKind::Claim => "claim",
        }
    }
}

/// An order as a caller places it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewOrder {
    /// The caller's own ID for the order. Accepted once.
    pub order_id: String,
    pub policy: PolicyId,
    pub fund: FundId,
    pub direction: Direction,
    pub amount: OrderAmount,
    pub kind: OrderKind,
    /// Assigned at the edge from the fund's cut-off and carried here.
    pub pricing_date: NaiveDate,
    /// For a sell that is the first half of a switch: the fund the proceeds
    /// buy. The buy is placed when the sell executes, for the same pricing
    /// date where that fund prices then and has not yet struck, and
    /// otherwise for its next pricing date.
    #[serde(default)]
    pub switch_to: Option<FundId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Pending,
    Executed {
        price_id: String,
        cash_minor: i64,
        units_micro: i64,
        on: NaiveDate,
    },
    Cancelled {
        on: NaiveDate,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitOrder {
    pub order: NewOrder,
    pub placed_on: NaiveDate,
    pub state: OrderState,
}

/// Everything about investment that is not a ledger balance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvestmentBook {
    /// Fund versions by fund ID. The highest version is the one in force.
    #[serde(default)]
    funds: BTreeMap<String, BTreeMap<u32, FundDefinition>>,
    #[serde(default)]
    prices: BTreeMap<String, UnitPrice>,
    /// `fund/date` -> the price in force for that fund and pricing date.
    #[serde(default)]
    in_force: BTreeMap<String, String>,
    #[serde(default)]
    orders: BTreeMap<String, UnitOrder>,
    /// `fund/date/order` for every pending order, in execution order.
    #[serde(default)]
    pending: BTreeSet<String>,
    /// The day each policy's pending investment cash last rose from zero.
    #[serde(default)]
    waiting_since: BTreeMap<String, NaiveDate>,
}

fn slot(fund: &FundId, date: NaiveDate) -> String {
    format!("{}/{date}", fund.0)
}

fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
}

/// Units on issue in one fund on this partition, for the control the unit
/// pricing module runs before it strikes a price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitsOnIssue {
    pub fund: FundId,
    pub units_micro: i128,
    /// The latest pricing date with a price in force and no order pending.
    pub executed_through: Option<NaiveDate>,
    /// Orders still pending, counted by pricing date.
    pub pending: BTreeMap<NaiveDate, u32>,
}

/// One policy's cash waiting for a price or to be paid out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingCash {
    pub policy: PolicyId,
    pub amount_minor: i64,
    pub currency: Currency,
    pub since: NaiveDate,
}

impl InvestmentBook {
    pub fn fund(&self, id: &FundId) -> Option<&FundDefinition> {
        self.funds.get(&id.0).and_then(|versions| versions.values().next_back())
    }

    pub fn price(&self, price_id: &str) -> Option<&UnitPrice> {
        self.prices.get(price_id)
    }

    /// The price in force for a fund and pricing date.
    pub fn price_for(&self, fund: &FundId, date: NaiveDate) -> Option<&UnitPrice> {
        self.in_force.get(&slot(fund, date)).and_then(|id| self.prices.get(id))
    }

    pub fn order(&self, order_id: &str) -> Option<&UnitOrder> {
        self.orders.get(order_id)
    }

    /// Pending orders of one fund and pricing date, in execution order.
    pub fn pending_for(&self, fund: &FundId, date: NaiveDate) -> Vec<&UnitOrder> {
        let prefix = format!("{}/", slot(fund, date));
        self.pending
            .range(prefix.clone()..)
            .take_while(|k| k.starts_with(&prefix))
            .filter_map(|k| self.orders.get(&k[prefix.len()..]))
            .collect()
    }

    fn pending_of<'a>(&'a self, policy: &'a PolicyId) -> impl Iterator<Item = &'a UnitOrder> + 'a {
        self.orders
            .values()
            .filter(move |o| o.state == OrderState::Pending && o.order.policy == *policy)
    }

    /// Whether the policy has anything in the investment ledger that a move
    /// to another partition would have to carry.
    pub fn has_orders(&self, policy: &PolicyId) -> bool {
        self.pending_of(policy).next().is_some()
    }

    /// Whether the policy has units, pending investment cash or pending
    /// orders: what a move to another partition would have to carry.
    pub fn holds_anything(&self, ledger: &Ledger, policy: &Policy) -> bool {
        self.has_orders(&policy.id)
            || ledger.balance(&accounts::pending_investment_cash(&policy.id, policy.currency).id) != 0
            || self
                .funds
                .keys()
                .any(|fund| ledger.balance(&accounts::unit_account(&policy.id, &FundId(fund.clone())).id) != 0)
    }

    pub fn units_on_issue(&self, ledger: &Ledger, fund: &FundId) -> UnitsOnIssue {
        let prefix = format!("{}/", fund.0);
        let mut pending: BTreeMap<NaiveDate, u32> = BTreeMap::new();
        for key in self.pending.range(prefix.clone()..).take_while(|k| k.starts_with(&prefix)) {
            if let Some(date) = key[prefix.len()..].split('/').next().and_then(|d| d.parse().ok()) {
                *pending.entry(date).or_default() += 1;
            }
        }
        let executed_through = self
            .in_force
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter_map(|(k, _)| k[prefix.len()..].parse::<NaiveDate>().ok())
            .filter(|date| !pending.contains_key(date))
            .max();
        UnitsOnIssue {
            fund: fund.clone(),
            units_micro: ledger.balance(&accounts::fund_unit_position(fund).id),
            executed_through,
            pending,
        }
    }

    /// Every policy with pending investment cash, and since when.
    pub fn waiting_cash(&self, ledger: &Ledger, policies: &BTreeMap<PolicyId, Policy>) -> Vec<WaitingCash> {
        self.waiting_since
            .iter()
            .filter_map(|(id, since)| {
                let policy = policies.get(&PolicyId::from(id.as_str()))?;
                let held = -ledger.balance(&accounts::pending_investment_cash(&policy.id, policy.currency).id);
                (held != 0).then(|| WaitingCash {
                    policy: policy.id.clone(),
                    amount_minor: held as i64,
                    currency: policy.currency,
                    since: *since,
                })
            })
            .collect()
    }

    /// Keep the held-since date in step with the pending cash balance.
    fn note_waiting(&mut self, ledger: &Ledger, policy: &Policy, today: NaiveDate) {
        let held = ledger.balance(&accounts::pending_investment_cash(&policy.id, policy.currency).id);
        if held == 0 {
            self.waiting_since.remove(&policy.id.0);
        } else {
            self.waiting_since.entry(policy.id.0.clone()).or_insert(today);
        }
    }
}

/// What happened in the investment ledger, for the unit pricing module, the
/// finance ledger and statements. No personal data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestEvent {
    FundActivated {
        fund: FundId,
        version: u32,
        content_hash: String,
    },
    UnitPriceActivated {
        price_id: String,
        fund: FundId,
        pricing_date: NaiveDate,
        version: u32,
        supersedes: Option<String>,
        /// Orders waiting for this price on this partition.
        pending_orders: u32,
    },
    /// Cash moved between the policy's suspense and its pending investment
    /// cash. Negative when it was moved back.
    CashAllocated {
        policy: PolicyId,
        reference: String,
        amount_minor: i64,
    },
    UnitOrderPlaced {
        order_id: String,
        policy: PolicyId,
        fund: FundId,
        direction: Direction,
        amount: OrderAmount,
        kind: OrderKind,
        pricing_date: NaiveDate,
    },
    UnitOrderCancelled {
        order_id: String,
        policy: PolicyId,
        fund: FundId,
        pricing_date: NaiveDate,
    },
    UnitOrderExecuted {
        order_id: String,
        policy: PolicyId,
        fund: FundId,
        direction: Direction,
        kind: OrderKind,
        cash_minor: i64,
        units_micro: i64,
        price_id: String,
        pricing_date: NaiveDate,
    },
}

/// The investment ledger's commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvestCommand {
    ActivateFund(Box<FundDefinition>),
    ActivateUnitPrice(Box<UnitPrice>),
    /// Move money between a policy's suspense and its pending investment
    /// cash: positive to make it available to buy orders, negative to return
    /// cash no pending buy order is counting on.
    AllocateCash {
        policy: PolicyId,
        reference: String,
        amount_minor: i64,
    },
    PlaceOrder(Box<NewOrder>),
    CancelOrder {
        order_id: String,
    },
    /// Execute up to `limit` of the orders pending for this fund and pricing
    /// date at the price in force. Sent again until nothing is pending.
    ExecuteOrders {
        fund: FundId,
        pricing_date: NaiveDate,
        limit: u32,
    },
}

impl InvestCommand {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            InvestCommand::ActivateFund(_) => &["config"],
            InvestCommand::ActivateUnitPrice(_) => &["unit-pricing"],
            InvestCommand::AllocateCash { .. } => &["policy-servicing", "cash-matching"],
            InvestCommand::PlaceOrder(_) | InvestCommand::CancelOrder { .. } => &["policy-servicing", "claims", "fees"],
            InvestCommand::ExecuteOrders { .. } => &["unit-orders"],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            InvestCommand::ActivateFund(_) => "activate_fund",
            InvestCommand::ActivateUnitPrice(_) => "activate_unit_price",
            InvestCommand::AllocateCash { .. } => "allocate_investment_cash",
            InvestCommand::PlaceOrder(_) => "place_unit_order",
            InvestCommand::CancelOrder { .. } => "cancel_unit_order",
            InvestCommand::ExecuteOrders { .. } => "execute_unit_orders",
        }
    }

    /// The policy the command is about, where it names one.
    pub fn policy<'a>(&'a self, book: &'a InvestmentBook) -> Option<&'a PolicyId> {
        match self {
            InvestCommand::AllocateCash { policy, .. } => Some(policy),
            InvestCommand::PlaceOrder(order) => Some(&order.policy),
            InvestCommand::CancelOrder { order_id } => book.orders.get(order_id).map(|o| &o.order.policy),
            _ => None,
        }
    }
}

fn bad(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

pub(crate) fn activate_fund(book: &mut InvestmentBook, ctx: &mut Ctx<'_>, fund: FundDefinition) -> Result<(), Reject> {
    let invalid = |m: &str| bad("invalid_fund", m);
    if !plain(&fund.id.0) || fund.id.0.contains('/') {
        return Err(invalid("a fund is named by a plain identifier"));
    }
    if fund.content_hash.is_empty() {
        return Err(invalid("the fund carries no content hash; load it from its source"));
    }
    if fund.version == 0 || fund.price_decimals > 8 {
        return Err(invalid("versions start at 1 and a price has at most 8 decimals"));
    }
    let cut_off_ok = chrono::NaiveTime::parse_from_str(&fund.cut_off, "%H:%M").is_ok();
    if !cut_off_ok || fund.buy_spread_bps > 10_000 || fund.sell_spread_bps > 10_000 {
        return Err(invalid("the cut-off is HH:MM and a spread is at most 10000 basis points"));
    }
    let versions = book.funds.entry(fund.id.0.clone()).or_default();
    if let Some(active) = versions.get(&fund.version) {
        // The same content again is a repeated request.
        return if *active == fund {
            Ok(())
        } else {
            Err(invalid("this version of the fund is already active with other content"))
        };
    }
    if let Some((latest, current)) = versions.iter().next_back() {
        if fund.version < *latest {
            return Err(invalid("a later version of the fund is already active"));
        }
        if current.currency != fund.currency {
            return Err(invalid("a fund's currency is frozen once the fund is in use"));
        }
    }
    ctx.emit(crate::Event::Investment(InvestEvent::FundActivated {
        fund: fund.id.clone(),
        version: fund.version,
        content_hash: fund.content_hash.clone(),
    }));
    versions.insert(fund.version, fund);
    Ok(())
}

fn positive(price: &DecStr) -> bool {
    price.get() > ic_money::dec_i128(0)
}

pub(crate) fn activate_price(book: &mut InvestmentBook, ctx: &mut Ctx<'_>, price: UnitPrice) -> Result<(), Reject> {
    let invalid = |m: String| bad("invalid_unit_price", m);
    if !plain(&price.price_id) || !plain(&price.inputs_hash) {
        return Err(invalid("the price ID and the inputs hash must be plain identifiers".to_string()));
    }
    if let Some(active) = book.prices.get(&price.price_id) {
        return if *active == price {
            Ok(())
        } else {
            Err(invalid(format!("price {} is already active with other content", price.price_id)))
        };
    }
    let Some(fund) = book.fund(&price.fund) else {
        return Err(bad("unknown_fund", format!("fund {} is not active on this partition", price.fund.0)));
    };
    if !fund.prices_on(price.pricing_date) {
        return Err(invalid(format!("fund {} does not price on {}", price.fund.0, price.pricing_date)));
    }
    if price.pricing_date > ctx.date {
        return Err(invalid("a price cannot be struck for a date that has not come".to_string()));
    }
    if !(positive(&price.buy) && positive(&price.sell) && positive(&price.mid)) {
        return Err(invalid("a unit price is above zero".to_string()));
    }
    let key = slot(&price.fund, price.pricing_date);
    let current = book.in_force.get(&key).and_then(|id| book.prices.get(id));
    match (current, &price.supersedes) {
        (None, None) if price.version == 1 => {}
        (None, _) => return Err(invalid("the first price of a date is version 1 and supersedes nothing".to_string())),
        (Some(current), Some(superseded)) if *superseded == current.price_id && price.version > current.version => {
            // Orders already executed at the superseded price are replayed
            // at this one; that is not built yet, so a correction is taken
            // only while none has executed.
            let executed = book
                .orders
                .values()
                .any(|o| matches!(&o.state, OrderState::Executed { price_id, .. } if *price_id == current.price_id));
            if executed {
                return Err(bad(
                    "replay_required",
                    format!("orders have executed at {}; a correction after execution needs the replay", current.price_id),
                ));
            }
        }
        (Some(current), _) => {
            return Err(invalid(format!(
                "{} is in force for this fund and date; a correction names it and carries a higher version",
                current.price_id
            )))
        }
    }
    let pending_orders = book.pending_for(&price.fund, price.pricing_date).len() as u32;
    ctx.emit(crate::Event::Investment(InvestEvent::UnitPriceActivated {
        price_id: price.price_id.clone(),
        fund: price.fund.clone(),
        pricing_date: price.pricing_date,
        version: price.version,
        supersedes: price.supersedes.clone(),
        pending_orders,
    }));
    book.in_force.insert(key, price.price_id.clone());
    book.prices.insert(price.price_id.clone(), price);
    Ok(())
}

fn investable(policy: &Policy) -> Result<(), Reject> {
    match policy.status {
        PolicyStatus::InForce => Ok(()),
        _ => Err(bad("not_in_force", "the policy is not in force")),
    }
}

/// Cash the policy's pending buy orders are counting on.
fn committed_cash(book: &InvestmentBook, policy: &PolicyId) -> i128 {
    book.pending_of(policy)
        .filter(|o| o.order.direction == Direction::Buy)
        .map(|o| match o.order.amount {
            OrderAmount::Cash { minor } => minor as i128,
            _ => 0,
        })
        .sum()
}

pub(crate) fn allocate_cash(
    ledger: &mut Ledger,
    book: &mut InvestmentBook,
    policy: &Policy,
    ctx: &mut Ctx<'_>,
    reference: String,
    amount_minor: i64,
) -> Result<(), Reject> {
    if !plain(&reference) || amount_minor == 0 {
        return Err(bad("invalid_allocation", "an allocation has a plain reference and an amount other than zero"));
    }
    investable(policy)?;
    let c = policy.currency;
    let suspense = accounts::policy_suspense(&policy.id, c);
    let pending = accounts::pending_investment_cash(&policy.id, c);
    let amount = amount_minor as i128;
    if amount > 0 && -ledger.balance(&suspense.id) < amount {
        return Err(bad("insufficient_suspense", "the policy's suspense does not hold that much"));
    }
    if amount < 0 && -ledger.balance(&pending.id) - committed_cash(book, &policy.id) < -amount {
        return Err(bad("cash_committed", "that cash is waiting for a pending buy order or is not there"));
    }
    let mut set = PostingSet::new();
    set.debit(suspense, amount, Dimensions::default());
    set.credit(pending, amount, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    let detail = BTreeMap::from([("reference".to_string(), reference.clone())]);
    ctx.post(ledger, "investment_cash_allocated", Some(policy), ctx.date, postings, detail)?;
    book.note_waiting(ledger, policy, ctx.date);
    ctx.emit(crate::Event::Investment(InvestEvent::CashAllocated {
        policy: policy.id.clone(),
        reference,
        amount_minor,
    }));
    Ok(())
}

pub(crate) fn place_order(ledger: &Ledger, book: &mut InvestmentBook, policy: &Policy, ctx: &mut Ctx<'_>, order: NewOrder) -> Result<(), Reject> {
    let invalid = |m: &str| bad("invalid_order", m);
    if !plain(&order.order_id) || order.order_id.contains('/') {
        return Err(invalid("an order is named by a plain identifier"));
    }
    if let Some(existing) = book.orders.get(&order.order_id) {
        return if existing.order == order {
            Ok(())
        } else {
            Err(bad("duplicate_order", format!("{} was placed with other content", order.order_id)))
        };
    }
    investable(policy)?;
    let Some(fund) = book.fund(&order.fund) else {
        return Err(bad("unknown_fund", format!("fund {} is not active on this partition", order.fund.0)));
    };
    if fund.currency != policy.currency {
        return Err(invalid("the fund is not in the policy's currency"));
    }
    if order.pricing_date < ctx.date || (order.pricing_date - ctx.date).num_days() > MAX_PRICING_DAYS_AHEAD {
        return Err(invalid("the pricing date is today or later, and within 400 days"));
    }
    if !fund.prices_on(order.pricing_date) {
        return Err(invalid("the fund does not price on that date; the order takes its next pricing date"));
    }
    if book.price_for(&order.fund, order.pricing_date).is_some() {
        return Err(bad(
            "price_struck",
            "the price for that date is already active; the order takes the next pricing date",
        ));
    }
    match (order.direction, order.amount) {
        (Direction::Buy, OrderAmount::Cash { minor }) if minor > 0 => {
            if !fund.open_to_new_money && !matches!(order.kind, OrderKind::SwitchIn | OrderKind::FeeRebate) {
                return Err(bad("fund_closed", "the fund is closed to new money"));
            }
            let held = -ledger.balance(&accounts::pending_investment_cash(&policy.id, policy.currency).id);
            if held - committed_cash(book, &policy.id) < minor as i128 {
                return Err(bad(
                    "insufficient_pending_cash",
                    "the policy's pending investment cash does not cover the order",
                ));
            }
            if order.switch_to.is_some() {
                return Err(invalid("only a sell can be the first half of a switch"));
            }
        }
        (Direction::Buy, _) => return Err(invalid("a buy is for a cash amount above zero")),
        (Direction::Sell, amount) => {
            let held = -ledger.balance(&accounts::unit_account(&policy.id, &order.fund).id);
            let mut promised: i128 = 0;
            for other in book
                .pending_of(&policy.id)
                .filter(|o| o.order.fund == order.fund && o.order.direction == Direction::Sell)
            {
                match other.order.amount {
                    OrderAmount::Units { micro } => promised += micro as i128,
                    OrderAmount::AllUnits => return Err(bad("units_committed", "an order to sell everything in this fund is already pending")),
                    OrderAmount::Cash { .. } => {}
                }
            }
            match amount {
                OrderAmount::Units { micro } if micro > 0 && held - promised >= micro as i128 => {}
                OrderAmount::Units { .. } => return Err(bad("insufficient_units", "the policy does not hold that many uncommitted units")),
                OrderAmount::Cash { minor } if minor > 0 && held - promised > 0 => {}
                OrderAmount::Cash { .. } => return Err(bad("insufficient_units", "a sell for cash needs an amount above zero and units to sell")),
                OrderAmount::AllUnits if held > 0 && promised == 0 => {}
                OrderAmount::AllUnits => return Err(bad("insufficient_units", "there is nothing uncommitted to sell")),
            }
            if let Some(target) = &order.switch_to {
                let known = book.fund(target).is_some_and(|t| t.currency == policy.currency);
                if !known || *target == order.fund {
                    return Err(invalid("a switch goes to another active fund in the policy's currency"));
                }
            }
        }
    }
    book.pending.insert(format!("{}/{}", slot(&order.fund, order.pricing_date), order.order_id));
    ctx.emit(crate::Event::Investment(InvestEvent::UnitOrderPlaced {
        order_id: order.order_id.clone(),
        policy: order.policy.clone(),
        fund: order.fund.clone(),
        direction: order.direction,
        amount: order.amount,
        kind: order.kind,
        pricing_date: order.pricing_date,
    }));
    book.orders.insert(
        order.order_id.clone(),
        UnitOrder {
            order,
            placed_on: ctx.date,
            state: OrderState::Pending,
        },
    );
    Ok(())
}

pub(crate) fn cancel_order(book: &mut InvestmentBook, ctx: &mut Ctx<'_>, order_id: &str) -> Result<(), Reject> {
    let Some(held) = book.orders.get_mut(order_id) else {
        return Err(bad("unknown_order", format!("{order_id} was not placed on this partition")));
    };
    match held.state {
        OrderState::Cancelled { .. } => return Ok(()),
        OrderState::Executed { .. } => return Err(bad("order_executed", "an order cannot be cancelled once it has executed")),
        OrderState::Pending => {}
    }
    let key = slot(&held.order.fund, held.order.pricing_date);
    // Once the price is active the order is as good as executed: the fund
    // has been told the day's flows.
    if book.in_force.contains_key(&key) {
        return Err(bad("price_struck", "the order's price is active; it can no longer be cancelled"));
    }
    held.state = OrderState::Cancelled { on: ctx.date };
    book.pending.remove(&format!("{key}/{order_id}"));
    ctx.emit(crate::Event::Investment(InvestEvent::UnitOrderCancelled {
        order_id: order_id.to_string(),
        policy: held.order.policy.clone(),
        fund: held.order.fund.clone(),
        pricing_date: held.order.pricing_date,
    }));
    Ok(())
}

/// The next date from `from` on which the fund prices and has not struck.
fn next_open_date(book: &InvestmentBook, fund: &FundDefinition, from: NaiveDate) -> NaiveDate {
    let mut date = from;
    for _ in 0..MAX_PRICING_DAYS_AHEAD {
        if fund.prices_on(date) && book.price_for(&fund.id, date).is_none() {
            break;
        }
        date = date.succ_opt().unwrap_or(date);
    }
    date
}

/// Execute one pending order at the price in force for its date. Returns
/// false, and writes nothing, when the order can no longer be carried out as
/// placed (the cash or the units are gone); it is then cancelled.
fn execute_one(
    ledger: &mut Ledger,
    book: &mut InvestmentBook,
    policy: &Policy,
    ctx: &mut Ctx<'_>,
    order_id: &str,
    price: &UnitPrice,
    rounding: RoundingMode,
) -> Result<bool, Reject> {
    let Some(order) = book.orders.get(order_id).map(|o| o.order.clone()) else {
        return Ok(false);
    };
    let c = policy.currency;
    let pending_cash = accounts::pending_investment_cash(&policy.id, c);
    let unit_account = accounts::unit_account(&policy.id, &order.fund);
    let held_units = -ledger.balance(&unit_account.id);
    let (cash_minor, units_micro) = match (order.direction, order.amount) {
        (Direction::Buy, OrderAmount::Cash { minor }) => {
            let units = UnitQty::for_cash(Money::new(minor, c), price.buy.get(), order.fund.clone(), rounding)?;
            (minor, units.micro)
        }
        (Direction::Sell, OrderAmount::Units { micro }) => {
            let cash = UnitQty::new(micro, order.fund.clone()).value_at(price.sell.get(), c, RoundingMode::HalfEven)?;
            (cash.minor, micro)
        }
        (Direction::Sell, OrderAmount::AllUnits) => {
            let micro = i64::try_from(held_units).map_err(|_| bad("overflow", "holding too large"))?;
            let cash = UnitQty::new(micro, order.fund.clone()).value_at(price.sell.get(), c, RoundingMode::HalfEven)?;
            (cash.minor, micro)
        }
        (Direction::Sell, OrderAmount::Cash { minor }) => {
            // Sell what the cash amount needs; if the holding is worth
            // less, sell all of it for what it fetches.
            let wanted = UnitQty::for_cash(Money::new(minor, c), price.sell.get(), order.fund.clone(), RoundingMode::Up)?;
            if (wanted.micro as i128) <= held_units {
                (minor, wanted.micro)
            } else {
                let micro = i64::try_from(held_units).map_err(|_| bad("overflow", "holding too large"))?;
                let cash = UnitQty::new(micro, order.fund.clone()).value_at(price.sell.get(), c, RoundingMode::HalfEven)?;
                (cash.minor, micro)
            }
        }
        // Refused when placed.
        (Direction::Buy, _) => return Ok(false),
    };
    let short = match order.direction {
        Direction::Buy => -ledger.balance(&pending_cash.id) < cash_minor as i128 || units_micro <= 0,
        Direction::Sell => held_units < units_micro as i128 || units_micro <= 0,
    };
    if short {
        return Ok(false);
    }
    // Debits positive. A buy takes cash from the policy and gives it units.
    let sign: i128 = if order.direction == Direction::Buy { 1 } else { -1 };
    let mut set = PostingSet::new();
    set.debit(pending_cash, sign * cash_minor as i128, Dimensions::default());
    set.credit(accounts::fund_cash_position(&order.fund, c), sign * cash_minor as i128, Dimensions::default());
    set.debit(accounts::fund_unit_position(&order.fund), sign * units_micro as i128, Dimensions::default());
    set.credit(unit_account, sign * units_micro as i128, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    let (used, used_price) = match order.direction {
        Direction::Buy => ("buy", &price.buy),
        Direction::Sell => ("sell", &price.sell),
    };
    let detail = BTreeMap::from([
        ("order_id".to_string(), order.order_id.clone()),
        ("order_kind".to_string(), order.kind.name().to_string()),
        ("price_id".to_string(), price.price_id.clone()),
        ("price".to_string(), used_price.as_str().to_string()),
        ("price_side".to_string(), used.to_string()),
        ("pricing_date".to_string(), price.pricing_date.to_string()),
    ]);
    ctx.post(ledger, "unit_order_executed", Some(policy), ctx.date, postings, detail)?;
    if let Some(held) = book.orders.get_mut(order_id) {
        held.state = OrderState::Executed {
            price_id: price.price_id.clone(),
            cash_minor,
            units_micro,
            on: ctx.date,
        };
    }
    book.note_waiting(ledger, policy, ctx.date);
    ctx.emit(crate::Event::Investment(InvestEvent::UnitOrderExecuted {
        order_id: order.order_id.clone(),
        policy: policy.id.clone(),
        fund: order.fund.clone(),
        direction: order.direction,
        kind: order.kind,
        cash_minor,
        units_micro,
        price_id: price.price_id.clone(),
        pricing_date: price.pricing_date,
    }));
    // The second half of a switch: the proceeds buy the other fund.
    if let Some(target) = order.switch_to.as_ref().and_then(|t| book.fund(t)).cloned() {
        let buy = NewOrder {
            order_id: format!("{}.in", order.order_id),
            policy: policy.id.clone(),
            fund: target.id.clone(),
            direction: Direction::Buy,
            amount: OrderAmount::Cash { minor: cash_minor },
            kind: OrderKind::SwitchIn,
            pricing_date: next_open_date(book, &target, price.pricing_date.max(ctx.date)),
            switch_to: None,
        };
        place_order(ledger, book, policy, ctx, buy)?;
    }
    Ok(true)
}

/// Execute a chunk of the orders pending for a fund and pricing date. An
/// order whose cash or units are no longer there is cancelled and reported,
/// and does not stop the rest.
pub(crate) fn execute_orders(
    ledger: &mut Ledger,
    book: &mut InvestmentBook,
    policies: &BTreeMap<PolicyId, Policy>,
    ctx: &mut Ctx<'_>,
    fund: &FundId,
    pricing_date: NaiveDate,
    limit: u32,
) -> Result<(), Reject> {
    if limit == 0 || limit > MAX_EXECUTION_CHUNK {
        return Err(bad("invalid_chunk", format!("a chunk is between 1 and {MAX_EXECUTION_CHUNK} orders")));
    }
    let Some(price) = book.price_for(fund, pricing_date).cloned() else {
        return Err(bad("no_price", format!("no price is active for {} on {pricing_date}", fund.0)));
    };
    let rounding = book.fund(fund).map(|f| f.unit_rounding).unwrap_or(RoundingMode::HalfEven);
    let prefix = format!("{}/", slot(fund, pricing_date));
    let chunk: Vec<String> = book
        .pending
        .range(prefix.clone()..)
        .take_while(|k| k.starts_with(&prefix))
        .take(limit as usize)
        .cloned()
        .collect();
    for key in chunk {
        let order_id = &key[prefix.len()..];
        let Some(policy_id) = book.orders.get(order_id).map(|o| o.order.policy.clone()) else {
            book.pending.remove(&key);
            continue;
        };
        // A policy in flight to another partition keeps its orders pending
        // here until it lands or comes back.
        let Some(policy) = policies.get(&policy_id).filter(|p| !p.is_frozen() && p.status != PolicyStatus::Transferred) else {
            continue;
        };
        let executed = execute_one(ledger, book, policy, ctx, order_id, &price, rounding)?;
        book.pending.remove(&key);
        if !executed {
            if let Some(held) = book.orders.get_mut(order_id) {
                held.state = OrderState::Cancelled { on: ctx.date };
                ctx.emit(crate::Event::Investment(InvestEvent::UnitOrderCancelled {
                    order_id: order_id.to_string(),
                    policy: policy_id,
                    fund: fund.clone(),
                    pricing_date,
                }));
            }
        }
    }
    Ok(())
}
