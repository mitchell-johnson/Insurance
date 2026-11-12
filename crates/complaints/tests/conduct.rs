//! Conduct cases: a licence event and its affected population, telling
//! each customer, the sampled review of advice files, the regulator, and
//! what must be done before a case can close.

mod support;

use chrono::{Duration, NaiveDate};
use ic_complaints::conduct::licence_case_id;
use ic_complaints::model::*;
use ic_complaints::state::MAX_CHUNK;
use ic_complaints::{Command, Event};
use ic_core::PolicyId;
use sha2::{Digest, Sha256};
use support::*;

const CASE: &str = "licence:ev-1";

fn licence(status: &str) -> ConductKind {
    ConductKind::LicenceEvent {
        licence_event: "ev-1".into(),
        party: party("fap-1"),
        status: status.into(),
        effective: NaiveDate::from_ymd_opt(2027, 11, 12).unwrap(),
    }
}

fn open(case: &str, kind: ConductKind, notify_regulator: bool) -> Command {
    Command::OpenConductCase {
        case: case.into(),
        kind,
        owner: "conduct-officer".into(),
        summary_document: Some("doc-summary".into()),
        notify_regulator,
    }
}

fn attach(customers: &[&str], advisers: &[&str], policies: &[&str]) -> Command {
    Command::AttachPopulation {
        case: CASE.into(),
        customers: customers.iter().map(|c| party(c)).collect(),
        advisers: advisers.iter().map(|a| party(a)).collect(),
        policies: policies.iter().map(|p| policy(p)).collect(),
    }
}

/// A licence case with three customers, one adviser and five policies,
/// the population complete.
fn populated() -> Kit {
    let mut kit = Kit::ready();
    kit.ok("distribution", "commission-service", open(CASE, licence("revoked"), true));
    kit.ok(
        "distribution",
        "commission-service",
        attach(&["cust-1", "cust-2"], &["adviser-1"], &["p1", "p2", "p3"]),
    );
    kit.ok("distribution", "commission-service", attach(&["cust-3"], &[], &["p4", "p5"]));
    kit.ok(
        "distribution",
        "commission-service",
        Command::CompletePopulation {
            case: CASE.into(),
            customers: 3,
            policies: 5,
        },
    );
    kit
}

/// Purpose: a licence event opens exactly one conduct case, named after
/// the event and owned by the conduct officer, however many times the
/// event is delivered; a different matter under the same name is refused.
/// Why it matters: the commission module reports the event at least once.
/// Two cases for one revocation would send every customer two letters and
/// the regulator two notifications.
#[test]
fn a_licence_event_opens_one_case_however_often_it_arrives() {
    let mut kit = Kit::ready();
    assert_eq!(licence_case_id("ev-1"), CASE);
    let today = kit.today();
    assert_eq!(
        kit.ok("distribution", "commission-service", open(CASE, licence("revoked"), true)),
        vec![Event::ConductCaseOpened {
            case: CASE.into(),
            owner: "conduct-officer".into(),
            licence_event: Some("ev-1".into())
        }]
    );
    let case = kit.state.conduct[CASE].clone();
    assert_eq!(case.owner, "conduct-officer");
    assert_eq!(case.opened_by, "commission-service");
    assert_eq!(case.opened_on, today);
    assert_eq!(case.regulator.as_ref().unwrap().due, today + Duration::days(10));

    // Delivered again, under a new key and a day later: accepted, nothing changes.
    kit.advance_days(1);
    assert!(kit.ok("distribution", "commission-service", open(CASE, licence("revoked"), true)).is_empty());
    assert_eq!(kit.state.conduct[CASE], case);
    assert_eq!(kit.state.conduct.len(), 1);

    assert_eq!(kit.err("distribution", "commission-service", open(CASE, licence("suspended"), true)), "exists");
    // The case is named after its event, and the event says what happened.
    assert_eq!(
        kit.err("distribution", "commission-service", open("licence:other", licence("revoked"), true)),
        "invalid"
    );
    assert_eq!(kit.err("distribution", "commission-service", open(CASE, licence(""), true)), "invalid");
}

