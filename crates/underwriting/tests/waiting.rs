//! An income cover's waiting period shortened when business cover is cancelled.

mod common;

use common::*;
use ic_core::policy::{LifeKind, Policy};
use ic_core::{LifeId, PolicyId};
use ic_underwriting::command::{Command, Effect, Event, OpenProposal};
use ic_underwriting::model::{CancelledBecause, DecisionSource, Entry, Facility, ProposalStatus, Stage};
use ic_underwriting::waiting::conversion;

fn request(w: &World, id: &str, cover: &str, cancelled_cover: &str) -> OpenProposal {
    let mut open = w.open_command(id, &["L1"], &[("L1", cover, "income_protection", 4_000_00)], false);
    open.product_code = "aia-living".into();
    open.entry = Entry::WaitingReduction {
        policy: open.policy.clone(),
        cancelled_policy: "POL-BUS".into(),
        cancelled_group: "grp-bus".into(),
        cancelled_life: "L1".into(),
        cancelled_cover: cancelled_cover.into(),
        why: CancelledBecause::StoppedTrading,
        evidence: "vault://ird/ceased".into(),
    };
    open
}

/// Purpose: a reduction request is one AIA Living income cover of one life on the policy it
/// is on, after Business Continuity or Rural Continuity cover on another policy was
/// cancelled, naming where the proof is kept; anything else is refused `waiting_reduction`. An
/// accepted request opens a financial case (RC-0982) and asks no health questions; once the
/// person holding it accepts the proof, it is decided with no underwriting (source `facility`,
/// waiting period reduction), reserves no reinsurance capacity, and goes to the core as terms
/// for the policy in force. Refused, it ends withdrawn.
/// Why it matters: the benefit is a shorter wait with no evidence, so it must be confined to
/// the covers and triggers in the wording; a shorter wait adds no amount, so reserving capacity
/// for it would count the cover against the treaty twice.
#[test]
fn a_reduction_is_decided_once_a_person_accepts_why_the_business_cover_ended() {
    let mut w = World::new();
    let refused = |w: &mut World, o: OpenProposal| w.err("new-business", Command::OpenProposal(Box::new(o)));
    let o = request(&w, "W0", "start-up-income", "business-continuity-agreed-value");
    assert_eq!(refused(&mut w, o), "waiting_reduction", "not an AIA Living income cover");
    let o = request(&w, "W0", "income-agreed-value", "life");
    assert_eq!(refused(&mut w, o), "waiting_reduction", "not business cover");
    let mut o = request(&w, "W0", "income-agreed-value", "rural-continuity");
    o.product_code = "aia-living-business".into();
    assert_eq!(refused(&mut w, o), "waiting_reduction");
    let mut o = request(&w, "W0", "income-agreed-value", "rural-continuity");
    if let Entry::WaitingReduction { evidence, .. } = &mut o.entry {
        *evidence = "".into();
    }
    assert_eq!(refused(&mut w, o), "waiting_reduction");
    let mut o = request(&w, "W0", "income-agreed-value", "rural-continuity");
    if let Entry::WaitingReduction { cancelled_policy, .. } = &mut o.entry {
        *cancelled_policy = "POL-W0".into();
    }
    assert_eq!(refused(&mut w, o), "waiting_reduction", "the business cover is on another policy");
    let mut o = request(&w, "W0", "income-agreed-value", "rural-continuity");
    o.policy = "POL-ELSEWHERE".into();
    assert_eq!(refused(&mut w, o), "waiting_reduction");

    let o = request(&w, "W1", "income-agreed-value", "business-continuity-agreed-value");
    w.ok("new-business", Command::OpenProposal(Box::new(o)));
    assert!(w.p("W1").lives[&LifeId::from("L1")].open_case().unwrap().reasons.contains("RC-0982"));
    let case = w.take("W1", "L1", "amy");
    let checked = w.ok(
        "underwriting-console",
        Command::CheckOptionEvidence {
            case: case.clone(),
            underwriter: "amy".into(),
            accepted: true,
            note: String::new(),
        },
    );
    assert!(checked.events.iter().any(|e| matches!(
        e,
        Event::FacilityChecked {
            facility: Facility::WaitingPeriodReduction,
            ..
        }
    )));
    assert!(World::reservations(&checked).is_empty(), "no capacity for a shorter wait");
    assert_eq!(w.stage("W1", "L1"), Stage::Accepted);
    let d = &w.p("W1").lives[&LifeId::from("L1")].decisions[0];
    assert!(matches!(
        d.source,
        DecisionSource::Facility {
            facility: Facility::WaitingPeriodReduction,
            ..
        }
    ));
    assert!(checked.effects.iter().any(|e| matches!(
        e,
        Effect::ApplyTerms {
            entry: Entry::WaitingReduction { .. },
            ..
        }
    )));

    let o = request(&w, "W2", "income-indemnity", "rural-continuity");
    w.ok("new-business", Command::OpenProposal(Box::new(o)));
    let case = w.take("W2", "L1", "amy");
    w.ok(
        "underwriting-console",
        Command::CheckOptionEvidence {
            case,
            underwriter: "amy".into(),
            accepted: false,
            note: "the business is still trading under a new name".into(),
        },
    );
    assert!(matches!(w.p("W2").status, ProposalStatus::Withdrawn { .. }));
}

