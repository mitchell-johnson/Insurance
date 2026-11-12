//! How proposals begin and end, and the guarantees of the state machine
//! itself: idempotency, callers, no trace of a refused command, replay.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use ic_core::{LifeId, PolicyId};
use ic_log::StateMachine;
use ic_underwriting::command::{Applied, CapacityAnswer, Command, Effect, Envelope, Event, NoticeKind, OfferResponse, SigningProof, View};
use ic_underwriting::evidence::EvidenceStatus;
use ic_underwriting::model::{AccessEnd, ConsentKind, DecisionStatus, Entry, OfferStatus, ProposalStatus, QuoteBinding, Stage};
use ic_underwriting::prepare::{age_on, prepare_disclosure, PrepareError};
use ic_underwriting::questions::{Answer, AnswerProblem};
use ic_underwriting::rules::Disposition;
use ic_underwriting::UnderwritingState;

/// Purpose: a proposal is refused at the door if it repeats an id, has no lives or covers, names
/// a life twice, has a cover with no life, no amount or an unknown type, leaves a life without
/// cover, claims a legacy received date without being a migration, or is for a product with no
/// rules in force.
/// Why it matters: everything after intake assumes the proposal is well formed. A cover with
/// no life, or a product with no rules, would otherwise surface as a stuck application.
#[test]
fn a_malformed_proposal_is_refused_at_the_door() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let cover = ("L1", "life", "life", 500_000_00);
    let open = |w: &World, lives: &[&str], covers: &[(&str, &str, &str, i64)]| w.open_command("P2", lives, covers, false);
    let send = |w: &mut World, o| w.err("new-business", Command::OpenProposal(Box::new(o)));
    let again = w.open_command("P1", &["L1"], &[cover], false);
    assert_eq!(send(&mut w, again), "duplicate_proposal");
    let o = w.open_command("P2", &["L1"], &[cover], false);
    assert_eq!(w.err("migration", Command::OpenProposal(Box::new(o))), "received_on");
    let mut o = open(&w, &["L1"], &[cover]);
    o.legacy_received_on = Some(d(2026, 1, 1));
    assert_eq!(send(&mut w, o), "received_on");
    let mut o = open(&w, &["L1"], &[cover]);
    o.lives.clear();
    assert_eq!(send(&mut w, o), "lives");
    let o = open(&w, &["L1", "L1"], &[cover]);
    assert_eq!(send(&mut w, o), "lives");
    for covers in [
        vec![],
        vec![cover, cover],
        vec![("L9", "life", "life", 1)],
        vec![("L1", "life", "life", 0)],
        vec![("L1", "life", "pets", 1)],
    ] {
        let o = open(&w, &["L1"], &covers);
        assert_eq!(send(&mut w, o), "covers");
    }
    let o = open(&w, &["L1", "L2"], &[cover]);
    assert_eq!(send(&mut w, o), "covers", "a life with no cover");
    let mut o = open(&w, &["L1"], &[cover]);
    o.product_code = "funeral".into();
    assert_eq!(send(&mut w, o), "no_rules");
    // With nothing activated, nothing can be proposed.
    let mut empty = World {
        s: UnderwritingState::default(),
        index: 0,
        now: w.now,
        log: vec![],
    };
    let o = open(&w, &["L1"], &[cover]);
    assert_eq!(empty.err("new-business", Command::OpenProposal(Box::new(o))), "no_rules");
}

