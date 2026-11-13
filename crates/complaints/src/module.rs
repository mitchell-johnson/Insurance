//! The complaints module as the cluster hosts it: the questions it
//! answers, and its spec for the deployed node.

use chrono::NaiveDate;
use ic_core::PartyId;
use ic_raft::{hosted, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::ComplaintsState;

/// The module's permanent name on the cluster.
pub const NAME: &str = "complaints";

/// The most a single answer carries.
pub const MAX_ANSWER: usize = 500;

/// Who is asking, as the API layer established it. Conduct cases are a
/// restricted case type: only the conduct function and legal see them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Complaints,
    Conduct,
    Legal,
    Other,
}

impl Role {
    fn sees_conduct(self) -> bool {
        matches!(self, Role::Conduct | Role::Legal)
    }
}

/// A question the module answers from its state. Queries never change it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    /// The whole complaint, for someone the case has given access. The
    /// answer is refused to anyone else.
    Complaint {
        id: String,
        actor: String,
    },
    /// The record a complainant is given under their right of access.
    SubjectRecord {
        id: String,
    },
    /// What an adviser may see of complaints about their service.
    AdviserView {
        engagement: String,
    },
    Queue {
        limit: usize,
    },
    /// The timers running on a complaint, with their due dates.
    Running {
        id: String,
    },
    ComplaintsOf {
        party: PartyId,
    },
    Register {
        from: NaiveDate,
        to: NaiveDate,
    },
    Report {
        from: NaiveDate,
        to: NaiveDate,
    },
    ConductCase {
        id: String,
        role: Role,
    },
    OpenConductCases {
        role: Role,
    },
    Timers {
        version: u32,
    },
    PendingEvents {
        limit: usize,
    },
    /// The letters owed that nobody has issued, for the documents service.
    /// Names parties and dates, never what a complaint is about.
    LettersDue {
        limit: usize,
    },
    /// Written outcomes not yet proven delivered, for the documents service.
    OutcomesAwaitingDelivery {
        limit: usize,
    },
    StateHash,
}

fn json<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Answer a question. A question that is not one of [`Query`] is refused.
pub fn answer(state: &ComplaintsState, question: Value) -> Result<Value, String> {
    let query: Query = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    let restricted = || Err("conduct cases are open to the conduct function and legal only".to_string());
    match query {
        Query::Complaint { id, actor } => {
            if state.access(&id, &actor).is_empty() {
                return Err("the case has not given this person access".to_string());
            }
            json(state.complaints.get(&id))
        }
        Query::SubjectRecord { id } => json(state.subject_record(&id)),
        Query::AdviserView { engagement } => json(state.adviser_view(&engagement)),
        Query::Queue { limit } => json(state.queue(limit.min(MAX_ANSWER))),
        Query::LettersDue { limit } => json(state.letters_due(limit.min(MAX_ANSWER))),
        Query::OutcomesAwaitingDelivery { limit } => json(state.outcomes_awaiting_delivery(limit.min(MAX_ANSWER))),
        Query::Running { id } => json(state.complaints.get(&id).map(|c| state.running(c))),
        Query::ComplaintsOf { party } => json(state.complaints_of(&party)),
        Query::Register { from, to } => json(state.register(from, to)),
        Query::Report { from, to } => json(state.report(from, to)),
        Query::ConductCase { role, .. } | Query::OpenConductCases { role } if !role.sees_conduct() => restricted(),
        Query::ConductCase { id, .. } => json(state.conduct.get(&id)),
        Query::OpenConductCases { .. } => json(state.open_conduct.iter().take(MAX_ANSWER).collect::<Vec<_>>()),
        Query::Timers { version } => json(state.timers(version)),
        Query::PendingEvents { limit } => json(state.pending_events(limit.min(MAX_ANSWER))),
        Query::StateHash => json(state.state_hash()),
    }
}

fn make() -> Box<dyn ModuleState> {
    hosted(ComplaintsState::new(), answer)
}

/// The module as a node hosts it: listed in `ic_node::modules()`.
pub fn module_spec() -> Result<ModuleSpec, String> {
    ModuleSpec::new(NAME, make)
}
