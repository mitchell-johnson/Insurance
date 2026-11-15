//! The collections queue, and the module as the cluster hosts it.

mod common;

use chrono::NaiveDate;
use common::*;
use ic_collections::machine::{Command, CoreFact, CORE_EVENTS};
use ic_collections::query::{answer, fact, outcome_command, react, OUTBOX};
use ic_collections::{Reason, Role};
use ic_core::{Event as CoreEvent, EventRecord, PolicyId, PolicyStatus, Standing};
use ic_raft::{CoreOutcome, Route};
use serde_json::json;

/// Purpose: COL-019. The day's queue lists policies in arrears ordered by
/// days to lapse and then by annual premium, an officer's answer states
/// what it withheld, and segments and open tasks are shown.
/// Why it matters: the notes start the officer's day with what is about to
/// be lost and matters most; an officer who sees the wrong order calls the
/// wrong customer first, and one who is not told fields were withheld may
/// think there is nothing more to know.
#[test]
fn the_queue_puts_what_is_about_to_lapse_and_matters_most_first() {
    let mut world = World::new();
    world.issue("old-small", "party-1", 5_000);
    world.issue("old-big", "party-2", 50_000);
    world.issue("new", "party-3", 90_000);
    world.issue("paid", "party-4", 90_000);
    world.overdue("old-small", d(2027, 2, 1));
    world.overdue("old-big", d(2027, 2, 1));
    world.overdue("new", d(2027, 2, 20));
    let as_at = d(2027, 3, 1);
    let queue = world.state.queue(as_at, Role::Collections, 10);
    let order: Vec<&str> = queue.items.iter().map(|i| i.policy.0.as_str()).collect();
    assert_eq!(order, vec!["old-big", "old-small", "new"]);
    assert_eq!(queue.items[0].days_in_arrears, 28);
    assert_eq!(queue.items[0].days_to_lapse, Some(32));
    assert_eq!(queue.items[0].segment, "retail");
    assert_eq!(queue.items[0].standing, Some(Standing::InGrace));
    assert!(queue.withheld.contains(&"health".to_string()));
    assert!(world.state.queue(as_at, Role::Hardship, 10).withheld.is_empty());
    assert_eq!(world.state.queue(as_at, Role::Collections, 1).items.len(), 1);

    // The answer over the wire is the same.
    let wire = answer(&world.state, json!({"q": "queue", "as_at": "2027-03-01", "role": "collections", "limit": 10})).unwrap();
    assert_eq!(wire["items"][0]["policy"], "old-big");

    // Segments: held, hardship, and a task shown on its line.
    world.close_through(d(2027, 3, 5));
    let task = world
        .state
        .tasks
        .values()
        .find(|t| t.policies.contains(&PolicyId::from("old-big")))
        .unwrap()
        .id
        .clone();
    world.officer(Command::RecordOutcome {
        task,
        reason: Reason::Disputed,
        note: "says it was paid".into(),
    });
    let small = world
        .state
        .tasks
        .values()
        .find(|t| t.policies.contains(&PolicyId::from("old-small")))
        .unwrap()
        .id
        .clone();
    world.officer(Command::RecordOutcome {
        task: small,
        reason: Reason::HardshipReferral,
        note: "lost job".into(),
    });
    let queue = world.state.queue(world.today, Role::Collections, 10);
    let seg = |p: &str| queue.items.iter().find(|i| i.policy.0 == p).unwrap().segment.clone();
    assert_eq!(
        (seg("old-big"), seg("old-small"), seg("new")),
        ("held".into(), "hardship".into(), "retail".into())
    );
    assert!(!queue.items.iter().find(|i| i.policy.0 == "old-big").unwrap().open_tasks.is_empty());

    // A policy with no sequence has no lapse day here and goes last.
    world.core(
        0,
        CoreFact::Issued {
            policy: "wol".into(),
            number: "N-wol".into(),
            party: "party-5".into(),
            product: "WOL0".into(),
        },
    );
    world.overdue("wol", d(2027, 1, 1));
    let queue = world.state.queue(world.today, Role::Collections, 10);
    assert_eq!(queue.items.last().unwrap().policy.0, "wol");
    assert_eq!(queue.items.last().unwrap().days_to_lapse, None);
}

