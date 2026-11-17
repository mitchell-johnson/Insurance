//! What arrives from outside the underwriting team: evidence, the
//! reinsurer's answers, the core's replies, the passing of time, and the
//! adviser licensing feed.

use std::ops::Bound;

use chrono::{Duration, NaiveDate};
use ic_core::LifeId;

use crate::case::{CaseType, Escalation, Queue};
use crate::codes::reason;
use crate::command::{CapacityAnswer, Effect, Event, FacultativeOutcome, LicenceStatus, NoticeKind, Recipient, Reject, View};
use crate::config::Config;
use crate::evidence::EvidenceStatus;
use crate::flow;
use crate::machine::{life_mut, reject, Cx, UnderwritingState};
use crate::model::{AccessEnd, CapacityState, OfferStatus, Proposal, ProposalId, ProposalStatus, Stage};

pub(crate) fn acknowledged(_st: &UnderwritingState, p: &mut Proposal, life: &LifeId, _cx: &mut Cx, request: &str, provider_ref: String) -> Result<(), Reject> {
    match life_mut(p, life)?.evidence.iter_mut().find(|e| e.id == request && e.outstanding()) {
        Some(e) => {
            e.provider_ref = Some(provider_ref);
            Ok(())
        }
        None => reject("not_outstanding", format!("evidence request {request} is not outstanding")),
    }
}

/// Evidence arrived (`received` carries where it is and what it cost) or the
/// source could not supply it.
pub(crate) fn settled(
    _st: &UnderwritingState,
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    request: &str,
    received: Option<(String, i64)>,
) -> Result<(), Reject> {
    let proposal = p.id.clone();
    let Some(e) = life_mut(p, life)?.evidence.iter_mut().find(|e| e.id == request && e.outstanding()) else {
        return reject("not_outstanding", format!("evidence request {request} is not outstanding"));
    };
    e.status = match received {
        Some((vault_ref, cost_minor)) => {
            if vault_ref.is_empty() || cost_minor < 0 {
                return reject("evidence", "received evidence needs where it is sealed, and a cost that is not negative");
            }
            e.vault_ref = Some(vault_ref);
            e.cost_minor = cost_minor;
            EvidenceStatus::Received
        }
        None => EvidenceStatus::Unobtainable,
    };
    cx.event(Event::EvidenceSettled {
        proposal,
        life: life.clone(),
        request: request.to_string(),
        status: e.status,
    });
    flow::evidence_settled(p, life, cx)
}

pub(crate) fn capacity(st: &UnderwritingState, p: &mut Proposal, life: &LifeId, cover: &str, cx: &mut Cx, answer: CapacityAnswer) -> Result<(), Reject> {
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    let Some(r) = f.capacity.get_mut(cover).filter(|r| r.state == CapacityState::Pending) else {
        return reject("not_pending", "the reservation has already been answered");
    };
    match answer {
        CapacityAnswer::Automatic => {
            r.state = CapacityState::Automatic;
            flow::capacity_progress(&st.config, p, life, cx)
        }
        // Above the treaty's automatic limit: no offer until the reinsurer's own terms are in.
        CapacityAnswer::FacultativeRequired => {
            r.state = CapacityState::FacultativeRequired;
            cx.event(Event::FacultativeReferral { proposal, life: life.clone() });
            flow::open_case(
                p,
                life,
                CaseType::ReinsurerReferral,
                Queue::ReinsurerLiaison,
                [reason::FACULTATIVE_REQUIRED.to_string()].into(),
                cx,
            )?;
            life_mut(p, life)?.stage = Stage::AwaitingFacultative;
            Ok(())
        }
        // The treaty will not take the risk: a person decides what, if anything, can be offered.
        CapacityAnswer::Excluded => {
            r.state = CapacityState::Excluded;
            flow::open_case(
                p,
                life,
                CaseType::Referral,
                Queue::SeniorReview,
                [reason::RISK_EXCLUDED_BY_TREATY.to_string()].into(),
                cx,
            )?;
            Ok(())
        }
    }
}

/// The reinsurer answered a facultative referral. Either way the liaison
/// then makes the decision: with the reinsurer's terms, or without their cover.
pub(crate) fn facultative(p: &mut Proposal, life: &LifeId, cover: &str, cx: &mut Cx, outcome: FacultativeOutcome) -> Result<(), Reject> {
    let f = life_mut(p, life)?;
    let Some(r) = f.capacity.get_mut(cover).filter(|r| r.state == CapacityState::FacultativeRequired) else {
        return reject("not_facultative", "the reservation is not waiting on the reinsurer");
    };
    let why = match outcome {
        FacultativeOutcome::Offered { reference } => {
            if reference.is_empty() {
                return reject("reference", "placed terms carry the reinsurer's reference");
            }
            r.state = CapacityState::Placed { reference: reference.clone() };
            if let Some(d) = f.decisions.last_mut() {
                d.facultative_ref = Some(reference);
            }
            reason::FACULTATIVE_TERMS
        }
        FacultativeOutcome::Declined => {
            r.state = CapacityState::FacultativeDeclined;
            reason::FACULTATIVE_DECLINED
        }
    };
    f.stage = Stage::InAssessment;
    match f.cases.iter_mut().find(|c| c.is_open() && c.case_type == CaseType::ReinsurerReferral) {
        Some(c) => {
            c.reasons.insert(why.to_string());
        }
        None => {
            flow::open_case(p, life, CaseType::ReinsurerReferral, Queue::ReinsurerLiaison, [why.to_string()].into(), cx)?;
        }
    }
    Ok(())
}

