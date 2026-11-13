//! The sequence running: steps as events, notices and tasks, and what stops
//! it.

mod common;

use common::*;
use ic_collections::machine::{Command, CoreFact, Event};
use ic_collections::model::{NoticeState, TaskKind};
use ic_collections::StepKind;
use ic_core::{PolicyId, PolicyStatus};

/// Purpose: COL-012. With the sequence of day 3, 10, 17, 31 and 45, a
/// policy unpaid from 1 March reaches each step on its day, each step is an
/// event, the reminders and mandatory notices are written for the
/// documents service, and the call step opens a task on the queue.
/// Why it matters: every notice and every call in collections traces to a
/// step event; that is how the business shows a customer was told, and how
/// an officer's day is made.
#[test]
fn each_step_is_an_event_with_its_notice_or_task() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    let events = world.close_through(d(2027, 4, 15));
    let steps = reached(&events);
    let days: Vec<u32> = steps.iter().map(|s| s.1).collect();
    assert_eq!(days, vec![3, 10, 17, 31, 45]);
    assert_eq!(steps[2].2, StepKind::CallTask);

    let notices: Vec<_> = world.state.notices.values().collect();
    assert_eq!(notices.len(), 4);
    let suspension = notices.iter().find(|n| n.kind == StepKind::SuspensionNotice).unwrap();
    assert_eq!(suspension.on, d(2027, 4, 1));
    assert!(suspension.proof_of_delivery);
    assert!(notices.iter().find(|n| n.kind == StepKind::Reminder).is_some_and(|n| !n.proof_of_delivery));
    let tasks: Vec<_> = world.state.tasks.values().filter(|t| t.kind == TaskKind::Call).collect();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].opened, d(2027, 3, 18));
    // Nothing is sent twice: closing more days adds nothing.
    assert!(reached(&world.close_through(d(2027, 4, 20))).is_empty());
}

/// Purpose: a policy that comes into arrears late in the sequence (a
/// dishonour found weeks later, or a migrated policy) gets only the latest
/// reminder it has reached and every mandatory notice it has reached; the
/// earlier reminders are recorded as passed.
/// Why it matters: the notes ask that a customer is never sent a step twice
/// or a pile of stale reminders at once, but a mandatory notice must never
/// be lost because the policy arrived after its day.
#[test]
fn a_late_arrival_gets_the_latest_reminder_and_every_mandatory_notice() {
    let mut world = World::new();
    world.today = d(2027, 4, 5);
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    let events = world.close_day();
    let sent: Vec<u32> = reached(&events).iter().map(|s| s.1).collect();
    assert_eq!(sent, vec![17, 31]);
    let passed: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            Event::StepPassed { day, .. } => Some(*day),
            _ => None,
        })
        .collect();
    assert_eq!(passed, vec![3, 10]);
}

/// Purpose: COL-026. A customer with three policies in arrears on the same
/// days gets one notice naming all three at each step, and one call task
/// covering all three.
/// Why it matters: three letters and three calls about one person's
/// arrears confuse the customer and waste the officer's day; the notes ask
/// for one message and one task.
#[test]
fn a_customer_with_several_policies_gets_one_notice_and_one_task() {
    let mut world = World::new();
    for p in ["p1", "p2", "p3"] {
        world.issue(p, "party-1", 10_000);
        world.overdue(p, d(2027, 3, 1));
    }
    world.issue("p4", "party-2", 10_000);
    world.overdue("p4", d(2027, 3, 1));
    world.close_through(d(2027, 3, 18));
    let reminders: Vec<_> = world.state.notices.values().filter(|n| n.party.0 == "party-1").collect();
    assert_eq!(reminders.len(), 2);
    assert!(reminders.iter().all(|n| n.policies.len() == 3));
    let calls: Vec<_> = world.state.tasks.values().filter(|t| t.kind == TaskKind::Call).collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls.iter().find(|t| t.party.0 == "party-1").unwrap().policies.len(), 3);
}

