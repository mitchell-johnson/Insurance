//! A party's event stream.
//!
//! Every change to a party is an event appended to that party's own stream,
//! with a sequence number and a hash chain. The payload is personal and is
//! stored sealed under the party's key. What other services see is a
//! [`PublicEvent`]: IDs, the kind of change and the names of fields that
//! changed, never their values.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{Assertion, Consent, DeclinedCorrection, Field, PartyId, PartyKind, Rating, Relationship, RelationshipKind, Target, Verification};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    Created {
        kind: PartyKind,
        group: String,
        source: String,
    },
    Asserted {
        assertions: Vec<Assertion>,
    },
    CorrectionDeclined(DeclinedCorrection),
    RelationshipAdded(Relationship),
    RelationshipEnded {
        id: String,
        valid_to: NaiveDate,
    },
    ConsentSet(Consent),
    Verified(Verification),
    RiskRated(Rating),
    Activated,
    Deceased {
        source: String,
    },
    DeathRetracted {
        reason: String,
    },
    Exited,
    GroupAssigned {
        group: String,
    },
    /// This party was a duplicate of `survivor`, and resolves to it.
    MergedInto {
        survivor: PartyId,
    },
    /// `other` was merged into this party.
    Absorbed {
        other: PartyId,
    },
    /// A merge into `survivor` was undone.
    Unmerged {
        survivor: PartyId,
    },
    /// `other` is no longer merged into this party.
    Released {
        other: PartyId,
    },
    HoldPlaced {
        reference: String,
    },
    HoldReleased {
        reference: String,
    },
    /// Someone other than the person told the insurer they are worried
    /// about them. Recorded as who raised it and what about, nothing more.
    ConcernRaised {
        from: String,
        about: String,
    },
    /// The party's key was destroyed. Written in the clear, with no payload.
    Shredded,
}

impl EventBody {
    pub fn kind(&self) -> &'static str {
        match self {
            EventBody::Created { .. } => "created",
            EventBody::Asserted { .. } => "asserted",
            EventBody::CorrectionDeclined(_) => "correction_declined",
            EventBody::RelationshipAdded(_) => "relationship_added",
            EventBody::RelationshipEnded { .. } => "relationship_ended",
            EventBody::ConsentSet(_) => "consent_set",
            EventBody::Verified(_) => "verified",
            EventBody::RiskRated(_) => "risk_rated",
            EventBody::Activated => "activated",
            EventBody::Deceased { .. } => "deceased",
            EventBody::DeathRetracted { .. } => "death_retracted",
            EventBody::Exited => "exited",
            EventBody::GroupAssigned { .. } => "group_assigned",
            EventBody::MergedInto { .. } => "merged_into",
            EventBody::Absorbed { .. } => "absorbed",
            EventBody::Unmerged { .. } => "unmerged",
            EventBody::Released { .. } => "released",
            EventBody::HoldPlaced { .. } => "hold_placed",
            EventBody::HoldReleased { .. } => "hold_released",
            EventBody::ConcernRaised { .. } => "concern_raised",
            EventBody::Shredded => "shredded",
        }
    }

    /// The names of the fields an event changes, which are public.
    pub fn fields(&self) -> Vec<Field> {
        let mut fields: Vec<Field> = match self {
            EventBody::Asserted { assertions } => assertions.iter().map(|a| a.field).collect(),
            EventBody::CorrectionDeclined(d) => vec![d.field],
            _ => vec![],
        };
        fields.sort_unstable();
        fields.dedup();
        fields
    }

    /// Changes that take access away go out on the priority channel, so
    /// every gateway applies them before anything else.
    pub fn is_priority(&self) -> bool {
        matches!(
            self,
            EventBody::RelationshipEnded { .. } | EventBody::Deceased { .. } | EventBody::MergedInto { .. } | EventBody::Unmerged { .. } | EventBody::Shredded
        ) || matches!(self, EventBody::ConsentSet(c) if !c.granted)
    }
}

/// An event as it sits in the stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    pub seq: u64,
    pub recorded_at: DateTime<Utc>,
    /// The service that sent the command, and on whose behalf.
    pub caller: String,
    pub actor: String,
    pub body: EventBody,
}

/// What a relationship event says in public.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRelationship {
    pub id: String,
    pub kind: RelationshipKind,
    pub target: Target,
    pub valid_from: NaiveDate,
    pub valid_to: Option<NaiveDate>,
}

/// The public form of an event: what the outbox publishes to Event Hubs.
/// It carries IDs and the names of changed fields, never values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicEvent {
    /// Unique and stable: `{party}:{seq}`. Consumers de-duplicate on it.
    pub id: String,
    pub party: PartyId,
    pub seq: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub recorded_at: DateTime<Utc>,
    pub caller: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<PublicRelationship>,
    /// The other party of a merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<PartyId>,
    /// The new partitioning group, on creation and on a group change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub priority: bool,
}

impl PublicEvent {
    /// `relationship` is the relationship an end refers to, which the
    /// caller looks up so the public event says what ended.
    pub fn of(party: &PartyId, rec: &Recorded, ended: Option<&Relationship>) -> Self {
        let public = |r: &Relationship| PublicRelationship {
            id: r.id.clone(),
            kind: r.kind,
            target: r.target.clone(),
            valid_from: r.valid_from,
            valid_to: r.valid_to,
        };
        let relationship = match &rec.body {
            EventBody::RelationshipAdded(r) => Some(public(r)),
            EventBody::RelationshipEnded { valid_to, .. } => ended.map(|r| PublicRelationship {
                valid_to: Some(*valid_to),
                ..public(r)
            }),
            _ => None,
        };
        let other = match &rec.body {
            EventBody::MergedInto { survivor } | EventBody::Unmerged { survivor } => Some(survivor.clone()),
            EventBody::Absorbed { other } | EventBody::Released { other } => Some(other.clone()),
            _ => None,
        };
        let group = match &rec.body {
            EventBody::Created { group, .. } | EventBody::GroupAssigned { group } => Some(group.clone()),
            _ => None,
        };
        Self {
            id: format!("{}:{}", party, rec.seq),
            party: party.clone(),
            seq: rec.seq,
            kind: rec.body.kind().to_string(),
            recorded_at: rec.recorded_at,
            caller: rec.caller.clone(),
            fields: rec.body.fields(),
            relationship,
            other,
            group,
            priority: rec.body.is_priority(),
        }
    }
}

/// The link in a party's hash chain for one event. It covers everything
/// public about the event and the commitment to its payload, so neither can
/// be changed without breaking every later link.
pub fn chain_hash(prev: &[u8], party: &PartyId, seq: u64, kind: &str, recorded_at: DateTime<Utc>, commitment: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b"ic.customers.chain.v1\0");
    h.update(prev);
    h.update(party.0.as_bytes());
    h.update([0]);
    h.update(seq.to_be_bytes());
    h.update(kind.as_bytes());
    h.update([0]);
    h.update(recorded_at.timestamp_micros().to_be_bytes());
    h.update(commitment);
    h.finalize().to_vec()
}

/// The link before the first event of every stream.
pub const GENESIS: [u8; 32] = [0; 32];
