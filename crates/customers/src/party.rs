//! One party: its state folded from its events, what it looked like on
//! either timeline, and what a command does to it.
//!
//! Deciding is pure. It takes the party as it stands, the command and the
//! time, and returns the events to append or the reason the command is
//! refused. A refused command writes nothing.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{EventBody, Recorded};
use crate::model::{
    Assertion, Consent, DeclinedCorrection, Field, Lifecycle, PartyId, PartyKind, Rating, Relationship, RelationshipKind, RiskRating, Target, Verification,
    VerificationOutcome,
};
use crate::validate;

/// Why a command was refused. `code` is stable for callers to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Reject {
    pub code: String,
    pub message: String,
}

pub fn reject(code: &str, message: impl Into<String>) -> Reject {
    Reject {
        code: code.to_string(),
        message: message.into(),
    }
}

/// A fact as a caller states it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactInput {
    pub field: Field,
    /// `null` says the fact no longer holds from `valid_from`.
    pub value: Value,
    /// When it became true. Defaults to the day it is recorded.
    #[serde(default)]
    pub valid_from: Option<NaiveDate>,
}

/// A command to one party.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// Record facts, or correct them. A correction is a fact for a date
    /// already stated, with a reason.
    Record {
        facts: Vec<FactInput>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// A request to correct a fact was declined; the person's statement of
    /// correction is attached to the record.
    DeclineCorrection {
        field: Field,
        statement: String,
    },
    AddRelationship {
        /// The caller's ID for the relationship, unique for the party, so a
        /// retry adds it once.
        id: String,
        kind: RelationshipKind,
        target: Target,
        valid_from: NaiveDate,
        #[serde(default)]
        valid_to: Option<NaiveDate>,
        #[serde(default)]
        attributes: serde_json::Map<String, Value>,
    },
    /// End a relationship: `valid_to` is the first day it no longer holds.
    EndRelationship {
        id: String,
        valid_to: NaiveDate,
    },
    SetConsent {
        purpose: String,
        granted: bool,
        #[serde(default)]
        valid_from: Option<NaiveDate>,
    },
    RecordVerification {
        method: String,
        provider: String,
        outcome: VerificationOutcome,
        evidence_ref: String,
        verified_at: DateTime<Utc>,
    },
    SetRiskRating {
        rating: RiskRating,
        review_by: NaiveDate,
    },
    Activate,
    MarkDeceased {
        date_of_death: NaiveDate,
    },
    /// A death recorded in error.
    RetractDeath {
        reason: String,
    },
    Exit,
    AssignGroup {
        group: String,
    },
    PlaceHold {
        reference: String,
    },
    ReleaseHold {
        reference: String,
    },
}

impl Change {
    /// The name authorisation and audit use.
    pub fn name(&self) -> &'static str {
        match self {
            Change::Record { .. } => "record",
            Change::DeclineCorrection { .. } => "decline_correction",
            Change::AddRelationship { .. } => "add_relationship",
            Change::EndRelationship { .. } => "end_relationship",
            Change::SetConsent { .. } => "set_consent",
            Change::RecordVerification { .. } => "record_verification",
            Change::SetRiskRating { .. } => "set_risk_rating",
            Change::Activate => "activate",
            Change::MarkDeceased { .. } => "mark_deceased",
            Change::RetractDeath { .. } => "retract_death",
            Change::Exit => "exit",
            Change::AssignGroup { .. } => "assign_group",
            Change::PlaceHold { .. } => "place_hold",
            Change::ReleaseHold { .. } => "release_hold",
        }
    }
}

/// A fact as it was on some date, as known at some time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactView {
    pub value: Value,
    pub valid_from: NaiveDate,
    pub recorded_at: DateTime<Utc>,
    pub source: String,
}

