//! A complaint from intake to close: who may handle it, who may see it,
//! what an outcome needs, and what must be done before it is closed.

mod support;

use std::collections::BTreeSet;

use ic_complaints::command::FrontLine;
use ic_complaints::model::*;
use ic_complaints::{Command, Event, Timers};
use support::*;

/// Purpose: the timer configuration is versioned in order, refuses values
/// that cannot be right, and nothing can be recorded before one is active.
/// Why it matters: the timers come from the conduct regime and the dispute
/// scheme's rules. A configuration with a zero period or a deadlock before
/// the decision would make every complaint late or deadlocked at once, and
/// a complaint recorded with no timers would have no deadlines at all.
#[test]
fn timers_are_versioned_and_checked() {
    let mut kit = Kit::new();
    assert_eq!(
        kit.err("intake", "agent-1", Command::RecordComplaint(Box::new(intake("c1", Process::Service)))),
        "no_timers"
    );
    let open = Command::OpenConductCase {
        case: "b1".into(),
        kind: ConductKind::Breach,
        owner: "conduct-officer".into(),
        summary_document: None,
        notify_regulator: false,
    };
    assert_eq!(kit.err("conduct", "conduct-officer", open), "no_timers");

    let bad = |change: fn(&mut Timers)| {
        let mut t = timers();
        change(&mut t);
        Command::ActivateTimers(t)
    };
    assert_eq!(kit.err("config", "ops", bad(|t| t.version = 2)), "version");
    assert_eq!(kit.err("config", "ops", bad(|t| t.acknowledge_days = 0)), "invalid");
    assert_eq!(kit.err("config", "ops", bad(|t| t.regulator_notification_days = 0)), "invalid");
    assert_eq!(kit.err("config", "ops", bad(|t| t.decide_days = 50)), "invalid");
    assert_eq!(kit.err("config", "ops", bad(|t| t.acknowledge_days = 30)), "invalid");
    assert_eq!(kit.err("config", "ops", bad(|t| t.front_line_limit_minor = -1)), "invalid");
    assert_eq!(
        kit.err(
            "config",
            "ops",
            bad(|t| {
                t.ex_gratia_limits_minor.insert(2, -5);
            })
        ),
        "invalid"
    );
    assert_eq!(kit.err("config", "ops", bad(|t| t.cause_codes.clear())), "invalid");

    assert_eq!(
        kit.ok("config", "ops", Command::ActivateTimers(timers())),
        vec![Event::TimersActivated { version: 1 }]
    );
    kit.record(intake("c1", Process::Service));
    let mut longer = timers();
    longer.version = 2;
    longer.decide_days = 25;
    kit.ok("config", "ops", Command::ActivateTimers(longer));
    kit.record(intake("c2", Process::Service));
    // Each complaint keeps the version it was received under.
    assert_eq!((kit.complaint("c1").timers_version, kit.complaint("c2").timers_version), (1, 2));
    assert_eq!(kit.state.timers(1).unwrap().decide_days, 20);
    assert!(kit.state.timers(9).is_none());
}

