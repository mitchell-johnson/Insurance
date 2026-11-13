//! A proposal from quote to policy: straight through, with terms, and the
//! ways an offer can end.

mod common;

use common::*;
use ic_core::LifeId;
use ic_underwriting::command::{Command, Effect, Event, NoticeKind, OfferResponse, Recipient};
use ic_underwriting::config::{InterimSettings, Settings};
use ic_underwriting::model::{CoverOutcome, DecisionSource, DecisionStatus, OfferStatus, ProposalStatus, Stage};
use ic_underwriting::questions::Answer;

fn interim(w: &mut World) {
    let settings = Settings {
        version: 2,
        interim: Some(InterimSettings {
            product_code: "interim-accident".into(),
            cover_code: "accidental-death".into(),
            max_sum_insured_minor: 250_000_00,
        }),
        ..Settings::default()
    };
    w.ok(
        "config",
        Command::ActivateSettings {
            settings: Box::new(settings),
            approval: approval(),
        },
    );
}

/// Purpose: a clean application goes from proposal to policy with no person involved: interim
/// accidental death cover is asked for at once and capped, the rules accept at standard rates,
/// capacity is reserved before the decision takes effect, the policy is issued as one command
/// to the core bound to the quoted product version, and on the core's confirmation the capacity
/// is confirmed, the rating factors go to reinsurance and the interim cover ends.
/// Why it matters: this is the straight-through path most applicants take. Each step guards a
/// loss: no offer before capacity, the quoted terms are the issued terms, and interim cover is
/// never left running beside the policy that replaced it.
#[test]
fn a_clean_application_is_issued_straight_through() {
    let mut w = World::new();
    interim(&mut w);
    let opened = w.open("P1", 500_000_00);
    assert!(matches!(&opened.events[0], Event::ProposalReceived { lives: 1, expires_on, .. } if *expires_on == d(2027, 2, 9)));
    let Some(Effect::IssueInterim { command, key, .. }) = opened.effects.first() else {
        panic!("no interim cover")
    };
    assert_eq!(
        (key.as_str(), command.product_code.as_str(), command.covers[0].sum_insured_minor),
        ("P1/interim", "interim-accident", 250_000_00)
    );
    assert_eq!(command.policy.0, "POL-P1-interim");
    w.ok("issuer", Command::InterimIssued { proposal: "P1".into() });
    assert!(w.p("P1").interim_in_force());

    let disclosed = w.disclose("P1", "L1", &clean());
    assert_eq!(w.stage("P1", "L1"), Stage::AwaitingCapacity);
    let Some(Effect::ReserveCapacity {
        reservation,
        sum_insured_minor,
        cover_type,
        proposal_expires,
        ..
    }) = disclosed.effects.first()
    else {
        panic!("no reservation")
    };
    assert_eq!(
        (reservation.as_str(), *sum_insured_minor, cover_type.as_str(), *proposal_expires),
        ("P1/L1/life/1", 500_000_00, "life", d(2027, 2, 9))
    );
    assert!(
        !disclosed.events.iter().any(|e| matches!(e, Event::DecisionMade { .. })),
        "nothing is decided before capacity is in hand"
    );

    let settled = w.automatic(&disclosed);
    assert_eq!((w.stage("P1", "L1"), w.p("P1").status.clone()), (Stage::Accepted, ProposalStatus::Issuing));
    let decision = &w.p("P1").lives[&LifeId::from("L1")].decisions[0];
    assert_eq!(
        (decision.source.clone(), decision.status, decision.all_reasons()),
        (DecisionSource::Rules, DecisionStatus::Effective, set(&["RC-0001"]))
    );
    assert_eq!(decision.versions.rule_set.as_ref().unwrap().id, "individual");
    assert_eq!(decision.versions.question_set.as_ref().unwrap().version, 1);
    assert!(decision.versions.exclusions.is_some() && decision.versions.reasons.is_some());
    let Some(Effect::IssuePolicy {
        key,
        expected_product_hash,
        migrated,
        command,
        ..
    }) = settled.effects.first()
    else {
        panic!("no issue")
    };
    assert_eq!((key.as_str(), expected_product_hash.as_str(), *migrated), ("P1/issue/1", "prodhash-1", false));
    assert_eq!(
        (command.policy.0.as_str(), command.inception, command.lives.len(), command.covers.len()),
        ("POL-P1", w.today(), 1, 1)
    );
    assert!(command.covers[0].loadings.is_empty() && command.covers[0].exclusions.is_empty());
    assert_eq!(command.covers[0].sum_insured_minor, 500_000_00);

    assert_eq!(
        w.err(
            "issuer",
            Command::IssueRejected {
                proposal: "P9".into(),
                code: "x".into()
            }
        ),
        "unknown_proposal"
    );
    let issued = w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
    );
    assert!(matches!(w.p("P1").status, ProposalStatus::Issued { .. }));
    assert!(matches!(&issued.effects[0], Effect::ConfirmCapacity { reservation, cover, .. } if reservation == "P1/L1/life/1" && cover == "life"));
    assert!(matches!(
        &issued.effects[1],
        Effect::RecordRiskFactors {
            smoker: Some(false),
            date_of_birth: Some(_),
            ..
        }
    ));
    assert!(matches!(&issued.effects[2], Effect::EndInterim { key, .. } if key == "P1/interim/end"));
    assert!(matches!(
        &issued.effects[3],
        Effect::Notify {
            kind: NoticeKind::PolicyIssued,
            ..
        }
    ));
    assert!(!w.p("P1").interim_in_force());
    assert_eq!(
        w.err(
            "issuer",
            Command::PolicyIssued {
                proposal: "P1".into(),
                layers: vec![],
            }
        ),
        "not_issuing"
    );
    assert_eq!(
        w.err(
            "issuer",
            Command::IssueRejected {
                proposal: "P1".into(),
                code: "x".into()
            }
        ),
        "not_issuing"
    );
    assert_eq!(w.err("new-business", Command::Withdraw { proposal: "P1".into() }), "closed");
    assert_eq!(w.err("issuer", Command::InterimIssued { proposal: "P9".into() }), "unknown_proposal");
}

