//! The capacity step: what the insurer already holds on a life, and whether
//! the treaty can take a new cover automatically, asked before any terms are
//! offered.
//!
//! Capacity is reserved for the life of the proposal, so two proposals on
//! the same life see each other. A reservation takes its place in the same
//! fixed order as issued cover, by commencement date and then by key, so the
//! answer does not depend on which proposal asked first.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::{LifeId, PartyId, PolicyId};
use ic_ledger::PartitionId;
use serde::{Deserialize, Serialize};

use crate::cession::{cession_on, timeline, Facultative, Risk, Source, UnplacedReason};
use crate::machine::{Ctx, FacTarget, FacultativeTerms, Notice, Reinsurance, Reject};
use crate::{LifeKey, ReinsurerId, RiskKey, TreatyRef};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityRequest {
    /// Idempotent: the proposal, the life and the cover.
    pub reservation_key: String,
    pub party: PartyId,
    pub product_code: String,
    pub cover_code: String,
    pub cover_type: String,
    pub sum_insured_minor: i64,
    pub commencement: NaiveDate,
    /// The reservation is released after this date unless confirmed.
    pub proposal_expires: NaiveDate,
    /// Opaque, non-clinical codes for what the risk is, checked against the
    /// treaty's exclusions.
    #[serde(default)]
    pub risk_codes: Vec<String>,
    /// For an excess over a scheme's retention: the reference of the
    /// module's `RequireUnderwrittenExcess` notice
    /// (`UWX/<cover key>/<amount>`), so a person can find the reservation
    /// underwriting it ([`Reinsurance::excess_underwriting`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excess_reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    Held,
    /// Issued. The reservation stands in for the layers until their events
    /// arrive.
    Confirmed {
        policy: PolicyId,
        cover: String,
        layers: Vec<u32>,
    },
    Released {
        reason: String,
    },
}

/// Where a facultative referral has got to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacultativeStatus {
    Pending,
    Offered {
        reference: String,
        reinsurer: ReinsurerId,
        amount_minor: i64,
        loading: Option<String>,
        exclusions: Vec<String>,
        terms_ref: String,
    },
    Declined {
        reference: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub request: CapacityRequest,
    pub status: ReservationStatus,
    pub facultative: FacultativeStatus,
}

/// Facultative terms waiting for the layer they belong to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTerms {
    pub terms: Vec<Facultative>,
    pub excluded: bool,
}

