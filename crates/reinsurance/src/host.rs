//! The module as a ledger node hosts it (`docs/modules.md`): its name, the
//! questions it answers, and how it is shown the core's events.

use chrono::NaiveDate;
use ic_core::EventRecord;
use ic_raft::{hosted_reading, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::machine::{Audience, Command, Envelope, Reinsurance};
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
    /// The oldest instructions not yet acknowledged: `[seq, instruction]`,
    /// from after `after` when it is given.
    Outbox {
        limit: usize,
        #[serde(default)]
        after: Option<u64>,
    },
    /// The oldest notices not yet handed on: `[seq, notice]`. With an
    /// audience, only that audience's, leaving out any it refused.
    Notices {
        #[serde(default)]
        audience: Option<Audience>,
        limit: usize,
    },
    /// The core command for an instruction still in the outbox.
    Delivery {
        seq: u64,
        on: NaiveDate,
    },
    /// The core command that tells claims of a notice still waiting, in the
    /// shape of `Delivery`.
    NoticeDelivery {
        seq: u64,
    },
    /// How many times a person has sent a refused notice again.
    NoticeRetries {
        seq: u64,
    },
    /// The capacity answer for a reservation as things stand. Null once it
    /// is released or its cover has arrived.
    Capacity {
        reservation_id: String,
    },
    /// Where the facultative referral for a reservation has got to.
    FacultativeTerms {
        reservation_id: String,
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
    /// The close check finance reports as `reinsurance_sweep` for a period
    /// ending on the date: `{"green": bool, "items": [...]}`.
    SweepCheck {
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
    AbandonedNotices,
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
        Query::Outbox { limit, after } => json(state.outbox_after(after, limit.min(MAX_ANSWER))),
        Query::Notices { audience: None, limit } => json(state.notices(limit.min(MAX_ANSWER))),
        Query::Notices {
            audience: Some(audience),
            limit,
        } => json(state.notices_for(audience, limit.min(MAX_ANSWER))),
        Query::Delivery { seq, on } => {
            let found = state.outbox(usize::MAX).into_iter().find(|(s, _)| *s == seq);
            let (_, instruction) = found.ok_or_else(|| format!("instruction {seq} is not waiting to be delivered"))?;
            let delivery = state.to_core(instruction, on).ok_or("the instruction's treaty is not active")?;
            json((delivery.partition, delivery.key, delivery.caller, delivery.command))
        }
        Query::NoticeDelivery { seq } => {
            let delivery = state
                .notice_to_core(seq)
                .ok_or_else(|| format!("notice {seq} is not waiting to be handed to claims"))?;
            json((delivery.partition, delivery.key, delivery.caller, delivery.command))
        }
        Query::NoticeRetries { seq } => json(state.notice_retries(seq)),
        Query::Capacity { reservation_id } => json(state.capacity(&reservation_id)),
        Query::FacultativeTerms { reservation_id } => json(state.facultative_terms(&reservation_id)),
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
        Query::SweepCheck { period_end } => {
            let items = state.sweep_check(period_end);
            json(serde_json::json!({"green": items.is_empty(), "items": items}))
        }
        Query::Exceptions => json(state.exceptions()),
        Query::Abandoned => json(state.abandoned()),
        Query::AbandonedNotices => json(state.notices_abandoned()),
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
