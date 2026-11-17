//! The register and the figures built from it, the views each reader is
//! allowed, the event stream, and the module as the cluster hosts it.

mod support;

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDate};
use ic_complaints::command::FrontLine;
use ic_complaints::model::*;
use ic_complaints::module::{self, Query, Role, NAME};
use ic_complaints::report::{AdviserView, Overturns, QueueItem, RegisterRow, Report};
use ic_complaints::{Applied, Command, ComplaintsState, Event};
use ic_log::StateMachine;
use ic_raft::ModuleState;
use serde_json::{json, Value};
use support::*;

/// Four complaints a week in: `c1` about a declined claim, upheld on day 3
/// with a goodwill payment made, a refund still to pay and a systemic
/// finding; `c2` from the same person, marked vulnerable, never
/// acknowledged; `c3` put right on the spot; `c4` acknowledged late on
/// day 7. One conduct case is open.
fn scenario() -> (Kit, NaiveDate) {
    let mut kit = Kit::ready();
    let start = kit.today();
    let mut c1 = claim_intake("c1");
    c1.links.adviser_engagement = Some("eng-1".into());
    kit.record(c1);
    kit.assign("c1", "reviewer-1", 3);
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::RecordAcknowledgement {
            complaint: "c1".into(),
            document: "doc-ack-c1".into(),
        },
    );
    let mut c2 = intake("c2", Process::Service);
    c2.vulnerable = true;
    c2.channel = IntakeChannel::App;
    c2.adjustments = vec!["interpreter".into()];
    kit.record(c2);
    let mut c3 = intake("c3", Process::Service);
    c3.complainant = party("bill");
    c3.resolved_on_the_spot = Some(FrontLine {
        outcome: OutcomeKind::Upheld,
        reason_codes: vec!["apology".into()],
        cause: "delay".into(),
        put_right: None,
    });
    kit.record(c3);
    let mut c4 = intake("c4", Process::Billing);
    c4.complainant = party("cara");
    kit.record(c4);

    kit.advance_days(3);
    kit.decide("c1", OutcomeKind::Upheld);
    kit.delivered("c1");
    for (kind, reference, amount) in [(RemediationKind::ExGratia, "pay-1", 20_000), (RemediationKind::Refund, "pay-2", 5_000)] {
        kit.ok(
            "complaints",
            "reviewer-1",
            Command::RecordRemediation {
                complaint: "c1".into(),
                kind,
                reference: reference.into(),
                amount_minor: amount,
                authorised_by: "lead".into(),
                authority: 1,
            },
        );
    }
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::CompleteRemediation {
            complaint: "c1".into(),
            reference: "pay-1".into(),
        },
    );
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::RecordSystemicFinding {
            complaint: "c1".into(),
            remediation_case: "rem-1".into(),
        },
    );
    kit.advance_days(4);
    kit.ok(
        "complaints",
        "lead",
        Command::RecordAcknowledgement {
            complaint: "c4".into(),
            document: "doc-ack-c4".into(),
        },
    );
    kit.ok(
        "conduct",
        "conduct-officer",
        Command::OpenConductCase {
            case: "i-1".into(),
            kind: ConductKind::Incident,
            owner: "conduct-officer".into(),
            summary_document: None,
            notify_regulator: false,
        },
    );
    (kit, start)
}

/// Purpose: the register has one row per complaint received in the period,
/// carrying what the regulator's return is built from.
/// Why it matters: every reported figure must trace to the rows that make
/// it up. A row that loses the timing, the outcome or the money paid makes
/// the return unverifiable.
#[test]
fn the_register_has_a_row_for_every_complaint_in_the_period() {
    let (kit, start) = scenario();
    let rows = kit.state.register(start, kit.today());
    assert_eq!(rows.iter().map(|r| r.complaint.as_str()).collect::<Vec<_>>(), vec!["c1", "c2", "c3", "c4"]);
    assert_eq!(
        rows[0],
        RegisterRow {
            complaint: "c1".into(),
            received_on: start,
            channel: IntakeChannel::Call,
            complainant_kind: ComplainantKind::Claimant,
            process: Process::Claims,
            product: Some("term-life".into()),
            cause: None,
            status: Status::Decided,
            outcome: Some(OutcomeKind::Upheld),
            front_line: false,
            vulnerable: false,
            days_to_acknowledge: Some(0),
            days_to_decide: Some(3),
            closed_on: None,
            deadlocked: false,
            forum: None,
            forum_outcome: None,
            remediation_minor: 25_000,
            systemic_case: Some("rem-1".into()),
            timers_version: 1,
        }
    );
    assert!(rows[2].front_line && rows[2].closed_on == Some(start) && rows[2].cause.as_deref() == Some("delay"));
    assert_eq!(rows[3].days_to_acknowledge, Some(7));
    assert_eq!(rows[1].days_to_acknowledge, None);
    // Only what was received in the period.
    assert!(kit.state.register(start + Duration::days(1), kit.today()).is_empty());
    assert_eq!(kit.state.register(start - Duration::days(30), start).len(), 4);
}

