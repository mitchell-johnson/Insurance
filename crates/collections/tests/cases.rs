//! What people do: outcomes, arrangements and hardship.

mod common;

use common::*;
use ic_collections::machine::{Command, CoreFact, Event};
use ic_collections::model::{Hold, TaskKind};
use ic_collections::{Reason, Relief, Role, StepKind};
use ic_core::arrangement::Instalment;
use ic_core::PolicyId;

fn call_task(world: &World) -> String {
    world.state.tasks.values().find(|t| t.kind == TaskKind::Call).unwrap().id.clone()
}

fn in_call(world: &mut World) {
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    world.close_through(d(2027, 3, 18));
}

fn outcome(world: &mut World, task: &str, reason: Reason) -> ic_collections::Applied {
    world.officer(Command::RecordOutcome {
        task: task.to_string(),
        reason,
        note: "spoke to the owner".to_string(),
    })
}

/// Purpose: COL-013. Each outcome is a reason code that drives the next
/// step: no contact keeps the task open and counts the attempt; a reduction
/// or cancellation opens a servicing task; a dispute stops reminders and
/// goes to the team lead; a death stops everything and goes to claims.
/// Why it matters: the notes make outcomes reason codes so the next step
/// happens without someone remembering it, and so outcomes can be measured.
#[test]
fn an_outcome_drives_the_next_step() {
    let mut world = World::new();
    in_call(&mut world);
    let task = call_task(&world);
    assert_eq!(
        world
            .officer(Command::RecordOutcome {
                task: task.clone(),
                reason: Reason::NoContact,
                note: " ".into()
            })
            .result
            .unwrap_err()
            .code,
        "no_note"
    );
    assert_eq!(outcome(&mut world, &task, Reason::NoContact).result, Ok(()));
    assert_eq!(world.state.tasks[&task].attempts, 1);
    assert!(world.state.open_tasks.contains(&task));

    assert_eq!(outcome(&mut world, &task, Reason::Disputed).result, Ok(()));
    assert_eq!(world.state.policies[&PolicyId::from("p1")].hold, Some(Hold::Disputed));
    assert!(world.state.tasks.values().any(|t| t.kind == TaskKind::Dispute));
    assert_eq!(world.state.tasks_for(Role::TeamLead, 10).len(), 1);
    assert_eq!(outcome(&mut world, &task, Reason::Paid).result.unwrap_err().code, "task_closed");
    assert_eq!(outcome(&mut world, "task-x", Reason::Paid).result.unwrap_err().code, "unknown_task");

    // Disputed: reminders wait, the suspension notice still goes.
    let events = world.close_through(d(2027, 4, 1));
    assert_eq!(reached(&events), vec![("p1".to_string(), 31, StepKind::SuspensionNotice)]);

    let mut world = World::new();
    in_call(&mut world);
    let task = call_task(&world);
    outcome(&mut world, &task, Reason::CancellationRequested);
    assert!(world.state.tasks.values().any(|t| t.kind == TaskKind::Servicing));

    let mut world = World::new();
    in_call(&mut world);
    let task = call_task(&world);
    outcome(&mut world, &task, Reason::DeceasedNotified);
    assert!(world.state.tasks.values().any(|t| t.kind == TaskKind::Deceased));
    assert!(reached(&world.close_through(d(2027, 4, 20))).is_empty());
    // Paid off, the death hold stays: nothing is sent about the next spell.
    world.paid("p1");
    assert_eq!(world.state.policies[&PolicyId::from("p1")].hold, Some(Hold::Deceased));
}

/// Purpose: a hardship referral holds reminders for the referral period;
/// with no application by then, the sequence carries on.
/// Why it matters: a referral must give the customer room to apply, but a
/// referral nobody follows up must not leave a policy to lapse unchased.
#[test]
fn a_referral_holds_reminders_until_it_runs_out() {
    let mut world = World::new();
    in_call(&mut world);
    let task = call_task(&world);
    outcome(&mut world, &task, Reason::HardshipReferral);
    assert!(matches!(world.state.policies[&PolicyId::from("p1")].hold, Some(Hold::HardshipReferral { .. })));
    let events = world.close_through(d(2027, 4, 15));
    let steps = reached(&events);
    assert_eq!(steps.iter().map(|s| s.1).collect::<Vec<_>>(), vec![31, 45]);
    assert_eq!(world.state.policies[&PolicyId::from("p1")].hold, None);
}

