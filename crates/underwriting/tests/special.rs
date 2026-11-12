//! Decisions that arrive as data, the claims module's way in, peer review,
//! declared conflicts and the queries other modules ask.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_core::{LifeId, PartyId, PolicyId};
use ic_underwriting::case::{CaseType, Queue};
use ic_underwriting::codes::AppliedExclusion;
use ic_underwriting::command::{
    Applied, Command, DelegatedDecision, Effect, Event, MigratedLife, MigratedPolicy, NonDisclosureOutcome, PremiumOnAvoidance, Reject, View,
};
use ic_underwriting::config::{DelegatedAuthority, SampleRates, Settings};
use ic_underwriting::evidence::EvidenceKind;
use ic_underwriting::model::{CoverOutcome, CoverTerms, DecisionSource, DecisionStatus, ProposalStatus, Signing, Stage};
use ic_underwriting::questions::DisclosureRegime;
use ic_underwriting::{answer, Query};

fn console(w: &mut World, c: Command) -> Result<Applied, Reject> {
    w.send("underwriting-console", c)
}

fn authority() -> DelegatedAuthority {
    DelegatedAuthority {
        id: "acme".into(),
        version: 1,
        delegate: "acme-tpa".into(),
        products: ["term-life".to_string()].into(),
        per_life_minor: [("life".to_string(), 300_000_00)].into(),
        aggregate_minor: 500_000_00,
        max_loading_percent: 50,
        may_exclude: false,
        may_decline: true,
        rule_set: Some(("individual".into(), 1)),
        effective_from: d(2026, 1, 1),
        effective_to: None,
        reinsurer_agreement_ref: Some("RE-1".into()),
    }
}

fn delegated(proposal: &str, reference: &str, terms: Vec<CoverTerms>) -> DelegatedDecision {
    DelegatedDecision {
        proposal: proposal.into(),
        life: "L1".into(),
        authority: "acme".into(),
        terms,
        effective: d(2026, 11, 10),
        rule_version: Some(1),
        evidence: BTreeSet::from([EvidenceKind::GpReport]),
        decided_by: "tpa-underwriter-4".into(),
        delegate_ref: reference.into(),
        consent_ref: "CONS-1".into(),
    }
}

fn excluded(code: &str) -> CoverTerms {
    CoverTerms {
        cover: "life".into(),
        outcome: CoverOutcome::Accept {
            loading_percent: 0,
            exclusions: vec![AppliedExclusion {
                code: code.into(),
                from: d(2026, 11, 11),
                until: None,
            }],
            revised_sum_insured_minor: None,
        },
    }
}

