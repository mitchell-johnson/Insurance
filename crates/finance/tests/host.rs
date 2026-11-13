//! The finance ledger as a module of the deployed node.

mod common;

use common::*;
use ic_core::event::EventRecord;
use ic_finance::books::FinEvent;
use ic_finance::host::{module_spec, react, MODULE};
use ic_finance::{FinCommand, FinEnvelope};
use serde_json::{json, Value};

/// Ten days of one partition with one paying policy.
fn ten_days() -> Vec<EventRecord> {
    let mut w = World::new(1, d(2027, 11, 15));
    let (p, id) = w.issue(1, d(2027, 11, 15));
    w.pay(p, &id, "r1", MONTHLY);
    for _ in 0..10 {
        w.core.end_of_day().unwrap();
    }
    w.core.take_events()
}

/// Deliver events the way the host does: one command each, results ignored.
fn deliver(b: &mut Books, events: &[EventRecord]) {
    for record in events {
        for env in react(record) {
            assert_eq!(b.apply(env).result, Ok(()), "the host cannot act on a refusal, so there must be none");
        }
    }
}

fn feed_events(b: &Books) -> Vec<String> {
    b.applied
        .iter()
        .flat_map(|a| &a.events)
        .filter_map(|e| match &e.event {
            FinEvent::FeedStalled { code, .. } => Some(format!("stalled:{code}")),
            FinEvent::FeedResumed { .. } => Some("resumed".to_string()),
            _ => None,
        })
        .collect()
}

/// Purpose: events the node delivers before the ledger is configured, or before a chart is
/// active, are held in order and not refused. The stall is announced once. When the chart is
/// approved the held events are taken in and the books are exactly what a ledger fed from
/// the start would hold. An event delivered twice is taken once, and an event out of
/// sequence is refused.
/// Why it matters: the node shows each event to the module once and moves on. If the ledger
/// refused an event because finance had not yet approved a chart, that event would be gone
/// and every later one would be a gap. The books would be permanently short with nothing
/// to recover from.
#[test]
fn events_that_arrive_before_the_books_can_take_them_are_held_not_lost() {
    let events = ten_days();
    let (early, later) = events.split_at(events.len() / 2);
    let mut b = Books::empty();
    deliver(&mut b, early);
    assert_eq!(b.state.held_events(0), early.len());
    assert_eq!(b.state.stalled[&0].code, "chart");
    assert_eq!(feed_events(&b), vec!["stalled:chart"], "said once, not once for each event");

    b.send(
        "operator",
        None,
        FinCommand::Configure {
            partitions: 1,
            year_end_month: 3,
        },
    )
    .unwrap();
    assert_eq!(b.state.held_events(0), early.len(), "still no chart");
    deliver(&mut b, &early[early.len() - 3..]);
    assert_eq!(b.state.held_events(0), early.len(), "a repeat of a held event is not held twice");
    let skipped = react(&later[2]).remove(0);
    assert_eq!(b.apply(skipped).result.unwrap_err().code, "gap");

    b.activate_chart(chart());
    assert_eq!((b.state.held_events(0), b.state.stalled.len()), (0, 0));
    assert_eq!(feed_events(&b), vec!["stalled:chart", "resumed"]);
    deliver(&mut b, later);
    deliver(&mut b, &events[..10]);
    assert_eq!(b.state.held_events(0), 0);

    let mut direct = Books::ready(1);
    direct.feed(&events).unwrap();
    let nov = period(2027, 11);
    assert_eq!(b.state.trial_balance(nov, None), direct.state.trial_balance(nov, None));
    assert_eq!(b.state.sources(nov), direct.state.sources(nov));
    assert_eq!(b.state.ledger.head(), direct.state.ledger.head(), "the same journals, byte for byte");
}