/// A party folded from its events.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Party {
    pub id: PartyId,
    pub kind: Option<PartyKind>,
    pub lifecycle: Option<Lifecycle>,
    pub group: String,
    pub created_at: Option<DateTime<Utc>>,
    pub source: String,
    pub assertions: Vec<Assertion>,
    pub declined: Vec<DeclinedCorrection>,
    pub relationships: BTreeMap<String, Relationship>,
    pub consents: Vec<Consent>,
    pub verifications: Vec<Verification>,
    pub ratings: Vec<Rating>,
    pub merged_into: Option<PartyId>,
    pub absorbed: BTreeSet<PartyId>,
    pub holds: BTreeSet<String>,
    pub shredded: bool,
    /// The last sequence number applied.
    pub version: u64,
}

/// The limit on text a caller supplies outside the field values.
const MAX_TEXT: usize = 2_000;

fn short_text(what: &str, text: &str) -> Result<String, Reject> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > MAX_TEXT || t.chars().any(|c| c.is_control() && c != '\n') {
        return Err(reject("invalid", format!("{what} must be between 1 and {MAX_TEXT} characters of text")));
    }
    Ok(t.to_string())
}

fn token(what: &str, text: &str) -> Result<String, Reject> {
    if (1..=128).contains(&text.len()) && text.chars().all(|c| c.is_ascii_alphanumeric() || "_-:.".contains(c)) {
        Ok(text.to_string())
    } else {
        Err(reject("invalid", format!("{what} must be 1 to 128 letters, digits or _-:.")))
    }
}

/// A group ID: the core's partition key.
pub fn group_for(party: &PartyId) -> String {
    format!("grp_{}", party.0.trim_start_matches("pty_"))
}

/// The events that create a party.
pub fn create(id: &PartyId, kind: PartyKind, facts: &[FactInput], group: Option<String>, source: &str, now: DateTime<Utc>) -> Result<Vec<EventBody>, Reject> {
    let source = token("source", source)?;
    let group = match group {
        Some(g) => token("group", &g)?,
        None => group_for(id),
    };
    let blank = Party {
        id: id.clone(),
        kind: Some(kind),
        ..Party::default()
    };
    let assertions = blank.assertions_for(facts, None, &source, now)?;
    let needed: &[Field] = match kind {
        PartyKind::Person => &[Field::FamilyName],
        PartyKind::Organisation => &[Field::OrganisationName],
    };
    for f in needed {
        if !assertions.iter().any(|a| a.field == *f && a.value.is_some()) {
            return Err(reject("invalid", format!("a new {} needs {f}", kind_name(kind))));
        }
    }
    Ok(vec![EventBody::Created { kind, group, source }, EventBody::Asserted { assertions }])
}

fn kind_name(kind: PartyKind) -> &'static str {
    match kind {
        PartyKind::Person => "person",
        PartyKind::Organisation => "organisation",
    }
}

impl Party {
    pub fn new(id: PartyId) -> Self {
        Self { id, ..Self::default() }
    }

    /// Fold one event in.
    pub fn apply(&mut self, rec: &Recorded) {
        self.version = rec.seq;
        match &rec.body {
            EventBody::Created { kind, group, source } => {
                self.kind = Some(*kind);
                self.group = group.clone();
                self.source = source.clone();
                self.lifecycle = Some(Lifecycle::Prospect);
                self.created_at = Some(rec.recorded_at);
            }
            EventBody::Asserted { assertions } => self.assertions.extend(assertions.iter().cloned()),
            EventBody::CorrectionDeclined(d) => self.declined.push(d.clone()),
            EventBody::RelationshipAdded(r) => {
                self.relationships.insert(r.id.clone(), r.clone());
            }
            EventBody::RelationshipEnded { id, valid_to } => {
                if let Some(r) = self.relationships.get_mut(id) {
                    r.valid_to = Some(*valid_to);
                }
            }
            EventBody::ConsentSet(c) => self.consents.push(c.clone()),
            EventBody::Verified(v) => self.verifications.push(v.clone()),
            EventBody::RiskRated(r) => self.ratings.push(r.clone()),
            EventBody::Activated => self.lifecycle = Some(Lifecycle::Active),
            EventBody::Deceased { .. } => self.lifecycle = Some(Lifecycle::Deceased),
            EventBody::DeathRetracted { .. } => {
                let any = self.relationships.values().any(|r| r.valid_to.is_none());
                self.lifecycle = Some(if any { Lifecycle::Active } else { Lifecycle::Prospect });
            }
            EventBody::Exited => self.lifecycle = Some(Lifecycle::Exited),
            EventBody::GroupAssigned { group } => self.group = group.clone(),
            EventBody::MergedInto { survivor } => self.merged_into = Some(survivor.clone()),
            EventBody::Unmerged { .. } => self.merged_into = None,
            EventBody::Absorbed { other } => {
                self.absorbed.insert(other.clone());
            }
            EventBody::Released { other } => {
                self.absorbed.remove(other);
            }
            EventBody::HoldPlaced { reference } => {
                self.holds.insert(reference.clone());
            }
            EventBody::HoldReleased { reference } => {
                self.holds.remove(reference);
            }
            EventBody::Shredded => {
                let id = std::mem::take(&mut self.id);
                *self = Party {
                    id,
                    kind: self.kind,
                    version: rec.seq,
                    shredded: true,
                    ..Party::default()
                };
            }
        }
    }

