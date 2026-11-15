//! The path every life follows once it has disclosed: routing by what the
//! rules said, evidence, the decision, reinsurance capacity, the offer, and
//! the issue command when every life on the proposal is settled.

use std::collections::BTreeSet;

use ic_core::command::{CoverSpec, IssuePolicy, LifeSpec};
use ic_core::{LifeId, LifeKind};
use ic_money::DecStr;

use crate::case::{Case, CaseType, Queue};
use crate::codes::{reason, AppliedExclusion};
use crate::command::{Effect, Event, NoticeKind, Recipient, Reject};
use crate::config::Config;
use crate::evidence::{EvidenceKind, EvidenceRequest, EvidenceStatus};
use crate::machine::{life_mut, life_ref, reject, Cx};
use crate::model::{
    AccessEnd, CapacityState, CoverOutcome, CoverTerms, Decision, DecisionSource, DecisionStatus, Entry, InterimCover, LifeFile, Offer, OfferStatus, Proposal,
    ProposalStatus, Reservation, Stage, Versions,
};
use crate::rules::Disposition;

fn set(codes: &[&str]) -> BTreeSet<String> {
    codes.iter().map(|c| c.to_string()).collect()
}

/// The versions a decision made now on this life can be reproduced from.
pub(crate) fn versions(cfg: &Config, p: &Proposal, f: &LifeFile) -> Versions {
    Versions {
        rule_set: f.rule_set.clone(),
        question_set: f.disclosures.last().map(|d| d.question_set.clone()),
        rate_tables: p.quote.rate_tables.clone(),
        exclusions: cfg.exclusions_ref(p.received_on),
        reasons: cfg.reasons_ref(p.received_on),
    }
}

/// Put the life in front of a person. One case is open on a life at a time:
/// if there is one it changes type and queue, otherwise one opens.
pub(crate) fn open_case(p: &mut Proposal, life: &LifeId, case_type: CaseType, queue: Queue, reasons: BTreeSet<String>, cx: &mut Cx) -> Result<String, Reject> {
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    f.stage = Stage::InAssessment;
    if let Some(case) = f.cases.iter_mut().find(|c| c.is_open() && c.case_type != CaseType::NonDisclosureReview) {
        if case.case_type != case_type || case.queue != queue {
            case.move_to(case_type, queue, reasons, cx.at);
            cx.event(Event::CaseMoved {
                case: case.id.clone(),
                case_type,
                queue,
            });
        } else {
            case.reasons.extend(reasons);
        }
        return Ok(case.id.clone());
    }
    let id = format!("{proposal}/{life}/case/{}", f.cases.len() + 1);
    f.cases.push(Case::open(id.clone(), case_type, queue, reasons, cx.at));
    cx.event(Event::Referred {
        proposal,
        life: life.clone(),
        case: id.clone(),
        case_type,
        queue,
    });
    Ok(id)
}

/// Close the underwriting case on the life, if one is open.
pub(crate) fn close_cases(f: &mut LifeFile, cx: &mut Cx) {
    for case in f.cases.iter_mut().filter(|c| c.is_open() && c.case_type != CaseType::NonDisclosureReview) {
        case.close(cx.at);
        cx.event(Event::CaseClosed { case: case.id.clone() });
    }
}