/// Purpose: when the core posts to an account role the active chart does not place, the feed
/// from that partition stalls with the reason, later events queue behind it, a retry while
/// the cause remains changes nothing, and approving a chart version that maps the role
/// resumes the feed with nothing missing.
/// Why it matters: a new role means the core has started doing something finance has not
/// yet decided how to account for. The right response is to stop and wait for that decision,
/// visibly, and then carry on without anyone having to replay or repair anything.
#[test]
fn a_role_the_chart_does_not_place_stalls_the_feed_until_a_chart_does() {
    let events = ten_days();
    let mut without = chart();
    for account in &mut without.accounts {
        account.roles.retain(|r| r != "earned_premium");
    }
    without.content_hash = "no-earned-premium".to_string();
    let mut b = Books::empty();
    b.send(
        "operator",
        None,
        FinCommand::Configure {
            partitions: 1,
            year_end_month: 3,
        },
    )
    .unwrap();
    b.activate_chart(without);
    b.activate_close(close_config());
    deliver(&mut b, &events);
    assert_eq!(b.state.stalled[&0].code, "unmapped_role");
    assert!(
        b.state.held_events(0) > 0 && b.state.next_event_seq(0) > 1,
        "everything before the first earning was taken"
    );
    let held = b.state.held_events(0);

    assert_eq!(b.code("feed", None, FinCommand::RetryFeed { partition: 0 }), "caller");
    b.send("operator", None, FinCommand::RetryFeed { partition: 0 }).unwrap();
    assert_eq!(b.state.held_events(0), held);
    assert_eq!(feed_events(&b), vec!["stalled:unmapped_role"]);

    let mut fixed = chart();
    fixed.version = 2;
    b.activate_chart(fixed);
    assert_eq!((b.state.held_events(0), b.state.stalled.len()), (0, 0));
    assert_eq!(feed_events(&b), vec!["stalled:unmapped_role", "resumed"]);
    assert_eq!(b.state.next_event_seq(0), events.last().unwrap().seq + 1);
    assert!(b.state.day(d(2027, 11, 24), 0).unwrap().differences.is_empty());
    b.send("operator", None, FinCommand::RetryFeed { partition: 0 }).unwrap();
    assert_eq!(feed_events(&b).len(), 2, "a retry with nothing held says nothing");
}