/// Purpose: one command records a complaint from anyone, on any channel,
/// with what it concerns; an acknowledgement falls due; and an intake that
/// is incomplete, repeated or back-dated is refused.
/// Why it matters: a complaint is recorded wherever it is heard, by
/// whoever hears it. If intake were fussy, complaints would go unrecorded;
/// if it accepted a back-dated one from anyone, the timers the regulator
/// relies on could be falsified.
#[test]
fn a_complaint_is_recorded_by_one_command() {
    let mut kit = Kit::ready();
    let today = kit.today();
    let mut i = claim_intake("c1");
    i.complainant = party("third-party-9");
    i.complainant_kind = ComplainantKind::ThirdParty;
    i.channel = IntakeChannel::SocialMedia;
    i.links.recorded_from = Some("claim-1/call-77".into());
    let events = kit.record(i);
    assert_eq!(
        events,
        vec![
            Event::ComplaintRecorded {
                complaint: "c1".into(),
                complainant: party("third-party-9"),
                process: Process::Claims,
                vulnerable: false
            },
            Event::AcknowledgementDue {
                complaint: "c1".into(),
                by: today + chrono::Duration::days(5)
            },
        ]
    );
    let c = kit.complaint("c1");
    assert_eq!((c.status, c.received_on, c.recorded_by.as_str()), (Status::Open, today, "agent-1"));
    assert_eq!(c.links.recorded_from.as_deref(), Some("claim-1/call-77"));
    assert_eq!(c.involved(), BTreeSet::from(["assessor-1"]));
    assert_eq!(c.authority_needed(), 2);

    assert_eq!(kit.err("intake", "agent-1", Command::RecordComplaint(Box::new(claim_intake("c1")))), "exists");
    let mut no_summary = intake("c2", Process::Service);
    no_summary.summary_document.clear();
    assert_eq!(kit.err("intake", "agent-1", Command::RecordComplaint(Box::new(no_summary))), "invalid");
    assert_eq!(
        kit.err("intake", "agent-1", Command::RecordComplaint(Box::new(intake("", Process::Service)))),
        "invalid"
    );
    let mut odd_cause = intake("c2", Process::Service);
    odd_cause.category.cause = Some("not-on-the-list".into());
    assert_eq!(kit.err("intake", "agent-1", Command::RecordComplaint(Box::new(odd_cause))), "unknown_cause");
    let mut back_dated = intake("c2", Process::Service);
    back_dated.received_on = Some(today - chrono::Duration::days(3));
    assert_eq!(kit.err("intake", "agent-1", Command::RecordComplaint(Box::new(back_dated))), "invalid");

    // The category is refined as the case is worked; representatives are
    // recorded with the complainant's authority.
    kit.ok(
        "complaints",
        "lead",
        Command::RefineCategory {
            complaint: "c1".into(),
            category: Category {
                product: Some("term-life".into()),
                process: Process::Claims,
                cause: Some("wrong_decision".into()),
            },
        },
    );
    assert_eq!(kit.complaint("c1").category.cause.as_deref(), Some("wrong_decision"));
    let refine = Command::RefineCategory {
        complaint: "c1".into(),
        category: Category {
            product: None,
            process: Process::Claims,
            cause: Some("nope".into()),
        },
    };
    assert_eq!(kit.err("complaints", "lead", refine), "unknown_cause");
    let representative = Representative {
        party: party("lawyer-1"),
        relationship: "lawyer".into(),
        authority_document: "doc-authority".into(),
    };
    kit.ok(
        "complaints",
        "lead",
        Command::AddRepresentative {
            complaint: "c1".into(),
            representative: representative.clone(),
        },
    );
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::AddRepresentative {
                complaint: "c1".into(),
                representative: representative.clone()
            }
        ),
        "exists"
    );
    let mut unauthorised = representative;
    unauthorised.party = party("friend");
    unauthorised.authority_document.clear();
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::AddRepresentative {
                complaint: "c1".into(),
                representative: unauthorised
            }
        ),
        "invalid"
    );
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::RecordUpdate {
                complaint: "nothing".into(),
                document: "d".into()
            }
        ),
        "unknown_complaint"
    );
}

