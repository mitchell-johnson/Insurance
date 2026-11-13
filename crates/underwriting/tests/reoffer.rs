//! Moving a scheme member's loading from the employer to the member: staff
//! ask, the member is offered the same terms with them paying, and only
//! their signed acceptance changes who pays.

mod common;

use common::*;
use ic_underwriting::command::{Applied, Command, Effect, Event, IssuedLayer, NoticeKind, OfferResponse, Recipient};
use ic_underwriting::model::{Entry, OfferStatus, ProposalStatus};

const ID: &str = "grp-uw:S1:7";

/// A scheme member's excess cover accepted with a 50 per cent loading, offered,
/// accepted by the member, and written by the core as layer 2.
fn issued_with_loading(w: &mut World) {
    let mut open = w.open_command(ID, &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    open.product_code = "group-life".into();
    open.entry = Entry::GroupExcess { policy: open.policy.clone() };
    w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    w.disclose(ID, "L1", &yes("heart_condition"));
    let case = w.take(ID, "L1", "amy");
    let proposed = w.propose(&case, "amy", vec![accept("life", 50, None)]).unwrap();
    w.automatic(&proposed);
    let accepted = w.respond(ID, "L1", OfferResponse::Accept);
    assert!(accepted.effects.iter().any(|e| matches!(
        e,
        Effect::ApplyTerms {
            member_pays_loading: false,
            ..
        }
    )));
    issued(w, 2);
}

fn issued(w: &mut World, layer: u32) -> Applied {
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: ID.into(),
            layers: vec![IssuedLayer {
                life: "L1".into(),
                cover: "life".into(),
                layer,
            }],
        },
    )
}

fn reoffer(reason: &str) -> Command {
    Command::ReofferExcessTerms {
        proposal: ID.into(),
        reason: reason.into(),
    }
}

/// The member's signed answer to the open offer to pay their loading.
fn answer(w: &mut World, response: OfferResponse) -> Result<Applied, ic_underwriting::command::Reject> {
    let offer = w.p(ID).open_reoffer().expect("an open offer").id.clone();
    let signed = signed("offer", &[ID, "L1", &offer, response.key()]);
    w.send(
        "new-business",
        Command::RespondToOffer {
            proposal: ID.into(),
            life: "L1".into(),
            offer,
            response,
            signed,
        },
    )
}

/// Purpose: group administration's staff ask for an issued excess proposal's loading to move
/// to the member; the member is sent an offer letter naming no reasons; on their signed
/// acceptance the same terms are applied again under a new issue key with
/// `member_pays_loading`, and when the core has written the replacement layer the proposal is
/// issued again without asking reinsurance for capacity or writing risk factors a second time.
/// Why it matters: the member was told their employer pays. Moving the loading without their
/// agreement would bill them for something they never accepted, and a second capacity
/// confirmation for unchanged cover would double the reinsured amount.
#[test]
fn a_loading_moves_to_the_member_only_on_their_signed_acceptance() {
    let mut w = World::new();
    issued_with_loading(&mut w);

    let made = w.ok("group-admin", reoffer("members with underwritten cover fell below the scheme's minimum"));
    let Some(Event::ReofferMade { offer, .. }) = made.events.first() else {
        panic!("no offer: {:?}", made.events)
    };
    assert_eq!(offer, &format!("{ID}/reoffer/1"));
    assert!(made.effects.iter().any(|e| matches!(
        e,
        Effect::Notify { to: Recipient::Party { .. }, kind: NoticeKind::OfferMade, reasons, .. } if reasons.is_empty()
    )));
    assert!(matches!(w.p(ID).status, ProposalStatus::Issued { .. }), "the terms in place stand meanwhile");
    assert_eq!(w.err("group-admin", reoffer("again")), "reoffer_open");

    // The member cannot bargain the terms here, and a forged answer is refused.
    assert_eq!(answer(&mut w, OfferResponse::Counter).unwrap_err().code, "counter");
    let forged = Command::RespondToOffer {
        proposal: ID.into(),
        life: "L1".into(),
        offer: format!("{ID}/reoffer/1"),
        response: OfferResponse::Accept,
        signed: signed("offer", &[ID, "L1", "another offer", "accept"]),
    };
    assert_eq!(w.err("new-business", forged), "signature");

    let accepted = answer(&mut w, OfferResponse::Accept).unwrap();
    let terms: Vec<_> = accepted
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::ApplyTerms {
                key,
                covers,
                member_pays_loading,
                ..
            } => Some((key.clone(), covers[0].loadings.len(), *member_pays_loading)),
            _ => None,
        })
        .collect();
    assert_eq!(terms, vec![(format!("{ID}/issue/2"), 1, true)], "the same loaded terms, under a new key");
    assert_eq!(w.p(ID).status, ProposalStatus::Issuing);

    let written = issued(&mut w, 3);
    assert!(matches!(w.p(ID).status, ProposalStatus::Issued { .. }));
    assert!(
        !written
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ConfirmCapacity { .. } | Effect::RecordRiskFactors { .. } | Effect::Notify { .. })),
        "{:?}",
        written.effects
    );
    assert!(w.p(ID).reoffers[0].written);
    assert_eq!(w.err("group-admin", reoffer("again")), "member_pays");
}

