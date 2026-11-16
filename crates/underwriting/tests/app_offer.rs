//! A life answers an offer of terms from the app: the progress row says
//! which offer is waiting, and the app may send the signed answer.

mod common;

use common::*;
use ic_core::LifeId;
use ic_underwriting::command::{Command, OfferResponse};
use ic_underwriting::progress;
use ic_underwriting::questions::Answer;

/// Purpose: while terms wait for the life's answer, the life's progress row names the offer and
/// when it lapses; the answer may come from the app, signed on the life's device, and an
/// unsigned or other caller's answer is refused; once answered, the row no longer offers it.
/// Why it matters: nothing sent the life's answer to an offer of terms, so a loaded or excluded
/// applicant could read their offer letter and never accept it; the app needs to know which
/// offer to answer, and only the life's own signed step may accept terms.
#[test]
fn the_life_answers_an_offer_of_terms_from_the_app() {
    let mut w = World::new();
    let open = w.open_command("P1", &["L1"], &[("L1", "life", "life", 400_000_00)], true);
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let answers = with(&[
        ("smoked_last_12_months", Answer::YesNo(true)),
        ("back_or_neck_last_5_years", Answer::YesNo(true)),
    ]);
    let disclosed = w.disclose("P1", "L1", &answers);
    w.automatic(&disclosed);
    let offer = w.p("P1").lives[&LifeId::from("L1")].open_offer().expect("terms to accept").clone();
    let row = |w: &World| progress::rows(w.p("P1")).into_iter().find(|r| r.kind == "proposal_life").unwrap();
    assert_eq!(row(&w).body["offer"]["offer"], offer.id.as_str());
    assert_eq!(row(&w).body["offer"]["expires_on"], offer.expires_on.to_string());

    let answer = |signed| Command::RespondToOffer {
        proposal: "P1".into(),
        life: "L1".into(),
        offer: offer.id.clone(),
        response: OfferResponse::Accept,
        signed,
    };
    assert_eq!(w.err("customer-app", answer(signed("offer", &["P1", "L1", &offer.id, "decline"]))), "signature");
    assert_eq!(w.err("adviser-portal", answer(signed("offer", &["P1", "L1", &offer.id, "accept"]))), "caller");
    w.ok("customer-app", answer(signed("offer", &["P1", "L1", &offer.id, "accept"])));
    assert!(row(&w).body.get("offer").is_none(), "{}", row(&w).body);
}