/// Purpose: a delegate's decision arrives as typed data. Inside the authority it becomes the
/// life's decision, marked as delegated with the authority version, and the capacity step still
/// runs on our side. Outside it (above the per-life ceiling, terms the authority does not
/// allow, a date out of tolerance, the wrong rule version, no consent reference, another
/// product, no authority in force, or the aggregate exhausted) nothing is bound: the reason is
/// returned and the life goes to our own queue. A repeated reference is refused outright.
/// Why it matters: the ceilings exist so that a systematic error in a delegate's system cannot
/// bind the insurer to risk it cannot cede. Every branch here is one way that could happen.
#[test]
fn a_delegated_decision_is_bound_only_inside_its_authority() {
    let mut w = World::new();
    w.ok(
        "config",
        Command::ActivateDelegatedAuthority {
            authority: Box::new(authority()),
            approval: approval(),
        },
    );
    let send = |w: &mut World, d: DelegatedDecision| w.send("delegate-api", Command::DelegatedDecision(Box::new(d)));
    let rejected = |w: &mut World, id: &str, sum: i64, change: &dyn Fn(&mut DelegatedDecision)| -> String {
        w.open(id, sum);
        let mut d = delegated(id, &format!("REF-{id}"), vec![standard("life")]);
        change(&mut d);
        let out = send(w, d).unwrap();
        let Some(View::Delegated {
            accepted: false,
            why: Some(why),
        }) = out.view
        else {
            panic!("expected a rejection")
        };
        assert!(matches!(&out.events[0], Event::DelegatedDecisionRejected { .. }));
        assert!(matches!(
            &out.events[1],
            Event::Referred {
                case_type: CaseType::Referral,
                queue: Queue::Medical,
                ..
            }
        ));
        assert!(w.p(id).lives[&LifeId::from("L1")].decisions.is_empty(), "nothing is bound");
        assert!(w.p(id).lives[&LifeId::from("L1")].open_case().unwrap().reasons.contains("RC-0971"));
        why
    };
    assert!(rejected(&mut w, "R1", 300_000_01, &|_| {}).contains("per-life ceiling"));
    assert!(rejected(&mut w, "R2", 100_000_00, &|d| d.terms = vec![accept("life", 51, None)]).contains("outside the authority"));
    assert!(rejected(&mut w, "R3", 100_000_00, &|d| d.terms = vec![excluded("EX-4001")]).contains("outside the authority"));
    assert!(rejected(&mut w, "R4", 100_000_00, &|x| x.effective = d(2026, 10, 1)).contains("tolerance"));
    assert!(rejected(&mut w, "R5", 100_000_00, &|x| x.effective = d(2026, 11, 12)).contains("tolerance"));
    assert!(rejected(&mut w, "R6", 100_000_00, &|d| d.rule_version = None).contains("rule version"));
    assert!(rejected(&mut w, "R7", 100_000_00, &|d| d.consent_ref = String::new()).contains("consent"));
    assert!(rejected(&mut w, "R8", 100_000_00, &|d| d.authority = "other".into()).contains("no authority"));
    assert!(rejected(&mut w, "R9", 100_000_00, &|x| x.terms = vec![CoverTerms {
        cover: "life".into(),
        outcome: CoverOutcome::Defer { until: d(2027, 6, 1) }
    }])
    .contains("outside the authority"));
    let open = w.open_command("R10", &["L1"], &[("L1", "trauma", "trauma", 50_000_00)], false);
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let out = send(&mut w, delegated("R10", "REF-R10", vec![standard("trauma")])).unwrap();
    assert!(matches!(out.view, Some(View::Delegated { accepted: false, .. })), "no ceiling for the line");

    // Malformed or misdirected: refused as a command, with nothing recorded.
    w.open("P1", 300_000_00);
    assert_eq!(
        w.err(
            "delegate-api",
            Command::DelegatedDecision(Box::new(delegated("P1", "", vec![standard("life")])))
        ),
        "duplicate"
    );
    assert_eq!(
        w.err(
            "delegate-api",
            Command::DelegatedDecision(Box::new(delegated("P1", "REF-R1", vec![standard("life")])))
        ),
        "duplicate"
    );
    assert_eq!(
        w.err("delegate-api", Command::DelegatedDecision(Box::new(delegated("P1", "REF-1", vec![])))),
        "terms"
    );
    assert_eq!(
        w.err(
            "delegate-api",
            Command::DelegatedDecision(Box::new(delegated("P1", "REF-1", vec![excluded("EX-7777")])))
        ),
        "terms"
    );
    assert_eq!(
        w.err(
            "delegate-api",
            Command::DelegatedDecision(Box::new(delegated("P9", "REF-1", vec![standard("life")])))
        ),
        "unknown_proposal"
    );
    assert_eq!(
        w.err(
            "delegate-api",
            Command::DelegatedDecision(Box::new(delegated("R1", "REF-1", vec![standard("life")])))
        ),
        "not_awaiting"
    );

    // Inside the authority: bound as delegated, and capacity still runs on our side.
    let bound = send(&mut w, delegated("P1", "REF-1", vec![accept("life", 50, None)])).unwrap();
    assert_eq!(bound.view, Some(View::Delegated { accepted: true, why: None }));
    assert_eq!(World::reservations(&bound), vec!["P1/L1/life/1".to_string()]);
    let offered = w.automatic(&bound);
    assert!(offered.events.iter().any(|e| matches!(e, Event::OfferMade { .. })));
    let decision = &w.p("P1").lives[&LifeId::from("L1")].decisions[0];
    assert!(
        matches!(&decision.source, DecisionSource::Delegated { authority, authority_version: 1, delegate_ref, consent_ref, decided_by } if authority == "acme" && delegate_ref == "REF-1" && consent_ref == "CONS-1" && decided_by == "tpa-underwriter-4")
    );
    assert_eq!(
        (decision.all_reasons(), decision.evidence.clone(), decision.signatures.len()),
        (set(&["RC-0970"]), BTreeSet::from([EvidenceKind::GpReport]), 0)
    );

    // A decline is within this authority, and counts nothing towards the aggregate.
    w.open("P2", 250_000_00);
    let declined = send(&mut w, delegated("P2", "REF-2", vec![decline("life")])).unwrap();
    assert!(declined.events.iter().any(|e| matches!(e, Event::ProposalDeclined { .. })));
    // The aggregate: 300k stands accepted, so another 250k would pass 500k.
    assert!(rejected(&mut w, "R11", 250_000_00, &|_| {}).contains("aggregate"));
    w.open("P3", 200_000_00);
    assert!(matches!(
        send(&mut w, delegated("P3", "REF-3", vec![standard("life")])).unwrap().view,
        Some(View::Delegated { accepted: true, .. })
    ));

    // Clinical information stays with the delegate; we can call for the file, and that is logged.
    let request = |who: &str, life: &str, purpose: &str, id: &str| Command::RequestDelegateFile {
        proposal: id.into(),
        life: life.into(),
        underwriter: who.into(),
        purpose: purpose.into(),
    };
    assert_eq!(console(&mut w, request("nobody", "L1", "sampling", "P1")).unwrap_err().code, "no_profile");
    assert_eq!(console(&mut w, request("sam", "L1", "", "P1")).unwrap_err().code, "not_authorised");
    assert_eq!(console(&mut w, request("sam", "L1", "sampling", "R1")).unwrap_err().code, "not_delegated");
    let asked = console(&mut w, request("sam", "L1", "quality sampling", "P1")).unwrap();
    assert!(matches!(&asked.effects[0], Effect::RequestDelegateFile { delegate_ref, .. } if delegate_ref == "REF-1"));
    assert!(matches!(&asked.events[0], Event::FileAccessed { who, purpose, .. } if who == "sam" && purpose == "quality sampling"));
    let received = |hash: &str| Command::DelegateFileReceived {
        proposal: "P1".into(),
        life: "L1".into(),
        vault_ref: "vault://delegate/REF-1".into(),
        hash: hash.into(),
    };
    assert_eq!(w.err("delegate-api", received("short")), "disclosure");
    w.ok("delegate-api", received(&"a".repeat(64)));
    assert_eq!(w.p("P1").lives[&LifeId::from("L1")].delegate_file.as_ref().unwrap().0, "vault://delegate/REF-1");
}

