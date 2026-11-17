//! External forums: the dispute scheme, the courts and the privacy
//! regulator. Referral, the insurer's response, document bundles, the
//! forum's decision and its implementation.

mod support;

use chrono::Duration;
use ic_complaints::model::*;
use ic_complaints::{Command, Event};
use support::*;

fn refer(id: &str, forum: Forum, response_in: i64, documents_in: Option<i64>, kit: &Kit) -> Command {
    Command::Refer {
        complaint: id.into(),
        forum,
        reference: format!("ref-{id}"),
        response_due: kit.today() + Duration::days(response_in),
        documents_due: documents_in.map(|d| kit.today() + Duration::days(d)),
    }
}

fn bundle(id: &str, bundle: &str, documents: &[&str], reviewed_by: &str) -> Command {
    Command::ReleaseBundle {
        complaint: id.into(),
        bundle: bundle.into(),
        documents: documents.iter().map(|d| d.to_string()).collect(),
        reviewed_by: reviewed_by.into(),
    }
}

fn forum_decision(id: &str, outcome: OutcomeKind, binding: bool, due_in: Option<i64>, kit: &Kit) -> Command {
    Command::RecordForumDecision {
        complaint: id.into(),
        outcome,
        binding,
        remediation_due: due_in.map(|d| kit.today() + Duration::days(d)),
    }
}

/// Purpose: a referral to the scheme is recorded once with the forum's
/// reference and deadlines still ahead, puts the complaint with the forum,
/// and replaces the internal timers with the forum's.
/// Why it matters: once the scheme has the complaint its deadlines are the
/// ones that bind the insurer. Internal timers still running would raise
/// escalations nobody can act on, and a referral recorded twice would
/// double-count in the return to the scheme.
#[test]
fn a_referral_puts_the_complaint_with_the_forum_under_its_deadlines() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    let today = kit.today();

    let bad = |change: &dyn Fn(&mut Command)| {
        let mut c = Command::Refer {
            complaint: "c1".into(),
            forum: Forum::Scheme,
            reference: "IFSO-88".into(),
            response_due: today + Duration::days(10),
            documents_due: Some(today + Duration::days(5)),
        };
        change(&mut c);
        c
    };
    let invalid: [&dyn Fn(&mut Command); 3] = [
        &|c| {
            if let Command::Refer { reference, .. } = c {
                reference.clear()
            }
        },
        &|c| {
            if let Command::Refer { response_due, .. } = c {
                *response_due = today - Duration::days(1)
            }
        },
        &|c| {
            if let Command::Refer { documents_due, .. } = c {
                *documents_due = Some(today - Duration::days(1))
            }
        },
    ];
    for change in invalid {
        assert_eq!(kit.err("complaints", "reviewer-1", bad(change)), "invalid");
    }
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            bad(&|c| {
                if let Command::Refer { complaint, .. } = c {
                    *complaint = "nobody".into()
                }
            })
        ),
        "unknown_complaint"
    );
    // Nothing about a forum can be recorded before the referral.
    assert_eq!(
        kit.err("complaints", "reviewer-1", Command::RecordForumResponse { complaint: "c1".into() }),
        "not_referred"
    );
    assert_eq!(kit.err("complaints", "reviewer-1", bundle("c1", "b1", &["doc-1"], "privacy-1")), "not_referred");
    assert_eq!(
        kit.err("complaints", "reviewer-1", forum_decision("c1", OutcomeKind::NotUpheld, false, None, &kit)),
        "not_referred"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", Command::CompleteForumRemediation { complaint: "c1".into() }),
        "nothing_owed"
    );

    assert_eq!(
        kit.ok("complaints", "reviewer-1", bad(&|_| {})),
        vec![Event::Referred {
            complaint: "c1".into(),
            forum: Forum::Scheme,
            reference: "IFSO-88".into()
        }]
    );
    assert_eq!(kit.err("complaints", "reviewer-1", bad(&|_| {})), "already_referred");
    let c = kit.complaint("c1");
    assert_eq!(c.status, Status::Referred);
    assert_eq!(c.owner.as_deref(), Some("reviewer-1"), "a scheme referral stays with its handler");
    assert_eq!(
        kit.state.running(c),
        vec![
            (Timer::ForumResponse, today + Duration::days(10)),
            (Timer::ForumDocuments, today + Duration::days(5))
        ]
    );
    // The insurer's own outcome is no longer its to give, and it cannot close.
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::Decide {
                complaint: "c1".into(),
                outcome: OutcomeKind::Upheld,
                reason_codes: vec!["r".into()],
                document: Some("doc".into())
            }
        ),
        "already_decided"
    );
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::Close {
                complaint: "c1".into(),
                detail: detail("delay")
            }
        ),
        "with_forum"
    );
    assert_eq!(kit.state.subject_record("c1").unwrap().forum, Some((Forum::Scheme, "IFSO-88".into())));
}

