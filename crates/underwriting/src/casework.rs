//! Case work: what a named underwriter does. Every action is checked
//! against the person's authority profile by the authorisation policy, and
//! against the case they hold.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ic_core::LifeId;

use crate::authority::{CaseAction, Subject};
use crate::case::{Case, CaseType, Opinion, Queue};
use crate::codes::reason;
use crate::command::{Effect, Event, NoticeKind, Recipient, Reject, SubmitDisclosure};
use crate::evidence::{EvidenceKind, EvidenceStatus};
use crate::flow;
use crate::machine::{life_mut, life_ref, reject, Cx, UnderwritingState};
use crate::model::{
    CapacityState, ConsentKind, CoverOutcome, CoverTerms, DecisionSource, DecisionStatus, Interview, Proposal, ProposalStatus, Signature, Stage,
};

/// The case, which the underwriter must hold.
fn held<'a>(p: &'a mut Proposal, life: &LifeId, case: &str, underwriter: &str) -> Result<&'a mut Case, Reject> {
    let c = life_mut(p, life)?
        .cases
        .iter_mut()
        .find(|c| c.id == case)
        .ok_or_else(|| Reject::new("unknown_case", format!("no case {case}")))?;
    if !c.is_open() {
        return reject("case_closed", format!("case {case} is closed"));
    }
    if c.owner.as_deref() != Some(underwriter) {
        return reject("not_owner", format!("{underwriter} does not hold case {case}"));
    }
    Ok(c)
}

/// The case itself as the subject of an action that decides nothing.
fn whole_case<'a>(p: &'a Proposal, life: &LifeId) -> Subject<'a> {
    let largest = p.covers.iter().filter(|c| &c.life == life).max_by_key(|c| c.sum_insured_minor);
    Subject {
        product: &p.product_code,
        cover_type: largest.map_or("", |c| c.cover_type.as_str()),
        sum_insured_minor: largest.map_or(0, |c| c.sum_insured_minor),
        loading_percent: 0,
        has_exclusions: false,
        outcome: "none",
    }
}

fn known_reasons(st: &UnderwritingState, p: &Proposal, reasons: &BTreeSet<String>, withheld: &BTreeSet<String>) -> Result<(), Reject> {
    let Some(library) = st.config.reasons_on(p.received_on) else {
        return reject("no_rules", "the reason list is not active");
    };
    if reasons.is_empty() {
        return reject("reasons", "a decision records its reasons");
    }
    match library.first_unknown(reasons.iter().chain(withheld)) {
        Some(code) => reject("unknown_code", format!("reason {code} is not in the list")),
        None => Ok(()),
    }
}

/// Whether this person may be given this case.
fn may_hold(st: &UnderwritingState, p: &Proposal, life: &LifeId, case: &Case, underwriter: &str) -> Result<(), Reject> {
    let profile = st.profile(underwriter)?;
    if !profile.queues.contains(&case.queue) {
        return reject("wrong_queue", format!("{underwriter} does not work the {} queue", case.queue.key()));
    }
    let action = if case.case_type == CaseType::NonDisclosureReview {
        CaseAction::ReviewNonDisclosure
    } else {
        CaseAction::Work
    };
    st.require(p, life, underwriter, action, &whole_case(p, life), false)?;
    // The second signature is never the first signer's.
    let f = life_ref(p, life)?;
    if case.case_type == CaseType::Countersign && f.decisions.last().is_some_and(|d| d.signers().any(|s| s == underwriter)) {
        return reject("already_signed", "the second signature must be another person's");
    }
    if st.workload(underwriter) >= st.config.settings.timers.work_in_progress_limit {
        return reject("work_in_progress", format!("{underwriter} already holds as many cases as one person may"));
    }
    Ok(())
}

pub(crate) fn take(st: &UnderwritingState, p: &mut Proposal, life: &LifeId, cx: &mut Cx, case: &str, underwriter: &str) -> Result<(), Reject> {
    let Some(c) = life_ref(p, life)?.cases.iter().find(|c| c.id == case && c.is_open()) else {
        return reject("case_closed", format!("case {case} is closed"));
    };
    if c.owner.is_some() {
        return reject("owned", format!("case {case} already has an owner"));
    }
    may_hold(st, p, life, c, underwriter)?;
    held_or(p, life, case)?.assign(underwriter, cx.at);
    cx.event(Event::CaseAssigned {
        case: case.to_string(),
        owner: underwriter.to_string(),
        from: None,
    });
    Ok(())
}

