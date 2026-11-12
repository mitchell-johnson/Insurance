//! The documents module as the cluster hosts it.
//!
//! The state machine runs in its own replicated group on the ledger nodes
//! (`ic_raft::module`). The host feeds it every partition's events; the
//! drivers send it commands and ask it questions through
//! `ic_raft::ClusterClient`.

use chrono::{Duration, NaiveDate, NaiveTime};
use ic_core::{PartyId, PolicyId};
use ic_raft::{hosted_reading, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::{Command, Envelope};
use crate::kinds::DocumentKind;
use crate::state::DocumentsState;
use crate::template::TemplateRef;

/// The module's permanent name on the cluster.
pub const NAME: &str = "documents";

/// The most a single answer carries.
pub const MAX_ANSWER: usize = 500;

/// A question the module answers from its state. Queries never change it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    /// One document's record.
    Document {
        id: String,
    },
    /// Everything a party may see, newest first.
    Inbox {
        party: PartyId,
    },
    PolicyDocuments {
        policy: PolicyId,
    },
    ScheduleHistory {
        policy: PolicyId,
    },
    /// May what a notice warns of take effect on a date?
    NoticeGate {
        policy: PolicyId,
        purpose: String,
        on: NaiveDate,
    },
    /// The template version in force for an id on a date.
    TemplateFor {
        id: String,
        on: NaiveDate,
    },
    Template {
        template: TemplateRef,
    },
    /// The parts a template version is composed from, to render it.
    Parts {
        template: TemplateRef,
    },
    PartyKey {
        party: PartyId,
    },
    PendingSends {
        limit: usize,
    },
    /// The send in progress that a provider's reference belongs to.
    SendByReference {
        reference: String,
    },
    /// How long documents of a kind are kept.
    Retention {
        kind: DocumentKind,
    },
    PendingEvents {
        limit: usize,
    },
    /// Instructions for the core not yet acknowledged.
    CoreOutbox {
        limit: usize,
    },
    /// Vault objects waiting to be deleted.
    PendingDeletes {
        limit: usize,
    },
    /// Documents the core's events require and nobody has issued yet.
    Triggers {
        limit: usize,
    },
    /// The parties with an interest in a policy standing on a date, who
    /// are owed a copy of its arrears, suspension and lapse notices.
    InterestedParties {
        policy: PolicyId,
        on: NaiveDate,
    },
    Defect {
        id: String,
    },
    /// The proven notices of a template version in a period.
    NoticesByTemplate {
        template: TemplateRef,
        from: NaiveDate,
        to: NaiveDate,
    },
    Report {
        from: NaiveDate,
        to: NaiveDate,
    },
    Settings,
    StateHash,
}

fn json<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Answer a question. A question that is not one of [`Query`] is refused.
pub fn answer(state: &DocumentsState, question: Value) -> Result<Value, String> {
    let query: Query = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    let cap = |limit: usize| limit.min(MAX_ANSWER);
    match query {
        Query::Document { id } => json(state.documents.get(&id)),
        Query::Inbox { party } => json(state.inbox(&party).into_iter().take(MAX_ANSWER).collect::<Vec<_>>()),
        Query::PolicyDocuments { policy } => json(state.policy_documents(&policy)),
        Query::ScheduleHistory { policy } => json(state.schedule_history(&policy)),
        Query::NoticeGate { policy, purpose, on } => json(state.notice_gate(&policy, &purpose, on)),
        Query::TemplateFor { id, on } => json(state.template_for(&id, on)),
        Query::Template { template } => json(state.templates.get(&template)),
        Query::Parts { template } => {
            let record = state.templates.get(&template).ok_or("no such template")?;
            json(record.template.parts.iter().filter_map(|r| state.parts.get(r)).collect::<Vec<_>>())
        }
        Query::PartyKey { party } => json(state.party_keys.get(&party)),
        Query::PendingSends { limit } => json(state.pending_sends(cap(limit))),
        Query::SendByReference { reference } => json(state.find_send(&reference)),
        Query::Retention { kind } => json(state.retention_rule(kind)),
        Query::PendingEvents { limit } => json(state.pending_events(cap(limit))),
        Query::CoreOutbox { limit } => json(state.core_outbox.iter().take(cap(limit)).collect::<Vec<_>>()),
        Query::PendingDeletes { limit } => json(state.pending_deletes.iter().take(cap(limit)).collect::<Vec<_>>()),
        Query::Triggers { limit } => json(state.triggers.values().take(cap(limit)).collect::<Vec<_>>()),
        Query::InterestedParties { policy, on } => json(state.interested_parties(&policy, on)),
        Query::Defect { id } => json(state.defects.get(&id)),
        Query::NoticesByTemplate { template, from, to } => json(state.notices_by_template(&template, from, to)),
        Query::Report { from, to } => json(state.report(from, to)),
        Query::Settings => json(&state.settings),
        Query::StateHash => json(state.state_hash()),
    }
}

/// What one core event becomes: the module is shown it as a command of its
/// own. The event carries no clock, so the command carries the first hour
/// of its business date in New Zealand (midday UTC the day before), which
/// can never be ahead of the real time. The state's own time never runs
/// backwards, so an event moves it only when nothing later has been seen.
pub fn react(record: &ic_core::EventRecord) -> Vec<Envelope> {
    vec![Envelope {
        key: format!("core:{}", record.id),
        caller: "relay".to_string(),
        actor: "module-host".to_string(),
        at: record.business_date.and_time(NaiveTime::MIN).and_utc() - Duration::hours(12),
        command: Command::ObserveCoreEvents {
            partition: record.partition,
            records: vec![record.clone()],
        },
    }]
}

fn make() -> Box<dyn ModuleState> {
    hosted_reading(DocumentsState::new(), answer, react)
}

/// The module as a node hosts it: listed in `ic_node::modules()`.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(NAME, make)?.reading_events())
}