/// Purpose: each kind of conduct case is opened only by the function that
/// may open it, with an id and an owner, under active timers.
/// Why it matters: a conduct case is a restricted record that can end in a
/// report to the regulator. The commission module opening a breach, or the
/// complaints team recording an incident, would bypass the conduct officer.
#[test]
fn each_kind_of_case_is_opened_by_the_function_that_owns_it() {
    let mut kit = Kit::new();
    assert_eq!(kit.err("conduct", "conduct-officer", open("inc-1", ConductKind::Incident, false)), "no_timers");
    let mut kit = Kit::ready();

    assert_eq!(
        kit.err("distribution", "commission-service", open("b-1", ConductKind::Breach, false)),
        "not_allowed"
    );
    assert_eq!(
        kit.err("distribution", "commission-service", open("a-1", ConductKind::AdviserPattern, false)),
        "not_allowed"
    );
    assert_eq!(kit.err("complaints", "lead", open("b-1", ConductKind::Breach, false)), "not_allowed");
    assert_eq!(kit.err("complaints", "lead", open("i-1", ConductKind::Incident, false)), "not_allowed");
    assert_eq!(kit.err("intake", "agent-1", open("a-1", ConductKind::AdviserPattern, false)), "not_allowed");
    assert_eq!(kit.err("conduct", "conduct-officer", open("", ConductKind::Breach, false)), "invalid");
    let mut ownerless = open("b-1", ConductKind::Breach, false);
    if let Command::OpenConductCase { owner, .. } = &mut ownerless {
        owner.clear();
    }
    assert_eq!(kit.err("conduct", "conduct-officer", ownerless), "invalid");

    // The complaints team raises a pattern or a systemic finding it has seen.
    kit.ok("complaints", "lead", open("a-1", ConductKind::AdviserPattern, false));
    kit.ok("complaints", "lead", open("s-1", ConductKind::SystemicFinding, false));
    kit.ok("conduct", "conduct-officer", open("b-1", ConductKind::Breach, true));
    assert_eq!(
        kit.ok("conduct", "conduct-officer", open("i-1", ConductKind::Incident, false)),
        vec![Event::ConductCaseOpened {
            case: "i-1".into(),
            owner: "conduct-officer".into(),
            licence_event: None
        }]
    );
    assert_eq!(kit.state.conduct["i-1"].regulator, None);
    assert_eq!(kit.err("conduct", "conduct-officer", open("i-1", ConductKind::Breach, false)), "exists");
    assert_eq!(kit.state.report(kit.today(), kit.today()).open_conduct_cases, 4);

    // A case with nothing owed closes on its findings alone.
    let close = |doc: &str| Command::CloseConductCase {
        case: "i-1".into(),
        findings_document: doc.into(),
    };
    assert_eq!(kit.err("conduct", "conduct-officer", close("")), "invalid");
    assert_eq!(
        kit.ok("conduct", "conduct-officer", close("doc-findings")),
        vec![Event::ConductCaseClosed { case: "i-1".into() }]
    );
    assert_eq!(kit.err("conduct", "conduct-officer", close("doc-findings")), "closed");
}

/// Purpose: the affected population is attached in bounded chunks that can
/// be repeated safely, and is complete only when the sender's counts match
/// what is attached; nothing is added afterwards.
/// Why it matters: a firm's book can be tens of thousands of customers and
/// arrives over many commands, any of which may be retried. Starting
/// communication on a partial population leaves customers untold; a count
/// that does not match means a chunk was lost.
#[test]
fn the_population_arrives_in_chunks_and_is_complete_only_when_the_counts_match() {
    let mut kit = Kit::ready();
    kit.ok("distribution", "commission-service", open(CASE, licence("revoked"), true));
    assert_eq!(
        kit.err(
            "distribution",
            "commission-service",
            Command::AttachPopulation {
                case: "licence:none".into(),
                customers: vec![],
                advisers: vec![],
                policies: vec![]
            }
        ),
        "unknown_case"
    );
    let too_many: Vec<String> = (0..=MAX_CHUNK).map(|n| format!("cust-{n}")).collect();
    let refs: Vec<&str> = too_many.iter().map(String::as_str).collect();
    assert_eq!(kit.err("distribution", "commission-service", attach(&refs, &[], &[])), "invalid");

    assert_eq!(
        kit.ok(
            "distribution",
            "commission-service",
            attach(&["cust-1", "cust-2"], &["adviser-1"], &["p1", "p2", "p3"])
        ),
        vec![Event::PopulationAttached {
            case: CASE.into(),
            customers: 2,
            policies: 3
        }]
    );
    // The same chunk again, then one that overlaps.
    kit.ok(
        "distribution",
        "commission-service",
        attach(&["cust-1", "cust-2"], &["adviser-1"], &["p1", "p2", "p3"]),
    );
    assert_eq!(
        kit.ok("distribution", "commission-service", attach(&["cust-2", "cust-3"], &[], &["p3", "p4"])),
        vec![Event::PopulationAttached {
            case: CASE.into(),
            customers: 3,
            policies: 4
        }]
    );
    let complete = |customers, policies| Command::CompletePopulation {
        case: CASE.into(),
        customers,
        policies,
    };
    // Nothing that needs the whole population can start yet.
    assert_eq!(
        kit.err("conduct", "conduct-officer", Command::DrawSample { case: CASE.into(), size: 2 }),
        "population_incomplete"
    );
    assert_eq!(kit.err("distribution", "commission-service", complete(4, 4)), "count_mismatch");
    assert_eq!(kit.err("distribution", "commission-service", complete(3, 5)), "count_mismatch");
    assert_eq!(
        kit.ok("distribution", "commission-service", complete(3, 4)),
        vec![Event::PopulationComplete {
            case: CASE.into(),
            customers: 3
        }]
    );
    assert_eq!(kit.err("distribution", "commission-service", complete(3, 4)), "already_complete");
    assert_eq!(kit.err("distribution", "commission-service", attach(&["cust-9"], &[], &[])), "already_complete");
    let case = &kit.state.conduct[CASE];
    assert!(case.customers.values().all(|c| *c == Contact::Outstanding));
    assert_eq!(case.advisers.len(), 1);
}

