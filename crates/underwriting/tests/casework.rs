//! The human steps: evidence, referral queues, authority, the second
//! signature, interviews and the reinsurer.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_core::{LifeId, PartyId};
use ic_underwriting::case::{CaseType, ChaserFrom, Escalation, Queue};
use ic_underwriting::command::{CapacityAnswer, Command, Effect, Event, FacultativeOutcome, NoticeKind, Recipient, SigningProof};
use ic_underwriting::config::Settings;
use ic_underwriting::evidence::{EvidenceKind, EvidenceStatus};
use ic_underwriting::model::{CapacityState, ConsentKind, CoverOutcome, CoverTerms, DecisionStatus, ProposalStatus, Stage};
use ic_underwriting::questions::Answer;

fn console(w: &mut World, c: Command) -> Result<ic_underwriting::command::Applied, ic_underwriting::command::Reject> {
    w.send("underwriting-console", c)
}

fn life<'a>(w: &'a World, id: &str) -> &'a ic_underwriting::model::LifeFile {
    &w.p(id).lives[&LifeId::from("L1")]
}

/// Purpose: when the rules call for evidence it is asked for only with the life's consent to
/// records being obtained; each request has a stable id; it is chased on the timer; and when it
/// arrives nothing is decided automatically: a case opens for a person, with the reasons, who
/// then makes the decision.
/// Why it matters: evidence must never default to standard or to decline. A blood test that
/// arrives and silently accepts, or a doctor asked twice because a step was retried, are both
/// failures the design rules out.
#[test]
fn evidence_is_requested_chased_and_then_read_by_a_person() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let diabetes = yes("diabetes");
    let d = w.disclosure("P1", "L1", &diabetes);
    assert_eq!(w.err("new-business", Command::SubmitDisclosure(Box::new(d))), "consent_required");
    w.consent("P1", "L1", ConsentKind::ObtainRecords);
    let asked = w.disclose("P1", "L1", &diabetes);
    assert_eq!(w.stage("P1", "L1"), Stage::AwaitingEvidence);
    assert!(
        matches!(&asked.effects[0], Effect::RequestEvidence { request, kind: EvidenceKind::BloodTest, party } if request == "P1/L1/blood_test" && party == &PartyId::from("PTY-L1"))
    );
    assert!(matches!(&asked.events[1], Event::EvidenceRequested { deadline, .. } if *deadline == w.now + chrono::Duration::days(21)));

    let request = "P1/L1/blood_test".to_string();
    w.ok(
        "evidence",
        Command::EvidenceAcknowledged {
            request: request.clone(),
            provider_ref: "LAB-9".into(),
        },
    );
    assert_eq!(life(&w, "P1").evidence[0].provider_ref.as_deref(), Some("LAB-9"));
    assert_eq!(
        w.err(
            "evidence",
            Command::EvidenceAcknowledged {
                request: "nope".into(),
                provider_ref: "x".into()
            }
        ),
        "unknown_request"
    );

    w.days(6);
    assert!(
        w.ok("timer", Command::Sweep { after: None, max: 10 }).effects.is_empty(),
        "not yet due a chaser"
    );
    w.days(1);
    let chased = w.ok("timer", Command::Sweep { after: None, max: 10 });
    assert!(matches!(&chased.effects[0], Effect::ChaseEvidence { request } if request == "P1/L1/blood_test"));
    assert!(matches!(&chased.events[0], Event::EvidenceChased { chasers: 1, .. }));
    assert!(
        w.ok("timer", Command::Sweep { after: None, max: 10 }).effects.is_empty(),
        "chased once per interval"
    );

    assert_eq!(
        w.err(
            "evidence",
            Command::EvidenceReceived {
                request: request.clone(),
                vault_ref: "".into(),
                cost_minor: 1
            }
        ),
        "evidence"
    );
    assert_eq!(
        w.err(
            "evidence",
            Command::EvidenceReceived {
                request: request.clone(),
                vault_ref: "v".into(),
                cost_minor: -1
            }
        ),
        "evidence"
    );
    let arrived = w.ok(
        "evidence",
        Command::EvidenceReceived {
            request: request.clone(),
            vault_ref: "vault://lab".into(),
            cost_minor: 85_00,
        },
    );
    assert!(matches!(
        &arrived.events[0],
        Event::EvidenceSettled {
            status: EvidenceStatus::Received,
            ..
        }
    ));
    assert!(matches!(
        &arrived.events[1],
        Event::Referred {
            case_type: CaseType::MedicalEvidence,
            queue: Queue::Medical,
            ..
        }
    ));
    assert_eq!(w.stage("P1", "L1"), Stage::InAssessment);
    assert!(life(&w, "P1").decisions.is_empty(), "evidence decides nothing by arriving");
    assert!(life(&w, "P1").open_case().unwrap().reasons.contains("RC-0902"));
    assert_eq!(
        w.err(
            "evidence",
            Command::EvidenceReceived {
                request: request.clone(),
                vault_ref: "v".into(),
                cost_minor: 0
            }
        ),
        "not_outstanding"
    );
    assert_eq!(
        w.err(
            "evidence",
            Command::EvidenceAcknowledged {
                request,
                provider_ref: "x".into()
            }
        ),
        "not_outstanding"
    );

    let case = w.take("P1", "L1", "amy");
    let decided = w.propose(&case, "amy", vec![accept("life", 50, None)]).unwrap();
    let offered = w.automatic(&decided);
    assert!(offered.events.iter().any(|e| matches!(e, Event::OfferMade { .. })));
    assert_eq!(
        w.s.report(w.now - chrono::Duration::days(30), w.now + chrono::Duration::days(1))
            .evidence_cost_minor,
        85_00
    );
}