/// Ask for evidence. A kind already asked for and still outstanding is not
/// asked for again; the request id is stable so a retried effect never asks
/// a doctor twice.
pub(crate) fn request_evidence(cfg: &Config, p: &mut Proposal, life: &LifeId, kinds: &BTreeSet<EvidenceKind>, cx: &mut Cx) -> Result<(), Reject> {
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    for kind in kinds {
        if f.evidence.iter().any(|e| e.kind == *kind && e.outstanding()) {
            continue;
        }
        let earlier = f.evidence.iter().filter(|e| e.kind == *kind).count();
        let id = match earlier {
            0 => format!("{proposal}/{life}/{}", kind.key()),
            n => format!("{proposal}/{life}/{}/{}", kind.key(), n + 1),
        };
        let extra = if f.more_time { cfg.settings.more_time_days } else { 0 };
        let deadline = cx.at + chrono::Duration::days(cfg.settings.evidence_days + extra);
        f.evidence.push(EvidenceRequest {
            id: id.clone(),
            kind: *kind,
            status: EvidenceStatus::Requested,
            requested_at: cx.at,
            deadline,
            chasers: 0,
            last_chased_at: None,
            provider_ref: None,
            vault_ref: None,
            cost_minor: 0,
        });
        cx.event(Event::EvidenceRequested {
            proposal: proposal.clone(),
            life: life.clone(),
            request: id.clone(),
            kind: *kind,
            deadline,
        });
        cx.effect(Effect::RequestEvidence {
            request: id,
            kind: *kind,
            party: f.party.clone(),
        });
    }
    Ok(())
}

/// Evidence has arrived or been given up on. Evidence never decides
/// anything by itself: once nothing is outstanding a person reads it.
pub(crate) fn evidence_settled(p: &mut Proposal, life: &LifeId, cx: &mut Cx) -> Result<(), Reject> {
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    if f.evidence.iter().any(|e| e.outstanding()) {
        return Ok(());
    }
    if let Some(case) = f.open_case() {
        if let Some(owner) = &case.owner {
            cx.effect(Effect::Notify {
                to: Recipient::Underwriter { underwriter: owner.clone() },
                kind: NoticeKind::EvidenceArrived,
                proposal,
                life: Some(life.clone()),
                reasons: vec![],
            });
        }
        return Ok(());
    }
    if f.stage != Stage::AwaitingEvidence {
        return Ok(());
    }
    let mut reasons: BTreeSet<String> = f.assessments.values().flat_map(|a| a.reasons.iter().cloned()).collect();
    let missing = f.evidence.iter().any(|e| e.status == EvidenceStatus::Unobtainable);
    reasons.insert(if missing { reason::EVIDENCE_NOT_OBTAINED } else { reason::EVIDENCE_COMPLETE }.to_string());
    let financial = f.evidence.iter().all(|e| e.kind == EvidenceKind::FinancialStatement);
    let (case_type, queue) = if financial {
        (CaseType::FinancialEvidence, Queue::Financial)
    } else {
        (CaseType::MedicalEvidence, Queue::Medical)
    };
    open_case(p, life, case_type, queue, reasons, cx)?;
    Ok(())
}