/// Purpose: every affected customer is either told, by a named document,
/// or excluded for a stated reason, once.
/// Why it matters: the customers of an adviser who can no longer advise
/// must each hear it. The case can show for each of them which letter went
/// or why none did, which is what the regulator will ask.
#[test]
fn each_customer_is_told_or_excluded_for_a_reason() {
    let mut kit = populated();
    let told = |customer: &str, doc: &str| Command::RecordCustomerNotified {
        case: CASE.into(),
        customer: party(customer),
        document: doc.into(),
    };
    let excluded = |customer: &str, reason: &str| Command::ExcludeCustomer {
        case: CASE.into(),
        customer: party(customer),
        reason: reason.into(),
    };
    assert_eq!(kit.err("documents", "documents-service", told("cust-1", "")), "not_outstanding");
    assert_eq!(kit.err("documents", "documents-service", told("stranger", "doc-1")), "not_outstanding");
    kit.ok("documents", "documents-service", told("cust-1", "doc-1"));
    assert_eq!(kit.err("documents", "documents-service", told("cust-1", "doc-2")), "not_outstanding");
    assert_eq!(kit.err("conduct", "conduct-officer", excluded("cust-1", "deceased")), "not_outstanding");
    assert_eq!(kit.err("conduct", "conduct-officer", excluded("cust-2", "")), "not_outstanding");
    assert_eq!(kit.err("documents", "documents-service", excluded("cust-2", "deceased")), "not_allowed");
    kit.ok("conduct", "conduct-officer", excluded("cust-2", "deceased"));
    kit.ok("conduct", "conduct-officer", told("cust-3", "doc-3"));

    let case = &kit.state.conduct[CASE];
    assert_eq!(
        case.customers[&party("cust-1")],
        Contact::Notified {
            document: "doc-1".into(),
            on: kit.today()
        }
    );
    assert_eq!(case.customers[&party("cust-2")], Contact::Excluded { reason: "deceased".into() });
}

