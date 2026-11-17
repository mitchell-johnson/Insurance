//! The module as a ledger node hosts it: its name, the questions it answers,
//! and its spec for the deployed node.

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::invest::{Direction, InvestCommand, InvestEvent, UnitPrice};
use ic_core::{Event as CoreEvent, EventRecord};
use ic_money::FundId;
use ic_raft::client::fresh_salt;
use ic_raft::{hosted_reading, CoreOutcome, CorePosting, ModuleSpec, ModuleState, Outbox, Route};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::command::{Command, Delivery, Envelope, MovementKind, UnitMovement, CALLER_EVENTS, CALLER_OUTBOX};
use crate::state::{slot, PriceStatus, PricingState, Publication};

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
    /// What operations must be told of now: prices late as at a date,
    /// prices a partition refused, and valuation files that could not be
    /// read. Each item has a stable `id`, so the driver raises it once.
    Alerts {
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
                .map(|(fund, versions)| json!({ "fund": fund, "versions": versions, "suspensions": state.suspensions.get(fund), "next": state.next_to_price(fund) }))
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
        // One row per fund, not per day: a fund months behind is one thing
        // to put right, with how many days and the first and last (F2-06-6).
        Question::Late { as_at } => {
            let mut by_fund: std::collections::BTreeMap<FundId, Vec<NaiveDate>> = std::collections::BTreeMap::new();
            for (fund, date) in state.late(as_at) {
                by_fund.entry(fund).or_default().push(date);
            }
            let rows: Vec<Value> = by_fund
                .into_iter()
                .map(|(fund, dates)| json!({ "fund": fund, "days": dates.len(), "first": dates.first(), "last": dates.last() }))
                .collect();
            Ok(json!(rows))
        }
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
                    let currency = state.definition_of(&x.fund).map(|d| d.currency);
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
        Question::Alerts { as_at } => {
            let mut items: Vec<Value> = Vec::new();
            for (fund, date) in state.late(as_at) {
                items.push(json!({ "id": format!("late:{}", slot(&fund, date)), "kind": "late_price", "fund": fund, "pricing_date": date }));
            }
            for p in state.prices.values().filter(|p| p.status == PriceStatus::Activating) {
                for (partition, d) in &p.deliveries {
                    if let Delivery::Refused { code, message } = d {
                        items.push(json!({
                            "id": format!("refused:{}/{partition}", p.id),
                            "kind": "publication_refused",
                            "price": p.id,
                            "partition": partition,
                            "code": code,
                            "message": message,
                        }));
                    }
                }
            }
            for (definition, reason) in &state.refused_definitions {
                items.push(json!({ "id": format!("definition:{definition}"), "kind": "definition_refused", "definition": definition, "reason": reason }));
            }
            for (file, reason) in &state.refused_files {
                items.push(json!({ "id": format!("file:{file}"), "kind": "file_refused", "file": file, "reason": reason }));
            }
            items.truncate(MAX_ANSWER);
            Ok(json!(items))
        }
    }
}