fn migrated(w: &World, id: &str, terms: Vec<CoverTerms>) -> MigratedPolicy {
    MigratedPolicy {
        open: w.open_command(id, &["L1"], &[("L1", "life", "life", 250_000_00)], false),
        commencement: d(2019, 3, 1),
        original_decision_date: d(2019, 2, 20),
        legacy_ref: "LEGACY-POL-881".into(),
        lives: vec![MigratedLife {
            life: "L1".into(),
            terms,
            file_ref: Some("vault://legacy/881".into()),
            file_hash: Some("b".repeat(64)),
            legacy_underwriter: Some("j.smith".into()),
        }],
    }
}

/// Purpose: a policy migrated from a legacy system, or taken over from another insurer, carries
/// its underwriting terms as data: the decision is recorded as migrated with its own reason
/// code and original date, the legacy file is referenced with its legacy evidence of signing,
/// a clause with no code of ours rides under a verbatim code, and nothing asks for capacity.
/// Where the original terms are unknown they are marked unknown and a senior is asked: they are
/// never read as standard.
/// Why it matters: a migrated exclusion silently dropped means a claim paid that should not
/// be; unknown terms defaulted to standard does the same across a whole book.
#[test]
fn migrated_terms_are_carried_as_data_and_never_defaulted() {
    let mut w = World::new();
    let policy = PolicyId::from("POL-M1");
    let life = LifeId::from("L1");
    let m = migrated(&w, "M1", vec![excluded("EX-9000")]);
    let done = w.ok("migration", Command::MigratedPolicy(Box::new(m.clone())));
    assert!(done.effects.is_empty(), "the policy is already in force and already ceded");
    assert!(matches!(&done.events[0], Event::DecisionMade { .. }));
    let p = w.p("M1");
    assert!(matches!(p.status, ProposalStatus::Issued { .. }) && p.migrated && p.received_on == d(2019, 3, 1));
    let f = &p.lives[&life];
    let decision = &f.decisions[0];
    assert!(
        matches!(&decision.source, DecisionSource::Migrated { legacy_ref, legacy_underwriter: Some(u) } if legacy_ref == "LEGACY-POL-881" && u == "j.smith")
    );
    assert_eq!(
        (decision.status, decision.all_reasons(), decision.made_at.date_naive(), f.stage),
        (DecisionStatus::Effective, set(&["RC-0960"]), d(2019, 2, 20), Stage::Accepted)
    );
    assert!(matches!(&f.disclosures[0].evidence_of_signing, Signing::Migrated { legacy_ref } if legacy_ref == "LEGACY-POL-881"));
    let terms = w.s.terms_on(&policy, &life, "life").unwrap();
    assert_eq!(
        (terms.exclusions[0].code.as_str(), terms.unknown_terms, terms.loading_percent),
        ("EX-9000", false, 0)
    );
    assert_eq!(w.err("migration", Command::MigratedPolicy(Box::new(m))), "duplicate_proposal");

    // Unknown terms: marked, and put to a senior.
    let unknown = migrated(
        &w,
        "M2",
        vec![CoverTerms {
            cover: "life".into(),
            outcome: CoverOutcome::UnknownTerms,
        }],
    );
    let done = w.ok("migration", Command::MigratedPolicy(Box::new(unknown)));
    assert!(matches!(
        &done.events[1],
        Event::Referred {
            queue: Queue::SeniorReview,
            ..
        }
    ));
    let terms = w.s.terms_on(&PolicyId::from("POL-M2"), &life, "life").unwrap();
    assert!(terms.unknown_terms && terms.exclusions.is_empty());
    let f = &w.p("M2").lives[&life];
    assert_eq!((f.decisions[0].all_reasons(), f.stage), (set(&["RC-0961"]), Stage::Accepted));
    assert!(f.open_case().unwrap().reasons.contains("RC-0961"));
    assert_eq!(w.s.queue(Queue::SeniorReview, w.now).len(), 1);

    // What cannot be carried faithfully is refused, not guessed.
    let bad = |w: &mut World, change: &dyn Fn(&mut MigratedPolicy)| {
        let mut m = migrated(w, "M3", vec![standard("life")]);
        change(&mut m);
        w.err("migration", Command::MigratedPolicy(Box::new(m)))
    };
    assert_eq!(bad(&mut w, &|m| m.lives[0].terms = vec![excluded("EX-7777")]), "unknown_code");
    assert_eq!(bad(&mut w, &|m| m.lives[0].terms = vec![]), "terms");
    assert_eq!(bad(&mut w, &|m| m.lives[0].terms = vec![standard("trauma")]), "terms");
    assert_eq!(bad(&mut w, &|m| m.lives[0].life = "L2".into()), "migration");
    assert_eq!(bad(&mut w, &|m| m.lives.clear()), "migration");
    assert_eq!(bad(&mut w, &|m| m.legacy_ref = String::new()), "migration");
    assert_eq!(bad(&mut w, &|m| m.original_decision_date = d(2026, 11, 12)), "migration");
    assert_eq!(bad(&mut w, &|m| m.open.covers.clear()), "terms");
    let mut empty = World {
        s: Default::default(),
        index: 0,
        now: w.now,
        log: vec![],
    };
    let m = migrated(&w, "M3", vec![standard("life")]);
    assert_eq!(empty.err("migration", Command::MigratedPolicy(Box::new(m))), "no_rules");
    // A file with no legacy underwriting record at all still migrates, with nothing invented.
    let mut bare = migrated(&w, "M4", vec![accept("life", 75, None)]);
    bare.lives[0].file_ref = None;
    w.ok("migration", Command::MigratedPolicy(Box::new(bare)));
    assert!(w.p("M4").lives[&life].disclosures.is_empty());
    assert_eq!(w.s.terms_on(&PolicyId::from("POL-M4"), &life, "life").unwrap().loading_percent, 75);
}

