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
    /// Documents whose channel rows are to be written (`true`) or taken
    /// down (`false`).
    Listing {
        limit: usize,
    },
    /// Where a shared part is used, and the products a new version of it
    /// has not reached.
    PartUsage {
        part: String,
    },
    /// Requested letters still to issue, oldest first.
    LettersRequested {
        limit: usize,
    },
    /// One requested letter and where it stands.
    LetterRequest {
        request: String,
    },
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
    /// The active template a product's document of a kind is rendered
    /// with on a date, if there is one.
    TemplateForKind {
        kind: crate::kinds::DocumentKind,
        product: String,
        on: chrono::NaiveDate,
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
    /// Every template version, by reference from the last, with where it stands: for
    /// staff to pick one rather than type its reference.
    Templates {
        limit: usize,
    },
    /// The legal holds in place, and the case each was placed for.
    LegalHolds,
    /// Notice defects still open, oldest first.
    OpenDefects {
        limit: usize,
    },
    Settings,
    StateHash,
    /// Documents a recipient of which could not be reached, newest first.
    Undeliverable {
        limit: usize,
    },
    /// The notices remediation asked to correct that have no correction
    /// yet, as (defect, notice).
    CorrectionsDue {
        limit: usize,
    },
    /// A template version rendered for a synthetic persona: the text a
    /// person approves, its SHA-256 for a rendering test, the fields the
    /// template needs and the fields the data gives. Without data, it is
    /// the data a product's document of the kind is given
    /// (`supplied::sample`).
    RenderSample {
        template: TemplateRef,
        #[serde(default)]
        language: Option<String>,
        #[serde(default)]
        data: Option<Value>,
    },
    /// What a person writing a template picks from: every kind of
    /// document and what it implies, the alternate formats, and the
    /// shipped placeholder wordings to start from.
    Authoring,
}

/// What [`Query::Authoring`] answers.
fn authoring() -> Value {
    let kinds: Vec<Value> = DocumentKind::ALL
        .iter()
        .map(|k| {
            serde_json::json!({
                "kind": k,
                "words": k.words(),
                "answers_events": crate::supplied::answers_events(*k),
                "proven": k.requires_proof(),
                "compliance": k.needs_compliance_approval(),
                "fields": crate::supplied::fields(*k),
            })
        })
        .collect();
    let placeholders: Vec<Value> = crate::placeholders::PLACEHOLDERS
        .iter()
        .map(|p| serde_json::json!({ "id": p.id, "kind": p.kind, "title": p.title, "notice_days": p.notice_days, "body": p.body }))
        .collect();
    serde_json::json!({
        "kinds": kinds,
        "formats": ["large_print", "plain_text", "audio", "braille"],
        "marker": crate::placeholders::MARKER,
        "placeholders": placeholders,
    })
}

fn json<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// A template rendered for a sample: what [`Query::RenderSample`] answers.
fn render_sample(state: &DocumentsState, template: &TemplateRef, language: Option<String>, data: Option<Value>) -> Result<Value, String> {
    let record = state
        .templates
        .get(template)
        .ok_or_else(|| format!("there is no template {} version {}", template.id, template.version))?;
    let t = &record.template;
    let language = language.unwrap_or_else(|| t.primary_language.clone());
    let data = data
        .or_else(|| crate::supplied::sample(t.kind))
        .ok_or_else(|| format!("a {} is given what the module that asks for it sends: give the sample's data", t.kind.words()))?;
    let text = t.render(&state.parts, &language, &data).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "template": template,
        "language": language,
        "text": text,
        "sha256": crate::template::sha256_hex(text.as_bytes()),
        "required_fields": record.required_fields,
        "product_fields": crate::render::supplied(&data),
        "reading_grade_x10": record.reading_grade_x10,
        "data": data,
    }))
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
        Query::Listing { limit } => json(state.listing_due(cap(limit))),
        Query::PartUsage { part } => json(state.part_usage(&part)),
        Query::LettersRequested { limit } => json(state.letters_requested(cap(limit))),
        Query::LetterRequest { request } => json(state.letter_requests.get(&request)),
        Query::Triggers { limit } => json(state.triggers.values().take(cap(limit)).collect::<Vec<_>>()),
        Query::TemplateForKind { kind, product, on } => json(state.template_for_kind(kind, &product, on).map(|t| t.template.reference())),
        Query::InterestedParties { policy, on } => json(state.interested_parties(&policy, on)),
        Query::Defect { id } => json(state.defects.get(&id)),
        Query::NoticesByTemplate { template, from, to } => json(state.notices_by_template(&template, from, to)),
        Query::Report { from, to } => json(state.report(from, to)),
        Query::Templates { limit } => json(state.template_list(cap(limit))),
        Query::LegalHolds => json(state.legal_holds()),
        Query::OpenDefects { limit } => json(state.defects.values().filter(|d| d.closed.is_none()).take(cap(limit)).collect::<Vec<_>>()),
        Query::Settings => json(&state.settings),
        Query::StateHash => json(state.state_hash()),
        Query::Undeliverable { limit } => json(state.undeliverable(cap(limit))),
        Query::CorrectionsDue { limit } => json(state.corrections_due(cap(limit))),
        Query::RenderSample { template, language, data } => render_sample(state, &template, language, data),
        Query::Authoring => Ok(authoring()),
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