fn record(event: CoreEvent) -> EventRecord {
    EventRecord {
        id: "3-17".into(),
        partition: 3,
        seq: 17,
        business_date: d(2027, 3, 4),
        log_index: 9,
        traceparent: None,
        event,
    }
}

/// Purpose: the core events the module follows become its commands under
/// the event's ID, on the event's partition and business date; the rest are
/// ignored; a closed day is the date the partition advanced from.
/// Why it matters: this is the only way collections hears about premiums,
/// standing and arrangements; a wrong mapping would chase the wrong people
/// or nobody, and a key that is not the event's would take one in twice.
#[test]
fn the_core_events_it_follows_become_its_commands() {
    let issued = record(CoreEvent::PolicyIssued {
        policy: "p1".into(),
        number: "N1".into(),
        group: "g1".into(),
        policyholder: "party-1".into(),
        product_code: PRODUCT.into(),
        product_hash: "h".into(),
        inception: d(2027, 1, 1),
        lives: vec![],
        layers: vec![],
        members: vec![],
    });
    let envelopes = react(&issued);
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].key, "core-3-17");
    assert_eq!(envelopes[0].caller, CORE_EVENTS);
    assert!(
        matches!(&envelopes[0].command, Command::Core { partition: 3, on, fact: CoreFact::Issued { party, .. } } if *on == d(2027, 3, 4) && party.0 == "party-1")
    );

    let instalment = ic_core::arrangement::Instalment {
        due: d(2027, 4, 1),
        amount_minor: 10,
    };
    let cases = [
        CoreEvent::PremiumDue {
            discounts: vec![],
            policy: "p1".into(),
            receivable: 1,
            period_start: d(2027, 1, 1),
            period_end: d(2027, 12, 31),
            total_minor: 120_000,
            members: None,
            by_cover_type: Default::default(),
        },
        CoreEvent::StandingChanged {
            policy: "p1".into(),
            from: None,
            to: Some(Standing::InGrace),
            oldest_unpaid_due: Some(d(2027, 3, 1)),
        },
        CoreEvent::PolicyStatusChanged {
            policy: "p1".into(),
            from: PolicyStatus::InForce,
            to: PolicyStatus::Lapsed,
        },
        CoreEvent::ArrangementMade {
            policy: "p1".into(),
            arrangement: "a1".into(),
            arrears_minor: 10,
            instalments: vec![instalment],
        },
        CoreEvent::ArrangementCompleted {
            policy: "p1".into(),
            arrangement: "a1".into(),
        },
        CoreEvent::ArrangementBroken {
            policy: "p1".into(),
            arrangement: "a1".into(),
            outstanding_minor: 5,
        },
        CoreEvent::PolicyBalance {
            policy: "p1".into(),
            outstanding_minor: 10,
            oldest_unpaid_due: Some(d(2027, 3, 1)),
        },
        CoreEvent::BusinessDateAdvanced {
            from: Some(d(2027, 3, 3)),
            to: d(2027, 3, 4),
            closing_head: ic_ledger::Hash32([0; 32]),
        },
    ];
    for event in cases {
        assert!(fact(&event).is_some(), "{event:?}");
    }
    let opened = CoreEvent::BusinessDateAdvanced {
        from: None,
        to: d(2027, 3, 4),
        closing_head: ic_ledger::Hash32([0; 32]),
    };
    assert!(react(&record(opened)).is_empty());
    let annual = fact(&CoreEvent::PremiumDue {
        discounts: vec![],
        policy: "p1".into(),
        receivable: 1,
        period_start: d(2027, 1, 1),
        period_end: d(2027, 12, 31),
        total_minor: 120_000,
        members: None,
        by_cover_type: Default::default(),
    });
    assert!(matches!(annual, Some(CoreFact::PremiumDue { total_minor: 120_000, .. })));
    assert_eq!(ic_collections::dunning::annualise(d(2027, 1, 1), d(2027, 1, 31), 31_000), 365_000);
    assert_eq!(ic_collections::dunning::annualise(d(2027, 1, 1), d(2026, 1, 1), 1), 365);
}