/// Purpose: evidence that never arrives is chased the set number of times and, once past its
/// deadline, is marked unobtainable and put in front of a person with that reason. A source
/// saying it cannot supply does the same at once. Financial evidence goes to the financial
/// queue. An underwriter can ask for more, and a decision made while evidence is outstanding
/// cancels the requests.
/// Why it matters: a missing GP report must not stall an application for ever, and must not be
/// read as good or bad news. The applicant waits on a person, with interim cover running.
#[test]
fn evidence_that_cannot_be_had_goes_to_a_person_with_the_reason() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    w.consent("P1", "L1", ConsentKind::ObtainRecords);
    w.disclose("P1", "L1", &yes("diabetes"));
    for expected in [1, 2] {
        w.days(7);
        let swept = w.ok("timer", Command::Sweep { after: None, max: 10 });
        assert!(matches!(&swept.events[0], Event::EvidenceChased { chasers, .. } if *chasers == expected));
    }
    w.days(6);
    assert!(w.ok("timer", Command::Sweep { after: None, max: 10 }).events.is_empty(), "inside the deadline");
    w.days(1);
    let gave_up = w.ok("timer", Command::Sweep { after: None, max: 10 });
    assert!(matches!(
        &gave_up.events[0],
        Event::EvidenceSettled {
            status: EvidenceStatus::Unobtainable,
            ..
        }
    ));
    assert!(matches!(&gave_up.effects[0], Effect::CancelEvidence { .. }));
    assert!(life(&w, "P1").open_case().unwrap().reasons.contains("RC-0901"));
    assert!(life(&w, "P1").decisions.is_empty());

    // The underwriter asks for something else; a second request of a kind gets its own id.
    let case = w.take("P1", "L1", "amy");
    let kinds = BTreeSet::from([EvidenceKind::BloodTest, EvidenceKind::GpReport]);
    let more = console(
        &mut w,
        Command::RequestEvidence {
            case: case.clone(),
            underwriter: "amy".into(),
            kinds: kinds.clone(),
        },
    )
    .unwrap();
    let ids: Vec<_> = more
        .effects
        .iter()
        .filter_map(|e| {
            if let Effect::RequestEvidence { request, .. } = e {
                Some(request.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ids, vec!["P1/L1/gp_report", "P1/L1/blood_test/2"]);
    assert!(
        console(
            &mut w,
            Command::RequestEvidence {
                case: case.clone(),
                underwriter: "amy".into(),
                kinds
            }
        )
        .unwrap()
        .effects
        .is_empty(),
        "already outstanding"
    );
    assert_eq!(
        console(
            &mut w,
            Command::RequestEvidence {
                case: case.clone(),
                underwriter: "amy".into(),
                kinds: BTreeSet::new()
            }
        )
        .unwrap_err()
        .code,
        "consent_required"
    );
    assert_eq!(
        console(
            &mut w,
            Command::RequestEvidence {
                case: case.clone(),
                underwriter: "abe".into(),
                kinds: BTreeSet::new()
            }
        )
        .unwrap_err()
        .code,
        "not_owner"
    );
    // One arrives while the case is held: its owner is told, and nothing else changes.
    let arrived = w.ok(
        "evidence",
        Command::EvidenceReceived {
            request: "P1/L1/gp_report".into(),
            vault_ref: "v".into(),
            cost_minor: 0,
        },
    );
    assert!(arrived.effects.is_empty(), "another request is still outstanding");
    let arrived = w.ok(
        "evidence",
        Command::EvidenceUnobtainable {
            request: "P1/L1/blood_test/2".into(),
        },
    );
    assert!(
        matches!(&arrived.effects[0], Effect::Notify { to: Recipient::Underwriter { underwriter }, kind: NoticeKind::EvidenceArrived, .. } if underwriter == "amy")
    );

    // A decision made while evidence is outstanding cancels what is still awaited.
    console(
        &mut w,
        Command::RequestEvidence {
            case: case.clone(),
            underwriter: "amy".into(),
            kinds: BTreeSet::from([EvidenceKind::MedicalExam]),
        },
    )
    .unwrap();
    let decided = w.propose(&case, "amy", vec![standard("life")]).unwrap();
    assert!(decided
        .effects
        .iter()
        .any(|e| matches!(e, Effect::CancelEvidence { request } if request == "P1/L1/medical_exam")));
    assert_eq!(life(&w, "P1").evidence.last().unwrap().status, EvidenceStatus::Cancelled);

    // Financial evidence is read by the financial queue.
    let mut w = World::new();
    w.open("P2", 1_600_000_00);
    w.consent("P2", "L1", ConsentKind::ObtainRecords);
    w.disclose("P2", "L1", &clean());
    w.ok(
        "evidence",
        Command::EvidenceUnobtainable {
            request: "P2/L1/financial_statement".into(),
        },
    );
    let case = life(&w, "P2").open_case().unwrap();
    assert_eq!((case.case_type, case.queue), (CaseType::FinancialEvidence, Queue::Financial));
    // No consent, no request: an underwriter cannot ask either.
    let mut w = World::new();
    w.open("P3", 500_000_00);
    w.disclose("P3", "L1", &yes("heart_condition"));
    let case = w.take("P3", "L1", "amy");
    let ask = Command::RequestEvidence {
        case,
        underwriter: "amy".into(),
        kinds: BTreeSet::from([EvidenceKind::GpReport]),
    };
    assert_eq!(console(&mut w, ask).unwrap_err().code, "consent_required");
}

/// Purpose: a referral sits in a skill queue until someone who works that queue and is
/// accredited takes it; one owner at a time; a limit on cases held; handover by the owner or a
/// lead; untouched cases escalate to the lead and then the manager; chasers are recorded and
/// raise priority; and the worklist is ordered by priority.
/// Why it matters: referral queues are where applications stall. Ownership, the work-in-progress
/// limit and escalation are what keep a case from being nobody's.
#[test]
fn referral_queues_have_one_owner_limits_handovers_and_escalation() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let referred = w.disclose("P1", "L1", &yes("hazardous_occupation"));
    assert!(matches!(
        &referred.events[1],
        Event::Referred {
            case_type: CaseType::Referral,
            queue: Queue::Occupational,
            ..
        }
    ));
    let case = w.case_of("P1", "L1");
    let take = |who: &str| Command::TakeCase {
        case: "P1/L1/case/1".into(),
        underwriter: who.into(),
    };
    assert_eq!(console(&mut w, take("nobody")).unwrap_err().code, "no_profile");
    assert_eq!(console(&mut w, take("ash")).unwrap_err().code, "wrong_queue");
    assert_eq!(
        console(
            &mut w,
            Command::TakeCase {
                case: "nope".into(),
                underwriter: "amy".into()
            }
        )
        .unwrap_err()
        .code,
        "unknown_case"
    );

    // Untouched: the lead after 8 hours, the manager after 24.
    w.hours(8);
    let swept = w.ok("timer", Command::Sweep { after: None, max: 10 });
    assert!(matches!(&swept.events[0], Event::CaseEscalated { to: Escalation::QueueLead, .. }));
    assert!(matches!(
        &swept.effects[0],
        Effect::Notify {
            to: Recipient::QueueLead { queue: Queue::Occupational },
            kind: NoticeKind::CaseEscalated,
            ..
        }
    ));
    w.hours(16);
    let swept = w.ok("timer", Command::Sweep { after: None, max: 10 });
    assert!(matches!(
        &swept.effects[0],
        Effect::Notify {
            to: Recipient::UnderwritingManager,
            ..
        }
    ));
    assert!(matches!(
        w.ok("timer", Command::Sweep { after: None, max: 10 }).view,
        Some(ic_underwriting::command::View::Sweep { escalated: 0, .. })
    ));

    w.ok(
        "new-business",
        Command::Chaser {
            proposal: "P1".into(),
            life: "L1".into(),
            from: ChaserFrom::Customer,
        },
    );
    assert_eq!(
        w.err(
            "new-business",
            Command::Chaser {
                proposal: "P1".into(),
                life: "L1".into(),
                from: ChaserFrom::Adviser
            }
        ),
        "no_adviser"
    );
    w.open("P2", 400_000_00);
    w.disclose("P2", "L1", &yes("hazardous_occupation"));
    let list = w.s.queue(Queue::Occupational, w.now);
    assert_eq!(
        (list.len(), list[0].case.as_str(), list[0].owner.clone()),
        (2, "P1/L1/case/1", None),
        "older, escalated and chased comes first"
    );
    assert!(list[0].priority > list[1].priority && list[0].reasons.contains("RC-0120"));
    assert!(w.s.queue(Queue::Medical, w.now).is_empty());

    console(&mut w, take("amy")).unwrap();
    assert_eq!(console(&mut w, take("abe")).unwrap_err().code, "owned");
    assert_eq!(w.s.workload("amy"), 1);
    // Handover: by the owner, or by a lead; never to someone who cannot hold it.
    let hand = |by: &str, to: &str| Command::HandOver {
        case: "P1/L1/case/1".into(),
        by: by.into(),
        to: to.into(),
    };
    assert_eq!(console(&mut w, hand("abe", "abe")).unwrap_err().code, "not_authorised");
    assert_eq!(console(&mut w, hand("amy", "ash")).unwrap_err().code, "wrong_queue");
    assert_eq!(console(&mut w, hand("amy", "amy")).unwrap_err().code, "owned");
    let handed = console(&mut w, hand("amy", "abe")).unwrap();
    assert!(matches!(&handed.events[0], Event::CaseAssigned { owner, from: Some(from), .. } if owner == "abe" && from == "amy"));
    console(&mut w, hand("sam", "amy")).unwrap();
    assert_eq!((w.s.workload("amy"), w.s.workload("abe")), (1, 0));
    assert_eq!(life(&w, "P1").underwriters(), set(&["abe", "amy"]));

    // One person holds only so many cases.
    let mut tight = Settings {
        version: 2,
        ..Settings::default()
    };
    tight.timers.work_in_progress_limit = 1;
    w.ok(
        "config",
        Command::ActivateSettings {
            settings: Box::new(tight),
            approval: approval(),
        },
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
        "work_in_progress"
    );

    // Moving a case changes what it needs and releases it for the new queue.
    let moved = Command::MoveCase {
        case: case.clone(),
        underwriter: "amy".into(),
        case_type: CaseType::Referral,
        queue: Queue::Medical,
        reasons: set(&["RC-0201"]),
    };
    assert!(matches!(
        &console(&mut w, moved).unwrap().events[0],
        Event::CaseMoved { queue: Queue::Medical, .. }
    ));
    assert_eq!((life(&w, "P1").open_case().unwrap().owner.clone(), w.s.workload("amy")), (None, 0));
    console(&mut w, take("amy")).unwrap();
    let mv = |case_type, reasons: &[&str]| Command::MoveCase {
        case: "P1/L1/case/1".into(),
        underwriter: "amy".into(),
        case_type,
        queue: Queue::SeniorReview,
        reasons: set(reasons),
    };
    assert_eq!(console(&mut w, mv(CaseType::Countersign, &["RC-0201"])).unwrap_err().code, "not_allowed");
    assert_eq!(console(&mut w, mv(CaseType::Referral, &[])).unwrap_err().code, "reasons");
    assert_eq!(console(&mut w, mv(CaseType::Referral, &["RC-7777"])).unwrap_err().code, "unknown_code");

    // A medical officer's opinion is a task inside the case.
    let ask = |of: &str| Command::AskOpinion {
        case: "P1/L1/case/1".into(),
        underwriter: "amy".into(),
        of: of.into(),
    };
    assert_eq!(console(&mut w, ask("nobody")).unwrap_err().code, "no_profile");
    console(&mut w, ask("cat")).unwrap();
    let opinion = |of: &str, r: &str| Command::RecordOpinion {
        case: "P1/L1/case/1".into(),
        of: of.into(),
        vault_ref: r.into(),
    };
    assert_eq!(console(&mut w, opinion("sam", "v")).unwrap_err().code, "no_opinion");
    assert_eq!(console(&mut w, opinion("cat", "")).unwrap_err().code, "no_opinion");
    console(&mut w, opinion("cat", "vault://opinion")).unwrap();
    assert_eq!(console(&mut w, opinion("cat", "v")).unwrap_err().code, "no_opinion");
    assert_eq!(life(&w, "P1").open_case().unwrap().opinions[0].vault_ref.as_deref(), Some("vault://opinion"));
}

/// Purpose: authority is enforced on the decision itself. Terms must cover each cover once and
/// be well formed; an underwriter cannot accept above their limit, load above their level or
/// decline without the authority; a decline takes two signatures, the second from a different
/// senior; a refused second signature sends the case back to its queue with the first decision
/// kept on file as replaced.
/// Why it matters: these are the controls that stop one person binding the insurer beyond what
/// they are trusted with, and stop a decline resting on one person's view.
#[test]
fn decisions_are_held_to_authority_and_declines_take_two_signatures() {
    let mut w = World::new();
    w.open("P1", 1_000_000_00);
    w.disclose("P1", "L1", &yes("heart_condition"));
    assert_eq!(life(&w, "P1").open_case().unwrap().queue, Queue::Medical);
    let case = w.take("P1", "L1", "amy");
    let code = |r: Result<_, ic_underwriting::command::Reject>| r.map(|_: ic_underwriting::command::Applied| ()).unwrap_err().code;

    assert_eq!(code(w.propose(&case, "abe", vec![standard("life")])), "not_owner");
    assert_eq!(code(w.propose(&case, "amy", vec![])), "terms");
    assert_eq!(code(w.propose(&case, "amy", vec![standard("life"), standard("life")])), "terms");
    assert_eq!(code(w.propose(&case, "amy", vec![standard("trauma")])), "terms");
    assert_eq!(code(w.propose(&case, "amy", vec![accept("life", 501, None)])), "terms");
    assert_eq!(code(w.propose(&case, "amy", vec![accept("life", 0, Some(0))])), "terms");
    assert_eq!(code(w.propose(&case, "amy", vec![accept("life", 0, Some(1_000_000_00))])), "terms");
    assert_eq!(
        code(w.propose(
            &case,
            "amy",
            vec![CoverTerms {
                cover: "life".into(),
                outcome: CoverOutcome::UnknownTerms
            }]
        )),
        "terms"
    );
    assert_eq!(
        code(w.propose(
            &case,
            "amy",
            vec![CoverTerms {
                cover: "life".into(),
                outcome: CoverOutcome::Defer { until: w.today() }
            }]
        )),
        "terms"
    );
    let excluded = |code: &str, until| CoverTerms {
        cover: "life".into(),
        outcome: CoverOutcome::Accept {
            loading_percent: 0,
            exclusions: vec![ic_underwriting::codes::AppliedExclusion {
                code: code.into(),
                from: d(2026, 11, 11),
                until,
            }],
            revised_sum_insured_minor: None,
        },
    };
    assert_eq!(code(w.propose(&case, "amy", vec![excluded("EX-7777", None)])), "unknown_code");
    assert_eq!(code(w.propose(&case, "amy", vec![excluded("EX-4001", Some(d(2026, 11, 11)))])), "terms");
    assert_eq!(code(w.propose(&case, "amy", vec![accept("life", 101, None)])), "not_authorised");
    assert_eq!(code(w.propose(&case, "amy", vec![decline("life")])), "not_authorised");
    let reasons = |reasons: &[&str], withheld: &[&str]| Command::ProposeDecision {
        case: "P1/L1/case/1".into(),
        underwriter: "amy".into(),
        terms: vec![standard("life")],
        reasons: set(reasons),
        withheld_reasons: set(withheld),
        notes_ref: None,
        evidence: BTreeSet::new(),
    };
    assert_eq!(console(&mut w, reasons(&[], &[])).unwrap_err().code, "reasons");
    assert_eq!(console(&mut w, reasons(&["RC-0101"], &["RC-7777"])).unwrap_err().code, "unknown_code");

    // A senior proposes a decline: it waits for another senior.
    console(
        &mut w,
        Command::HandOver {
            case: case.clone(),
            by: "amy".into(),
            to: "sam".into(),
        },
    )
    .unwrap();
    let proposed = w.propose(&case, "sam", vec![decline("life")]).unwrap();
    assert!(matches!(
        &proposed.events[0],
        Event::DecisionProposed {
            needs_second_signature: true,
            ..
        }
    ));
    assert!(matches!(
        &proposed.events[1],
        Event::CaseMoved {
            case_type: CaseType::Countersign,
            queue: Queue::SeniorReview,
            ..
        }
    ));
    assert_eq!(
        (w.stage("P1", "L1"), life(&w, "P1").decisions[0].status),
        (Stage::AwaitingCountersign, DecisionStatus::AwaitingCountersign)
    );
    assert!(World::reservations(&proposed).is_empty());
    let take = |who: &str| Command::TakeCase {
        case: "P1/L1/case/1".into(),
        underwriter: who.into(),
    };
    assert_eq!(console(&mut w, take("sam")).unwrap_err().code, "already_signed");
    assert_eq!(console(&mut w, take("amy")).unwrap_err().code, "wrong_queue");
    console(&mut w, take("sue")).unwrap();
    assert_eq!(code(w.propose(&case, "sue", vec![standard("life")])), "not_allowed");
    let sign = |who: &str, agree: bool, reasons: &[&str]| Command::Countersign {
        case: "P1/L1/case/1".into(),
        underwriter: who.into(),
        agree,
        reasons: set(reasons),
        withheld_reasons: BTreeSet::new(),
    };
    assert_eq!(console(&mut w, sign("sam", true, &["RC-0101"])).unwrap_err().code, "not_owner");
    assert_eq!(console(&mut w, sign("sue", true, &[])).unwrap_err().code, "reasons");

    // Refused: back to the medical queue, the first decision kept as replaced.
    let refused = console(&mut w, sign("sue", false, &["RC-0108"])).unwrap();
    assert!(matches!(&refused.events[0], Event::CountersignRefused { .. }));
    assert!(matches!(
        &refused.events[1],
        Event::CaseMoved {
            case_type: CaseType::Referral,
            queue: Queue::Medical,
            ..
        }
    ));
    let f = life(&w, "P1");
    assert_eq!((f.stage, f.decisions[0].status), (Stage::InAssessment, DecisionStatus::Superseded));
    assert!(f.open_case().unwrap().reasons.contains("RC-0940") && f.open_case().unwrap().reasons.contains("RC-0108"));
    assert_eq!(console(&mut w, sign("sue", true, &["RC-0101"])).unwrap_err().code, "not_owner");

    // Proposed again and agreed: the life is declined, told why, and the proposal ends.
    console(&mut w, take("sam")).unwrap();
    w.propose(&case, "sam", vec![decline("life")]).unwrap();
    console(&mut w, take("sue")).unwrap();
    let agreed = console(&mut w, sign("sue", true, &["RC-0107"])).unwrap();
    assert!(agreed.events.iter().any(|e| matches!(e, Event::DecisionMade { .. })));
    assert!(agreed.events.iter().any(|e| matches!(e, Event::ProposalDeclined { .. })));
    let Some(Effect::Notify {
        kind: NoticeKind::Declined,
        reasons,
        ..
    }) = agreed.effects.first()
    else {
        panic!("no decline notice")
    };
    assert_eq!(reasons, &vec!["Your medical history.".to_string(), "A medical test result.".to_string()]);
    let f = life(&w, "P1");
    assert_eq!((f.stage, f.decisions[1].signatures.len()), (Stage::Declined, 2));
    assert!(matches!(w.p("P1").status, ProposalStatus::Declined { .. }));
    assert_eq!(code(w.propose(&case, "sam", vec![standard("life")])), "closed");
    assert_eq!(console(&mut w, sign("sue", true, &["RC-0101"])).unwrap_err().code, "closed");
    assert_eq!(console(&mut w, take("sue")).unwrap_err().code, "case_closed");
    assert_eq!(
        console(
            &mut w,
            Command::HandOver {
                case,
                by: "sam".into(),
                to: "sue".into()
            }
        )
        .unwrap_err()
        .code,
        "case_closed"
    );
}

/// Purpose: the rules never decline or accept a large amount alone. A rule decline opens a
/// decline review; an amount above the straight-through limit goes to a senior even when the
/// answers are clean; an accepted amount or loading above the countersign thresholds takes a
/// second signature; and an amount above the senior's own limit cannot be countersigned by them.
/// Why it matters: these thresholds are the dual sign-off the design requires on large risks,
/// and the reason a rule change cannot start declining people with no human seeing it.
#[test]
fn large_amounts_and_rule_declines_always_reach_people() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    w.disclose("P1", "L1", &yes("awaiting_tests_or_results"));
    let c = life(&w, "P1").open_case().unwrap();
    assert_eq!((c.case_type, c.queue), (CaseType::DeclineReview, Queue::Medical));
    assert!(life(&w, "P1").decisions.is_empty());
    let case = w.take("P1", "L1", "amy");
    let deferred = w
        .propose(
            &case,
            "amy",
            vec![CoverTerms {
                cover: "life".into(),
                outcome: CoverOutcome::Defer { until: d(2027, 6, 1) },
            }],
        )
        .unwrap();
    assert!(matches!(
        &deferred.events[0],
        Event::DecisionProposed {
            needs_second_signature: true,
            ..
        }
    ));

    // Clean answers, but above the straight-through limit and the countersign threshold.
    let mut w = World::new();
    w.open("P2", 2_500_000_00);
    w.consent("P2", "L1", ConsentKind::ObtainRecords);
    w.disclose("P2", "L1", &clean());
    w.ok(
        "evidence",
        Command::EvidenceReceived {
            request: "P2/L1/financial_statement".into(),
            vault_ref: "v".into(),
            cost_minor: 0,
        },
    );
    let case = w.take("P2", "L1", "sam");
    let proposed = w.propose(&case, "sam", vec![standard("life")]).unwrap();
    assert!(matches!(
        &proposed.events[0],
        Event::DecisionProposed {
            needs_second_signature: true,
            ..
        }
    ));
    w.take("P2", "L1", "sue");
    let agreed = w.ok(
        "underwriting-console",
        Command::Countersign {
            case,
            underwriter: "sue".into(),
            agree: true,
            reasons: set(&["RC-0130"]),
            withheld_reasons: set(&["RC-0190"]),
        },
    );
    assert_eq!(World::reservations(&agreed), vec!["P2/L1/life/1".to_string()]);
    assert_eq!(life(&w, "P2").decisions[0].signatures[1].withheld_reasons, set(&["RC-0190"]));

    // No evidence called for, but still too large for the rules alone.
    let mut w = World::new();
    let open = w.open_command("P3", &["L1"], &[("L1", "health", "health", 1_200_000_00)], false);
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    w.disclose("P3", "L1", &clean());
    let c = life(&w, "P3").open_case().unwrap();
    assert_eq!((c.case_type, c.queue), (CaseType::Referral, Queue::SeniorReview));
    assert!(c.reasons.contains("RC-0203"));
    // Beyond the senior's own health limit: they cannot decide it, and a heavy loading takes two.
    let case = w.take("P3", "L1", "sam");
    assert_eq!(w.propose(&case, "sam", vec![standard("health")]).unwrap_err().code, "not_authorised");
    let reduced = w.propose(&case, "sam", vec![accept("health", 150, Some(900_000_00))]).unwrap();
    assert!(matches!(
        &reduced.events[0],
        Event::DecisionProposed {
            needs_second_signature: true,
            ..
        }
    ));
}