/// Purpose: the report counts complaints by process, cause, product and
/// channel, timers met and missed as they stand at the end of the period,
/// outcomes, deadlocks, money paid, repeat and vulnerable complainants,
/// overturns by decision maker and rule, and months with no review.
/// Why it matters: these are the figures given to the conduct regulator,
/// the scheme, the board and the conduct committee. A timer not yet due
/// counted as late, or a missed one not counted, misstates the insurer's
/// conduct either way.
#[test]
fn the_report_counts_what_the_regulator_and_the_board_are_given() {
    let (mut kit, start) = scenario();

    // A week in: nothing is yet late to decide, and c2 is late to acknowledge.
    let early = kit.state.report(start, kit.today());
    assert_eq!((early.acknowledged_on_time, early.acknowledged_late), (2, 2));
    assert_eq!((early.decided_on_time, early.decided_late), (2, 0));
    assert_eq!(early.deadlocks, 0);
    assert!(early.months_without_review.is_empty(), "the month has not ended");
    // As it stood on the day of receipt, nothing was yet missed but c4,
    // whose acknowledgement is known to have come late.
    let day_one = kit.state.report(start, start);
    assert_eq!((day_one.acknowledged_on_time, day_one.acknowledged_late), (2, 1));

    kit.advance_days(38);
    kit.tick();
    let end = kit.today();
    let report = kit.state.report(start, end);
    assert_eq!(
        report,
        Report {
            from: Some(start),
            to: Some(end),
            received: 4,
            by_process: [(Process::Claims, 1), (Process::Service, 2), (Process::Billing, 1)].into(),
            by_cause: [("delay".to_string(), 1)].into(),
            by_product: [("term-life".to_string(), 4)].into(),
            by_channel: [(IntakeChannel::Call, 3), (IntakeChannel::App, 1)].into(),
            acknowledged_on_time: 2,
            acknowledged_late: 2,
            decided_on_time: 2,
            decided_late: 2,
            outcomes: [(OutcomeKind::Upheld, 2)].into(),
            resolved_on_the_spot: 1,
            deadlocks: 2,
            deadlocks_by_time: 2,
            referrals: Default::default(),
            forum_outcomes: Default::default(),
            remediation_paid_minor: [(RemediationKind::ExGratia, 20_000)].into(),
            systemic_cases: 1,
            repeat_complainants: 1,
            vulnerable: 1,
            vulnerable_late: 1,
            by_decision_maker: [(
                "assessor-1".to_string(),
                Overturns {
                    complained_about: 1,
                    overturned: 1
                }
            )]
            .into(),
            by_rule_version: [(
                "claims-7".to_string(),
                Overturns {
                    complained_about: 1,
                    overturned: 1
                }
            )]
            .into(),
            by_adviser_engagement: [("eng-1".to_string(), 1)].into(),
            months_without_review: vec!["2027-11".to_string()],
            open_conduct_cases: 1,
            records_removed: 0,
        }
    );
    // An empty period reports nothing received.
    let empty = kit.state.report(start + Duration::days(1), end);
    assert_eq!(empty.received, 0);
    assert_eq!(empty.repeat_complainants, 0);
}