/// Purpose: a service complaint put right by the person who heard it is
/// still recorded, with its outcome, cause and what was spent, and is
/// closed at once; anything beyond that small authority is refused.
/// Why it matters: the pattern matters even when the case is closed in a
/// minute, so these must be on the register. And front-line authority is
/// small on purpose: it must not become a way to settle a claims decision
/// or pay out without review.
#[test]
fn a_service_complaint_can_be_resolved_on_the_spot() {
    let mut kit = Kit::ready();
    let today = kit.today();
    let front = |amount: i64| FrontLine {
        outcome: OutcomeKind::ResolvedByAgreement,
        reason_codes: vec!["apology".into()],
        cause: "delay".into(),
        put_right: Some(("payment-55".into(), amount)),
    };
    let mut i = intake("c1", Process::Service);
    i.resolved_on_the_spot = Some(front(5_000));
    let events = kit.record(i);
    assert!(events.contains(&Event::ComplaintClosed {
        complaint: "c1".into(),
        cause: "delay".into(),
        adviser_engagement: None
    }));
    assert!(!events.iter().any(|e| matches!(e, Event::AcknowledgementDue { .. })));
    let c = kit.complaint("c1");
    assert_eq!(c.status, Status::Closed);
    let outcome = c.outcome.as_ref().unwrap();
    assert!(outcome.front_line && outcome.decided_by == "agent-1" && outcome.document.is_none());
    assert_eq!((c.remediation[0].amount_minor, c.remediation[0].completed_on), (5_000, Some(today)));
    assert_eq!(c.closure.as_ref().unwrap().cause, "delay");
    assert!(kit.state.running(c).is_empty());

    let refused = |kit: &mut Kit, change: &dyn Fn(&mut ic_complaints::command::Intake)| {
        let mut i = intake("c2", Process::Service);
        i.resolved_on_the_spot = Some(front(5_000));
        change(&mut i);
        kit.err("intake", "agent-1", Command::RecordComplaint(Box::new(i)))
    };
    assert_eq!(refused(&mut kit, &|i| i.resolved_on_the_spot = Some(front(10_001))), "over_authority");
    assert_eq!(refused(&mut kit, &|i| i.resolved_on_the_spot = Some(front(0))), "over_authority");
    assert_eq!(refused(&mut kit, &|i| i.category.process = Process::Claims), "not_front_line");
    assert_eq!(refused(&mut kit, &|i| i.links.decisions = vec![claim_decision()]), "not_front_line");
    assert_eq!(refused(&mut kit, &|i| i.links.staff = vec!["agent-1".into()]), "separation");
    assert_eq!(refused(&mut kit, &|i| i.resolved_on_the_spot.as_mut().unwrap().reason_codes.clear()), "invalid");
    assert_eq!(
        refused(&mut kit, &|i| i.resolved_on_the_spot.as_mut().unwrap().cause = "unlisted".into()),
        "invalid"
    );
    assert_eq!(
        refused(&mut kit, &|i| i.resolved_on_the_spot.as_mut().unwrap().outcome = OutcomeKind::Withdrawn),
        "invalid"
    );

    // Nothing spent is also a resolution.
    let mut apology = intake("c3", Process::Service);
    apology.resolved_on_the_spot = Some(FrontLine {
        outcome: OutcomeKind::Upheld,
        reason_codes: vec!["apology".into()],
        cause: "poor_explanation".into(),
        put_right: None,
    });
    kit.record(apology);
    assert!(kit.complaint("c3").remediation.is_empty());
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::RecordUpdate {
                complaint: "c3".into(),
                document: "d".into()
            }
        ),
        "closed"
    );
}

