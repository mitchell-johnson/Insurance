//! What a ledger node needs to host this module in its own replicated group:
//! how one of the core's events becomes a command, the questions the module
//! answers, and the two envelopes its driver sends.
//!
//! The node's outbox carrier is the driver ([`OUTBOX`]). It asks `outbox`,
//! sends each posting to the core with [`posting_envelope`], and reports
//! what became of it: delivered, or refused with [`refusal`]. Everything it
//! needs to carry on after a crash is in the module's replicated state.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyPosting, PartyLedger};
use ic_core::{EventRecord, PartyId, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_raft::client::fresh_salt;
use ic_raft::{hosted_reading, CoreOutcome, CorePosting, ModuleSpec, ModuleState, Outbox, Route};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::MoveRequest;
use crate::state::{Actor, Command, DistributionState, Envelope};

/// The module's permanent name on the ledger nodes.
pub const MODULE: &str = "distribution";
/// The caller name the core knows this module by.
pub const CORE_CALLER: &str = "distribution";
const EVENT_RELAY: &str = "event-relay";

/// What can be asked of the module. Questions never change state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Question {
    /// Everything the workbench shows for one party.
    Position {
        party: PartyId,
        as_at: NaiveDate,
    },
    /// The proof of debt extract for a liquidator.
    ProofOfDebt {
        party: PartyId,
        as_at: NaiveDate,
    },
    Statement {
        payee: PartyId,
        number: u32,
    },
    /// The held total across all parties, aged.
    HeldControl {
        date: NaiveDate,
    },
    /// Parties whose aged held balance blocks month-end sign-off.
    MonthEndBlocks {
        date: NaiveDate,
    },
    /// Initial commission still at risk of clawback, by the party that would bear it.
    ClawbackExposure {
        date: NaiveDate,
    },
    /// What a premium would earn for each payee: commission disclosure.
    DryRun {
        policy: PolicyId,
        date: NaiveDate,
        net_by_cover_type: BTreeMap<String, i64>,
    },
    /// The changes that would carry out an adviser's move, to be proposed.
    PlanMove(Box<MoveRequest>),
    /// Postings asked of the core and not yet confirmed: the driver's work list.
    Unconfirmed,
    /// Held balances by party and reason with their age and any documented
    /// decision: what finance's month-end aged-items check is sent.
    AgedHeld {
        as_at: NaiveDate,
    },
    /// Whether every swept amount has been paid or returned: clearing
    /// accounts with a balance and no payment outstanding are listed.
    SweepCheck,
    Action {
        id: String,
    },
    Case {
        id: String,
    },
    OpenTasks,
    /// What has been let go of to keep the state bounded: the date line
    /// detail starts from, brought-forward balances and the last run.
    History,
    Status,
    /// Every conduct case a licence event opened, with the event and the
    /// affected population: what the complaints module's conduct register
    /// is fed from. Customers are not held here; the policies are.
    ConductCases,
    /// Every layer the replacement rule matched to cover given up on the
    /// same life, with who wrote it, the reason, and whether it is disputed
    /// or was overturned: what oversight and conduct reporting count.
    ReplacementMatches,
    /// Every dated link in the structure: who sits under, is licensed by or
    /// is paid for whom, and from when to when.
    Relations,
    /// The oldest postings still to be carried to the core: what the host's
    /// outbox carrier asks.
    Outbox {
        limit: usize,
    },
    /// Parties that changed after a module sequence, with the sequence to
    /// ask from next time. A publisher keeps the sequence as its cursor.
    ChangedParties {
        since: u64,
    },
    /// Engagements, all of them or one adviser's, each with the licence
    /// problem that stops it being used on the day, if there is one.
    Engagements {
        #[serde(default)]
        adviser: Option<PartyId>,
        as_at: NaiveDate,
    },
}

/// Answer one question. An `Err` is a question that could not be understood.
pub fn answer(state: &DistributionState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    ask(state, question)
}

