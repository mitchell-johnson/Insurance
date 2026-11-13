//! The module as the cluster hosts it: the questions it answers, the core
//! events it reads, and the outbox the host carries to the core.

use chrono::{NaiveDate, Utc};
use ic_core::{Event as CoreEvent, PartyId, PolicyId, Standing};
use ic_raft::{hosted_reading, CoreOutcome, CorePosting, ModuleSpec, ModuleState, Outbox, Route};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::machine::{CollectionsState, Command, CoreFact, Envelope, CORE_EVENTS, CORE_OUTBOX};
use crate::model::{HardshipCase, Hold, Notice, NoticeState, Posting, Role, Task};

/// The module's permanent name on the cluster.
pub const MODULE: &str = "collections";

/// The caller the module's postings carry to the core.
pub const CORE_CALLER: &str = "collections";

/// The most a single answer carries.
pub const MAX_ANSWER: usize = 500;

/// What an officer is not shown, said in every answer that withholds it.
pub const WITHHELD_FROM_OFFICERS: &[&str] = &["hardship_note", "health", "claims", "underwriting"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "q", rename_all = "snake_case")]
pub enum Question {
    /// The day's work, ordered by days to lapse and then annual premium.
    Queue {
        as_at: NaiveDate,
        role: Role,
        limit: usize,
    },
    Policy {
        policy: PolicyId,
    },
    /// Notices written and not yet taken by the documents service.
    NoticesDue {
        limit: usize,
    },
    /// Notices taken and waiting for their delivery outcome, oldest first.
    NoticesSent {
        limit: usize,
    },
    /// Open tasks for a team, oldest first.
    Tasks {
        role: Role,
        limit: usize,
    },
    HardshipCase {
        case: String,
        role: Role,
    },
    /// Postings for the core not yet answered, oldest first.
    Outbox {
        limit: usize,
    },
    /// Products with policies and no sequence: nothing is sent for them.
    Unsequenced,
    /// Lapsed policies the core sent to underwriting for reinstatement
    /// (an open `reinstatement_underwriting` task names them). The
    /// underwriting driver opens a proposal for each, as caller
    /// `collections`; `duplicate_proposal` means it already did.
    ReinstatementsForUnderwriting,
    StateHash,
}

/// One line of the collections queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub policy: PolicyId,
    pub number: String,
    pub party: PartyId,
    pub product: String,
    /// `retail`, `arrangement`, `hardship` or `held`.
    pub segment: String,
    pub standing: Option<Standing>,
    pub days_in_arrears: i64,
    /// Unknown without a sequence, and while an arrangement holds the clock.
    pub days_to_lapse: Option<i64>,
    pub annual_premium_minor: i64,
    pub open_tasks: Vec<String>,
    /// An arrangement's next instalment date, for its review.
    pub review: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    pub as_at: NaiveDate,
    pub items: Vec<QueueItem>,
    /// What this answer leaves out for the asker's role.
    pub withheld: Vec<String>,
}

fn segment(p: &crate::model::Tracked) -> &'static str {
    match (p.kept.is_some(), p.relief.is_some(), p.hold) {
        (true, _, _) => "arrangement",
        (_, true, _) | (_, _, Some(Hold::HardshipReferral { .. })) => "hardship",
        (_, _, Some(_)) => "held",
        _ => "retail",
    }
}

fn withheld(role: Role) -> Vec<String> {
    if role == Role::Hardship {
        Vec::new()
    } else {
        WITHHELD_FROM_OFFICERS.iter().map(|s| s.to_string()).collect()
    }
}

impl CollectionsState {
    /// The queue as at a day. The queue holds ordinary and financial
    /// details only, whoever asks.
    pub fn queue(&self, as_at: NaiveDate, role: Role, limit: usize) -> Queue {
        let mut items: Vec<QueueItem> = self
            .in_arrears
            .iter()
            .filter_map(|id| {
                let p = self.policies.get(id).filter(|p| !p.ended)?;
                let age = p.age(as_at)?;
                let lapse = self.sequences.get(&p.product).map(|s| i64::from(s.lapse_day));
                let open_tasks = self
                    .open_tasks
                    .iter()
                    .filter(|t| self.tasks.get(*t).is_some_and(|t| t.policies.contains(id)))
                    .cloned()
                    .collect();
                Some(QueueItem {
                    policy: id.clone(),
                    number: p.number.clone(),
                    party: p.party.clone(),
                    product: p.product.clone(),
                    segment: segment(p).to_string(),
                    standing: p.standing,
                    days_in_arrears: age,
                    days_to_lapse: lapse.filter(|_| p.kept.is_none()).map(|l| l - age),
                    annual_premium_minor: p.annual_premium_minor,
                    open_tasks,
                    review: p.kept.as_ref().and_then(|k| k.dues.iter().find(|d| **d >= as_at).copied()),
                })
            })
            .collect();
        items.sort_by(|a, b| {
            let key = |i: &QueueItem| (i.days_to_lapse.unwrap_or(i64::MAX), -i.annual_premium_minor);
            key(a).cmp(&key(b)).then_with(|| a.policy.cmp(&b.policy))
        });
        items.truncate(limit.min(MAX_ANSWER));
        Queue {
            as_at,
            items,
            withheld: withheld(role),
        }
    }