/// Purpose: the host reads the module's outbox, routes each posting to the
/// policy's partition under the module's own key and caller, and turns the
/// core's answer into the module's outcome command; the node hosts the
/// module under its name, reading events and posting to the core.
/// Why it matters: the outbox is how an arrangement or a relief reaches the
/// core exactly once; a wrong key would apply it twice, a wrong caller would
/// have it refused.
#[test]
fn the_outbox_carries_postings_to_the_core_and_back() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    world.officer(Command::ProposeArrangement {
        policy: "p1".into(),
        arrangement: "a1".into(),
        instalments: vec![ic_core::arrangement::Instalment {
            due: d(2027, 4, 1),
            amount_minor: 10_000,
        }],
    });
    let pending = answer(&world.state, (OUTBOX.pending)(10)).unwrap();
    let postings = (OUTBOX.read)(pending).unwrap();
    assert_eq!(postings.len(), 1);
    assert_eq!(postings[0].route, Route::Policy("p1".into()));
    assert_eq!(postings[0].envelope.key, format!("collections:{}", postings[0].id));
    assert_eq!(postings[0].envelope.caller, "collections");
    assert!((OUTBOX.read)(json!("nonsense")).is_err());

    let applied = CoreOutcome::Applied {
        partition: 0,
        log_index: 1,
        duplicate: false,
    };
    let refused = CoreOutcome::Refused(ic_core::Reject::new("invalid_arrangement", "no"));
    let unroutable = CoreOutcome::Unroutable("no partition".into());
    for (outcome, ok) in [(&applied, true), (&refused, false), (&unroutable, false)] {
        let command = outcome_command(&postings[0], outcome);
        assert!(matches!(command.command, Command::RecordCoreOutcome { applied, .. } if applied == ok));
        assert!((OUTBOX.outcome)(&postings[0], outcome).is_object());
    }

    let spec = ic_collections::module_spec().unwrap();
    assert_eq!(spec.name, "collections");
    assert!(spec.events && spec.outbox.is_some());
    let mut hosted = (spec.make)();
    assert!(hosted.query(json!({"q": "unsequenced"})).is_ok());
    let saved = hosted.save().unwrap();
    hosted.load(saved).unwrap();
}