fn value<T: Serialize>(v: &T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// Answer a typed question.
pub fn ask(state: &DistributionState, question: Question) -> Result<Value, String> {
    match question {
        Question::Position { party, as_at } => value(&state.position(&party, as_at)),
        Question::ProofOfDebt { party, as_at } => value(&state.proof_of_debt(&party, as_at)),
        Question::Statement { payee, number } => value(&state.statements.get(&format!("{payee}.{number}"))),
        Question::HeldControl { date } => value(&state.held_control(date)),
        Question::MonthEndBlocks { date } => value(&state.month_end_blocks(date)),
        Question::ClawbackExposure { date } => value(&state.clawback_exposure(date)),
        Question::DryRun {
            policy,
            date,
            net_by_cover_type,
        } => value(&state.dry_run(&policy, date, &net_by_cover_type)),
        Question::PlanMove(request) => match state.plan_move(&request) {
            Ok(changes) => Ok(json!({ "changes": changes })),
            Err(reject) => Ok(json!({ "refused": reject })),
        },
        Question::Unconfirmed => value(&state.unconfirmed()),
        Question::AgedHeld { as_at } => Ok(aged_held(state, as_at)),
        Question::SweepCheck => {
            let stray: Vec<Value> = state
                .positions
                .keys()
                .filter(|p| !state.payment_outstanding(p))
                .filter_map(|p| {
                    let clearing = state.balance(p, PartyLedger::Clearing);
                    (clearing != 0).then(|| json!({ "party": p, "clearing_minor": clearing }))
                })
                .collect();
            Ok(json!({ "green": stray.is_empty(), "items": stray }))
        }
        Question::Action { id } => value(&state.actions.get(&id)),
        Question::Case { id } => value(&state.cases.get(&id)),
        Question::OpenTasks => value(&state.tasks.values().filter(|t| t.done.is_none()).collect::<Vec<_>>()),
        Question::ReplacementMatches => {
            let rows: Vec<Value> = state
                .layers
                .iter()
                .filter_map(|(key, l)| l.replacement.as_ref().map(|m| (key, l, m)))
                .map(|(key, l, m)| {
                    serde_json::json!({
                        "layer": key,
                        "policy": l.policy,
                        "writing": l.writing.iter().map(|(e, _)| e).collect::<Vec<_>>(),
                        "matched_minor": m.matched_minor,
                        "sum_insured_minor": l.sum_insured_minor,
                        "reason": m.reason,
                        "on": m.on,
                        "replaced": m.replaced.iter().map(|(k, _)| k).collect::<Vec<_>>(),
                        "disputed": m.disputed,
                        "overturned": m.overturned,
                    })
                })
                .collect();
            Ok(Value::Array(rows))
        }
        Question::ConductCases => {
            let structure = &state.config.structure;
            let rows: Vec<Value> = state
                .cases
                .values()
                .filter(|c| c.kind == crate::state::CaseKind::Conduct)
                .map(|c| {
                    let event = structure
                        .parties
                        .get(&c.subject)
                        .and_then(|p| p.licence.iter().rev().find(|r| r.source_ref == c.source_ref && r.effective <= c.opened));
                    let advisers: std::collections::BTreeSet<&PartyId> = c
                        .engagements
                        .iter()
                        .filter_map(|id| structure.engagements.get(id))
                        .map(|e| &e.adviser)
                        .collect();
                    json!({
                        "case": c.id,
                        "licence_event": c.source_ref,
                        "party": c.subject,
                        "status": event.map(|r| r.state),
                        "effective": event.map(|r| r.effective),
                        "opened": c.opened,
                        "advisers": advisers,
                        "payees": c.payees,
                        "policies": c.policies,
                        "closed": c.closed,
                    })
                })
                .collect();
            Ok(json!(rows))
        }
        Question::Relations => value(&state.config.structure.relations),
        Question::Outbox { limit } => value(&state.outbox(limit)),
        Question::ChangedParties { since } => {
            let parties: Vec<&PartyId> = state.changed.iter().filter(|(_, at)| **at > since).map(|(p, _)| p).collect();
            Ok(json!({ "seq": state.seq, "parties": parties }))
        }
        Question::Engagements { adviser, as_at } => {
            let structure = &state.config.structure;
            let rows: Vec<Value> = structure
                .engagements
                .iter()
                .filter(|(_, e)| adviser.as_ref().is_none_or(|a| &e.adviser == a))
                .map(|(id, e)| {
                    json!({
                        "id": id,
                        "adviser": e.adviser,
                        "with": e.with,
                        "from": e.from,
                        "to": e.to,
                        "accreditations": e.accreditations,
                        "impairment": structure.engagement_impairment(id, as_at),
                    })
                })
                .collect();
            Ok(json!({ "as_at": as_at, "engagements": rows }))
        }
        Question::History => value(&state.history),
        Question::Status => Ok(json!({
            "today": state.today,
            "seq": state.seq,
            "checkpoints": state.checkpoints,
            "unconfirmed": state.unconfirmed().len(),
            "state_hash": state.state_hash(),
        })),
    }
}

/// The most items one aged report lists. The totals cover everything held.
pub const MAX_AGED_ITEMS: usize = 2_000;

fn aged_held(state: &DistributionState, as_at: NaiveDate) -> Value {
    let mut groups: BTreeMap<(PartyId, String), (i64, NaiveDate)> = BTreeMap::new();
    for h in state.held.iter().filter(|h| h.remaining_minor > 0 && h.date <= as_at) {
        let reason = h.reason.map_or("other".to_string(), |r| format!("{r:?}"));
        let e = groups.entry((h.party.clone(), reason)).or_insert((0, h.date));
        e.0 += h.remaining_minor;
        e.1 = e.1.min(h.date);
    }
    let total: i64 = groups.values().map(|(a, _)| *a).sum();
    let count = groups.len();
    let mut items: Vec<((PartyId, String), (i64, NaiveDate))> = groups.into_iter().collect();
    // Oldest first, so what is cut off at the cap is the newest.
    items.sort_by_key(|(_, (_, since))| *since);
    items.truncate(MAX_AGED_ITEMS);
    let items: Vec<Value> = items
        .into_iter()
        .map(|((party, reason), (amount_minor, since))| {
            json!({
                "action": state.held_decision(&party, as_at),
                "party": party,
                "reason": reason,
                "amount_minor": amount_minor,
                "since": since,
                "owner": "counterparty_finance",
            })
        })
        .collect();
    json!({ "kind": "held_balance", "as_at": as_at, "items": items, "total_minor": total, "count": count })
}

/// One of the core's events as the module's own command. The key is the
/// event's ID, so an event offered twice is taken in once.
pub fn react(record: &EventRecord) -> Vec<Envelope> {
    vec![Envelope {
        key: format!("event:{}", record.id),
        actor: Actor::Module(EVENT_RELAY.to_string()),
        command: Command::Ingest(Box::new(record.clone())),
    }]
}

/// The core command that posts what the module asked for. The key is the
/// posting's own reference, so a retry after a timeout posts once.
pub fn posting_envelope(posting: CounterpartyPosting, salt: String) -> ic_core::Envelope {
    ic_core::Envelope {
        key: format!("{CORE_CALLER}:{}", posting.reference),
        caller: CORE_CALLER.to_string(),
        salt,
        traceparent: None,
        command: ic_core::Command::PostCounterparty(Box::new(posting)),
    }
}

/// Tell the module the core refused one of its postings.
pub fn refusal(reference: &str, reject: &Reject) -> Envelope {
    Envelope {
        key: format!("refused:{reference}"),
        actor: Actor::Module(EVENT_RELAY.to_string()),
        command: Command::PostingRefused {
            reference: reference.to_string(),
            code: reject.code.clone(),
            message: reject.message.clone(),
        },
    }
}

fn outbox_pending(limit: usize) -> Value {
    json!({ "outbox": { "limit": limit } })
}

fn outbox_read(answer: Value) -> Result<Vec<CorePosting>, String> {
    let items: Vec<(PartitionId, CounterpartyPosting)> = serde_json::from_value(answer).map_err(|e| format!("not an outbox answer: {e}"))?;
    Ok(items
        .into_iter()
        .map(|(partition, posting)| CorePosting {
            id: posting.reference.clone(),
            route: Route::Partition(partition),
            envelope: posting_envelope(posting, fresh_salt()),
        })
        .collect())
}

fn outbox_outcome(posting: &CorePosting, outcome: &CoreOutcome) -> Value {
    let envelope = match outcome {
        CoreOutcome::Applied { .. } => Envelope {
            key: format!("delivered:{}", posting.id),
            actor: Actor::Module(EVENT_RELAY.to_string()),
            command: Command::PostingDelivered { reference: posting.id.clone() },
        },
        CoreOutcome::Refused(reject) => refusal(&posting.id, reject),
        CoreOutcome::Unroutable(why) => refusal(&posting.id, &Reject::new("unroutable", why.clone())),
    };
    serde_json::to_value(envelope).unwrap_or(Value::Null)
}

/// How the host carries the module's postings to the core.
pub const OUTBOX: Outbox = Outbox {
    pending: outbox_pending,
    read: outbox_read,
    outcome: outbox_outcome,
};

fn make() -> Box<dyn ModuleState> {
    hosted_reading(DistributionState::default(), answer, react)
}

/// The distribution module, for a ledger node to host.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events().posting_to_core(OUTBOX))
}
