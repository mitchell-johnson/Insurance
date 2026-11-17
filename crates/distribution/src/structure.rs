//! The dated structure: advisers, authorised bodies, financial advice
//! providers and the groups above them, the engagements between advisers and
//! providers, and each party's licence status over time.
//!
//! Nothing here assumes a number of levels. A licence change is one dated
//! record on one party; what it does to everyone below is worked out from the
//! relationships in force on the day in question, so mixed states over weeks
//! need no bookkeeping of their own.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::PartyId;
use serde::{Deserialize, Serialize};

use crate::EngagementId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyKind {
    Adviser,
    /// An adviser's own company, or any other business paid on someone's behalf.
    Company,
    Team,
    AuthorisedBody,
    Fap,
    /// An aggregator or dealer group above several providers.
    Group,
    ReferralPartner,
    Broker,
    /// A counterparty outside distribution: a reinsurer, an employer, a
    /// claimant with an overpayment, an estate, a liquidator's office.
    Counterparty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenceState {
    Active,
    Suspended,
    Revoked,
    Lapsed,
}

/// One entry in a party's licence history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceRecord {
    pub state: LicenceState,
    pub effective: NaiveDate,
    /// The regulator's notice, or the insurer's own record.
    pub source_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CeasedReason {
    Deceased,
    Liquidation,
    WoundUp,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyNode {
    pub kind: PartyKind,
    /// Ordered by effective date; the last record on or before a day applies.
    pub licence: Vec<LicenceRecord>,
    pub ceased: Option<(NaiveDate, CeasedReason)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// Sits under the parent in the structure: a team in a branch, a provider in a group.
    Under,
    /// Operates under the parent's licence.
    LicensedBy,
    /// The child is paid what the parent (a person) earns.
    PayeeFor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relation {
    pub child: PartyId,
    pub parent: PartyId,
    pub kind: RelationKind,
    pub from: NaiveDate,
    /// First day the relation no longer holds.
    pub to: Option<NaiveDate>,
}

impl Relation {
    pub fn on(&self, day: NaiveDate) -> bool {
        self.from <= day && self.to.is_none_or(|t| day < t)
    }
}

/// An effective-dated relationship between an adviser and the business that
/// engages them. Every piece of business is written under one engagement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Engagement {
    pub adviser: PartyId,
    /// The provider or authorised body.
    pub with: PartyId,
    pub from: NaiveDate,
    /// First day the engagement no longer holds.
    pub to: Option<NaiveDate>,
    /// Product codes the engagement is accredited to advise on.
    pub accreditations: BTreeSet<String>,
}

impl Engagement {
    pub fn on(&self, day: NaiveDate) -> bool {
        self.from <= day && self.to.is_none_or(|t| day < t)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StructureError {
    #[error("party {0} is not in the structure")]
    UnknownParty(PartyId),
    #[error("party {0} is already in the structure")]
    DuplicateParty(PartyId),
    #[error("engagement {0} is not known")]
    UnknownEngagement(EngagementId),
    #[error("engagement {0} already exists")]
    DuplicateEngagement(EngagementId),
    #[error("{0}")]
    Invalid(String),
}

/// Why a party may not be paid, or an engagement may not advise, on a day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Impairment {
    /// The party whose licence record causes it.
    pub source: PartyId,
    pub state: LicenceState,
    pub since: NaiveDate,
    pub source_ref: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Structure {
    pub parties: BTreeMap<PartyId, PartyNode>,
    pub relations: Vec<Relation>,
    pub engagements: BTreeMap<EngagementId, Engagement>,
}

/// A structure deeper than this is a loop someone has made by mistake.
const MAX_DEPTH: usize = 32;

impl Structure {
    pub fn add_party(&mut self, id: PartyId, kind: PartyKind) -> Result<(), StructureError> {
        if self.parties.contains_key(&id) {
            return Err(StructureError::DuplicateParty(id));
        }
        self.parties.insert(
            id,
            PartyNode {
                kind,
                licence: vec![],
                ceased: None,
            },
        );
        Ok(())
    }

    fn known(&self, id: &PartyId) -> Result<&PartyNode, StructureError> {
        self.parties.get(id).ok_or_else(|| StructureError::UnknownParty(id.clone()))
    }

    /// Would adding `child -> parent` on `from` make a loop?
    fn reaches(&self, start: &PartyId, target: &PartyId, day: NaiveDate, depth: usize) -> bool {
        if start == target {
            return true;
        }
        if depth >= MAX_DEPTH {
            return true;
        }
        self.relations
            .iter()
            .filter(|r| &r.child == start && r.on(day))
            .any(|r| self.reaches(&r.parent, target, day, depth + 1))
    }

    pub fn relate(&mut self, relation: Relation) -> Result<(), StructureError> {
        self.known(&relation.child)?;
        self.known(&relation.parent)?;
        if relation.to.is_some_and(|t| t <= relation.from) {
            return Err(StructureError::Invalid("a relation must last at least a day".into()));
        }
        if self.reaches(&relation.parent, &relation.child, relation.from, 0) {
            return Err(StructureError::Invalid(format!(
                "{} is already above {}; the structure cannot loop",
                relation.child, relation.parent
            )));
        }
        if self
            .relations
            .iter()
            .any(|r| r.child == relation.child && r.parent == relation.parent && r.kind == relation.kind && r.to.is_none_or(|t| t > relation.from))
        {
            return Err(StructureError::Invalid("that relation is already in force".into()));
        }
        self.relations.push(relation);
        Ok(())
    }

    /// End every open relation of one kind between two parties.
    pub fn end_relation(&mut self, child: &PartyId, parent: &PartyId, kind: RelationKind, on: NaiveDate) -> Result<(), StructureError> {
        let mut found = false;
        for r in self
            .relations
            .iter_mut()
            .filter(|r| &r.child == child && &r.parent == parent && r.kind == kind && r.to.is_none())
        {
            if on <= r.from {
                return Err(StructureError::Invalid("a relation cannot end before it began".into()));
            }
            r.to = Some(on);
            found = true;
        }
        if found {
            Ok(())
        } else {
            Err(StructureError::Invalid("no such relation is in force".into()))
        }
    }

    pub fn start_engagement(&mut self, id: EngagementId, engagement: Engagement) -> Result<(), StructureError> {
        if self.engagements.contains_key(&id) {
            return Err(StructureError::DuplicateEngagement(id));
        }
        if self.known(&engagement.adviser)?.kind != PartyKind::Adviser {
            return Err(StructureError::Invalid(format!("{} is not an adviser", engagement.adviser)));
        }
        // A broking firm is a licensed provider too: its people are engaged by it.
        if !matches!(
            self.known(&engagement.with)?.kind,
            PartyKind::Fap | PartyKind::AuthorisedBody | PartyKind::Broker
        ) {
            return Err(StructureError::Invalid(format!(
                "{} is not a provider, a broker or an authorised body",
                engagement.with
            )));
        }
        if engagement.to.is_some_and(|t| t <= engagement.from) {
            return Err(StructureError::Invalid("an engagement must last at least a day".into()));
        }
        // One adviser, one provider, one engagement at a time: a second one
        // over the same days is a mistake, and the provider's page would list
        // the adviser twice (F2-20-3). Engagements with different providers
        // may overlap, as an adviser may be engaged by more than one.
        let overlaps = |e: &Engagement| e.from < engagement.to.unwrap_or(NaiveDate::MAX) && engagement.from < e.to.unwrap_or(NaiveDate::MAX);
        if let Some((held, e)) = self
            .engagements
            .iter()
            .find(|(_, e)| e.adviser == engagement.adviser && e.with == engagement.with && overlaps(e))
        {
            return Err(StructureError::Invalid(format!(
                "{} is already engaged with {} under {held} from {}: end or withdraw that engagement first",
                engagement.adviser,
                engagement.with,
                e.from.format("%d/%m/%Y")
            )));
        }
        self.engagements.insert(id, engagement);
        Ok(())
    }

    /// End an engagement from a day. Ending it on the day it began withdraws
    /// it: it was entered in error and was never in force (F2-20-4).
    pub fn end_engagement(&mut self, id: &EngagementId, on: NaiveDate) -> Result<(), StructureError> {
        let e = self.engagements.get_mut(id).ok_or_else(|| StructureError::UnknownEngagement(id.clone()))?;
        if on < e.from {
            return Err(StructureError::Invalid(format!(
                "an engagement cannot end before it began on {}; ending it on that day withdraws it",
                e.from.format("%d/%m/%Y")
            )));
        }
        if e.to.is_some_and(|t| t <= on) {
            return Err(StructureError::Invalid("the engagement has already ended".into()));
        }
        e.to = Some(on);
        Ok(())
    }

    /// Record a licence status from a date. History is added to, never
    /// rewritten: a second status for a date already on file (a suspension
    /// recorded in error and put right the same day, a regulator's
    /// reinstatement notice) is kept after the first, and the latest
    /// recorded for a day is the one in force. Sending the same record
    /// again changes nothing.
    pub fn record_licence(&mut self, party: &PartyId, record: LicenceRecord) -> Result<(), StructureError> {
        let node = self.parties.get_mut(party).ok_or_else(|| StructureError::UnknownParty(party.clone()))?;
        if record.source_ref.trim().is_empty() {
            return Err(StructureError::Invalid("a licence status needs its source reference".into()));
        }
        if node.licence.iter().rev().find(|r| r.effective == record.effective) == Some(&record) {
            return Ok(());
        }
        node.licence.push(record);
        // A stable sort: records for the same day keep the order they were recorded in.
        node.licence.sort_by_key(|r| r.effective);
        Ok(())
    }

    pub fn mark_ceased(&mut self, party: &PartyId, on: NaiveDate, reason: CeasedReason) -> Result<(), StructureError> {
        let node = self.parties.get_mut(party).ok_or_else(|| StructureError::UnknownParty(party.clone()))?;
        node.ceased = Some((on, reason));
        Ok(())
    }

    /// The party's own licence record on a day.
    pub fn own_licence(&self, party: &PartyId, day: NaiveDate) -> Option<&LicenceRecord> {
        self.parties.get(party)?.licence.iter().rev().find(|r| r.effective <= day)
    }

    fn own_impairment(&self, party: &PartyId, day: NaiveDate) -> Option<Impairment> {
        self.own_licence(party, day).filter(|r| r.state != LicenceState::Active).map(|r| Impairment {
            source: party.clone(),
            state: r.state,
            since: r.effective,
            source_ref: r.source_ref.clone(),
        })
    }

    /// The first licence problem found on the party itself or on any licence
    /// it depends on, on that day.
    fn chain_impairment(&self, party: &PartyId, day: NaiveDate, depth: usize) -> Option<Impairment> {
        if depth >= MAX_DEPTH {
            return None;
        }
        if let Some(i) = self.own_impairment(party, day) {
            return Some(i);
        }
        self.relations
            .iter()
            .filter(|r| &r.child == party && r.kind == RelationKind::LicensedBy && r.on(day))
            .find_map(|r| self.chain_impairment(&r.parent, day, depth + 1))
    }

    /// Why this engagement may not be used to give advice on a day, if it may not.
    pub fn engagement_impairment(&self, id: &EngagementId, day: NaiveDate) -> Option<Impairment> {
        let e = self.engagements.get(id)?;
        self.own_impairment(&e.adviser, day).or_else(|| self.chain_impairment(&e.with, day, 0))
    }

    /// May business be written under this engagement for this product today?
    pub fn may_advise(&self, id: &EngagementId, product: &str, day: NaiveDate) -> Result<(), String> {
        let e = self.engagements.get(id).ok_or_else(|| format!("engagement {id} is not known"))?;
        if !e.on(day) {
            return Err(format!("engagement {id} is not in force on {day}"));
        }
        if let Some(i) = self.engagement_impairment(id, day) {
            return Err(format!("the licence of {} is {:?} since {}", i.source, i.state, i.since));
        }
        if !e.accreditations.contains(product) {
            return Err(format!("engagement {id} is not accredited for {product}"));
        }
        Ok(())
    }

    /// Why this payee may not be paid on a day, if it may not. A provider or
    /// authorised body is suspended with its licence chain. An adviser is
    /// suspended on their own record, or when every engagement they hold that
    /// day sits under a licence that is not active. A company paid on a
    /// person's behalf follows that person.
    pub fn payee_impairment(&self, payee: &PartyId, day: NaiveDate) -> Option<Impairment> {
        self.payee_impairment_at(payee, day, 0)
    }

    fn payee_impairment_at(&self, payee: &PartyId, day: NaiveDate, depth: usize) -> Option<Impairment> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let node = self.parties.get(payee)?;
        if let Some(i) = self.chain_impairment(payee, day, 0) {
            return Some(i);
        }
        if node.kind == PartyKind::Adviser {
            let mut first = None;
            let mut any = false;
            for (id, _) in self.engagements.iter().filter(|(_, e)| &e.adviser == payee && e.on(day)) {
                any = true;
                let i = self.engagement_impairment(id, day)?;
                first = first.or(Some(i));
            }
            if any {
                return first;
            }
        }
        self.relations
            .iter()
            .filter(|r| &r.child == payee && r.kind == RelationKind::PayeeFor && r.on(day))
            .find_map(|r| self.payee_impairment_at(&r.parent, day, depth + 1))
    }

    /// Every engagement that cannot advise on the day because of a licence
    /// record on `source`, and every party in the structure that cannot be
    /// paid for the same reason.
    pub fn affected_by(&self, source: &PartyId, day: NaiveDate) -> (BTreeSet<EngagementId>, BTreeSet<PartyId>) {
        let engagements = self
            .engagements
            .iter()
            .filter(|(id, e)| e.on(day) && self.engagement_impairment(id, day).is_some_and(|i| &i.source == source))
            .map(|(id, _)| id.clone())
            .collect();
        let payees = self
            .parties
            .keys()
            .filter(|p| self.payee_impairment(p, day).is_some_and(|i| &i.source == source))
            .cloned()
            .collect();
        (engagements, payees)
    }

    /// A party and everything it sits under or is licensed by on a day.
    fn above(&self, party: &PartyId, day: NaiveDate) -> BTreeSet<PartyId> {
        let mut seen = BTreeSet::new();
        let mut queue = vec![party.clone()];
        while let Some(p) = queue.pop() {
            if !seen.insert(p.clone()) {
                continue;
            }
            for r in self.relations.iter().filter(|r| r.child == p && r.kind != RelationKind::PayeeFor && r.on(day)) {
                queue.push(r.parent.clone());
            }
        }
        seen
    }

    /// Two providers are related on a day when they are the same, one sits
    /// under the other, or both sit under the same provider or group.
    pub fn related(&self, a: &PartyId, b: &PartyId, day: NaiveDate) -> bool {
        !self.above(a, day).is_disjoint(&self.above(b, day))
    }

    /// Everything at or below a party on a day: who a provider or group sees.
    pub fn below(&self, top: &PartyId, day: NaiveDate) -> BTreeSet<PartyId> {
        let mut seen = BTreeSet::new();
        let mut queue = vec![top.clone()];
        while let Some(p) = queue.pop() {
            if !seen.insert(p.clone()) {
                continue;
            }
            for r in self.relations.iter().filter(|r| r.parent == p && r.on(day)) {
                queue.push(r.child.clone());
            }
            for e in self.engagements.values().filter(|e| e.with == p && e.on(day)) {
                queue.push(e.adviser.clone());
            }
        }
        seen
    }
}