/// Purpose: every question the module answers is answered from its state,
/// and anything else is refused.
/// Why it matters: the portal's collections screens and the documents
/// service read these answers; a question that silently answered nothing
/// would look like an empty queue.
#[test]
fn every_question_is_answered_and_others_refused() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    world.close_through(d(2027, 3, 18));
    let state = &world.state;
    for question in [
        json!({"q": "policy", "policy": "p1"}),
        json!({"q": "notices_due", "limit": 5}),
        json!({"q": "tasks", "role": "collections", "limit": 5}),
        json!({"q": "hardship_case", "case": "none", "role": "hardship"}),
        json!({"q": "outbox", "limit": 5}),
        json!({"q": "unsequenced"}),
        json!({"q": "state_hash"}),
    ] {
        assert!(answer(state, question.clone()).is_ok(), "{question}");
    }
    assert_eq!(answer(state, json!({"q": "policy", "policy": "p1"})).unwrap()["number"], "N-p1");
    assert_eq!(
        answer(state, json!({"q": "tasks", "role": "collections", "limit": 5}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(answer(state, json!({"q": "everything"})).is_err());
    // the sequence in force for each product, as the configuration screen shows it
    let sequences = answer(state, json!({"q": "sequences"})).unwrap();
    assert_eq!(sequences.as_object().unwrap().len(), state.sequences.len());
    for (product, sequence) in &state.sequences {
        assert_eq!(sequences[product]["lapse_day"], sequence.lapse_day);
    }
}

/// Purpose: a command's key is answered once while it is remembered, and is
/// forgotten after the retention period; a day never runs backwards whatever
/// a proposer's clock says.
/// Why it matters: callers retry after a timeout; the second answer must be
/// the first. Keys kept for ever would grow the state without end.
#[test]
fn keys_are_answered_once_and_forgotten_in_time() {
    let mut world = World::new();
    let command = || Command::SetSequence { sequence: sequence() };
    let first = world.send_keyed("once", "staff", "config-1", &[Role::Configuration], command());
    let again = world.send_keyed("once", "staff", "config-1", &[Role::Configuration], command());
    assert!(!first.duplicate && again.duplicate);
    assert_eq!(again.index, first.index);
    world.today = d(2027, 5, 1);
    world.send_keyed("later", "staff", "config-1", &[Role::Configuration], command());
    let after = world.send_keyed("once", "staff", "config-1", &[Role::Configuration], command());
    assert!(!after.duplicate);
    world.today = d(2027, 1, 1);
    world.send_keyed("earlier", "staff", "config-1", &[Role::Configuration], command());
    assert_eq!(world.state.today, Some(NaiveDate::from_ymd_opt(2027, 5, 1).unwrap()));
}

/// Purpose: every command has its own snake-case name, which is what a
/// refusal names when the wrong caller or role sends it.
/// Why it matters: refusals are read by people in logs and screens; two
/// commands sharing a name, or a name that is not the one on the wire,
/// would send them looking in the wrong place.
#[test]
fn every_command_names_itself() {
    let d0 = d(2027, 3, 1);
    let commands = vec![
        Command::Core {
            partition: 0,
            on: d0,
            fact: CoreFact::DayClosed { closed: d0 },
        },
        Command::SetSequence { sequence: sequence() },
        Command::RecordOutcome {
            task: "t".into(),
            reason: Reason::Paid,
            note: "n".into(),
        },
        Command::ProposeArrangement {
            policy: "p".into(),
            arrangement: "a".into(),
            instalments: vec![],
        },
        Command::EndArrangement { policy: "p".into() },
        Command::ApplyHardship {
            case: "h".into(),
            party: "x".into(),
            policies: vec![],
            relief: ic_collections::Relief::Holiday,
            from: d0,
            to: d0,
            discloses_health: false,
            note: "n".into(),
        },
        Command::DecideHardship {
            case: "h".into(),
            approve: true,
            note: "n".into(),
        },
        Command::EndHardship { case: "h".into() },
        Command::RecordNoticeSent { notice: "n".into() },
        Command::RecordNoticeDelivery {
            notice: "n".into(),
            delivered: true,
        },
        Command::RecordCoreOutcome {
            posting: "p".into(),
            applied: true,
            code: None,
            message: None,
        },
    ];
    let mut names = std::collections::BTreeSet::new();
    let mut world = World::new();
    for command in commands {
        let name = command.name();
        let wire = serde_json::to_value(&command).unwrap();
        assert_eq!(wire["type"], name);
        assert!(names.insert(name));
        let refused = world.send("nobody", "x", &[], command);
        assert!(refused.result.unwrap_err().message.contains(name));
    }
}

/// Purpose: a broken arrangement the module never saw made still opens a
/// task and starts no pause; a hardship application for a policy whose
/// owner has died leaves the death hold in place.
/// Why it matters: events can be first seen after a restart from an older
/// snapshot or for a policy migrated in mid-arrangement; neither must make
/// the module invent a pause or start writing to the estate.
#[test]
fn odd_orders_of_events_are_handled() {
    let mut world = World::new();
    world.issue("p1", "party-1", 10_000);
    world.overdue("p1", d(2027, 3, 1));
    world.core(
        0,
        CoreFact::ArrangementBroken {
            policy: "p1".into(),
            arrangement: "a0".into(),
            outstanding_minor: 1,
        },
    );
    assert!(world.state.policies[&PolicyId::from("p1")].pause.is_none());
    world.close_through(d(2027, 3, 18));
    let task = world
        .state
        .tasks
        .values()
        .find(|t| t.kind == ic_collections::model::TaskKind::Call)
        .unwrap()
        .id
        .clone();
    world.officer(Command::RecordOutcome {
        task,
        reason: Reason::DeceasedNotified,
        note: "estate called".into(),
    });
    let applied = world.officer(Command::ApplyHardship {
        case: "h1".into(),
        party: "party-1".into(),
        policies: vec!["p1".into()],
        relief: ic_collections::Relief::Holiday,
        from: world.today,
        to: d(2027, 6, 1),
        discloses_health: false,
        note: "estate asks for time".into(),
    });
    assert_eq!(applied.result, Ok(()));
    assert_eq!(world.state.policies[&PolicyId::from("p1")].hold, Some(ic_collections::model::Hold::Deceased));
}