pub(crate) fn interim_issued(p: &mut Proposal) -> Result<(), Reject> {
    match p.interim.as_mut() {
        Some(i) => {
            i.issued = true;
            Ok(())
        }
        None => reject("no_interim", "the proposal has no interim cover"),
    }
}

/// The core issued the policy: confirm the capacity, tell the reinsurance
/// module the rating factors, and end the interim cover the policy replaces.
pub(crate) fn policy_issued(p: &mut Proposal, cx: &mut Cx, layers: &[crate::command::IssuedLayer]) -> Result<(), Reject> {
    if p.status != ProposalStatus::Issuing {
        return reject("not_issuing", format!("proposal {} is {}", p.id, p.status.key()));
    }
    p.status = ProposalStatus::Issued { at: cx.at };
    cx.event(Event::PolicyIssued {
        proposal: p.id.clone(),
        policy: p.policy.clone(),
    });
    if crate::reoffer::written(p) {
        return Ok(());
    }
    // Group administration is waiting to mark the leaver's take-up done.
    if let crate::model::Entry::GroupContinuation { policy, .. } = &p.entry {
        cx.effect(Effect::ProposalEnded {
            key: format!("{}/ended", p.id),
            proposal: p.id.clone(),
            entry: p.entry.clone(),
            policy: policy.clone(),
            outcome: "issued".into(),
        });
    }
    for f in p.lives.values().filter(|f| f.stage == Stage::Accepted) {
        for (cover, r) in &f.capacity {
            cx.effect(Effect::ConfirmCapacity {
                reservation: r.key.clone(),
                policy: p.policy.clone(),
                life: f.life.clone(),
                cover: cover.clone(),
                layers: layers.iter().filter(|l| l.life == f.life && &l.cover == cover).map(|l| l.layer).collect(),
            });
        }
        cx.effect(Effect::RecordRiskFactors {
            policy: p.policy.clone(),
            life: f.life.clone(),
            date_of_birth: f.factors.date_of_birth,
            sex: f.factors.sex,
            smoker: f.factors.smoker,
        });
    }
    record_replacements(p, cx);
    if let crate::model::Entry::Facility {
        facility: crate::model::Facility::BusinessContinuity,
        from_policy,
        from_group,
        ..
    } = &p.entry
    {
        cx.effect(Effect::EndConverted {
            key: format!("{}/end-converted", p.id),
            proposal: p.id.clone(),
            group: from_group.clone(),
            policy: from_policy.clone(),
        });
    }
    crate::advice::advise(p, cx);
    flow::end_interim(p, cx);
    cx.effect(Effect::Notify {
        to: Recipient::Party { party: p.policyholder.clone() },
        kind: NoticeKind::PolicyIssued,
        proposal: p.id.clone(),
        life: None,
        reasons: vec![],
    });
    Ok(())
}

/// The core refused the issue, for example because the product version the
/// quote was bound to is no longer the one it holds. Nothing is retried
/// blindly: a person, or a new quote the applicant accepts, moves it on.
pub(crate) fn issue_rejected(p: &mut Proposal, cx: &mut Cx, code: String) -> Result<(), Reject> {
    if p.status != ProposalStatus::Issuing {
        return reject("not_issuing", format!("proposal {} is {}", p.id, p.status.key()));
    }
    cx.event(Event::IssueFailed {
        proposal: p.id.clone(),
        code: code.clone(),
    });
    // The core's billing catching up is not a person's problem until the
    // sweep has tried again and given up.
    if code != BILLING_BEHIND {
        cx.effect(Effect::Notify {
            to: Recipient::UnderwritingManager,
            kind: NoticeKind::IssueFailed,
            proposal: p.id.clone(),
            life: None,
            reasons: vec![],
        });
    }
    p.status = ProposalStatus::IssueFailed { code };
    Ok(())
}