/// Act on what the rules said about a life. `interviewed` is set once the
/// life has confirmed an interview, so the rules do not ask for another.
pub(crate) fn route(cfg: &Config, p: &mut Proposal, life: &LifeId, cx: &mut Cx, interviewed: bool) -> Result<(), Reject> {
    let f = life_ref(p, life)?;
    let assessments: Vec<_> = f
        .assessments
        .values()
        .cloned()
        .map(|mut a| {
            a.tele_interview &= !interviewed;
            a
        })
        .collect();
    let worst = assessments.iter().map(|a| a.disposition()).max().unwrap_or(Disposition::Clean);
    let mut reasons: BTreeSet<String> = assessments.iter().flat_map(|a| a.reasons.iter().cloned()).collect();
    let queue = assessments.iter().flat_map(|a| a.queues.iter().copied()).min();
    let has_case = f.open_case().is_some();
    match worst {
        // The rules never decline alone: a person proposes and a senior signs.
        Disposition::Decline | Disposition::Defer => {
            open_case(p, life, CaseType::DeclineReview, queue.unwrap_or(Queue::Medical), reasons, cx)?;
        }
        Disposition::Refer => {
            open_case(p, life, CaseType::Referral, queue.unwrap_or(Queue::Medical), reasons, cx)?;
        }
        Disposition::TeleInterview => {
            open_case(p, life, CaseType::TeleInterview, Queue::TeleInterview, reasons, cx)?;
        }
        Disposition::NeedsEvidence => {
            if !f.consented(crate::model::ConsentKind::ObtainRecords) {
                return reject(
                    "consent_required",
                    "the rules call for evidence, and the life has not consented to records being obtained",
                );
            }
            let kinds: BTreeSet<EvidenceKind> = assessments.iter().flat_map(|a| a.evidence.iter().copied()).collect();
            request_evidence(cfg, p, life, &kinds, cx)?;
            if has_case {
                let queue = kinds.iter().map(|k| k.queue()).min().unwrap_or(Queue::Medical);
                let case_type = if queue == Queue::Financial {
                    CaseType::FinancialEvidence
                } else {
                    CaseType::MedicalEvidence
                };
                open_case(p, life, case_type, queue, reasons, cx)?;
            } else {
                life_mut(p, life)?.stage = Stage::AwaitingEvidence;
            }
        }
        Disposition::Terms | Disposition::Clean => {
            if p.largest_sum_on(life) > cfg.settings.straight_through_limit_minor {
                reasons.insert("RC-0203".to_string());
                open_case(p, life, CaseType::Referral, Queue::SeniorReview, reasons, cx)?;
                return Ok(());
            }
            let f = life_ref(p, life)?;
            let terms = p
                .covers_on(life)
                .map(|c| {
                    let a = f.assessments.get(&c.code).cloned().unwrap_or_default();
                    CoverTerms {
                        cover: c.code.clone(),
                        outcome: CoverOutcome::Accept {
                            loading_percent: a.loading_percent,
                            exclusions: a
                                .exclusions
                                .into_iter()
                                .map(|code| AppliedExclusion {
                                    code,
                                    from: cx.today,
                                    until: None,
                                })
                                .collect(),
                            revised_sum_insured_minor: None,
                        },
                    }
                })
                .collect();
            if reasons.is_empty() {
                reasons = set(&[reason::RULES_CLEAN]);
            }
            let decision = new_decision(cfg, p, f, DecisionSource::Rules, terms, reasons, cx);
            life_mut(p, life)?.decisions.push(decision);
            after_signed(cfg, p, life, cx)?;
        }
    }
    Ok(())
}

/// A scheme member's excess that a previous insurer had already accepted is
/// honoured under the scheme's takeover terms: no larger than the amount
/// accepted, it is taken on standard terms with no disclosure. Larger, it is
/// underwritten in full. When reinsurance asked for the cover to be
/// underwritten because of the member's other cover, it always is.
pub(crate) fn takeover(cfg: &Config, p: &mut Proposal, cx: &mut Cx) -> Result<(), Reject> {
    let Entry::GroupExcess {
        takeover_accepted_minor: Some(honoured),
        reinsurance_reference: None,
        ..
    } = p.entry
    else {
        return Ok(());
    };
    let lives: Vec<LifeId> = p.lives.keys().cloned().collect();
    for life in &lives {
        if p.covers_on(life).any(|c| c.sum_insured_minor > honoured) {
            continue;
        }
        let terms = p
            .covers_on(life)
            .map(|c| CoverTerms {
                cover: c.code.clone(),
                outcome: CoverOutcome::Accept {
                    loading_percent: 0,
                    exclusions: vec![],
                    revised_sum_insured_minor: None,
                },
            })
            .collect();
        let decision = new_decision(cfg, p, life_ref(p, life)?, DecisionSource::Rules, terms, set(&[reason::TAKEOVER]), cx);
        life_mut(p, life)?.decisions.push(decision);
        after_signed(cfg, p, life, cx)?;
    }
    Ok(())
}