/// Purpose: a disclosure is accepted only if it answers the question set and was assessed with
/// the rule set in force for the contract date, carries its hash and where it is sealed, is
/// signed by the life's device key over that hash, and has one assessment per cover naming only
/// known codes. Legacy evidence of signing is for migrated proposals only.
/// Why it matters: the disclosure is the evidence a non-disclosure finding rests on years
/// later. One that was not signed, or was assessed under the wrong rules, is worthless then.
#[test]
fn a_disclosure_must_be_signed_and_made_under_the_versions_in_force() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let good = w.disclosure("P1", "L1", &clean());
    let send = |w: &mut World, d| w.err("new-business", Command::SubmitDisclosure(Box::new(d)));
    let mut bad = good.clone();
    bad.question_set.version = 2;
    assert_eq!(send(&mut w, bad), "question_set");
    let mut bad = good.clone();
    bad.rule_set.content_hash = "x".into();
    assert_eq!(send(&mut w, bad), "rule_set");
    let mut bad = good.clone();
    bad.vault_ref = String::new();
    assert_eq!(send(&mut w, bad), "disclosure");
    let mut bad = good.clone();
    bad.hash = "abc".into();
    assert_eq!(send(&mut w, bad), "disclosure");
    let mut bad = good.clone();
    bad.signing = SigningProof::Device(signed("disclosure", &["P1", "L1", &"0".repeat(64)]));
    assert_eq!(send(&mut w, bad), "signature");
    let mut bad = good.clone();
    bad.signing = SigningProof::Migrated { legacy_ref: "L-1".into() };
    assert_eq!(send(&mut w, bad), "signature");
    let mut bad = good.clone();
    bad.assessments.clear();
    assert_eq!(send(&mut w, bad), "assessments");
    let mut bad = good.clone();
    bad.assessments.get_mut("life").unwrap().reasons.insert("RC-7777".into());
    assert_eq!(send(&mut w, bad), "unknown_code");
    let mut bad = good.clone();
    bad.assessments.get_mut("life").unwrap().exclusions.insert("EX-7777".into());
    assert_eq!(send(&mut w, bad), "unknown_code");
    let mut bad = good.clone();
    bad.entered_by_adviser = 2;
    assert_eq!(send(&mut w, bad), "adviser_not_permitted");
    let mut bad = good.clone();
    bad.life = "L9".into();
    assert_eq!(send(&mut w, bad), "unknown_life");
    let mut bad = good.clone();
    bad.proposal = "P9".into();
    assert_eq!(send(&mut w, bad), "unknown_proposal");

    w.ok("new-business", Command::SubmitDisclosure(Box::new(good.clone())));
    assert_eq!(send(&mut w, good), "not_awaiting_disclosure");
    w.ok("new-business", Command::Withdraw { proposal: "P1".into() });
    let hash = w.p("P1").lives[&LifeId::from("L1")].disclosures[0].hash.clone();
    let mut confirm = w.disclosure("P1", "L1", &clean());
    confirm.hash = hash;
    assert_eq!(w.err("new-business", Command::ConfirmInterview(Box::new(confirm))), "no_interview");

    // A proposal that came across mid-flight keeps its received date and its legacy signing.
    let mut o = w.open_command("M1", &["L1"], &[("L1", "life", "life", 100_000_00)], false);
    o.legacy_received_on = Some(d(2026, 10, 1));
    let opened = w.ok("migration", Command::OpenProposal(Box::new(o)));
    assert!(opened.effects.is_empty(), "no new interim cover for a migrated proposal");
    assert_eq!(
        (w.p("M1").received_on, w.p("M1").expires_on, w.p("M1").migrated),
        (d(2026, 10, 1), d(2026, 12, 30), true)
    );
    let mut legacy = w.disclosure("M1", "L1", &clean());
    legacy.signing = SigningProof::Migrated { legacy_ref: "".into() };
    assert_eq!(w.err("migration", Command::SubmitDisclosure(Box::new(legacy.clone()))), "signature");
    legacy.signing = SigningProof::Migrated { legacy_ref: "LEG-55".into() };
    assert_eq!(
        send(&mut w, legacy.clone()),
        "signature",
        "only the migration itself may vouch for legacy signing"
    );
    let disclosed = w.ok("migration", Command::SubmitDisclosure(Box::new(legacy)));
    let issued = w.automatic(&disclosed);
    assert!(matches!(&issued.effects[0], Effect::IssuePolicy { migrated: true, .. }));
}