/// Purpose: the monthly root cause review is recorded once for a month
/// that has begun, with actions that each name a cause and the record of
/// the change, and a month that ended without one shows in the report.
/// Why it matters: complaints are only worth recording if their causes are
/// acted on. The review is the control, and its absence is itself
/// something the conduct committee must see.
#[test]
fn the_monthly_root_cause_review_is_recorded_and_its_absence_reported() {
    let (mut kit, start) = scenario();
    kit.advance_days(30);
    let action = |cause: &str, reference: &str| Action {
        kind: ActionKind::RuleChange,
        cause: cause.into(),
        reference: reference.into(),
    };
    let review = |month: &str, actions: Vec<Action>| Command::RecordRootCauseReview { month: month.into(), actions };
    for month in ["2027-13", "2028-01", "2027-1", "november", "2027-11-01"] {
        assert_eq!(kit.err("complaints", "lead", review(month, vec![])), "invalid", "{month}");
    }
    assert_eq!(kit.err("complaints", "lead", review("2027-11", vec![action("", "chg-1")])), "invalid");
    assert_eq!(kit.err("complaints", "lead", review("2027-11", vec![action("delay", "")])), "invalid");
    assert_eq!(kit.err("conduct", "conduct-officer", review("2027-11", vec![])), "not_allowed");
    assert_eq!(kit.state.report(start, kit.today()).months_without_review, vec!["2027-11".to_string()]);

    assert_eq!(
        kit.ok("complaints", "lead", review("2027-11", vec![action("wrong_decision", "chg-41")])),
        vec![Event::RootCauseReviewed {
            month: "2027-11".into(),
            actions: 1
        }]
    );
    assert_eq!(kit.err("complaints", "lead", review("2027-11", vec![])), "exists");
    assert_eq!(
        kit.state.reviews["2027-11"],
        RootCauseReview {
            reviewed_on: kit.today(),
            by: "lead".into(),
            actions: vec![action("wrong_decision", "chg-41")],
        }
    );
    assert!(kit.state.report(start, kit.today()).months_without_review.is_empty());
    // A period of several months lists each one missed.
    kit.advance_days(60);
    kit.record(intake("c9", Process::Service));
    assert_eq!(
        kit.state.report(start, kit.today()).months_without_review,
        vec!["2027-12".to_string(), "2028-01".to_string()]
    );
}

/// Purpose: an adviser sees that a complaint about their service exists
/// and how it ended; a complainant is given the record of their own
/// complaint with every document, and neither view names staff.
/// Why it matters: the adviser must not see the complaint's contents, and
/// the complainant has a legal right to their record but not to the names
/// of the people who handled it.
#[test]
fn advisers_and_complainants_each_see_only_their_view() {
    let (kit, start) = scenario();
    assert_eq!(
        kit.state.adviser_view("eng-1"),
        vec![AdviserView {
            complaint: "c1".into(),
            received_on: start,
            status: Status::Decided,
            outcome: Some(OutcomeKind::Upheld)
        }]
    );
    assert!(kit.state.adviser_view("eng-2").is_empty());

    let record = kit.state.subject_record("c1").unwrap();
    assert_eq!(record.documents, vec!["doc-summary-c1", "doc-ack-c1", "doc-outcome-c1"]);
    assert_eq!(record.reason_codes, vec!["evidence_reassessed"]);
    assert_eq!(record.remediation, vec![(RemediationKind::ExGratia, 20_000), (RemediationKind::Refund, 5_000)]);
    assert_eq!(record.acknowledged_on, Some(start));
    assert_eq!(record.policies, vec![policy("pol-1")]);
    assert_eq!(record.closed_on, None);
    let text = serde_json::to_string(&record).unwrap();
    for name in ["reviewer-1", "assessor-1", "lead", "agent-1"] {
        assert!(!text.contains(name), "the subject's record names {name}");
    }
    assert_eq!(kit.state.subject_record("c3").unwrap().closed_on, Some(start));
    assert!(kit.state.subject_record("nobody").is_none());
}

/// Purpose: the queue lists open complaints, those marked vulnerable
/// first, then by the timer due soonest, with the adjustments to make but
/// not the reason, and anything with nothing due last.
/// Why it matters: the queue is how work is chosen. The complaint closest
/// to a missed deadline must be the next one picked up, and the handler
/// must see how to treat a vulnerable customer without seeing why.
#[test]
fn the_queue_puts_vulnerable_complainants_and_the_nearest_deadline_first() {
    let (kit, start) = scenario();
    let queue = kit.state.queue(10);
    assert_eq!(queue.iter().map(|i| i.complaint.as_str()).collect::<Vec<_>>(), vec!["c2", "c4", "c1"]);
    assert_eq!(
        queue[0],
        QueueItem {
            complaint: "c2".into(),
            owner: None,
            process: Process::Service,
            adjustments: vec!["interpreter".into()],
            needs_trained_handler: true,
            authority_needed: 0,
            next_due: Some((Timer::Acknowledge, start + Duration::days(5))),
        }
    );
    assert_eq!(queue[1].next_due, Some((Timer::Update, start + Duration::days(17))));
    assert_eq!(queue[2].next_due, None);
    assert_eq!(queue[2].authority_needed, 2);
    assert_eq!(queue[2].owner.as_deref(), Some("reviewer-1"));
    assert_eq!(kit.state.queue(1).len(), 1);
}