/// Purpose: a customer's policies on two partitions join one notice when
/// the second partition closes the day before the documents service has
/// taken it, and go in a notice of their own once it has.
/// Why it matters: partitions close at different moments; the grouping must
/// hold where it can and never add a policy to a letter already printed.
#[test]
fn a_notice_already_taken_is_not_added_to() {
    let mut world = World::new();
    world.issue_on(0, "p1", "party-1", 10_000);
    world.issue_on(1, "p2", "party-1", 10_000);
    world.issue_on(2, "p3", "party-1", 10_000);
    for p in ["p1", "p2", "p3"] {
        world.overdue(p, d(2027, 3, 1));
    }
    world.today = d(2027, 3, 4);
    let day = world.today;
    world.core(0, CoreFact::DayClosed { closed: day });
    world.core(1, CoreFact::DayClosed { closed: day });
    let first = world.state.notices_due(10)[0].id.clone();
    assert_eq!(world.state.notices[&first].policies.len(), 2);
    assert_eq!(world.delivery(Command::RecordNoticeSent { notice: first.clone() }).result, Ok(()));
    assert_eq!(world.state.notices[&first].state, NoticeState::Sent);
    let sent = ic_collections::query::answer(&world.state, serde_json::json!({"q": "notices_sent", "limit": 10})).unwrap();
    assert_eq!(sent[0]["id"], first.as_str());
    assert_eq!(world.delivery(Command::RecordNoticeSent { notice: first.clone() }).result, Ok(()));
    world.core(2, CoreFact::DayClosed { closed: day });
    let second = world.state.notices_due(10);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].id, format!("{first}:2"));
    assert_eq!(second[0].policies, vec![PolicyId::from("p3")]);
    assert_eq!(
        world.delivery(Command::RecordNoticeSent { notice: "nope".into() }).result.unwrap_err().code,
        "unknown_notice"
    );
}

/// Purpose: COL-018. A payment that takes the policy out of arrears stops
/// the sequence at once; a later spell of arrears starts it again from the
/// first step.
/// Why it matters: nothing in the sequence may keep chasing a customer who
/// has paid, and a customer who falls behind again months later is owed the
/// gentle first reminder, not the final notice.
#[test]
fn paying_stops_the_sequence_and_a_new_spell_starts_it_afresh() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    world.close_through(d(2027, 3, 12));
    world.paid("p1");
    let applied = world.core(0, CoreFact::DayClosed { closed: world.today });
    assert!(applied.events.is_empty());
    assert!(!world.state.in_arrears.contains(&PolicyId::from("p1")));

    world.today = d(2027, 6, 1);
    world.overdue("p1", d(2027, 6, 1));
    let events = world.close_through(d(2027, 6, 4));
    assert_eq!(reached(&events), vec![("p1".to_string(), 3, StepKind::Reminder)]);
}

/// Purpose: a policy whose annual premium is at or above the early call
/// threshold is called on the early day, not the sequence's call day.
/// Why it matters: the notes bring a person in earlier where more is at
/// stake; a large premium waiting until day 17 for a call is more likely to
/// be lost.
#[test]
fn a_high_premium_is_called_earlier() {
    let mut world = World::new();
    world.issue("big", "party-1", 50_000);
    world.overdue("big", d(2027, 3, 1));
    let events = world.close_through(d(2027, 3, 8));
    let steps = reached(&events);
    assert_eq!(steps.last().unwrap(), &("big".to_string(), 17, StepKind::CallTask));
    assert_eq!(world.state.tasks.values().next().unwrap().opened, d(2027, 3, 8));
    assert!(world.state.policies[&PolicyId::from("big")].annual_premium_minor >= 500_000);
}

/// Purpose: a call task open longer than the product's limit is escalated
/// to the team lead once, and then shows in the team lead's tasks.
/// Why it matters: an unworked call in the last weeks before suspension is
/// how customers lose cover without anyone speaking to them; the notes give
/// call tasks an age limit.
#[test]
fn a_call_left_too_long_goes_to_the_team_lead() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    let events = world.close_through(d(2027, 3, 25));
    let escalated = events.iter().filter(|e| matches!(e, Event::TaskEscalated { .. })).count();
    assert_eq!(escalated, 1);
    assert_eq!(world.state.tasks_for(ic_collections::Role::TeamLead, 10).len(), 1);
}

/// Purpose: a mandatory notice the documents service reports undeliverable
/// raises a task to reach the customer another way; a delivered one and an
/// undelivered reminder raise nothing.
/// Why it matters: the notes forbid lapsing on a notice nobody received; the
/// task is how a person finds out before the lapse day.
#[test]
fn an_undeliverable_mandatory_notice_raises_a_task() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    world.close_through(d(2027, 4, 1));
    let reminder = world.state.notices.values().find(|n| n.kind == StepKind::Reminder).unwrap().id.clone();
    let suspension = world.state.notices.values().find(|n| n.kind == StepKind::SuspensionNotice).unwrap().id.clone();
    let applied = world.delivery(Command::RecordNoticeDelivery {
        notice: reminder,
        delivered: false,
    });
    assert!(applied.events.is_empty());
    let applied = world.delivery(Command::RecordNoticeDelivery {
        notice: suspension.clone(),
        delivered: false,
    });
    assert!(applied
        .events
        .iter()
        .any(|r| matches!(&r.event, Event::NoticeUndeliverable { notice, .. } if *notice == suspension)));
    assert!(world.state.tasks.values().any(|t| t.kind == TaskKind::UndeliverableNotice));
    // Told twice, it is one task.
    let again = world.delivery(Command::RecordNoticeDelivery {
        notice: suspension.clone(),
        delivered: false,
    });
    assert!(again.events.is_empty());
    let applied = world.delivery(Command::RecordNoticeDelivery {
        notice: suspension,
        delivered: true,
    });
    assert!(applied.events.is_empty());
    let unknown = world.delivery(Command::RecordNoticeDelivery {
        notice: "none".into(),
        delivered: true,
    });
    assert_eq!(unknown.result.unwrap_err().code, "unknown_notice");
}

