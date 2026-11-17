//! Carrying servicing requests to the core.
//!
//! A customer asks for a change in the app, or approves one an adviser or a
//! member of staff prepared for them: decline indexation, change how often
//! they pay, cancel the policy. Each is a request owned by
//! `policy-servicing`. Once it is pending (every owner has signed it, and a
//! second member of staff has agreed it where that was needed) the portal
//! turns it into the core's own command and proposes it as caller
//! `policy-servicing`, with the request's ID as the key, so carrying it
//! twice changes the policy once. The request is then closed with what the
//! core said.
//!
//! A request the customer master said must go to a trained person first
//! (`route_to_person`) is never carried by the sweep, nor is one of a kind
//! the portal does not know how to carry out. Each waits for a member of
//! staff holding [`actions::CARRY_OUT`], who has spoken to the customer, to
//! carry it out or decline it from the staff console.

use chrono::{Datelike, NaiveDate};
use ic_core::invest::{Direction, InvestCommand, NewOrder, OrderAmount, OrderKind};
use ic_core::{Command, PolicyId};
use ic_money::FundId;
use ic_product::Frequency;
use serde_json::Value;

/// The module that owns these requests, and the caller the core hears them from.
pub const MODULE: &str = "policy-servicing";

/// The kinds of request this carries, with what each needs in its payload.
pub const KINDS: [(&str, &str); 5] = [
    (
        "decline_indexation",
        "`declined`: true to decline indexation, false to take it again (true if left out)",
    ),
    (
        "change_frequency",
        "`frequency` (fortnightly, monthly, quarterly, half_yearly, yearly) and optionally `billing_day` (1 to 28)",
    ),
    ("cancel_policy", "nothing more"),
    (
        "reduce_cover",
        "`cover` and `life` as the policy names them (term_life, life-1) and `sum_insured_minor`: the cover the life keeps, in cents, less than now (0 gives it up)",
    ),
    (
        "switch_funds",
        "`from_fund` and `to_fund` (fund codes such as balanced, cash, growth) and either `amount_minor`: how much to move, in cents, or `all`: true to move everything held in `from_fund`",
    ),
];

/// What the customer is told when they ask for a change of a kind this
/// does not take, with who can help (F2-05-1).
pub fn unknown_kind(kind: &str) -> String {
    let kinds: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
    format!(
        "{kind} is not a change you can ask for here; the changes are {}. For anything else, ask your adviser or call us",
        kinds.join(", ")
    )
}

/// The pricing date a switch placed now takes for `fund`: the books' date,
/// or the day after the fund's latest price where that has been struck,
/// moved past a weekend. A fund holiday is the core's to refuse, naming the
/// next pricing day.
pub fn switch_pricing_date(today: NaiveDate, struck: Option<NaiveDate>) -> NaiveDate {
    let mut date = match struck {
        Some(last) if last >= today => last + chrono::Days::new(1),
        _ => today,
    };
    while matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
        date = date + chrono::Days::new(1);
    }
    date
}

/// Kinds that give up cover, so that commission clawed back for them may
/// fall on whoever submitted them. Before the core is asked, the commission
/// module is told which engagement an adviser submitted one under
/// (`attribution`).
pub const ATTRIBUTED: [&str; 2] = ["cancel_policy", "reduce_cover"];

/// The commission module's envelope saying which adviser engagement
/// submitted this request's alteration, sent as `policy-servicing` on the
/// day and before the core command. Its key is the request's, so carrying
/// it twice tells the module once.
pub fn attribution(request: &Value, engagement: &str) -> Value {
    serde_json::json!({
        "key": format!("servicing:{}:attribution", request["id"].as_str().unwrap_or_default()),
        "actor": { "module": MODULE },
        "command": { "attribute_alteration": { "policy": request["policy"], "engagement": engagement } },
    })
}

/// Staff actions for requests that went to a person first.
pub mod actions {
    /// Carry out or decline a customer's request the customer master sent to a trained person.
    pub const CARRY_OUT: &str = "servicing.carry_out";
}

/// A request's refusal before it reaches the core: (code, message).
pub type Refusal = (String, String);