fn instalments(amounts: &[(u32, i64)]) -> Vec<Instalment> {
    amounts
        .iter()
        .map(|(day, amount_minor)| Instalment {
            due: d(2027, 4, *day),
            amount_minor: *amount_minor,
        })
        .collect()
}

/// Purpose: COL-015. An officer's arrangement goes to the core through the
/// outbox under the caller the core accepts; while the core keeps it the
/// sequence is held and the queue shows its review date; when it breaks the
/// lapse clock resumes from where it stood and a task is opened.
/// Why it matters: an arrangement is a promise to the customer that cover
/// is safe while they keep it; a reminder or a suspension notice during it
/// would break that promise, and a broken one must not cost them the days it
/// ran.
#[test]
fn an_arrangement_holds_the_sequence_and_a_break_resumes_the_clock() {
    let mut world = World::new();
    in_call(&mut world);
    let plan = instalments(&[(1, 5_000), (15, 5_000)]);
    let applied = world.officer(Command::ProposeArrangement {
        policy: "p1".into(),
        arrangement: "a1".into(),
        instalments: plan.clone(),
    });
    assert_eq!(applied.result, Ok(()));
    let posting = world.state.outbox.values().next().unwrap().clone();
    assert!(matches!(&posting.command, ic_core::Command::MakeArrangement { arrangement, .. } if arrangement == "a1"));
    let envelope = ic_collections::query::core_envelope(&posting, "salt".into());
    assert!(envelope.command.allowed_callers().contains(&envelope.caller.as_str()));
    let again = world.officer(Command::ProposeArrangement {
        policy: "p1".into(),
        arrangement: "a1".into(),
        instalments: plan,
    });
    assert_eq!(again.result.unwrap_err().code, "arrangement_pending");
    world.outbox(Command::RecordCoreOutcome {
        posting: posting.id.clone(),
        applied: true,
        code: None,
        message: None,
    });
    assert!(world.state.outbox.is_empty());

    let made_on = world.today;
    world.core(
        0,
        CoreFact::ArrangementMade {
            policy: "p1".into(),
            arrangement: "a1".into(),
            arrears_minor: 10_000,
            dues: vec![d(2027, 4, 1), d(2027, 4, 15)],
        },
    );
    let queue = world.state.queue(world.today, Role::Collections, 10);
    assert_eq!(queue.items[0].segment, "arrangement");
    assert_eq!(queue.items[0].review, Some(d(2027, 4, 1)));
    assert_eq!(queue.items[0].days_to_lapse, None);
    assert!(reached(&world.close_through(d(2027, 4, 1))).is_empty());
    assert_eq!(
        world
            .officer(Command::ProposeArrangement {
                policy: "p1".into(),
                arrangement: "a2".into(),
                instalments: instalments(&[(20, 1)])
            })
            .result
            .unwrap_err()
            .code,
        "arrangement_exists"
    );

    // Broken on 2 April after running 14 days: day 31 is reached 14 days
    // later than it would have been.
    let applied = world.core(
        0,
        CoreFact::ArrangementBroken {
            policy: "p1".into(),
            arrangement: "a1".into(),
            outstanding_minor: 5_000,
        },
    );
    assert!(applied.events.iter().any(|r| matches!(
        r.event,
        Event::TaskOpened {
            kind: TaskKind::Arrangement,
            ..
        }
    )));
    let ran = (world.today - made_on).num_days();
    let p = &world.state.policies[&PolicyId::from("p1")];
    assert_eq!(p.pause.unwrap().days, ran);
    assert_eq!(p.age(world.today), Some(32 - ran));
    let events = world.close_through(d(2027, 4, 14));
    assert!(reached(&events).is_empty());
    let events = world.close_through(d(2027, 4, 15));
    assert_eq!(reached(&events), vec![("p1".to_string(), 31, StepKind::SuspensionNotice)]);
}