fn held_or<'a>(p: &'a mut Proposal, life: &LifeId, case: &str) -> Result<&'a mut Case, Reject> {
    life_mut(p, life)?
        .cases
        .iter_mut()
        .find(|c| c.id == case)
        .ok_or_else(|| Reject::new("unknown_case", format!("no case {case}")))
}

/// Hand a case to someone else: by its owner, or by a queue lead or manager.
pub(crate) fn hand_over(st: &UnderwritingState, p: &mut Proposal, life: &LifeId, cx: &mut Cx, case: &str, by: &str, to: &str) -> Result<(), Reject> {
    let Some(c) = life_ref(p, life)?.cases.iter().find(|c| c.id == case && c.is_open()) else {
        return reject("case_closed", format!("case {case} is closed"));
    };
    if c.owner.as_deref() != Some(by) {
        st.require(p, life, by, CaseAction::Reassign, &whole_case(p, life), false)?;
    }
    if c.owner.as_deref() == Some(to) {
        return reject("owned", format!("{to} already holds case {case}"));
    }
    may_hold(st, p, life, c, to)?;
    let from = c.owner.clone();
    held_or(p, life, case)?.assign(to, cx.at);
    cx.event(Event::CaseAssigned {
        case: case.to_string(),
        owner: to.to_string(),
        from,
    });
    Ok(())
}

pub(crate) fn move_case(
    st: &UnderwritingState,
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    case: &str,
    underwriter: &str,
    case_type: CaseType,
    queue: Queue,
    reasons: BTreeSet<String>,
) -> Result<(), Reject> {
    known_reasons(st, p, &reasons, &BTreeSet::new())?;
    let c = held(p, life, case, underwriter)?;
    // A second signature and a claim-time review are reached only by their own steps.
    let special = |t: CaseType| matches!(t, CaseType::Countersign | CaseType::NonDisclosureReview);
    if special(c.case_type) || special(case_type) {
        return reject("not_allowed", "this kind of case is not moved by hand");
    }
    c.move_to(case_type, queue, reasons, cx.at);
    cx.event(Event::CaseMoved {
        case: case.to_string(),
        case_type,
        queue,
    });
    Ok(())
}

pub(crate) fn request_evidence(
    st: &UnderwritingState,
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    case: &str,
    underwriter: &str,
    kinds: BTreeSet<EvidenceKind>,
) -> Result<(), Reject> {
    held(p, life, case, underwriter)?;
    if p.status != ProposalStatus::Open {
        return reject("closed", "the proposal is no longer open");
    }
    if kinds.is_empty() || !life_ref(p, life)?.consented(ConsentKind::ObtainRecords) {
        return reject(
            "consent_required",
            "evidence is asked for by kind, and only with the life's consent to records being obtained",
        );
    }
    flow::request_evidence(&st.config, p, life, &kinds, cx)
}

pub(crate) fn ask_opinion(st: &UnderwritingState, p: &mut Proposal, life: &LifeId, cx: &mut Cx, case: &str, underwriter: &str, of: &str) -> Result<(), Reject> {
    st.profile(of)?;
    if st.conflicted(of, p) {
        return reject("not_authorised", format!("{of} has declared a relationship with someone on the application"));
    }
    let c = held(p, life, case, underwriter)?;
    c.opinions.push(Opinion {
        asked_of: of.to_string(),
        asked_at: cx.at,
        vault_ref: None,
    });
    cx.event(Event::OpinionAsked {
        case: case.to_string(),
        of: of.to_string(),
    });
    Ok(())
}

pub(crate) fn record_opinion(p: &mut Proposal, life: &LifeId, cx: &mut Cx, case: &str, of: &str, vault_ref: String) -> Result<(), Reject> {
    let c = held_or(p, life, case)?;
    let Some(o) = c
        .opinions
        .iter_mut()
        .find(|o| o.asked_of == of && o.vault_ref.is_none())
        .filter(|_| !vault_ref.is_empty())
    else {
        return reject("no_opinion", format!("no opinion is awaited from {of} on case {case}"));
    };
    o.vault_ref = Some(vault_ref);
    cx.event(Event::OpinionRecorded {
        case: case.to_string(),
        of: of.to_string(),
    });
    Ok(())
}

