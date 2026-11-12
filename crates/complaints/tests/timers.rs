//! Time: the timers on a complaint, their escalation, deadlock, the
//! complainant's own time, retention, and the rules every command meets.

mod support;

use chrono::Duration;
use ic_complaints::command::FrontLine;
use ic_complaints::model::*;
use ic_complaints::state::MAX_CHUNK;
use ic_complaints::{Command, Event};
use support::*;

fn escalations(events: &[Event], timer: Timer) -> Vec<(Level, chrono::NaiveDate)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TimerEscalated { timer: t, level, due, .. } if *t == timer => Some((*level, *due)),
            _ => None,
        })
        .collect()
}

/// Purpose: a timer raises a warning before it is due, goes to the
/// complaints lead when it is missed and to the conduct officer when it is
/// still not done, each exactly once.
/// Why it matters: a timer that depends on someone remembering it is not a
/// control. An escalation raised on every tick would bury the one that
/// matters; one never raised lets a regulatory deadline pass unseen.
#[test]
fn a_timer_warns_then_goes_to_the_lead_then_the_conduct_officer() {
    let mut kit = Kit::ready();
    kit.record(intake("c1", Process::Service));
    let due = kit.today() + Duration::days(5);
    assert!(kit.tick().is_empty(), "nothing is near due on the day of receipt");

    kit.advance_days(3);
    assert_eq!(
        kit.tick(),
        vec![Event::TimerEscalated {
            subject: "c1".into(),
            timer: Timer::Acknowledge,
            level: Level::Warning,
            due
        }]
    );
    assert!(kit.tick().is_empty(), "a warning is raised once");

    kit.advance_days(2);
    assert!(kit.tick().is_empty(), "on the due date the timer is not yet missed");
    kit.advance_days(1);
    assert_eq!(escalations(&kit.tick(), Timer::Acknowledge), vec![(Level::Lead, due)]);
    kit.advance_days(2);
    assert!(escalations(&kit.tick(), Timer::Acknowledge).is_empty());
    kit.advance_days(1);
    assert_eq!(escalations(&kit.tick(), Timer::Acknowledge), vec![(Level::ConductOfficer, due)]);
    kit.advance_days(5);
    assert!(escalations(&kit.tick(), Timer::Acknowledge).is_empty(), "the top level is raised once");
    assert_eq!(kit.complaint("c1").escalated[&Timer::Acknowledge], Level::ConductOfficer);
}

/// Purpose: a timer skipped straight past its warning is raised at the
/// level it has reached, and the acknowledgement or an update restarts the
/// update timer with its escalation cleared.
/// Why it matters: the scheduler can be down for days; when it returns the
/// lead must hear of the miss, not of a warning. And a complainant updated
/// today must produce a fresh warning when the next update nears, or the
/// second update is never prompted.
#[test]
fn the_update_timer_restarts_with_each_contact() {
    let mut kit = Kit::ready();
    kit.record(claim_intake("c1"));
    kit.assign("c1", "reviewer-1", 3);
    let received = kit.today();

    // Nothing sent for twelve days: both the acknowledgement and the first
    // update are missed outright.
    kit.advance_days(12);
    let events = kit.tick();
    assert_eq!(
        escalations(&events, Timer::Acknowledge),
        vec![(Level::ConductOfficer, received + Duration::days(5))]
    );
    assert_eq!(escalations(&events, Timer::Update), vec![(Level::Lead, received + Duration::days(10))]);

    kit.ok(
        "complaints",
        "reviewer-1",
        Command::RecordAcknowledgement {
            complaint: "c1".into(),
            document: "doc-ack".into(),
        },
    );
    let acknowledged = kit.today();
    assert!(!kit.complaint("c1").escalated.contains_key(&Timer::Update));
    assert!(kit
        .state
        .running(kit.complaint("c1"))
        .contains(&(Timer::Update, acknowledged + Duration::days(10))));
    assert!(!kit.state.running(kit.complaint("c1")).iter().any(|(t, _)| *t == Timer::Acknowledge));

    kit.advance_days(8);
    let events = kit.tick();
    assert_eq!(escalations(&events, Timer::Update), vec![(Level::Warning, acknowledged + Duration::days(10))]);

    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::RecordUpdate {
                complaint: "c1".into(),
                document: String::new()
            }
        ),
        "invalid"
    );
    assert_eq!(
        kit.ok(
            "complaints",
            "reviewer-1",
            Command::RecordUpdate {
                complaint: "c1".into(),
                document: "doc-update-1".into()
            }
        ),
        vec![Event::ComplainantUpdated { complaint: "c1".into() }]
    );
    let updated = kit.today();
    assert_eq!(kit.complaint("c1").updates, vec![updated]);
    assert!(escalations(&kit.tick(), Timer::Update).is_empty());
    kit.advance_days(8);
    assert_eq!(escalations(&kit.tick(), Timer::Update), vec![(Level::Warning, updated + Duration::days(10))]);
}