/// A policy issued on terms with an exclusion, decided by `amy`.
fn issued_with_terms(w: &mut World) {
    w.open("P1", 500_000_00);
    w.disclose("P1", "L1", &yes("hazardous_occupation"));
    let case = w.take("P1", "L1", "amy");
    let decided = w.propose(&case, "amy", vec![excluded("EX-5001")]).unwrap();
    w.automatic(&decided);
    w.respond("P1", "L1", ic_underwriting::command::OfferResponse::Accept);
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
    );
}

/// Purpose: the claims module's way in. It reads the terms on a cover (loading, dated exclusion
/// codes, the decision reference); opening the underwriting file is logged with the claim as
/// its purpose and returns the decision, its versions and where the disclosures are; and the
/// file is never opened to someone who underwrote the life.
/// Why it matters: the cover check at claim pays or declines on these terms, and separation of
/// duties between underwriting and claims on the same life is a control against collusion.
#[test]
fn claims_reads_terms_and_opens_the_file_with_a_logged_purpose() {
    let mut w = World::new();
    issued_with_terms(&mut w);
    let (policy, life) = (PolicyId::from("POL-P1"), LifeId::from("L1"));
    let terms = w.s.terms_on(&policy, &life, "life").unwrap();
    assert_eq!(
        (
            terms.decision.as_str(),
            terms.exclusions[0].code.as_str(),
            terms.exclusions[0].from,
            terms.unknown_terms
        ),
        ("P1/L1/d1", "EX-5001", d(2026, 11, 11), false)
    );
    assert!(w.s.terms_on(&policy, &life, "trauma").is_none() && w.s.terms_on(&PolicyId::from("nope"), &life, "life").is_none());
    assert_eq!(w.s.underwriters_of(&policy, &life), set(&["amy"]));
    assert!(w.s.underwriters_of(&policy, &LifeId::from("L9")).is_empty());
    assert!(w.s.exclusions(d(2026, 11, 11)).unwrap().contains("EX-5001"));

    let open = |assessor: &str, claim: &str, policy: &str| Command::OpenFileForClaim {
        claim_ref: claim.into(),
        policy: PolicyId::from(policy),
        life: "L1".into(),
        assessor: assessor.into(),
    };
    assert_eq!(w.err("claims", open("amy", "CLM-1", "POL-P1")), "separation_of_duties");
    assert_eq!(w.err("claims", open("", "CLM-1", "POL-P1")), "purpose");
    assert_eq!(w.err("claims", open("cl-1", "", "POL-P1")), "purpose");
    assert_eq!(w.err("claims", open("cl-1", "CLM-1", "POL-NOPE")), "no_file");
    let opened = w.ok("claims", open("cl-1", "CLM-1", "POL-P1"));
    assert!(
        matches!(&opened.events[0], Event::FileAccessed { who, role, purpose, documents, .. } if who == "cl-1" && role == "claims_assessor" && purpose == "claim CLM-1" && documents.len() == 1)
    );
    let Some(View::ClaimFiles(files)) = &opened.view else { panic!("no file") };
    assert_eq!(
        (files.len(), files[0].decision.as_str(), files[0].underwriters.clone()),
        (1, "P1/L1/d1", set(&["amy"]))
    );
    assert_eq!(
        files[0].disclosures[0],
        (
            "vault://P1/L1/disclosure".to_string(),
            w.p("P1").lives[&life].disclosures[0].hash.clone(),
            false
        )
    );
    assert_eq!(files[0].versions.question_set.as_ref().unwrap().version, 1);

    // The queries the module host answers are the same reads, as JSON.
    let ask = |w: &World, q: Query| answer(&w.s, serde_json::to_value(q).unwrap()).unwrap();
    assert_eq!(
        ask(
            &w,
            Query::TermsOn {
                policy: policy.clone(),
                life: life.clone(),
                cover: "life".into()
            }
        )["decision"],
        "P1/L1/d1"
    );
    assert_eq!(
        ask(
            &w,
            Query::UnderwritersOf {
                policy: policy.clone(),
                life: life.clone()
            }
        ),
        serde_json::json!(["amy"])
    );
    assert_eq!(ask(&w, Query::Proposal { proposal: "P1".into() })["policy"], "POL-P1");
    assert_eq!(ask(&w, Query::Proposal { proposal: "P9".into() }), serde_json::Value::Null);
    assert_eq!(ask(&w, Query::Workload { underwriter: "amy".into() }), 0);
    assert_eq!(
        ask(
            &w,
            Query::Queue {
                queue: Queue::Medical,
                now: w.now
            }
        ),
        serde_json::json!([])
    );
    assert_eq!(ask(&w, Query::Exclusions { on: d(2026, 11, 11) })["id"], "exclusions");
    let report = ask(
        &w,
        Query::Report {
            from: w.now - chrono::Duration::days(1),
            to: w.now + chrono::Duration::days(1),
        },
    );
    assert_eq!(
        (
            report["decisions"].clone(),
            report["straight_through"].clone(),
            report["by_outcome"]["terms"].clone()
        ),
        (1.into(), 0.into(), 1.into())
    );
    assert!(answer(&w.s, serde_json::json!({"query": "nope"})).is_err());
}