    pub fn exists(&self) -> bool {
        self.kind.is_some()
    }

    /// The facts as they were on `as_of`, as we knew them at `known_at`.
    /// For each field, the statement in effect is the one with the latest
    /// `valid_from` on or before `as_of`, and among statements for the same
    /// date the one recorded last.
    pub fn facts(&self, as_of: NaiveDate, known_at: DateTime<Utc>) -> BTreeMap<Field, FactView> {
        let mut best: BTreeMap<Field, &Assertion> = BTreeMap::new();
        for a in self.assertions.iter().filter(|a| a.recorded_at <= known_at && a.valid_from <= as_of) {
            let better = best
                .get(&a.field)
                .is_none_or(|b| (a.valid_from, a.recorded_at) >= (b.valid_from, b.recorded_at));
            if better {
                best.insert(a.field, a);
            }
        }
        best.into_iter()
            .filter_map(|(f, a)| {
                a.value.clone().map(|value| {
                    (
                        f,
                        FactView {
                            value,
                            valid_from: a.valid_from,
                            recorded_at: a.recorded_at,
                            source: a.source.clone(),
                        },
                    )
                })
            })
            .collect()
    }

    /// The current value of one field.
    pub fn current(&self, field: Field, now: DateTime<Utc>) -> Option<Value> {
        self.facts(now.date_naive(), now).remove(&field).map(|f| f.value)
    }

    /// Consents in effect on `day`, by purpose.
    pub fn consents_on(&self, day: NaiveDate) -> BTreeMap<String, bool> {
        let mut out: BTreeMap<String, (NaiveDate, DateTime<Utc>, bool)> = BTreeMap::new();
        for c in self.consents.iter().filter(|c| c.valid_from <= day) {
            let newer = out.get(&c.purpose).is_none_or(|(d, at, _)| (c.valid_from, c.recorded_at) >= (*d, *at));
            if newer {
                out.insert(c.purpose.clone(), (c.valid_from, c.recorded_at, c.granted));
            }
        }
        out.into_iter().map(|(p, (_, _, g))| (p, g)).collect()
    }