/// What is already held on the life when the new cover would start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub retained_minor: i64,
    pub ceded_minor: i64,
    pub facultative_minor: i64,
    pub unplaced_minor: i64,
    /// Sums reserved for other proposals on the life.
    pub reserved_minor: i64,
    /// Scheme cover on members who may be this person but have not been
    /// confirmed as them. Not counted in the outcome; shown so that an
    /// underwriter can see it.
    pub possible_match_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The treaty takes the cover without referral.
    Automatic { retained_minor: i64, ceded_minor: i64 },
    /// Refer to a reinsurer for facultative terms before any offer.
    FacultativeRequired {
        reason_code: String,
        retained_minor: i64,
        excess_minor: i64,
    },
    /// The treaty does not accept this risk: the whole cover needs
    /// facultative terms.
    Excluded { reason_code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityAnswer {
    pub reservation_id: String,
    pub treaty: Option<TreatyRef>,
    pub outcome: Outcome,
    pub held: Held,
    /// The event sequence of each partition this answer reflects.
    pub life_index_sequence: BTreeMap<PartitionId, u64>,
}

fn reservation_risk_key(id: &str, cover: &str) -> RiskKey {
    RiskKey {
        // No policy ID starts with a tilde.
        policy: PolicyId(format!("~reservation/{id}")),
        cover: cover.to_string(),
        layer: 0,
        life: LifeId(String::new()),
    }
}

impl Reinsurance {
    fn reservation_key(&self, id: &str) -> Option<RiskKey> {
        self.reservations.get(id).map(|r| reservation_risk_key(id, &r.request.cover_code))
    }

    pub(crate) fn reserve(&mut self, ctx: &mut Ctx, request: CapacityRequest) -> Result<(), Reject> {
        if let Some(have) = self.reservations.get(&request.reservation_key) {
            return if have.request == request {
                Ok(())
            } else {
                Err(Reject::new("reservation_exists", "the reservation key is already used with other details"))
            };
        }
        if request.reservation_key.is_empty() || request.sum_insured_minor <= 0 || request.proposal_expires < ctx.on {
            return Err(Reject::new(
                "invalid_request",
                "a reservation needs a key, a sum insured above zero and an expiry that has not passed",
            ));
        }
        let treaty = self.select_treaty(&request.product_code, &request.cover_type, request.commencement);
        let key = reservation_risk_key(&request.reservation_key, &request.cover_code);
        let risk = Risk {
            key,
            partition: 0,
            product: request.product_code.clone(),
            cover_type: request.cover_type.clone(),
            source: Source::Reservation,
            underwritten: true,
            start: request.commencement,
            end: None,
            amounts: vec![(request.commencement, request.sum_insured_minor)],
            suspensions: vec![],
            treaties: vec![(request.commencement, treaty.map(|t| t.reference()))],
            facultative: vec![],
            excluded: treaty.is_some_and(|t| t.excludes(&request.risk_codes)),
            excess_path: None,
            migrated: None,
            continues: None,
        };
        self.put_risk_on(ctx, LifeKey::Party(request.party.clone()), risk);
        self.held_until.insert((request.proposal_expires, request.reservation_key.clone()));
        self.reservations.insert(
            request.reservation_key.clone(),
            Reservation {
                request,
                status: ReservationStatus::Held,
                facultative: FacultativeStatus::Pending,
            },
        );
        Ok(())
    }

    fn drop_reservation_risk(&mut self, ctx: &mut Ctx, id: &str) -> Option<Risk> {
        let key = self.reservation_key(id)?;
        let life = self.risk_life.remove(&key)?;
        let book = self.lives.get_mut(&life)?;
        let risk = book.risks.remove(&key);
        if book.risks.is_empty() {
            self.lives.remove(&life);
        } else {
            ctx.dirty.insert(life);
        }
        risk
    }

    pub(crate) fn release(&mut self, ctx: &mut Ctx, id: &str, reason: &str) -> Result<(), Reject> {
        let r = self
            .reservations
            .get(id)
            .ok_or_else(|| Reject::new("unknown_reservation", format!("no reservation {id}")))?;
        match r.status {
            ReservationStatus::Released { .. } => return Ok(()),
            ReservationStatus::Confirmed { .. } => return Err(Reject::new("reservation_confirmed", "the cover has been issued")),
            ReservationStatus::Held => {}
        }
        self.held_until.remove(&(r.request.proposal_expires, id.to_string()));
        self.drop_reservation_risk(ctx, id);
        if let Some(r) = self.reservations.get_mut(id) {
            r.status = ReservationStatus::Released { reason: reason.to_string() };
            forget_offer_detail(&mut r.facultative);
        }
        ctx.notices.push(Notice::CapacityReleased {
            reservation_id: id.to_string(),
            reason: reason.to_string(),
        });
        Ok(())
    }

    /// Release every held reservation whose proposal expired before today.
    pub(crate) fn expire_reservations(&mut self, ctx: &mut Ctx) {
        while let Some((_, id)) = self.held_until.first().filter(|(expires, _)| *expires < ctx.on).cloned() {
            // A held reservation always releases.
            let _ = self.release(ctx, &id, "expired");
        }
    }

    pub(crate) fn confirm(&mut self, ctx: &mut Ctx, id: &str, policy: PolicyId, cover: String, layers: Vec<u32>) -> Result<(), Reject> {
        let r = self
            .reservations
            .get(id)
            .ok_or_else(|| Reject::new("unknown_reservation", format!("no reservation {id}")))?;
        let confirmed = ReservationStatus::Confirmed {
            policy: policy.clone(),
            cover: cover.clone(),
            layers: layers.clone(),
        };
        match &r.status {
            ReservationStatus::Held if !layers.is_empty() => {}
            ReservationStatus::Held => return Err(Reject::new("invalid_request", "a confirmation names the layers that were issued")),
            same if *same == confirmed => return Ok(()),
            ReservationStatus::Confirmed { .. } => return Err(Reject::new("reservation_confirmed", "the reservation was confirmed against another cover")),
            ReservationStatus::Released { .. } => return Err(Reject::new("reservation_released", "the reservation was released")),
        }
        self.held_until.remove(&(r.request.proposal_expires, id.to_string()));
        // The reinsurer's terms move to the cover. They wait for its first
        // layer if the core's event has not arrived yet.
        let (terms, excluded) = self
            .reservation_key(id)
            .and_then(|k| self.risk(&k))
            .map(|r| (r.facultative.clone(), r.excluded))
            .unwrap_or_default();
        self.confirming.insert((policy.clone(), cover.clone()), id.to_string());
        if let Some(r) = self.reservations.get_mut(id) {
            r.status = confirmed;
            forget_offer_detail(&mut r.facultative);
        }
        let first = self.policy_risks(&policy).into_iter().find(|k| k.cover == cover && layers.contains(&k.layer));
        let Some(key) = first else {
            // The core's event has not arrived yet: the terms wait for the
            // cover's first layer.
            self.pending_terms.insert((policy, cover), crate::capacity::PendingTerms { terms, excluded });
            return Ok(());
        };
        let Some(risk) = self.risk_mut(ctx, &key) else { return Ok(()) };
        let start = risk.start;
        risk.facultative.extend(terms.into_iter().map(|f| Facultative { from: start, ..f }));
        risk.excluded = excluded;
        self.settle_confirmed(ctx, &key);
        Ok(())
    }

    /// Once every layer a confirmed reservation stood in for has arrived,
    /// the reservation leaves the accumulation.
    pub(crate) fn settle_confirmed(&mut self, ctx: &mut Ctx, key: &RiskKey) {
        let cover = (key.policy.clone(), key.cover.clone());
        let Some(id) = self.confirming.get(&cover).cloned() else { return };
        // Only a confirmed reservation is ever in `confirming`.
        let status = self.reservations.get(&id).map(|r| &r.status);
        let Some(ReservationStatus::Confirmed { layers, .. }) = status else { return };
        let have = self.policy_risks(&key.policy);
        if layers.iter().all(|l| have.iter().any(|k| k.cover == key.cover && k.layer == *l)) {
            self.drop_reservation_risk(ctx, &id);
            self.confirming.remove(&cover);
        }
    }

    pub(crate) fn record_facultative(&mut self, ctx: &mut Ctx, t: FacultativeTerms) -> Result<(), Reject> {
        if t.reference.is_empty() || t.amount_minor <= 0 || t.rate < 0 || t.terms_ref.is_empty() {
            return Err(Reject::new(
                "invalid_terms",
                "facultative terms need a reference, an amount above zero and where the terms are filed",
            ));
        }
        let (key, from) = match &t.target {
            FacTarget::Reservation(id) => {
                let r = self
                    .reservations
                    .get(id)
                    .ok_or_else(|| Reject::new("unknown_reservation", format!("no reservation {id}")))?;
                if r.status != ReservationStatus::Held {
                    return Err(Reject::new("reservation_not_held", "terms can be recorded only while the proposal is open"));
                }
                (reservation_risk_key(id, &r.request.cover_code), r.request.commencement)
            }
            FacTarget::Risk(key) => {
                let risk = self.risk(key).ok_or_else(|| Reject::new("unknown_risk", format!("no cover {key}")))?;
                (key.clone(), ctx.on.max(risk.start))
            }
        };
        if self.risk(&key).is_some_and(|r| r.facultative.iter().any(|f| f.reference == t.reference)) {
            return Err(Reject::new("duplicate_reference", "these facultative terms are already recorded"));
        }
        if let Some(risk) = self.risk_mut(ctx, &key) {
            risk.facultative.push(Facultative {
                reference: t.reference.clone(),
                reinsurer: t.reinsurer.clone(),
                amount_minor: t.amount_minor,
                rate: t.rate,
                from,
                terms_ref: t.terms_ref.clone(),
            });
        }
        let mut reference = String::new();
        if let FacTarget::Reservation(id) = t.target {
            if let Some(r) = self.reservations.get_mut(&id) {
                reference = t.reference.clone();
                r.facultative = FacultativeStatus::Offered {
                    reference: t.reference,
                    reinsurer: t.reinsurer,
                    amount_minor: t.amount_minor,
                    loading: t.loading,
                    exclusions: t.exclusions,
                    terms_ref: t.terms_ref,
                };
            }
            ctx.notices.push(Notice::FacultativeTermsRecorded {
                reservation_id: id,
                offered: true,
                reference,
            });
        }
        Ok(())
    }

    pub(crate) fn decline_facultative(&mut self, ctx: &mut Ctx, id: &str, reference: String) -> Result<(), Reject> {
        match self.reservations.get_mut(id) {
            Some(r) if r.status == ReservationStatus::Held && r.facultative == FacultativeStatus::Pending => {
                r.facultative = FacultativeStatus::Declined { reference: reference.clone() };
                ctx.notices.push(Notice::FacultativeTermsRecorded {
                    reservation_id: id.to_string(),
                    offered: false,
                    reference,
                });
                Ok(())
            }
            Some(_) => Err(Reject::new("reservation_not_held", "the proposal is not waiting for facultative terms")),
            None => Err(Reject::new("unknown_reservation", format!("no reservation {id}"))),
        }
    }

    pub fn reservation(&self, id: &str) -> Option<&Reservation> {
        self.reservations.get(id)
    }

    /// Where the facultative referral for a reservation has got to.
    pub fn facultative_terms(&self, id: &str) -> Option<&FacultativeStatus> {
        self.reservations.get(id).map(|r| &r.facultative)
    }

    /// The capacity answer for a reservation, as things stand now. `None`
    /// once the reservation has been released or its cover has arrived.
    /// The reservations underwriting the excess a `RequireUnderwrittenExcess`
    /// notice named, oldest key first, with where each has got to.
    pub fn excess_underwriting(&self, reference: &str) -> Vec<(String, ReservationStatus)> {
        self.reservations
            .iter()
            .filter(|(_, r)| r.request.excess_reference.as_deref() == Some(reference))
            .map(|(key, r)| (key.clone(), r.status.clone()))
            .collect()
    }

    pub fn capacity(&self, id: &str) -> Option<CapacityAnswer> {
        let r = self.reservations.get(id)?;
        let key = reservation_risk_key(id, &r.request.cover_code);
        let book = self.lives.get(self.risk_life.get(&key)?)?;
        let on = r.request.commencement;
        let all = timeline(book.risks.values(), |t| self.treaty(t), true);
        let mine = cession_on(all.get(&key)?, on)?;
        let mut held = Held::default();
        for (other, segments) in all.iter().filter(|(k, _)| **k != key) {
            let Some(c) = cession_on(segments, on) else { continue };
            if book.risks.get(other).is_some_and(|r| r.source == Source::Reservation) {
                held.reserved_minor += c.sum_at_risk_minor;
            } else {
                held.retained_minor += c.retained_minor;
                held.ceded_minor += c.treaty_ceded();
                held.facultative_minor += c.facultative_ceded();
                held.unplaced_minor += c.unplaced_minor;
            }
        }
        for ((policy, life), parties) in &self.possible {
            if parties.contains(&r.request.party) {
                let member = LifeKey::Unmatched {
                    policy: policy.clone(),
                    life: life.clone(),
                };
                let risks = self.lives.get(&member).into_iter().flat_map(|b| b.risks.values());
                held.possible_match_minor += risks.filter(|r| r.in_force_on(on)).map(|r| r.amount_on(on)).sum::<i64>();
            }
        }
        let outcome = match mine.unplaced_reason {
            None => Outcome::Automatic {
                retained_minor: mine.retained_minor,
                ceded_minor: mine.treaty_ceded() + mine.facultative_ceded(),
            },
            Some(UnplacedReason::ExcludedRisk) => Outcome::Excluded {
                reason_code: "excluded_risk".to_string(),
            },
            Some(_) => Outcome::FacultativeRequired {
                reason_code: "above_automatic_limit".to_string(),
                retained_minor: mine.retained_minor,
                excess_minor: mine.unplaced_minor,
            },
        };
        Some(CapacityAnswer {
            reservation_id: id.to_string(),
            treaty: mine.treaty.clone(),
            outcome,
            held,
            life_index_sequence: self.checkpoints.clone(),
        })
    }
}

/// The reinsurer's loading and exclusions are needed only while the proposal
/// is open.
fn forget_offer_detail(status: &mut FacultativeStatus) {
    if let FacultativeStatus::Offered { loading, exclusions, .. } = status {
        *loading = None;
        exclusions.clear();
    }
}