/// Purpose: once the core lapses or otherwise ends a policy its sequence
/// stops and it leaves the queue; reinstated, it is followed again.
/// Why it matters: a lapsed policy has no premium to chase; reminders about
/// cover that has ended would mislead the customer.
#[test]
fn an_ended_policy_leaves_the_sequence() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    let applied = world.core(
        0,
        CoreFact::Status {
            policy: PolicyId::from("p1"),
            to: PolicyStatus::Lapsed,
        },
    );
    assert!(applied
        .events
        .iter()
        .any(|r| matches!(&r.event, Event::SequenceStopped { why, .. } if why == "lapsed")));
    assert!(reached(&world.close_through(d(2027, 3, 20))).is_empty());
    world.core(
        0,
        CoreFact::Status {
            policy: PolicyId::from("p1"),
            to: PolicyStatus::InForce,
        },
    );
    assert!(!world.state.policies[&PolicyId::from("p1")].ended);
    // Facts about a policy the module never saw are ignored.
    for fact in [
        CoreFact::Status {
            policy: PolicyId::from("ghost"),
            to: PolicyStatus::Lapsed,
        },
        CoreFact::Standing {
            policy: PolicyId::from("ghost"),
            to: None,
            oldest_unpaid_due: None,
        },
        CoreFact::PremiumDue {
            policy: PolicyId::from("ghost"),
            period_start: d(2027, 1, 1),
            period_end: d(2027, 1, 31),
            total_minor: 1,
        },
    ] {
        assert!(world.core(0, fact).events.is_empty());
    }
}

/// Purpose: a partition's closed day takes the steps of that partition's
/// policies only; a product with no sequence is listed and sends nothing.
/// Why it matters: partitions close their days at different moments, and a
/// step taken before a partition's end-of-day has settled standing could
/// chase someone who paid that day.
#[test]
fn a_closed_day_takes_only_its_own_partitions_steps() {
    let mut world = World::new();
    world.issue_on(1, "p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    let applied = world.core(
        1,
        CoreFact::Issued {
            policy: PolicyId::from("other"),
            number: "N-other".into(),
            party: "party-9".into(),
            product: "WOL0".into(),
        },
    );
    assert_eq!(applied.result, Ok(()));
    world.overdue("other", d(2027, 3, 1));
    assert!(world.state.unsequenced.contains("WOL0"));
    assert!(reached(&world.close_through(d(2027, 3, 5))).is_empty());
    world.today = d(2027, 3, 5);
    let events = world.close_day_on(1);
    assert_eq!(reached(&events), vec![("p1".to_string(), 3, StepKind::Reminder)]);
}

/// Purpose: a payment that clears the oldest premium while a later one is
/// still overdue moves the arrears' age forward without a change of
/// standing, so the next step comes when the later premium has aged to it;
/// a balance with nothing unpaid ends the spell like a paid standing.
/// Why it matters: an age read only from standing changes would stay at
/// the oldest premium's date after it was paid, and send the suspension or
/// final notice weeks early.
#[test]
fn the_age_of_the_arrears_follows_the_oldest_unpaid_premium() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    world.close_through(d(2027, 3, 20));
    // The 1 March premium is paid; the one due 15 March is now the oldest.
    world.core(
        0,
        CoreFact::Balance {
            policy: "p1".into(),
            oldest_unpaid_due: Some(d(2027, 3, 15)),
        },
    );
    assert_eq!(world.state.policies[&PolicyId::from("p1")].age(d(2027, 4, 1)), Some(17));
    let events = world.close_through(d(2027, 4, 14));
    assert!(reached(&events).is_empty());
    let events = world.close_through(d(2027, 4, 15));
    assert_eq!(reached(&events), vec![("p1".to_string(), 31, StepKind::SuspensionNotice)]);
    let applied = world.core(
        0,
        CoreFact::Balance {
            policy: "p1".into(),
            oldest_unpaid_due: None,
        },
    );
    assert!(applied
        .events
        .iter()
        .any(|r| matches!(&r.event, Event::SequenceStopped { why, .. } if why == "paid")));
    assert!(world
        .core(
            0,
            CoreFact::Balance {
                policy: "ghost".into(),
                oldest_unpaid_due: None
            }
        )
        .events
        .is_empty());
}