/// Purpose: the person who made the decision complained about never
/// handles the complaint, the reviewer's authority is no lower than the
/// decision's, and a complaint marked vulnerable goes only to trained
/// staff.
/// Why it matters: this separation is the whole value of an internal
/// review. A reviewer marking their own work, or a junior reviewing a
/// senior's decline, is a review in name only, and the dispute scheme
/// treats it as none.
#[test]
fn review_is_by_someone_who_was_not_involved() {
    let mut kit = Kit::ready();
    let mut i = claim_intake("c1");
    i.links.staff = vec!["manager-4".into()];
    i.vulnerable = true;
    i.adjustments = vec!["call, do not write".into()];
    kit.record(i);
    let assign = |handler: &str, authority: u8, trained: bool| Command::Assign {
        complaint: "c1".into(),
        handler: handler.into(),
        authority,
        trained_for_vulnerability: trained,
    };
    assert_eq!(kit.err("complaints", "lead", assign("assessor-1", 5, true)), "separation");
    assert_eq!(kit.err("complaints", "lead", assign("manager-4", 5, true)), "separation");
    assert_eq!(kit.err("complaints", "lead", assign("reviewer-1", 1, true)), "authority");
    assert_eq!(kit.err("complaints", "lead", assign("reviewer-1", 3, false)), "not_trained");
    assert_eq!(kit.err("complaints", "lead", assign("", 3, true)), "invalid");
    assert_eq!(
        kit.ok("complaints", "lead", assign("reviewer-1", 2, true)),
        vec![Event::ComplaintAssigned {
            complaint: "c1".into(),
            handler: "reviewer-1".into()
        }]
    );
    assert_eq!(
        (kit.complaint("c1").owner.as_deref(), kit.complaint("c1").owner_authority),
        (Some("reviewer-1"), 2)
    );

    // Only the assigned reviewer decides, and the marker is a person's call.
    let decide = Command::Decide {
        complaint: "c1".into(),
        outcome: OutcomeKind::NotUpheld,
        reason_codes: vec!["r".into()],
        document: Some("doc".into()),
    };
    assert_eq!(kit.err("complaints", "lead", decide.clone()), "not_owner");
    assert_eq!(kit.err("complaints", "assessor-1", decide), "not_owner");
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::SetVulnerability {
            complaint: "c1".into(),
            vulnerable: false,
            adjustments: vec![],
        },
    );
    assert!(!kit.complaint("c1").vulnerable);
    kit.ok("complaints", "lead", assign("reviewer-2", 4, false));
    assert_eq!(kit.complaint("c1").owner.as_deref(), Some("reviewer-2"));
}

/// Purpose: access to the complaint file is granted by the case, for a
/// purpose; clinical information only to the assigned reviewer of a claims
/// or underwriting decision; and it ends when the handler changes or the
/// complaint closes.
/// Why it matters: a complaint file holds the most sensitive information
/// the insurer has about a person. Standing access to it, or clinical
/// detail open to anyone on the team, is exactly what the privacy law and
/// the design forbid.
#[test]
fn access_is_granted_by_the_case_and_ends_with_it() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    let ordinary = BTreeSet::from([DataClass::Personal, DataClass::Financial, DataClass::InsuranceSensitive]);
    assert_eq!(kit.state.access("c1", "reviewer-1"), ordinary);
    assert!(kit.state.access("c1", "lead").is_empty());
    assert!(kit.state.access("nothing", "reviewer-1").is_empty());

    let grant = |to: &str, classes: &[DataClass], purpose: &str| Command::GrantAccess {
        complaint: "c1".into(),
        to: to.into(),
        classes: classes.iter().copied().collect(),
        purpose: purpose.into(),
    };
    let events = kit.ok(
        "complaints",
        "lead",
        grant("reviewer-1", &[DataClass::Personal, DataClass::Clinical], "review of the declined claim"),
    );
    assert_eq!(
        events,
        vec![Event::AccessGranted {
            complaint: "c1".into(),
            to: "reviewer-1".into(),
            clinical: true,
            purpose: "review of the declined claim".into()
        }]
    );
    assert!(kit.state.access("c1", "reviewer-1").contains(&DataClass::Clinical));

    assert_eq!(
        kit.err("complaints", "lead", grant("helper-1", &[DataClass::Clinical], "second opinion")),
        "clinical"
    );
    assert_eq!(
        kit.err("complaints", "lead", grant("assessor-1", &[DataClass::Personal], "to respond")),
        "separation"
    );
    assert_eq!(kit.err("complaints", "lead", grant("lead", &[DataClass::Personal], "oversight")), "separation");
    assert_eq!(kit.err("complaints", "lead", grant("helper-1", &[DataClass::Personal], "")), "invalid");
    assert_eq!(kit.err("complaints", "lead", grant("helper-1", &[], "help")), "invalid");
    kit.ok("complaints", "lead", grant("helper-1", &[DataClass::Personal], "drafting the letter"));
    assert_eq!(
        kit.ok(
            "complaints",
            "lead",
            Command::RevokeAccess {
                complaint: "c1".into(),
                from: "helper-1".into()
            }
        ),
        vec![Event::AccessRevoked {
            complaint: "c1".into(),
            from: "helper-1".into()
        }]
    );
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::RevokeAccess {
                complaint: "c1".into(),
                from: "helper-1".into()
            }
        ),
        "no_grant"
    );

    // A service complaint has no clinical file to open, whoever asks.
    kit.record(intake("c2", Process::Service));
    kit.assign("c2", "reviewer-1", 1);
    let clinical = Command::GrantAccess {
        complaint: "c2".into(),
        to: "reviewer-1".into(),
        classes: BTreeSet::from([DataClass::Clinical]),
        purpose: "review".into(),
    };
    assert_eq!(kit.err("complaints", "lead", clinical), "clinical");

    // A new handler: the old one's access is gone.
    kit.assign("c1", "reviewer-2", 3);
    assert!(kit.state.access("c1", "reviewer-1").is_empty());
    assert_eq!(kit.state.access("c1", "reviewer-2"), ordinary);
    // Closed: nobody's access survives.
    kit.ok(
        "complaints",
        "reviewer-2",
        Command::Decide {
            complaint: "c1".into(),
            outcome: OutcomeKind::Withdrawn,
            reason_codes: vec!["withdrawn_by_complainant".into()],
            document: None,
        },
    );
    kit.ok(
        "complaints",
        "reviewer-2",
        Command::Close {
            complaint: "c1".into(),
            detail: detail("poor_explanation"),
        },
    );
    assert!(kit.state.access("c1", "reviewer-2").is_empty());
}