/// Purpose: tele-underwriting. The rules ask for an interview; it is booked and held by an
/// underwriter; what was said is the underwriter's note until the life confirms it with a
/// signed step over the same hash; then it is disclosure and the rules run on it without asking
/// for another interview. The adviser is on the call only with the life's consent.
/// Why it matters: interview answers are relied on at claim time exactly as written answers
/// are. An unconfirmed note, or a confirmation of different words, cannot be evidence of what
/// the person disclosed.
#[test]
fn interview_answers_count_only_once_the_life_confirms_them() {
    let mut w = World::new();
    let open = w.open_command("P1", &["L1"], &[("L1", "life", "life", 500_000_00)], true);
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let answers = yes("mental_health_last_5_years");
    let referred = w.disclose("P1", "L1", &answers);
    assert!(matches!(
        &referred.events[1],
        Event::Referred {
            case_type: CaseType::TeleInterview,
            queue: Queue::TeleInterview,
            ..
        }
    ));
    let when = w.now + chrono::Duration::days(2);
    let book = |life: &str| Command::ScheduleInterview {
        proposal: "P1".into(),
        life: life.into(),
        scheduled_for: when,
        booked_by: "amy".into(),
    };
    assert_eq!(w.err("new-business", book("L9")), "unknown_life");
    let prepared = w.prepared("P1", "L1", &answers);
    let record = |who: &str, hash: &str, adviser: bool| Command::RecordInterview {
        case: "P1/L1/case/1".into(),
        underwriter: who.into(),
        notes_ref: "vault://notes".into(),
        notes_hash: hash.into(),
        recording_ref: Some("vault://recording".into()),
        present: vec!["interpreter".into()],
        adviser_present: adviser,
    };
    let case = w.take("P1", "L1", "ash");
    assert_eq!(console(&mut w, record("ash", &prepared.hash, false)).unwrap_err().code, "no_interview");
    w.ok("new-business", book("L1"));
    assert_eq!(console(&mut w, record("amy", &prepared.hash, false)).unwrap_err().code, "not_owner");
    assert_eq!(console(&mut w, record("ash", &prepared.hash, true)).unwrap_err().code, "adviser_not_permitted");
    assert_eq!(console(&mut w, record("ash", "short", false)).unwrap_err().code, "disclosure");

    let confirm = |w: &World, hash: &str, domain: &str| {
        let mut d = w.disclosure("P1", "L1", &answers);
        d.hash = hash.into();
        d.signing = SigningProof::Device(signed(domain, &["P1", "L1", hash]));
        Command::ConfirmInterview(Box::new(d))
    };
    assert_eq!(w.err("new-business", confirm(&w, &prepared.hash, "interview")), "no_interview");
    let recorded = console(&mut w, record("ash", &prepared.hash, false)).unwrap();
    assert!(matches!(
        &recorded.effects[0],
        Effect::Notify {
            kind: NoticeKind::InterviewToConfirm,
            ..
        }
    ));
    assert!(life(&w, "P1").disclosures.len() == 1, "a recorded interview is not yet disclosure");
    assert_eq!(w.err("new-business", confirm(&w, &"0".repeat(64), "interview")), "no_interview");
    assert_eq!(w.err("new-business", confirm(&w, &prepared.hash, "disclosure")), "signature");

    let confirmed = w.ok("new-business", confirm(&w, &prepared.hash, "interview"));
    assert!(confirmed.events.iter().any(|e| matches!(e, Event::InterviewConfirmed { .. })));
    assert!(
        confirmed.events.iter().any(|e| matches!(e, Event::CaseClosed { .. })),
        "the rules run and need nobody else"
    );
    let f = life(&w, "P1");
    assert_eq!((f.disclosures.len(), f.stage, f.interviews[0].present.len()), (2, Stage::AwaitingCapacity, 1));
    assert!(f.interviews[0].confirmed_at.is_some() && f.cases[0].reasons.contains("RC-0950"));
    assert_eq!(w.err("new-business", confirm(&w, &prepared.hash, "interview")), "no_interview");
    assert_eq!(w.err("new-business", book("L1")), "not_now");
    assert_eq!(console(&mut w, record("ash", &prepared.hash, false)).unwrap_err().code, "case_closed");
    let _ = case;

    // An interview instead of the form: booked before any disclosure, it opens the case itself,
    // and when the confirmed answers call for evidence the same case carries on as an evidence case.
    let mut w = World::new();
    w.open("P2", 500_000_00);
    w.consent("P2", "L1", ConsentKind::ObtainRecords);
    let booked = w.ok(
        "underwriting-console",
        Command::ScheduleInterview {
            proposal: "P2".into(),
            life: "L1".into(),
            scheduled_for: when,
            booked_by: "ash".into(),
        },
    );
    assert!(matches!(
        &booked.events[1],
        Event::Referred {
            case_type: CaseType::TeleInterview,
            ..
        }
    ));
    w.take("P2", "L1", "ash");
    let answers = yes("diabetes");
    let hash = w.prepared("P2", "L1", &answers).hash;
    let record = Command::RecordInterview {
        case: "P2/L1/case/1".into(),
        underwriter: "ash".into(),
        notes_ref: "v".into(),
        notes_hash: hash.clone(),
        recording_ref: None,
        present: vec![],
        adviser_present: false,
    };
    console(&mut w, record).unwrap();
    let mut d = w.disclosure("P2", "L1", &answers);
    d.signing = SigningProof::Device(signed("interview", &["P2", "L1", &hash]));
    let confirmed = w.ok("new-business", Command::ConfirmInterview(Box::new(d)));
    assert!(confirmed.events.iter().any(|e| matches!(
        e,
        Event::CaseMoved {
            case_type: CaseType::MedicalEvidence,
            queue: Queue::Medical,
            ..
        }
    )));
    assert!(confirmed.effects.iter().any(|e| matches!(
        e,
        Effect::RequestEvidence {
            kind: EvidenceKind::BloodTest,
            ..
        }
    )));
    assert_eq!(w.stage("P2", "L1"), Stage::InAssessment);
    w.ok("new-business", Command::Withdraw { proposal: "P2".into() });
    assert_eq!(
        w.err(
            "underwriting-console",
            Command::ScheduleInterview {
                proposal: "P2".into(),
                life: "L1".into(),
                scheduled_for: when,
                booked_by: "ash".into()
            }
        ),
        "closed"
    );
}

