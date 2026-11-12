//! The core's event streams, turned into changes to the covers on each life.
//!
//! Events arrive from every partition, in order per partition and at least
//! once. Nothing here depends on how the partitions' streams interleave: the
//! cession calculation orders covers by their own dates.

use chrono::NaiveDate;
use ic_core::event::{Event, EventRecord, LayerInfo, LifeInfo, MemberCover};
use ic_core::{PolicyId, PolicyStatus};
use ic_ledger::PartitionId;

use crate::cession::{Risk, Source};
use crate::machine::{Ctx, InstructionBody, PolicyInfo, Reinsurance, Reject};
use crate::premium::Period;
use crate::RiskKey;

impl Reinsurance {
    pub(crate) fn core_events(&mut self, ctx: &mut Ctx, events: Vec<EventRecord>) -> Result<(), Reject> {
        // Check the whole batch before applying any of it.
        let mut expected = self.checkpoints.clone();
        for rec in &events {
            // A partition's first event may be any: a module added to a
            // running cluster starts at the oldest event the partition holds.
            let Some(at) = expected.get(&rec.partition).copied() else {
                expected.insert(rec.partition, rec.seq);
                continue;
            };
            if rec.seq > at + 1 {
                return Err(Reject::new(
                    "gap",
                    format!("partition {}: expected event {}, got {}", rec.partition, at + 1, rec.seq),
                ));
            }
            expected.insert(rec.partition, at.max(rec.seq));
        }
        for rec in events {
            if !self.checkpoints.contains_key(&rec.partition) && rec.seq > 1 {
                // Never silent: covers issued before this are not counted
                // against any retention until they are loaded.
                let detail = format!(
                    "partition {} was first read at event {}: the {} before it were not seen, so cover written earlier is missing from the accumulation",
                    rec.partition,
                    rec.seq,
                    rec.seq - 1
                );
                self.set_exception(ctx, format!("feed_start:{}", rec.partition), "events_not_seen", Some(detail));
            } else if rec.seq <= self.checkpoint(rec.partition) {
                continue;
            }
            self.checkpoints.insert(rec.partition, rec.seq);
            if let Event::Module { module, kind, subject, detail } = &rec.event {
                if module == crate::claims::CLAIMS_MODULE {
                    self.claims_event(ctx, &rec.id, kind, subject, detail);
                }
                continue;
            }
            self.project(ctx, rec.partition, rec.business_date, rec.event);
        }
        Ok(())
    }

    /// Send the whole cession record of a policy's covers to a partition.
    /// The core refuses a change to a policy that is moving between
    /// partitions, and a refused command is not applied on a retry with the
    /// same key. So when a transfer lands, or is abandoned, the record is
    /// sent again in full to wherever the policy now is; recording the same
    /// cession twice changes nothing.
    fn resend_record(&mut self, ctx: &mut Ctx, policy: &PolicyId, partition: PartitionId) {
        for key in self.policy_risks(policy) {
            let segments = self.cession_record(&key).to_vec();
            if let Some((from, _)) = segments.first() {
                ctx.instruct(partition, InstructionBody::RecordCession { key, from: *from, segments });
            }
        }
    }

    fn set_lives(&mut self, ctx: &mut Ctx, policy: &PolicyId, lives: &[LifeInfo]) {
        for l in lives {
            if let (Some(party), Some(info)) = (&l.party, self.policies.get_mut(policy)) {
                if info.lives.insert(l.life.clone(), party.clone()).as_ref() != Some(party) {
                    self.relink(ctx, policy, &l.life);
                    self.clear_possible(ctx, policy, &l.life, None);
                }
            }
        }
    }