/// Purpose: a kept arrangement that completes is cleared, and an officer can
/// end one the customer cannot keep, which goes to the core as its end
/// command.
/// Why it matters: the queue's arrangement view must show only live
/// promises, and ending one is a core command so the core restarts the
/// lapse clock by its own rule.
#[test]
fn an_arrangement_completes_or_is_ended() {
    let mut world = World::new();
    in_call(&mut world);
    assert_eq!(
        world.officer(Command::EndArrangement { policy: "p1".into() }).result.unwrap_err().code,
        "no_arrangement"
    );
    let made = CoreFact::ArrangementMade {
        policy: "p1".into(),
        arrangement: "a1".into(),
        arrears_minor: 10_000,
        dues: vec![d(2027, 4, 1)],
    };
    world.core(0, made);
    assert_eq!(world.officer(Command::EndArrangement { policy: "p1".into() }).result, Ok(()));
    assert!(matches!(
        world.state.outbox.values().next().unwrap().command,
        ic_core::Command::EndArrangement { .. }
    ));
    let applied = world.core(
        0,
        CoreFact::ArrangementCompleted {
            policy: "p1".into(),
            arrangement: "a1".into(),
        },
    );
    assert!(matches!(applied.events[0].event, Event::ArrangementCompleted { .. }));
    assert!(world.state.policies[&PolicyId::from("p1")].kept.is_none());
    // Facts for a policy never seen change nothing.
    for fact in [
        CoreFact::ArrangementMade {
            policy: "ghost".into(),
            arrangement: "a".into(),
            arrears_minor: 1,
            dues: vec![],
        },
        CoreFact::ArrangementCompleted {
            policy: "ghost".into(),
            arrangement: "a".into(),
        },
        CoreFact::ArrangementBroken {
            policy: "ghost".into(),
            arrangement: "a".into(),
            outstanding_minor: 1,
        },
    ] {
        assert!(world.core(0, fact).events.is_empty());
    }
}

/// Purpose: an arrangement is checked before it goes to the core: the
/// policy must be in arrears, the name plain, the instalments positive,
/// ascending and from today, and no more than the officer's limit unless a
/// team lead agrees it. A refusal by the core becomes a task.
/// Why it matters: the notes set authority limits on arrangement length;
/// an arrangement the core would refuse should be caught while the officer
/// is still talking to the customer, and one it does refuse must not be
/// forgotten.
#[test]
fn an_arrangement_is_checked_and_a_refusal_is_a_task() {
    let mut world = World::new();
    world.issue("paid-up", "party-2", 10_000);
    let propose = |policy: &str, name: &str, plan: Vec<Instalment>| Command::ProposeArrangement {
        policy: policy.into(),
        arrangement: name.into(),
        instalments: plan,
    };
    assert_eq!(
        world.officer(propose("ghost", "a", instalments(&[(1, 1)]))).result.unwrap_err().code,
        "unknown_policy"
    );
    assert_eq!(
        world.officer(propose("paid-up", "a", instalments(&[(1, 1)]))).result.unwrap_err().code,
        "not_in_arrears"
    );
    in_call(&mut world);
    assert_eq!(
        world.officer(propose("p1", "a b", instalments(&[(1, 1)]))).result.unwrap_err().code,
        "invalid_arrangement"
    );
    assert_eq!(world.officer(propose("p1", "a", vec![])).result.unwrap_err().code, "invalid_arrangement");
    assert_eq!(
        world.officer(propose("p1", "a", instalments(&[(5, 1), (2, 1)]))).result.unwrap_err().code,
        "invalid_arrangement"
    );
    assert_eq!(
        world.officer(propose("p1", "a", instalments(&[(5, 0)]))).result.unwrap_err().code,
        "invalid_arrangement"
    );
    let long: Vec<(u32, i64)> = (1..=7).map(|n| (n, 100)).collect();
    assert_eq!(
        world.officer(propose("p1", "a", instalments(&long))).result.unwrap_err().code,
        "beyond_authority"
    );
    let lead = world.staff("lead-1", &[Role::TeamLead], propose("p1", "a", instalments(&long)));
    assert_eq!(lead.result, Ok(()));

    let posting = world.state.outbox.keys().next().unwrap().clone();
    let applied = world.outbox(Command::RecordCoreOutcome {
        posting: posting.clone(),
        applied: false,
        code: Some("invalid_arrangement".into()),
        message: Some("the instalments do not add up to the arrears".into()),
    });
    assert!(applied
        .events
        .iter()
        .any(|r| matches!(&r.event, Event::PostingRefused { code, .. } if code == "invalid_arrangement")));
    assert!(world.state.tasks.values().any(|t| t.kind == TaskKind::Arrangement));
    // The same outcome again changes nothing.
    let again = world.outbox(Command::RecordCoreOutcome {
        posting,
        applied: false,
        code: None,
        message: None,
    });
    assert!(again.events.is_empty());

    let mut other = World::new();
    other.state.sequences.clear();
    other.issue("p1", "party-1", 10_000);
    other.overdue("p1", d(2027, 3, 1));
    assert_eq!(
        other.officer(propose("p1", "a", instalments(&[(1, 1)]))).result.unwrap_err().code,
        "no_sequence"
    );
}

