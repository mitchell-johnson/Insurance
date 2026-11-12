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
    /// `price/order` for every order executed at a price, so that the
    /// orders a corrected price must replay are found without a scan.
    #[serde(default)]
    executed_at: BTreeSet<String>,
    /// What each fund's cash position has been paid, net, to put price
    /// errors right. Part of what the position reconciles to.
    #[serde(default)]
    remediation_minor: BTreeMap<String, i128>,
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
    /// The latest pricing date with a price in force and nothing pending
    /// for it or for any earlier date.
    pub executed_through: Option<NaiveDate>,
    /// Orders still pending, counted by pricing date. A switch into the
    /// fund whose sell has not executed counts against the date its buy
    /// will take.
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
        // A switch into this fund whose sell has not executed yet will place
        // a buy here, for the first date from the sell's own on which this
        // fund prices. It counts against that date now, so the date is not
        // taken as finished while units can still arrive for it.
        if let Some(definition) = self.fund(fund) {
            for order in self.orders.values() {
                if order.state == OrderState::Pending && order.order.switch_to.as_ref() == Some(fund) {
                    *pending.entry(next_pricing_date(definition, order.order.pricing_date)).or_default() += 1;
                }
            }
        }
        // Finished means struck, with nothing pending for that date or any
        // date before it.
        let first_pending = pending.keys().next().copied();
        let executed_through = self
            .in_force
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter_map(|(k, _)| k[prefix.len()..].parse::<NaiveDate>().ok())
            .filter(|date| first_pending.is_none_or(|first| *date < first))
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
        /// The definition itself, so a module that keeps funds (unit
        /// pricing) can follow the core from the event feed and never holds
        /// a version the core does not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        definition: Option<Box<FundDefinition>>,
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
    CashAllocated { policy: PolicyId, reference: String, amount_minor: i64 },
    /// Pending investment cash was raised to benefit payable, to be paid
    /// under the payout's ID as the payment reference.
    PayoutRaised {
        policy: PolicyId,
        /// The policy's owner: the only party the payout may be paid to.
        policyholder: crate::PartyId,
        payout: String,
        amount_minor: i64,
        reason: PayoutReason,
    },
    /// The bank sent a payout's payment back: it is owed again and needs a
    /// new payment reference before it can be paid.
    PayoutReturned {
        policy: PolicyId,
        payout: String,
        payment: String,
        amount_minor: i64,
    },
    /// A returned payout is to be paid again under a new reference.
    PayoutReinstructed {
        policy: PolicyId,
        policyholder: crate::PartyId,
        payout: String,
        payment: String,
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
    /// An executed order was put on a corrected price. The deltas are what
    /// the policy gained: units added to its holding and cash added to its
    /// pending investment cash, either of which may be negative. `borne_minor`
    /// is what the insurer paid the fund through the pricing error account
    /// where the policy could not or should not make the fund whole.
    UnitOrderReplayed {
        order_id: String,
        policy: PolicyId,
        fund: FundId,
        pricing_date: NaiveDate,
        from_price: String,
        to_price: String,
        units_delta_micro: i64,
        cash_delta_minor: i64,
        borne_minor: i64,
        /// False where the difference was under the materiality threshold
        /// and nothing was posted.
        compensated: bool,
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
    /// Raise pending investment cash that no buy order is counting on to
    /// the policy's benefit payable, to be paid out. The payments module
    /// settles it with the payout's ID as the payment reference.
    RaisePayout {
        policy: PolicyId,
        payout: String,
        amount_minor: i64,
        reason: PayoutReason,
    },
    /// Give a payout the bank sent back a new payment reference.
    ReinstructPayout {
        policy: PolicyId,
        payout: String,
        payment: String,
    },
    /// Execute up to `limit` of the orders pending for this fund and pricing
    /// date at the price in force. Sent again until nothing is pending.
    ExecuteOrders {
        fund: FundId,
        pricing_date: NaiveDate,
        limit: u32,
    },
    /// Put up to `limit` of the orders executed at a superseded price on the
    /// price that corrected it. Sent again until none is left. A difference
    /// worth less than `materiality_minor` at the corrected price is recorded
    /// and not compensated.
    ReplayOrders {
        superseded: String,
        limit: u32,
        materiality_minor: i64,
    },
}

impl InvestCommand {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            InvestCommand::ActivateFund(_) => &["config"],
            InvestCommand::ActivateUnitPrice(_) => &["unit-pricing"],
            InvestCommand::AllocateCash { .. } => &["policy-servicing", "cash-matching"],
            InvestCommand::PlaceOrder(_) | InvestCommand::CancelOrder { .. } => &["policy-servicing", "claims", "fees"],
            InvestCommand::RaisePayout { .. } | InvestCommand::ReinstructPayout { .. } => &["policy-servicing", "claims"],
            InvestCommand::ExecuteOrders { .. } | InvestCommand::ReplayOrders { .. } => &["unit-orders"],
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            InvestCommand::ActivateFund(_) => "activate_fund",
            InvestCommand::ActivateUnitPrice(_) => "activate_unit_price",
            InvestCommand::AllocateCash { .. } => "allocate_investment_cash",
            InvestCommand::PlaceOrder(_) => "place_unit_order",
            InvestCommand::CancelOrder { .. } => "cancel_unit_order",
            InvestCommand::RaisePayout { .. } => "raise_investment_payout",
            InvestCommand::ReinstructPayout { .. } => "reinstruct_investment_payout",
            InvestCommand::ExecuteOrders { .. } => "execute_unit_orders",
            InvestCommand::ReplayOrders { .. } => "replay_unit_orders",
        }
    }

    /// The policy the command is about, where it names one.
    pub fn policy<'a>(&'a self, book: &'a InvestmentBook) -> Option<&'a PolicyId> {
        match self {
            InvestCommand::AllocateCash { policy, .. } | InvestCommand::RaisePayout { policy, .. } | InvestCommand::ReinstructPayout { policy, .. } => {
                Some(policy)
            }
            InvestCommand::PlaceOrder(order) => Some(&order.policy),
            InvestCommand::CancelOrder { order_id } => book.orders.get(order_id).map(|o| &o.order.policy),
            _ => None,
        }
    }
}