/// Purpose: the forum's deadlines escalate like any other, and stop when
/// the response is recorded and the first bundle is released.
/// Why it matters: missing the scheme's deadline can decide a dispute
/// against the insurer by default. The timers must stop only on the act
/// itself, recorded once.
#[test]
fn forum_deadlines_escalate_until_they_are_met() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    kit.decide("c1", OutcomeKind::NotUpheld);
    kit.delivered("c1");
    let today = kit.today();
    let command = refer("c1", Forum::Scheme, 6, Some(4), &kit);
    kit.ok("complaints", "reviewer-1", command);

    kit.advance_days(5);
    let events = kit.tick();
    assert_eq!(
        events,
        vec![
            Event::TimerEscalated {
                subject: "c1".into(),
                timer: Timer::ForumResponse,
                level: Level::Warning,
                due: today + Duration::days(6)
            },
            Event::TimerEscalated {
                subject: "c1".into(),
                timer: Timer::ForumDocuments,
                level: Level::Lead,
                due: today + Duration::days(4)
            },
        ]
    );

    kit.ok("complaints", "reviewer-1", Command::RecordForumResponse { complaint: "c1".into() });
    assert_eq!(kit.complaint("c1").referral.as_ref().unwrap().responded_on, Some(kit.today()));
    assert_eq!(
        kit.err("legal", "counsel-1", Command::RecordForumResponse { complaint: "c1".into() }),
        "already_recorded"
    );
    kit.ok("complaints", "reviewer-1", bundle("c1", "b1", &["doc-claim-file"], "privacy-1"));
    assert!(kit.state.running(kit.complaint("c1")).is_empty());
    kit.advance_days(30);
    assert!(kit.tick().is_empty());
}

/// Purpose: a bundle for the forum names its documents, is checked by one
/// person and released by another, and is released once.
/// Why it matters: a bundle leaves the building with clinical and personal
/// information in it. Two people, on record, is the control against
/// sending the scheme something it has no right to see.
#[test]
fn a_bundle_is_reviewed_by_one_person_and_released_by_another() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    let command = refer("c1", Forum::Scheme, 10, Some(10), &kit);
    kit.ok("complaints", "reviewer-1", command);

    assert_eq!(kit.err("complaints", "reviewer-1", bundle("c1", "", &["d"], "privacy-1")), "invalid");
    assert_eq!(kit.err("complaints", "reviewer-1", bundle("c1", "b1", &[], "privacy-1")), "invalid");
    assert_eq!(kit.err("complaints", "reviewer-1", bundle("c1", "b1", &["d", ""], "privacy-1")), "invalid");
    assert_eq!(kit.err("complaints", "reviewer-1", bundle("c1", "b1", &["d"], "reviewer-1")), "separation");
    assert_eq!(kit.err("complaints", "reviewer-1", bundle("c1", "b1", &["d"], "")), "separation");

    assert_eq!(
        kit.ok("complaints", "reviewer-1", bundle("c1", "b1", &["doc-a", "doc-b"], "privacy-1")),
        vec![Event::BundleReleased {
            complaint: "c1".into(),
            bundle: "b1".into(),
            documents: 2
        }]
    );
    assert_eq!(kit.err("complaints", "reviewer-1", bundle("c1", "b1", &["doc-c"], "privacy-1")), "exists");
    kit.ok("legal", "counsel-1", bundle("c1", "b2", &["doc-c"], "privacy-1"));
    assert_eq!(
        kit.complaint("c1").referral.as_ref().unwrap().bundles[0],
        Bundle {
            id: "b1".into(),
            documents: vec!["doc-a".into(), "doc-b".into()],
            reviewed_by: "privacy-1".into(),
            released_by: "reviewer-1".into(),
            released_on: kit.today(),
        }
    );
    assert_eq!(kit.complaint("c1").referral.as_ref().unwrap().bundles.len(), 2);
}

