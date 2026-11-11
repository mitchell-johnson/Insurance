//! The documents module as the cluster hosts it: commands and questions as
//! JSON, core events fed one at a time, and the state saved and restored.

mod support;

use chrono::NaiveDate;
use ic_core::{Event as CoreEvent, EventRecord, Standing};
use ic_documents::command::{Command, Envelope};
use ic_documents::model::Channel;
use ic_documents::module::{self, Query, MAX_ANSWER, NAME};
use ic_documents::{Applied, DocumentKind, Gate};
use ic_raft::ModuleState;
use serde_json::{json, Value};
use support::*;

fn core(seq: u64, on: NaiveDate, event: CoreEvent) -> EventRecord {
    EventRecord {
        id: format!("7-{seq}"),
        partition: 7,
        seq,
        business_date: on,
        log_index: seq,
        traceparent: None,
        event,
    }
}

fn grace(seq: u64, on: NaiveDate) -> EventRecord {
    core(
        seq,
        on,
        CoreEvent::StandingChanged {
            policy: policy("pol-1"),
            from: Some(Standing::Paid),
            to: Some(Standing::InGrace),
            oldest_unpaid_due: None,
        },
    )
}

/// A hosted module brought to the state of a prepared kit by replaying
/// the kit's log through it, as a member catching up would.
fn hosted_from(kit: &Kit) -> Box<dyn ModuleState> {
    let spec = module::module_spec().unwrap();
    let mut hosted = (spec.make)();
    for (i, envelope) in kit.log.iter().enumerate() {
        let reply = hosted.apply(i as u64 + 1, serde_json::to_value(envelope).unwrap()).unwrap();
        let applied: Applied = serde_json::from_value(reply).unwrap();
        assert!(applied.result.is_ok());
    }
    hosted
}

fn ask(hosted: &dyn ModuleState, query: Query) -> Value {
    hosted.query(serde_json::to_value(query).unwrap()).unwrap()
}

/// Purpose: the module declares itself to the host under its permanent
/// name and as a reader of the core's events.
/// Why it matters: the name is the directory on every node and the route
/// every driver calls; a module not declared as a reader would never be fed
/// an event and no notice would ever be triggered.
#[test]
fn the_module_is_declared_as_an_event_reader_named_documents() {
    let spec = module::module_spec().unwrap();
    assert_eq!(spec.name, NAME);
    assert_eq!(spec.name, "documents");
    assert!(spec.events);
}

/// Purpose: a command sent as JSON is applied, a repeat of it returns the
/// original result, and JSON that is no command of the module is refused
/// without changing anything.
/// Why it matters: delivery to a module is at least once and the host
/// knows nothing about the command type; this is the whole contract
/// between a driver and the replicated state.
#[test]
fn commands_travel_as_json_and_repeat_safely() {
    let mut kit = Kit::ready();
    let mut hosted = hosted_from(&kit);
    let before = ask(hosted.as_ref(), Query::StateHash);

    let refused = hosted.apply(900, json!({"not": "a command"})).unwrap_err();
    assert!(refused.contains("not a command of this module"));
    assert_eq!(ask(hosted.as_ref(), Query::StateHash), before);

    let spec = kit.renewal_spec("n1");
    kit.issue(spec);
    let envelope = serde_json::to_value(kit.log.last().unwrap()).unwrap();
    let index = kit.log.len() as u64;
    let first: Applied = serde_json::from_value(hosted.apply(index, envelope.clone()).unwrap()).unwrap();
    assert!(first.result.is_ok() && !first.duplicate && !first.events.is_empty());
    let again: Applied = serde_json::from_value(hosted.apply(902, envelope).unwrap()).unwrap();
    assert!(again.duplicate && again.events.is_empty());
    assert_eq!(again.index, index);
    assert_eq!(ask(hosted.as_ref(), Query::StateHash), json!(kit.state.state_hash()));
}