    fn new_risk(&self, key: RiskKey, partition: PartitionId, cover_type: &str, source: Source, start: NaiveDate, amount: i64) -> Risk {
        let product = self.policies.get(&key.policy).map(|p| p.product.clone()).unwrap_or_default();
        let treaty = self.select_treaty(&product, cover_type, start).map(|t| t.reference());
        let pending = self.pending_terms.get(&(key.policy.clone(), key.cover.clone()));
        Risk {
            partition,
            cover_type: cover_type.to_string(),
            source,
            underwritten: source == Source::Layer,
            start,
            end: None,
            amounts: vec![(start, amount)],
            suspensions: self
                .policies
                .get(&key.policy)
                .and_then(|p| p.out_of_force_from)
                .map(|from| (from.max(start), None))
                .into_iter()
                .collect(),
            treaties: vec![(start, treaty)],
            facultative: pending.map(|p| p.terms.clone()).unwrap_or_default(),
            excluded: pending.is_some_and(|p| p.excluded),
            excess_path: self.schemes.get(&key.policy).map(|s| s.excess_path),
            migrated: None,
            product,
            key,
        }
    }

    fn add_layer(&mut self, ctx: &mut Ctx, policy: &PolicyId, partition: PartitionId, l: &LayerInfo, reopen: bool) {
        let key = RiskKey {
            policy: policy.clone(),
            cover: l.cover.clone(),
            layer: l.layer,
            life: l.life.clone(),
        };
        if let Some(risk) = self.risk_mut(ctx, &key) {
            // Already known: the policy has landed on another partition, or
            // a layer ended in error is restored, with no gap.
            risk.partition = partition;
            if reopen {
                risk.end = None;
            }
            return;
        }
        let risk = self.new_risk(key.clone(), partition, &l.cover_type, Source::Layer, l.start, l.sum_insured_minor);
        self.put_risk(ctx, risk);
        self.pending_terms.remove(&(key.policy.clone(), key.cover.clone()));
        self.settle_confirmed(ctx, &key);
    }

    /// Set a member's cover from a date. Covers the member no longer has end
    /// on that date.
    fn set_member(&mut self, ctx: &mut Ctx, policy: &PolicyId, partition: PartitionId, life: &ic_core::LifeId, effective: NaiveDate, covers: &[MemberCover]) {
        let existing: Vec<RiskKey> = self
            .policy_risks(policy)
            .into_iter()
            .filter(|k| &k.life == life && self.risk(k).is_some_and(|r| r.source == Source::Member))
            .collect();
        for key in &existing {
            let now = covers.iter().find(|c| c.cover == key.cover).map(|c| c.sum_insured_minor);
            let Some(risk) = self.risk_mut(ctx, key) else { continue };
            risk.partition = partition;
            match now {
                Some(amount) => {
                    let at = effective.max(risk.start);
                    if risk.end.is_some_and(|e| e <= at) {
                        // The member has come back to this cover.
                        risk.suspensions.push((risk.end.unwrap_or(at), Some(at)));
                        risk.end = None;
                    }
                    if risk.amount_on(at) != amount || risk.amounts.iter().any(|(d, _)| *d > at) {
                        risk.amounts.retain(|(d, _)| *d < at);
                        risk.amounts.push((at, amount));
                    }
                }
                None => end_risk(risk, effective),
            }
        }
        for c in covers.iter().filter(|c| !existing.iter().any(|k| k.cover == c.cover)) {
            let key = RiskKey {
                policy: policy.clone(),
                cover: c.cover.clone(),
                layer: 0,
                life: c.life.clone(),
            };
            let risk = self.new_risk(key, partition, &c.cover_type, Source::Member, effective, c.sum_insured_minor);
            self.put_risk(ctx, risk);
        }
    }

    fn set_in_force(&mut self, ctx: &mut Ctx, policy: &PolicyId, in_force: bool, on: NaiveDate) {
        let Some(info) = self.policies.get_mut(policy) else { return };
        if info.out_of_force_from.is_none() != in_force {
            info.out_of_force_from = (!in_force).then_some(on);
            for key in self.policy_risks(policy) {
                let Some(risk) = self.risk_mut(ctx, &key) else { continue };
                let at = on.max(risk.start);
                if in_force {
                    // A cover of a policy out of force carries one open suspension.
                    for (_, until) in risk.suspensions.iter_mut().filter(|(_, until)| until.is_none()) {
                        *until = Some(at);
                    }
                } else {
                    risk.suspensions.push((at, None));
                }
            }
        }
    }

