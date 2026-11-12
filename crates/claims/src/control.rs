//! Standing controls: the exclusion-match control, peer review by sample,
//! and what the reinsurance module tells claims.

use std::ops::Bound;

use sha2::{Digest, Sha256};

use crate::command::ReinsuranceNotice;
use crate::config::{Match, Queue, Rules};
use crate::intake::finish_task;
use crate::model::{Claim, Decision, DecisionKind, Status, TaskKind};
use crate::port::Reject;
use crate::state::{note, open_task, ClaimsState, Cx};
use crate::ClaimId;

/// Claims one pass of the exclusion-match control looks at per command.
pub const CONTROL_CHUNK: usize = 200;

impl ClaimsState {
    /// Peer review by sample, weighted to large amounts and partial
    /// payments. The draw is a hash of the claim and decision, so every
    /// replica draws the same sample.
    pub(crate) fn sample(&self, claim: &mut Claim, cx: &Cx<'_>, decision: &Decision) {
        let pct = self.rules.sample_pct.get(&decision.kind).copied().unwrap_or(0);
        let large = decision.amount_minor >= self.rules.large_claim_minor;
        let partial = decision.kind == DecisionKind::Admit && decision.amount_minor < claim.claimed_minor && claim.schedule.is_none();
        let weight = pct * (1 + u32::from(large) + u32::from(partial));
        let mut h = Sha256::new();
        h.update(b"ic.claims.sample.v1");
        h.update(claim.id.as_bytes());
        h.update(decision.no.to_be_bytes());
        let digest = h.finalize();
        let draw = u32::from(u16::from_be_bytes([digest[0], digest[1]])) % 100;
        if draw < weight {
            open_task(claim, cx, format!("quality-review/{}", decision.no), TaskKind::QualityReview, None);
        }
    }

    /// The standing control: every claim in payment whose recorded cause
    /// matches an exclusion on the paying cover goes to senior review. It
    /// reads the cover as the policy record has it now, so an exclusion that
    /// was missed at admission is seen here.
    pub(crate) fn exclusion_control(&mut self, cx: &mut Cx<'_>, after: Option<String>) -> Result<(), Reject> {
        self.configured()?;
        if after.is_none() {
            self.control.scanned = 0;
            self.control.hits = 0;
        }
        let from = after.map_or(Bound::Unbounded, Bound::Excluded);
        let ids: Vec<ClaimId> = self.in_payment.range((from, Bound::Unbounded)).take(CONTROL_CHUNK + 1).cloned().collect();
        for id in ids.iter().take(CONTROL_CHUNK) {
            cx.core.work(1);
            let Ok(mut claim) = self.claim(id) else { continue };
            self.control.scanned += 1;
            let recorded = claim
                .cover_check
                .as_ref()
                .map(|c| (c.facts.exclusions.clone(), c.facts.cover_type.clone()))
                .unwrap_or_default();
            let read = cx.core.cover_facts(&claim.policy, &claim.cover, claim.life.as_deref(), claim.event_date);
            let (codes, cover_type) = read.map(|f| (f.exclusions, f.cover_type)).unwrap_or(recorded);
            if self.rules.exclusion_check(&codes, &cover_type, claim.cause_code.as_deref()).0 != Match::Applies {
                continue;
            }
            // One task per cause and rules version: a hit a senior has
            // already looked at is not raised again by every pass.
            let task = format!("exclusion-match/{}/{}", claim.cause_code.clone().unwrap_or_default(), self.rules.version);
            if open_task(&mut claim, cx, task, TaskKind::ExclusionMatch, None) {
                self.control.hits += 1;
                claim.queue = Queue::ComplexLarge;
                note(&mut claim, cx, None, "exclusion_match");
                cx.notify("exclusion_match", id, &[]);
                self.commit(claim);
            }
        }
        self.control.cursor = if ids.len() > CONTROL_CHUNK {
            ids.get(CONTROL_CHUNK - 1).cloned()
        } else {
            None
        };
        self.control.last_run = Some(cx.today);
        let detail = [
            ("scanned", self.control.scanned.to_string()),
            ("hits", self.control.hits.to_string()),
            ("complete", self.control.cursor.is_none().to_string()),
        ];
        cx.notify("exclusion_control_run", "exclusion-control", &detail);
        Ok(())
    }