/// Purpose: a non-disclosure review opened from a claim is a case of its own for a senior who
/// never touched the life; it reports the legal basis of the contract's date; no action closes
/// it with one signature; a variation or an avoidance needs a second, different senior, is
/// recorded against the original decision without editing it, and goes to the core as its own
/// command. A variation carries the terms that would have been offered, and the remedy takes
/// them to the core; an avoidance records whether the premiums paid are refunded or kept, and
/// is refused without that.
/// Why it matters: avoiding a policy at claim time is the most serious thing an insurer does
/// to a customer. The remedy depends on the law in force at the contract date, and must never
/// rest on the person who underwrote the case or on one person alone.
#[test]
fn a_non_disclosure_review_needs_untouched_seniors_and_two_for_a_remedy() {
    let mut w = World::new();
    issued_with_terms(&mut w);
    let open = |claim: &str, policy: &str, by: &str| Command::OpenNonDisclosureReview {
        claim_ref: claim.into(),
        policy: PolicyId::from(policy),
        life: "L1".into(),
        opened_by: by.into(),
    };
    assert_eq!(w.err("claims", open("CLM-1", "POL-NOPE", "cl-1")), "no_file");
    assert_eq!(w.err("claims", open("", "POL-P1", "cl-1")), "duplicate");
    let opened = w.ok("claims", open("CLM-1", "POL-P1", "cl-1"));
    assert_eq!(
        opened.view,
        Some(View::Review {
            review: "ndr/CLM-1/L1".into(),
            regime: DisclosureRegime::DutyOfDisclosure
        })
    );
    assert_eq!(w.err("claims", open("CLM-1", "POL-P1", "cl-1")), "duplicate");
    let review = "ndr/CLM-1/L1".to_string();
    assert_eq!(w.s.queue(Queue::SeniorReview, w.now)[0].case_type, CaseType::NonDisclosureReview);
    assert_eq!(w.stage("P1", "L1"), Stage::Accepted, "the review is beside the policy, not a step in it");

    let take = |who: &str| Command::TakeCase {
        case: "ndr/CLM-1/L1".into(),
        underwriter: who.into(),
    };
    assert_eq!(console(&mut w, take("amy")).unwrap_err().code, "wrong_queue");
    // A senior who had touched the life could not take it.
    let mut touched = World::new();
    touched.open("P1", 500_000_00);
    touched.disclose("P1", "L1", &yes("heart_condition"));
    let case = touched.take("P1", "L1", "sam");
    let decided = touched.propose(&case, "sam", vec![accept("life", 50, None)]).unwrap();
    touched.automatic(&decided);
    touched.respond("P1", "L1", ic_underwriting::command::OfferResponse::Accept);
    touched.ok("claims", open("CLM-1", "POL-P1", "cl-1"));
    assert_eq!(console(&mut touched, take("sam")).unwrap_err().code, "not_authorised");

    console(&mut w, take("sam")).unwrap();
    let close = |outcome, by: &str, second: Option<&str>, reasons: &[&str], terms: Vec<CoverTerms>| Command::RecordNonDisclosureOutcome {
        review: "ndr/CLM-1/L1".into(),
        outcome,
        decided_by: by.into(),
        countersigned_by: second.map(str::to_string),
        reasons: set(reasons),
        varied_terms: terms,
        premium: None,
    };
    let vary = NonDisclosureOutcome::Vary;
    assert_eq!(
        console(&mut w, close(vary, "sue", Some("sam"), &["RC-0980"], vec![accept("life", 50, None)]))
            .unwrap_err()
            .code,
        "not_owner"
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", None, &["RC-0980"], vec![accept("life", 50, None)]))
            .unwrap_err()
            .code,
        "countersign_required"
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", Some("sam"), &["RC-0980"], vec![accept("life", 50, None)]))
            .unwrap_err()
            .code,
        "countersign_required"
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", Some("amy"), &["RC-0980"], vec![accept("life", 50, None)]))
            .unwrap_err()
            .code,
        "not_authorised"
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", Some("sue"), &[], vec![accept("life", 50, None)]))
            .unwrap_err()
            .code,
        "reasons"
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", Some("sue"), &["RC-7777"], vec![accept("life", 50, None)]))
            .unwrap_err()
            .code,
        "reasons"
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", Some("sue"), &["RC-0980"], vec![])).unwrap_err().code,
        "terms"
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", Some("sue"), &["RC-0980"], vec![decline("life")]))
            .unwrap_err()
            .code,
        "terms"
    );
    assert_eq!(
        console(
            &mut w,
            close(NonDisclosureOutcome::Avoid, "sam", Some("sue"), &["RC-0980"], vec![accept("life", 50, None)])
        )
        .unwrap_err()
        .code,
        "terms"
    );
    assert_eq!(
        console(
            &mut w,
            Command::RecordNonDisclosureOutcome {
                review: "nope".into(),
                outcome: vary,
                decided_by: "sam".into(),
                countersigned_by: None,
                reasons: set(&["RC-0980"]),
                varied_terms: vec![],
                premium: None,
            }
        )
        .unwrap_err()
        .code,
        "unknown_review"
    );

    let mut kept = close(vary, "sam", Some("sue"), &["RC-0980"], vec![accept("life", 50, None)]);
    if let Command::RecordNonDisclosureOutcome { premium, .. } = &mut kept {
        *premium = Some(PremiumOnAvoidance::Refund);
    }
    assert_eq!(console(&mut w, kept).unwrap_err().code, "premium");
    let varied = console(&mut w, close(vary, "sam", Some("sue"), &["RC-0980"], vec![accept("life", 50, None)])).unwrap();
    // The remedy carries the terms themselves: the core is not asked to look them up.
    assert!(
        matches!(&varied.effects[0], Effect::NonDisclosureRemedy { terms, premium: None, .. }
            if terms.len() == 1 && terms[0].cover == "life" && terms[0].life == LifeId::from("L1") && terms[0].loadings[0].as_str() == "0.50"),
        "{:?}",
        varied.effects[0]
    );
    assert_eq!(varied.view, Some(View::NonDisclosure { decision: "P1/L1/d2".into() }));
    assert!(
        matches!(&varied.effects[0], Effect::NonDisclosureRemedy { key, outcome: NonDisclosureOutcome::Vary, decision: Some(d), .. } if key == "ndr/CLM-1/L1/remedy" && d == "P1/L1/d2")
    );
    assert!(varied.events.iter().any(|e| matches!(
        e,
        Event::NonDisclosureReviewClosed {
            outcome: NonDisclosureOutcome::Vary,
            ..
        }
    )));
    let f = &w.p("P1").lives[&LifeId::from("L1")];
    assert_eq!(
        (f.decisions[0].status, f.decisions[0].non_disclosure.clone(), f.decisions[1].status),
        (DecisionStatus::Superseded, Some(review.clone()), DecisionStatus::Effective)
    );
    assert_eq!(f.decisions[1].signers().collect::<Vec<_>>(), vec!["sam", "sue"]);
    assert_eq!(
        w.s.terms_on(&PolicyId::from("POL-P1"), &LifeId::from("L1"), "life").unwrap().loading_percent,
        50
    );
    assert_eq!(
        console(&mut w, close(vary, "sam", Some("sue"), &["RC-0980"], vec![accept("life", 50, None)]))
            .unwrap_err()
            .code,
        "unknown_review"
    );

    // No action: one signature, no command to the core. Avoidance: two, and a remedy with no new terms.
    w.ok("claims", open("CLM-2", "POL-P1", "cl-1"));
    console(
        &mut w,
        Command::TakeCase {
            case: "ndr/CLM-2/L1".into(),
            underwriter: "cat".into(),
        },
    )
    .unwrap();
    let none = console(
        &mut w,
        Command::RecordNonDisclosureOutcome {
            review: "ndr/CLM-2/L1".into(),
            outcome: NonDisclosureOutcome::NoAction,
            decided_by: "cat".into(),
            countersigned_by: None,
            reasons: set(&["RC-0980"]),
            varied_terms: vec![],
            premium: None,
        },
    )
    .unwrap();
    assert!(none.effects.is_empty());
    assert_eq!(none.view, Some(View::NonDisclosure { decision: "P1/L1/d2".into() }));
    let mut w2 = World::new();
    issued_with_terms(&mut w2);
    w2.ok("claims", open("CLM-3", "POL-P1", "cl-1"));
    console(
        &mut w2,
        Command::TakeCase {
            case: "ndr/CLM-3/L1".into(),
            underwriter: "sam".into(),
        },
    )
    .unwrap();
    let avoid = |premium| Command::RecordNonDisclosureOutcome {
        review: "ndr/CLM-3/L1".into(),
        outcome: NonDisclosureOutcome::Avoid,
        decided_by: "sam".into(),
        countersigned_by: Some("sue".into()),
        reasons: set(&["RC-0980"]),
        varied_terms: vec![],
        premium,
    };
    // Whether the premiums go back is part of the decision, not something left to the core.
    assert_eq!(console(&mut w2, avoid(None)).unwrap_err().code, "premium");
    let avoided = console(&mut w2, avoid(Some(PremiumOnAvoidance::Retain))).unwrap();
    assert!(matches!(
        &avoided.effects[0],
        Effect::NonDisclosureRemedy {
            outcome: NonDisclosureOutcome::Avoid,
            decision: None,
            premium: Some(PremiumOnAvoidance::Retain),
            terms,
            ..
        } if terms.is_empty()
    ));
    assert_eq!(
        [NonDisclosureOutcome::NoAction, NonDisclosureOutcome::Vary, NonDisclosureOutcome::Avoid].map(NonDisclosureOutcome::key),
        ["no_action", "vary", "avoid"]
    );

    // A migrated contract has no question set of ours: the legal basis follows its contract date.
    let mut w3 = World::new();
    let mut m = migrated(&w3, "M1", vec![standard("life")]);
    w3.ok("migration", Command::MigratedPolicy(Box::new(m.clone())));
    assert!(matches!(
        w3.ok("claims", open("CLM-4", "POL-M1", "cl-1")).view,
        Some(View::Review {
            regime: DisclosureRegime::DutyOfDisclosure,
            ..
        })
    ));
    w3.now += chrono::Duration::days(500);
    m.open = w3.open_command("M2", &["L1"], &[("L1", "life", "life", 250_000_00)], false);
    m.commencement = d(2027, 12, 1);
    m.original_decision_date = d(2027, 11, 20);
    w3.ok("migration", Command::MigratedPolicy(Box::new(m)));
    assert!(matches!(
        w3.ok("claims", open("CLM-5", "POL-M2", "cl-1")).view,
        Some(View::Review {
            regime: DisclosureRegime::ReasonableCare,
            ..
        })
    ));
}