fn bad(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

/// What is wrong with a fund definition taken on its own, if anything.
fn fund_fault(fund: &FundDefinition) -> Option<&'static str> {
    if !plain(&fund.id.0) || fund.id.0.contains('/') {
        return Some("a fund is named by a plain identifier");
    }
    if fund.content_hash.is_empty() {
        return Some("the fund carries no content hash; load it from its source");
    }
    if fund.version == 0 || fund.price_decimals > 8 {
        return Some("versions start at 1 and a price has at most 8 decimals");
    }
    let cut_off_ok = chrono::NaiveTime::parse_from_str(&fund.cut_off, "%H:%M").is_ok();
    if !cut_off_ok || fund.buy_spread_bps > 10_000 || fund.sell_spread_bps > 10_000 {
        return Some("the cut-off is HH:MM and a spread is at most 10000 basis points");
    }
    None
}

/// Load a fund definition from its Starlark source. The module defines
/// `FUND` with the `fund(...)` helper. The content hash covers the source
/// and the helper, so the same file always gives the same definition and a
/// changed file cannot pass as a version already active.
pub fn load_fund(file: &str, source: &str) -> Result<FundDefinition, ic_product::LoadError> {
    let (mut value, hash) = ic_product::load_fund_source(file, source)?;
    if let Some(fields) = value.as_object_mut() {
        fields.insert("content_hash".to_string(), hash.into());
    }
    let fund: FundDefinition = serde_json::from_value(value).map_err(|e| ic_product::LoadError::Schema {
        file: file.to_string(),
        error: e.to_string(),
    })?;
    match fund_fault(&fund) {
        None => Ok(fund),
        Some(fault) => Err(ic_product::LoadError::Invalid {
            file: file.to_string(),
            error: fault.to_string(),
        }),
    }
}