/// Purpose: an outcome is one of the five, with reason codes and a written
/// decision; upholding names the decisions overturned; and the letter's
/// proof of delivery is recorded against it.
/// Why it matters: the written outcome with reasons is what the
/// complainant takes to the dispute scheme, and the overturned decision is
/// what the claims function must reopen. An outcome without either is a
/// complaint that was ended, not resolved.
#[test]
fn an_outcome_has_reasons_a_letter_and_proof_of_delivery() {
    let mut kit = Kit::ready();
    let today = kit.today();
    kit.in_review("c1");
    let decide = |outcome, codes: &[&str], document: Option<&str>| Command::Decide {
        complaint: "c1".into(),
        outcome,
        reason_codes: codes.iter().map(|c| c.to_string()).collect(),
        document: document.map(str::to_string),
    };
    assert_eq!(kit.err("complaints", "reviewer-1", decide(OutcomeKind::Upheld, &[], Some("doc"))), "invalid");
    assert_eq!(kit.err("complaints", "reviewer-1", decide(OutcomeKind::Upheld, &[""], Some("doc"))), "invalid");
    assert_eq!(kit.err("complaints", "reviewer-1", decide(OutcomeKind::Upheld, &["r"], None)), "invalid");
    assert_eq!(kit.err("complaints", "reviewer-1", decide(OutcomeKind::NotUpheld, &["r"], Some(""))), "invalid");
    let delivered = |on| Command::RecordOutcomeDelivered { complaint: "c1".into(), on };
    assert_eq!(kit.err("documents", "svc", delivered(today)), "no_outcome");

    let events = kit.decide("c1", OutcomeKind::PartlyUpheld);
    assert_eq!(
        events,
        vec![Event::Decided {
            complaint: "c1".into(),
            outcome: OutcomeKind::PartlyUpheld,
            overturned: vec!["claim-1:decline".into()]
        }]
    );
    assert_eq!(kit.complaint("c1").status, Status::Decided);
    assert_eq!(
        kit.err("complaints", "reviewer-1", decide(OutcomeKind::Upheld, &["r"], Some("doc"))),
        "already_decided"
    );

    kit.advance_days(2);
    assert_eq!(kit.err("documents", "svc", delivered(today - chrono::Duration::days(1))), "invalid");
    assert_eq!(kit.err("documents", "svc", delivered(kit.today() + chrono::Duration::days(1))), "invalid");
    let on = kit.today();
    assert_eq!(
        kit.ok("documents", "svc", delivered(on)),
        vec![Event::OutcomeDelivered { complaint: "c1".into(), on }]
    );
    assert_eq!(kit.err("documents", "svc", delivered(on)), "no_outcome");

    // Not upheld overturns nothing.
    kit.in_review("c2");
    let events = kit.decide("c2", OutcomeKind::NotUpheld);
    assert_eq!(
        events,
        vec![Event::Decided {
            complaint: "c2".into(),
            outcome: OutcomeKind::NotUpheld,
            overturned: vec![]
        }]
    );
}