/// Purpose: a binding decision in the complainant's favour carries a date
/// and is implemented as remediation through ordinary commands, on a timer,
/// before the complaint can close; the closed record links the overturned
/// decision to who made it and under which rule and wording.
/// Why it matters: the scheme's decision binds the insurer. A complaint
/// closed with the decision unimplemented is a breach of the scheme's
/// rules, and an overturn that never reaches the claims quality loop will
/// be made again.
#[test]
fn a_binding_decision_is_implemented_before_the_complaint_closes() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    kit.decide("c1", OutcomeKind::NotUpheld);
    kit.delivered("c1");
    let command = refer("c1", Forum::Scheme, 10, None, &kit);
    kit.ok("complaints", "reviewer-1", command);
    kit.ok("complaints", "reviewer-1", Command::RecordForumResponse { complaint: "c1".into() });
    let today = kit.today();

    let remediate = || Command::RecordRemediation {
        complaint: "c1".into(),
        kind: RemediationKind::DecisionReopened,
        reference: "claim-1:reopen-1".into(),
        amount_minor: 0,
        authorised_by: "lead".into(),
        authority: 3,
    };
    assert_eq!(kit.err("complaints", "reviewer-1", remediate()), "no_basis");

    // A binding decision to put something right has a date; no other has.
    for (outcome, binding, due) in [
        (OutcomeKind::Upheld, true, None),
        (OutcomeKind::Upheld, false, Some(10)),
        (OutcomeKind::NotUpheld, true, Some(10)),
        (OutcomeKind::Upheld, true, Some(-1)),
    ] {
        let command = forum_decision("c1", outcome, binding, due, &kit);
        assert_eq!(kit.err("complaints", "reviewer-1", command), "invalid");
    }
    let command = forum_decision("c1", OutcomeKind::Upheld, true, Some(15), &kit);
    assert_eq!(
        kit.ok("complaints", "reviewer-1", command.clone()),
        vec![Event::ForumDecided {
            complaint: "c1".into(),
            outcome: OutcomeKind::Upheld,
            binding: true
        }]
    );
    assert_eq!(kit.err("complaints", "reviewer-1", command), "already_decided");
    assert_eq!(
        kit.state.running(kit.complaint("c1")),
        vec![(Timer::ForumRemediation, today + Duration::days(15))]
    );

    let close = || Command::Close {
        complaint: "c1".into(),
        detail: detail("wrong_decision"),
    };
    let implemented = || Command::CompleteForumRemediation { complaint: "c1".into() };
    assert_eq!(kit.err("complaints", "reviewer-1", close()), "remediation_outstanding");
    assert_eq!(kit.err("complaints", "reviewer-1", implemented()), "remediation_outstanding");
    kit.ok("complaints", "reviewer-1", remediate());
    assert_eq!(kit.err("complaints", "reviewer-1", implemented()), "remediation_outstanding");
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::CompleteRemediation {
            complaint: "c1".into(),
            reference: "claim-1:reopen-1".into(),
        },
    );
    assert_eq!(kit.err("complaints", "reviewer-1", close()), "remediation_outstanding");

    // Left undone, the date escalates.
    kit.advance_days(16);
    assert_eq!(
        kit.tick(),
        vec![Event::TimerEscalated {
            subject: "c1".into(),
            timer: Timer::ForumRemediation,
            level: Level::Lead,
            due: today + Duration::days(15)
        }]
    );
    kit.ok("legal", "counsel-1", implemented());
    assert_eq!(kit.err("legal", "counsel-1", implemented()), "nothing_owed");
    assert!(kit.state.running(kit.complaint("c1")).is_empty());

    assert_eq!(
        kit.ok("complaints", "reviewer-1", close()),
        vec![Event::ComplaintClosed {
            complaint: "c1".into(),
            cause: "wrong_decision".into(),
            adviser_engagement: None
        }]
    );
    let closure = kit.complaint("c1").closure.clone().unwrap();
    assert_eq!(closure.staff, vec!["assessor-1".to_string()]);
    assert_eq!(closure.rule_version.as_deref(), Some("claims-7"));
    assert_eq!(closure.template_version.as_deref(), Some("wording-3"));

    let report = kit.state.report(today, kit.today());
    assert_eq!(report.referrals[&Forum::Scheme], 1);
    assert_eq!(report.forum_outcomes[&OutcomeKind::Upheld], 1);
    assert_eq!(report.by_decision_maker["assessor-1"].overturned, 1);
    assert_eq!(report.by_rule_version["claims-7"].overturned, 1);
    let row = &kit.state.register(today, kit.today())[0];
    assert_eq!((row.forum, row.forum_outcome), (Some(Forum::Scheme), Some(OutcomeKind::Upheld)));
}