/// Purpose: when the scheme's period ends without an outcome the complaint
/// is deadlocked by the passing of time; the insurer can also declare it.
/// Either way a notice is owed before the complaint can close, and an
/// outcome can still be given afterwards.
/// Why it matters: deadlock gives the complainant the right to go to the
/// external scheme. If it depended on the insurer saying so, a complaint
/// could be left open for ever to keep it away from the scheme.
#[test]
fn deadlock_arrives_by_time_or_by_declaration_and_owes_a_notice() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    kit.in_review("c2");

    assert_eq!(
        kit.ok("complaints", "reviewer-1", Command::DeclareDeadlock { complaint: "c2".into() }),
        vec![Event::DeadlockReached {
            complaint: "c2".into(),
            automatic: false
        }]
    );
    assert_eq!(
        kit.err("complaints", "reviewer-1", Command::DeclareDeadlock { complaint: "c2".into() }),
        "not_open"
    );
    assert_eq!(kit.complaint("c2").status, Status::Deadlocked);

    kit.advance_days(40);
    assert!(!kit.tick().contains(&Event::DeadlockReached {
        complaint: "c1".into(),
        automatic: true
    }));
    kit.advance_days(1);
    let events = kit.tick();
    assert!(events.contains(&Event::DeadlockReached {
        complaint: "c1".into(),
        automatic: true
    }));
    assert!(escalations(&events, Timer::Deadlock).is_empty(), "deadlock is not an escalation");
    let c1 = kit.complaint("c1");
    assert_eq!(c1.status, Status::Deadlocked);
    assert_eq!(
        c1.deadlock,
        Some(Deadlock {
            on: kit.today(),
            automatic: true,
            notice: None
        })
    );
    assert!(!kit.state.running(c1).iter().any(|(t, _)| *t == Timer::Deadlock));
    assert!(!kit.tick().iter().any(|e| matches!(e, Event::DeadlockReached { .. })), "reached once");

    // The notice.
    let notice = |doc: &str| Command::RecordDeadlockNotice {
        complaint: "c1".into(),
        document: doc.into(),
    };
    assert_eq!(kit.err("complaints", "reviewer-1", notice("")), "no_deadlock");
    // An outcome can still be given; without the notice it cannot close.
    kit.decide("c1", OutcomeKind::NotUpheld);
    kit.delivered("c1");
    let close = || Command::Close {
        complaint: "c1".into(),
        detail: detail("delay"),
    };
    assert_eq!(kit.err("complaints", "reviewer-1", close()), "no_deadlock_notice");
    assert_eq!(
        kit.ok("complaints", "reviewer-1", notice("doc-deadlock")),
        vec![Event::DeadlockNoticeSent { complaint: "c1".into() }]
    );
    assert_eq!(kit.err("complaints", "reviewer-1", notice("doc-again")), "no_deadlock");
    kit.ok("complaints", "reviewer-1", close());
    assert!(kit.state.subject_record("c1").unwrap().documents.contains(&"doc-deadlock".to_string()));

    // A complaint that was never deadlocked has no notice to record.
    kit.in_review("c3");
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::RecordDeadlockNotice {
                complaint: "c3".into(),
                document: "doc".into()
            }
        ),
        "no_deadlock"
    );
    // A deadlocked complaint without an outcome or a forum cannot close.
    assert_eq!(
        kit.err(
            "complaints",
            "reviewer-1",
            Command::Close {
                complaint: "c2".into(),
                detail: detail("delay")
            }
        ),
        "no_outcome"
    );
}