    /// A hardship case. An officer reads it without the applicant's note
    /// when the note discloses health.
    pub fn hardship_case(&self, case: &str, role: Role) -> Option<Value> {
        let case = self.hardship.get(case)?;
        let mut shown: HardshipCase = case.clone();
        let hide = case.discloses_health && role != Role::Hardship;
        if hide {
            shown.note = String::new();
        }
        let mut value = serde_json::to_value(shown).ok()?;
        value["withheld"] = json!(if hide { vec!["hardship_note"] } else { Vec::<&str>::new() });
        Some(value)
    }

    pub fn notices_due(&self, limit: usize) -> Vec<&Notice> {
        self.notices_in(NoticeState::Due, limit)
    }

    /// Notices in a state, oldest first by day and then ID.
    pub fn notices_in(&self, state: NoticeState, limit: usize) -> Vec<&Notice> {
        let mut notices: Vec<&Notice> = self.notices.values().filter(|n| n.state == state).collect();
        notices.sort_by(|a, b| (a.on, &a.id).cmp(&(b.on, &b.id)));
        notices.truncate(limit.min(MAX_ANSWER));
        notices
    }

    pub fn tasks_for(&self, role: Role, limit: usize) -> Vec<&Task> {
        let mut tasks: Vec<&Task> = self
            .open_tasks
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .filter(|t| t.kind.team() == role || (role == Role::TeamLead && t.escalated))
            .collect();
        tasks.sort_by(|a, b| (a.opened, &a.id).cmp(&(b.opened, &b.id)));
        tasks.truncate(limit.min(MAX_ANSWER));
        tasks
    }
}

fn value<T: Serialize>(answer: T) -> Result<Value, String> {
    serde_json::to_value(answer).map_err(|e| e.to_string())
}

/// Answer a question. Anything that is not a [`Question`] is refused.
pub fn answer(state: &CollectionsState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    match question {
        Question::Queue { as_at, role, limit } => value(state.queue(as_at, role, limit)),
        Question::Policy { policy } => value(state.policies.get(&policy)),
        Question::NoticesDue { limit } => value(state.notices_due(limit)),
        Question::NoticesSent { limit } => value(state.notices_in(NoticeState::Sent, limit)),
        Question::Tasks { role, limit } => value(state.tasks_for(role, limit)),
        Question::HardshipCase { case, role } => value(state.hardship_case(&case, role)),
        Question::Outbox { limit } => value(state.outbox.values().take(limit.min(MAX_ANSWER)).collect::<Vec<_>>()),
        Question::Unsequenced => value(&state.unsequenced),
        Question::ReinstatementsForUnderwriting => value(state.reinstatements_for_underwriting()),
        Question::StateHash => value(state.state_hash()),
    }
}