    /// The dry run for a proposed rules version: every admitted claim whose
    /// cause the proposed rules would match to an exclusion on its cover.
    pub fn exclusion_hits(&self, rules: &Rules) -> Vec<ClaimId> {
        let admitted = |c: &&Claim| matches!(c.status, Status::Admitted | Status::InPayment | Status::Paid | Status::Ended);
        let hit = |c: &&Claim| {
            c.cover_check
                .as_ref()
                .is_some_and(|k| rules.exclusion_check(&k.facts.exclusions, &k.facts.cover_type, c.cause_code.as_deref()).0 == Match::Applies)
        };
        self.claims.values().filter(admitted).filter(hit).map(|c| c.id.clone()).collect()
    }

    pub(crate) fn reinsurance(&mut self, cx: &mut Cx<'_>, notice: ReinsuranceNotice) -> Result<(), Reject> {
        let id = match &notice {
            ReinsuranceNotice::LargeClaimNotification { claim, .. }
            | ReinsuranceNotice::ReinsurerConsentRequired { claim, .. }
            | ReinsuranceNotice::ReinsurerConsent { claim, .. }
            | ReinsuranceNotice::DisputedItemRaised { claim, .. }
            | ReinsuranceNotice::DisputeResolved { claim, .. } => claim.clone(),
        };
        let mut claim = self.claim(&id)?;
        match notice {
            ReinsuranceNotice::LargeClaimNotification { treaty, .. } => {
                claim.reinsurance.large_claim_notified = true;
                note(&mut claim, cx, None, format!("large_claim_notified:{treaty}"));
            }
            ReinsuranceNotice::ReinsurerConsentRequired { treaty, due_by, .. } => {
                // Admission waits for the reinsurer, on a task with a date.
                open_task(&mut claim, cx, format!("reinsurer-consent/{treaty}"), TaskKind::ReinsurerConsent, Some(due_by));
                note(&mut claim, cx, None, format!("reinsurer_consent_required:{treaty}"));
                claim.reinsurance.consent_required.insert(treaty);
            }
            ReinsuranceNotice::ReinsurerConsent {
                reinsurer,
                agreed,
                reference,
                treaty,
                ..
            } => {
                let answered: Vec<String> = claim
                    .reinsurance
                    .consent_required
                    .iter()
                    .filter(|t| treaty.as_ref().is_none_or(|x| x == *t))
                    .cloned()
                    .collect();
                let outcome = if agreed { "agreed" } else { "refused" };
                for t in answered {
                    claim.reinsurance.consent_required.remove(&t);
                    finish_task(&mut claim, &format!("reinsurer-consent/{t}"), cx.today, None, outcome.into(), vec![], None, 0);
                }
                if agreed {
                    note(&mut claim, cx, None, format!("reinsurer_consent:{reinsurer}"));
                    claim.reinsurance.consents.push((reinsurer, reference));
                } else {
                    // The decision is still the insurer's, and a senior makes
                    // it: the claim moves to the senior queue with a review.
                    claim.reinsurance.consent_refused = true;
                    if claim.status == Status::Open {
                        claim.queue = Queue::ComplexLarge;
                        let senior = claim.owner.as_ref().is_some_and(|o| self.authority.profile(o).is_ok_and(|p| p.senior));
                        if !senior {
                            claim.owner = None;
                        }
                    }
                    open_task(&mut claim, cx, "senior-review/reinsurer-refused", TaskKind::SeniorReview, None);
                    note(&mut claim, cx, None, format!("reinsurer_refused:{reinsurer}"));
                }
            }
            ReinsuranceNotice::DisputedItemRaised { reinsurer, reference, .. } => {
                // Settled by people under the treaty, never reversed automatically.
                note(&mut claim, cx, None, format!("disputed_item:{reinsurer}"));
                claim.reinsurance.disputed_items.push((reinsurer, reference));
            }
            ReinsuranceNotice::DisputeResolved { reference, repay_minor, .. } => {
                // The settlement is the reinsurance module's entry. Claims
                // records that the item is closed and for how much, so the
                // claim's file shows no dispute left open.
                if !claim.reinsurance.disputed_items.iter().any(|(_, r)| *r == reference) {
                    return Err(Reject::new("unknown_dispute", "no such disputed item was raised on this claim"));
                }
                if claim.reinsurance.disputes_settled.contains_key(&reference) {
                    return Ok(());
                }
                note(&mut claim, cx, None, format!("dispute_resolved:{reference}"));
                claim.reinsurance.disputes_settled.insert(reference, repay_minor);
            }
        }
        self.commit(claim);
        Ok(())
    }
}
