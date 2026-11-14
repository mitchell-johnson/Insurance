//! A future insurability option exercised on a life event.
//!
//! The product gives a cover an option to add to it after events such as a
//! marriage or a birth, with no health questions. The request opens as a
//! proposal, entry `insurability_option`, carrying the event, its date and
//! where its proof is kept. A person on the financial queue checks the
//! proof: accepted, the cover is decided with no underwriting (source
//! `insurability_option`, reason RC-0977), capacity is reserved as for any
//! increase, and the driver sends the core `ExerciseOption`, which checks the
//! option's window, age, uses and limits and writes the layer on the terms of
//! the life's original cover. Not accepted, the request ends withdrawn and the
//! policyholder is told why (RC-0978).

use std::collections::BTreeSet;

use ic_core::LifeId;

use crate::case::{CaseType, Queue};
use crate::codes::reason;
use crate::command::{Effect, Event, NoticeKind, OpenProposal, Recipient, Reject};
use crate::flow;
use crate::machine::{life_mut, life_ref, reject, Cx, UnderwritingState};
use crate::model::{CoverOutcome, CoverTerms, DecisionSource, Entry, Proposal, ProposalStatus};

/// What an option request must be before it opens.
pub(crate) fn door(open: &OpenProposal, cx: &Cx) -> Result<(), Reject> {
    let Entry::InsurabilityOption {
        policy,
        event,
        event_date,
        evidence,
    } = &open.entry
    else {
        return Ok(());
    };
    if open.lives.len() != 1 || open.covers.len() != 1 || policy != &open.policy || open.replacement {
        return reject("option", "an option is exercised on one cover of one life on the policy it belongs to");
    }
    if event.trim().is_empty() || evidence.trim().is_empty() {
        return reject("option", "an option names its life event and where the proof of it is kept");
    }
    if *event_date > cx.today {
        return reject("option", "an option is exercised after its life event, not before");
    }
    Ok(())
}

/// A new option request goes to a person to check the proof of the event.
pub(crate) fn start(p: &mut Proposal, cx: &mut Cx) -> Result<(), Reject> {
    if !matches!(p.entry, Entry::InsurabilityOption { .. }) {
        return Ok(());
    }
    let life = p.lives.keys().next().cloned().expect("one life, checked at the door");
    flow::open_case(
        p,
        &life,
        CaseType::Referral,
        Queue::Financial,
        [reason::INSURABILITY_OPTION.to_string()].into(),
        cx,
    )
    .map(|_| ())
}

/// The person holding the case accepts the proof of the event, or does not.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check(
    st: &UnderwritingState,
    p: &mut Proposal,
    life: &LifeId,
    cx: &mut Cx,
    case: &str,
    underwriter: &str,
    accepted: bool,
    note: &str,
) -> Result<(), Reject> {
    let Entry::InsurabilityOption { event, .. } = p.entry.clone() else {
        return reject("entry", "only an insurability option has proof of a life event to check");
    };
    if p.status != ProposalStatus::Open {
        return reject("closed", "the proposal is no longer open");
    }
    crate::casework::held(p, life, case, underwriter)?;
    if !accepted && note.trim().is_empty() {
        return reject("note", "say why the proof of the event is not accepted");
    }
    let proposal = p.id.clone();
    cx.event(Event::OptionEvidenceChecked {
        proposal: proposal.clone(),
        life: life.clone(),
        accepted,
        by: underwriter.to_string(),
    });
    flow::close_cases(life_mut(p, life)?, cx);
    if !accepted {
        p.status = ProposalStatus::Withdrawn { at: cx.at };
        cx.event(Event::ProposalWithdrawn { proposal: proposal.clone() });
        flow::end_proposal(p, cx);
        cx.effect(Effect::Notify {
            to: Recipient::Party { party: p.policyholder.clone() },
            kind: NoticeKind::ProposalWithdrawn,
            proposal,
            life: None,
            reasons: vec![reason::OPTION_EVIDENCE_REFUSED.to_string()],
        });
        return Ok(());
    }
    // The core writes the layer on the terms of the life's original cover:
    // nothing here adds to them or takes from them.
    let terms: Vec<CoverTerms> = p
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
    let source = DecisionSource::InsurabilityOption {
        event,
        checked_by: underwriter.to_string(),
    };
    let reasons: BTreeSet<String> = [reason::INSURABILITY_OPTION.to_string()].into();
    let decision = flow::new_decision(&st.config, p, life_ref(p, life)?, source, terms, reasons, cx);
    life_mut(p, life)?.decisions.push(decision);
    flow::after_signed(&st.config, p, life, cx)
}
