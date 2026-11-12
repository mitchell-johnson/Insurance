//! The module as a ledger node hosts it: its name, the questions it answers,
//! and its spec for the deployed node.

use chrono::NaiveDate;
use ic_money::FundId;
use ic_raft::{hosted, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::{slot, PricingState};

/// The module's permanent name on the cluster.
pub const NAME: &str = "unit-pricing";
/// The caller name the core knows this module by.
pub const CORE_CALLER: &str = "unit-pricing";
/// The most items one answer carries.
pub const MAX_ANSWER: usize = 500;

/// A question the module answers. Questions never change state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "q", rename_all = "snake_case")]
pub enum Question {
    /// Partitions, counts and the state hash.
    Status,
    /// Every fund's definitions, newest last, and any suspension.
    Funds,
    Price {
        id: String,
    },
    /// The prices in force for a fund between two dates, with their versions.
    Prices {
        fund: FundId,
        from: NaiveDate,
        to: NaiveDate,
    },
    /// The valuations received for a fund and date.
    Valuations {
        fund: FundId,
        pricing_date: NaiveDate,
    },
    /// Prices waiting for a person, and files that could not be read.
    Work,
    /// Prices overdue as at a date.
    Late {
        as_at: NaiveDate,
    },
    Errors,
    Error {
        id: String,
    },
    /// The oldest publications still to be carried to the core.
    Outbox {
        limit: usize,
    },
    Events {
        after: u64,
        limit: usize,
    },
    /// Extracts due to the administrator, with their holdings.
    ExtractsDue {
        limit: usize,
    },
    /// The units a policy holds of a fund, as the core's events report them.
    Holding {
        fund: FundId,
        policy: String,
    },
    /// The finance close's check: green when no price is late, waiting for
    /// a person or held by an open price error, and every extract is written.
    CloseCheck {
        as_at: NaiveDate,
    },
}

fn value<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// Answer a question. Anything that is not a [`Question`] is refused.
pub fn answer(state: &PricingState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    ask(state, question)
}

/// Answer a typed question.
pub fn ask(state: &PricingState, question: Question) -> Result<Value, String> {
    match question {
        Question::Status => Ok(json!({
            "partitions": state.partitions,
            "funds": state.funds.len(),
            "prices": state.prices.len(),
            "unpublished": state.publications.len(),
            "open_errors": state.errors.values().filter(|e| e.closed.is_none()).count(),
            "event_seq": state.event_seq,
            "state_hash": state.state_hash(),
        })),
        Question::Funds => {
            let rows: Vec<Value> = state
                .funds
                .iter()
                .map(|(fund, versions)| json!({ "fund": fund, "versions": versions, "suspensions": state.suspensions.get(fund) }))
                .collect();
            Ok(json!(rows))
        }
        Question::Price { id } => value(state.prices.get(&id)),
        Question::Prices { fund, from, to } => {
            let (lo, hi) = (slot(&fund, from), slot(&fund, to));
            let rows: Vec<Value> = state
                .active
                .range(lo..=hi)
                .filter_map(|(_, ids)| ids.last())
                .filter_map(|id| state.prices.get(id))
                .take(MAX_ANSWER)
                .map(|p| {
                    json!({
                        "price": p.id,
                        "pricing_date": p.pricing_date,
                        "version": p.version,
                        "mid": p.struck.mid,
                        "buy": p.struck.buy,
                        "sell": p.struck.sell,
                        "inputs_hash": p.struck.inputs_hash,
                    })
                })
                .collect();
            Ok(json!(rows))
        }
        Question::Valuations { fund, pricing_date } => value(state.valuations.get(&slot(&fund, pricing_date))),
        Question::Work => Ok(json!({
            "prices": state.work().into_iter().take(MAX_ANSWER).collect::<Vec<_>>(),
            "refused_files": state.refused_files,
        })),
        Question::Late { as_at } => value(state.late(as_at)),
        Question::Errors => value(state.errors.values().take(MAX_ANSWER).collect::<Vec<_>>()),
        Question::Error { id } => value(state.errors.get(&id)),
        Question::Outbox { limit } => value(state.outbox(limit.min(MAX_ANSWER))),
        Question::Events { after, limit } => value(state.events_after(after, limit.min(MAX_ANSWER))),
        Question::ExtractsDue { limit } => {
            // Holdings make an extract large: a few at a time.
            let rows: Vec<Value> = state
                .extracts_due(limit.clamp(1, 5))
                .into_iter()
                .map(|x| {
                    let currency = state.definition_on(&x.fund, x.pricing_date).map(|d| d.currency);
                    json!({ "extract": x, "currency": currency })
                })
                .collect();
            Ok(json!(rows))
        }
        Question::Holding { fund, policy } => Ok(json!({
            "units_micro": state.holdings.get(&fund).and_then(|h| h.get(&policy)).copied().unwrap_or(0),
        })),
        Question::CloseCheck { as_at } => {
            let mut items: Vec<Value> = Vec::new();
            for (fund, date) in state.late(as_at) {
                items.push(json!({ "kind": "late_price", "fund": fund, "pricing_date": date }));
            }
            for p in state.work() {
                items.push(json!({ "kind": "price_awaiting_action", "price": p.id, "status": p.status }));
            }
            for e in state.errors.values().filter(|e| e.closed.is_none()) {
                items.push(json!({ "kind": "open_price_error", "error": e.id, "corrected": e.corrected() }));
            }
            for x in state.extracts.values().filter(|x| x.written.is_none() && x.pricing_date <= as_at) {
                items.push(json!({ "kind": "extract_not_written", "extract": x.id }));
            }
            items.truncate(MAX_ANSWER);
            Ok(json!({ "green": items.is_empty(), "items": items }))
        }
    }
}

fn make() -> Box<dyn ModuleState> {
    hosted(PricingState::new(), answer)
}

/// The module as a node hosts it: listed in `ic_node::modules()`.
pub fn module_spec() -> Result<ModuleSpec, String> {
    ModuleSpec::new(NAME, make)
}