fn policy(c: &ic_sim::Cluster, id: &str) -> Policy {
    c.state(0).policies[&PolicyId::from(id)].clone()
}

/// Purpose: the core's conversion is worked out from both policies as the core holds them: the
/// income cover's choices with only the wait changed to the cancelled cover's, for the lesser
/// of the two benefits, naming the proof. It is refused while the business cover is still in
/// force (`waiting_cover_not_cancelled`), when the business cover is someone else's
/// (`waiting_not_the_same_life`), when the policy does not hold the named business cover or
/// the income cover (`waiting_cover_not_held`, `waiting_income_not_held`), when the business
/// cover has no end or no wait the core can show, when the life is not a named person, and for
/// any other entry.
/// Why it matters: the screens say which covers; the core holds what they are. A wait taken
/// from a cover that is still running, or from another person's business cover, would give
/// a shorter wait the wording does not allow.
#[test]
fn the_conversion_is_worked_out_from_both_policies_in_the_core() {
    let mut c = ledger(&[
        ("LIV", "aia-living", "income-agreed-value", "waiting=104w,period=to-65", 2_500_00, "PTY-L1"),
        (
            "BUS",
            "aia-living-business",
            "business-continuity-agreed-value",
            "waiting=8w",
            3_000_00,
            "PTY-L1",
        ),
        (
            "OTHER",
            "aia-living-business",
            "business-continuity-agreed-value",
            "waiting=8w",
            3_000_00,
            "PTY-L2",
        ),
    ]);
    let entry = |cancelled: &str, cover: &str| Entry::WaitingReduction {
        policy: "LIV".into(),
        cancelled_policy: cancelled.into(),
        cancelled_group: "grp".into(),
        cancelled_life: "L1".into(),
        cancelled_cover: cover.into(),
        why: CancelledBecause::LeftBusiness,
        evidence: "vault://proof".into(),
    };
    let today = d(2027, 2, 1);
    let l1 = LifeId::from("L1");
    let work = |c: &ic_sim::Cluster, e: &Entry, cancelled: &str, income_cover: &str| {
        conversion(e, &policy(c, "LIV"), &policy(c, cancelled), income_cover, &l1, "W1/terms", today)
    };
    let bc = entry("BUS", "business-continuity-agreed-value");
    assert_eq!(work(&c, &bc, "BUS", "income-agreed-value"), Err("waiting_cover_not_cancelled".into()));
    assert_eq!(
        conversion(
            &Entry::NewBusiness,
            &policy(&c, "LIV"),
            &policy(&c, "BUS"),
            "income-agreed-value",
            &l1,
            "r",
            today
        ),
        Err("not_a_waiting_reduction".into())
    );

    c.end_of_day().unwrap();
    for id in ["BUS", "OTHER"] {
        c.submit(0, "policy-servicing", ic_core::Command::CancelPolicy { policy: PolicyId::from(id) })
            .unwrap();
    }
    let converted = work(&c, &bc, "BUS", "income-agreed-value").unwrap();
    assert_eq!(
        (
            converted.cover.as_str(),
            converted.to_cover.as_str(),
            converted.sum_insured_minor,
            converted.reference.as_str(),
            converted.evidence.as_deref()
        ),
        ("income-agreed-value", "income-agreed-value", 2_500_00, "W1/terms", Some("vault://proof")),
        "the lesser benefit is the income cover's here"
    );
    assert!(converted.excess_option.contains("waiting=8w") && converted.excess_option.contains("period=to-65"));
    assert!(!converted.excess_option.contains("104w"));

    assert_eq!(
        work(&c, &entry("OTHER", "business-continuity-agreed-value"), "OTHER", "income-agreed-value"),
        Err("waiting_not_the_same_life".into())
    );
    assert_eq!(
        work(&c, &entry("BUS", "rural-continuity"), "BUS", "income-agreed-value"),
        Err("waiting_cover_not_held".into())
    );
    assert_eq!(work(&c, &bc, "BUS", "income-indemnity"), Err("waiting_income_not_held".into()));

    // A business cover with no end the core can date, or with no wait on it, says nothing
    // about what to shorten to; a life the core holds by member reference is no named person.
    let mut undated = policy(&c, "BUS");
    let cover = undated.covers.get_mut("business-continuity-agreed-value").unwrap();
    cover.ended_on = None;
    cover.layers.iter_mut().for_each(|l| l.end = None);
    let livs = policy(&c, "LIV");
    assert_eq!(
        conversion(&bc, &livs, &undated, "income-agreed-value", &l1, "r", today),
        Err("waiting_cover_not_held".into())
    );
    let mut no_wait = policy(&c, "BUS");
    no_wait
        .covers
        .get_mut("business-continuity-agreed-value")
        .unwrap()
        .layers
        .iter_mut()
        .for_each(|l| l.excess_option = None);
    assert_eq!(
        conversion(&bc, &livs, &no_wait, "income-agreed-value", &l1, "r", today),
        Err("waiting_cover_not_held".into())
    );
    let mut member = policy(&c, "LIV");
    member.lives.get_mut(&l1).unwrap().kind = LifeKind::UnnamedMember {
        member_ref: "m1".into(),
        linked_party: None,
    };
    assert_eq!(
        conversion(&bc, &member, &policy(&c, "BUS"), "income-agreed-value", &l1, "r", today),
        Err("waiting_not_the_same_life".into())
    );
}