pub(crate) fn activate_fund(book: &mut InvestmentBook, ctx: &mut Ctx<'_>, fund: FundDefinition) -> Result<(), Reject> {
    let invalid = |m: &str| bad("invalid_fund", m);
    if let Some(fault) = fund_fault(&fund) {
        return Err(invalid(fault));
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
        definition: Some(Box::new(fund.clone())),
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
            // Orders already executed at the superseded price stay as they
            // are until the replay reaches them (`ReplayOrders`).
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

/// Why unit-linked money is leaving. It is the posting type on the payable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutReason {
    Withdrawal,
    Surrender,
    /// Cash owed after a price error to a policyholder who has left.
    PriceRemediation,
}

impl PayoutReason {
    pub fn posting_type(self) -> &'static str {
        match self {
            PayoutReason::Withdrawal => "unit_withdrawal",
            PayoutReason::Surrender => "unit_surrender",
            PayoutReason::PriceRemediation => "unit_price_remediation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutState {
    /// In benefit payable, waiting for the bank.
    Owed,
    Settled,
    /// Sent back by the bank: in benefit payable again, with no payment
    /// reference it can be paid under until it is reinstructed.
    Returned,
}

/// Unit-linked money raised to benefit payable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payout {
    pub amount_minor: i64,
    pub reason: PayoutReason,
    /// The payment reference it is, or was last, to be paid under.
    pub payment: String,
    pub state: PayoutState,
    pub raised_on: NaiveDate,
}

/// What the policy's benefit payable holds for payouts not yet settled.
pub(crate) fn payouts_outstanding(policy: &Policy) -> i128 {
    policy
        .payouts
        .values()
        .filter(|p| p.state != PayoutState::Settled)
        .map(|p| p.amount_minor as i128)
        .sum()
}

fn owed_under<'a>(policy: &'a mut Policy, payment: &str) -> Option<(&'a String, &'a mut Payout)> {
    policy.payouts.iter_mut().find(|(_, p)| p.payment == payment)
}

/// Whether this payment is a payout waiting for the bank, for that amount.
pub(crate) fn payout_instructed(policy: &Policy, payment: &str, amount_minor: i64) -> bool {
    policy
        .payouts
        .values()
        .any(|p| p.payment == payment && p.state == PayoutState::Owed && p.amount_minor == amount_minor)
}

/// The bank settled a benefit payment: if it paid a payout, that is done.
pub(crate) fn payout_settled(policy: &mut Policy, payment: &str) {
    if let Some((_, payout)) = owed_under(policy, payment).filter(|(_, p)| p.state == PayoutState::Owed) {
        payout.state = PayoutState::Settled;
    }
}

/// The bank sent a settled payment back: if it paid a payout, the payout
/// is owed again and servicing is told, so it can find out where to pay.
pub(crate) fn payout_returned(policy: &mut Policy, ctx: &mut Ctx<'_>, payment: &str) {
    let id = policy.id.clone();
    if let Some((payout, record)) = owed_under(policy, payment).filter(|(_, p)| p.state == PayoutState::Settled) {
        record.state = PayoutState::Returned;
        ctx.emit(crate::Event::Investment(InvestEvent::PayoutReturned {
            policy: id,
            payout: payout.clone(),
            payment: payment.to_string(),
            amount_minor: record.amount_minor,
        }));
    }
}

