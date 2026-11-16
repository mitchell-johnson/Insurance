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

/// Purpose: when the core refuses the issue because the product changed, the life's progress row
/// carries a requote: the code, the premium the applicant accepted and the policy it would issue
/// as (each accepted life and cover on its decided terms), for the channel to price; the applicant
/// accepts the new price from the app with a signed step and the issue goes again; another refusal
/// code offers no requote.
/// Why it matters: nothing put a changed product's new price in front of the applicant, so a proposal
/// refused for a product change sat there until a person noticed. The channel cannot price a
/// proposal it cannot see, and a new price must never be applied without the applicant's own signature.
#[test]
fn the_applicant_accepts_a_new_price_from_the_app_when_the_product_changed() {
    use ic_underwriting::model::QuoteBinding;
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let disclosed = w.disclose("P1", "L1", &with(&[]));
    w.automatic(&disclosed);
    let row = |w: &World| progress::rows(w.p("P1")).into_iter().find(|r| r.kind == "proposal_life").unwrap();
    w.ok(
        "issuer",
        Command::IssueRejected {
            proposal: "P1".into(),
            code: "policy_exists".into(),
        },
    );
    assert!(row(&w).body.get("requote").is_none(), "only a product change is requoted");
    w.ok(
        "underwriting-console",
        Command::RetryIssue {
            proposal: "P1".into(),
            underwriter: "amy".into(),
        },
    );
    w.ok(
        "issuer",
        Command::IssueRejected {
            proposal: "P1".into(),
            code: "product_changed".into(),
        },
    );
    let requote = row(&w).body["requote"].clone();
    assert_eq!(
        (requote["code"].as_str(), requote["quoted_minor"].as_i64()),
        (Some("product_changed"), Some(w.p("P1").quote.premium_minor))
    );
    assert_eq!(requote["application"]["policy"], w.p("P1").policy.0.as_str());
    assert_eq!(requote["application"]["lives"][0]["id"], "L1");
    assert_eq!(requote["application"]["covers"][0]["sum_insured_minor"], 500_000_00);

    let quote = QuoteBinding {
        product_hash: "prodhash-2".into(),
        rate_tables: vec![],
        premium_minor: 91_00,
        quoted_on: w.today(),
        cover_premiums_minor: Default::default(),
    };
    let accepted = w.ok(
        "customer-app",
        Command::Requote {
            proposal: "P1".into(),
            quote,
            signed: signed("requote", &["P1", "prodhash-2", "9100"]),
        },
    );
    assert!(
        matches!(&accepted.effects[0], ic_underwriting::command::Effect::IssuePolicy { expected_product_hash, .. } if expected_product_hash == "prodhash-2")
    );
    assert!(row(&w).body.get("requote").is_none());
}