fn apply(world: &mut World, case: &str, relief: Relief, from: chrono::NaiveDate, to: chrono::NaiveDate) -> ic_collections::Applied {
    world.officer(Command::ApplyHardship {
        case: case.into(),
        party: "party-1".into(),
        policies: vec!["p1".into()],
        relief,
        from,
        to,
        discloses_health: true,
        note: "lost work after an injury".into(),
    })
}

/// Purpose: COL-014. A hardship application by an officer holds reminders
/// and opens a task for the hardship team; the applicant cannot decide it;
/// approval sends the core a dated override with the share of premium still
/// billed; ending it early in month four tells the core to end it that day.
/// Why it matters: the notes have hardship follow the bank's path: an
/// application, a decision by someone else, and an override with dates that
/// the core applies with no retrospective charge.
#[test]
fn hardship_is_applied_for_decided_by_another_and_ended_early() {
    let mut world = World::new();
    in_call(&mut world);
    let (from, to) = (d(2027, 3, 20), d(2027, 9, 19));
    let applied = apply(&mut world, "h1", Relief::Reduced { percent: 50 }, from, to);
    assert_eq!(applied.result, Ok(()));
    assert!(world.state.tasks_for(Role::Hardship, 10).iter().any(|t| t.kind == TaskKind::HardshipDecision));
    assert!(matches!(world.state.policies[&PolicyId::from("p1")].hold, Some(Hold::HardshipReferral { .. })));
    // The same application again is the same case.
    assert_eq!(apply(&mut world, "h1", Relief::Reduced { percent: 50 }, from, to).result, Ok(()));
    assert_eq!(apply(&mut world, "h1", Relief::Holiday, from, to).result.unwrap_err().code, "duplicate_case");

    let decide = |approve: bool| Command::DecideHardship {
        case: "h1".into(),
        approve,
        note: "income evidence seen".into(),
    };
    assert_eq!(
        world.staff("officer-1", &[Role::Hardship], decide(true)).result.unwrap_err().code,
        "same_person"
    );
    assert_eq!(world.officer(decide(true)).result.unwrap_err().code, "not_allowed");
    assert_eq!(
        world
            .staff(
                "hardship-1",
                &[Role::Hardship],
                Command::DecideHardship {
                    case: "h1".into(),
                    approve: true,
                    note: "".into()
                }
            )
            .result
            .unwrap_err()
            .code,
        "no_note"
    );
    assert_eq!(
        world
            .staff("hardship-1", &[Role::Hardship], Command::EndHardship { case: "h1".into() })
            .result
            .unwrap_err()
            .code,
        "not_approved"
    );
    assert_eq!(world.staff("hardship-1", &[Role::Hardship], decide(true)).result, Ok(()));
    assert_eq!(
        world.staff("hardship-2", &[Role::Hardship], decide(false)).result.unwrap_err().code,
        "already_decided"
    );
    let grant = world.state.outbox.values().next().unwrap().clone();
    match &grant.command {
        ic_core::Command::GrantHardship {
            hardship,
            from: f,
            to: t,
            share_bps,
            ..
        } => {
            assert_eq!((hardship.as_str(), *f, *t, *share_bps), ("h1", from, to, 5_000));
        }
        other => panic!("not a grant: {other:?}"),
    }
    assert!(world.state.tasks_for(Role::Hardship, 10).is_empty());

    // Under relief nothing is sent, whatever the arrears.
    assert!(reached(&world.close_through(d(2027, 4, 30))).is_empty());

    // Ended early in month four: billing resumes that day.
    world.today = d(2027, 7, 5);
    let end = world.staff("hardship-1", &[Role::Hardship], Command::EndHardship { case: "h1".into() });
    assert_eq!(end.result, Ok(()));
    assert!(world
        .state
        .outbox
        .values()
        .any(|p| matches!(&p.command, ic_core::Command::EndHardship { hardship, .. } if hardship == "h1")));
    let case = &world.state.hardship["h1"];
    assert_eq!(case.last_day(), Some(d(2027, 7, 4)));
    assert!(!case.covers(d(2027, 7, 5)));
    world.today = d(2027, 9, 30);
    assert_eq!(
        world
            .staff("hardship-1", &[Role::Hardship], Command::EndHardship { case: "h1".into() })
            .result
            .unwrap_err()
            .code,
        "invalid_end"
    );
    assert_eq!(
        world
            .staff("hardship-1", &[Role::Hardship], Command::EndHardship { case: "none".into() })
            .result
            .unwrap_err()
            .code,
        "unknown_case"
    );
    // After relief the sequence takes up again from the arrears' age.
    let events = world.close_day();
    let days: Vec<u32> = reached(&events).iter().map(|s| s.1).collect();
    assert_eq!(days, vec![31, 45]);
    assert!(world.state.policies[&PolicyId::from("p1")].relief.is_none());
}