/// Purpose: a complainant asked for something has a set time to answer,
/// longer if they are marked vulnerable; a late answer prompts the handler
/// once and never escalates, and the insurer's own timers do not move.
/// Why it matters: the regime allows a vulnerable customer more time, not
/// the insurer. Extending the insurer's deadline because it asked a
/// question would let any complaint be stalled by asking one.
#[test]
fn a_vulnerable_complainant_gets_longer_and_the_insurer_does_not() {
    let mut kit = Kit::ready();
    kit.in_review("c1");
    let mut vulnerable = claim_intake("c2");
    vulnerable.vulnerable = true;
    vulnerable.adjustments = vec!["call in the afternoon".into()];
    kit.record(vulnerable);
    assert_eq!(
        kit.err(
            "complaints",
            "lead",
            Command::Assign {
                complaint: "c2".into(),
                handler: "reviewer-1".into(),
                authority: 3,
                trained_for_vulnerability: false
            }
        ),
        "not_trained"
    );
    kit.assign("c2", "reviewer-1", 3);
    let today = kit.today();
    let before = kit.state.running(kit.complaint("c1"));

    let ask = |id: &str| Command::AskComplainant { complaint: id.into() };
    let answered = |id: &str| Command::RecordComplainantResponse { complaint: id.into() };
    assert_eq!(kit.err("complaints", "reviewer-1", answered("c1")), "not_awaiting");
    kit.ok("complaints", "reviewer-1", ask("c1"));
    kit.ok("complaints", "reviewer-1", ask("c2"));
    assert_eq!(kit.err("complaints", "reviewer-1", ask("c1")), "already_awaiting");
    assert_eq!(
        kit.complaint("c1").awaiting,
        Some(Awaiting {
            asked_on: today,
            due: today + Duration::days(10)
        })
    );
    assert_eq!(kit.complaint("c2").awaiting.as_ref().unwrap().due, today + Duration::days(20));

    // The insurer's timers are where they were, with the complainant's added.
    let mut after = kit.state.running(kit.complaint("c1"));
    assert_eq!(after.pop(), Some((Timer::ComplainantResponse, today + Duration::days(10))));
    assert_eq!(after, before);

    // The marker can be set later and takes effect on the next request.
    kit.ok(
        "complaints",
        "reviewer-1",
        Command::SetVulnerability {
            complaint: "c1".into(),
            vulnerable: true,
            adjustments: vec!["large print".into()],
        },
    );
    assert!(kit.complaint("c1").vulnerable);
    assert_eq!(kit.complaint("c1").adjustments, vec!["large print".to_string()]);

    // No warning to the complainant; one prompt to the handler when late.
    kit.advance_days(9);
    assert!(escalations(&kit.tick(), Timer::ComplainantResponse).is_empty());
    kit.advance_days(2);
    assert_eq!(
        escalations(&kit.tick(), Timer::ComplainantResponse),
        vec![(Level::Lead, today + Duration::days(10))]
    );
    kit.advance_days(8);
    assert!(
        escalations(&kit.tick(), Timer::ComplainantResponse).is_empty(),
        "c2 still has a day, c1 was prompted"
    );
    kit.advance_days(2);
    assert_eq!(
        escalations(&kit.tick(), Timer::ComplainantResponse),
        vec![(Level::Lead, today + Duration::days(20))]
    );

    kit.ok("complaints", "reviewer-1", answered("c1"));
    assert_eq!(kit.complaint("c1").awaiting, None);
    assert!(!kit.complaint("c1").escalated.contains_key(&Timer::ComplainantResponse));
    // Asked again, now marked vulnerable: the longer time.
    kit.ok("complaints", "reviewer-1", ask("c1"));
    assert_eq!(kit.complaint("c1").awaiting.as_ref().unwrap().due, kit.today() + Duration::days(20));
}

fn spot(id: &str) -> Command {
    let mut i = intake(id, Process::Service);
    i.resolved_on_the_spot = Some(FrontLine {
        outcome: OutcomeKind::Upheld,
        reason_codes: vec!["apology".into()],
        cause: "delay".into(),
        put_right: None,
    });
    Command::RecordComplaint(Box::new(i))
}