/// Purpose: remediation is recorded only after an outcome in the
/// complainant's favour, as a link to the ordinary command that did it,
/// with goodwill payments inside the authoriser's limit and never
/// authorised by the person complained about.
/// Why it matters: nothing is adjusted outside the system. The complaint
/// holds the reference to the refund or reopened claim, not a side
/// payment, and an ex gratia limit that could be exceeded here would be
/// an uncontrolled way to pay money out.
#[test]
fn remediation_follows_an_outcome_through_ordinary_commands() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    let remediate = |kind, reference: &str, amount: i64, by: &str, authority: u8| Command::RecordRemediation {
        complaint: "c1".into(),
        kind,
        reference: reference.into(),
        amount_minor: amount,
        authorised_by: by.into(),
        authority,
    };
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::Refund, "pay-1", 100, "lead", 3)),
        "no_basis"
    );
    kit.decide("c1", OutcomeKind::Upheld);

    let events = kit.ok(
        "complaints",
        "reviewer-1",
        remediate(RemediationKind::DecisionReopened, "claim-1:reopen-2", 0, "lead", 3),
    );
    assert_eq!(
        events,
        vec![Event::RemediationRecorded {
            complaint: "c1".into(),
            kind: RemediationKind::DecisionReopened,
            reference: "claim-1:reopen-2".into(),
            amount_minor: 0
        }]
    );
    kit.ok("complaints", "reviewer-1", remediate(RemediationKind::Refund, "pay-1", 12_000, "lead", 0));
    // Level 2 has no limit of its own: it has level 1's.
    kit.ok("complaints", "reviewer-1", remediate(RemediationKind::ExGratia, "pay-2", 50_000, "lead", 2));
    kit.ok(
        "complaints",
        "reviewer-1",
        remediate(RemediationKind::ExGratia, "pay-3", 500_000, "head-of-claims", 9),
    );

    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::ExGratia, "pay-4", 50_001, "lead", 2)),
        "over_authority"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::ExGratia, "pay-4", 1, "lead", 0)),
        "over_authority"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::Refund, "pay-1", 5, "lead", 3)),
        "exists"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::Refund, "pay-5", 5, "assessor-1", 3)),
        "separation"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::Refund, "pay-5", 0, "lead", 3)),
        "invalid"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::RecordCorrection, "fix-1", 5, "lead", 3)),
        "invalid"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::Refund, "", 5, "lead", 3)),
        "invalid"
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", remediate(RemediationKind::Refund, "pay-5", -5, "lead", 3)),
        "invalid"
    );

    // The cause affects others: a remediation case, and the conduct officer is told.
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::RecordSystemicFinding {
                complaint: "c1".into(),
                remediation_case: String::new()
            }
        ),
        "invalid"
    );
    let events = kit.ok(
        "complaints",
        "reviewer-1",
        Command::RecordSystemicFinding {
            complaint: "c1".into(),
            remediation_case: "rem-9".into(),
        },
    );
    assert_eq!(
        events,
        vec![Event::SystemicFindingRaised {
            complaint: "c1".into(),
            remediation_case: "rem-9".into()
        }]
    );
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::RecordSystemicFinding {
                complaint: "c1".into(),
                remediation_case: "rem-10".into()
            }
        ),
        "already_recorded"
    );

    // A complaint that was not upheld has nothing to put right.
    kit.in_review("c2");
    kit.decide("c2", OutcomeKind::NotUpheld);
    let nothing = Command::RecordRemediation {
        complaint: "c2".into(),
        kind: RemediationKind::Refund,
        reference: "pay-9".into(),
        amount_minor: 5,
        authorised_by: "lead".into(),
        authority: 3,
    };
    assert_eq!(kit.err("complaints", "reviewer-1", nothing), "no_basis");
}