fn refuse(code: &str, message: impl Into<String>) -> Refusal {
    (code.to_string(), message.into())
}

/// The core's command for a pending request, as the pending list gives it
/// (`id`, `kind`, `policy`, `payload`), or why it cannot be one.
pub fn command(request: &Value) -> Result<Command, Refusal> {
    let id = request["id"].as_str().unwrap_or_default();
    let policy = request["policy"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(PolicyId::from)
        .ok_or_else(|| refuse("no_policy", "the request names no policy"))?;
    let payload = &request["payload"];
    match request["kind"].as_str().unwrap_or_default() {
        "decline_indexation" => Ok(Command::DeclineIndexation {
            policy,
            declined: payload["declined"].as_bool().unwrap_or(true),
        }),
        "cancel_policy" => Ok(Command::CancelPolicy { policy }),
        "reduce_cover" => {
            let named = |field: &str| {
                payload[field]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| refuse("nothing_to_reduce", format!("say the {field} to reduce")))
            };
            let (cover, life) = (named("cover")?, named("life")?);
            let sum_insured_minor = payload["sum_insured_minor"]
                .as_i64()
                .filter(|s| *s >= 0)
                .ok_or_else(|| refuse("bad_amount", "say the cover the life keeps, in cents, 0 or more"))?;
            Ok(Command::ReduceCover(Box::new(ic_core::command::ReduceCover {
                policy,
                cover,
                life: ic_core::LifeId::from(life.as_str()),
                sum_insured_minor,
            })))
        }
        "change_frequency" => {
            let frequency: Option<Frequency> = match &payload["frequency"] {
                Value::Null => None,
                given => Some(serde_json::from_value(given.clone()).map_err(|_| refuse("bad_frequency", format!("{given} is not a payment frequency")))?),
            };
            let billing_day = match &payload["billing_day"] {
                Value::Null => None,
                given => Some(
                    given
                        .as_u64()
                        .and_then(|d| u32::try_from(d).ok())
                        .ok_or_else(|| refuse("bad_billing_day", format!("{given} is not a day of the month")))?,
                ),
            };
            if frequency.is_none() && billing_day.is_none() {
                return Err(refuse("nothing_to_change", "say the new frequency or the new billing day"));
            }
            Ok(Command::ChangeBilling(Box::new(ic_core::billing_change::ChangeBilling {
                policy,
                reference: id.to_string(),
                frequency,
                billing_day,
            })))
        }
        "switch_funds" => {
            let fund = |field: &str, which: &str| {
                payload[field]
                    .as_str()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(|v| FundId(v.to_string()))
                    .ok_or_else(|| refuse("nothing_to_switch", format!("say the fund to switch {which}")))
            };
            let (from, to) = (fund("from_fund", "from")?, fund("to_fund", "to")?);
            if from == to {
                return Err(refuse("nothing_to_switch", "a switch is from one fund to another"));
            }
            let amount = match (payload["all"].as_bool(), payload["amount_minor"].as_i64()) {
                (Some(true), _) => OrderAmount::AllUnits,
                (_, Some(minor)) if minor > 0 => OrderAmount::Cash { minor },
                _ => return Err(refuse("bad_amount", "say how much to switch, in cents, or that it is everything in the fund")),
            };
            let pricing_date = request["pricing_date"]
                .as_str()
                .and_then(|d| d.parse().ok())
                .ok_or_else(|| refuse("no_pricing_date", "the switch has no pricing date yet"))?;
            Ok(Command::Investment(Box::new(InvestCommand::PlaceOrder(Box::new(NewOrder {
                order_id: id.to_string(),
                policy,
                fund: from,
                direction: Direction::Sell,
                amount,
                kind: OrderKind::SwitchOut,
                pricing_date,
                switch_to: Some(to),
            })))))
        }
        other => Err(refuse("not_carried", unknown_kind(other))),
    }
}

/// Whether the sweep carries this pending request. It leaves for a person
/// one the customer master sent to a person first, and one of a kind the
/// portal does not know how to carry out.
pub fn carried_by_sweep(request: &Value) -> bool {
    let known = KINDS.iter().any(|(kind, _)| request["kind"] == *kind);
    known && request["route_to_person"] != true
}