/// Purpose: every event has an id `cmp-<seq>` with no gaps, waits in the
/// module until the relay says it is published, and is then dropped; the
/// relay cannot acknowledge an event that does not exist.
/// Why it matters: other services act on these events: the documents
/// service issues the acknowledgement, the conduct officer is told of a
/// systemic finding. An event lost between the state and the stream is an
/// action that never happens.
#[test]
fn events_wait_in_order_until_the_relay_has_published_them() {
    let (mut kit, _) = scenario();
    let pending = kit.state.pending_events(1000);
    assert!(pending.len() > 10);
    for (n, record) in pending.iter().enumerate() {
        assert_eq!(record.seq, n as u64 + 1);
        assert_eq!(record.id, format!("cmp-{}", n + 1));
    }
    assert_eq!(pending[0].event, Event::TimersActivated { version: 1 });
    assert_eq!(pending[0].log_index, 1);
    assert_eq!(kit.state.pending_events(3).len(), 3);

    let last = pending.len() as u64;
    assert_eq!(kit.err("relay", "relay", Command::MarkPublished { through_seq: last + 1 }), "invalid");
    assert!(kit.ok("relay", "relay", Command::MarkPublished { through_seq: 3 }).is_empty());
    assert_eq!(kit.state.pending_events(1000)[0].seq, 4);
    // Acknowledging the same events again is harmless.
    kit.ok("relay", "relay", Command::MarkPublished { through_seq: 2 });
    assert_eq!(kit.state.pending_events(1000).len() as u64, last - 3);
    kit.ok("relay", "relay", Command::MarkPublished { through_seq: last });
    assert!(kit.state.pending_events(1000).is_empty());
    // Numbering carries on.
    let next = kit.send("intake", "agent-1", Command::RecordComplaint(Box::new(intake("c9", Process::Service))));
    assert_eq!(next.events[0].seq, last + 1);
}