/// Purpose: where the rules set terms themselves (a smoker's loading, an exclusion for a back
/// condition on trauma only) the terms are offered back with the reasons the person may be
/// told, and are issued only when the offer is accepted with a signed step: loadings as the
/// fraction the core rates with and exclusions as codes, each cover with its own terms.
/// Why it matters: one decision writes different terms per cover, and a loading or exclusion
/// the applicant never agreed to is not a contract. An unsigned or mis-signed acceptance must
/// not issue a policy.
#[test]
fn terms_set_by_the_rules_are_offered_and_issued_once_accepted() {
    let mut w = World::new();
    let open = w.open_command(
        "P1",
        &["L1"],
        &[("L1", "life", "life", 400_000_00), ("L1", "trauma", "trauma", 100_000_00)],
        false,
    );
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let answers = with(&[
        ("smoked_last_12_months", Answer::YesNo(true)),
        ("back_or_neck_last_5_years", Answer::YesNo(true)),
    ]);
    let disclosed = w.disclose("P1", "L1", &answers);
    assert_eq!(World::reservations(&disclosed).len(), 2);
    let offered = w.automatic(&disclosed);
    assert_eq!(w.stage("P1", "L1"), Stage::OfferOpen);
    let Some(Effect::Notify {
        kind: NoticeKind::OfferMade,
        reasons,
        to: Recipient::Party { .. },
        ..
    }) = offered.effects.first()
    else {
        panic!("no offer notice")
    };
    assert_eq!(reasons, &vec!["Your medical history.".to_string(), "Your smoking or vaping.".to_string()]);
    assert!(matches!(&offered.events[1], Event::OfferMade { expires_on, .. } if *expires_on == d(2026, 12, 11)));
    assert_eq!(w.p("P1").status, ProposalStatus::Open, "nothing is issued on terms nobody has accepted");

    let offer = w.p("P1").lives[&LifeId::from("L1")].open_offer().unwrap().id.clone();
    let respond = |signed, offer: &str| Command::RespondToOffer {
        proposal: "P1".into(),
        life: "L1".into(),
        offer: offer.into(),
        response: OfferResponse::Accept,
        signed,
    };
    assert_eq!(
        w.err("new-business", respond(signed("offer", &["P1", "L1", &offer, "decline"]), &offer)),
        "signature"
    );
    assert_eq!(
        w.err("new-business", respond(signed("disclosure", &["P1", "L1", &offer, "accept"]), &offer)),
        "signature"
    );
    assert_eq!(
        w.err("new-business", respond(signed("offer", &["P1", "L1", "other", "accept"]), "other")),
        "no_offer"
    );

    let accepted = w.respond("P1", "L1", OfferResponse::Accept);
    let Some(Effect::IssuePolicy { command, .. }) = accepted.effects.first() else {
        panic!("no issue")
    };
    let life = command.covers.iter().find(|c| c.code == "life").unwrap();
    let trauma = command.covers.iter().find(|c| c.code == "trauma").unwrap();
    assert_eq!(serde_json::to_value(&life.loadings).unwrap(), serde_json::json!(["0.50"]));
    assert!(life.exclusions.is_empty());
    assert_eq!(trauma.exclusions, vec!["EX-1001".to_string()]);
    let f = &w.p("P1").lives[&LifeId::from("L1")];
    assert_eq!(f.offers[0].status, OfferStatus::Accepted);
    let CoverOutcome::Accept { exclusions, .. } = f.decisions[0].terms_for("trauma").unwrap() else {
        panic!()
    };
    assert_eq!((exclusions[0].from, exclusions[0].until), (w.today(), None));
}