/// The module's reading of one core event, if it is one the module follows.
pub fn fact(event: &CoreEvent) -> Option<CoreFact> {
    Some(match event {
        CoreEvent::PolicyIssued {
            policy,
            number,
            policyholder,
            product_code,
            ..
        } => CoreFact::Issued {
            policy: policy.clone(),
            number: number.clone(),
            party: policyholder.clone(),
            product: product_code.clone(),
        },
        CoreEvent::PremiumDue {
            policy,
            period_start,
            period_end,
            total_minor,
            ..
        } => CoreFact::PremiumDue {
            policy: policy.clone(),
            period_start: *period_start,
            period_end: *period_end,
            total_minor: *total_minor,
        },
        CoreEvent::StandingChanged {
            policy, to, oldest_unpaid_due, ..
        } => CoreFact::Standing {
            policy: policy.clone(),
            to: *to,
            oldest_unpaid_due: *oldest_unpaid_due,
        },
        CoreEvent::PolicyBalance { policy, oldest_unpaid_due, .. } => CoreFact::Balance {
            policy: policy.clone(),
            oldest_unpaid_due: *oldest_unpaid_due,
        },
        CoreEvent::PolicyStatusChanged { policy, to, .. } => CoreFact::Status {
            policy: policy.clone(),
            to: *to,
        },
        CoreEvent::ArrangementMade {
            policy,
            arrangement,
            arrears_minor,
            instalments,
        } => CoreFact::ArrangementMade {
            policy: policy.clone(),
            arrangement: arrangement.clone(),
            arrears_minor: *arrears_minor,
            dues: instalments.iter().map(|i| i.due).collect(),
        },
        CoreEvent::ArrangementCompleted { policy, arrangement } => CoreFact::ArrangementCompleted {
            policy: policy.clone(),
            arrangement: arrangement.clone(),
        },
        CoreEvent::ArrangementBroken {
            policy,
            arrangement,
            outstanding_minor,
        } => CoreFact::ArrangementBroken {
            policy: policy.clone(),
            arrangement: arrangement.clone(),
            outstanding_minor: *outstanding_minor,
        },
        CoreEvent::BusinessDateAdvanced { from: Some(from), .. } => CoreFact::DayClosed { closed: *from },
        CoreEvent::ReinstatementApproved { policy, .. } => CoreFact::ReinstatementApproved { policy: policy.clone() },
        _ => return None,
    })
}

/// Turn a core event into the module's command. The key is the event's ID,
/// so an event offered twice is taken in once.
pub fn react(record: &ic_core::EventRecord) -> Vec<Envelope> {
    let Some(fact) = fact(&record.event) else {
        return Vec::new();
    };
    vec![Envelope {
        key: format!("core-{}", record.id),
        caller: CORE_EVENTS.to_string(),
        actor: CORE_EVENTS.to_string(),
        roles: Vec::new(),
        at: record.business_date.and_time(chrono::NaiveTime::MIN).and_utc(),
        command: Command::Core {
            partition: record.partition,
            on: record.business_date,
            fact,
        },
    }]
}

/// The core command for one outbox item, under the module's key for it so
/// a second offer is applied once.
pub fn core_envelope(posting: &Posting, salt: String) -> ic_core::Envelope {
    ic_core::Envelope {
        key: format!("{CORE_CALLER}:{}", posting.id),
        caller: CORE_CALLER.to_string(),
        salt,
        traceparent: None,
        command: posting.command.clone(),
    }
}

fn outbox_pending(limit: usize) -> Value {
    json!({ "q": "outbox", "limit": limit })
}

fn outbox_read(answer: Value) -> Result<Vec<CorePosting>, String> {
    let items: Vec<Posting> = serde_json::from_value(answer).map_err(|e| format!("not an outbox answer: {e}"))?;
    Ok(items
        .iter()
        .map(|p| CorePosting {
            id: p.id.clone(),
            route: Route::Policy(p.policy.clone()),
            envelope: core_envelope(p, ic_raft::client::fresh_salt()),
        })
        .collect())
}

/// The command that tells the module what the core made of a posting.
pub fn outcome_command(posting: &CorePosting, outcome: &CoreOutcome) -> Envelope {
    let (applied, code, message) = match outcome {
        CoreOutcome::Applied { .. } => (true, None, None),
        CoreOutcome::Refused(reject) => (false, Some(reject.code.clone()), Some(reject.message.clone())),
        CoreOutcome::Unroutable(why) => (false, Some("unroutable".to_string()), Some(why.clone())),
    };
    Envelope {
        key: format!("outcome:{}", posting.id),
        caller: CORE_OUTBOX.to_string(),
        actor: CORE_OUTBOX.to_string(),
        roles: Vec::new(),
        at: Utc::now(),
        command: Command::RecordCoreOutcome {
            posting: posting.id.clone(),
            applied,
            code,
            message,
        },
    }
}

fn outbox_outcome(posting: &CorePosting, outcome: &CoreOutcome) -> Value {
    serde_json::to_value(outcome_command(posting, outcome)).unwrap_or(Value::Null)
}

/// How the host carries the module's arrangements and reliefs to the core.
pub const OUTBOX: Outbox = Outbox {
    pending: outbox_pending,
    read: outbox_read,
    outcome: outbox_outcome,
};

fn make() -> Box<dyn ModuleState> {
    hosted_reading(CollectionsState::new(), answer, react)
}

/// The module as a node hosts it: listed in `ic_node::modules()`.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events().posting_to_core(OUTBOX))
}