fn every_command() -> Vec<Command> {
    let day = NaiveDate::from_ymd_opt(2027, 12, 1).unwrap();
    let c = || "c1".to_string();
    let k = || "k1".to_string();
    vec![
        Command::ActivateTimers(timers()),
        Command::RecordComplaint(Box::new(intake("c1", Process::Service))),
        Command::RefineCategory {
            complaint: c(),
            category: Category {
                product: None,
                process: Process::Service,
                cause: None,
            },
        },
        Command::AddRepresentative {
            complaint: c(),
            representative: Representative {
                party: party("rep"),
                relationship: "family".into(),
                authority_document: "doc".into(),
            },
        },
        Command::SetVulnerability {
            complaint: c(),
            vulnerable: true,
            adjustments: vec![],
        },
        Command::Assign {
            complaint: c(),
            handler: "h".into(),
            authority: 1,
            trained_for_vulnerability: true,
        },
        Command::RecordAcknowledgement {
            complaint: c(),
            document: "d".into(),
        },
        Command::RecordUpdate {
            complaint: c(),
            document: "d".into(),
        },
        Command::AskComplainant { complaint: c() },
        Command::RecordComplainantResponse { complaint: c() },
        Command::GrantAccess {
            complaint: c(),
            to: "x".into(),
            classes: BTreeSet::from([DataClass::Personal]),
            purpose: "p".into(),
        },
        Command::RevokeAccess {
            complaint: c(),
            from: "x".into(),
        },
        Command::Decide {
            complaint: c(),
            outcome: OutcomeKind::Upheld,
            reason_codes: vec!["r".into()],
            document: Some("d".into()),
        },
        Command::RecordOutcomeDelivered { complaint: c(), on: day },
        Command::RecordRemediation {
            complaint: c(),
            kind: RemediationKind::Refund,
            reference: "r".into(),
            amount_minor: 1,
            authorised_by: "a".into(),
            authority: 1,
        },
        Command::CompleteRemediation {
            complaint: c(),
            reference: "r".into(),
        },
        Command::RecordSystemicFinding {
            complaint: c(),
            remediation_case: "rem".into(),
        },
        Command::DeclareDeadlock { complaint: c() },
        Command::RecordDeadlockNotice {
            complaint: c(),
            document: "d".into(),
        },
        Command::Refer {
            complaint: c(),
            forum: Forum::Scheme,
            reference: "r".into(),
            response_due: day,
            documents_due: None,
        },
        Command::RecordForumResponse { complaint: c() },
        Command::ReleaseBundle {
            complaint: c(),
            bundle: "b".into(),
            documents: vec!["d".into()],
            reviewed_by: "r".into(),
        },
        Command::RecordForumDecision {
            complaint: c(),
            outcome: OutcomeKind::NotUpheld,
            binding: false,
            remediation_due: None,
        },
        Command::CompleteForumRemediation { complaint: c() },
        Command::Close {
            complaint: c(),
            detail: detail("delay"),
        },
        Command::RecordRootCauseReview {
            month: "2027-11".into(),
            actions: vec![],
        },
        Command::OpenConductCase {
            case: k(),
            kind: ConductKind::Breach,
            owner: "o".into(),
            summary_document: None,
            notify_regulator: false,
        },
        Command::AttachPopulation {
            case: k(),
            customers: vec![],
            advisers: vec![],
            policies: vec![],
        },
        Command::CompletePopulation {
            case: k(),
            customers: 0,
            policies: 0,
        },
        Command::RecordCustomerNotified {
            case: k(),
            customer: party("p"),
            document: "d".into(),
        },
        Command::ExcludeCustomer {
            case: k(),
            customer: party("p"),
            reason: "r".into(),
        },
        Command::DrawSample { case: k(), size: 1 },
        Command::RecordFileReview {
            case: k(),
            policy: policy("p"),
            finding: ReviewFinding::Satisfactory,
        },
        Command::RecordRegulatorNotification {
            case: k(),
            reference: "r".into(),
            document: "d".into(),
        },
        Command::RecordRegulatorRequest {
            case: k(),
            request: "q".into(),
            due: day,
            document: "d".into(),
        },
        Command::RespondToRegulator {
            case: k(),
            request: "q".into(),
            document: "d".into(),
        },
        Command::LinkComplaint { case: k(), complaint: c() },
        Command::CloseConductCase {
            case: k(),
            findings_document: "d".into(),
        },
        Command::Tick,
        Command::MarkPublished { through_seq: 0 },
    ]
}

/// Purpose: every command has its own name and a list of the services that
/// may send it; a service not on the list is refused, and so is a command
/// that names nobody responsible.
/// Why it matters: the caller list is the module's authorisation. A
/// command added without one, or one that an unlisted service can send,
/// is a way round the separation between complaints, conduct and legal;
/// a command with no actor cannot be checked for separation at all.
#[test]
fn every_command_is_named_and_guarded_by_its_callers() {
    let commands = every_command();
    let names: BTreeSet<&str> = commands.iter().map(Command::name).collect();
    assert_eq!(names.len(), commands.len());
    assert_eq!(names.len(), 40);

    let mut kit = Kit::ready();
    for command in commands {
        let callers = command.allowed_callers();
        assert!(!callers.is_empty(), "{} has no caller", command.name());
        assert_eq!(kit.err("stranger", "someone", command.clone()), "not_allowed", "{}", command.name());
        assert_eq!(kit.err(callers[0], "", command.clone()), "no_actor", "{}", command.name());
        // Only the conduct function and legal work a conduct case; only
        // config, the scheduler and the relay do their own jobs.
        let conduct_only = matches!(
            command,
            Command::ExcludeCustomer { .. }
                | Command::DrawSample { .. }
                | Command::RecordFileReview { .. }
                | Command::RecordRegulatorNotification { .. }
                | Command::RecordRegulatorRequest { .. }
                | Command::RespondToRegulator { .. }
                | Command::LinkComplaint { .. }
                | Command::CloseConductCase { .. }
        );
        if conduct_only {
            assert_eq!(callers, ["conduct", "legal"]);
        }
    }
    assert_eq!(Command::Tick.allowed_callers(), ["scheduler"]);
    assert_eq!(Command::MarkPublished { through_seq: 0 }.allowed_callers(), ["relay"]);
    assert_eq!(Command::ActivateTimers(timers()).allowed_callers(), ["config"]);
}