/// Purpose: a complaint closes only with an outcome, proof the outcome
/// letter was delivered, every remediation done and a cause code from the
/// list; an overturned decision is linked to who made it and under which
/// rule and wording.
/// Why it matters: the closed record is what the regulator and the root
/// cause review read. A complaint closed with money still owed, a letter
/// never proven delivered, or no cause, is a gap in both. The link to the
/// assessor and rule is how overturn rates reach the quality loops.
#[test]
fn a_complaint_closes_only_when_everything_is_done() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    let close = |cause: &str| Command::Close {
        complaint: "c1".into(),
        detail: detail(cause),
    };
    assert_eq!(kit.err("complaints", "reviewer-1", close("wrong_decision")), "no_outcome");
    kit.decide("c1", OutcomeKind::Upheld);
    assert_eq!(kit.err("complaints", "reviewer-1", close("wrong_decision")), "not_delivered");
    kit.delivered("c1");
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::RecordRemediation {
            complaint: "c1".into(),
            kind: RemediationKind::DecisionReopened,
            reference: "claim-1:reopen-2".into(),
            amount_minor: 0,
            authorised_by: "lead".into(),
            authority: 3,
        },
    );
    assert_eq!(kit.err("complaints", "reviewer-1", close("wrong_decision")), "remediation_outstanding");
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::CompleteRemediation {
                complaint: "c1".into(),
                reference: "other".into()
            }
        ),
        "no_remediation"
    );
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::CompleteRemediation {
            complaint: "c1".into(),
            reference: "claim-1:reopen-2".into(),
        },
    );
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::CompleteRemediation {
                complaint: "c1".into(),
                reference: "claim-1:reopen-2".into()
            }
        ),
        "no_remediation"
    );
    assert_eq!(kit.err("complaints", "reviewer-1", close("because")), "unknown_cause");
    assert_eq!(kit.err("complaints", "assessor-1", close("wrong_decision")), "separation");

    let events = kit.ok("complaints", "reviewer-1", close("wrong_decision"));
    assert_eq!(
        events,
        vec![Event::ComplaintClosed {
            complaint: "c1".into(),
            cause: "wrong_decision".into(),
            adviser_engagement: None
        }]
    );
    let c = kit.complaint("c1");
    let closure = c.closure.as_ref().unwrap();
    assert_eq!(c.status, Status::Closed);
    assert_eq!(
        (closure.rule_version.as_deref(), closure.template_version.as_deref()),
        (Some("claims-7"), Some("wording-3"))
    );
    assert_eq!(closure.staff, vec!["assessor-1".to_string()]);
    assert_eq!(c.category.cause.as_deref(), Some("wrong_decision"));
    assert_eq!(kit.err("complaints", "reviewer-1", close("wrong_decision")), "closed");

    // Not upheld: the decision maker is not recorded against it, and
    // versions given at close are kept as given.
    kit.in_review("c2");
    kit.decide("c2", OutcomeKind::NotUpheld);
    kit.delivered("c2");
    let given = ic_complaints::command::CloseDetail {
        cause: "poor_explanation".into(),
        product_version: Some("term-life-4".into()),
        rule_version: None,
        template_version: Some("decline-letter-2".into()),
    };
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::Close {
            complaint: "c2".into(),
            detail: given,
        },
    );
    let closure = kit.complaint("c2").closure.clone().unwrap();
    assert!(closure.staff.is_empty() && closure.rule_version.is_none());
    assert_eq!(
        (closure.product_version.as_deref(), closure.template_version.as_deref()),
        (Some("term-life-4"), Some("decline-letter-2"))
    );
}