/// Purpose: every question the drivers and other services ask is answered
/// from the state, and a question the module does not know is refused.
/// Why it matters: the driver renders, sends, deletes and reports only from
/// these answers; a question that silently answered wrongly would send the
/// wrong document or miss one.
#[test]
fn every_question_is_answered_from_the_state() {
    let mut kit = Kit::ready();
    let today = kit.today();
    let spec = kit.renewal_spec("n1");
    kit.issue(spec);
    let mut schedule = kit.issue_spec("s1", DocumentKind::PolicySchedule, "schedule");
    schedule.wordings = vec![tref("wording", 1)];
    kit.issue(schedule);
    kit.ok(
        "remediation",
        "lead",
        Command::RaiseNoticeDefect {
            defect: "d1".into(),
            case: "rem-1".into(),
            description: "wrong premium".into(),
            cause: ic_documents::model::DefectCause::WrongRate,
            from: today,
            to: today,
            grace_days: None,
        },
    );
    kit.ok(
        "relay",
        "svc",
        Command::ObserveCoreEvents {
            partition: 7,
            records: vec![grace(1, today)],
        },
    );
    let hosted = hosted_from(&kit);
    let h = hosted.as_ref();

    assert_eq!(ask(h, Query::Document { id: "n1".into() })["kind"], json!("renewal_notice"));
    assert_eq!(ask(h, Query::Document { id: "none".into() }), Value::Null);
    assert_eq!(ask(h, Query::Inbox { party: party("aroha") }).as_array().unwrap().len(), 2);
    assert_eq!(ask(h, Query::PolicyDocuments { policy: policy("pol-1") }).as_array().unwrap().len(), 2);
    assert_eq!(ask(h, Query::ScheduleHistory { policy: policy("pol-1") }).as_array().unwrap().len(), 1);
    let gate: Gate = serde_json::from_value(ask(
        h,
        Query::NoticeGate {
            policy: policy("pol-1"),
            purpose: "renewal".into(),
            on: today,
        },
    ))
    .unwrap();
    assert_eq!(gate, kit.state.notice_gate(&policy("pol-1"), "renewal", today));
    assert!(matches!(gate, Gate::Waiting(_)));
    assert_eq!(
        ask(
            h,
            Query::TemplateFor {
                id: "renewal".into(),
                on: today
            }
        )["template"]["version"],
        json!(1)
    );
    assert_eq!(ask(h, Query::Template { template: tref("renewal", 1) })["template"]["id"], json!("renewal"));
    assert_eq!(ask(h, Query::Parts { template: tref("renewal", 1) }), json!([]));
    assert_eq!(
        hosted
            .query(serde_json::to_value(Query::Parts { template: tref("nothing", 1) }).unwrap())
            .unwrap_err(),
        "no such template"
    );
    assert_eq!(ask(h, Query::PartyKey { party: party("aroha") }), json!("wrapped-aroha"));
    let sends = ask(h, Query::PendingSends { limit: 10 });
    assert_eq!(sends.as_array().unwrap().len(), 2);
    assert_eq!(sends[0]["channel"], json!("email"));
    assert_eq!(ask(h, Query::PendingSends { limit: 1 }).as_array().unwrap().len(), 1);
    assert!(!ask(h, Query::PendingEvents { limit: usize::MAX }).as_array().unwrap().is_empty());
    assert!(ask(h, Query::PendingEvents { limit: usize::MAX }).as_array().unwrap().len() <= MAX_ANSWER);
    assert_eq!(ask(h, Query::CoreOutbox { limit: 10 }), json!([]));
    assert_eq!(ask(h, Query::PendingDeletes { limit: 10 }), json!([]));
    let triggers = ask(h, Query::Triggers { limit: 10 });
    assert_eq!(triggers[0]["kind"], json!("lapse_notice"));
    assert_eq!(ask(h, Query::Defect { id: "d1".into() })["case"], json!("rem-1"));
    assert_eq!(
        ask(
            h,
            Query::NoticesByTemplate {
                template: tref("renewal", 1),
                from: today,
                to: today
            }
        ),
        json!(["n1"])
    );
    assert_eq!(ask(h, Query::Report { from: today, to: today })["issued_by_kind"]["renewal_notice"], json!(1));
    assert_eq!(ask(h, Query::Settings)["post_deemed_days"], json!(kit.state.settings.post_deemed_days));
    assert_eq!(ask(h, Query::StateHash), json!(kit.state.state_hash()));

    assert!(hosted
        .query(json!({"tell_me": "everything"}))
        .unwrap_err()
        .contains("not a question of this module"));
}