/// A decision with the next reference on the life, not yet in force.
pub(crate) fn new_decision(
    cfg: &Config,
    p: &Proposal,
    f: &LifeFile,
    source: DecisionSource,
    terms: Vec<CoverTerms>,
    reasons: BTreeSet<String>,
    cx: &Cx,
) -> Decision {
    Decision {
        reference: format!("{}/{}/d{}", p.id, f.life, f.decisions.len() + 1),
        source,
        status: DecisionStatus::AwaitingCapacity,
        terms,
        versions: versions(cfg, p, f),
        reasons,
        signatures: vec![],
        evidence: BTreeSet::new(),
        notes_ref: None,
        facultative_ref: None,
        made_at: cx.at,
        effective_at: None,
        peer_review: None,
        sampled_for: None,
        non_disclosure: None,
    }
}

fn release(f: &mut LifeFile, cover: &str, why: &str, cx: &mut Cx) {
    if let Some(r) = f.capacity.remove(cover) {
        cx.effect(Effect::ReleaseCapacity {
            reservation: r.key,
            reason: why.to_string(),
        });
    }
}

fn release_all(f: &mut LifeFile, why: &str, cx: &mut Cx) {
    let covers: Vec<String> = f.capacity.keys().cloned().collect();
    for cover in covers {
        release(f, &cover, why, cx);
    }
}

/// The newest decision on the life has every signature it needs. Reserve
/// reinsurance capacity for what it accepts; when capacity is in hand the
/// decision takes effect.
pub(crate) fn after_signed(cfg: &Config, p: &mut Proposal, life: &LifeId, cx: &mut Cx) -> Result<(), Reject> {
    let proposal = p.id.clone();
    let (product, expires, commencement) = (p.product_code.clone(), p.expires_on, cx.today);
    let covers: Vec<_> = p.covers_on(life).cloned().collect();
    let alteration = matches!(p.entry, Entry::Alteration { .. } | Entry::Reinstatement { .. });
    let excess_reference = match &p.entry {
        Entry::GroupExcess { reinsurance_reference, .. } => reinsurance_reference.clone(),
        _ => None,
    };
    let library = cfg.exclusions_on(p.received_on);
    let f = life_mut(p, life)?;
    close_cases(f, cx);
    let Some(decision) = f.decisions.last().cloned() else {
        return reject("no_decision", "there is no decision to act on");
    };
    // Cover that is already in force with a legacy system was ceded there.
    // Cover whose terms alone are changing, or that is coming back from a
    // lapse inside the reinstatement window, is ceded here already: its sum
    // insured does not change, and reserving it again would count the
    // life's cover twice against the retention.
    let ceded = matches!(decision.source, DecisionSource::Migrated { .. }) || alteration;
    // What reinsurance is told of the risk, never anything clinical: what
    // the answers declared, and the pursuits, occupation and travel the
    // decision excluded for.
    let declared: BTreeSet<String> = f.disclosures.last().map(|d| d.risk_tags.clone()).unwrap_or_default();
    let risk_codes = |cover: &str| -> Vec<String> {
        let mut codes = declared.clone();
        if let Some(CoverOutcome::Accept { exclusions, .. }) = decision.terms_for(cover) {
            for e in exclusions {
                let tags = library
                    .and_then(|l| l.entries.get(&e.code))
                    .map(|x| x.cause_tags.as_slice())
                    .unwrap_or_default();
                codes.extend(tags.iter().filter(|t| crate::questions::is_risk_tag(t)).cloned());
            }
        }
        codes.into_iter().collect()
    };
    for c in &covers {
        let accepted = decision.accepted_sum(&c.code, c.sum_insured_minor).filter(|_| !ceded);
        if accepted.is_some_and(|a| f.capacity.get(&c.code).is_some_and(|r| r.amount_minor == a)) {
            continue;
        }
        release(f, &c.code, "superseded", cx);
        if let Some(amount) = accepted {
            f.reservations_made += 1;
            let key = format!("{proposal}/{life}/{}/{}", c.code, f.reservations_made);
            f.capacity.insert(
                c.code.clone(),
                Reservation {
                    key: key.clone(),
                    amount_minor: amount,
                    state: CapacityState::Pending,
                },
            );
            cx.event(Event::CapacityRequested {
                proposal: proposal.clone(),
                life: life.clone(),
                reservation: key.clone(),
            });
            cx.effect(Effect::ReserveCapacity {
                reservation: key,
                party: f.party.clone(),
                product_code: product.clone(),
                cover_code: c.code.clone(),
                cover_type: c.cover_type.clone(),
                sum_insured_minor: amount,
                commencement,
                proposal_expires: expires,
                risk_codes: risk_codes(&c.code),
                excess_reference: excess_reference.clone(),
            });
        }
    }
    capacity_progress(cfg, p, life, cx)
}