/// Purpose: commands for complaints and cases that do not exist are
/// refused by name, and small edits to a complaint are validated.
/// Why it matters: a typo in an id must be a refusal the caller can show
/// the user, never a silent success or a record created by accident.
#[test]
fn unknown_complaints_and_invalid_details_are_refused() {
    let mut kit = Kit::ready();
    kit.record(intake("c1", Process::Service));
    let category = |cause: Option<&str>| Category {
        product: Some("income-protection".into()),
        process: Process::Billing,
        cause: cause.map(String::from),
    };
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::RefineCategory {
                complaint: "nobody".into(),
                category: category(None)
            }
        ),
        "unknown_complaint"
    );
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::RefineCategory {
                complaint: "c1".into(),
                category: category(Some("made_up"))
            }
        ),
        "unknown_cause"
    );
    kit.ok(
        "complaints",
        "lead",
        Command::RefineCategory {
            complaint: "c1".into(),
            category: category(Some("delay")),
        },
    );
    assert_eq!(kit.complaint("c1").category, category(Some("delay")));

    let representative = |relationship: &str, document: &str| Command::AddRepresentative {
        complaint: "c1".into(),
        representative: Representative {
            party: party("lawyer-1"),
            relationship: relationship.into(),
            authority_document: document.into(),
        },
    };
    assert_eq!(kit.err("complaints", "lead", representative("lawyer", "")), "invalid");
    assert_eq!(kit.err("complaints", "lead", representative("", "doc-authority")), "invalid");
    kit.ok("complaints", "lead", representative("lawyer", "doc-authority"));
    assert_eq!(kit.err("complaints", "lead", representative("lawyer", "doc-authority")), "exists");
    assert_eq!(kit.complaint("c1").representatives.len(), 1);

    assert_eq!(
        kit.err(
            "conduct",
            "conduct-officer",
            Command::DrawSample {
                case: "nobody".into(),
                size: 1
            }
        ),
        "unknown_case"
    );
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::RecordSystemicFinding {
                complaint: "c1".into(),
                remediation_case: String::new()
            }
        ),
        "invalid"
    );
    let finding = || Command::RecordSystemicFinding {
        complaint: "c1".into(),
        remediation_case: "rem-1".into(),
    };
    assert_eq!(
        kit.ok("complaints", "lead", finding()),
        vec![Event::SystemicFindingRaised {
            complaint: "c1".into(),
            remediation_case: "rem-1".into()
        }]
    );
    assert_eq!(kit.err("complaints", "lead", finding()), "already_recorded");
}

/// Purpose: replaying the same log gives the same state and the same
/// hash, and the state survives being saved and loaded unchanged.
/// Why it matters: every replica applies the log independently and a
/// snapshot replaces the log. Any difference between replicas, or between
/// a restored state and the one saved, is silent divergence of the
/// complaints register.
#[test]
fn replay_and_snapshot_give_the_same_state() {
    let (mut kit, _) = scenario();
    kit.advance_days(40);
    kit.tick();
    let mut replayed = ComplaintsState::new();
    for (i, envelope) in kit.log.iter().enumerate() {
        replayed.apply(i as u64 + 1, envelope.clone());
    }
    assert_eq!(replayed, kit.state);
    assert_eq!(replayed.state_hash(), kit.state.state_hash());

    let restored: ComplaintsState = serde_json::from_slice(&serde_json::to_vec(&kit.state).unwrap()).unwrap();
    assert_eq!(restored, kit.state);
    assert_eq!(restored.state_hash(), kit.state.state_hash());
    // A different history has a different hash.
    assert_ne!(ComplaintsState::new().state_hash(), kit.state.state_hash());
}

fn hosted_from(kit: &Kit) -> Box<dyn ModuleState> {
    let spec = module::module_spec().unwrap();
    let mut hosted = (spec.make)();
    for (i, envelope) in kit.log.iter().enumerate() {
        hosted.apply(i as u64 + 1, serde_json::to_value(envelope).unwrap()).unwrap();
    }
    hosted
}