    fn assertions_for(&self, facts: &[FactInput], reason: Option<&str>, source: &str, now: DateTime<Utc>) -> Result<Vec<Assertion>, Reject> {
        if facts.is_empty() || facts.len() > 50 {
            return Err(reject("invalid", "between 1 and 50 facts at a time"));
        }
        let today = now.date_naive();
        let reason = reason.map(|r| short_text("the reason", r)).transpose()?;
        let mut out = Vec::new();
        for f in facts {
            let kind = self.kind.unwrap_or(PartyKind::Person);
            let organisation = matches!(f.field.class(), crate::model::FieldClass::Organisation);
            let person_only = matches!(
                f.field,
                Field::DateOfBirth | Field::DateOfDeath | Field::GivenName | Field::MiddleNames | Field::FamilyName
            );
            if (kind == PartyKind::Person && organisation) || (kind == PartyKind::Organisation && person_only) {
                return Err(reject("invalid", format!("{} is not a fact about a {}", f.field, kind_name(kind))));
            }
            if f.field == Field::DateOfDeath {
                return Err(reject("invalid", "a death is recorded with mark_deceased"));
            }
            let valid_from = f.valid_from.unwrap_or(today);
            if valid_from > today {
                return Err(reject("invalid", format!("{} cannot be stated for a future date", f.field)));
            }
            let value = if f.value.is_null() {
                None
            } else {
                let v = validate::normalise(f.field, &f.value).map_err(|p| reject("invalid", format!("{} {p}", f.field)))?;
                if f.field == Field::DateOfBirth && validate::date(&v).is_ok_and(|d| d > today) {
                    return Err(reject("invalid", "date_of_birth is in the future"));
                }
                Some(v)
            };
            // A correction of what was already stated for that date needs a reason.
            let restates = self.assertions.iter().any(|a| a.field == f.field && a.valid_from == valid_from);
            if restates && reason.is_none() {
                return Err(reject(
                    "reason_required",
                    format!("{} was already stated from {valid_from}; a correction needs a reason", f.field),
                ));
            }
            out.push(Assertion {
                field: f.field,
                value,
                valid_from,
                recorded_at: now,
                reason: reason.clone(),
                source: source.to_string(),
            });
        }
        Ok(out)
    }

    fn date_of_birth(&self, now: DateTime<Utc>) -> Option<NaiveDate> {
        self.current(Field::DateOfBirth, now).and_then(|v| validate::date(&v).ok())
    }

    fn in_force_on(&self, day: NaiveDate) -> bool {
        self.relationships.values().any(|r| r.in_force(day) || r.valid_from > day)
    }