/// Purpose: an offer can end three other ways. Declined: the life is settled without cover,
/// capacity is released and the proposal ends. Left to expire: the same, on the first sweep
/// after its last day. Countered: the life goes back to a person, the earlier decision stays on
/// file as replaced, and a new decision makes a new offer.
/// Why it matters: capacity reserved for an offer nobody takes is capacity another applicant
/// cannot have, and a counter-offer must never lose the record of what was first decided.
#[test]
fn an_offer_can_be_declined_expire_or_be_countered() {
    let smoker = yes("smoked_last_12_months");
    // Declined.
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let disclosed = w.disclose("P1", "L1", &smoker);
    w.automatic(&disclosed);
    let declined = w.respond("P1", "L1", OfferResponse::Decline);
    assert!(matches!(&declined.effects[0], Effect::ReleaseCapacity { reservation, .. } if reservation == "P1/L1/life/1"));
    assert_eq!(w.stage("P1", "L1"), Stage::OfferDeclined);
    assert!(matches!(&w.p("P1").status, ProposalStatus::Lapsed { reason, .. } if reason == "RC-0912"));
    assert!(declined.events.iter().any(|e| matches!(e, Event::ProposalLapsed { .. })));

    // Expired.
    let mut w = World::new();
    w.open("P2", 500_000_00);
    let disclosed = w.disclose("P2", "L1", &smoker);
    w.automatic(&disclosed);
    w.days(30);
    w.ok("timer", Command::Sweep { after: None, max: 10 });
    assert_eq!(w.stage("P2", "L1"), Stage::OfferOpen, "the offer is open through its last day");
    w.days(1);
    let swept = w.ok("timer", Command::Sweep { after: None, max: 10 });
    assert_eq!(w.p("P2").lives[&LifeId::from("L1")].offers[0].status, OfferStatus::Expired);
    assert!(swept.effects.iter().any(|e| matches!(e, Effect::ReleaseCapacity { .. })));
    assert!(!w.p("P2").status.is_open());

    // Countered.
    let mut w = World::new();
    w.open("P3", 500_000_00);
    let disclosed = w.disclose("P3", "L1", &smoker);
    w.automatic(&disclosed);
    let countered = w.respond("P3", "L1", OfferResponse::Counter);
    assert!(countered.events.iter().any(|e| matches!(e, Event::Referred { .. })));
    assert_eq!(w.stage("P3", "L1"), Stage::InAssessment);
    let f = &w.p("P3").lives[&LifeId::from("L1")];
    assert_eq!((f.offers[0].status, f.decisions[0].status), (OfferStatus::Countered, DecisionStatus::Effective));
    assert!(f.open_case().unwrap().reasons.contains("RC-0920"));
    let case = w.take("P3", "L1", "amy");
    let again = w.propose(&case, "amy", vec![accept("life", 25, None)]).unwrap();
    assert!(
        World::reservations(&again).is_empty(),
        "the capacity already reserved for the same amount is kept"
    );
    let f = &w.p("P3").lives[&LifeId::from("L1")];
    assert_eq!(
        (f.decisions[0].status, f.decisions[1].status, f.stage),
        (DecisionStatus::Superseded, DecisionStatus::Effective, Stage::OfferOpen)
    );
    assert_eq!(f.offers.len(), 2);
    let accepted = w.respond("P3", "L1", OfferResponse::Accept);
    let Some(Effect::IssuePolicy { command, .. }) = accepted.effects.first() else {
        panic!("no issue")
    };
    assert_eq!(serde_json::to_value(&command.covers[0].loadings).unwrap(), serde_json::json!(["0.25"]));
}