pub(crate) fn schedule_interview(
    _st: &UnderwritingState,
    p: &mut Proposal,
    cx: &mut Cx,
    life: &LifeId,
    scheduled_for: DateTime<Utc>,
    booked_by: String,
) -> Result<(), Reject> {
    if p.status != ProposalStatus::Open {
        return reject("closed", "the proposal is no longer open");
    }
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    if !matches!(f.stage, Stage::AwaitingDisclosure | Stage::InAssessment) {
        return reject("not_now", "an interview is booked while the life is being assessed");
    }
    f.interviews.push(Interview {
        scheduled_for,
        booked_by,
        underwriter: None,
        notes_ref: None,
        notes_hash: None,
        recording_ref: None,
        present: vec![],
        adviser_present: false,
        confirmed_at: None,
    });
    cx.event(Event::InterviewScheduled {
        proposal,
        life: life.clone(),
        scheduled_for,
    });
    if f.open_case().is_none() {
        flow::open_case(p, life, CaseType::TeleInterview, Queue::TeleInterview, ["RC-0202".to_string()].into(), cx)?;
    }
    Ok(())
}

pub(crate) fn record_interview(
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    case: &str,
    underwriter: &str,
    notes_ref: String,
    notes_hash: String,
    recording_ref: Option<String>,
    present: Vec<String>,
    adviser_present: bool,
) -> Result<(), Reject> {
    let proposal = p.id.clone();
    if held(p, life, case, underwriter)?.case_type != CaseType::TeleInterview {
        return reject("not_interview", "the case is not an interview");
    }
    let f = life_mut(p, life)?;
    // The adviser is on the call only with the life's consent.
    if adviser_present && !(f.consented(ConsentKind::AdviserAccess) && f.adviser_access_ended.is_none()) {
        return reject("adviser_not_permitted", "the life has not consented to the adviser being present");
    }
    if notes_ref.is_empty() || notes_hash.len() != 64 {
        return reject("disclosure", "interview notes need their SHA-256 and where they are sealed");
    }
    let party = f.party.clone();
    let Some(i) = f.interviews.iter_mut().rev().find(|i| i.confirmed_at.is_none()) else {
        return reject("no_interview", "no interview is booked");
    };
    i.underwriter = Some(underwriter.to_string());
    i.notes_ref = Some(notes_ref);
    i.notes_hash = Some(notes_hash);
    i.recording_ref = recording_ref;
    i.present = present;
    i.adviser_present = adviser_present;
    cx.event(Event::InterviewRecorded {
        proposal: proposal.clone(),
        life: life.clone(),
    });
    // What was said is the underwriter's note until the person confirms it.
    cx.effect(Effect::Notify {
        to: Recipient::Party { party },
        kind: NoticeKind::InterviewToConfirm,
        proposal,
        life: Some(life.clone()),
        reasons: vec![],
    });
    Ok(())
}

/// The life confirms the recorded answers with a signed step: now they are
/// disclosure, and the rules run on them.
pub(crate) fn confirm_interview(st: &UnderwritingState, p: &mut Proposal, cx: &mut Cx, d: SubmitDisclosure) -> Result<(), Reject> {
    let f = life_ref(p, &d.life)?;
    if !f
        .interviews
        .last()
        .is_some_and(|i| i.confirmed_at.is_none() && i.notes_hash.as_deref() == Some(d.hash.as_str()))
    {
        return reject("no_interview", "there are no recorded interview answers with that hash to confirm");
    }
    crate::intake::accept_disclosure(st, p, cx, &d, "interview")?;
    let f = life_mut(p, &d.life)?;
    if let Some(i) = f.interviews.last_mut() {
        i.confirmed_at = Some(cx.at);
    }
    if let Some(c) = f.cases.iter_mut().find(|c| c.is_open() && c.case_type == CaseType::TeleInterview) {
        c.reasons.insert(reason::INTERVIEW_CONFIRMED.to_string());
    }
    cx.event(Event::InterviewConfirmed {
        proposal: p.id.clone(),
        life: d.life.clone(),
    });
    flow::route(&st.config, p, &d.life, cx, true)
}

/// One cover's terms as the authorisation policy sees them.
fn subject<'a>(p: &'a Proposal, cover: &'a crate::model::ProposedCover, outcome: &CoverOutcome) -> Subject<'a> {
    let (loading_percent, has_exclusions, sum) = match outcome {
        CoverOutcome::Accept {
            loading_percent,
            exclusions,
            revised_sum_insured_minor,
        } => (
            *loading_percent,
            !exclusions.is_empty(),
            revised_sum_insured_minor.unwrap_or(cover.sum_insured_minor),
        ),
        _ => (0, false, cover.sum_insured_minor),
    };
    Subject {
        product: &p.product_code,
        cover_type: &cover.cover_type,
        sum_insured_minor: sum,
        loading_percent,
        has_exclusions,
        outcome: outcome.key(),
    }
}