    /// The events a command adds, or why it is refused. Commands that would
    /// change nothing (a repeat) return no events.
    pub fn decide(&self, change: &Change, source: &str, now: DateTime<Utc>) -> Result<Vec<EventBody>, Reject> {
        if !self.exists() {
            return Err(reject("not_found", "no such party"));
        }
        if self.shredded {
            return Err(reject("shredded", "the party's record has been erased"));
        }
        if let Some(survivor) = &self.merged_into {
            return Err(reject("merged", format!("the party was merged into {survivor}; send it there")));
        }
        let today = now.date_naive();
        let lifecycle = self.lifecycle.unwrap_or(Lifecycle::Prospect);
        let mut events = Vec::new();
        match change {
            Change::Record { facts, reason } => {
                let assertions = self.assertions_for(facts, reason.as_deref(), &token("source", source)?, now)?;
                events.push(EventBody::Asserted { assertions });
            }
            Change::DeclineCorrection { field, statement } => events.push(EventBody::CorrectionDeclined(DeclinedCorrection {
                field: *field,
                statement: short_text("the statement", statement)?,
                recorded_at: now,
            })),
            Change::AddRelationship {
                id,
                kind,
                target,
                valid_from,
                valid_to,
                attributes,
            } => {
                let id = token("the relationship ID", id)?;
                if target.kind() != kind.target_kind() {
                    return Err(reject("invalid", format!("{kind} is a relationship to a {}", kind.target_kind())));
                }
                token("the target ID", target.id())?;
                if *target == Target::Party(self.id.clone()) {
                    return Err(reject("invalid", "a party cannot be related to itself"));
                }
                if valid_to.is_some_and(|end| end <= *valid_from) {
                    return Err(reject("invalid", "a relationship must end after it starts"));
                }
                if serde_json::to_vec(attributes).map_or(0, |b| b.len()) > 4_096 {
                    return Err(reject("invalid", "relationship attributes are limited to 4 KB"));
                }
                if *kind == RelationshipKind::Dependant {
                    let controls = attributes.get("controls_from").and_then(|v| validate::date(v).ok());
                    if controls.is_none() {
                        return Err(reject(
                            "invalid",
                            "a dependant needs controls_from, the date they control their own health information",
                        ));
                    }
                }
                let wanted = Relationship {
                    id: id.clone(),
                    kind: *kind,
                    target: target.clone(),
                    valid_from: *valid_from,
                    valid_to: *valid_to,
                    recorded_at: now,
                    attributes: attributes.clone(),
                };
                if let Some(existing) = self.relationships.get(&id) {
                    let same = Relationship {
                        recorded_at: existing.recorded_at,
                        ..wanted
                    };
                    if same == *existing {
                        return Ok(vec![]);
                    }
                    return Err(reject("conflict", format!("relationship {id} already exists with other details")));
                }
                if self.relationships.len() >= 10_000 {
                    return Err(reject("limit", "the party has too many relationships"));
                }
                events.push(EventBody::RelationshipAdded(wanted));
                if matches!(lifecycle, Lifecycle::Prospect | Lifecycle::Exited) && kind.target_kind() != "party" {
                    events.push(EventBody::Activated);
                }
            }
            Change::EndRelationship { id, valid_to } => {
                let r = self.relationships.get(id).ok_or_else(|| reject("not_found", format!("no relationship {id}")))?;
                if r.valid_to == Some(*valid_to) {
                    return Ok(vec![]);
                }
                if *valid_to <= r.valid_from {
                    return Err(reject("invalid", "a relationship must end after it starts"));
                }
                if let Some(end) = r.valid_to.filter(|end| *end <= today) {
                    return Err(reject("ended", format!("relationship {id} already ended on {end}")));
                }
                events.push(EventBody::RelationshipEnded {
                    id: id.clone(),
                    valid_to: *valid_to,
                });
            }
            Change::SetConsent { purpose, granted, valid_from } => {
                let purpose = token("the purpose", purpose)?;
                let valid_from = valid_from.unwrap_or(today);
                if valid_from > today {
                    return Err(reject("invalid", "a consent cannot be recorded for a future date"));
                }
                events.push(EventBody::ConsentSet(Consent {
                    purpose,
                    granted: *granted,
                    valid_from,
                    recorded_at: now,
                }));
            }
            Change::RecordVerification {
                method,
                provider,
                outcome,
                evidence_ref,
                verified_at,
            } => {
                if *verified_at > now {
                    return Err(reject("invalid", "a verification cannot be in the future"));
                }
                events.push(EventBody::Verified(Verification {
                    method: token("the method", method)?,
                    provider: token("the provider", provider)?,
                    outcome: *outcome,
                    evidence_ref: token("the evidence reference", evidence_ref)?,
                    verified_at: *verified_at,
                }));
            }
            Change::SetRiskRating { rating, review_by } => {
                if *review_by <= today {
                    return Err(reject("invalid", "the review date must be after today"));
                }
                events.push(EventBody::RiskRated(Rating {
                    rating: *rating,
                    review_by: *review_by,
                    recorded_at: now,
                }));
            }
            Change::Activate => match lifecycle {
                Lifecycle::Active => {}
                Lifecycle::Deceased => return Err(reject("deceased", "the party is deceased")),
                Lifecycle::Prospect | Lifecycle::Exited => events.push(EventBody::Activated),
            },
            Change::MarkDeceased { date_of_death } => {
                if self.kind != Some(PartyKind::Person) {
                    return Err(reject("invalid", "only a person can die"));
                }
                if *date_of_death > today {
                    return Err(reject("invalid", "the date of death is in the future"));
                }
                if self.date_of_birth(now).is_some_and(|dob| *date_of_death < dob) {
                    return Err(reject("invalid", "the date of death is before the date of birth"));
                }
                let recorded = self.current(Field::DateOfDeath, now).and_then(|v| validate::date(&v).ok());
                if lifecycle == Lifecycle::Deceased {
                    if recorded == Some(*date_of_death) {
                        return Ok(vec![]);
                    }
                    return Err(reject(
                        "already_deceased",
                        "a different date of death is already recorded; retract it first or correct it with a reason",
                    ));
                }
                events.push(EventBody::Asserted {
                    assertions: vec![Assertion {
                        field: Field::DateOfDeath,
                        value: Some(Value::String(date_of_death.to_string())),
                        valid_from: *date_of_death,
                        recorded_at: now,
                        reason: None,
                        source: token("source", source)?,
                    }],
                });
                events.push(EventBody::Deceased {
                    source: token("source", source)?,
                });
            }
            Change::RetractDeath { reason } => {
                if lifecycle != Lifecycle::Deceased {
                    return Err(reject("not_deceased", "no death is recorded"));
                }
                let reason = short_text("the reason", reason)?;
                let since = self
                    .assertions
                    .iter()
                    .filter(|a| a.field == Field::DateOfDeath)
                    .map(|a| a.valid_from)
                    .min()
                    .unwrap_or(today);
                events.push(EventBody::Asserted {
                    assertions: vec![Assertion {
                        field: Field::DateOfDeath,
                        value: None,
                        valid_from: since,
                        recorded_at: now,
                        reason: Some(reason.clone()),
                        source: token("source", source)?,
                    }],
                });
                events.push(EventBody::DeathRetracted { reason });
            }
            Change::Exit => match lifecycle {
                Lifecycle::Exited => {}
                _ if self.in_force_on(today) => {
                    return Err(reject("relationships_in_force", "end every relationship before the party exits"));
                }
                _ => events.push(EventBody::Exited),
            },
            Change::AssignGroup { group } => {
                let group = token("the group", group)?;
                if group != self.group {
                    events.push(EventBody::GroupAssigned { group });
                }
            }
            Change::PlaceHold { reference } => {
                let reference = token("the hold reference", reference)?;
                if !self.holds.contains(&reference) {
                    events.push(EventBody::HoldPlaced { reference });
                }
            }
            Change::ReleaseHold { reference } => {
                if self.holds.contains(reference) {
                    events.push(EventBody::HoldReleased { reference: reference.clone() });
                } else {
                    return Err(reject("not_found", format!("no hold {reference}")));
                }
            }
        }
        Ok(events)
    }
}