/// Whether capacity is in hand for the decision waiting on it.
pub(crate) fn capacity_progress(cfg: &Config, p: &mut Proposal, life: &LifeId, cx: &mut Cx) -> Result<(), Reject> {
    let f = life_mut(p, life)?;
    if f.capacity
        .values()
        .all(|r| matches!(r.state, CapacityState::Automatic | CapacityState::Placed { .. }))
    {
        return make_effective(cfg, p, life, cx);
    }
    // A case is open when the reinsurer or a person is needed: the stage
    // stays with the case.
    if f.open_case().is_none() {
        f.stage = Stage::AwaitingCapacity;
    }
    if let Some(d) = f.decisions.last_mut() {
        d.status = DecisionStatus::AwaitingCapacity;
    }
    Ok(())
}

pub(crate) fn end_adviser_access(p: &mut Proposal, life: &LifeId, why: AccessEnd, cx: &mut Cx) {
    let (proposal, adviser) = (p.id.clone(), p.adviser.clone());
    let (Some(adviser), Some(f)) = (adviser, p.lives.get_mut(life)) else { return };
    if f.adviser_access_ended.is_none() && f.consented(crate::model::ConsentKind::AdviserAccess) {
        f.adviser_access_ended = Some(why);
        cx.event(Event::AdviserAccessEnded {
            proposal,
            life: life.clone(),
            adviser: adviser.adviser,
            why,
        });
    }
}

/// The reasons the person may be told, in plain words.
fn told(cfg: &Config, p: &Proposal, d: &Decision) -> Vec<String> {
    cfg.reasons_on(p.received_on).map(|l| l.for_customer(&d.all_reasons()).0).unwrap_or_default()
}

fn offer(p: &mut Proposal, life: &LifeId, cfg: &Config, cx: &mut Cx) -> Result<(), Reject> {
    let (proposal, expires) = (p.id.clone(), p.expires_on);
    let f = life_ref(p, life)?;
    let Some(d) = f.effective_decision() else {
        return reject("no_decision", "there is no decision in force to offer");
    };
    let reasons = told(cfg, p, d);
    let offer = Offer {
        id: format!("{}/offer/{}", d.reference, f.offers.len() + 1),
        decision: d.reference.clone(),
        status: OfferStatus::Open,
        made_at: cx.at,
        expires_on: (cx.today + chrono::Duration::days(cfg.settings.offer_days + if f.more_time { cfg.settings.more_time_days } else { 0 })).min(expires),
        responded_at: None,
    };
    cx.event(Event::OfferMade {
        proposal: proposal.clone(),
        life: life.clone(),
        offer: offer.id.clone(),
        expires_on: offer.expires_on,
    });
    cx.effect(Effect::Notify {
        to: Recipient::Party { party: f.party.clone() },
        kind: NoticeKind::OfferMade,
        proposal,
        life: Some(life.clone()),
        reasons,
    });
    let f = life_mut(p, life)?;
    f.offers.push(offer);
    f.stage = Stage::OfferOpen;
    f.reconfirm = false;
    Ok(())
}

