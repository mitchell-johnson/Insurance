//! The module as a ledger node hosts it (`docs/modules.md`): its name, the
//! questions it answers, and how it is shown the core's events.

use chrono::NaiveDate;
use ic_core::EventRecord;
use ic_raft::{hosted_reading, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::machine::{Command, Envelope, Reinsurance};
use crate::{ClaimId, LifeKey, ReinsurerId, RiskKey, TreatyId};

/// The module's permanent name on the cluster.
pub const MODULE: &str = "reinsurance";

/// The most rows one answer carries. A caller pages with what it was given.
pub const MAX_ANSWER: usize = 500;

/// The questions the module answers. Answers come from the group's leader
/// after it has confirmed it still leads, and never change state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    /// The oldest instructions not yet acknowledged: `[seq, instruction]`.
    Outbox {
        limit: usize,
    },
    /// The oldest notices not yet handed on: `[seq, notice]`.
    Notices {
        limit: usize,
    },
    /// The core command for an instruction still in the outbox.
    Delivery {
        seq: u64,
        on: NaiveDate,
    },
    /// The cession record of one cover.
    Cession {
        key: RiskKey,
    },
    /// What the insurer keeps on a life on a date.
    Retained {
        life: LifeKey,
        on: NaiveDate,
    },
    /// What is owed to a reinsurer under a treaty. Positive is owed to them.
    Balance {
        treaty: TreatyId,
        reinsurer: ReinsurerId,
    },
    Statement {
        number: u32,
    },
    Listing {
        treaty: TreatyId,
        reinsurer: ReinsurerId,
        period_start: NaiveDate,
        period_end: NaiveDate,
    },
    /// Ceded sums and open recoverables by reinsurer.
    Exposure {
        on: NaiveDate,
    },
    SchemeConcentration {
        on: NaiveDate,
    },
    ConsentStatus {
        claim: ClaimId,
    },
    Exceptions,
    Abandoned,
    /// The module's own invariants: an empty list when they hold.
    Reconcile,
    StateHash,
}

fn json<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Answer a question. A question that is not one of [`Query`] is refused.
pub fn answer(state: &Reinsurance, question: Value) -> Result<Value, String> {
    let query: Query = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    match query {
        Query::Outbox { limit } => json(state.outbox(limit.min(MAX_ANSWER))),
        Query::Notices { limit } => json(state.notices(limit.min(MAX_ANSWER))),
        Query::Delivery { seq, on } => {
            let found = state.outbox(usize::MAX).into_iter().find(|(s, _)| *s == seq);
            let (_, instruction) = found.ok_or_else(|| format!("instruction {seq} is not waiting to be delivered"))?;
            let delivery = state.to_core(instruction, on).ok_or("the instruction's treaty is not active")?;
            json((delivery.partition, delivery.key, delivery.caller, delivery.command))
        }
        Query::Cession { key } => json(state.cession_record(&key)),
        Query::Retained { life, on } => json(state.retained_on(&life, on)),
        Query::Balance { treaty, reinsurer } => json(state.balance(&treaty, &reinsurer)),
        Query::Statement { number } => json(state.statement(number)),
        Query::Listing {
            treaty,
            reinsurer,
            period_start,
            period_end,
        } => json(
            state
                .listing(&treaty, &reinsurer, period_start, period_end)
                .into_iter()
                .take(MAX_ANSWER)
                .collect::<Vec<_>>(),
        ),
        Query::Exposure { on } => json(state.exposure(on)),
        Query::SchemeConcentration { on } => json(state.scheme_concentration(on)),
        Query::ConsentStatus { claim } => json(state.consent_status(&claim)),
        Query::Exceptions => json(state.exceptions()),
        Query::Abandoned => json(state.abandoned()),
        Query::Reconcile => json(state.reconcile()),
        Query::StateHash => json(hex::encode(state.state_hash().0)),
    }
}

/// What one core event becomes: a batch of one for the module's feed, keyed
/// by the event's own ID so a second showing is recognised, and dated by the
/// event's business date.
pub fn react(record: &EventRecord) -> Vec<Envelope> {
    vec![Envelope {
        key: format!("core:{}", record.id),
        caller: "event-feed".to_string(),
        on: record.business_date,
        command: Command::CoreEvents { events: vec![record.clone()] },
    }]
}

fn make() -> Box<dyn ModuleState> {
    hosted_reading(Reinsurance::new(), answer, react)
}

/// The reinsurance module, for a ledger node to host: listed in
/// `ic_node::modules()`.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events())
}