/// The events of merging `absorbed` into `survivor`: one for each stream.
pub fn merge(survivor: &Party, absorbed: &Party) -> Result<(EventBody, EventBody), Reject> {
    for p in [survivor, absorbed] {
        if !p.exists() {
            return Err(reject("not_found", format!("no party {}", p.id)));
        }
        if p.shredded {
            return Err(reject("shredded", format!("{} has been erased", p.id)));
        }
        if p.merged_into.is_some() {
            return Err(reject("merged", format!("{} is already merged", p.id)));
        }
    }
    if survivor.id == absorbed.id {
        return Err(reject("invalid", "a party cannot be merged into itself"));
    }
    if survivor.kind != absorbed.kind {
        return Err(reject("invalid", "a person and an organisation cannot be merged"));
    }
    if !absorbed.absorbed.is_empty() {
        return Err(reject("invalid", "unmerge the parties merged into this one first"));
    }
    Ok((
        EventBody::Absorbed { other: absorbed.id.clone() },
        EventBody::MergedInto { survivor: survivor.id.clone() },
    ))
}

/// The events of undoing a merge.
pub fn unmerge(survivor: &Party, absorbed: &Party) -> Result<(EventBody, EventBody), Reject> {
    if absorbed.merged_into.as_ref() != Some(&survivor.id) || !survivor.absorbed.contains(&absorbed.id) {
        return Err(reject("not_merged", format!("{} is not merged into {}", absorbed.id, survivor.id)));
    }
    if survivor.shredded || absorbed.shredded {
        return Err(reject("shredded", "an erased party cannot be unmerged"));
    }
    Ok((
        EventBody::Released { other: absorbed.id.clone() },
        EventBody::Unmerged { survivor: survivor.id.clone() },
    ))
}