/// Purpose: peer review. Each period a sample of decisions is drawn, weighted to declines and
/// non-standard terms, repeatable from its seed and never drawing a decision twice; a senior
/// who never touched the case records agreement, or disagreement with reasons; the finding
/// shows in the management figures.
/// Why it matters: sampling is how drift in underwriting quality is caught. A sample the
/// decision's own author can review, or one that cannot be reproduced, proves nothing.
#[test]
fn peer_review_samples_decisions_for_a_senior_who_never_touched_them() {
    let mut w = World::new();
    let all = Settings {
        version: 2,
        sample: SampleRates {
            standard_per_mille: 0,
            non_standard_per_mille: 1000,
            decline_per_mille: 1000,
        },
        ..Settings::default()
    };
    w.ok(
        "config",
        Command::ActivateSettings {
            settings: Box::new(all),
            approval: approval(),
        },
    );
    issued_with_terms(&mut w);
    w.open("P2", 100_000_00);
    let clean_one = w.disclose("P2", "L1", &clean());
    w.automatic(&clean_one);
    w.open("P3", 100_000_00);
    w.disclose("P3", "L1", &yes("heart_condition"));
    let case = w.take("P3", "L1", "sam");
    w.propose(&case, "sam", vec![decline("life")]).unwrap();
    w.take("P3", "L1", "sue");
    console(
        &mut w,
        Command::Countersign {
            case,
            underwriter: "sue".into(),
            agree: true,
            reasons: set(&["RC-0101"]),
            withheld_reasons: BTreeSet::new(),
        },
    )
    .unwrap();
    let m = migrated(&w, "M1", vec![accept("life", 50, None)]);
    w.ok("migration", Command::MigratedPolicy(Box::new(m)));

    let (from, to) = (w.now - chrono::Duration::days(1), w.now + chrono::Duration::days(1));
    let draw = |period: &str, from, to| Command::DrawPeerReviewSample {
        period: period.into(),
        from,
        to,
        seed: 42,
    };
    assert_eq!(console(&mut w, draw("", from, to)).unwrap_err().code, "period");
    assert_eq!(console(&mut w, draw("2026-11", to, from)).unwrap_err().code, "period");
    let drawn = console(&mut w, draw("2026-11", from, to)).unwrap();
    assert_eq!(
        drawn.view,
        Some(View::Sample {
            decisions: vec!["P1/L1/d1".into(), "P3/L1/d1".into()]
        }),
        "terms and declines at 100%, standard at 0%, migrated never"
    );
    assert_eq!(drawn.events.len(), 2);
    assert_eq!(
        console(&mut w, draw("2026-11b", from, to)).unwrap().view,
        Some(View::Sample { decisions: vec![] }),
        "nothing is drawn twice"
    );

    let find = |decision: &str, who: &str, agrees: bool, reasons: &[&str]| Command::RecordPeerFinding {
        decision: decision.into(),
        reviewer: who.into(),
        agrees,
        reasons: set(reasons),
    };
    assert_eq!(console(&mut w, find("nope", "sam", true, &[])).unwrap_err().code, "unknown_decision");
    assert_eq!(
        console(&mut w, find("P3/L1/d1", "sam", true, &[])).unwrap_err().code,
        "not_authorised",
        "sam signed it"
    );
    assert_eq!(
        console(&mut w, find("P1/L1/d1", "abe", true, &[])).unwrap_err().code,
        "not_authorised",
        "not a senior"
    );
    assert_eq!(console(&mut w, find("P1/L1/d1", "sam", false, &[])).unwrap_err().code, "reasons");
    assert_eq!(console(&mut w, find("P1/L1/d1", "sam", false, &["RC-7777"])).unwrap_err().code, "reasons");
    assert_eq!(console(&mut w, find("P2/L1/d1", "sam", true, &[])).unwrap_err().code, "not_sampled");
    let found = console(&mut w, find("P1/L1/d1", "sam", false, &["RC-0120"])).unwrap();
    assert!(matches!(&found.events[0], Event::PeerReviewRecorded { agrees: false, .. }));
    assert_eq!(
        console(&mut w, find("P1/L1/d1", "sue", true, &[])).unwrap_err().code,
        "not_sampled",
        "already reviewed"
    );
    console(&mut w, find("P3/L1/d1", "cat", true, &[])).unwrap();
    let report = w.s.report(from, to);
    assert_eq!(
        (report.decisions, report.straight_through, report.peer_reviewed, report.peer_disagreed),
        (3, 1, 2, 1)
    );
    assert_eq!(
        (report.by_outcome["decline"], report.by_outcome["standard"], report.by_outcome["terms"]),
        (1, 1, 1)
    );

    // The draw is a pure function of the seed: at a middling rate two replicas draw the same set.
    let half = |seed: u64| {
        let mut w = World::new();
        let some = Settings {
            version: 2,
            sample: SampleRates {
                standard_per_mille: 500,
                non_standard_per_mille: 500,
                decline_per_mille: 500,
            },
            ..Settings::default()
        };
        w.ok(
            "config",
            Command::ActivateSettings {
                settings: Box::new(some),
                approval: approval(),
            },
        );
        for i in 0..12 {
            let id = format!("Q{i}");
            w.open(&id, 100_000_00);
            let d = w.disclose(&id, "L1", &clean());
            w.automatic(&d);
        }
        let out = console(
            &mut w,
            Command::DrawPeerReviewSample {
                period: "p".into(),
                from,
                to,
                seed,
            },
        )
        .unwrap();
        let Some(View::Sample { decisions }) = out.view else { panic!() };
        decisions
    };
    let first = half(7);
    assert_eq!(first, half(7));
    assert!(!first.is_empty() && first.len() < 12, "a half sample of twelve is some but not all");
    assert_ne!(first, half(8));
}