/// Purpose: the driver-side preparation checks answers against the question set, builds the
/// record whose hash is signed, marks answers the adviser typed, and runs the rules with the
/// life's age on the contract date and the cover the insurer already holds.
/// Why it matters: the state machine trusts the assessments it is given to have come from this
/// function. An age computed on the wrong date or existing cover left out is a mis-assessment.
#[test]
fn preparation_checks_answers_and_runs_the_rules() {
    let mut w = World::new();
    w.open("P1", 1_000_000_00);
    let life = LifeId::from("L1");
    let none = BTreeSet::new();
    let p = w.p("P1").clone();
    let cfg = &w.s.config;
    assert_eq!(
        prepare_disclosure(cfg, &p, &LifeId::from("L9"), &clean(), &none, true),
        Err(PrepareError::UnknownLife(LifeId::from("L9")))
    );
    let mut short = clean();
    short.remove("diabetes");
    assert_eq!(
        prepare_disclosure(cfg, &p, &life, &short, &none, true),
        Err(PrepareError::Answers(vec![AnswerProblem::Missing { question: "diabetes".into() }]))
    );
    assert!(
        prepare_disclosure(cfg, &p, &life, &short, &none, false).is_ok(),
        "a supplement need not be complete"
    );
    let mut other = p.clone();
    other.product_code = "funeral".into();
    assert_eq!(prepare_disclosure(cfg, &other, &life, &clean(), &none, true), Err(PrepareError::NoRules));
    let mut undated = p.clone();
    undated.lives.get_mut(&life).unwrap().factors.date_of_birth = None;
    assert_eq!(
        prepare_disclosure(cfg, &undated, &life, &clean(), &none, true),
        Err(PrepareError::NoDateOfBirth)
    );
    assert_eq!(PrepareError::NoRules.to_string(), "no question set or rules are in force for the contract date");

    let typed = BTreeSet::from(["annual_income".to_string(), "not_a_question".to_string()]);
    let prepared = prepare_disclosure(cfg, &p, &life, &clean(), &typed, true).unwrap();
    assert_eq!((prepared.entered_by_adviser, prepared.hash.len()), (1, 64));
    let record: serde_json::Value = serde_json::from_slice(&prepared.record).unwrap();
    assert_eq!(record["answers"]["annual_income"]["entered_by_adviser"], true);
    assert_eq!(record["answers"]["diabetes"]["entered_by_adviser"], false);
    assert_ne!(
        prepared.hash,
        prepare_disclosure(cfg, &p, &life, &clean(), &none, true).unwrap().hash,
        "who typed an answer is part of the record"
    );
    assert_eq!(prepared.assessments["life"].disposition(), Disposition::Clean);

    // Cover already held counts, whether or not the applicant was shown it.
    w.ok(
        "new-business",
        Command::PrefillExistingCover {
            proposal: "P1".into(),
            life: "L1".into(),
            matched: true,
            for_applicant: false,
            rows: vec![ic_underwriting::model::ExistingCover {
                policy: PolicyId::from("POL-OLD"),
                partition: 3,
                cover_type: "life".into(),
                sum_insured_minor: 600_000_00,
            }],
            reflects: BTreeMap::from([(3, 991)]),
        },
    );
    let prepared = w.prepared("P1", "L1", &clean());
    assert_eq!(prepared.assessments["life"].disposition(), Disposition::NeedsEvidence);
    assert_eq!(
        (
            age_on(d(1986, 5, 1), d(2026, 4, 30)),
            age_on(d(1986, 5, 1), d(2026, 5, 1)),
            age_on(d(2030, 1, 1), d(2026, 1, 1))
        ),
        (39, 40, 0)
    );
    let _ = Answer::YesNo(true);
}