/// Purpose: Tom Clarke's application is referred to a person, who accepts it with a 50%
/// loading. The offer goes to him with reasons in plain words, he accepts with a signed step,
/// and the core is sent the loading as the fraction it rates with and nothing else from the
/// file: no reason codes, no notes, no answers. The decision's reference is found from the
/// policy, life and cover, and the reasons and notes stay with the decision.
/// Why it matters: the core is read by servicing, payments and every channel. A reason or
/// clinical note in it would reach people who must not see it, while the loading must be there
/// for the premium to be right and the decision must still be findable at claim.
#[test]
fn a_referred_loading_reaches_the_core_as_a_rate_and_a_reference_only() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let referred = w.disclose("P1", "L1", &yes("hazardous_occupation"));
    assert!(referred.events.iter().any(|e| matches!(e, Event::Referred { .. })));
    assert!(w.p("P1").lives[&LifeId::from("L1")].decisions.is_empty(), "the rules decided nothing");
    let case = w.take("P1", "L1", "amy");
    let decided = w.propose(&case, "amy", vec![accept("life", 50, None)]).unwrap();
    let offered = w.automatic(&decided);
    let reasons = offered
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::Notify {
                kind: NoticeKind::OfferMade,
                to: Recipient::Party { .. },
                reasons,
                ..
            } => Some(reasons.clone()),
            _ => None,
        })
        .expect("an offer notice to Tom");
    assert!(
        !reasons.is_empty() && reasons.iter().all(|r| !r.starts_with("RC-") && r.ends_with('.')),
        "{reasons:?}"
    );

    let accepted = w.respond("P1", "L1", OfferResponse::Accept);
    let Some(Effect::IssuePolicy { command, .. }) = accepted.effects.first() else {
        panic!("no issue")
    };
    assert_eq!(serde_json::to_value(&command.covers[0].loadings).unwrap(), serde_json::json!(["0.50"]));
    let sent = serde_json::to_string(command).unwrap();
    for kept_back in ["RC-", "vault://", "hazardous", "amy"] {
        assert!(!sent.contains(kept_back), "{kept_back} reached the core: {sent}");
    }

    let policy = w.p("P1").policy.clone();
    let terms = w.s.terms_on(&policy, &LifeId::from("L1"), "life").expect("terms on the policy");
    assert_eq!((terms.decision.as_str(), terms.loading_percent), ("P1/L1/d1", 50));
    let decision = &w.p("P1").lives[&LifeId::from("L1")].decisions[0];
    assert!(decision.signatures[0].reasons.contains("RC-0101"));
    assert_eq!(decision.notes_ref.as_deref(), Some("vault://notes"));
}