/// Raise pending investment cash to benefit payable. Allowed on a policy
/// that is no longer in force, since that is when the last of it is paid.
pub(crate) fn raise_payout(
    ledger: &mut Ledger,
    book: &mut InvestmentBook,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    payout: String,
    amount_minor: i64,
    reason: PayoutReason,
) -> Result<(), Reject> {
    if !plain(&payout) || amount_minor <= 0 {
        return Err(bad("invalid_payout", "a payout has a plain identifier and an amount above zero"));
    }
    if let Some(existing) = policy.payouts.get(&payout) {
        return if existing.amount_minor == amount_minor && existing.reason == reason {
            Ok(())
        } else {
            Err(bad("duplicate_payout", format!("{payout} was raised with other content")))
        };
    }
    // The payout's ID is its payment reference, so it must be free.
    if policy.payments.contains_key(&payout) || policy.payouts.values().any(|p| p.payment == payout) {
        return Err(bad("duplicate_payout", format!("{payout} is already a payment reference on this policy")));
    }
    let c = policy.currency;
    let pending = accounts::pending_investment_cash(&policy.id, c);
    if -ledger.balance(&pending.id) - committed_cash(book, &policy.id) < amount_minor as i128 {
        return Err(bad("cash_committed", "that cash is waiting for a pending buy order or is not there"));
    }
    let dims = accounts::dims(policy, None, reason.posting_type());
    let mut set = PostingSet::new();
    set.debit(pending, amount_minor as i128, dims.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), amount_minor as i128, dims);
    let postings = set.into_postings(ledger)?;
    let detail = BTreeMap::from([("payout".to_string(), payout.clone())]);
    ctx.post(ledger, "investment_payout_raised", Some(policy), ctx.date, postings, detail)?;
    policy.payouts.insert(
        payout.clone(),
        Payout {
            amount_minor,
            reason,
            payment: payout.clone(),
            state: PayoutState::Owed,
            raised_on: ctx.date,
        },
    );
    book.note_waiting(ledger, policy, ctx.date);
    ctx.emit(crate::Event::Investment(InvestEvent::PayoutRaised {
        policy: policy.id.clone(),
        policyholder: policy.policyholder.clone(),
        payout,
        amount_minor,
        reason,
    }));
    Ok(())
}

/// A payout the bank sent back is paid again under a new reference, once
/// servicing knows where the money should go.
pub(crate) fn reinstruct_payout(policy: &mut Policy, ctx: &mut Ctx<'_>, payout: &str, payment: String) -> Result<(), Reject> {
    let Some(record) = policy.payouts.get(payout) else {
        return Err(bad("unknown_payout", format!("{payout} is not a payout on this policy")));
    };
    if record.state == PayoutState::Owed && record.payment == payment {
        return Ok(());
    }
    if record.state != PayoutState::Returned {
        return Err(bad("payout_not_returned", format!("{payout} has not been sent back by the bank")));
    }
    if !plain(&payment) || policy.payments.contains_key(&payment) || policy.payouts.values().any(|p| p.payment == payment) {
        return Err(bad("duplicate_payment", format!("{payment} is not a free payment reference on this policy")));
    }
    let amount_minor = record.amount_minor;
    if let Some(record) = policy.payouts.get_mut(payout) {
        record.payment = payment.clone();
        record.state = PayoutState::Owed;
    }
    ctx.emit(crate::Event::Investment(InvestEvent::PayoutReinstructed {
        policy: policy.id.clone(),
        policyholder: policy.policyholder.clone(),
        payout: payout.to_string(),
        payment,
        amount_minor,
    }));
    Ok(())
}

pub(crate) fn place_order(ledger: &Ledger, book: &mut InvestmentBook, policy: &Policy, ctx: &mut Ctx<'_>, order: NewOrder) -> Result<(), Reject> {
    place(ledger, book, policy, ctx, order, false)
}