pub(crate) fn retry_issue(st: &UnderwritingState, p: &mut Proposal, cx: &mut Cx, underwriter: &str) -> Result<(), Reject> {
    st.profile(underwriter)?;
    if st.conflicted(underwriter, p) {
        return reject(
            "not_authorised",
            format!("{underwriter} has declared a relationship with someone on the application"),
        );
    }
    if !matches!(p.status, ProposalStatus::IssueFailed { .. }) {
        return reject("not_failed", format!("proposal {} is {}", p.id, p.status.key()));
    }
    flow::issue(p, cx)
}

#[derive(Default)]
struct Swept {
    lapsed: u32,
    chased: u32,
    escalated: u32,
}

/// The core refuses cover written for today until the day's billing has
/// run. The end of day catches billing up, so the next day's sweep tries
/// again.
const BILLING_BEHIND: &str = "billing_behind";

/// How many times in all the core is asked before a refusal for billing
/// that is behind is left to a person.
const MOST_ISSUE_ATTEMPTS: u32 = 5;

/// What the passing of time does to one proposal.
fn sweep_one(cfg: &Config, p: &mut Proposal, cx: &mut Cx, n: &mut Swept) -> Result<(), Reject> {
    if matches!(&p.status, ProposalStatus::IssueFailed { code } if code == BILLING_BEHIND) {
        if cx.today <= p.expires_on && p.issue_attempts < MOST_ISSUE_ATTEMPTS {
            return flow::issue(p, cx);
        }
        // Still refused, or out of time: now it is a person's.
        p.status = ProposalStatus::IssueFailed {
            code: "billing_behind_retries_exhausted".to_string(),
        };
        cx.effect(Effect::Notify {
            to: Recipient::UnderwritingManager,
            kind: NoticeKind::IssueFailed,
            proposal: p.id.clone(),
            life: None,
            reasons: vec![],
        });
        return Ok(());
    }
    if matches!(p.status, ProposalStatus::Issued { .. }) {
        crate::reoffer::expire(p, cx);
        return Ok(());
    }
    if p.status != ProposalStatus::Open {
        return Ok(());
    }
    let proposal = p.id.clone();
    if cx.today > p.expires_on {
        // Out of time: lapse, with notice, and nothing left running.
        p.status = ProposalStatus::Lapsed {
            reason: reason::PROPOSAL_EXPIRED.to_string(),
            at: cx.at,
        };
        cx.event(Event::ProposalLapsed { proposal: proposal.clone() });
        flow::end_proposal(p, cx);
        cx.effect(Effect::Notify {
            to: Recipient::Party { party: p.policyholder.clone() },
            kind: NoticeKind::ProposalLapsed,
            proposal,
            life: None,
            reasons: cfg
                .reasons_on(p.received_on)
                .map(|l| l.for_customer(&[reason::PROPOSAL_EXPIRED.to_string()]).0)
                .unwrap_or_default(),
        });
        n.lapsed += 1;
        return Ok(());
    }
    let lives: Vec<LifeId> = p.lives.keys().cloned().collect();
    for life in &lives {
        let f = life_mut(p, life)?;
        let mut gave_up = false;
        for e in f.evidence.iter_mut().filter(|e| e.outstanding()) {
            let chased_out = e.chasers >= cfg.settings.max_chasers;
            if chased_out && cx.books_now() >= e.deadline {
                // Chased and still nothing: a person decides what to do
                // without it. It is never treated as good or bad news.
                e.status = EvidenceStatus::Unobtainable;
                gave_up = true;
                cx.event(Event::EvidenceSettled {
                    proposal: proposal.clone(),
                    life: life.clone(),
                    request: e.id.clone(),
                    status: e.status,
                });
                cx.effect(Effect::CancelEvidence { request: e.id.clone() });
            } else if !chased_out && cx.at - e.last_chased_at.unwrap_or(e.requested_at) >= Duration::days(cfg.settings.chase_every_days) {
                e.chasers += 1;
                e.last_chased_at = Some(cx.at);
                n.chased += 1;
                cx.event(Event::EvidenceChased {
                    proposal: proposal.clone(),
                    life: life.clone(),
                    request: e.id.clone(),
                    chasers: e.chasers,
                });
                cx.effect(Effect::ChaseEvidence { request: e.id.clone() });
            }
        }
        for c in f.cases.iter_mut().filter(|c| c.is_open()) {
            let due = c.escalation_due(&cfg.settings.timers, cx.at);
            if due > c.escalation {
                c.escalation = due;
                n.escalated += 1;
                cx.event(Event::CaseEscalated { case: c.id.clone(), to: due });
                cx.effect(Effect::Notify {
                    to: if due == Escalation::Manager {
                        Recipient::UnderwritingManager
                    } else {
                        Recipient::QueueLead { queue: c.queue }
                    },
                    kind: NoticeKind::CaseEscalated,
                    proposal: proposal.clone(),
                    life: Some(life.clone()),
                    reasons: vec![],
                });
            }
        }
        let offer_expired = f.open_offer().is_some_and(|o| cx.today > o.expires_on);
        if gave_up {
            flow::evidence_settled(p, life, cx)?;
        }
        if offer_expired {
            flow::offer_not_taken(cfg, p, life, OfferStatus::Expired, cx)?;
        }
    }
    Ok(())
}