fn ask(hosted: &dyn ModuleState, query: Query) -> Result<Value, String> {
    hosted.query(serde_json::to_value(query).unwrap())
}

/// Purpose: the module declares itself under its permanent name, takes
/// commands as JSON with repeats returning the original result, refuses
/// JSON that is not a command, and restores from a snapshot to the same
/// state.
/// Why it matters: the host knows nothing of the command type and
/// delivers at least once. This is the whole contract between the
/// complaints workbench and the replicated register.
#[test]
fn the_module_is_hosted_as_complaints_and_takes_commands_as_json() {
    let spec = module::module_spec().unwrap();
    assert_eq!(spec.name, NAME);
    assert_eq!(spec.name, "complaints");
    assert!(!spec.events, "the module reads no core events");

    let (kit, _) = scenario();
    let mut hosted = hosted_from(&kit);
    assert_eq!(ask(hosted.as_ref(), Query::StateHash).unwrap(), json!(kit.state.state_hash()));

    let repeat = serde_json::to_value(&kit.log[1]).unwrap();
    let applied: Applied = serde_json::from_value(hosted.apply(999, repeat).unwrap()).unwrap();
    assert!(applied.duplicate);
    assert_eq!(applied.index, 2);
    assert!(hosted.apply(1000, json!({"key": "k", "command": "nonsense"})).is_err());
    assert_eq!(ask(hosted.as_ref(), Query::StateHash).unwrap(), json!(kit.state.state_hash()));

    let mut restored = (spec.make)();
    restored.load(hosted.save().unwrap()).unwrap();
    assert_eq!(ask(restored.as_ref(), Query::StateHash).unwrap(), json!(kit.state.state_hash()));
    assert!(restored.load(json!("not a state")).is_err());
}

/// Purpose: each question is answered from the state, the complaint file
/// only to someone the case has given access, and conduct cases only to
/// the conduct function and legal; anything that is not a question is
/// refused, as is a register or report period that ends before it
/// starts.
/// Why it matters: queries are how every screen and report reads the
/// register. The access check here is what keeps a complaint file, and
/// the restricted conduct case type, away from everyone else in the
/// building.
#[test]
fn questions_are_answered_only_to_those_allowed_to_ask() {
    let (kit, start) = scenario();
    let hosted = hosted_from(&kit);
    let h = hosted.as_ref();
    let complaint = |actor: &str| Query::Complaint {
        id: "c1".into(),
        actor: actor.into(),
    };
    assert_eq!(ask(h, complaint("reviewer-1")).unwrap()["summary_document"], json!("doc-summary-c1"));
    for actor in ["lead", "assessor-1", "agent-1", ""] {
        assert!(ask(h, complaint(actor)).unwrap_err().contains("access"), "{actor}");
    }
    // The closed complaint is open to nobody: access ended with the case.
    assert!(ask(
        h,
        Query::Complaint {
            id: "c3".into(),
            actor: "agent-1".into()
        }
    )
    .is_err());

    assert_eq!(
        ask(h, Query::SubjectRecord { id: "c1".into() }).unwrap(),
        serde_json::to_value(kit.state.subject_record("c1")).unwrap()
    );
    assert_eq!(ask(h, Query::SubjectRecord { id: "nobody".into() }).unwrap(), Value::Null);
    assert_eq!(ask(h, Query::AdviserView { engagement: "eng-1".into() }).unwrap()[0]["complaint"], json!("c1"));
    assert_eq!(
        ask(
            h,
            Query::Queue {
                limit: usize::MAX,
                owner: None
            }
        )
        .unwrap()
        .as_array()
        .unwrap()
        .len(),
        3
    );
    assert_eq!(ask(h, Query::Queue { limit: 2, owner: None }).unwrap().as_array().unwrap().len(), 2);
    assert_eq!(
        ask(h, Query::Running { id: "c2".into() }).unwrap(),
        serde_json::to_value(kit.state.running(kit.complaint("c2"))).unwrap()
    );
    assert_eq!(ask(h, Query::Running { id: "nobody".into() }).unwrap(), Value::Null);
    assert_eq!(ask(h, Query::ComplaintsOf { party: party("aroha") }).unwrap(), json!(["c1", "c2"]));
    let period = (start, kit.today());
    assert_eq!(ask(h, Query::Register { from: period.0, to: period.1 }).unwrap().as_array().unwrap().len(), 4);
    assert_eq!(ask(h, Query::Report { from: period.0, to: period.1 }).unwrap()["received"], json!(4));
    // A period that ends before it starts is refused in words, never
    // answered with an empty register that reads as "no complaints".
    for reversed in [Query::Register { from: period.1, to: period.0 }, Query::Report { from: period.1, to: period.0 }] {
        assert!(ask(h, reversed).unwrap_err().contains("before it starts"));
    }
    assert!(ask(h, Query::Register { from: period.1, to: period.1 }).unwrap().is_array());
    assert_eq!(ask(h, Query::Timers { version: 1 }).unwrap()["decide_days"], json!(20));
    assert_eq!(ask(h, Query::Timers { version: 9 }).unwrap(), Value::Null);
    assert_eq!(ask(h, Query::CurrentTimers).unwrap()["front_line_limit_minor"], json!(10_000));
    assert_eq!(ask(h, Query::PendingEvents { limit: 2 }).unwrap().as_array().unwrap().len(), 2);
    assert!(ask(h, Query::PendingEvents { limit: usize::MAX }).unwrap().as_array().unwrap().len() <= module::MAX_ANSWER);

    // Conduct cases.
    for role in [Role::Complaints, Role::Other] {
        assert!(ask(h, Query::ConductCase { id: "i-1".into(), role }).unwrap_err().contains("conduct"));
        assert!(ask(h, Query::OpenConductCases { role }).is_err());
    }
    for role in [Role::Conduct, Role::Legal] {
        assert_eq!(
            ask(h, Query::ConductCase { id: "i-1".into(), role }).unwrap()["owner"],
            json!("conduct-officer")
        );
        assert_eq!(ask(h, Query::OpenConductCases { role }).unwrap(), json!(["i-1"]));
    }
    assert_eq!(
        ask(
            h,
            Query::ConductCase {
                id: "nobody".into(),
                role: Role::Conduct
            }
        )
        .unwrap(),
        Value::Null
    );

    assert!(hosted.query(json!({"everything": {}})).unwrap_err().contains("not a question"));
}

