//! What the module host needs from the module: how an event from the core
//! becomes a command for it, and how questions about its state are answered.
//! The deployed node hosts the module from [`module_spec`].

use ic_core::event::Event as CoreEvent;
use ic_core::{EventRecord, LayerOrigin};
use ic_raft::{hosted_reading, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::command::{caller, Envelope, GroupCommand};
use crate::event::Outbound;
use crate::state::GroupState;

/// The module's permanent name on the cluster.
pub const MODULE: &str = "group";

fn make() -> Box<dyn ModuleState> {
    hosted_reading(GroupState::default(), answer, react)
}

/// The group scheme module, for a ledger node to host. It reads the core's
/// events: the business date, scheme invoices and balances, and the layers
/// the core writes for underwritten cover.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events())
}

/// The commands one event from a partition's stream turns into. Most events
/// are nothing to this module. The key is the event's own ID, so an event
/// fed twice changes nothing.
pub fn react(record: &EventRecord) -> Vec<Envelope> {
    let command = match &record.event {
        CoreEvent::BusinessDateAdvanced { .. } => GroupCommand::Tick,
        CoreEvent::SchemeInvoiceChunk { .. } | CoreEvent::PremiumAdjusted { .. } | CoreEvent::LoadingAdjusted { .. } | CoreEvent::PolicyBalance { .. } => {
            GroupCommand::ObserveCore {
                event: Box::new(record.event.clone()),
            }
        }
        CoreEvent::LayerAdded {
            origin: LayerOrigin::UnderwrittenExcess,
            ..
        } => GroupCommand::ObserveCore {
            event: Box::new(record.event.clone()),
        },
        _ => return vec![],
    };
    // The day a date change opens is the new date; every other event
    // belongs to the day it happened on.
    let today = match &record.event {
        CoreEvent::BusinessDateAdvanced { to, .. } => *to,
        _ => record.business_date,
    };
    vec![Envelope {
        key: format!("core:{}", record.id),
        caller: caller::DRIVER.to_string(),
        actor: "core-feed".to_string(),
        today,
        command,
    }]
}

/// The questions the driver, the portals and operations ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum Question {
    /// What the driver has to deliver, oldest first.
    Outbox { limit: usize },
    /// A scheme's standing: terms, counts, what is open.
    Scheme { scheme: String },
    /// One file with its preview and row-level results.
    File { scheme: String, file: String },
    /// The invoices kept for a scheme, as the employer sees them.
    Invoices { scheme: String },
    /// Open exceptions, for one scheme or all.
    Exceptions { scheme: Option<String> },
}

pub fn answer(state: &GroupState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question this module answers: {e}"))?;
    let scheme = |id: &str| state.schemes.get(id).ok_or_else(|| format!("scheme {id} is not registered"));
    Ok(match question {
        Question::Outbox { limit } => {
            let items: Vec<(u64, &Outbound)> = state.outbox.iter().take(limit).map(|(id, o)| (*id, o)).collect();
            json!({ "items": items, "more": state.outbox.len() > limit })
        }
        Question::Scheme { scheme: id } => {
            let s = scheme(&id)?;
            json!({
                "scheme": s.id,
                "policy": s.policy,
                "status": s.status,
                "blocked": s.blocked,
                "frozen": s.frozen,
                "terms_version": s.terms_version,
                "terms": s.terms,
                "members": s.membership.active_count(),
                "open_file": s.open_file,
                "queued": s.queue.len(),
                "scheduled": s.scheduled.len(),
                "in_flight": s.in_flight.is_some(),
                "possible_matches": s.possible.len(),
                "open_offers": s.offers.values().filter(|o| o.status == crate::state::OfferStatus::Open).count(),
                "renewal_open": s.renewal.is_some(),
                "outstanding_minor": s.outstanding_minor,
                "oldest_unpaid_due": s.oldest_unpaid_due,
                "short_payments": s.short_payments,
                "joiners_suspended": s.joiners_suspended,
            })
        }
        Question::File { scheme: id, file } => {
            let f = scheme(&id)?
                .files
                .get(&file)
                .ok_or_else(|| format!("file {file} is not known for scheme {id}"))?;
            json!({
                "file": f.id,
                "status": f.status,
                "received": f.received,
                "summary": f.summary,
                "rejected": f.rejected,
                "ambiguities": f.ambiguities,
                "applied": f.applied,
                "refused": f.refused,
                "scheduled": f.scheduled,
                "code": f.code,
            })
        }
        Question::Invoices { scheme: id } => json!({ "invoices": scheme(&id)?.invoices }),
        Question::Exceptions { scheme: only } => {
            let open: Vec<_> = state
                .exceptions
                .iter()
                .filter(|(_, e)| only.as_ref().is_none_or(|s| *s == e.scheme))
                .map(|(id, e)| json!({ "id": id, "scheme": e.scheme, "kind": e.kind, "subject": e.subject, "raised": e.raised }))
                .collect();
            json!({ "exceptions": open })
        }
    })
}