/// Check terms are well formed for the covers on the life.
pub(crate) fn check_terms(st: &UnderwritingState, p: &Proposal, life: &LifeId, terms: &[CoverTerms], cx: &Cx) -> Result<(), Reject> {
    let covers: BTreeSet<&str> = p.covers_on(life).map(|c| c.code.as_str()).collect();
    if terms.len() != covers.len() || covers != terms.iter().map(|t| t.cover.as_str()).collect() {
        return reject("terms", "a decision gives terms for each cover on the life, once");
    }
    let Some(library) = st.config.exclusions_on(p.received_on) else {
        return reject("no_rules", "the exclusion list is not active");
    };
    for t in terms {
        let proposed = p.cover(life, &t.cover).map_or(0, |c| c.sum_insured_minor);
        match &t.outcome {
            CoverOutcome::Accept {
                loading_percent,
                exclusions,
                revised_sum_insured_minor,
            } => {
                if let Some(e) = exclusions.iter().find(|e| !library.contains(&e.code)) {
                    return reject("unknown_code", format!("exclusion {} is not in the code list", e.code));
                }
                if *loading_percent > 500
                    || exclusions.iter().any(|e| e.until.is_some_and(|u| u <= e.from))
                    || revised_sum_insured_minor.is_some_and(|r| r <= 0 || r >= proposed)
                {
                    return reject("terms", format!("the terms on {} are out of range", t.cover));
                }
            }
            CoverOutcome::Defer { until } if *until <= cx.today => return reject("terms", "a deferral runs to a later date"),
            // Unknown terms exist only on migrated cover.
            CoverOutcome::UnknownTerms => return reject("terms", "a decision cannot leave terms unknown"),
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn propose(
    st: &UnderwritingState,
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    case: &str,
    underwriter: &str,
    terms: Vec<CoverTerms>,
    reasons: BTreeSet<String>,
    withheld_reasons: BTreeSet<String>,
    notes_ref: Option<String>,
    evidence: BTreeSet<EvidenceKind>,
) -> Result<(), Reject> {
    if p.status != ProposalStatus::Open {
        return reject("closed", "the proposal is no longer open");
    }
    if matches!(
        held(p, life, case, underwriter)?.case_type,
        CaseType::Countersign | CaseType::NonDisclosureReview
    ) {
        return reject("not_allowed", "this case does not take a new decision");
    }
    known_reasons(st, p, &reasons, &withheld_reasons)?;
    check_terms(st, p, life, &terms, cx)?;
    let f = life_ref(p, life)?;
    let settings = &st.config.settings;
    let mut second = false;
    for t in &terms {
        let Some(cover) = p.cover(life, &t.cover) else { continue };
        let s = subject(p, cover, &t.outcome);
        st.require(p, life, underwriter, CaseAction::Decide, &s, false)?;
        if t.outcome.is_accept() {
            // The same amount cannot go ahead of the reinsurer, or around them.
            match f.capacity.get(&t.cover).filter(|r| r.amount_minor == s.sum_insured_minor).map(|r| &r.state) {
                Some(CapacityState::FacultativeRequired) => return reject("awaiting_reinsurer", "the reinsurer has not yet answered for this amount"),
                Some(CapacityState::FacultativeDeclined | CapacityState::Excluded) => {
                    return reject("no_capacity", "there is no reinsurance capacity for this amount")
                }
                _ => {}
            }
        }
        // Declines and deferrals, large amounts and heavy loadings take two signatures.
        second |=
            !t.outcome.is_accept() || s.sum_insured_minor > settings.countersign_above_minor || s.loading_percent > settings.countersign_loading_above_percent;
    }
    let profile_version = st.profile(underwriter)?.version;
    let mut decision = flow::new_decision(&st.config, p, f, DecisionSource::Underwriter, terms, BTreeSet::new(), cx);
    decision.signatures.push(Signature {
        underwriter: underwriter.to_string(),
        profile_version,
        reasons,
        withheld_reasons,
        at: cx.at,
    });
    decision.evidence = evidence;
    decision.notes_ref = notes_ref;
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    // Evidence still awaited is no longer needed: the decision was made without it.
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
    // An earlier decision stays on file, replaced.
    for d in f.decisions.iter_mut().filter(|d| d.status != DecisionStatus::Superseded) {
        d.status = DecisionStatus::Superseded;
    }
    cx.event(Event::DecisionProposed {
        proposal,
        life: life.clone(),
        decision: decision.reference.clone(),
        needs_second_signature: second,
    });
    if !second {
        f.decisions.push(decision);
        return flow::after_signed(&st.config, p, life, cx);
    }
    decision.status = DecisionStatus::AwaitingCountersign;
    f.decisions.push(decision);
    f.stage = Stage::AwaitingCountersign;
    if let Some(c) = f.cases.iter_mut().find(|c| c.id == case) {
        c.move_to(CaseType::Countersign, Queue::SeniorReview, [], cx.at);
        cx.event(Event::CaseMoved {
            case: case.to_string(),
            case_type: CaseType::Countersign,
            queue: Queue::SeniorReview,
        });
    }
    Ok(())
}

pub(crate) fn countersign(
    st: &UnderwritingState,
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    case: &str,
    underwriter: &str,
    agree: bool,
    reasons: BTreeSet<String>,
    withheld_reasons: BTreeSet<String>,
) -> Result<(), Reject> {
    if p.status != ProposalStatus::Open {
        return reject("closed", "the proposal is no longer open");
    }
    if held(p, life, case, underwriter)?.case_type != CaseType::Countersign {
        return reject("not_allowed", "the case is not waiting for a second signature");
    }
    known_reasons(st, p, &reasons, &withheld_reasons)?;
    let f = life_ref(p, life)?;
    let Some(d) = f.decisions.last().filter(|d| d.status == DecisionStatus::AwaitingCountersign) else {
        return reject("no_decision", "no decision is waiting for a second signature");
    };
    let signed = d.signers().any(|s| s == underwriter);
    for t in &d.terms {
        let Some(cover) = p.cover(life, &t.cover) else { continue };
        st.require(p, life, underwriter, CaseAction::Countersign, &subject(p, cover, &t.outcome), signed)?;
    }
    let (proposal, reference, profile_version) = (p.id.clone(), d.reference.clone(), st.profile(underwriter)?.version);
    let f = life_mut(p, life)?;
    if agree {
        if let Some(d) = f.decisions.last_mut() {
            d.signatures.push(Signature {
                underwriter: underwriter.to_string(),
                profile_version,
                reasons,
                withheld_reasons,
                at: cx.at,
            });
        }
        return flow::after_signed(&st.config, p, life, cx);
    }
    // Refused: the decision stays on file as replaced and the case goes back
    // to the queue it came from, with the senior's reasons.
    if let Some(d) = f.decisions.last_mut() {
        d.status = DecisionStatus::Superseded;
    }
    f.stage = Stage::InAssessment;
    cx.event(Event::CountersignRefused {
        proposal,
        life: life.clone(),
        decision: reference,
    });
    if let Some(c) = f.cases.iter_mut().find(|c| c.id == case) {
        let back = c.moves.iter().rev().map(|m| m.1).find(|q| *q != Queue::SeniorReview).unwrap_or(Queue::Medical);
        c.move_to(
            CaseType::Referral,
            back,
            reasons.into_iter().chain([reason::COUNTERSIGN_REFUSED.to_string()]),
            cx.at,
        );
        cx.event(Event::CaseMoved {
            case: case.to_string(),
            case_type: CaseType::Referral,
            queue: back,
        });
    }
    Ok(())
}

/// The liaison releases named documents for a facultative referral.
pub(crate) fn release_to_reinsurer(
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    case: &str,
    underwriter: &str,
    treaty: String,
    documents: Vec<String>,
) -> Result<(), Reject> {
    if held(p, life, case, underwriter)?.case_type != CaseType::ReinsurerReferral {
        return reject("not_allowed", "documents go to a reinsurer only on a facultative referral");
    }
    if documents.is_empty() || treaty.is_empty() {
        return reject("documents", "a release names the treaty and the documents");
    }
    cx.event(Event::FileAccessed {
        who: treaty,
        role: "reinsurer".to_string(),
        purpose: format!("facultative referral, released by {underwriter}"),
        proposal: p.id.clone(),
        life: Some(life.clone()),
        documents,
    });
    Ok(())
}