/// Capacity is in hand: the decision takes effect. Standard terms are
/// accepted as applied for; anything else is offered back to the applicant.
pub(crate) fn make_effective(cfg: &Config, p: &mut Proposal, life: &LifeId, cx: &mut Cx) -> Result<(), Reject> {
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    let Some(d) = f.decisions.last_mut() else {
        return reject("no_decision", "there is no decision to bring into force");
    };
    d.status = DecisionStatus::Effective;
    d.effective_at = Some(cx.at);
    let d = d.clone();
    cx.event(Event::DecisionMade {
        proposal: proposal.clone(),
        life: life.clone(),
        decision: d.reference.clone(),
    });
    // The adviser's sight of the life's answers ends when the decision is made.
    end_adviser_access(p, life, AccessEnd::DecisionMade, cx);
    if !d.any_accept() {
        let reasons = told(cfg, p, &d);
        let f = life_mut(p, life)?;
        f.stage = Stage::Declined;
        release_all(f, "declined", cx);
        cx.effect(Effect::Notify {
            to: Recipient::Party { party: f.party.clone() },
            kind: NoticeKind::Declined,
            proposal,
            life: Some(life.clone()),
            reasons,
        });
        dropped_out(p, life);
    } else if d.all_standard()
        || matches!(
            d.source,
            DecisionSource::Migrated { .. } | DecisionSource::Continuation { .. } | DecisionSource::InsurabilityOption { .. } | DecisionSource::Facility { .. }
        )
    {
        // Terms the person already accepted, under the old system or the
        // scheme, are not offered to them again.
        life_mut(p, life)?.stage = Stage::Accepted;
    } else {
        offer(p, life, cfg, cx)?;
        // Cover declined or deferred beside cover offered: the person is
        // told of the decline in its own notice, with the reasons they may be
        // told and how to ask for review, as for a decline of everything.
        if d.terms.iter().any(|t| matches!(t.outcome, CoverOutcome::Decline | CoverOutcome::Defer { .. })) {
            let reasons = told(cfg, p, &d);
            let party = life_mut(p, life)?.party.clone();
            cx.effect(Effect::Notify {
                to: Recipient::Party { party },
                kind: NoticeKind::Declined,
                proposal,
                life: Some(life.clone()),
                reasons,
            });
        }
    }
    settle(cfg, p, cx)
}

/// A life is settled without cover. Every other life applied for a
/// contract that included it, so each is asked again before issue.
fn dropped_out(p: &mut Proposal, life: &LifeId) {
    for f in p
        .lives
        .values_mut()
        .filter(|f| &f.life != life && !matches!(f.stage, Stage::Declined | Stage::OfferDeclined))
    {
        f.reconfirm = true;
    }
}

/// When every life is settled, the proposal ends: as one command to the
/// core for what was accepted, or as a decline.
pub(crate) fn settle(cfg: &Config, p: &mut Proposal, cx: &mut Cx) -> Result<(), Reject> {
    if p.status != ProposalStatus::Open || !p.lives.values().all(|f| f.stage.is_final()) {
        return Ok(());
    }
    let accepted: Vec<LifeId> = p.lives.values().filter(|f| f.stage == Stage::Accepted).map(|f| f.life.clone()).collect();
    if accepted.is_empty() {
        if p.lives.values().any(|f| f.stage == Stage::Declined) {
            p.status = ProposalStatus::Declined { at: cx.at };
            cx.event(Event::ProposalDeclined { proposal: p.id.clone() });
        } else {
            p.status = ProposalStatus::Lapsed {
                reason: reason::OFFER_DECLINED.to_string(),
                at: cx.at,
            };
            cx.event(Event::ProposalLapsed { proposal: p.id.clone() });
        }
        end_proposal(p, cx);
        return Ok(());
    }
    // A life dropped out: the contract is no longer the one applied for,
    // so each life that was accepted is asked whether they still want it.
    let mut asked = false;
    for life in &accepted {
        if life_ref(p, life)?.reconfirm {
            offer(p, life, cfg, cx)?;
            asked = true;
        }
    }
    if asked {
        return Ok(());
    }
    issue(p, cx)
}

