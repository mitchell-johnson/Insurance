//! A scheme leaver continuing underwritten cover above the automatic limit.
//!
//! Group administration issues the continuation policy itself for the
//! automatic amount, then opens a proposal for the amount above it, entry
//! `group_continuation`, naming the member's earlier excess proposals. The
//! amount is accepted on the terms those proposals accepted, with no
//! disclosure, no new evidence and no offer: the member accepted those terms
//! when they were made. Capacity is reserved as for any retail cover, and
//! the driver writes the amount as a layer on the continuation policy.

use std::collections::BTreeSet;

use crate::codes::{reason, AppliedExclusion};
use crate::command::{OpenProposal, Reject};
use crate::flow;
use crate::machine::{life_mut, life_ref, reject, Cx, UnderwritingState};
use crate::model::{CoverOutcome, CoverTerms, DecisionSource, Entry, Proposal, ProposalStatus};

/// Scheme life cover carries on as individual life cover.
fn kind(cover_type: &str) -> &str {
    if cover_type == "group_life" {
        "life"
    } else {
        cover_type
    }
}

/// The terms the earlier excess proposals give the cover being continued,
/// or why they give none. The heaviest loading and every exclusion stand
/// where the earlier decisions differ.
pub(crate) fn earlier_terms(st: &UnderwritingState, open: &OpenProposal) -> Result<Option<(i64, u32, Vec<AppliedExclusion>)>, Reject> {
    let Entry::GroupContinuation { earlier, policy, .. } = &open.entry else {
        return Ok(None);
    };
    let refuse = |why: &str| reject("continuation", why.to_string());
    if earlier.is_empty() || open.lives.len() != 1 || open.covers.len() != 1 || policy != &open.policy {
        return refuse("a continuation is one life and one cover on the continuation policy, carried on from earlier excess proposals");
    }
    let (life, cover) = (&open.lives[0], &open.covers[0]);
    let mut accepted = 0i64;
    let mut loading = 0u32;
    let mut exclusions: Vec<AppliedExclusion> = Vec::new();
    for id in earlier {
        let Some(p) = st.proposals.get(id) else {
            return reject("continuation", format!("earlier proposal {id} is not known"));
        };
        if !matches!(p.entry, Entry::GroupExcess { .. }) || !matches!(p.status, ProposalStatus::Issued { .. }) {
            return reject("continuation", format!("earlier proposal {id} is not issued scheme excess cover"));
        }
        let Some(f) = p.lives.values().find(|f| f.party == life.party) else {
            return reject("continuation", format!("earlier proposal {id} is not on this member"));
        };
        let Some(d) = f.effective_decision() else {
            return reject("continuation", format!("earlier proposal {id} has no decision in force"));
        };
        for c in p.covers_on(&f.life).filter(|c| kind(&c.cover_type) == kind(&cover.cover_type)) {
            if let Some(CoverOutcome::Accept {
                loading_percent,
                exclusions: excluded,
                revised_sum_insured_minor,
            }) = d.terms_for(&c.code)
            {
                accepted += revised_sum_insured_minor.unwrap_or(c.sum_insured_minor);
                loading = loading.max(*loading_percent);
                for e in excluded {
                    if !exclusions.iter().any(|x| x.code == e.code) {
                        exclusions.push(e.clone());
                    }
                }
            }
        }
    }
    if accepted < cover.sum_insured_minor {
        return reject(
            "continuation",
            format!("the earlier proposals accepted {accepted}, less than the {} continued", cover.sum_insured_minor),
        );
    }
    Ok(Some((accepted, loading, exclusions)))
}

/// Decide the continued cover on the earlier terms, as soon as the proposal
/// opens. The exclusions run from the continuation's inception, as the
/// cover itself does.
pub(crate) fn decide(st: &UnderwritingState, p: &mut Proposal, terms: (i64, u32, Vec<AppliedExclusion>), cx: &mut Cx) -> Result<(), Reject> {
    let Entry::GroupContinuation { earlier, inception, .. } = p.entry.clone() else {
        return Ok(());
    };
    let (_, loading_percent, exclusions) = terms;
    let life = p.lives.keys().next().cloned().expect("one life, checked at the door");
    let terms: Vec<CoverTerms> = p
        .covers_on(&life)
        .map(|c| CoverTerms {
            cover: c.code.clone(),
            outcome: CoverOutcome::Accept {
                loading_percent,
                exclusions: exclusions.iter().map(|e| AppliedExclusion { from: inception, ..e.clone() }).collect(),
                revised_sum_insured_minor: None,
            },
        })
        .collect();
    let source = DecisionSource::Continuation {
        earlier: earlier.iter().map(ToString::to_string).collect(),
    };
    let reasons: BTreeSet<String> = [reason::CONTINUATION.to_string()].into();
    let decision = flow::new_decision(&st.config, p, life_ref(p, &life)?, source, terms, reasons, cx);
    life_mut(p, &life)?.decisions.push(decision);
    flow::after_signed(&st.config, p, &life, cx)
}