/// Purpose: a closed complaint's record is removed the day after its
/// retention ends, counted, announced, and taken out of the complainant's
/// list while their other complaints stay.
/// Why it matters: a complaint record is personal data kept for a stated
/// time. Keeping it longer is a privacy breach; removing the index entry
/// for the wrong complaint would hide a repeat complainant.
#[test]
fn a_closed_complaint_is_removed_when_its_retention_ends() {
    let mut kit = Kit::ready();
    kit.ok("intake", "agent-1", spot("c1"));
    kit.advance_days(1);
    kit.ok("intake", "agent-1", spot("c2"));
    kit.record(intake("c3", Process::Billing));
    assert_eq!(kit.state.complaints_of(&party("aroha")), vec!["c1", "c2", "c3"]);

    kit.advance_days(99);
    assert!(
        !kit.tick().iter().any(|e| matches!(e, Event::ComplaintRecordRemoved { .. })),
        "kept through the last day"
    );
    kit.advance_days(1);
    assert!(kit.tick().contains(&Event::ComplaintRecordRemoved { complaint: "c1".into() }));
    assert!(!kit.state.complaints.contains_key("c1"));
    assert_eq!(kit.state.removed, 1);
    assert_eq!(kit.state.complaints_of(&party("aroha")), vec!["c2", "c3"]);
    assert!(kit.state.subject_record("c1").is_none());

    kit.advance_days(1);
    assert!(kit.tick().contains(&Event::ComplaintRecordRemoved { complaint: "c2".into() }));
    assert_eq!(kit.state.removed, 2);
    // The open complaint is never removed, however old.
    kit.advance_days(400);
    kit.tick();
    assert_eq!(kit.state.complaints_of(&party("aroha")), vec!["c3"]);
    assert_eq!(kit.state.report(kit.today() - Duration::days(600), kit.today()).records_removed, 2);
}

/// Purpose: when the last complaint a party has is removed, the party
/// disappears from the index altogether.
/// Why it matters: an index entry that outlives the records is itself a
/// record that the person once complained.
#[test]
fn removing_a_partys_last_complaint_removes_the_party() {
    let mut kit = Kit::ready();
    kit.ok("intake", "agent-1", spot("c1"));
    kit.advance_days(101);
    kit.tick();
    assert!(kit.state.complaints_of(&party("aroha")).is_empty());
    let saved = serde_json::to_value(&kit.state).unwrap();
    assert_eq!(saved["by_complainant"], serde_json::json!({}));
}

/// Purpose: one tick examines a bounded number of complaints and the next
/// carries on where it stopped, so every complaint is reached.
/// Why it matters: a command that grows with the register would one day be
/// too large to apply. A bound without a cursor would starve the complaints
/// at the end of the list of their escalations for ever.
#[test]
fn a_tick_is_bounded_and_carries_on_where_it_stopped() {
    let mut kit = Kit::ready();
    for n in 0..=MAX_CHUNK {
        kit.record(intake(&format!("c{n:04}"), Process::Service));
    }
    kit.advance_days(3);
    let first = kit.tick();
    assert_eq!(first.len(), MAX_CHUNK);
    let second = kit.tick();
    assert_eq!(
        second,
        vec![Event::TimerEscalated {
            subject: format!("c{MAX_CHUNK:04}"),
            timer: Timer::Acknowledge,
            level: Level::Warning,
            due: kit.today() + Duration::days(2)
        }]
    );
    assert!(kit.tick().is_empty(), "the third tick starts again from the top and finds nothing new");
}

/// Purpose: a migrated complaint keeps the date it was first received and
/// its acknowledgement, so its timers continue; nobody else may back-date,
/// and dates out of order are refused.
/// Why it matters: a complaint open at cut-over is already partway through
/// its regulatory time. Restarting its clock would hide a late complaint;
/// letting any caller back-date would let one be made to look on time.
#[test]
fn a_migrated_complaint_keeps_its_clock() {
    let mut kit = Kit::ready();
    let today = kit.today();
    let mut old = claim_intake("m1");
    old.received_on = Some(today - Duration::days(18));
    old.acknowledged_on = Some(today - Duration::days(16));
    let events = kit.ok("migration", "cutover", Command::RecordComplaint(Box::new(old.clone())));
    assert_eq!(events.len(), 1, "no acknowledgement is asked for again");
    let c = kit.complaint("m1");
    assert!(c.migrated);
    assert_eq!(c.received_on, today - Duration::days(18));
    assert!(kit.state.running(c).contains(&(Timer::Decide, today + Duration::days(2))));
    assert_eq!(escalations(&kit.tick(), Timer::Decide), vec![(Level::Warning, today + Duration::days(2))]);

    let mut refused = |caller: &str, change: &dyn Fn(&mut ic_complaints::command::Intake)| {
        let mut i = old.clone();
        i.complaint = "m2".into();
        change(&mut i);
        kit.err(caller, "cutover", Command::RecordComplaint(Box::new(i)))
    };
    assert_eq!(refused("intake", &|_| {}), "invalid");
    assert_eq!(refused("complaints", &|i| i.received_on = None), "invalid");
    assert_eq!(refused("migration", &|i| i.received_on = Some(today + Duration::days(1))), "invalid");
    assert_eq!(refused("migration", &|i| i.acknowledged_on = Some(today - Duration::days(19))), "invalid");
    assert_eq!(refused("migration", &|i| i.acknowledged_on = Some(today + Duration::days(1))), "invalid");
}