/// Loading percent as the fraction the core rates with.
pub(crate) fn loading(percent: u32) -> Result<DecStr, Reject> {
    DecStr::parse(&format!("{}.{:02}", percent / 100, percent % 100)).map_err(|e| Reject::new("loading", e.to_string()))
}

/// Send what was accepted to the core.
pub(crate) fn issue(p: &mut Proposal, cx: &mut Cx) -> Result<(), Reject> {
    let mut lives = Vec::new();
    let mut covers = Vec::new();
    for f in p.lives.values().filter(|f| f.stage == Stage::Accepted) {
        let Some(d) = f.effective_decision() else { continue };
        lives.push(LifeSpec {
            id: f.life.clone(),
            kind: LifeKind::Named { party: f.party.clone() },
            factors: f.factors.clone(),
        });
        for c in p.covers_on(&f.life) {
            if let Some(CoverOutcome::Accept {
                loading_percent,
                exclusions,
                revised_sum_insured_minor,
            }) = d.terms_for(&c.code)
            {
                covers.push(CoverSpec {
                    excess_option: c.option.clone(),
                    code: c.code.clone(),
                    life: Some(f.life.clone()),
                    sum_insured_minor: revised_sum_insured_minor.unwrap_or(c.sum_insured_minor),
                    loadings: if *loading_percent > 0 { vec![loading(*loading_percent)?] } else { vec![] },
                    exclusions: exclusions.iter().map(|e| e.code.clone()).collect(),
                });
            }
        }
    }
    let employer_may_see_loading = matches!(p.entry, Entry::GroupExcess { .. })
        && !lives.is_empty()
        && p.lives
            .values()
            .filter(|f| f.stage == Stage::Accepted)
            .all(|f| f.consents.contains_key(&crate::model::ConsentKind::EmployerSeesLoading));
    p.issue_attempts += 1;
    p.status = ProposalStatus::Issuing;
    let key = format!("{}/issue/{}", p.id, p.issue_attempts);
    cx.event(Event::IssueRequested {
        proposal: p.id.clone(),
        policy: p.policy.clone(),
    });
    let facility = matches!(p.entry, Entry::Facility { .. }).then(|| Box::new(p.entry.clone()));
    cx.effect(match &p.entry {
        Entry::NewBusiness | Entry::Facility { .. } => Effect::IssuePolicy {
            facility,
            key,
            proposal: p.id.clone(),
            expected_product_hash: p.quote.product_hash.clone(),
            migrated: p.migrated,
            command: Box::new(IssuePolicy {
                continuation_of: None,
                policy: p.policy.clone(),
                number: p.policy_number.clone(),
                group: p.group.clone(),
                policyholder: p.policyholder.clone(),
                product_code: p.product_code.clone(),
                inception: cx.today,
                frequency: p.frequency,
                lives,
                covers,
                // A proposal carried over mid-flight still becomes a policy written here, today.
                migrated: false,
            }),
        },
        entry => Effect::ApplyTerms {
            key,
            proposal: p.id.clone(),
            entry: entry.clone(),
            policy: p.policy.clone(),
            effective: cx.today,
            covers,
            employer_may_see_loading,
            member_pays_loading: p.member_pays_loading(),
        },
    });
    Ok(())
}