/// Purpose: the module the node hosts is named `finance`, reads the core's events, takes
/// commands and events as JSON, saves and loads its whole state, and answers each question:
/// status, dashboard, checks, trial balance, movements, sources, backdated items, a journal,
/// conduct and a filed return. An unknown question or a missing journal or return is an
/// error, not an empty answer.
/// Why it matters: this is the surface the finance console, the close workflow and the other
/// modules' drivers use in production. A question that silently returned nothing, or a
/// snapshot that did not restore the books exactly, would be found at a month end.
#[test]
fn the_hosted_module_takes_events_and_answers_questions() {
    let spec = module_spec().unwrap();
    assert_eq!((spec.name.as_str(), spec.events, MODULE), ("finance", true, "finance"));
    let mut module = (spec.make)();
    let mut index = 0;
    let mut send = |module: &mut Box<dyn ic_raft::ModuleState>, caller: &str, actor: Option<&str>, command: FinCommand| -> Value {
        index += 1;
        let env = FinEnvelope {
            key: format!("k{index}"),
            caller: caller.to_string(),
            actor: actor.map(str::to_string),
            roles: actor.map(roles).unwrap_or_default(),
            command,
        };
        let reply = module.apply(index, serde_json::to_value(env).unwrap()).unwrap();
        assert_eq!(reply["result"], json!({ "Ok": null }), "{reply}");
        reply
    };
    send(
        &mut module,
        "operator",
        None,
        FinCommand::Configure {
            partitions: 1,
            year_end_month: 3,
        },
    );
    let (c, k) = (chart(), close_config());
    let (chart_id, close_id) = (
        format!("chart:{}:{}", c.version, c.content_hash),
        format!("close:{}:{}", k.version, k.content_hash),
    );
    send(&mut module, "finance-console", Some("olive"), FinCommand::ProposeChart { chart: c });
    send(&mut module, "finance-console", Some("lena"), FinCommand::ApproveConfig { id: chart_id });
    send(&mut module, "finance-console", Some("olive"), FinCommand::ProposeCloseConfig { config: k });
    send(&mut module, "finance-console", Some("lena"), FinCommand::ApproveConfig { id: close_id });
    let events = ten_days();
    for (i, record) in events.iter().enumerate() {
        module.on_event(100 + i as u64, record);
    }
    let accrual = journal("j1", ic_finance::books::JournalKind::Accrual, period(2027, 11), "6500", "2500", 25_00);
    send(&mut module, "finance-console", Some("olive"), FinCommand::ProposeJournal { journal: accrual });
    assert!(module
        .apply(999, json!({"not": "an envelope"}))
        .unwrap_err()
        .contains("not a command of this module"));

    let ask = |module: &dyn ic_raft::ModuleState, question: Value| module.query(question);
    let status = ask(module.as_ref(), json!({"q": "status"})).unwrap();
    assert_eq!(
        (status["configured"].clone(), status["chart"].clone(), status["last_date"].clone()),
        (json!(true), json!(1), json!("2027-11-24"))
    );
    assert_eq!(
        status["partitions"][0],
        json!({"partition": 0, "next_event": events.last().unwrap().seq + 1, "held": 0, "stalled": null})
    );
    assert_eq!(status["state_hash"].as_str().unwrap().len(), 64);

    let board = ask(module.as_ref(), json!({"q": "dashboard", "period": "2027-11", "today": "2027-11-25"})).unwrap();
    assert_eq!(
        (board["control"]["checked"].clone(), board["journals"]["proposed"].clone()),
        (json!(10), json!(1))
    );
    let checks = ask(module.as_ref(), json!({"q": "checks", "period": "2027-11"})).unwrap();
    assert_eq!(checks.as_array().unwrap().len(), 21);
    let all = ask(module.as_ref(), json!({"q": "trial_balance", "period": "2027-11"})).unwrap();
    let life = ask(module.as_ref(), json!({"q": "trial_balance", "period": "2027-11", "fund": "SF1-life"})).unwrap();
    assert_eq!(all, life, "one policy, one fund");
    assert!(ask(module.as_ref(), json!({"q": "trial_balance", "period": "2027-11", "fund": "none"}))
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!ask(module.as_ref(), json!({"q": "movements", "period": "2027-11"}))
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        ask(module.as_ref(), json!({"q": "sources", "period": "2027-11"}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(ask(module.as_ref(), json!({"q": "backdated", "period": "2027-11"})).unwrap(), json!([]));
    assert_eq!(ask(module.as_ref(), json!({"q": "journal", "id": "j1"})).unwrap()["maker"], json!("olive"));
    assert_eq!(
        ask(module.as_ref(), json!({"q": "conduct", "period": "2027-11"})).unwrap()["declined"],
        json!(0)
    );

    assert_eq!(ask(module.as_ref(), json!({"q": "journal", "id": "j9"})).unwrap_err(), "there is no journal j9");
    assert_eq!(
        ask(module.as_ref(), json!({"q": "return", "id": "q1"})).unwrap_err(),
        "return q1 has not been filed"
    );
    assert!(ask(module.as_ref(), json!({"q": "weather"}))
        .unwrap_err()
        .starts_with("not a question the finance ledger answers"));
    assert!(ask(module.as_ref(), json!({"q": "checks", "period": "November"})).is_err());

    // A snapshot restores a new member to exactly the same books.
    let saved = module.save().unwrap();
    let mut member = (spec.make)();
    member.load(saved.clone()).unwrap();
    assert_eq!(member.save().unwrap(), saved);
    assert_eq!(ask(member.as_ref(), json!({"q": "status"})).unwrap(), status);
}