/// Purpose: a complaint runs for its whole life under the timers in force
/// when it was received, whatever is activated later.
/// Why it matters: changing the rules for a complaint already running
/// would make its deadlines unknowable to the person it was promised to,
/// and a report could not say which standard a complaint was held to.
#[test]
fn a_complaint_keeps_the_timers_it_was_received_under() {
    let mut kit = Kit::ready();
    kit.record(intake("c1", Process::Service));
    let mut quicker = timers();
    quicker.version = 2;
    quicker.acknowledge_days = 2;
    kit.ok("config", "ops", Command::ActivateTimers(quicker.clone()));
    kit.record(intake("c2", Process::Service));
    let today = kit.today();

    assert_eq!(kit.complaint("c1").timers_version, 1);
    assert_eq!(kit.complaint("c2").timers_version, 2);
    assert_eq!(kit.state.running(kit.complaint("c1"))[0], (Timer::Acknowledge, today + Duration::days(5)));
    assert_eq!(kit.state.running(kit.complaint("c2"))[0], (Timer::Acknowledge, today + Duration::days(2)));
    assert_eq!(kit.state.timers(2), Some(&quicker));
    assert_eq!(kit.state.timers(1).unwrap().acknowledge_days, 5);
    assert_eq!(kit.state.timers(3), None);
    assert_eq!(kit.state.register(today, today)[0].timers_version, 1);
}

/// Purpose: the module's time never goes backwards, whatever clock a
/// proposer has.
/// Why it matters: proposers run on different machines. A command stamped
/// earlier than one already applied must not date a record before events
/// that came first, or un-miss a deadline.
#[test]
fn time_never_runs_backwards() {
    let mut kit = Kit::ready();
    kit.advance_days(10);
    kit.record(intake("c1", Process::Service));
    let later = kit.today();
    kit.advance_days(-4);
    kit.record(intake("c2", Process::Service));
    assert_eq!(kit.complaint("c2").received_on, later);
    assert_eq!(kit.state.now, Some(kit.now + Duration::days(4)));
}

/// Purpose: a command with a key already seen returns the original result,
/// accepted or refused, and does nothing more.
/// Why it matters: every caller retries. A retried intake must not record
/// a second complaint, and a retried refusal must stay refused even if the
/// state has since changed, or two callers would disagree on what happened.
#[test]
fn a_repeated_key_returns_the_original_result() {
    let mut kit = Kit::ready();
    let record = || Command::RecordComplaint(Box::new(intake("c1", Process::Service)));
    let first = kit.send_keyed("intake-1", "intake", "agent-1", record());
    assert!(first.result.is_ok() && !first.duplicate);
    assert_eq!(first.events.len(), 2);
    let again = kit.send_keyed("intake-1", "intake", "agent-1", record());
    assert!(again.duplicate && again.result.is_ok() && again.events.is_empty());
    assert_eq!(again.index, first.index);
    assert_eq!(kit.state.complaints.len(), 1);

    // A refusal is remembered too.
    let close = || Command::Close {
        complaint: "c1".into(),
        detail: detail("delay"),
    };
    let refused = kit.send_keyed("close-1", "complaints", "lead", close());
    assert_eq!(refused.result.as_ref().unwrap_err().code, "no_outcome");
    let again = kit.send_keyed("close-1", "complaints", "lead", close());
    assert!(again.duplicate);
    assert_eq!(again.result, refused.result);
    assert_eq!(again.index, refused.index);
}