/// Purpose: reinsurance capacity. Above the treaty's automatic limit nothing is offered until
/// the reinsurer's own terms are placed: a liaison case opens, named documents are released
/// with a log entry, and the liaison then decides. If the reinsurer declines, the same amount
/// cannot be decided again; a reduced amount reserves afresh and releases the old reservation.
/// A risk the treaty excludes goes to a senior. Answers for a reservation that has been
/// replaced are refused.
/// Why it matters: an offer made before capacity is confirmed is cover the insurer may have to
/// carry alone. This is the gate that prevents it.
#[test]
fn nothing_is_offered_above_the_treaty_limit_until_the_reinsurer_has_answered() {
    let mut w = World::new();
    w.open("P1", 900_000_00);
    let disclosed = w.disclose("P1", "L1", &clean());
    let first = World::reservations(&disclosed)[0].clone();
    let answer = |r: &str, answer| Command::CapacityAnswered { reservation: r.into(), answer };
    assert_eq!(w.err("reinsurance", answer("nope", CapacityAnswer::Automatic)), "unknown_reservation");
    let referred = w.ok("reinsurance", answer(&first, CapacityAnswer::FacultativeRequired));
    assert!(matches!(&referred.events[0], Event::FacultativeReferral { .. }));
    assert!(matches!(
        &referred.events[1],
        Event::Referred {
            case_type: CaseType::ReinsurerReferral,
            queue: Queue::ReinsurerLiaison,
            ..
        }
    ));
    assert_eq!(w.stage("P1", "L1"), Stage::AwaitingFacultative);
    assert!(life(&w, "P1").offers.is_empty() && life(&w, "P1").effective_decision().is_none());
    assert_eq!(w.err("reinsurance", answer(&first, CapacityAnswer::Automatic)), "not_pending");

    let case = w.take("P1", "L1", "sam");
    assert_eq!(w.propose(&case, "sam", vec![standard("life")]).unwrap_err().code, "awaiting_reinsurer");
    let release = |treaty: &str, docs: Vec<String>| Command::ReleaseToReinsurer {
        case: "P1/L1/case/1".into(),
        underwriter: "sam".into(),
        treaty: treaty.into(),
        documents: docs,
    };
    assert_eq!(console(&mut w, release("T1", vec![])).unwrap_err().code, "documents");
    let released = console(&mut w, release("T1", vec!["vault://P1/L1/disclosure".into()])).unwrap();
    assert!(matches!(&released.events[0], Event::FileAccessed { who, role, documents, .. } if who == "T1" && role == "reinsurer" && documents.len() == 1));

    let terms = |r: &str, outcome| Command::FacultativeTermsRecorded {
        reservation: r.into(),
        outcome,
    };
    assert_eq!(
        w.err("reinsurance", terms(&first, FacultativeOutcome::Offered { reference: "".into() })),
        "reference"
    );
    w.ok("reinsurance", terms(&first, FacultativeOutcome::Declined));
    assert_eq!(w.err("reinsurance", terms(&first, FacultativeOutcome::Declined)), "not_facultative");
    assert!(life(&w, "P1").open_case().unwrap().reasons.contains("RC-0931"));
    assert_eq!(w.propose(&case, "sam", vec![standard("life")]).unwrap_err().code, "no_capacity");

    // A reduced amount: the old reservation is released and a new one made.
    let reduced = w.propose(&case, "sam", vec![accept("life", 0, Some(600_000_00))]).unwrap();
    assert!(matches!(&reduced.effects[0], Effect::ReleaseCapacity { reservation, .. } if reservation == &first));
    let second = World::reservations(&reduced)[0].clone();
    assert_eq!(second, "P1/L1/life/2");
    assert_eq!(w.err("reinsurance", answer(&first, CapacityAnswer::Automatic)), "stale_reservation");
    // This time the reinsurer places it: the liaison decides with their terms.
    w.ok("reinsurance", answer(&second, CapacityAnswer::FacultativeRequired));
    w.take("P1", "L1", "sam");
    let placed = w.ok("reinsurance", terms(&second, FacultativeOutcome::Offered { reference: "FAC-77".into() }));
    assert!(placed.events.is_empty());
    assert_eq!(life(&w, "P1").capacity["life"].state, CapacityState::Placed { reference: "FAC-77".into() });
    assert!(life(&w, "P1").open_case().unwrap().reasons.contains("RC-0932"));
    let case = w.case_of("P1", "L1");
    assert_eq!(
        console(
            &mut w,
            Command::ReleaseToReinsurer {
                case: case.clone(),
                underwriter: "sam".into(),
                treaty: "T1".into(),
                documents: vec!["d".into()]
            }
        )
        .map(|_| ()),
        Ok(())
    );
    let decided = w.propose(&case, "sam", vec![accept("life", 75, Some(600_000_00))]).unwrap();
    assert!(World::reservations(&decided).is_empty(), "the placed capacity is for this amount");
    assert!(decided.events.iter().any(|e| matches!(e, Event::OfferMade { .. })));
    assert_eq!(life(&w, "P1").decisions.iter().filter(|d| d.status == DecisionStatus::Effective).count(), 1);

    // The treaty excludes the risk: a senior decides what can be done.
    let mut w = World::new();
    let open = w.open_command(
        "P2",
        &["L1"],
        &[("L1", "life", "life", 300_000_00), ("L1", "trauma", "trauma", 100_000_00)],
        false,
    );
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let disclosed = w.disclose("P2", "L1", &clean());
    let keys = World::reservations(&disclosed);
    w.ok("reinsurance", answer(&keys[1], CapacityAnswer::Excluded));
    let c = life(&w, "P2").open_case().unwrap();
    assert_eq!((c.case_type, c.queue), (CaseType::Referral, Queue::SeniorReview));
    assert!(c.reasons.contains("RC-0933"));
    // The other cover's capacity arrives while the case is open: the stage stays with the case.
    w.ok("reinsurance", answer(&keys[0], CapacityAnswer::Automatic));
    assert_eq!(w.stage("P2", "L1"), Stage::InAssessment);
    let case = w.take("P2", "L1", "sam");
    assert_eq!(
        w.propose(&case, "sam", vec![standard("life"), standard("trauma")]).unwrap_err().code,
        "no_capacity"
    );
    let partial = w.propose(&case, "sam", vec![standard("life"), decline("trauma")]).unwrap();
    assert!(matches!(
        &partial.events[0],
        Event::DecisionProposed {
            needs_second_signature: true,
            ..
        }
    ));
    w.take("P2", "L1", "sue");
    let agreed = w.ok(
        "underwriting-console",
        Command::Countersign {
            case,
            underwriter: "sue".into(),
            agree: true,
            reasons: set(&["RC-0933"]),
            withheld_reasons: BTreeSet::new(),
        },
    );
    assert!(agreed
        .effects
        .iter()
        .any(|e| matches!(e, Effect::ReleaseCapacity { reservation, .. } if reservation == &keys[1])));
    assert!(
        agreed.events.iter().any(|e| matches!(e, Event::OfferMade { .. })),
        "part declined is not what was applied for: it is offered"
    );
    let accepted = w.respond("P2", "L1", ic_underwriting::command::OfferResponse::Accept);
    let Some(Effect::IssuePolicy { command, .. }) = accepted.effects.first() else {
        panic!("no issue")
    };
    assert_eq!(command.covers.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(), vec!["life"]);
    let _ = Answer::YesNo(true);
}