/// Purpose: a deadlocked complaint that went to the scheme without an
/// outcome of the insurer's own closes on the scheme's decision, and a
/// decision that does not bind gives no basis for remediation.
/// Why it matters: the forum's decision ends the matter whichever way it
/// went. Requiring an internal outcome as well would leave these
/// complaints open for ever; treating a recommendation as an order would
/// pay out money nobody authorised.
#[test]
fn the_forums_decision_ends_a_complaint_that_had_no_outcome() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    // The date to decide by passes without an outcome.
    kit.advance_days(timers().decide_days as i64 + 1);
    kit.ok("complaints", "reviewer-1", Command::DeclareDeadlock { complaint: "c1".into() });
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::RecordDeadlockNotice {
            complaint: "c1".into(),
            document: "doc-deadlock".into(),
        },
    );
    let command = refer("c1", Forum::Scheme, 10, None, &kit);
    kit.ok("complaints", "reviewer-1", command);
    let command = forum_decision("c1", OutcomeKind::PartlyUpheld, false, None, &kit);
    kit.ok("complaints", "reviewer-1", command);
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::RecordRemediation {
                complaint: "c1".into(),
                kind: RemediationKind::RecordCorrection,
                reference: "fix-1".into(),
                amount_minor: 0,
                authorised_by: "lead".into(),
                authority: 3,
            }
        ),
        "no_basis"
    );
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::Close {
            complaint: "c1".into(),
            detail: detail("poor_explanation"),
        },
    );
    let c = kit.complaint("c1");
    assert_eq!(c.status, Status::Closed);
    assert_eq!(c.outcome, None);
    // The forum changed the decision, so the decision maker is linked.
    assert_eq!(c.closure.as_ref().unwrap().staff, vec!["assessor-1".to_string()]);
}

/// Purpose: court proceedings and privacy complaints are recorded only by
/// legal, and the complaint passes to the lawyer who recorded them.
/// Why it matters: litigation is privileged and run by legal. A handler
/// recording a court matter, or keeping ownership of one, would put
/// privileged material in the ordinary complaints queue.
#[test]
fn courts_and_the_privacy_regulator_belong_to_legal() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    kit.in_review("c2");
    for forum in [Forum::Court, Forum::PrivacyRegulator] {
        let command = refer("c1", forum, 10, None, &kit);
        assert_eq!(kit.err("complaints", "reviewer-1", command), "not_allowed");
    }
    let command = refer("c1", Forum::Court, 20, None, &kit);
    kit.ok("legal", "counsel-1", command);
    let command = refer("c2", Forum::PrivacyRegulator, 20, None, &kit);
    kit.ok("legal", "counsel-2", command);
    assert_eq!(kit.complaint("c1").owner.as_deref(), Some("counsel-1"));
    assert_eq!(kit.complaint("c2").owner.as_deref(), Some("counsel-2"));

    let command = forum_decision("c1", OutcomeKind::NotUpheld, true, None, &kit);
    kit.ok("legal", "counsel-1", command);
    kit.ok(
        "complaints",
        "lead",
        Command::Close {
            complaint: "c1".into(),
            detail: detail("wrong_decision"),
        },
    );
    let closure = kit.complaint("c1").closure.clone().unwrap();
    assert!(closure.staff.is_empty(), "a decision that stood is not linked to its maker");
    assert_eq!(closure.rule_version, None);
    let report = kit.state.report(kit.today(), kit.today());
    assert_eq!(report.referrals[&Forum::Court], 1);
    assert_eq!(report.referrals[&Forum::PrivacyRegulator], 1);
    assert_eq!(report.by_decision_maker["assessor-1"].overturned, 0);
    assert_eq!(report.by_decision_maker["assessor-1"].complained_about, 2);
}
