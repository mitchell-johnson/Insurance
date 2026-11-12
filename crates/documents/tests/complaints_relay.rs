//! The complaints relay: the documents service keeping time for the
//! complaints module and carrying its events to the stream.

#[path = "support/azure.rs"]
mod azure;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use ic_complaints::command::Intake;
use ic_complaints::model::{Category, ComplainantKind, ConductKind, Contact, IntakeChannel, Level, Links, Process, Timer};
use ic_complaints::module::Query;
use ic_complaints::{Command, Envelope, Event, EventRecord, Timers};
use ic_core::{PartyId, PolicyId};
use ic_documents::complaints_relay::{
    run_relay, ClusterComplaints, ClusterLicences, Complaints, ComplaintsStream, LicenceCase, LicenceFeed, LocalComplaints, MemoryLicences, MemoryStream,
    NoLicences, Relay, RelayCycle,
};
use ic_documents::driver::{DriverError, EventHubsPublisher};
use ic_raft::ClusterClient;
use serde_json::{json, Value};

type Clock = Arc<Mutex<DateTime<Utc>>>;

fn timers() -> Timers {
    Timers {
        version: 1,
        acknowledge_days: 5,
        update_every_days: 10,
        decide_days: 20,
        deadlock_days: 40,
        warning_days: 2,
        conduct_officer_after_days: 3,
        complainant_response_days: 10,
        vulnerable_extra_days: 10,
        front_line_limit_minor: 10_000,
        ex_gratia_limits_minor: BTreeMap::from([(1, 50_000)]),
        regulator_notification_days: 10,
        retain_closed_days: 100,
        cause_codes: BTreeSet::from(["delay".to_string()]),
    }
}

fn intake(id: &str) -> Command {
    Command::RecordComplaint(Box::new(Intake {
        complaint: id.into(),
        channel: IntakeChannel::Call,
        complainant: PartyId::from("aroha"),
        complainant_kind: ComplainantKind::Policyholder,
        links: Links::default(),
        category: Category {
            product: None,
            process: Process::Service,
            cause: None,
        },
        summary_document: "doc-summary".into(),
        vulnerable: false,
        adjustments: vec![],
        resolved_on_the_spot: None,
        received_on: None,
        acknowledged_on: None,
    }))
}

/// A complaints module with timers active and one complaint recorded, and
/// a relay over it whose clock the test moves.
async fn world() -> (Relay<LocalComplaints>, Clock) {
    let clock: Clock = Arc::new(Mutex::new(Utc.with_ymd_and_hms(2027, 11, 14, 23, 0, 0).unwrap()));
    let module = LocalComplaints::default();
    let at = *clock.lock().unwrap();
    for (key, caller, command) in [("t1", "config", Command::ActivateTimers(timers())), ("c1", "intake", intake("c1"))] {
        let applied = module
            .propose(Envelope {
                key: key.into(),
                caller: caller.into(),
                actor: "setup".into(),
                at,
                command,
            })
            .await
            .unwrap();
        assert!(applied.result.is_ok());
    }
    let reads = clock.clone();
    let relay = Relay {
        module,
        actor: "documents-service".into(),
        conduct_officer: "conduct-officer".into(),
        clock: Arc::new(move || *reads.lock().unwrap()),
        batch: 100,
    };
    (relay, clock)
}

fn pending(relay: &Relay<LocalComplaints>) -> Vec<EventRecord> {
    relay.module.read(|s| s.pending_events(1000))
}

/// Purpose: one pass of the relay lets the complaints module's time pass
/// and puts its events on the stream in order, acknowledging them only
/// once the stream has them; a pass with nothing new does nothing.
/// Why it matters: without this nothing in a deployed cluster ever tells
/// the module a deadline is near or missed, and nothing carries
/// `AcknowledgementDue` to the service that writes the letter. The
/// regulatory timers would exist only on paper.
#[tokio::test]
async fn a_pass_lets_time_pass_and_publishes_in_order() {
    let (relay, clock) = world().await;
    let stream = MemoryStream::default();
    assert_eq!(pending(&relay).len(), 3);

    assert_eq!(
        relay.cycle(&stream, &NoLicences).await,
        RelayCycle {
            raised: 0,
            published: 3,
            cases: 0,
            errors: vec![]
        }
    );
    assert_eq!(stream.seen().iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["cmp-1", "cmp-2", "cmp-3"]);
    assert!(pending(&relay).is_empty());
    assert_eq!(relay.cycle(&stream, &NoLicences).await, RelayCycle::default());

    // Three days on the acknowledgement is two days from due.
    *clock.lock().unwrap() += chrono::Duration::days(3);
    let cycle = relay.cycle(&stream, &NoLicences).await;
    assert_eq!((cycle.raised, cycle.published), (1, 1));
    assert!(matches!(
        &stream.seen()[3].event,
        Event::TimerEscalated {
            timer: Timer::Acknowledge,
            level: Level::Warning,
            ..
        }
    ));
    // A second pass in the same second raises nothing twice.
    assert_eq!(relay.cycle(&stream, &NoLicences).await, RelayCycle::default());
    assert_eq!(stream.seen().len(), 4);
}

