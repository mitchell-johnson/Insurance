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
    /// The people a member might be, for staff deciding a possible match.
    /// Never served to the employer.
    PossibleMatch { scheme: String, employee: String },
    /// The underwritten cover whose loading the employer pays in one total,
    /// with the underwriting proposal behind each: what staff may ask a
    /// member to take over once too few members hold such cover. Never
    /// served to the employer: it says who has a loading.
    LoadingsInTotal { scheme: String },
    /// Whether a signed-in person may claim a membership: the employee ID
    /// is an active member whose birth date the employer gave is the
    /// person's, and whether it is already linked to them or to someone
    /// else. Nothing else about the member is said, and a wrong birth date
    /// reads the same as no such member.
    MemberClaim {
        scheme: String,
        employee: String,
        date_of_birth: chrono::NaiveDate,
        party: ic_core::PartyId,
    },
    /// The continuation offers open to a person, across schemes: for the
    /// member's own app. Each with what may be taken up.
    MemberOffers { party: ic_core::PartyId },
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
                "members_by_group": members_by_group(s),
                "cover_minor": cover_totals(s),
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
            let s = scheme(&id)?;
            let f = s.files.get(&file).ok_or_else(|| format!("file {file} is not known for scheme {id}"))?;
            json!({
                "file": f.id,
                "content_hash": f.content_hash,
                "status": f.status,
                "received": f.received,
                "kind": f.kind,
                "uploaded_by": f.uploaded_by,
                "approved_by": f.approved_by,
                "second_approver": f.second_approver || s.terms.second_approver,
                "summary": f.summary,
                "changes": listed(&f.changes),
                "rejected": f.rejected,
                "ambiguities": f.ambiguities,
                "applied": f.applied,
                "refused": f.refused,
                "scheduled": f.scheduled,
                "code": f.code,
            })
        }
        // Each invoice with its totals worked out, so nobody adds up the lines to see what is asked.
        Question::Invoices { scheme: id } => {
            let invoices: Vec<Value> = scheme(&id)?
                .invoices
                .iter()
                .map(|i| {
                    let mut v = json!(i);
                    v["members"] = json!(i.members());
                    v["total_minor"] = json!(i.total_minor());
                    v["amount_due_minor"] = json!(i.amount_due_minor());
                    v
                })
                .collect();
            json!({ "invoices": invoices })
        }
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
            json!({ "summary": compared.summary, "changes": listed(&compared.changes), "rejected": compared.rejected, "ambiguities": compared.ambiguities, "would_be_held": compared.held })
        }
        Question::MemberClaim {
            scheme: id,
            employee,
            date_of_birth,
            party,
        } => {
            let found = state
                .schemes
                .get(&id)
                .and_then(|s| s.membership.active(&employee))
                .filter(|m| m.attrs.date_of_birth == Some(date_of_birth));
            match found {
                None => json!({ "matches": false }),
                Some(m) => json!({ "matches": true, "linked": m.party.as_ref().map(|p| if *p == party { "you" } else { "someone_else" }) }),
            }
        }
        Question::MemberOffers { party } => {
            let today = state.today();
            let open: Vec<Value> = state
                .schemes
                .values()
                .flat_map(|s| s.offers.values().map(move |o| (s, o)))
                .filter(|(s, o)| {
                    o.status == crate::state::OfferStatus::Open
                        && today <= o.expires
                        && s.membership.by_life(&o.life).and_then(|m| m.party.as_ref()) == Some(&party)
                })
                .map(|(s, o)| {
                    let mut covers: Vec<String> = s.product.covers.iter().map(|c| c.cover_type.trim_start_matches("group_").to_string()).collect();
                    covers.dedup();
                    json!({ "scheme": s.id, "policy": s.policy, "offer": o.id, "left": o.left, "expires": o.expires, "max_sum_insured_minor": o.max_sum_insured_minor,
                        "products": s.terms.continuation.products, "covers": covers })
                })
                .collect();
            json!(open)
        }
        Question::LoadingsInTotal { scheme: id } => {
            let s = scheme(&id)?;
            let cases: Vec<Value> = s
                .cases
                .values()
                .filter(|c| c.end_from.is_none() && c.in_employer_total && matches!(c.status, CaseStatus::Writing | CaseStatus::Accepted))
                .map(|c| {
                    json!({
                        "case": c.id,
                        "employee": c.employee_id,
                        "cover": c.cover,
                        "proposal": format!("grp-uw:{}:{}", s.id, c.request_case()),
                    })
                })
                .collect();
            json!({ "scheme": s.id, "min_count": s.terms.excess_loading_min_count, "cases": cases })
        }
        Question::PossibleMatch { scheme: id, employee } => {
            let s = scheme(&id)?;
            let candidates = s
                .possible
                .get(&employee)
                .ok_or_else(|| format!("{employee} has no possible match waiting on scheme {id}"))?;
            let member = s.membership.members.get(&employee);
            json!({ "scheme": s.id, "employee": employee, "life": member.map(|m| m.life.clone()),
                "date_of_birth": member.and_then(|m| m.attrs.date_of_birth), "joined": member.map(|m| m.joined), "candidates": candidates })
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
                // The plan and tier set the premium; dependants are never named.
                "health": s.terms.health_menu.as_ref().map(|menu| {
                    let choice = m.health.as_ref();
                    json!({
                        "plan": choice.map_or(menu.default_plan.as_str(), |c| c.plan.as_str()),
                        "tier": choice.map_or(crate::terms::HealthTier::Single, |c| c.tier),
                        "since": choice.map(|c| c.effective),
                    })
                }),
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
                // For staff: the leaver's continuation offers and where each
                // stands. The employer portal does not pass this on.
                "continuation": s.offers.values().filter(|o| o.life == m.life).map(|o| json!({
                    "offer": o.id, "left": o.left, "expires": o.expires, "max_sum_insured_minor": o.max_sum_insured_minor, "status": json!(o.status)["status"],
                })).collect::<Vec<_>>(),
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

/// What each change in a file does, for the person approving it: the
/// member, what happens and from when, and the names of the fields that
/// change. Never the values, so nothing about health or identity shows.
fn listed(changes: &[crate::model::Proposed]) -> Vec<Value> {
    changes
        .iter()
        .map(|c| {
            let from = if let crate::model::ChangeKind::Rekey { from } = &c.kind {
                Some(from.as_str())
            } else {
                None
            };
            json!({ "row": c.row, "employee": c.employee_id, "change": c.kind.code(), "effective": c.effective, "fields": c.fields, "previously": from })
        })
        .collect()
}

/// Active members per billing group.
fn members_by_group(s: &crate::state::Scheme) -> std::collections::BTreeMap<String, u32> {
    let mut out = std::collections::BTreeMap::new();
    for m in s.membership.active_members() {
        let group = m
            .attrs
            .billing_group
            .clone()
            .filter(|g| s.terms.has_billing_group(g))
            .unwrap_or_else(|| s.terms.default_billing_group().to_string());
        *out.entry(group).or_insert(0u32) += 1;
    }
    out
}

/// The scheme's cover in force by cover code: each active member's
/// automatic amount and the underwritten cover accepted on top.
fn cover_totals(s: &crate::state::Scheme) -> std::collections::BTreeMap<String, i64> {
    let mut out = std::collections::BTreeMap::new();
    for m in s.membership.active_members() {
        for c in crate::benefit::member_cover(&s.product, &m.attrs, m.auto_cap_minor) {
            *out.entry(c.cover).or_insert(0i64) += c.automatic_minor;
        }
    }
    for c in s.cases.values().filter(|c| c.status == crate::state::CaseStatus::Accepted) {
        *out.entry(c.cover.clone()).or_insert(0i64) += c.counts_minor();
    }
    out
}