/// Purpose: a proposal has a life. On the first sweep after its last day it lapses with notice
/// and nothing is left running: cases closed, outstanding evidence cancelled, capacity
/// released, interim cover ended. Withdrawal does the same at once. A sweep works through a
/// bounded number of proposals and says where to carry on.
/// Why it matters: an expired proposal that keeps its capacity, its interim cover or an open
/// evidence request costs money and exposes the insurer with no contract behind it.
#[test]
fn a_proposal_lapses_or_is_withdrawn_and_leaves_nothing_running() {
    let mut w = World::new();
    let settings = ic_underwriting::config::Settings {
        version: 2,
        interim: Some(ic_underwriting::config::InterimSettings {
            product_code: "interim".into(),
            cover_code: "ad".into(),
            max_sum_insured_minor: 100_000_00,
        }),
        ..Default::default()
    };
    w.ok(
        "config",
        Command::ActivateSettings {
            settings: Box::new(settings),
            approval: approval(),
        },
    );
    w.open("P1", 500_000_00);
    w.consent("P1", "L1", ConsentKind::ObtainRecords);
    w.disclose("P1", "L1", &yes("diabetes"));
    w.open("P2", 500_000_00);
    let pending = w.disclose("P2", "L1", &clean());
    assert_eq!(World::reservations(&pending).len(), 1);
    w.open("P3", 500_000_00);
    w.disclose("P3", "L1", &yes("heart_condition"));

    assert_eq!(w.err("timer", Command::Sweep { after: None, max: 0 }), "max");
    assert_eq!(w.err("timer", Command::Sweep { after: None, max: 1001 }), "max");
    w.days(90);
    w.ok(
        "evidence",
        Command::EvidenceReceived {
            request: "P1/L1/blood_test".into(),
            vault_ref: "v".into(),
            cost_minor: 0,
        },
    );
    let quiet = w.ok("timer", Command::Sweep { after: None, max: 10 });
    assert!(
        matches!(quiet.view, Some(View::Sweep { lapsed: 0, next: None, .. })),
        "open through the last day"
    );
    w.days(1);
    let first = w.ok("timer", Command::Sweep { after: None, max: 2 });
    assert_eq!(
        first.view,
        Some(View::Sweep {
            next: Some("P2".into()),
            lapsed: 2,
            chased: 0,
            escalated: 0
        })
    );
    assert!(first
        .effects
        .iter()
        .any(|e| matches!(e, Effect::ReleaseCapacity { reservation, reason } if reservation == "P2/L1/life/1" && reason == "proposal_ended")));
    assert_eq!(first.effects.iter().filter(|e| matches!(e, Effect::EndInterim { .. })).count(), 2);
    let notices: Vec<_> = first
        .effects
        .iter()
        .filter_map(|e| {
            if let Effect::Notify {
                kind: NoticeKind::ProposalLapsed,
                reasons,
                ..
            } = e
            {
                Some(reasons.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        notices,
        vec![vec!["The application ran out of time before it could be completed.".to_string()]; 2]
    );
    assert!(
        first.events.iter().any(|e| matches!(e, Event::CaseClosed { .. })),
        "the evidence case on P1 is closed"
    );
    let second = w.ok(
        "timer",
        Command::Sweep {
            after: Some("P2".into()),
            max: 2,
        },
    );
    assert!(matches!(second.view, Some(View::Sweep { next: None, lapsed: 1, .. })));
    for id in ["P1", "P2", "P3"] {
        assert!(matches!(&w.p(id).status, ProposalStatus::Lapsed { reason, .. } if reason == "RC-0910"));
        assert!(w.p(id).lives[&LifeId::from("L1")].open_case().is_none() && w.p(id).lives[&LifeId::from("L1")].capacity.is_empty());
    }
    assert_eq!(w.p("P2").lives[&LifeId::from("L1")].decisions[0].status, DecisionStatus::Superseded);
    assert!(
        matches!(w.ok("timer", Command::Sweep { after: None, max: 10 }).view, Some(View::Sweep { lapsed: 0, .. })),
        "a lapsed proposal is not swept again"
    );
    assert_eq!(
        w.err(
            "reinsurance",
            Command::CapacityAnswered {
                reservation: "P2/L1/life/1".into(),
                answer: CapacityAnswer::Automatic
            }
        ),
        "stale_reservation"
    );
    assert_eq!(w.err("new-business", Command::Withdraw { proposal: "P1".into() }), "closed");
    assert_eq!(
        w.err(
            "new-business",
            Command::Chaser {
                proposal: "P1".into(),
                life: "L1".into(),
                from: ic_underwriting::case::ChaserFrom::Customer
            }
        ),
        "no_case"
    );

    // Withdrawal: at once, with evidence cancelled and the offer withdrawn.
    w.open("P4", 500_000_00);
    w.consent("P4", "L1", ConsentKind::ObtainRecords);
    w.disclose("P4", "L1", &yes("diabetes"));
    let withdrawn = w.ok("new-business", Command::Withdraw { proposal: "P4".into() });
    assert!(matches!(&withdrawn.events[0], Event::ProposalWithdrawn { .. }));
    assert!(withdrawn
        .effects
        .iter()
        .any(|e| matches!(e, Effect::CancelEvidence { request } if request == "P4/L1/blood_test")));
    assert!(withdrawn.effects.iter().any(|e| matches!(
        e,
        Effect::Notify {
            kind: NoticeKind::ProposalWithdrawn,
            ..
        }
    )));
    assert_eq!(w.p("P4").lives[&LifeId::from("L1")].evidence[0].status, EvidenceStatus::Cancelled);
    assert!(w.p("P4").interim.as_ref().unwrap().ended);
    w.open("P5", 500_000_00);
    let disclosed = w.disclose("P5", "L1", &yes("smoked_last_12_months"));
    w.automatic(&disclosed);
    w.ok("new-business", Command::Withdraw { proposal: "P5".into() });
    assert_eq!(w.p("P5").lives[&LifeId::from("L1")].offers[0].status, OfferStatus::Withdrawn);
    for command in [
        Command::RecordConsent {
            proposal: "P5".into(),
            life: "L1".into(),
            kind: ConsentKind::ObtainRecords,
            granted: true,
            signed: None,
        },
        Command::PrefillExistingCover {
            proposal: "P5".into(),
            life: "L1".into(),
            matched: true,
            for_applicant: false,
            rows: vec![],
            reflects: BTreeMap::new(),
        },
    ] {
        assert_eq!(w.err("new-business", command), "closed");
    }
}

/// Purpose: each life on a joint application has its own disclosure and decision. When one is
/// declined and the other accepted, the contract is no longer the one applied for, so the
/// accepted life is asked whether they still want it; only then is a policy issued, for that
/// life alone. When every life's offer is turned down the proposal ends as not taken up.
/// Why it matters: issuing a single-life policy at a different premium because a partner was
/// declined, without asking, is a contract nobody agreed to.
#[test]
fn a_joint_application_is_settled_life_by_life() {
    let mut w = World::new();
    let open = w.open_command(
        "P1",
        &["L1", "L2"],
        &[("L1", "life", "life", 400_000_00), ("L2", "life", "life", 400_000_00)],
        false,
    );
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let first = w.disclose("P1", "L1", &clean());
    w.automatic(&first);
    assert_eq!(
        (w.stage("P1", "L1"), w.p("P1").status.clone()),
        (Stage::Accepted, ProposalStatus::Open),
        "waits for the other life"
    );
    w.disclose("P1", "L2", &yes("heart_condition"));
    let case = w.take("P1", "L2", "sam");
    w.propose(&case, "sam", vec![decline("life")]).unwrap();
    w.take("P1", "L2", "sue");
    let declined = w.ok(
        "underwriting-console",
        Command::Countersign {
            case,
            underwriter: "sue".into(),
            agree: true,
            reasons: set(&["RC-0101"]),
            withheld_reasons: BTreeSet::new(),
        },
    );
    assert!(declined
        .events
        .iter()
        .any(|e| matches!(e, Event::OfferMade { life, .. } if life == &LifeId::from("L1"))));
    assert_eq!(
        (w.stage("P1", "L1"), w.stage("P1", "L2"), w.p("P1").status.clone()),
        (Stage::OfferOpen, Stage::Declined, ProposalStatus::Open)
    );
    let accepted = w.respond("P1", "L1", OfferResponse::Accept);
    let Some(Effect::IssuePolicy { command, .. }) = accepted.effects.first() else {
        panic!("no issue")
    };
    assert_eq!(
        (command.lives.len(), command.lives[0].id.clone(), command.covers.len()),
        (1, LifeId::from("L1"), 1)
    );
    let issued = w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
    );
    assert_eq!(issued.effects.iter().filter(|e| matches!(e, Effect::RecordRiskFactors { .. })).count(), 1);

    // Both offered terms; one accepts, one declines: asked again; then neither wants it.
    let mut w = World::new();
    let open = w.open_command(
        "P2",
        &["L1", "L2"],
        &[("L1", "life", "life", 400_000_00), ("L2", "life", "life", 400_000_00)],
        false,
    );
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    for life in ["L1", "L2"] {
        let disclosed = w.disclose("P2", life, &yes("smoked_last_12_months"));
        w.automatic(&disclosed);
    }
    w.respond("P2", "L1", OfferResponse::Accept);
    assert_eq!(w.p("P2").status, ProposalStatus::Open);
    let other = w.respond("P2", "L2", OfferResponse::Decline);
    assert!(
        !other.effects.iter().any(|e| matches!(e, Effect::IssuePolicy { .. })),
        "L1 accepted these terms for a joint policy, not for one alone"
    );
    assert_eq!((w.stage("P2", "L1"), w.p("P2").lives[&LifeId::from("L1")].offers.len()), (Stage::OfferOpen, 2));
    let neither = w.respond("P2", "L1", OfferResponse::Decline);
    assert!(neither.events.iter().any(|e| matches!(e, Event::ProposalLapsed { .. })));
    assert!(matches!(&w.p("P2").status, ProposalStatus::Lapsed { reason, .. } if reason == "RC-0912"));
}

/// Purpose: the core can refuse the issue, for example when the product version the quote was
/// bound to is no longer the one it holds. The proposal waits: a manager is told, a person can
/// retry, or the applicant accepts a new quote with a signed step and the issue is sent again
/// under a new key. Other entry points end as terms applied to the policy already in force.
/// Why it matters: the quoted terms are the issued terms. A product change must never move an
/// applicant to a new price silently, and a retried issue must never create two policies.
#[test]
fn a_refused_issue_waits_for_a_person_or_a_new_quote() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let disclosed = w.disclose("P1", "L1", &clean());
    w.automatic(&disclosed);
    assert_eq!(
        w.err(
            "underwriting-console",
            Command::RetryIssue {
                proposal: "P1".into(),
                underwriter: "amy".into()
            }
        ),
        "not_failed"
    );
    assert_eq!(
        w.err("new-business", Command::Withdraw { proposal: "P1".into() }),
        "closed",
        "the issue is with the core"
    );
    let refused = w.ok(
        "issuer",
        Command::IssueRejected {
            proposal: "P1".into(),
            code: "product_version_changed".into(),
        },
    );
    assert!(matches!(
        &refused.effects[0],
        Effect::Notify {
            to: ic_underwriting::command::Recipient::UnderwritingManager,
            kind: NoticeKind::IssueFailed,
            ..
        }
    ));
    assert_eq!(
        w.p("P1").status,
        ProposalStatus::IssueFailed {
            code: "product_version_changed".into()
        }
    );

    assert_eq!(
        w.err(
            "underwriting-console",
            Command::RetryIssue {
                proposal: "P1".into(),
                underwriter: "nobody".into()
            }
        ),
        "no_profile"
    );
    let retried = w.ok(
        "underwriting-console",
        Command::RetryIssue {
            proposal: "P1".into(),
            underwriter: "amy".into(),
        },
    );
    assert!(matches!(&retried.effects[0], Effect::IssuePolicy { key, .. } if key == "P1/issue/2"));
    w.ok(
        "issuer",
        Command::IssueRejected {
            proposal: "P1".into(),
            code: "product_version_changed".into(),
        },
    );

    let quote = QuoteBinding {
        product_hash: "prodhash-2".into(),
        rate_tables: vec![],
        premium_minor: 91_00,
        quoted_on: w.today(),
    };
    let requote = |signed| Command::Requote {
        proposal: "P1".into(),
        quote: quote.clone(),
        signed,
    };
    assert_eq!(w.err("new-business", requote(signed("requote", &["P1", "prodhash-2", "8500"]))), "signature");
    let reissued = w.ok("new-business", requote(signed("requote", &["P1", "prodhash-2", "9100"])));
    assert!(
        matches!(&reissued.effects[0], Effect::IssuePolicy { key, expected_product_hash, .. } if key == "P1/issue/3" && expected_product_hash == "prodhash-2")
    );
    assert_eq!(w.err("new-business", requote(signed("requote", &["P1", "prodhash-2", "9100"]))), "closed");
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
    );

    // A new quote while still open simply rebinds; a failed issue can also be withdrawn.
    w.open("P2", 500_000_00);
    let mut q2 = quote.clone();
    q2.product_hash = "prodhash-3".into();
    let rebound = w.ok(
        "new-business",
        Command::Requote {
            proposal: "P2".into(),
            quote: q2,
            signed: signed("requote", &["P2", "prodhash-3", "9100"]),
        },
    );
    assert!(rebound.effects.is_empty());
    let disclosed = w.disclose("P2", "L1", &clean());
    w.automatic(&disclosed);
    w.ok(
        "issuer",
        Command::IssueRejected {
            proposal: "P2".into(),
            code: "x".into(),
        },
    );
    w.ok("new-business", Command::Withdraw { proposal: "P2".into() });
    assert!(matches!(w.p("P2").status, ProposalStatus::Withdrawn { .. }));

    // An increase on a policy in force ends as terms applied to it, with no interim cover.
    for (id, entry) in [
        (
            "P3",
            Entry::Increase {
                policy: PolicyId::from("POL-P3"),
            },
        ),
        (
            "P4",
            Entry::Reinstatement {
                policy: PolicyId::from("POL-P4"),
            },
        ),
        (
            "P5",
            Entry::Alteration {
                policy: PolicyId::from("POL-P5"),
            },
        ),
        (
            "P6",
            Entry::GroupExcess {
                policy: PolicyId::from("POL-P6"),
            },
        ),
    ] {
        let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 200_000_00)], false);
        assert_eq!(entry.policy(), Some(&open.policy));
        open.entry = entry.clone();
        let opened = w.ok("new-business", Command::OpenProposal(Box::new(open)));
        assert!(opened.effects.is_empty());
        let disclosed = w.disclose(id, "L1", &clean());
        // A change of terms alone, or cover coming back from a lapse, reserves no capacity:
        // the cover is already ceded, so the terms are ready as soon as they are decided.
        let alteration = matches!(entry, Entry::Alteration { .. } | Entry::Reinstatement { .. });
        assert_eq!(World::reservations(&disclosed).is_empty(), alteration);
        let applied = if alteration { disclosed } else { w.automatic(&disclosed) };
        assert!(
            matches!(&applied.effects[0], Effect::ApplyTerms { entry: e, covers, effective, .. } if e == &entry && covers.len() == 1 && *effective == w.today())
        );
    }
    assert_eq!(Entry::NewBusiness.policy(), None);
}