/// Purpose: the advice files to review are drawn once, from the complete
/// population, in an order fixed by a hash of the case and each policy;
/// each drawn file is reviewed once, by someone who is not one of the
/// advisers, and a file that needs putting right names its remediation case.
/// Why it matters: a sample somebody chose proves nothing. This draw can
/// be repeated by the regulator from the case id and the policy list, and
/// an adviser marking their own file would make the review worthless.
#[test]
fn the_sample_is_drawn_by_hash_and_each_file_reviewed_once() {
    let mut kit = populated();
    let draw = |size| Command::DrawSample { case: CASE.into(), size };
    assert_eq!(kit.err("conduct", "conduct-officer", draw(0)), "invalid");
    assert_eq!(
        kit.ok("conduct", "conduct-officer", draw(2)),
        vec![Event::SampleDrawn { case: CASE.into(), files: 2 }]
    );
    assert_eq!(kit.err("conduct", "conduct-officer", draw(2)), "invalid");

    // The same draw, done independently.
    let mut order: Vec<(String, PolicyId)> = ["p1", "p2", "p3", "p4", "p5"]
        .iter()
        .map(|p| (hex::encode(Sha256::digest(format!("{CASE}|{p}").as_bytes())), policy(p)))
        .collect();
    order.sort();
    let expected: Vec<PolicyId> = {
        let mut drawn: Vec<PolicyId> = order.iter().take(2).map(|(_, p)| p.clone()).collect();
        drawn.sort();
        drawn
    };
    let sampled: Vec<PolicyId> = kit.state.conduct[CASE].sample.keys().cloned().collect();
    assert_eq!(sampled, expected);
    let not_sampled = order[4].1.clone();

    let review = |policy: &PolicyId, finding: ReviewFinding| Command::RecordFileReview {
        case: CASE.into(),
        policy: policy.clone(),
        finding,
    };
    let needs = |case: &str| ReviewFinding::RemediationNeeded { remediation_case: case.into() };
    assert_eq!(
        kit.err("conduct", "reviewer-9", review(&not_sampled, ReviewFinding::Satisfactory)),
        "not_sampled"
    );
    assert_eq!(kit.err("conduct", "adviser-1", review(&sampled[0], ReviewFinding::Satisfactory)), "separation");
    assert_eq!(kit.err("conduct", "reviewer-9", review(&sampled[0], needs(""))), "invalid");
    assert_eq!(
        kit.ok("conduct", "reviewer-9", review(&sampled[0], ReviewFinding::Satisfactory)),
        vec![Event::FileReviewed {
            case: CASE.into(),
            policy: sampled[0].clone(),
            remediation_case: None
        }]
    );
    assert_eq!(
        kit.err("conduct", "reviewer-9", review(&sampled[0], ReviewFinding::Satisfactory)),
        "already_reviewed"
    );
    assert_eq!(
        kit.ok("conduct", "reviewer-9", review(&sampled[1], needs("rem-7"))),
        vec![Event::FileReviewed {
            case: CASE.into(),
            policy: sampled[1].clone(),
            remediation_case: Some("rem-7".into())
        }]
    );
    assert_eq!(
        kit.state.conduct[CASE].sample[&sampled[1]],
        Some(FileReview {
            reviewer: "reviewer-9".into(),
            on: kit.today(),
            finding: needs("rem-7")
        })
    );
}

/// Purpose: a sample larger than the population reviews every file, and a
/// case with no advice files has nothing to draw.
/// Why it matters: a small book is reviewed in full rather than refused,
/// and an empty draw must not be recorded as a review having happened.
#[test]
fn a_sample_larger_than_the_population_takes_every_file() {
    let mut kit = populated();
    kit.ok("conduct", "conduct-officer", Command::DrawSample { case: CASE.into(), size: 50 });
    assert_eq!(kit.state.conduct[CASE].sample.len(), 5);

    kit.ok("conduct", "conduct-officer", open("b-1", ConductKind::Breach, false));
    kit.ok(
        "conduct",
        "conduct-officer",
        Command::CompletePopulation {
            case: "b-1".into(),
            customers: 0,
            policies: 0,
        },
    );
    assert_eq!(
        kit.err("conduct", "conduct-officer", Command::DrawSample { case: "b-1".into(), size: 1 }),
        "invalid"
    );
}

