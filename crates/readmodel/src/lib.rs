//! Read models: projections of the event stream. They can be thrown away and
//! rebuilt, may lag, and say so: every read that drives a decision records
//! the sequence it reflects.

pub mod scope;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::event::{Event, EventRecord, LayerInfo, LifeInfo, MemberCover};
use ic_core::{GroupId, LifeId, PartyId, PolicyId, PolicyStatus, Standing};
use ic_ledger::PartitionId;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectionError {
    /// A consumer that sees a gap asks for the missing events.
    #[error("gap on partition {partition}: expected seq {expected}, got {got}")]
    Gap { partition: PartitionId, expected: u64, got: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub policy: PolicyId,
    pub number: String,
    pub group: GroupId,
    pub partition: PartitionId,
    pub status: PolicyStatus,
    pub standing: Option<Standing>,
    pub product_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingRow {
    pub outstanding_minor: i64,
    pub oldest_unpaid_due: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeIndexRow {
    pub policy: PolicyId,
    pub partition: PartitionId,
    pub cover: String,
    pub cover_type: String,
    pub layer: u32,
    pub life: LifeId,
    pub sum_insured_minor: i64,
    pub in_force: bool,
}

/// The policy directory, the life index and the billing view, fed from every
/// partition's stream. Delivery is at least once: duplicates are dropped on
/// the per-partition sequence, and a gap is an error.
///
/// A projection can be saved and loaded whole, so a host that keeps one
/// beside its state need not replay the stream from the beginning.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projections {
    checkpoints: BTreeMap<PartitionId, u64>,
    directory: BTreeMap<PolicyId, DirectoryEntry>,
    by_number: BTreeMap<String, PolicyId>,
    billing: BTreeMap<PolicyId, BillingRow>,
    /// (policy, life) -> party, where the life is named or identified.
    #[serde(with = "as_pairs")]
    life_party: BTreeMap<(PolicyId, LifeId), PartyId>,
    /// policy -> (cover, layer) -> row.
    #[serde(with = "layers_as_rows")]
    layers: BTreeMap<PolicyId, BTreeMap<(String, u32), LifeIndexRow>>,
    /// policy -> (life, cover) -> row, for scheme members. Their cover comes
    /// from a formula over the member's details and is not held as layers;
    /// the row follows the details as notified. Layer is zero.
    #[serde(default, with = "members_as_rows")]
    members: BTreeMap<PolicyId, BTreeMap<(LifeId, String), LifeIndexRow>>,
}

impl Projections {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sequence this projection reflects for a partition.
    pub fn checkpoint(&self, partition: PartitionId) -> u64 {
        self.checkpoints.get(&partition).copied().unwrap_or(0)
    }

    /// Apply one event. Returns false if it was a duplicate.
    pub fn apply(&mut self, rec: &EventRecord) -> Result<bool, ProjectionError> {
        let at = self.checkpoint(rec.partition);
        if rec.seq <= at {
            return Ok(false);
        }
        if rec.seq != at + 1 {
            return Err(ProjectionError::Gap {
                partition: rec.partition,
                expected: at + 1,
                got: rec.seq,
            });
        }
        self.checkpoints.insert(rec.partition, rec.seq);
        self.project(rec.partition, &rec.event);
        Ok(true)
    }

    /// Drop everything known about a policy.
    fn forget(&mut self, policy: &PolicyId) {
        if let Some(entry) = self.directory.remove(policy) {
            self.by_number.remove(&entry.number);
        }
        self.billing.remove(policy);
        self.layers.remove(policy);
        self.members.remove(policy);
        self.life_party.retain(|(p, _), _| p != policy);
    }

    fn set_lives(&mut self, policy: &PolicyId, lives: &[LifeInfo]) {
        for l in lives {
            if let Some(p) = &l.party {
                self.life_party.insert((policy.clone(), l.life.clone()), p.clone());
            }
        }
    }

    fn set_layers(&mut self, policy: &PolicyId, partition: PartitionId, layers: &[LayerInfo]) {
        // A policy with no layers has no entry: an empty one would not
        // survive being saved and loaded.
        self.layers.remove(policy);
        if !layers.is_empty() {
            let rows = layers.iter().map(|l| ((l.cover.clone(), l.layer), row(policy, partition, l))).collect();
            self.layers.insert(policy.clone(), rows);
        }
    }

    /// Replace what the index holds for the members these covers are on.
    fn set_member_covers(&mut self, policy: &PolicyId, partition: PartitionId, life: Option<&LifeId>, covers: &[MemberCover]) {
        let in_force = self.directory.get(policy).is_none_or(|e| e.status == PolicyStatus::InForce);
        let rows = self.members.entry(policy.clone()).or_default();
        match life {
            Some(life) => rows.retain(|(l, _), _| l != life),
            None => rows.clear(),
        }
        if rows.is_empty() && covers.is_empty() {
            self.members.remove(policy);
            return;
        }
        for c in covers {
            rows.insert(
                (c.life.clone(), c.cover.clone()),
                LifeIndexRow {
                    policy: policy.clone(),
                    partition,
                    cover: c.cover.clone(),
                    cover_type: c.cover_type.clone(),
                    layer: 0,
                    life: c.life.clone(),
                    sum_insured_minor: c.sum_insured_minor,
                    in_force,
                },
            );
        }
    }

    fn project(&mut self, partition: PartitionId, event: &Event) {
        match event {
            Event::PolicyIssued {
                policy,
                number,
                group,
                product_code,
                lives,
                layers,
                members,
                ..
            } => {
                self.directory.insert(
                    policy.clone(),
                    DirectoryEntry {
                        policy: policy.clone(),
                        number: number.clone(),
                        group: group.clone(),
                        partition,
                        status: PolicyStatus::InForce,
                        standing: Some(Standing::Paid),
                        product_code: product_code.clone(),
                    },
                );
                self.by_number.insert(number.clone(), policy.clone());
                self.set_lives(policy, lives);
                self.set_layers(policy, partition, layers);
                self.set_member_covers(policy, partition, None, members);
            }
            Event::TransferLanded {
                policy,
                group,
                lives,
                layers,
                members,
                number,
                product_code,
                status,
                standing,
                ..
            } => {
                // The directory switches to the target when the policy lands.
                // A projection of the target partition alone meets the
                // policy here for the first time.
                let entry = self.directory.entry(policy.clone()).or_insert_with(|| DirectoryEntry {
                    policy: policy.clone(),
                    number: number.clone(),
                    group: group.clone(),
                    partition,
                    status: *status,
                    standing: *standing,
                    product_code: product_code.clone(),
                });
                entry.group = group.clone();
                entry.partition = partition;
                self.by_number.insert(number.clone(), policy.clone());
                self.set_lives(policy, lives);
                self.set_layers(policy, partition, layers);
                self.set_member_covers(policy, partition, None, members);
            }
            Event::PolicyStatusChanged { policy, to, .. } => {
                if *to == PolicyStatus::Transferred {
                    // The stub left behind on the source says Transferred;
                    // the directory follows the live policy, not the stub.
                    // If the directory still places the policy here, this
                    // projection has not seen it land: it is gone from here
                    // until it does.
                    if self.directory.get(policy).is_some_and(|e| e.partition == partition) {
                        self.forget(policy);
                    }
                    return;
                }
                if let Some(e) = self.directory.get_mut(policy) {
                    if e.partition == partition {
                        e.status = *to;
                    }
                }
                let in_force = *to == PolicyStatus::InForce;
                let layers = self.layers.get_mut(policy).into_iter().flat_map(BTreeMap::values_mut);
                let members = self.members.get_mut(policy).into_iter().flat_map(BTreeMap::values_mut);
                for r in layers.chain(members) {
                    r.in_force = in_force;
                }
            }
            Event::StandingChanged { policy, to, .. } => {
                if let Some(e) = self.directory.get_mut(policy) {
                    if e.partition == partition {
                        e.standing = *to;
                    }
                }
            }
            Event::PolicyBalance {
                policy,
                outstanding_minor,
                oldest_unpaid_due,
            } => {
                if self.directory.get(policy).is_some_and(|e| e.partition == partition) {
                    self.billing.insert(
                        policy.clone(),
                        BillingRow {
                            outstanding_minor: *outstanding_minor,
                            oldest_unpaid_due: *oldest_unpaid_due,
                        },
                    );
                }
            }
            Event::LayerAdded { policy, layer, .. } => {
                self.layers
                    .entry(policy.clone())
                    .or_default()
                    .insert((layer.cover.clone(), layer.layer), row(policy, partition, layer));
            }
            Event::LayersMerged {
                policy,
                cover,
                removed,
                into,
                sum_insured_minor,
            } => {
                if let Some(rows) = self.layers.get_mut(policy) {
                    for r in removed {
                        rows.remove(&(cover.clone(), *r));
                    }
                    if let Some(r) = rows.get_mut(&(cover.clone(), *into)) {
                        r.sum_insured_minor = *sum_insured_minor;
                    }
                }
            }
            Event::LayerUnwound { policy, cover, layer, .. } => {
                let emptied = self.layers.get_mut(policy).is_some_and(|rows| {
                    rows.remove(&(cover.clone(), *layer));
                    rows.is_empty()
                });
                if emptied {
                    self.layers.remove(policy);
                }
            }
            Event::CoverExpiryUnwound { policy, layers, .. } => {
                let rows = self.layers.entry(policy.clone()).or_default();
                for layer in layers {
                    rows.insert((layer.cover.clone(), layer.layer), row(policy, partition, layer));
                }
            }
            Event::CoverExpired { policy, cover, .. } => {
                let emptied = self.layers.get_mut(policy).is_some_and(|rows| {
                    rows.retain(|(c, _), _| c != cover);
                    rows.is_empty()
                });
                if emptied {
                    self.layers.remove(policy);
                }
            }
            Event::MemberJoined {
                policy, life, covers, party, ..
            } => {
                self.set_member_covers(policy, partition, Some(life), covers);
                if let Some(party) = party {
                    self.life_party.insert((policy.clone(), life.clone()), party.clone());
                }
            }
            Event::MemberChanged { policy, life, covers, .. } => {
                self.set_member_covers(policy, partition, Some(life), covers);
            }
            Event::LayerEnded { policy, cover, layer, .. } => {
                let emptied = self.layers.get_mut(policy).is_some_and(|rows| {
                    rows.remove(&(cover.clone(), *layer));
                    rows.is_empty()
                });
                if emptied {
                    self.layers.remove(policy);
                }
            }
            Event::MemberLeft { policy, life, .. } => {
                self.set_member_covers(policy, partition, Some(life), &[]);
            }
            Event::MemberIdentified { policy, life, party } => {
                self.life_party.insert((policy.clone(), life.clone()), party.clone());
            }
            _ => {}
        }
    }

    // --- policy directory ---

    pub fn policy(&self, id: &PolicyId) -> Option<&DirectoryEntry> {
        self.directory.get(id)
    }

    /// Policy number to group, partition and status. Used by cash matching,
    /// claim notification and the portals.
    pub fn by_number(&self, number: &str) -> Option<&DirectoryEntry> {
        self.by_number.get(number).and_then(|id| self.directory.get(id))
    }

    pub fn directory(&self) -> impl Iterator<Item = &DirectoryEntry> {
        self.directory.values()
    }

    pub fn billing(&self, id: &PolicyId) -> Option<&BillingRow> {
        self.billing.get(id)
    }

    // --- life index ---

    /// Every policy, cover and layer on a person, across all partitions:
    /// retail layers and cover as a member of a scheme alike.
    pub fn layers_on_party(&self, party: &PartyId) -> Vec<&LifeIndexRow> {
        let layers = self.layers.values().flat_map(BTreeMap::values);
        let members = self.members.values().flat_map(BTreeMap::values);
        layers
            .chain(members)
            .filter(|r| self.life_party.get(&(r.policy.clone(), r.life.clone())) == Some(party))
            .collect()
    }

    /// Sum at risk on a person by cover type: the input to reinsurance
    /// accumulation and the underwriting check on existing cover.
    pub fn sum_at_risk(&self, party: &PartyId) -> BTreeMap<String, i64> {
        let mut out = BTreeMap::new();
        for r in self.layers_on_party(party).into_iter().filter(|r| r.in_force) {
            *out.entry(r.cover_type.clone()).or_insert(0) += r.sum_insured_minor;
        }
        out
    }

    pub fn layers_on_policy(&self, policy: &PolicyId) -> Vec<&LifeIndexRow> {
        let layers = self.layers.get(policy).into_iter().flat_map(BTreeMap::values);
        let members = self.members.get(policy).into_iter().flat_map(BTreeMap::values);
        layers.chain(members).collect()
    }

    pub fn party_of(&self, policy: &PolicyId, life: &LifeId) -> Option<&PartyId> {
        self.life_party.get(&(policy.clone(), life.clone()))
    }
}

fn row(policy: &PolicyId, partition: PartitionId, l: &LayerInfo) -> LifeIndexRow {
    LifeIndexRow {
        policy: policy.clone(),
        partition,
        cover: l.cover.clone(),
        cover_type: l.cover_type.clone(),
        layer: l.layer,
        life: l.life.clone(),
        sum_insured_minor: l.sum_insured_minor,
        in_force: true,
    }
}

/// JSON object keys are strings, so a map keyed by a pair is written as a
/// list of entries.
mod as_pairs {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(map: &BTreeMap<K, V>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<K, V>, D::Error> {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}

/// Scheme members' cover is written as its rows, like the layers.
mod members_as_rows {
    use std::collections::BTreeMap;

    use ic_core::{LifeId, PolicyId};
    use serde::{Deserialize, Deserializer, Serializer};

    use super::LifeIndexRow;

    type Members = BTreeMap<PolicyId, BTreeMap<(LifeId, String), LifeIndexRow>>;

    pub fn serialize<S: Serializer>(members: &Members, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(members.values().flat_map(BTreeMap::values))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Members, D::Error> {
        let mut members = Members::new();
        for row in Vec::<LifeIndexRow>::deserialize(d)? {
            members
                .entry(row.policy.clone())
                .or_default()
                .insert((row.life.clone(), row.cover.clone()), row);
        }
        Ok(members)
    }
}

/// The life index is written as its rows: each row carries its own key.
mod layers_as_rows {
    use std::collections::BTreeMap;

    use ic_core::PolicyId;
    use serde::{Deserialize, Deserializer, Serializer};

    use super::LifeIndexRow;

    type Layers = BTreeMap<PolicyId, BTreeMap<(String, u32), LifeIndexRow>>;

    pub fn serialize<S: Serializer>(layers: &Layers, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(layers.values().flat_map(BTreeMap::values))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Layers, D::Error> {
        let mut layers = Layers::new();
        for row in Vec::<LifeIndexRow>::deserialize(d)? {
            layers.entry(row.policy.clone()).or_default().insert((row.cover.clone(), row.layer), row);
        }
        Ok(layers)
    }
}