/// Purpose: a holiday is sent to the core as no premium billed, a declined
/// application lifts the hold without relief, and a core refusal of a relief
/// goes back to the hardship team.
/// Why it matters: the share the core bills must be exactly what was
/// decided, and a relief the core could not apply must be seen by the team
/// that granted it.
#[test]
fn a_holiday_a_decline_and_a_refused_relief() {
    let mut world = World::new();
    in_call(&mut world);
    apply(&mut world, "h1", Relief::Holiday, d(2027, 3, 20), d(2027, 6, 19));
    world.staff(
        "hardship-1",
        &[Role::Hardship],
        Command::DecideHardship {
            case: "h1".into(),
            approve: true,
            note: "ok".into(),
        },
    );
    let grant = world.state.outbox.values().next().unwrap().clone();
    assert!(matches!(grant.command, ic_core::Command::GrantHardship { share_bps: 0, .. }));
    let refused = world.outbox(Command::RecordCoreOutcome {
        posting: grant.id,
        applied: false,
        code: Some("not_retail".into()),
        message: None,
    });
    assert!(refused.events.iter().any(|r| matches!(
        r.event,
        Event::TaskOpened {
            kind: TaskKind::HardshipDecision,
            ..
        }
    )));

    let mut world = World::new();
    in_call(&mut world);
    apply(&mut world, "h2", Relief::Holiday, d(2027, 3, 20), d(2027, 6, 19));
    world.staff(
        "hardship-1",
        &[Role::Hardship],
        Command::DecideHardship {
            case: "h2".into(),
            approve: false,
            note: "no".into(),
        },
    );
    let p = &world.state.policies[&PolicyId::from("p1")];
    assert_eq!((p.hold, p.relief.clone()), (None, None));
    assert!(world.state.outbox.is_empty());
}

