//! Read models: projections of the event stream. They can be thrown away and
//! rebuilt, may lag, and say so: every read that drives a decision records
//! the sequence it reflects.

pub mod scope;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::event::{Event, EventRecord, LayerInfo, LifeInfo};
use ic_core::{GroupId, LifeId, PartyId, PolicyId, PolicyStatus, Standing};
use ic_ledger::PartitionId;
use serde::Serialize;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectionError {
    /// A consumer that sees a gap asks for the missing events.
    #[error("gap on partition {partition}: expected seq {expected}, got {got}")]
    Gap { partition: PartitionId, expected: u64, got: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectoryEntry {
    pub policy: PolicyId,
    pub number: String,
    pub group: GroupId,
    pub partition: PartitionId,
    pub status: PolicyStatus,
    pub standing: Option<Standing>,
    pub product_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BillingRow {
    pub outstanding_minor: i64,
    pub oldest_unpaid_due: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Default)]
pub struct Projections {
    checkpoints: BTreeMap<PartitionId, u64>,
    directory: BTreeMap<PolicyId, DirectoryEntry>,
    by_number: BTreeMap<String, PolicyId>,
    billing: BTreeMap<PolicyId, BillingRow>,
    /// (policy, life) -> party, where the life is named or identified.
    life_party: BTreeMap<(PolicyId, LifeId), PartyId>,
    /// policy -> (cover, layer) -> row.
    layers: BTreeMap<PolicyId, BTreeMap<(String, u32), LifeIndexRow>>,
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

    fn set_lives(&mut self, policy: &PolicyId, lives: &[LifeInfo]) {
        for l in lives {
            if let Some(p) = &l.party {
                self.life_party.insert((policy.clone(), l.life.clone()), p.clone());
            }
        }
    }

    fn set_layers(&mut self, policy: &PolicyId, partition: PartitionId, layers: &[LayerInfo]) {
        let rows = self.layers.entry(policy.clone()).or_default();
        rows.clear();
        for l in layers {
            rows.insert((l.cover.clone(), l.layer), row(policy, partition, l));
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
            }
            Event::TransferLanded {
                policy, group, lives, layers, ..
            } => {
                // The directory switches to the target when the policy lands.
                if let Some(e) = self.directory.get_mut(policy) {
                    e.group = group.clone();
                    e.partition = partition;
                }
                self.set_lives(policy, lives);
                self.set_layers(policy, partition, layers);
            }
            Event::PolicyStatusChanged { policy, to, .. } => {
                // The stub left behind on the source says Transferred; the
                // directory follows the live policy, not the stub.
                if *to == PolicyStatus::Transferred {
                    return;
                }
                if let Some(e) = self.directory.get_mut(policy) {
                    if e.partition == partition {
                        e.status = *to;
                    }
                }
                let in_force = *to == PolicyStatus::InForce;
                if let Some(rows) = self.layers.get_mut(policy) {
                    for r in rows.values_mut() {
                        r.in_force = in_force;
                    }
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
            Event::CoverExpired { policy, cover, .. } => {
                if let Some(rows) = self.layers.get_mut(policy) {
                    rows.retain(|(c, _), _| c != cover);
                }
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

    /// Every policy, cover and layer on a person, across all partitions.
    pub fn layers_on_party(&self, party: &PartyId) -> Vec<&LifeIndexRow> {
        self.layers
            .iter()
            .flat_map(|(policy, rows)| {
                rows.values()
                    .filter(move |r| self.life_party.get(&(policy.clone(), r.life.clone())) == Some(party))
            })
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
        self.layers.get(policy).map(|r| r.values().collect()).unwrap_or_default()
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