impl UnderwritingState {
    /// Work through open proposals in id order, a bounded number at a time,
    /// so one command never does unbounded work. The timer driver calls
    /// again with `next` until there is none.
    pub(crate) fn sweep(&mut self, cx: &mut Cx, after: Option<ProposalId>, max: u32) -> Result<(), Reject> {
        if max == 0 || max > 1000 {
            return reject("max", "a sweep takes between 1 and 1000 proposals");
        }
        let from = after.map_or(Bound::Unbounded, Bound::Excluded);
        let mut ids: Vec<ProposalId> = self.open.range((from, Bound::Unbounded)).take(max as usize + 1).cloned().collect();
        let more = ids.len() > max as usize;
        ids.truncate(max as usize);
        let mut n = Swept::default();
        let mut swept = Vec::new();
        for id in &ids {
            let Some(mut p) = self.proposals.get(id).cloned() else { continue };
            sweep_one(&self.config, &mut p, cx, &mut n)?;
            swept.push(p);
        }
        // Kept only once the whole batch has gone through.
        for p in swept {
            self.store(p);
        }
        cx.out.view = Some(View::Sweep {
            next: ids.last().filter(|_| more).cloned(),
            lapsed: n.lapsed,
            chased: n.chased,
            escalated: n.escalated,
        });
        Ok(())
    }

    /// An adviser's or a provider's licence changed. On suspension,
    /// revocation or lapse the adviser's sight of every application ends at
    /// once and each customer is told; the proposals themselves carry on.
    pub(crate) fn licence_changed(
        &mut self,
        cx: &mut Cx,
        subject: String,
        status: LicenceStatus,
        effective: NaiveDate,
        source_ref: &str,
    ) -> Result<(), Reject> {
        if effective > cx.today {
            return reject("not_yet_effective", "a licence change is recorded on the day it takes effect");
        }
        if subject.is_empty() || source_ref.is_empty() {
            return reject("source", "a licence change names its subject and the register entry it comes from");
        }
        cx.event(Event::LicenceRecorded {
            subject: subject.clone(),
            status,
            effective,
        });
        if status != LicenceStatus::Active && self.licences.get(&subject).is_none_or(|l| *l == LicenceStatus::Active) {
            let ids: Vec<ProposalId> = self.by_adviser.get(&subject).into_iter().flatten().cloned().collect();
            for id in ids {
                let Some(mut p) = self.proposals.get(&id).cloned() else { continue };
                let lives: Vec<LifeId> = p.lives.keys().cloned().collect();
                for life in &lives {
                    flow::end_adviser_access(&mut p, life, AccessEnd::LicenceSuspended, cx);
                }
                cx.effect(Effect::Notify {
                    to: Recipient::Party { party: p.policyholder.clone() },
                    kind: NoticeKind::AdviserUnavailable,
                    proposal: id,
                    life: None,
                    reasons: vec![],
                });
                self.store(p);
            }
        }
        self.licences.insert(subject, status);
        Ok(())
    }
}

/// A new policy issued in place of cover on another of ours: the core is
/// told, cover by cover, so reinsurance carries the old cession on. The
/// amount replaced is the lesser of the old cover and the new as accepted.
fn record_replacements(p: &Proposal, cx: &mut Cx) {
    for (n, r) in p.replaces.iter().enumerate() {
        let Some(f) = p.lives.get(&r.life).filter(|f| f.stage == Stage::Accepted) else {
            continue;
        };
        let accepted = f.effective_decision().and_then(|d| match d.terms_for(&r.cover) {
            Some(crate::model::CoverOutcome::Accept { revised_sum_insured_minor, .. }) => {
                p.cover(&r.life, &r.cover).map(|c| revised_sum_insured_minor.unwrap_or(c.sum_insured_minor))
            }
            _ => None,
        });
        let Some(accepted) = accepted else { continue };
        cx.effect(Effect::RecordReplacement {
            key: format!("{}/replaces/{}", p.id, n + 1),
            proposal: p.id.clone(),
            group: p.group.clone(),
            command: Box::new(ic_core::command::Replacement {
                policy: p.policy.clone(),
                replaced_policy: r.replaced_policy.clone(),
                life: r.life.clone(),
                replaced_life: r.replaced_life.clone(),
                cover: r.cover.clone(),
                replaced_cover: r.replaced_cover.clone(),
                replaced_sum_insured_minor: r.replaced_sum_insured_minor.min(accepted),
                evidence: format!("uw-consent:{}/{}/replacement_disclosure", p.id, r.life),
            }),
        });
    }
}