/// Purpose: a pass publishes at most its batch, and the next carries on.
/// Why it matters: after an outage the backlog can be large. One request
/// carrying all of it would be refused by Event Hubs for size, and the
/// backlog would then never clear.
#[tokio::test]
async fn a_backlog_is_published_a_batch_at_a_time() {
    let (mut relay, _) = world().await;
    relay.batch = 2;
    let stream = MemoryStream::default();
    assert_eq!(relay.publish(&stream).await.unwrap(), 2);
    assert_eq!(pending(&relay)[0].seq, 3);
    assert_eq!(relay.publish(&stream).await.unwrap(), 1);
    assert_eq!(relay.publish(&stream).await.unwrap(), 0);
    assert_eq!(stream.seen().iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
}

/// Purpose: when the stream, the module or the command fails, the pass
/// says which step failed, the other step still runs, and nothing is
/// acknowledged that was not published.
/// Why it matters: an event acknowledged but never published is an
/// escalation nobody hears. And a stream outage must not stop time
/// passing in the module, or deadlines would be missed silently for as
/// long as Event Hubs was down.
#[tokio::test]
async fn a_failed_step_is_reported_and_loses_nothing() {
    let (relay, clock) = world().await;
    let stream = MemoryStream::default();

    stream.set_down(true);
    *clock.lock().unwrap() += chrono::Duration::days(3);
    let cycle = relay.cycle(&stream, &NoLicences).await;
    assert_eq!(cycle.raised, 1, "time still passed");
    assert_eq!(
        cycle.errors,
        vec!["publish: the documents module is unavailable: the stream is unavailable".to_string()]
    );
    assert_eq!(pending(&relay).len(), 4);
    assert!(stream.seen().is_empty());

    stream.set_down(false);
    relay.module.set_down(true);
    let cycle = relay.cycle(&stream, &NoLicences).await;
    assert_eq!(cycle.errors.len(), 2);
    assert!(cycle.errors[0].starts_with("tick: ") && cycle.errors[1].starts_with("publish: "));
    assert!(matches!(relay.tick().await, Err(DriverError::Unavailable(_))));
    assert!(stream.seen().is_empty());

    relay.module.set_down(false);
    assert_eq!(relay.cycle(&stream, &NoLicences).await.published, 4);
    assert!(pending(&relay).is_empty());

    // A command the module refuses is an error, not a silent success.
    let anonymous = Relay {
        module: relay.module.clone(),
        actor: String::new(),
        conduct_officer: "conduct-officer".into(),
        clock: relay.clock.clone(),
        batch: 10,
    };
    assert!(matches!(anonymous.tick().await, Err(DriverError::Refused(r)) if r.code == "no_actor"));
}

/// Purpose: the complaints events go to Event Hubs as one ordered batch
/// with the id and sequence consumers de-duplicate on, and a refusal by
/// Event Hubs leaves them unacknowledged.
/// Why it matters: the conduct officer's alerts and the letters the
/// documents service owes are driven from this hub. Consumers can only
/// cope with redelivery if every message carries its id.
#[tokio::test]
async fn complaints_events_are_published_to_event_hubs() {
    let fake = azure::Fake::start().await;
    let (relay, _) = world().await;
    let hub = EventHubsPublisher::new(&format!("{}/eh/", fake.base), "complaints-events", azure::Fake::credential());

    fake.force("eventhubs", 503);
    assert_eq!(relay.publish(&hub).await, Err(DriverError::Unavailable("Event Hubs returned 503".into())));
    assert_eq!(pending(&relay).len(), 3);
    fake.force("eventhubs", 0);
    let waiting = pending(&relay);
    assert_eq!(relay.publish(&hub).await.unwrap(), 3);
    {
        let inner = fake.lock();
        assert_eq!(inner.events.len(), 3);
        assert_eq!(inner.events[1]["UserProperties"], json!({ "event-id": "cmp-2", "seq": 2 }));
        let body: EventRecord = serde_json::from_str(inner.events[1]["Body"].as_str().unwrap()).unwrap();
        assert_eq!(body, waiting[1]);
    }
    assert!(pending(&relay).is_empty());
}

/// Purpose: the relay reaches the complaints module over the cluster's
/// routes for module `complaints`; a module refusal, an answer it cannot
/// read and a cluster with no leader are errors that say which.
/// Why it matters: in production every tick and acknowledgement goes over
/// these two routes. An unreachable cluster must read as "try again".
#[tokio::test]
async fn the_relay_reaches_the_module_over_the_cluster() {
    async fn commands(Json(command): Json<Value>) -> Json<Value> {
        match command["key"].as_str() {
            Some("undecodable") => Json(json!({ "log_index": 4, "result": { "Err": "not a command of this module" } })),
            Some("odd") => Json(json!({ "log_index": 4, "result": { "Ok": "not an output" } })),
            _ => Json(json!({ "log_index": 7, "result": { "Ok": { "index": 7, "result": { "Ok": null }, "events": [], "duplicate": false } } })),
        }
    }
    async fn query(Json(question): Json<Value>) -> Json<Value> {
        if question == json!("state_hash") {
            Json(json!({ "Ok": "abc" }))
        } else {
            Json(json!({ "Err": "refused" }))
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new()
        .route("/v1/modules/complaints/commands", post(commands))
        .route("/v1/modules/complaints/query", post(query));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let envelope = |key: &str| Envelope {
        key: key.into(),
        caller: "scheduler".into(),
        actor: "svc".into(),
        at: Utc.with_ymd_and_hms(2027, 11, 15, 0, 0, 0).unwrap(),
        command: Command::Tick,
    };
    let module = ClusterComplaints::new(ClusterClient::new(vec![addr]), Duration::from_millis(200));
    let applied = module.propose(envelope("tick:1")).await.unwrap();
    assert_eq!((applied.index, applied.duplicate), (7, false));
    assert_eq!(
        module.propose(envelope("undecodable")).await,
        Err(DriverError::Answer("not a command of this module".into()))
    );
    assert!(matches!(module.propose(envelope("odd")).await, Err(DriverError::Answer(_))));
    assert_eq!(module.ask(Query::StateHash).await.unwrap(), json!("abc"));
    assert_eq!(module.ask(Query::Queue { limit: 1 }).await, Err(DriverError::Answer("refused".into())));

    let nowhere = ClusterComplaints::new(ClusterClient::new(vec!["127.0.0.1:1".into()]), Duration::from_millis(50));
    assert!(matches!(nowhere.propose(envelope("tick:2")).await, Err(DriverError::Unavailable(m)) if m.contains("module complaints")));
    assert!(matches!(nowhere.ask(Query::StateHash).await, Err(DriverError::Unavailable(_))));
}

/// Purpose: the relay loop runs a pass, waits, and runs again until told
/// to stop, carrying on through a pass that failed.
/// Why it matters: if one failed pass ended the loop, a brief outage
/// would stop every complaint timer until someone restarted the service.
#[tokio::test]
async fn the_relay_loop_keeps_going_until_stopped() {
    let (relay, _) = world().await;
    let module = relay.module.clone();
    let stream = MemoryStream::default();
    stream.set_down(true);
    let seen = Arc::new(Mutex::new(Vec::<RelayCycle>::new()));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let looping = tokio::spawn({
        let stream = stream.clone();
        let record = seen.clone();
        async move {
            let stop = async {
                let _ = stopped.await;
            };
            run_relay(relay, stream, NoLicences, Duration::from_millis(5), stop, move |cycle| {
                record.lock().unwrap().push(cycle.clone())
            })
            .await;
        }
    });
    let wait_for = |done: Box<dyn Fn(&[RelayCycle]) -> bool + Send>| {
        let seen = seen.clone();
        async move {
            for _ in 0..400 {
                if done(&seen.lock().unwrap()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the loop did not get there");
        }
    };
    wait_for(Box::new(|cycles| cycles.len() >= 2)).await;
    assert!(seen.lock().unwrap().iter().all(|c| c.errors.len() == 1 && c.published == 0));

    stream.set_down(false);
    wait_for(Box::new(|cycles| cycles.iter().any(|c| c.published == 3))).await;
    assert!(module.read(|s| s.pending_events(10).is_empty()));

    stop.send(()).unwrap();
    looping.await.unwrap();
    let after = seen.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(seen.lock().unwrap().len(), after, "nothing runs after the stop");
}

/// Purpose: a memory stream that is down refuses, and one that is up
/// keeps what it was given in order.
/// Why it matters: every failure test above leans on this double. If it
/// accepted events while "down" those tests would pass for the wrong
/// reason.
#[tokio::test]
async fn the_memory_stream_refuses_when_down() {
    let (relay, _) = world().await;
    let events = pending(&relay);
    let stream = MemoryStream::default();
    stream.set_down(true);
    assert_eq!(stream.publish(&events).await, Err("the stream is unavailable".to_string()));
    stream.set_down(false);
    stream.publish(&events).await.unwrap();
    assert_eq!(stream.seen(), events);
    relay.module.set_down(true);
    assert!(matches!(relay.module.ask(Query::StateHash).await, Err(DriverError::Unavailable(_))));
}

fn licence_case(event: &str, advisers: &[&str], policies: &[String]) -> LicenceCase {
    LicenceCase {
        licence_event: event.into(),
        party: PartyId::from("fap-1"),
        status: Some("revoked".into()),
        effective: chrono::NaiveDate::from_ymd_opt(2027, 11, 12),
        advisers: advisers.iter().map(|a| PartyId::from(*a)).collect(),
        policies: policies.iter().map(|p| PolicyId::from(p.as_str())).collect(),
    }
}

/// A feed with one licence case over `n` policies, two to each customer.
fn feed_with(n: usize) -> MemoryLicences {
    let feed = MemoryLicences::default();
    let policies: Vec<String> = (0..n).map(|i| format!("pol-{i:04}")).collect();
    for (i, policy) in policies.iter().enumerate() {
        feed.policyholders
            .lock()
            .unwrap()
            .insert(PolicyId::from(policy.as_str()), PartyId::from(format!("cust-{:04}", i / 2).as_str()));
    }
    feed.cases.lock().unwrap().push(licence_case("ev-1", &["adviser-1", "adviser-2"], &policies));
    feed
}

/// Purpose: a licence event the commission module reports becomes one
/// conduct case owned by the conduct officer, with its advisers, its
/// policies and each policy's holder attached in bounded chunks and the
/// population completed; later passes leave a completed case alone.
/// Why it matters: this is the only path from a licence being revoked to
/// the customers of that adviser being told. A module cannot call another,
/// so without this forwarder the conduct case in the addendum would never
/// open in a deployed cluster.
#[tokio::test]
async fn a_licence_event_becomes_a_conduct_case_with_its_population() {
    let (relay, _) = world().await;
    let feed = feed_with(1101);
    assert_eq!(relay.forward_licence_cases(&feed).await.unwrap(), 1);

    let case = relay.module.read(|s| s.conduct["licence:ev-1"].clone());
    assert_eq!(case.owner, "conduct-officer");
    assert_eq!(
        case.kind,
        ConductKind::LicenceEvent {
            licence_event: "ev-1".into(),
            party: PartyId::from("fap-1"),
            status: "revoked".into(),
            effective: chrono::NaiveDate::from_ymd_opt(2027, 11, 12).unwrap(),
        }
    );
    assert!(case.population_complete);
    assert_eq!((case.customers.len(), case.advisers.len(), case.policies.len()), (551, 2, 1101));
    assert!(case.customers.values().all(|c| *c == Contact::Outstanding));
    assert!(case.regulator.is_some(), "the regulator is to be told of a licence event");
    let events = pending(&relay);
    assert!(events.iter().any(|e| matches!(&e.event, Event::PopulationComplete { customers: 551, .. })));

    // Nothing more is sent for a case that is complete.
    let before = relay.module.read(|s| s.state_hash());
    assert_eq!(relay.forward_licence_cases(&feed).await.unwrap(), 0);
    assert_eq!(relay.module.read(|s| s.state_hash()), before);
    // As part of a pass.
    let cycle = relay.cycle(&MemoryStream::default(), &feed).await;
    assert_eq!((cycle.cases, cycle.errors.len()), (0, 0));
    assert_eq!(NoLicences.cases().await, Ok(vec![]));
    assert_eq!(NoLicences.policyholder(&PolicyId::from("pol-0000")).await, Ok(None));
}

/// Purpose: a pass cut short partway through a population is finished by
/// the next, and the finished case holds each member exactly once.
/// Why it matters: the service can stop at any command. A half-attached
/// population that was then completed would leave customers of a struck
/// off adviser untold, with the case saying everyone had been found.
#[tokio::test]
async fn an_interrupted_population_is_finished_by_the_next_pass() {
    let (relay, clock) = world().await;
    let feed = feed_with(1101);
    // Play a crash after the case was opened and one chunk attached.
    let at = *clock.lock().unwrap();
    for (key, command) in [
        (
            "manual-open",
            Command::OpenConductCase {
                case: "licence:ev-1".into(),
                kind: ConductKind::LicenceEvent {
                    licence_event: "ev-1".into(),
                    party: PartyId::from("fap-1"),
                    status: "revoked".into(),
                    effective: chrono::NaiveDate::from_ymd_opt(2027, 11, 12).unwrap(),
                },
                owner: "conduct-officer".into(),
                summary_document: None,
                notify_regulator: true,
            },
        ),
        (
            "manual-chunk",
            Command::AttachPopulation {
                case: "licence:ev-1".into(),
                customers: vec![PartyId::from("cust-0000")],
                advisers: vec![],
                policies: vec![PolicyId::from("pol-0000")],
            },
        ),
    ] {
        let applied = relay
            .module
            .propose(Envelope {
                key: key.into(),
                caller: "distribution".into(),
                actor: "documents-service".into(),
                at,
                command,
            })
            .await
            .unwrap();
        assert!(applied.result.is_ok());
    }
    assert!(!relay.module.read(|s| s.conduct["licence:ev-1"].population_complete));

    assert_eq!(relay.forward_licence_cases(&feed).await.unwrap(), 1);
    let case = relay.module.read(|s| s.conduct["licence:ev-1"].clone());
    assert!(case.population_complete);
    assert_eq!((case.customers.len(), case.policies.len()), (551, 1101));
}

/// Purpose: when the commission module cannot be reached, a policy has no
/// holder on record, the report names no licence event, or the complaints
/// module refuses, the pass says so, completes nothing for that case,
/// still forwards the other cases, and succeeds once the cause is gone.
/// Why it matters: a population completed without a customer whose policy
/// could not be resolved would silently drop that customer from the
/// communication. Failing loudly on every pass until it is fixed is the
/// only safe behaviour; and one bad case must not block the others.
#[tokio::test]
async fn a_case_that_cannot_be_completed_is_reported_and_retried() {
    let (relay, clock) = world().await;
    let feed = feed_with(4);
    feed.cases.lock().unwrap().push(licence_case("ev-2", &["adviser-9"], &[]));

    feed.set_down(true);
    let cycle = relay.cycle(&MemoryStream::default(), &feed).await;
    assert_eq!(
        cycle.errors,
        vec!["licence cases: the documents module is unavailable: the commission module is unavailable".to_string()]
    );
    assert!(relay.module.read(|s| s.conduct.is_empty()));
    feed.set_down(false);

    // One policy of ev-1 has no holder: ev-1 fails, ev-2 is still forwarded.
    let lost = feed.policyholders.lock().unwrap().remove(&PolicyId::from("pol-0003")).unwrap();
    let error = relay.forward_licence_cases(&feed).await.unwrap_err();
    assert!(
        matches!(&error, DriverError::Unavailable(m) if m.contains("ev-1") && m.contains("pol-0003")),
        "{error}"
    );
    assert!(
        relay.module.read(|s| !s.conduct.contains_key("licence:ev-1")),
        "nothing is attached before everyone is known"
    );
    assert!(relay.module.read(|s| s.conduct["licence:ev-2"].population_complete));

    feed.policyholders.lock().unwrap().insert(PolicyId::from("pol-0003"), lost);
    *clock.lock().unwrap() += chrono::Duration::seconds(15);
    assert_eq!(relay.forward_licence_cases(&feed).await.unwrap(), 1);
    assert_eq!(relay.module.read(|s| s.conduct["licence:ev-1"].customers.len()), 2);

    // A case reported without its licence event cannot be opened.
    feed.cases.lock().unwrap().push(LicenceCase {
        status: None,
        ..licence_case("ev-3", &[], &[])
    });
    let error = relay.forward_licence_cases(&feed).await.unwrap_err();
    assert!(error.to_string().contains("ev-3: ") && error.to_string().contains("no licence event"));
    feed.cases.lock().unwrap().pop();

    // The complaints module is down partway: reported, and nothing is lost.
    feed.cases.lock().unwrap().push(licence_case("ev-4", &[], &[]));
    relay.module.set_down(true);
    assert!(relay.forward_licence_cases(&feed).await.is_err());
    relay.module.set_down(false);
    *clock.lock().unwrap() += chrono::Duration::seconds(15);
    assert_eq!(relay.forward_licence_cases(&feed).await.unwrap(), 1);
}

/// Purpose: a refusal met on one attempt is not remembered against the
/// case: with no timers active the case cannot open, and once they are
/// active the next pass opens it.
/// Why it matters: the module remembers the result of every key. Had the
/// forwarder used the same key each time, the first refusal would be
/// replayed for ever and the licence event would never get its case.
#[tokio::test]
async fn a_refusal_is_not_remembered_against_the_case() {
    let clock: Clock = Arc::new(Mutex::new(Utc.with_ymd_and_hms(2027, 11, 14, 23, 0, 0).unwrap()));
    let reads = clock.clone();
    let relay = Relay {
        module: LocalComplaints::default(),
        actor: "documents-service".into(),
        conduct_officer: "conduct-officer".into(),
        clock: Arc::new(move || *reads.lock().unwrap()),
        batch: 100,
    };
    let feed = feed_with(2);
    let error = relay.forward_licence_cases(&feed).await.unwrap_err();
    assert!(error.to_string().contains("no_timers") || error.to_string().contains("no timer"), "{error}");

    let at = *clock.lock().unwrap();
    relay
        .module
        .propose(Envelope {
            key: "t1".into(),
            caller: "config".into(),
            actor: "ops".into(),
            at,
            command: Command::ActivateTimers(timers()),
        })
        .await
        .unwrap();
    *clock.lock().unwrap() += chrono::Duration::seconds(15);
    assert_eq!(relay.forward_licence_cases(&feed).await.unwrap(), 1);
}

/// Purpose: over the cluster, licence cases are read from module
/// `distribution` with the question `conduct_cases`, and a policy's holder
/// from the partition that holds the policy; an unreadable answer, a
/// refusal and an unreachable cluster are errors.
/// Why it matters: these are the two production reads the forwarder
/// depends on. If the question or the route drifted from the commission
/// module's, licence cases would silently stop arriving.
#[tokio::test]
async fn licence_cases_and_policyholders_are_read_over_the_cluster() {
    async fn query(Json(question): Json<Value>) -> Json<Value> {
        assert_eq!(question, json!("conduct_cases"));
        Json(json!({ "Ok": [{
            "case": "conduct-fap-1-2027-11-12", "licence_event": "FMA-77", "party": "fap-1", "status": "suspended",
            "effective": "2027-11-12", "opened": "2027-11-13", "advisers": ["adviser-1"], "payees": [], "policies": ["pol-1"], "closed": null
        }] }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new().route("/v1/modules/distribution/query", post(query));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let feed = ClusterLicences::new(ClusterClient::new(vec![addr.clone()]).with_attempt_timeout(Duration::from_millis(300)), 2);
    let cases = feed.cases().await.unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].licence_event, "FMA-77");
    assert_eq!(cases[0].status.as_deref(), Some("suspended"));
    assert_eq!(cases[0].policies, vec![PolicyId::from("pol-1")]);
    // This fake has no partitions: the policy directory cannot be reached.
    assert!(feed.policyholder(&PolicyId::from("pol-1")).await.is_err());

    async fn garbled() -> Json<Value> {
        Json(json!({ "Ok": { "not": "a list" } }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let odd = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, Router::new().route("/v1/modules/distribution/query", post(garbled))).await;
    });
    let error = ClusterLicences::new(ClusterClient::new(vec![odd]), 1).cases().await.unwrap_err();
    assert!(error.contains("could not be read"), "{error}");

    let nowhere = ClusterLicences::new(
        ClusterClient::new(vec!["127.0.0.1:1".into()]).with_attempt_timeout(Duration::from_millis(100)),
        1,
    );
    assert!(nowhere.cases().await.is_err());
    // With no partitions to search, no policy is found anywhere.
    let empty = ClusterLicences::new(ClusterClient::new(vec!["127.0.0.1:1".into()]), 0);
    assert_eq!(empty.policyholder(&PolicyId::from("pol-1")).await, Ok(None));
}