/// One of the core's events as the module's own command: an executed unit
/// order, an order replayed at a corrected price, or a fund the core
/// activated, so this module holds exactly the definitions the core holds.
/// Anything else is ignored. The key is the event's ID, so an event offered twice is taken
/// in once; time is the event's business date, so every member makes the
/// same command.
pub fn react(record: &EventRecord) -> Vec<Envelope> {
    let at: DateTime<Utc> = record.business_date.and_time(chrono::NaiveTime::MIN).and_utc();
    let movement = match &record.event {
        // One per partition: the key names the version and content, so the
        // repeats, and the same fund sent by configuration, are taken once.
        CoreEvent::Investment(InvestEvent::FundActivated { definition: Some(def), .. }) => {
            let mut envelope = crate::command::activation((**def).clone(), at);
            envelope.caller = CALLER_EVENTS.to_string();
            envelope.actor = CALLER_EVENTS.to_string();
            return vec![envelope];
        }
        CoreEvent::Investment(InvestEvent::UnitOrderExecuted {
            order_id,
            policy,
            fund,
            direction,
            cash_minor,
            units_micro,
            price_id,
            pricing_date,
            ..
        }) => {
            let (kind, sign) = match direction {
                Direction::Buy => (MovementKind::Buy, 1),
                Direction::Sell => (MovementKind::Sell, -1),
            };
            UnitMovement {
                id: order_id.clone(),
                partition: record.partition,
                policy: policy.0.clone(),
                fund: fund.clone(),
                kind,
                units_micro: sign * units_micro.abs(),
                cash_minor: sign * cash_minor.abs(),
                price_id: Some(price_id.clone()),
                pricing_date: *pricing_date,
                borne_minor: 0,
                uncompensated: false,
            }
        }
        CoreEvent::Investment(InvestEvent::UnitOrderReplayed {
            order_id,
            policy,
            fund,
            pricing_date,
            to_price,
            units_delta_micro,
            cash_delta_minor,
            borne_minor,
            compensated,
            ..
        }) => UnitMovement {
            id: format!("{order_id}/replay/{to_price}"),
            partition: record.partition,
            policy: policy.0.clone(),
            fund: fund.clone(),
            kind: MovementKind::Correction,
            units_micro: *units_delta_micro,
            cash_minor: *cash_delta_minor,
            price_id: Some(to_price.clone()),
            pricing_date: *pricing_date,
            borne_minor: *borne_minor,
            uncompensated: !compensated,
        },
        _ => return Vec::new(),
    };
    vec![Envelope {
        key: format!("event:{}", record.id),
        caller: CALLER_EVENTS.to_string(),
        actor: CALLER_EVENTS.to_string(),
        roles: Vec::new(),
        at,
        today: None,
        command: Command::RecordMovement(Box::new(movement)),
    }]
}

/// The core command that activates one partition's copy of a price. The key
/// is the publication's own, so a retry after a timeout applies once.
pub fn price_envelope(p: &Publication, salt: String) -> ic_core::Envelope {
    ic_core::Envelope {
        key: format!("{CORE_CALLER}:{}", p.id),
        caller: CORE_CALLER.to_string(),
        salt,
        traceparent: None,
        command: ic_core::Command::Investment(Box::new(InvestCommand::ActivateUnitPrice(Box::new(UnitPrice {
            price_id: p.price.clone(),
            fund: p.fund.clone(),
            pricing_date: p.pricing_date,
            version: p.version,
            buy: p.buy.clone(),
            sell: p.sell.clone(),
            mid: p.mid.clone(),
            inputs_hash: p.inputs_hash.clone(),
            supersedes: p.supersedes.clone(),
        })))),
    }
}

fn outbox_pending(limit: usize) -> Value {
    json!({ "q": "outbox", "limit": limit })
}

fn outbox_read(answer: Value) -> Result<Vec<CorePosting>, String> {
    let items: Vec<Publication> = serde_json::from_value(answer).map_err(|e| format!("not an outbox answer: {e}"))?;
    Ok(items
        .iter()
        .map(|p| CorePosting {
            id: p.id.clone(),
            route: Route::Partition(p.partition),
            envelope: price_envelope(p, fresh_salt()),
        })
        .collect())
}

fn outbox_outcome(posting: &CorePosting, outcome: &CoreOutcome) -> Value {
    let delivery = match outcome {
        CoreOutcome::Applied { .. } => Delivery::Applied,
        CoreOutcome::Refused(reject) => Delivery::Refused {
            code: reject.code.clone(),
            message: reject.message.clone(),
        },
        CoreOutcome::Unroutable(why) => Delivery::Refused {
            code: "unroutable".to_string(),
            message: why.clone(),
        },
    };
    let envelope = Envelope {
        key: format!("outcome:{}", posting.id),
        caller: CALLER_OUTBOX.to_string(),
        actor: CALLER_OUTBOX.to_string(),
        roles: Vec::new(),
        at: Utc::now(),
        today: None,
        command: Command::RecordDelivery {
            publication: posting.id.clone(),
            delivery,
        },
    };
    serde_json::to_value(envelope).unwrap_or(Value::Null)
}

/// How the host carries the module's prices to the core.
pub const OUTBOX: Outbox = Outbox {
    pending: outbox_pending,
    read: outbox_read,
    outcome: outbox_outcome,
};

fn make() -> Box<dyn ModuleState> {
    hosted_reading(PricingState::new(), answer, react)
}

/// The module as a node hosts it: listed in `ic_node::modules()`.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(NAME, make)?.reading_events().posting_to_core(OUTBOX))
}
