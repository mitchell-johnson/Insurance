//! What a ledger node needs to host this module in its own replicated group:
//! how one of the core's events becomes a command, the questions the module
//! answers, and the two envelopes its driver sends.
//!
//! The driver is small and stateless. It asks `unconfirmed`, sends each
//! posting to the core with [`posting_envelope`], and reports a refusal back
//! with [`refusal`]. Everything it needs to carry on after a crash is in the
//! module's replicated state.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyPosting, PartyLedger};
use ic_core::{EventRecord, PartyId, PolicyId, Reject};
use ic_raft::{hosted_reading, ModuleSpec, ModuleState};
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
    Status,
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

fn make() -> Box<dyn ModuleState> {
    hosted_reading(DistributionState::default(), answer, react)
}

/// The distribution module, for a ledger node to host.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events())
}