/// Purpose: an application is checked: a plain case name, the party's own
/// policies, relief from today for at most two years, a reduction between
/// 1 and 99 per cent, and a note.
/// Why it matters: the core refuses an override it cannot apply; catching it
/// here keeps the hardship team from deciding a case that cannot be given.
#[test]
fn a_hardship_application_is_checked() {
    let mut world = World::new();
    in_call(&mut world);
    world.issue("p9", "party-9", 10_000);
    let base = |case: &str, policies: Vec<PolicyId>, relief: Relief, from, to, note: &str| Command::ApplyHardship {
        case: case.into(),
        party: "party-1".into(),
        policies,
        relief,
        from,
        to,
        discloses_health: false,
        note: note.into(),
    };
    let today = world.today;
    let later = d(2027, 6, 1);
    for (command, code) in [
        (base("bad id", vec!["p1".into()], Relief::Holiday, today, later, "n"), "invalid_case"),
        (base("h", vec![], Relief::Holiday, today, later, "n"), "invalid_case"),
        (base("h", vec!["p9".into()], Relief::Holiday, today, later, "n"), "invalid_case"),
        (base("h", vec!["p1".into()], Relief::Holiday, d(2027, 3, 1), later, "n"), "invalid_case"),
        (base("h", vec!["p1".into()], Relief::Holiday, later, today, "n"), "invalid_case"),
        (base("h", vec!["p1".into()], Relief::Holiday, today, d(2030, 1, 1), "n"), "invalid_case"),
        (base("h", vec!["p1".into()], Relief::Reduced { percent: 0 }, today, later, "n"), "invalid_case"),
        (
            base("h", vec!["p1".into()], Relief::Reduced { percent: 100 }, today, later, "n"),
            "invalid_case",
        ),
        (base("h", vec!["p1".into()], Relief::Holiday, today, later, " "), "no_note"),
    ] {
        assert_eq!(world.officer(command).result.unwrap_err().code, code);
    }
    assert_eq!(
        world
            .staff(
                "hardship-1",
                &[Role::Hardship],
                Command::DecideHardship {
                    case: "none".into(),
                    approve: true,
                    note: "n".into()
                }
            )
            .result
            .unwrap_err()
            .code,
        "unknown_case"
    );
    // A refused command's retry gets the same answer.
    let command = base("h", vec![], Relief::Holiday, today, later, "n");
    let first = world.send_keyed("same", "staff", "officer-1", &[Role::Collections], command.clone());
    let second = world.send_keyed("same", "staff", "officer-1", &[Role::Collections], command);
    assert!(second.duplicate);
    assert_eq!(first.result, second.result);
}

/// Purpose: an officer reads a hardship case without a note that discloses
/// health, and is told it was withheld; the hardship team reads it whole.
/// Why it matters: the notes keep collections officers to ordinary and
/// financial classes; health disclosed in a hardship application belongs to
/// the hardship team alone.
#[test]
fn an_officer_does_not_read_health_in_a_hardship_note() {
    let mut world = World::new();
    in_call(&mut world);
    apply(&mut world, "h1", Relief::Holiday, d(2027, 3, 20), d(2027, 6, 19));
    let officer = world.state.hardship_case("h1", Role::Collections).unwrap();
    assert_eq!(officer["note"], "");
    assert_eq!(officer["withheld"][0], "hardship_note");
    let team = world.state.hardship_case("h1", Role::Hardship).unwrap();
    assert_eq!(team["note"], "lost work after an injury");
    assert!(team["withheld"].as_array().unwrap().is_empty());
    assert!(world.state.hardship_case("none", Role::Hardship).is_none());
}