/// Interim accidental death cover, asked for the moment the proposal opens.
pub(crate) fn interim(cfg: &Config, p: &mut Proposal, cx: &mut Cx) {
    // A product with its own interim cover uses it; others use the general one.
    let Some(settings) = cfg.settings.interim_by_product.get(&p.product_code).or(cfg.settings.interim.as_ref()) else {
        return;
    };
    let policy = ic_core::PolicyId::from(format!("{}-interim", p.policy).as_str());
    let lives = p.lives.values();
    let command = IssuePolicy {
        continuation_of: None,
        policy: policy.clone(),
        number: format!("{}-I", p.policy_number),
        group: p.group.clone(),
        policyholder: p.policyholder.clone(),
        product_code: settings.product_code.clone(),
        inception: cx.today,
        frequency: p.frequency,
        lives: lives
            .clone()
            .map(|f| LifeSpec {
                id: f.life.clone(),
                kind: LifeKind::Named { party: f.party.clone() },
                factors: f.factors.clone(),
            })
            .collect(),
        covers: lives
            .map(|f| CoverSpec {
                excess_option: None,
                code: settings.cover_code.clone(),
                life: Some(f.life.clone()),
                sum_insured_minor: p.largest_sum_on(&f.life).min(settings.max_sum_insured_minor),
                loadings: vec![],
                exclusions: vec![],
            })
            .collect(),
        migrated: false,
    };
    cx.event(Event::InterimCoverRequested {
        proposal: p.id.clone(),
        policy: policy.clone(),
    });
    cx.effect(Effect::IssueInterim {
        key: format!("{}/interim", p.id),
        proposal: p.id.clone(),
        command: Box::new(command),
    });
    p.interim = Some(InterimCover {
        policy,
        issued: false,
        ended: false,
    });
}

pub(crate) fn end_interim(p: &mut Proposal, cx: &mut Cx) {
    let proposal = p.id.clone();
    if let Some(i) = p.interim.as_mut().filter(|i| !i.ended) {
        i.ended = true;
        cx.event(Event::InterimCoverEnded {
            proposal: proposal.clone(),
            policy: i.policy.clone(),
        });
        cx.effect(Effect::EndInterim {
            key: format!("{proposal}/interim/end"),
            proposal,
            policy: i.policy.clone(),
        });
    }
}

/// The proposal has ended without a policy: nothing is left running.
pub(crate) fn end_proposal(p: &mut Proposal, cx: &mut Cx) {
    let proposal = p.id.clone();
    let lives: Vec<LifeId> = p.lives.keys().cloned().collect();
    for life in &lives {
        end_adviser_access(p, life, AccessEnd::ProposalEnded, cx);
        let Some(f) = p.lives.get_mut(life) else { continue };
        close_cases(f, cx);
        for e in f.evidence.iter_mut().filter(|e| e.outstanding()) {
            e.status = EvidenceStatus::Cancelled;
            cx.event(Event::EvidenceSettled {
                proposal: proposal.clone(),
                life: life.clone(),
                request: e.id.clone(),
                status: EvidenceStatus::Cancelled,
            });
            cx.effect(Effect::CancelEvidence { request: e.id.clone() });
        }
        release_all(f, "proposal_ended", cx);
        for o in f.offers.iter_mut().filter(|o| o.status == OfferStatus::Open) {
            o.status = OfferStatus::Withdrawn;
        }
        for d in f
            .decisions
            .iter_mut()
            .filter(|d| matches!(d.status, DecisionStatus::AwaitingCountersign | DecisionStatus::AwaitingCapacity))
        {
            d.status = DecisionStatus::Superseded;
        }
    }
    end_interim(p, cx);
    if let Some(policy) = p.entry.policy() {
        cx.effect(Effect::ProposalEnded {
            key: format!("{proposal}/ended"),
            proposal,
            entry: p.entry.clone(),
            policy: policy.clone(),
            outcome: p.status.key().to_string(),
        });
    }
}

/// An offer was declined or ran out: the life is settled without cover.
pub(crate) fn offer_not_taken(cfg: &Config, p: &mut Proposal, life: &LifeId, status: OfferStatus, cx: &mut Cx) -> Result<(), Reject> {
    let f = life_mut(p, life)?;
    for o in f.offers.iter_mut().filter(|o| o.status == OfferStatus::Open) {
        o.status = status;
        o.responded_at = Some(cx.at);
    }
    f.stage = Stage::OfferDeclined;
    release_all(f, "offer_not_taken", cx);
    dropped_out(p, life);
    settle(cfg, p, cx)
}