    fn project(&mut self, ctx: &mut Ctx, partition: PartitionId, business_date: NaiveDate, event: Event) {
        match event {
            Event::PolicyIssued {
                policy,
                number,
                product_code,
                inception,
                lives,
                layers,
                members,
                ..
            } => {
                self.policies.insert(
                    policy.clone(),
                    PolicyInfo {
                        number,
                        product: product_code,
                        partition,
                        lives: Default::default(),
                        out_of_force_from: None,
                    },
                );
                self.set_lives(ctx, &policy, &lives);
                for l in &layers {
                    self.add_layer(ctx, &policy, partition, l, false);
                }
                for m in &members {
                    self.set_member(ctx, &policy, partition, &m.life, inception, std::slice::from_ref(m));
                }
            }
            Event::TransferLanded {
                policy,
                lives,
                layers,
                members,
                number,
                product_code,
                status,
                ..
            } => {
                // The policy keeps its covers and their dates; only where
                // its entries are posted changes.
                let info = self.policies.entry(policy.clone()).or_insert_with(|| PolicyInfo {
                    number,
                    product: product_code,
                    partition,
                    lives: Default::default(),
                    out_of_force_from: (!in_force(status)).then_some(business_date),
                });
                info.partition = partition;
                self.set_lives(ctx, &policy, &lives);
                for l in &layers {
                    self.add_layer(ctx, &policy, partition, l, false);
                }
                for key in self.policy_risks(&policy) {
                    if let Some(risk) = self.risk_mut(ctx, &key) {
                        risk.partition = partition;
                    }
                }
                for m in &members {
                    let key = RiskKey {
                        policy: policy.clone(),
                        cover: m.cover.clone(),
                        layer: 0,
                        life: m.life.clone(),
                    };
                    if self.risk(&key).is_none() {
                        self.set_member(ctx, &policy, partition, &m.life, business_date, std::slice::from_ref(m));
                    }
                }
                self.resend_record(ctx, &policy, partition);
            }
            // The policy stays where it was, and changes refused while it
            // was frozen have to reach it now.
            Event::TransferAborted { policy, .. } => self.resend_record(ctx, &policy, partition),
            // The stub a transfer leaves behind says Transferred. The policy
            // itself is still in force where it landed.
            Event::PolicyStatusChanged {
                to: PolicyStatus::Transferred, ..
            } => {}
            Event::PolicyStatusChanged { policy, to, .. } => {
                if self.policies.get(&policy).is_some_and(|p| p.partition == partition) {
                    self.set_in_force(ctx, &policy, in_force(to), business_date);
                }
            }
            Event::LayerAdded { policy, layer, .. } => self.add_layer(ctx, &policy, partition, &layer, true),
            Event::LayersMerged {
                policy,
                cover,
                removed,
                into,
                sum_insured_minor,
            } => {
                for key in self.policy_risks(&policy).into_iter().filter(|k| k.cover == cover) {
                    let Some(risk) = self.risk_mut(ctx, &key) else { continue };
                    if removed.contains(&key.layer) {
                        end_risk(risk, business_date);
                    } else if key.layer == into {
                        let at = business_date.max(risk.start);
                        risk.amounts.retain(|(d, _)| *d < at);
                        risk.amounts.push((at, sum_insured_minor));
                    }
                }
            }
            Event::CoverExpired { policy, cover, on } => {
                for key in self.policy_risks(&policy).into_iter().filter(|k| k.cover == cover) {
                    if let Some(risk) = self.risk_mut(ctx, &key) {
                        end_risk(risk, on);
                    }
                }
            }
            Event::MemberJoined {
                policy,
                life,
                effective,
                covers,
                party,
                ..
            } => {
                self.set_lives(ctx, &policy, &[LifeInfo::new(life.clone(), party)]);
                self.set_member(ctx, &policy, partition, &life, effective, &covers);
            }
            Event::MemberChanged {
                policy,
                life,
                effective,
                covers,
                ..
            } => self.set_member(ctx, &policy, partition, &life, effective, &covers),
            Event::MemberLeft { policy, life, effective } => self.set_member(ctx, &policy, partition, &life, effective, &[]),
            Event::MemberIdentified { policy, life, party } => {
                self.set_lives(ctx, &policy, &[LifeInfo::new(life, Some(party))]);
            }
            Event::PremiumDue {
                policy,
                receivable,
                period_start,
                period_end,
                by_cover_type,
                ..
            } => {
                // A period billed again after an unwind replaces the reversed
                // billing; what was charged for it stays, to be netted.
                let charged = self.periods.remove(&(policy.clone(), period_start)).map(|p| p.charged).unwrap_or_default();
                self.periods.insert(
                    (policy.clone(), period_start),
                    Period {
                        end: period_end,
                        receivable,
                        reversed: false,
                        by_cover_type,
                        charged,
                    },
                );
                self.reprice(ctx, &policy, period_start);
            }
            // An underwritten layer ended on its own: a decrease, or a scheme
            // member's excess cover when they leave.
            Event::LayerEnded {
                policy,
                cover,
                layer,
                life,
                effective,
                ..
            } => {
                let key = RiskKey { policy, cover, layer, life };
                if let Some(risk) = self.risk_mut(ctx, &key) {
                    end_risk(risk, effective);
                }
            }
            // A layer that started after the unwind date never started.
            Event::LayerUnwound {
                policy, cover, layer, start, ..
            } => {
                for key in self.policy_risks(&policy).into_iter().filter(|k| k.cover == cover && k.layer == layer) {
                    if let Some(risk) = self.risk_mut(ctx, &key) {
                        risk.end = Some(start.max(risk.start));
                    }
                }
            }
            // A cover that expired after the unwind date has not expired.
            Event::CoverExpiryUnwound { policy, cover, layers, .. } => {
                for l in &layers {
                    let key = RiskKey {
                        policy: policy.clone(),
                        cover: cover.clone(),
                        layer: l.layer,
                        life: l.life.clone(),
                    };
                    if let Some(risk) = self.risk_mut(ctx, &key) {
                        risk.end = None;
                    }
                }
            }
            // The billing of these periods was reversed: so is what was
            // ceded for them, by new entries with the periods' value dates.
            Event::PolicyUnwound { policy, receivables, .. } => {
                let starts: Vec<NaiveDate> = self
                    .periods
                    .range_mut((policy.clone(), NaiveDate::MIN)..)
                    .take_while(|((p, _), _)| *p == policy)
                    .filter(|(_, period)| receivables.contains(&period.receivable) && !period.reversed)
                    .map(|((_, start), period)| {
                        period.reversed = true;
                        *start
                    })
                    .collect();
                for start in starts {
                    self.reprice(ctx, &policy, start);
                }
            }
            // The day of the event was the last day of cover.
            Event::PolicyTerminated { policy, effective, .. } => {
                let ends = effective.succ_opt().unwrap_or(effective);
                for key in self.policy_risks(&policy) {
                    if let Some(risk) = self.risk_mut(ctx, &key) {
                        end_risk(risk, ends);
                    }
                }
            }
            _ => {}
        }
    }
}

fn in_force(status: PolicyStatus) -> bool {
    matches!(status, PolicyStatus::InForce | PolicyStatus::PaidUp)
}

/// End a cover on a date. A cover that ends on or before the day it starts
/// was never in force.
fn end_risk(risk: &mut Risk, on: NaiveDate) {
    let on = on.max(risk.start);
    if risk.end.is_none_or(|e| on < e) {
        risk.end = Some(on);
    }
}