/// Purpose: an officer asks the core to reinstate a lapsed policy; one
/// request waits at a time; a refusal for want of underwriting opens a task
/// to start the reinstatement proposal, any other refusal a reinstatement
/// task; asking again after approval is a new posting.
/// Why it matters: the customer has paid and is waiting for their cover.
/// A refusal nobody sees leaves them uninsured without knowing it, and a
/// retry under the old posting would be answered with the old refusal.
#[test]
fn a_reinstatement_is_asked_of_the_core_and_its_refusal_is_a_task() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    let ask = Command::RequestReinstatement { policy: PolicyId::from("p1") };
    assert_eq!(world.officer(ask.clone()).result.unwrap_err().code, "not_lapsed");
    let unknown = Command::RequestReinstatement { policy: PolicyId::from("p9") };
    assert_eq!(world.officer(unknown).result.unwrap_err().code, "unknown_policy");
    world.core(
        0,
        CoreFact::Status {
            policy: PolicyId::from("p1"),
            to: ic_core::PolicyStatus::Lapsed,
        },
    );
    assert_eq!(world.officer(ask.clone()).result, Ok(()));
    assert_eq!(world.officer(ask.clone()).result.unwrap_err().code, "reinstatement_pending");
    let refuse = |world: &mut World, code: &str| {
        let posting = world.state.outbox.keys().next().unwrap().clone();
        world.outbox(Command::RecordCoreOutcome {
            posting,
            applied: false,
            code: Some(code.into()),
            message: None,
        });
    };
    let first = world.state.outbox.keys().next().unwrap().clone();
    refuse(&mut world, "underwriting_required");
    assert!(world.state.tasks.values().any(|t| t.kind == TaskKind::ReinstatementUnderwriting));

    assert_eq!(world.officer(ask.clone()).result, Ok(()));
    refuse(&mut world, "arrears_unpaid");
    assert!(world.state.tasks.values().any(|t| t.kind == TaskKind::Reinstatement));

    assert_eq!(world.officer(ask).result, Ok(()));
    let posting = world.state.outbox.keys().next().unwrap().clone();
    assert!(posting.starts_with("reinstate-") && world.state.outbox.len() == 1, "{posting}");
    assert_ne!(posting, first);
}

/// Purpose: when underwriting approves a reinstatement that the core sent
/// to it, collections asks the core again for the policy it is waiting on,
/// once; an approval for a policy collections is not waiting on asks
/// nothing. The core's event is read as that fact. While it waits, the
/// policy is listed for underwriting's driver to open its proposal.
/// Why it matters: the customer has done everything asked of them once
/// underwriting says yes. Waiting for an officer to notice would leave them
/// uncovered for days; asking for policies nobody is chasing would reinstate
/// cover the customer did not ask to restore.
#[test]
fn an_underwriting_approval_asks_the_core_again() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.issue("p2", "party-2", 10_000);
    for policy in ["p1", "p2"] {
        world.core(
            0,
            CoreFact::Status {
                policy: PolicyId::from(policy),
                to: ic_core::PolicyStatus::Lapsed,
            },
        );
    }
    let approved = |policy: &str| CoreFact::ReinstatementApproved {
        policy: PolicyId::from(policy),
    };
    world.core(0, approved("p1"));
    assert!(world.state.outbox.is_empty(), "nobody asked for p1 yet");

    assert_eq!(world.officer(Command::RequestReinstatement { policy: PolicyId::from("p1") }).result, Ok(()));
    let first = world.state.outbox.keys().next().unwrap().clone();
    world.outbox(Command::RecordCoreOutcome {
        posting: first.clone(),
        applied: false,
        code: Some("underwriting_required".into()),
        message: None,
    });
    assert!(world.state.outbox.is_empty());
    let wanted = || ic_collections::query::answer(&world.state, serde_json::json!({"q": "reinstatements_for_underwriting"})).unwrap();
    assert_eq!(wanted(), serde_json::json!(["p1"]), "underwriting's driver is told to open p1");

    world.core(0, approved("p2"));
    assert!(world.state.outbox.is_empty(), "p2 was not waiting on underwriting");
    world.core(0, approved("p1"));
    world.core(0, approved("p1"));
    assert_eq!(world.state.outbox.len(), 1, "asked once");
    let again = world.state.outbox.values().next().unwrap();
    assert!(matches!(&again.command, ic_core::Command::Reinstate { policy } if *policy == PolicyId::from("p1")));
    assert_ne!(again.id, first);

    let event = ic_core::Event::ReinstatementApproved {
        policy: PolicyId::from("p1"),
        reference: "uw-1".into(),
        valid_until: chrono::NaiveDate::from_ymd_opt(2027, 12, 31).unwrap(),
    };
    assert_eq!(ic_collections::query::fact(&event), Some(approved("p1")));
}