/// Purpose: the core refuses cover written for today while its billing is behind. That refusal
/// tells nobody: the next day's sweep sends the terms again under a new key, and the proposal
/// issues when the core takes them. If the core goes on refusing, the sweep stops after five
/// attempts in all, and also once the proposal's terms are out of date, and only then is a
/// manager told.
/// Why it matters: billing is behind for a few hours every day, between the date moving on and
/// the end of day running. Waiting for a person each time leaves an increase the customer
/// accepted unwritten for days; retrying without end hides a core that never catches up.
#[test]
fn terms_refused_while_billing_is_behind_are_sent_again_by_the_sweep() {
    let mut w = World::new();
    let mut open = w.open_command("P1", &["L1"], &[("L1", "life", "life", 200_000_00)], false);
    open.entry = Entry::Increase {
        policy: PolicyId::from("POL-P1"),
    };
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let disclosed = w.disclose("P1", "L1", &clean());
    w.automatic(&disclosed);
    let behind = || Command::IssueRejected {
        proposal: "P1".into(),
        code: "billing_behind".into(),
    };
    let sweep = |w: &mut World| w.ok("timer", Command::Sweep { after: None, max: 100 });
    let told = |a: &Applied| {
        a.effects.iter().any(|e| {
            matches!(
                e,
                Effect::Notify {
                    kind: NoticeKind::IssueFailed,
                    ..
                }
            )
        })
    };
    let sent_again = |a: &Applied| {
        a.effects
            .iter()
            .filter_map(|e| match e {
                Effect::ApplyTerms { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    let refused = w.ok("issuer", behind());
    assert!(!told(&refused), "a refusal for billing behind tells nobody");
    w.days(1);
    let swept = sweep(&mut w);
    assert_eq!(sent_again(&swept), ["P1/issue/2"]);
    assert_eq!(w.p("P1").status, ProposalStatus::Issuing);
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
    );
    assert!(matches!(w.p("P1").status, ProposalStatus::Issued { .. }));

    // A core that never catches up: four more tries, then a person.
    let mut open = w.open_command("P2", &["L1"], &[("L1", "life", "life", 200_000_00)], false);
    open.entry = Entry::Increase {
        policy: PolicyId::from("POL-P2"),
    };
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let disclosed = w.disclose("P2", "L1", &clean());
    w.automatic(&disclosed);
    let behind = || Command::IssueRejected {
        proposal: "P2".into(),
        code: "billing_behind".into(),
    };
    for attempt in 2..=5 {
        w.ok("issuer", behind());
        w.days(1);
        assert_eq!(sent_again(&sweep(&mut w)), [format!("P2/issue/{attempt}")]);
    }
    w.ok("issuer", behind());
    w.days(1);
    let gave_up = sweep(&mut w);
    assert!(sent_again(&gave_up).is_empty() && told(&gave_up));
    assert!(matches!(&w.p("P2").status, ProposalStatus::IssueFailed { code } if code == "billing_behind_retries_exhausted"));
    assert!(sweep(&mut w).effects.is_empty(), "told once");

    // Out of date terms are not sent again however few the attempts.
    let mut open = w.open_command("P3", &["L1"], &[("L1", "life", "life", 200_000_00)], false);
    open.entry = Entry::Increase {
        policy: PolicyId::from("POL-P3"),
    };
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let disclosed = w.disclose("P3", "L1", &clean());
    w.automatic(&disclosed);
    w.ok(
        "issuer",
        Command::IssueRejected {
            proposal: "P3".into(),
            code: "billing_behind".into(),
        },
    );
    let expires = w.p("P3").expires_on;
    while w.today() <= expires {
        w.days(1);
    }
    let late = sweep(&mut w);
    assert!(sent_again(&late).iter().all(|k| !k.starts_with("P3/")) && told(&late));
}

/// Purpose: the state machine's own guarantees. A repeated key returns the original result and
/// changes nothing; a key is required; each command is accepted only from its own driver; a
/// refused command leaves the state exactly as it was; the same log replayed gives the same
/// state; and the state survives being saved and restored as JSON.
/// Why it matters: the module runs as a replicated group with at-least-once delivery. Replicas
/// that diverge, a retried step that acts twice, or a snapshot that does not restore, are each
/// the end of a correct ledger.
#[test]
fn applying_is_idempotent_deterministic_and_leaves_no_trace_when_refused() {
    let mut w = World::new();
    let open = w.open_command("P1", &["L1"], &[("L1", "life", "life", 500_000_00)], false);
    let first = w.send_keyed("open-P1", "new-business", Command::OpenProposal(Box::new(open.clone()))).unwrap();
    let again = w.send_keyed("open-P1", "new-business", Command::OpenProposal(Box::new(open.clone()))).unwrap();
    assert_eq!(
        again,
        Applied {
            duplicate: true,
            ..first.clone()
        }
    );
    assert!(!first.duplicate && !first.events.is_empty());
    assert_eq!(
        w.send_keyed("", "new-business", Command::OpenProposal(Box::new(open.clone())))
            .unwrap_err()
            .code,
        "key"
    );

    // Every command, from the wrong driver.
    let samples = vec![
        Command::OpenProposal(Box::new(open)),
        Command::Withdraw { proposal: "P1".into() },
        Command::Sweep { after: None, max: 1 },
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
        Command::TakeCase {
            case: "c".into(),
            underwriter: "amy".into(),
        },
        Command::CapacityAnswered {
            reservation: "r".into(),
            answer: CapacityAnswer::Automatic,
        },
        Command::EvidenceUnobtainable { request: "r".into() },
        Command::AdviserLookup {
            proposal: "P1".into(),
            adviser: "a".into(),
            purpose: "p".into(),
        },
        Command::ActivatePolicies {
            cedar: String::new(),
            approval: approval(),
        },
        Command::LicenceChanged {
            subject: "a".into(),
            status: ic_underwriting::command::LicenceStatus::Active,
            effective: d(2026, 1, 1),
            source_ref: "r".into(),
        },
        Command::OpenFileForClaim {
            claim_ref: "c".into(),
            policy: PolicyId::from("p"),
            life: "L1".into(),
            assessor: "a".into(),
        },
        Command::DelegateFileReceived {
            proposal: "P1".into(),
            life: "L1".into(),
            vault_ref: "v".into(),
            hash: "h".into(),
        },
        Command::ScheduleInterview {
            proposal: "P1".into(),
            life: "L1".into(),
            scheduled_for: w.now,
            booked_by: "a".into(),
        },
    ];
    for command in samples {
        assert_eq!(w.err("somebody-else", command), "caller");
    }

    // The same log on a fresh replica gives the same state, and the state restores from JSON.
    let disclosed = w.disclose("P1", "L1", &yes("smoked_last_12_months"));
    w.automatic(&disclosed);
    w.respond("P1", "L1", OfferResponse::Accept);
    let mut replica = UnderwritingState::default();
    for (i, envelope) in w.log.iter().enumerate() {
        let _ = replica.apply(i as u64 + 1, envelope.clone());
    }
    assert!(replica == w.s, "replicas that applied the same log must agree");
    let saved = serde_json::to_string(&w.s).unwrap();
    let restored: UnderwritingState = serde_json::from_str(&saved).unwrap();
    assert!(restored == w.s);
    assert_eq!(serde_json::to_string(&restored).unwrap(), saved);
    let envelope: Envelope = serde_json::from_str(&serde_json::to_string(&w.log[0]).unwrap()).unwrap();
    assert_eq!(envelope, w.log[0]);

    // Nothing in an event names a reason, a loading or an exclusion.
    let events = serde_json::to_string(
        &w.log
            .iter()
            .enumerate()
            .filter_map(|(i, e)| UnderwritingState::default().apply(i as u64, e.clone()).ok())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!events.contains("RC-0103"));
    let _ = (AccessEnd::DecisionMade, Stage::Accepted);
}
