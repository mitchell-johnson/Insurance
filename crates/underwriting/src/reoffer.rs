//! Moving a loading from the employer to a scheme member.
//!
//! A scheme's terms can bill a member's loading to the employer in one
//! total only while enough members hold underwritten cover. When leavers
//! take the count below that, group administration raises an exception and
//! staff may ask for the loading to move to the member. The member was told
//! the employer pays, so nothing moves until they agree: underwriting, which
//! holds the terms, offers them the same terms with them paying the loading.
//! Accepted, the terms are applied again with `member_pays_loading`, under a
//! new issue key. Declined or out of time, the terms in place stand and group
//! administration is told, so staff decide what next.

use ic_core::LifeId;

use crate::command::{Effect, Event, NoticeKind, OfferResponse, Recipient, Reject};
use crate::config::Config;
use crate::flow;
use crate::machine::{reject, Cx};
use crate::model::{CoverOutcome, Entry, LifeFile, OfferStatus, Proposal, ProposalStatus, Reoffer, Stage};
use crate::signing::{message, SignedStep};

/// The accepted life on the proposal whose terms carry a loading.
fn loaded_life(p: &Proposal) -> Option<&LifeFile> {
    p.lives.values().filter(|f| f.stage == Stage::Accepted).find(|f| {
        f.effective_decision().is_some_and(|d| {
            p.covers_on(&f.life)
                .any(|c| matches!(d.terms_for(&c.code), Some(CoverOutcome::Accept { loading_percent, .. }) if *loading_percent > 0))
        })
    })
}

/// Staff ask for the member to be offered to pay their own loading.
pub(crate) fn make(cfg: &Config, p: &mut Proposal, cx: &mut Cx, reason: &str) -> Result<(), Reject> {
    if !matches!(p.entry, Entry::GroupExcess { .. }) {
        return reject("entry", "only a scheme member's excess cover has a loading the employer can stop paying");
    }
    if !matches!(p.status, ProposalStatus::Issued { .. }) {
        return reject("not_issued", format!("proposal {} is {}: its terms are not in place", p.id, p.status.key()));
    }
    if reason.trim().is_empty() {
        return reject("reason", "say why the loading is moving to the member");
    }
    if p.member_pays_loading() {
        return reject("member_pays", "the member already pays their loading");
    }
    if p.open_reoffer().is_some() {
        return reject("reoffer_open", "the member already has an offer to pay their loading");
    }
    let Some(f) = loaded_life(p) else {
        return reject("no_loading", "the terms in place carry no loading to move");
    };
    let (life, party) = (f.life.clone(), f.party.clone());
    let r = Reoffer {
        id: format!("{}/reoffer/{}", p.id, p.reoffers.len() + 1),
        life: life.clone(),
        reason: reason.trim().to_string(),
        made_at: cx.at,
        expires_on: cx.today + chrono::Duration::days(cfg.settings.offer_days),
        status: OfferStatus::Open,
        responded_at: None,
        written: false,
    };
    cx.event(Event::ReofferMade {
        proposal: p.id.clone(),
        life: life.clone(),
        offer: r.id.clone(),
        expires_on: r.expires_on,
    });
    // The terms are the ones the member was told of; nothing new is said
    // about why.
    cx.effect(Effect::Notify {
        to: Recipient::Party { party },
        kind: NoticeKind::OfferMade,
        proposal: p.id.clone(),
        life: Some(life),
        reasons: vec![],
    });
    p.reoffers.push(r);
    Ok(())
}

/// Whether `offer` names an offer to move a loading.
pub(crate) fn is_reoffer(p: &Proposal, offer: &str) -> bool {
    p.reoffers.iter().any(|r| r.id == offer)
}

/// The member's answer, signed on their device as any offer is.
pub(crate) fn respond(p: &mut Proposal, cx: &mut Cx, life: &LifeId, offer: &str, response: OfferResponse, signed: &SignedStep) -> Result<(), Reject> {
    if !p.open_reoffer().is_some_and(|r| r.id == offer && &r.life == life) || !matches!(p.status, ProposalStatus::Issued { .. }) {
        return reject("no_offer", format!("offer {offer} is not open"));
    }
    if !signed.verifies(&message("offer", &[&p.id.0, &life.0, offer, response.key()])) {
        return reject("signature", "the response is not signed by the life's device key");
    }
    if response == OfferResponse::Counter {
        return reject("counter", "the terms are the ones in place: the member accepts or declines paying the loading");
    }
    cx.event(Event::OfferResponded {
        proposal: p.id.clone(),
        life: life.clone(),
        offer: offer.to_string(),
        response,
    });
    if response == OfferResponse::Decline {
        end(p, cx, OfferStatus::Declined);
        return Ok(());
    }
    if let Some(r) = p.reoffers.last_mut() {
        r.status = OfferStatus::Accepted;
        r.responded_at = Some(cx.at);
    }
    flow::issue(p, cx)
}

/// An open offer past its last day ends as declined would.
pub(crate) fn expire(p: &mut Proposal, cx: &mut Cx) {
    if p.open_reoffer().is_some_and(|r| cx.today > r.expires_on) {
        end(p, cx, OfferStatus::Expired);
    }
}

fn end(p: &mut Proposal, cx: &mut Cx, status: OfferStatus) {
    let (proposal, policy) = (p.id.clone(), p.policy.clone());
    let Some(r) = p.reoffers.last_mut() else { return };
    r.status = status;
    r.responded_at = Some(cx.at);
    cx.event(Event::ReofferEnded {
        proposal: proposal.clone(),
        offer: r.id.clone(),
        status,
    });
    cx.effect(Effect::ReofferEnded {
        key: format!("{}/ended", r.id),
        proposal,
        policy,
        outcome: if status == OfferStatus::Declined { "declined" } else { "expired" }.to_string(),
    });
}

/// The terms with the member paying were written: the first such answer
/// after the member accepted. Nothing else about the cover changed, so
/// capacity, risk factors and the issue letter are not done again.
pub(crate) fn written(p: &mut Proposal) -> bool {
    match p.reoffers.iter_mut().find(|r| r.status == OfferStatus::Accepted && !r.written) {
        Some(r) => {
            r.written = true;
            true
        }
        None => false,
    }
}
