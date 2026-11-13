//! What the module host needs from the module: how an event from the core
//! becomes a command for it, and how questions about its state are answered.
//! The deployed node hosts the module from [`module_spec`].

use ic_core::event::Event as CoreEvent;
use ic_core::{EventRecord, LayerOrigin, PolicyId};
use ic_raft::{hosted_reading, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::benefit::member_cover;
use crate::command::{caller, Envelope, GroupCommand};
use crate::event::Outbound;
use crate::mapping::{FileKind, RowReject};
use crate::record::MemberRow;
use crate::state::{compare, CaseStatus, FileStatus, GroupState, MAX_ROWS_PER_PART};

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
        }
        | CoreEvent::MemberCoverLapsed { .. } => GroupCommand::ObserveCore {
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
    /// What the driver has to deliver, oldest first, after an entry it
    /// has already looked at.
    Outbox {
        limit: usize,
        #[serde(default)]
        after: Option<u64>,
    },
    /// A scheme's standing: terms, counts, what is open.
    Scheme { scheme: String },
    /// One file with its preview and row-level results.
    File { scheme: String, file: String },
    /// The invoices kept for a scheme, as the employer sees them.
    Invoices { scheme: String },
    /// Open exceptions, for one scheme or all.
    Exceptions { scheme: Option<String> },
    /// Files waiting for the driver: stored and not yet scanned, or scanned
    /// and not yet read.
    FilesToRead,
    /// The mappings a scheme's files are read with: one layout, or one for
    /// full files and one for change files.
    Mapping { scheme: String },
    /// What a file's rows would do to the scheme as it stands, without
    /// storing anything: the trial of a proposed mapping against a sample.
    Preview {
        scheme: String,
        kind: FileKind,
        rows: Vec<MemberRow>,
        #[serde(default)]
        rejected: Vec<RowReject>,
    },
    /// The schemes written on a policy: a portal knows an employer by the
    /// scheme policy it administers.
    SchemeOf { policy: PolicyId },
    /// One member as the employer may see them: what the employer told us
    /// and what the member is covered for. Never date of birth, sex or
    /// smoker status, a loading, an exclusion, or why underwriting decided
    /// as it did.
    Member { scheme: String, employee: String },
}

/// A file the driver has work to do on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileToRead {
    pub scheme: String,
    pub file: String,
    pub status: FileStatus,
    pub content_hash: String,
}

pub fn answer(state: &GroupState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question this module answers: {e}"))?;
    let scheme = |id: &str| state.schemes.get(id).ok_or_else(|| format!("scheme {id} is not registered"));
    Ok(match question {
        Question::Outbox { limit, after } => {
            let from = after.map_or(0, |a| a.saturating_add(1));
            let items: Vec<(u64, &Outbound)> = state.outbox.range(from..).take(limit).map(|(id, o)| (*id, o)).collect();
            json!({ "items": items, "more": state.outbox.range(from..).count() > limit })
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
        Question::FilesToRead => {
            let files: Vec<FileToRead> = state
                .schemes
                .values()
                .flat_map(|s| s.files.values().map(move |f| (s, f)))
                .filter(|(_, f)| matches!(f.status, FileStatus::Quarantined | FileStatus::Scanned))
                .map(|(s, f)| FileToRead {
                    scheme: s.id.clone(),
                    file: f.id.clone(),
                    status: f.status,
                    content_hash: f.content_hash.clone(),
                })
                .collect();
            json!({ "files": files })
        }
        Question::Mapping { scheme: id } => {
            let s = scheme(&id)?;
            json!({ "mapping": s.mapping, "second_mapping": s.second_mapping })
        }
        Question::Preview {
            scheme: id,
            kind,
            rows,
            rejected,
        } => {
            let s = scheme(&id)?;
            if rows.len() > MAX_ROWS_PER_PART {
                return Err(format!("a sample has at most {MAX_ROWS_PER_PART} rows"));
            }
            let today = state.today();
            let compared = compare(s, today, kind, today, &rows, rejected);
            json!({ "summary": compared.summary, "rejected": compared.rejected, "ambiguities": compared.ambiguities, "would_be_held": compared.held })
        }
        Question::SchemeOf { policy } => {
            let schemes: Vec<&String> = state.by_policy.get(&policy).into_iter().collect();
            json!({ "schemes": schemes })
        }
        Question::Member { scheme: id, employee } => {
            let s = scheme(&id)?;
            let m = s
                .membership
                .members
                .get(&employee)
                .ok_or_else(|| format!("{employee} is not a member of scheme {id}"))?;
            let in_force = m.in_force_on(state.today());
            let covers = if !in_force {
                Vec::new()
            } else {
                member_cover(&s.product, &m.attrs, m.auto_cap_minor)
            };
            // Cover above the automatic amount: how much, and whether it is
            // in place. A decline and a request not proceeded with look the same.
            let above: Vec<Value> = s
                .cases
                .values()
                .filter(|c| c.employee_id == employee && c.life == m.life && c.status != CaseStatus::Ended)
                .map(|c| {
                    let (state, in_place_minor) = match c.status {
                        CaseStatus::Requested | CaseStatus::Writing => ("pending", 0),
                        CaseStatus::Accepted => ("in_place", c.accepted_minor),
                        _ => ("not_in_place", 0),
                    };
                    json!({ "cover": c.cover, "asked_minor": c.excess_minor, "state": state, "in_place_minor": in_place_minor, "from": c.effective })
                })
                .collect();
            json!({
                "scheme": s.id,
                "employee": m.employee_id,
                "category": m.attrs.category,
                "salary_minor": m.attrs.salary_minor,
                "hours_hundredths": m.attrs.hours_hundredths,
                "billing_group": m.attrs.billing_group,
                "occupation_class": m.attrs.occupation_class,
                "employment_start": m.attrs.employment_start,
                "joined": m.joined,
                "left": m.left,
                "in_force": in_force,
                "on_leave_since": m.absence_since,
                "covers": covers.iter().map(|c| json!({
                    "cover": c.cover,
                    "cover_type": c.cover_type,
                    "formula_minor": c.formula_minor,
                    "automatic_minor": c.automatic_minor,
                })).collect::<Vec<_>>(),
                "above_automatic": above,
            })
        }
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