/// Purpose: a member who declines, or lets the offer run out, keeps the terms in place with
/// the employer paying, and group administration is told under a key of the offer's own; a
/// new offer can then be made. Staff can ask only on a scheme member's issued excess
/// proposal that carries a loading, and only with a reason.
/// Why it matters: if the end of the offer produced no answer, the scheme's exception would
/// stay open with nobody knowing the member had said no; and an offer on standard terms, or on
/// a proposal still being underwritten, would tell a member about a loading they do not have.
#[test]
fn a_declined_or_expired_offer_leaves_the_terms_and_says_so() {
    let mut w = World::new();
    issued_with_loading(&mut w);
    assert_eq!(w.err("group-admin", reoffer(" ")), "reason");

    w.ok("group-admin", reoffer("below the minimum"));
    let declined = answer(&mut w, OfferResponse::Decline).unwrap();
    let ended = |a: &Applied| {
        a.effects.iter().find_map(|e| match e {
            Effect::ReofferEnded { key, outcome, .. } => Some((key.clone(), outcome.clone())),
            _ => None,
        })
    };
    assert_eq!(ended(&declined), Some((format!("{ID}/reoffer/1/ended"), "declined".into())));
    assert_eq!(w.p(ID).reoffers[0].status, OfferStatus::Declined);
    assert!(matches!(w.p(ID).status, ProposalStatus::Issued { .. }));
    assert_eq!(answer_closed(&mut w, 1), "no_offer");

    w.ok("group-admin", reoffer("asked again after a call"));
    w.days(30);
    let quiet = w.ok("timer", Command::Sweep { after: None, max: 100 });
    assert_eq!(ended(&quiet), None, "the last day is still open");
    w.days(1);
    let swept = w.ok("timer", Command::Sweep { after: None, max: 100 });
    assert_eq!(ended(&swept), Some((format!("{ID}/reoffer/2/ended"), "expired".into())));
    assert_eq!(w.p(ID).reoffers[1].status, OfferStatus::Expired);
    let again = w.ok("timer", Command::Sweep { after: None, max: 100 });
    assert_eq!(ended(&again), None, "an ended offer is not swept again");

    // Not group administration's to ask, not an excess proposal, not issued, no loading.
    assert_eq!(w.err("underwriting-console", reoffer("x")), "caller");
    w.open("P1", 100_000_00);
    assert_eq!(
        w.err(
            "group-admin",
            Command::ReofferExcessTerms {
                proposal: "P1".into(),
                reason: "x".into()
            }
        ),
        "entry"
    );
    let mut open = w.open_command("grp-uw:S1:8", &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    open.product_code = "group-life".into();
    open.entry = Entry::GroupExcess { policy: open.policy.clone() };
    w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    let standard = Command::ReofferExcessTerms {
        proposal: "grp-uw:S1:8".into(),
        reason: "x".into(),
    };
    assert_eq!(w.err("group-admin", standard.clone()), "not_issued");
    let disclosed = w.disclose("grp-uw:S1:8", "L1", &clean());
    w.automatic(&disclosed);
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "grp-uw:S1:8".into(),
            layers: vec![],
        },
    );
    assert_eq!(w.err("group-admin", standard), "no_loading");
}

/// An answer to offer `n`, which is no longer open.
fn answer_closed(w: &mut World, n: u32) -> String {
    let offer = format!("{ID}/reoffer/{n}");
    let signed = signed("offer", &[ID, "L1", &offer, OfferResponse::Accept.key()]);
    w.err(
        "new-business",
        Command::RespondToOffer {
            proposal: ID.into(),
            life: "L1".into(),
            offer,
            response: OfferResponse::Accept,
            signed,
        },
    )
}