/// Purpose: an underwriter who declares a relationship with someone on an application loses any
/// case they hold on it at once and can no longer take, be handed, decide or advise on it;
/// their other cases are untouched.
/// Why it matters: nobody underwrites their own family. The declaration has to bite on work
/// already in hand, not only on what comes next.
#[test]
fn a_declared_conflict_removes_the_underwriter_from_the_case() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    w.disclose("P1", "L1", &yes("heart_condition"));
    w.open("P2", 500_000_00);
    w.disclose("P2", "L1", &yes("heart_condition"));
    // Different people on the second proposal.
    let mut other = w.open_command("P3", &["L7"], &[("L7", "life", "life", 500_000_00)], false);
    other.policyholder = PartyId::from("PTY-L8");
    w.ok("new-business", Command::OpenProposal(Box::new(other)));
    w.disclose("P3", "L7", &yes("heart_condition"));
    w.take("P1", "L1", "amy");
    w.take("P3", "L7", "amy");
    assert_eq!(w.s.workload("amy"), 2);

    assert_eq!(
        console(
            &mut w,
            Command::DeclareConflict {
                underwriter: "nobody".into(),
                party: PartyId::from("PTY-L1")
            }
        )
        .unwrap_err()
        .code,
        "no_profile"
    );
    let declared = console(
        &mut w,
        Command::DeclareConflict {
            underwriter: "amy".into(),
            party: PartyId::from("PTY-L1"),
        },
    )
    .unwrap();
    assert!(matches!(&declared.events[0], Event::CaseReleased { case, from } if case == "P1/L1/case/1" && from == "amy"));
    assert_eq!((declared.events.len(), w.s.workload("amy")), (1, 1));
    assert_eq!(w.p("P1").lives[&LifeId::from("L1")].open_case().unwrap().owner, None);
    assert_eq!(
        console(
            &mut w,
            Command::TakeCase {
                case: "P1/L1/case/1".into(),
                underwriter: "amy".into()
            }
        )
        .unwrap_err()
        .code,
        "not_authorised"
    );
    assert_eq!(
        console(
            &mut w,
            Command::TakeCase {
                case: "P2/L1/case/1".into(),
                underwriter: "amy".into()
            }
        )
        .unwrap_err()
        .code,
        "not_authorised",
        "the same person on another application"
    );
    w.take("P1", "L1", "abe");
    assert_eq!(
        console(
            &mut w,
            Command::HandOver {
                case: "P1/L1/case/1".into(),
                by: "abe".into(),
                to: "amy".into()
            }
        )
        .unwrap_err()
        .code,
        "not_authorised"
    );
    assert_eq!(
        console(
            &mut w,
            Command::AskOpinion {
                case: "P1/L1/case/1".into(),
                underwriter: "abe".into(),
                of: "amy".into()
            }
        )
        .unwrap_err()
        .code,
        "not_authorised"
    );
    assert_eq!(
        console(
            &mut w,
            Command::RetryIssue {
                proposal: "P1".into(),
                underwriter: "amy".into()
            }
        )
        .unwrap_err()
        .code,
        "not_authorised"
    );
    // A relationship with the policyholder counts as much as one with a life insured.
    console(
        &mut w,
        Command::DeclareConflict {
            underwriter: "amy".into(),
            party: PartyId::from("PTY-L8"),
        },
    )
    .unwrap();
    assert_eq!(w.s.workload("amy"), 0);
}