/// `second_half` is the buy a switch places when its sell executes. The
/// customer committed to it when the switch was placed, before either
/// fund's price was known, so it keeps the switch's pricing date even where
/// that price has since been struck.
fn place(ledger: &Ledger, book: &mut InvestmentBook, policy: &Policy, ctx: &mut Ctx<'_>, order: NewOrder, second_half: bool) -> Result<(), Reject> {
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
    if !second_half && (order.pricing_date < ctx.date || (order.pricing_date - ctx.date).num_days() > MAX_PRICING_DAYS_AHEAD) {
        return Err(invalid("the pricing date is today or later, and within 400 days"));
    }
    if !fund.prices_on(order.pricing_date) {
        return Err(invalid("the fund does not price on that date; the order takes its next pricing date"));
    }
    if !second_half && book.price_for(&order.fund, order.pricing_date).is_some() {
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

/// The first date from `from` on which the fund prices.
fn next_pricing_date(fund: &FundDefinition, from: NaiveDate) -> NaiveDate {
    let mut date = from;
    for _ in 0..MAX_PRICING_DAYS_AHEAD {
        if fund.prices_on(date) {
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
    book.executed_at.insert(format!("{}/{order_id}", price.price_id));
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
            pricing_date: next_pricing_date(&target, price.pricing_date),
            switch_to: None,
        };
        place(ledger, book, policy, ctx, buy, true)?;
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

/// Put a chunk of the orders executed at a superseded price on the price in
/// force for the same fund and date.
///
/// The side of the order the customer fixed stays fixed, and the other side
/// is worked out again at the corrected price. Where the policy is owed
/// units it gets them, or their value in cash if it is no longer in force.
/// Where it is owed cash the fund pays it. Where the policy was given too
/// much, units are taken back as far as it still holds them and cash is not
/// taken back at all; what cannot be taken back the insurer pays the fund
/// through the pricing error account, so the other investors are not left
/// short. Every adjustment is posted today with the original pricing date
/// as its value date.
pub(crate) fn replay_orders(
    ledger: &mut Ledger,
    book: &mut InvestmentBook,
    policies: &BTreeMap<PolicyId, Policy>,
    ctx: &mut Ctx<'_>,
    superseded: &str,
    limit: u32,
    materiality_minor: i64,
) -> Result<(), Reject> {
    if limit == 0 || limit > MAX_EXECUTION_CHUNK || materiality_minor < 0 {
        return Err(bad(
            "invalid_chunk",
            format!("a chunk is between 1 and {MAX_EXECUTION_CHUNK} orders and a threshold is not negative"),
        ));
    }
    let Some(old) = book.prices.get(superseded).cloned() else {
        return Err(bad("unknown_price", format!("{superseded} is not a price on this partition")));
    };
    let Some(new) = book.price_for(&old.fund, old.pricing_date).filter(|p| p.price_id != old.price_id).cloned() else {
        return Err(bad(
            "not_superseded",
            format!("{superseded} is the price in force; there is nothing to replay it against"),
        ));
    };
    let rounding = book.fund(&old.fund).map(|f| f.unit_rounding).unwrap_or(RoundingMode::HalfEven);
    let prefix = format!("{superseded}/");
    let chunk: Vec<String> = book
        .executed_at
        .range(prefix.clone()..)
        .take_while(|k| k.starts_with(&prefix))
        .cloned()
        .collect();
    let mut done = 0u32;
    for key in chunk {
        if done == limit {
            break;
        }
        let order_id = key[prefix.len()..].to_string();
        let Some((order, was_cash, was_units)) = book.orders.get(&order_id).and_then(|o| match &o.state {
            OrderState::Executed { cash_minor, units_micro, .. } => Some((o.order.clone(), *cash_minor, *units_micro)),
            _ => None,
        }) else {
            book.executed_at.remove(&key);
            continue;
        };
        // A policy in flight is replayed when it has landed or come back.
        // One that has left for good stays in the index, so the question
        // `ToReplay` keeps showing it; it does not use up the chunk.
        let Some(policy) = policies.get(&order.policy).filter(|p| !p.is_frozen() && p.status != PolicyStatus::Transferred) else {
            continue;
        };
        done += 1;
        let c = policy.currency;
        let fund = &order.fund;
        // The side the customer fixed stays; the other is worked out again.
        // A sell for cash that fell short took the whole holding instead,
        // and so fixed the units.
        let cash_fixed = matches!(order.amount, OrderAmount::Cash { minor } if order.direction == Direction::Buy || minor == was_cash);
        let (now_cash, now_units) = match (order.direction, cash_fixed) {
            (Direction::Buy, _) => (
                was_cash,
                UnitQty::for_cash(Money::new(was_cash, c), new.buy.get(), fund.clone(), rounding)?.micro,
            ),
            (Direction::Sell, true) => (
                was_cash,
                UnitQty::for_cash(Money::new(was_cash, c), new.sell.get(), fund.clone(), RoundingMode::Up)?.micro,
            ),
            (Direction::Sell, false) => (
                UnitQty::new(was_units, fund.clone()).value_at(new.sell.get(), c, RoundingMode::HalfEven)?.minor,
                was_units,
            ),
        };
        // What the policy is owed: more units bought or fewer sold, and more
        // cash for what it sold.
        let owed_units = match order.direction {
            Direction::Buy => now_units - was_units,
            Direction::Sell => was_units - now_units,
        };
        let owed_cash = now_cash - was_cash;
        let value_of = |micro: i64| -> Result<i64, Reject> {
            Ok(UnitQty::new(micro.abs(), fund.clone())
                .value_at(new.mid.get(), c, RoundingMode::HalfEven)?
                .minor)
        };
        let material = value_of(owed_units)? + owed_cash.abs() >= materiality_minor.max(1);
        let unit_account = accounts::unit_account(&policy.id, fund);
        let pending_cash = accounts::pending_investment_cash(&policy.id, c);
        let fund_cash = accounts::fund_cash_position(fund, c);
        let mut set = PostingSet::new();
        let (mut units_delta, mut cash_delta, mut borne) = (0i64, 0i64, 0i64);
        if material {
            if owed_units > 0 && policy.status == PolicyStatus::InForce {
                units_delta = owed_units;
            } else if owed_units > 0 {
                // A policyholder who has left is owed cash, not units.
                cash_delta += value_of(owed_units)?;
            } else if owed_units < 0 {
                let held = -ledger.balance(&unit_account.id);
                let taken = (-(owed_units as i128)).min(held.max(0)) as i64;
                units_delta = -taken;
                borne += value_of(-owed_units - taken)?;
            }
            if owed_cash > 0 {
                cash_delta += owed_cash;
            } else {
                borne += -owed_cash;
            }
            // Units between the fund and the policy.
            set.debit(accounts::fund_unit_position(fund), units_delta as i128, Dimensions::default());
            set.credit(unit_account, units_delta as i128, Dimensions::default());
            // Cash the fund pays the policy, and cash the insurer pays the fund.
            set.debit(fund_cash.clone(), cash_delta as i128, Dimensions::default());
            set.credit(pending_cash, cash_delta as i128, Dimensions::default());
            set.debit(accounts::pricing_error(fund, c), borne as i128, Dimensions::default());
            set.credit(fund_cash, borne as i128, Dimensions::default());
        }
        if !set.is_empty() {
            let postings = set.into_postings(ledger)?;
            let detail = BTreeMap::from([
                ("order_id".to_string(), order_id.clone()),
                ("from_price".to_string(), old.price_id.clone()),
                ("price_id".to_string(), new.price_id.clone()),
                ("pricing_date".to_string(), new.pricing_date.to_string()),
            ]);
            ctx.post(ledger, "unit_price_correction", Some(policy), new.pricing_date, postings, detail)?;
            // The fund's cash position reconciles to its orders at their
            // corrected figures; what was settled outside those figures
            // (cash for a leaver's units, units the insurer paid for) is
            // kept beside them.
            *book.remediation_minor.entry(fund.0.clone()).or_default() += borne as i128 - cash_delta as i128 + owed_cash as i128;
        }
        // Once put right, the order stands at the corrected figures, however
        // the difference was settled, so a further correction starts from
        // them. Below the threshold it keeps the figures the policy has.
        if let Some(held) = book.orders.get_mut(&order_id) {
            let (stands_cash, stands_units) = if material { (now_cash, now_units) } else { (was_cash, was_units) };
            held.state = OrderState::Executed {
                price_id: new.price_id.clone(),
                cash_minor: stands_cash,
                units_micro: stands_units,
                on: ctx.date,
            };
        }
        book.executed_at.remove(&key);
        book.executed_at.insert(format!("{}/{order_id}", new.price_id));
        book.note_waiting(ledger, policy, ctx.date);
        ctx.emit(crate::Event::Investment(InvestEvent::UnitOrderReplayed {
            order_id,
            policy: policy.id.clone(),
            fund: fund.clone(),
            pricing_date: new.pricing_date,
            from_price: old.price_id.clone(),
            to_price: new.price_id.clone(),
            units_delta_micro: units_delta,
            cash_delta_minor: cash_delta,
            borne_minor: borne,
            compensated: material,
        }));
    }
    Ok(())
}

/// A question about the investment ledger on one partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Question {
    /// Units on issue in a fund, for the control before a price is struck.
    UnitsOnIssue { fund: FundId },
    /// Every policy's pending investment cash and the day it has waited
    /// since, for the aged-items check at the close.
    WaitingCash,
    /// One order and its state.
    Order { order_id: String },
    /// What one policy holds in each fund, in micro-units, and its pending
    /// cash. The value in currency is units times a price and is worked out
    /// where it is shown, labelled with the price date.
    Holdings { policy: PolicyId },
    /// The funds and pricing dates that have a price in force and orders
    /// still pending: what the execution run has left to do.
    Executable,
    /// The superseded prices that still have executed orders on them: what
    /// the replay run has left to do.
    ToReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holdings {
    pub policy: PolicyId,
    pub currency: Currency,
    pub units_micro: BTreeMap<String, i128>,
    pub pending_cash_minor: i128,
    pub pending_orders: Vec<UnitOrder>,
}

/// A fund and pricing date with a price in force and orders to execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Executable {
    pub fund: FundId,
    pub pricing_date: NaiveDate,
    pub price_id: String,
    pub pending: u32,
    /// The first of them. An order executes once, so a run that names it in
    /// its idempotency key never sends the same key for different work.
    pub first: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    UnitsOnIssue(UnitsOnIssue),
    WaitingCash(Vec<WaitingCash>),
    Order(Option<UnitOrder>),
    Holdings(Option<Holdings>),
    Executable(Vec<Executable>),
    ToReplay(Vec<Replayable>),
}

/// A superseded price with executed orders still standing on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replayable {
    pub superseded: String,
    pub fund: FundId,
    pub pricing_date: NaiveDate,
    pub orders: u32,
    /// The fund's approved minimum for compensating one order, from its
    /// pricing terms (`compensation_minimum_minor`; none means every
    /// difference is paid). The replay run sends it back as the threshold.
    pub compensation_minimum_minor: i64,
}

impl InvestmentBook {
    pub fn answer(&self, ledger: &Ledger, policies: &BTreeMap<PolicyId, Policy>, question: &Question) -> Reply {
        match question {
            Question::UnitsOnIssue { fund } => Reply::UnitsOnIssue(self.units_on_issue(ledger, fund)),
            Question::WaitingCash => Reply::WaitingCash(self.waiting_cash(ledger, policies)),
            Question::Order { order_id } => Reply::Order(self.orders.get(order_id).cloned()),
            Question::Holdings { policy } => Reply::Holdings(policies.get(policy).map(|p| {
                Holdings {
                    policy: p.id.clone(),
                    currency: p.currency,
                    units_micro: self
                        .funds
                        .keys()
                        .map(|fund| (fund.clone(), -ledger.balance(&accounts::unit_account(&p.id, &FundId(fund.clone())).id)))
                        .filter(|(_, units)| *units != 0)
                        .collect(),
                    pending_cash_minor: -ledger.balance(&accounts::pending_investment_cash(&p.id, p.currency).id),
                    pending_orders: self.pending_of(&p.id).cloned().collect(),
                }
            })),
            Question::Executable => Reply::Executable(self.executable()),
            Question::ToReplay => Reply::ToReplay(self.replayable()),
        }
    }

    /// Superseded prices with executed orders still standing on them.
    pub fn replayable(&self) -> Vec<Replayable> {
        let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
        for key in &self.executed_at {
            if let Some((price, _)) = key.split_once('/') {
                *counts.entry(price).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .filter_map(|(id, orders)| {
                let price = self.prices.get(id)?;
                let current = self.price_for(&price.fund, price.pricing_date)?;
                let minimum = self
                    .fund(&price.fund)
                    .and_then(|f| f.pricing.get("compensation_minimum_minor")?.parse::<i64>().ok());
                (current.price_id != price.price_id).then(|| Replayable {
                    superseded: price.price_id.clone(),
                    fund: price.fund.clone(),
                    pricing_date: price.pricing_date,
                    orders,
                    compensation_minimum_minor: minimum.unwrap_or(0).max(0),
                })
            })
            .collect()
    }

    /// Funds and dates with a price in force and orders still pending.
    pub fn executable(&self) -> Vec<Executable> {
        // In key order, so the first order seen in a slot is its first.
        let mut counts: BTreeMap<String, (u32, &str)> = BTreeMap::new();
        for key in &self.pending {
            if let Some((slot, order)) = key.rsplit_once('/') {
                counts.entry(slot.to_string()).or_insert((0, order)).0 += 1;
            }
        }
        counts
            .into_iter()
            .filter_map(|(slot, (pending, first))| {
                let price = self.prices.get(self.in_force.get(&slot)?)?;
                Some(Executable {
                    fund: price.fund.clone(),
                    pricing_date: price.pricing_date,
                    price_id: price.price_id.clone(),
                    pending,
                    first: first.to_string(),
                })
            })
            .collect()
    }

    /// The investment ledger's own reconciliation: each fund's unit position
    /// equals the units its policies hold, each fund's cash position equals
    /// the net cash of its executed orders, no policy's pending buy orders
    /// count on more cash than it has waiting, and every pending order is
    /// indexed for execution.
    pub fn breaks(&self, ledger: &Ledger, policies: &BTreeMap<PolicyId, Policy>) -> Vec<(&'static str, String)> {
        let mut breaks = Vec::new();
        let mut held: BTreeMap<FundId, i128> = BTreeMap::new();
        for (account, balance) in ledger.accounts() {
            if account.role == accounts::role::UNIT_ACCOUNT {
                if let ic_ledger::AccountUnit::Units(fund) = &account.unit {
                    *held.entry(fund.clone()).or_default() -= balance;
                }
            }
        }
        let mut flows: BTreeMap<FundId, i128> = BTreeMap::new();
        let mut committed: BTreeMap<&PolicyId, i128> = BTreeMap::new();
        let mut pending = 0usize;
        for order in self.orders.values() {
            match (&order.state, order.order.direction) {
                (OrderState::Executed { cash_minor, .. }, Direction::Buy) => *flows.entry(order.order.fund.clone()).or_default() += *cash_minor as i128,
                (OrderState::Executed { cash_minor, .. }, Direction::Sell) => *flows.entry(order.order.fund.clone()).or_default() -= *cash_minor as i128,
                (OrderState::Pending, direction) => {
                    pending += 1;
                    if let (Direction::Buy, OrderAmount::Cash { minor }) = (direction, order.order.amount) {
                        *committed.entry(&order.order.policy).or_default() += minor as i128;
                    }
                }
                (OrderState::Cancelled { .. }, _) => {}
            }
        }
        for fund in self.funds.keys().map(|f| FundId(f.clone())) {
            let position = ledger.balance(&accounts::fund_unit_position(&fund).id);
            let units = held.get(&fund).copied().unwrap_or(0);
            if position != units {
                breaks.push((
                    "fund_unit_position",
                    format!("{} has {position} micro-units on issue and policies hold {units}", fund.0),
                ));
            }
            let currency = self.fund(&fund).map(|f| f.currency);
            let cash = currency.map_or(0, |c| -ledger.balance(&accounts::fund_cash_position(&fund, c).id));
            let net = flows.get(&fund).copied().unwrap_or(0) + self.remediation_minor.get(&fund.0).copied().unwrap_or(0);
            if cash != net {
                breaks.push(("fund_cash_position", format!("{} holds {cash} and its executed orders net to {net}", fund.0)));
            }
        }
        if pending != self.pending.len() {
            breaks.push((
                "unit_orders_pending",
                format!("{pending} orders are pending and {} are indexed for execution", self.pending.len()),
            ));
        }
        for (policy, minor) in committed {
            let waiting = policies
                .get(policy)
                .map_or(0, |p| -ledger.balance(&accounts::pending_investment_cash(&p.id, p.currency).id));
            if waiting < minor {
                breaks.push(("pending_investment_cash", format!("{policy} has {waiting} waiting and buy orders for {minor}")));
            }
        }
        breaks
    }
}