/// Purpose: the notification to the regulator is recorded once with its
/// reference and document; each request from the regulator is recorded
/// once with a date ahead and answered once; both run on timers that
/// escalate and stop when the thing is done.
/// Why it matters: the notification period and the regulator's own
/// deadlines are statutory. Each must be visible before it is missed and
/// the case must show exactly what was sent and when.
#[test]
fn the_regulator_is_notified_and_answered_on_timers() {
    let mut kit = populated();
    let opened = kit.today();
    let notify = |reference: &str, doc: &str| Command::RecordRegulatorNotification {
        case: CASE.into(),
        reference: reference.into(),
        document: doc.into(),
    };
    let request = |id: &str, due: NaiveDate, doc: &str| Command::RecordRegulatorRequest {
        case: CASE.into(),
        request: id.into(),
        due,
        document: doc.into(),
    };
    let respond = |id: &str, doc: &str| Command::RespondToRegulator {
        case: CASE.into(),
        request: id.into(),
        document: doc.into(),
    };
    let escalated = |events: Vec<Event>| -> Vec<(Timer, Level)> {
        events
            .into_iter()
            .map(|e| match e {
                Event::TimerEscalated { subject, timer, level, .. } => {
                    assert_eq!(subject, CASE);
                    (timer, level)
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    };

    kit.advance_days(8);
    assert_eq!(escalated(kit.tick()), vec![(Timer::RegulatorNotification, Level::Warning)]);
    assert!(kit.tick().is_empty());
    kit.advance_days(3);
    assert_eq!(escalated(kit.tick()), vec![(Timer::RegulatorNotification, Level::Lead)]);
    kit.advance_days(3);
    assert_eq!(escalated(kit.tick()), vec![(Timer::RegulatorNotification, Level::ConductOfficer)]);

    assert_eq!(kit.err("conduct", "conduct-officer", notify("", "doc-n")), "not_due");
    assert_eq!(kit.err("conduct", "conduct-officer", notify("FMA-1", "")), "not_due");
    assert_eq!(
        kit.ok("legal", "counsel-1", notify("FMA-1", "doc-n")),
        vec![Event::RegulatorNotified {
            case: CASE.into(),
            reference: "FMA-1".into()
        }]
    );
    assert_eq!(kit.err("conduct", "conduct-officer", notify("FMA-2", "doc-n2")), "not_due");
    assert_eq!(
        kit.state.conduct[CASE].regulator,
        Some(RegulatorNotification {
            due: opened + Duration::days(10),
            reference: Some("FMA-1".into()),
            document: Some("doc-n".into()),
            notified_on: Some(kit.today()),
        })
    );
    assert!(kit.tick().is_empty());

    // Requests for information.
    let today = kit.today();
    let due = today + Duration::days(4);
    assert_eq!(kit.err("conduct", "conduct-officer", request("", due, "doc-r")), "invalid");
    assert_eq!(kit.err("conduct", "conduct-officer", request("rfi-1", due, "")), "invalid");
    assert_eq!(
        kit.err("conduct", "conduct-officer", request("rfi-1", today - Duration::days(1), "doc-r")),
        "invalid"
    );
    assert_eq!(
        kit.ok("conduct", "conduct-officer", request("rfi-1", due, "doc-r")),
        vec![Event::RegulatorRequestReceived {
            case: CASE.into(),
            request: "rfi-1".into(),
            due
        }]
    );
    assert_eq!(kit.err("conduct", "conduct-officer", request("rfi-1", due, "doc-r")), "invalid");
    kit.ok("conduct", "conduct-officer", request("rfi-2", today + Duration::days(30), "doc-r2"));

    kit.advance_days(5);
    assert_eq!(escalated(kit.tick()), vec![(Timer::RegulatorRequest, Level::Lead)]);
    assert_eq!(kit.err("conduct", "conduct-officer", respond("rfi-1", "")), "not_outstanding");
    assert_eq!(kit.err("conduct", "conduct-officer", respond("rfi-9", "doc-a")), "not_outstanding");
    assert_eq!(
        kit.ok("conduct", "conduct-officer", respond("rfi-1", "doc-a")),
        vec![Event::RegulatorAnswered {
            case: CASE.into(),
            request: "rfi-1".into()
        }]
    );
    assert_eq!(kit.err("conduct", "conduct-officer", respond("rfi-1", "doc-b")), "not_outstanding");
    let case = &kit.state.conduct[CASE];
    assert_eq!(case.requests["rfi-1"].response.as_deref(), Some("doc-a"));
    assert_eq!(case.requests["rfi-1"].responded_on, Some(kit.today()));
    assert!(!case.escalated.contains_key("request:rfi-1"));
    kit.advance_days(10);
    assert!(kit.tick().is_empty(), "the answered request is quiet and the other is not yet near");

    // A case the regulator need not be told of has no notification to record.
    kit.ok("conduct", "conduct-officer", open("i-1", ConductKind::Incident, false));
    assert_eq!(
        kit.err(
            "conduct",
            "conduct-officer",
            Command::RecordRegulatorNotification {
                case: "i-1".into(),
                reference: "FMA-3".into(),
                document: "doc".into()
            }
        ),
        "not_due"
    );
}

/// Purpose: a licence case closes only when the population is complete,
/// every customer is told or excluded, the sampled files are all reviewed,
/// the regulator is notified and every request answered; then it is closed
/// with its findings and takes no further commands.
/// Why it matters: closing the case is the conduct officer's statement
/// that the event has been dealt with. Each of these is something the
/// regulator can ask to see, so none can be skipped by closing early.
#[test]
fn a_licence_case_closes_only_when_everything_owed_is_done() {
    let mut kit = Kit::ready();
    kit.ok("distribution", "commission-service", open(CASE, licence("revoked"), true));
    let close = || Command::CloseConductCase {
        case: CASE.into(),
        findings_document: "doc-findings".into(),
    };
    // A licence case always has a population, even before any is attached.
    assert_eq!(kit.err("conduct", "conduct-officer", close()), "population_incomplete");
    kit.ok("distribution", "commission-service", attach(&["cust-1"], &["adviser-1"], &["p1", "p2"]));
    assert_eq!(kit.err("conduct", "conduct-officer", close()), "population_incomplete");
    kit.ok(
        "distribution",
        "commission-service",
        Command::CompletePopulation {
            case: CASE.into(),
            customers: 1,
            policies: 2,
        },
    );
    assert_eq!(kit.err("conduct", "conduct-officer", close()), "customers_outstanding");
    kit.ok(
        "documents",
        "documents-service",
        Command::RecordCustomerNotified {
            case: CASE.into(),
            customer: party("cust-1"),
            document: "doc-letter".into(),
        },
    );
    assert_eq!(kit.err("conduct", "conduct-officer", close()), "review_outstanding", "no sample has been drawn");
    kit.ok("conduct", "conduct-officer", Command::DrawSample { case: CASE.into(), size: 1 });
    assert_eq!(kit.err("conduct", "conduct-officer", close()), "review_outstanding");
    let drawn = kit.state.conduct[CASE].sample.keys().next().unwrap().clone();
    kit.ok(
        "conduct",
        "reviewer-9",
        Command::RecordFileReview {
            case: CASE.into(),
            policy: drawn,
            finding: ReviewFinding::Satisfactory,
        },
    );
    assert_eq!(kit.err("conduct", "conduct-officer", close()), "regulator_outstanding");
    kit.ok(
        "conduct",
        "conduct-officer",
        Command::RecordRegulatorNotification {
            case: CASE.into(),
            reference: "FMA-1".into(),
            document: "doc-n".into(),
        },
    );
    kit.ok(
        "conduct",
        "conduct-officer",
        Command::RecordRegulatorRequest {
            case: CASE.into(),
            request: "rfi-1".into(),
            due: kit.today() + Duration::days(5),
            document: "doc-r".into(),
        },
    );
    assert_eq!(kit.err("conduct", "conduct-officer", close()), "regulator_outstanding");
    kit.ok(
        "conduct",
        "conduct-officer",
        Command::RespondToRegulator {
            case: CASE.into(),
            request: "rfi-1".into(),
            document: "doc-a".into(),
        },
    );

    // Complaints that came from the same event are linked to the case.
    kit.record(intake("c1", Process::AdviserConduct));
    let link = |complaint: &str| Command::LinkComplaint {
        case: CASE.into(),
        complaint: complaint.into(),
    };
    assert_eq!(kit.err("conduct", "conduct-officer", link("nobody")), "unknown_complaint");
    kit.ok("conduct", "conduct-officer", link("c1"));
    assert_eq!(kit.err("conduct", "conduct-officer", link("c1")), "exists");

    assert_eq!(
        kit.ok("conduct", "conduct-officer", close()),
        vec![Event::ConductCaseClosed { case: CASE.into() }]
    );
    let case = &kit.state.conduct[CASE];
    assert_eq!(case.closed_on, Some(kit.today()));
    assert_eq!(case.findings_document.as_deref(), Some("doc-findings"));
    assert!(case.complaints.contains("c1"));
    assert_eq!(kit.state.report(kit.today(), kit.today()).open_conduct_cases, 0);
    assert_eq!(kit.err("conduct", "conduct-officer", link("c1")), "closed");
    assert_eq!(kit.err("distribution", "commission-service", attach(&["cust-2"], &[], &[])), "closed");
    // The event delivered yet again after the case closed still changes nothing.
    assert!(kit.ok("distribution", "commission-service", open(CASE, licence("revoked"), true)).is_empty());
    kit.advance_days(60);
    let quiet = !kit.tick().iter().any(|e| matches!(e, Event::TimerEscalated { subject, .. } if subject == CASE));
    assert!(quiet, "a closed case has no timers");
}