/// Purpose: the host shows the module each core event once; each becomes a
/// trigger for the document it requires, and the module's own time is
/// taken from the event's business date without running backwards.
/// Why it matters: a notice is rendered only because of the event it
/// reports. An event lost between host and module is a notice never sent,
/// and one taken twice is a customer told twice that their policy is
/// lapsing.
#[test]
fn fed_events_become_triggers_once() {
    let kit = Kit::ready();
    let today = kit.today();
    let mut hosted = hosted_from(&kit);
    let event = grace(1, today);

    let commands = module::react(&event);
    assert_eq!(commands.len(), 1);
    let Envelope { key, caller, at, .. } = &commands[0];
    assert_eq!((key.as_str(), caller.as_str()), ("core:7-1", "relay"));
    assert_eq!(ic_documents::state::nz_date(*at), today);
    assert!(*at <= kit.now, "an event may never move the module's time ahead of the real time");

    hosted.on_event(500, &event);
    hosted.on_event(501, &event);
    let triggers = ask(hosted.as_ref(), Query::Triggers { limit: 10 });
    assert_eq!(triggers.as_array().unwrap().len(), 1);
    assert_eq!(triggers[0]["id"], json!("7-1:LapseNotice"));
    assert_eq!(triggers[0]["purpose"], json!("lapse"));
    // Time did not go back to the start of the business date.
    let saved = hosted.save().unwrap();
    assert_eq!(saved["now"], json!(kit.state.now));
}

/// Purpose: a module added to a cluster that is already running starts at
/// the oldest event a partition still holds, and from there insists on the
/// partition's sequence.
/// Why it matters: the host can only offer what the partition kept. A
/// module that demanded event one would refuse everything for ever; one
/// that then accepted a gap would silently miss notices.
#[test]
fn a_late_started_module_begins_at_the_first_event_it_is_shown() {
    let mut kit = Kit::ready();
    let today = kit.today();
    kit.ok("relay", "svc", Command::ObserveCoreEvents { partition: 7, records: vec![] });
    assert!(kit.state.checkpoints.is_empty());
    kit.ok(
        "relay",
        "svc",
        Command::ObserveCoreEvents {
            partition: 7,
            records: vec![grace(41, today)],
        },
    );
    assert_eq!(kit.state.checkpoints[&7], 41);
    assert_eq!(
        kit.err(
            "relay",
            "svc",
            Command::ObserveCoreEvents {
                partition: 7,
                records: vec![grace(43, today)]
            }
        ),
        "gap"
    );
    kit.ok(
        "relay",
        "svc",
        Command::ObserveCoreEvents {
            partition: 7,
            records: vec![grace(42, today)],
        },
    );
    assert_eq!(kit.state.triggers.len(), 2);
}

/// Purpose: the state saved for a snapshot restores to a module that
/// answers the same and hashes the same, and core events leave no
/// idempotency keys behind.
/// Why it matters: a member that fell behind is caught up from a snapshot.
/// If the restored state differed in any way the members' state hashes
/// would diverge; and a key kept for every core event would grow the
/// snapshot without bound.
#[test]
fn a_saved_state_restores_to_the_same_module() {
    let mut kit = Kit::ready();
    let today = kit.today();
    let spec = kit.renewal_spec("n1");
    kit.issue(spec);
    kit.deliver("n1", "aroha", Channel::Email);
    let mut hosted = hosted_from(&kit);
    hosted.on_event(700, &grace(1, today));

    let saved = hosted.save().unwrap();
    assert!(saved["results"].as_object().unwrap().keys().all(|k| !k.starts_with("core:")));
    let mut restored = (module::module_spec().unwrap().make)();
    restored.load(saved.clone()).unwrap();
    assert_eq!(restored.save().unwrap(), saved);
    assert_eq!(ask(restored.as_ref(), Query::StateHash), ask(hosted.as_ref(), Query::StateHash));
    assert_eq!(ask(restored.as_ref(), Query::CoreOutbox { limit: 10 }).as_array().unwrap().len(), 1);
    assert!(restored.load(json!({"settings": 5})).is_err());
}