/// Purpose: a report for a period still running counts a timer as late
/// only once its due date has passed today, not because the period's end
/// is later than the due date.
/// Why it matters: found by the scenario run, SCN-21 (F-21-6). The month's
/// report, read mid-month, counted complaints received that day as
/// acknowledged late, and that is the report the regulator is sent.
#[test]
fn a_running_period_counts_nothing_late_before_it_is_due() {
    let mut kit = Kit::ready();
    let start = kit.today();
    let mut c1 = intake("c1", Process::Service);
    c1.vulnerable = true;
    kit.record(c1);
    let month_end = start + Duration::days(30);
    let report = kit.state.report(start, month_end);
    assert_eq!((report.received, report.acknowledged_late, report.vulnerable_late), (1, 0, 0));
    // Once the acknowledgement is overdue, it is late.
    kit.advance_days(timers().acknowledge_days as i64 + 1);
    kit.tick();
    let report = kit.state.report(start, month_end);
    assert_eq!((report.acknowledged_late, report.vulnerable_late), (1, 1));
    // A closed period is judged as it stood at its end.
    assert_eq!(kit.state.report(start, start + Duration::days(1)).acknowledged_late, 0);
}

/// Purpose: a handler's own complaints are in the queue they are shown however far down it they come; others are cut at the limit.
/// Why it matters: found by the persona suite run against the scenario stack (E2E-044, E2E-034, E2E-039 failing, reported with
/// SCN-21). With more than 200 open complaints, a decided complaint with nothing due fell off the end of the queue, and its own handler
/// could no longer see it to close it.
#[test]
fn a_handlers_own_complaints_never_fall_off_the_queue() {
    let (kit, _) = scenario();
    let ids = |q: Vec<QueueItem>| q.into_iter().map(|i| i.complaint).collect::<Vec<_>>();
    assert_eq!(ids(kit.state.queue_for(1, Some("reviewer-1"))), vec!["c2", "c1"]);
    assert_eq!(ids(kit.state.queue_for(1, Some("someone-else"))), vec!["c2"]);
    assert_eq!(ids(kit.state.queue_for(1, None)), vec!["c2"]);
}
