//! The dunning notices collections writes, issued by the documents driver
//! and reported back as sent.

mod support;

#[path = "../../collections/tests/common/mod.rs"]
mod collections_world;

use std::sync::Arc;

use chrono::NaiveDate;
use ic_collections::model::NoticeState;
use ic_collections::StepKind;
use ic_core::interest::InterestEvent;
use ic_core::{Event as CoreEvent, EventRecord};
use ic_documents::channels::{Channels, Contact, MemoryDirectory, Outbound, SendError};
use ic_documents::collections_notices::{channels_for, document_id, run_notices, ClusterCollections, Collections, LocalCollections, NoticeRelay};
use ic_documents::command::Command;
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, LocalModule, Module};
use ic_documents::kinds::DataClass;
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::vault::MemoryStore;
use ic_documents::DocumentKind;
use serde_json::{json, Value};
use support::*;

#[derive(Clone, Default)]
struct Quiet;

impl Channels for Quiet {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}-{}", message.document, message.party))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Quiet, MemoryDirectory>;

const REMINDER: &str = "Dear {{name}}. A payment is overdue ({{reference}}).";
const SUSPENSION: &str = "Dear {{name}}. Your cover is suspended from {{date|date}}.";

fn notice_data() -> Value {
    json!({ "name": "Mere", "reference": "n-1", "date": "2027-04-01", "step": "reminder", "policies": ["p1"] })
}

/// A lender's interest in policy p1 from the start of 2027.
fn lender_interest() -> EventRecord {
    EventRecord {
        id: "7-1".into(),
        partition: 7,
        seq: 1,
        business_date: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        log_index: 1,
        traceparent: None,
        event: CoreEvent::Interest(InterestEvent::Recorded {
            policy: policy("p1"),
            interest: "i-1".into(),
            party: party("bank"),
            notice_received: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
            extent_minor: None,
            effective_from: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        }),
    }
}

/// Collections with Mere's policy p1 unpaid from 1 March and closed
/// through `through`: the notices of each step reached are written.
fn collections_through(through: NaiveDate) -> LocalCollections {
    let mut world = collections_world::World::new();
    world.issue("p1", "mere", 10_000);
    world.overdue("p1", collections_world::d(2027, 3, 1));
    world.close_through(through);
    LocalCollections::from_state(world.state, world.index)
}

struct World {
    relay: NoticeRelay<LocalCollections>,
    driver: Arc<TestDriver>,
}

impl World {
    async fn new(through: NaiveDate, templates: &[(&str, DocumentKind, &str)]) -> Self {
        let mut kit = Kit::new();
        for (id, kind, body) in templates {
            kit.activate(template(id, *kind, body), &notice_data());
        }
        kit.ok(
            "relay",
            "svc",
            Command::ObserveCoreEvents {
                partition: 7,
                records: vec![lender_interest()],
            },
        );
        let module = LocalModule::default();
        for envelope in &kit.log {
            assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
        }
        let directory = MemoryDirectory::default();
        for (channel, format) in [(Channel::Email, "large_print"), (Channel::Sms, "braille")] {
            directory.set_contact(
                "mere",
                channel,
                Contact {
                    name: "Mere Parata".into(),
                    destination: "mere@example.nz".into(),
                    format: Some(format.into()),
                    ..Contact::default()
                },
            );
        }
        directory.set("mere", Channel::Post, "Mere Parata", "1 Queen St\nAuckland");
        directory.set("bank", Channel::Email, "Lender Bank", "loans@bank.example");
        let now = kit.now;
        let driver = Arc::new(Driver {
            module,
            objects: MemoryStore::default(),
            wrap: LocalWrap::new(crypto::new_key()),
            channels: Quiet,
            directory,
            actor: "documents-service".into(),
            clock: Arc::new(move || now),
            batch: 100,
        });
        let relay = NoticeRelay {
            collections: collections_through(through),
            actor: "documents-service".into(),
            clock: Arc::new(move || now),
            batch: 100,
        };
        Self { relay, driver }
    }

    async fn all(through: NaiveDate) -> Self {
        Self::new(
            through,
            &[
                ("collections/reminder", DocumentKind::Reminder, REMINDER),
                ("collections/suspension_notice", DocumentKind::SuspensionNotice, SUSPENSION),
            ],
        )
        .await
    }

    fn notices(&self, kind: StepKind) -> Vec<(String, NoticeState)> {
        self.relay
            .collections
            .read(|s| s.notices.values().filter(|n| n.kind == kind).map(|n| (n.id.clone(), n.state)).collect())
    }
}

/// Purpose: every reminder and suspension notice collections has written
/// is issued from its template to the customer on the step's channel, the
/// suspension notice with proof of delivery and a copy to the lender
/// standing on the policy, and collections is told each was sent; the next
/// pass issues nothing; and collections is told which arrived and which
/// failed for good, while one still in the post waits.
/// Why it matters: these notices are what lets a policy in arrears lapse
/// lawfully. One not sent leaves the insurer unable to lapse; one sent
/// twice tells a customer they are further behind than they are; and a
/// suspension notice the lender never sees breaks the assignment's terms.
/// A failed notice not reported back would let a policy lapse on a notice
/// that never arrived.
#[tokio::test]
async fn dunning_notices_are_issued_once_and_reported_sent() {
    let world = World::all(collections_world::d(2027, 4, 10)).await;
    let pass = world.relay.pass(&world.driver).await;
    assert_eq!((pass.issued, pass.left, pass.errors.clone()), (3, 0, vec![]));

    let reminders = world.notices(StepKind::Reminder);
    assert_eq!(reminders.len(), 2);
    assert!(reminders.iter().all(|(_, state)| *state == NoticeState::Sent));
    let (suspension, state) = world.notices(StepKind::SuspensionNotice).remove(0);
    assert_eq!(state, NoticeState::Sent);

    let document = world.driver.document(&document_id(&suspension)).await.unwrap();
    assert_eq!(
        (document.kind, document.proof, document.data_class),
        (DocumentKind::SuspensionNotice, true, DataClass::Financial)
    );
    assert_eq!(document.policy, Some(policy("p1")));
    assert_eq!(document.trigger_event.as_deref(), Some(document_id(&suspension).as_str()));
    let roles: Vec<(String, RecipientRole)> = document.deliveries.iter().map(|d| (d.recipient.party.0.clone(), d.recipient.role)).collect();
    assert_eq!(
        roles,
        [
            ("mere".to_string(), RecipientRole::Addressee),
            ("bank".to_string(), RecipientRole::InterestedParty)
        ]
    );
    assert_eq!(document.deliveries[0].chain.last(), Some(&Channel::Post));

    // A reminder goes on its step's channel, to the customer alone, in
    // the format the customer master gives first (email's, here, large
    // print); braille, which the template does not offer, would have gone
    // in the ordinary format.
    let email = world.driver.document(&document_id(&reminders[0].0)).await.unwrap();
    assert_eq!(email.deliveries.len(), 1);
    assert_eq!(email.deliveries[0].chain, [Channel::Email]);
    assert_eq!(email.format, Some(ic_documents::template::AlternateFormat::LargePrint));
    assert!(world
        .driver
        .read(&document_id(&reminders[0].0))
        .await
        .unwrap()
        .text
        .starts_with("Dear Mere Parata. A payment is overdue"));

    let again = world.relay.pass(&world.driver).await;
    assert_eq!((again.issued, again.left, again.reported), (0, 0, 0));

    // The emailed reminder is delivered and the texted one fails for
    // good; the suspension notice is still in the post.
    world.driver.deliver().await.unwrap();
    let report = |notice: &str, outcome| ic_documents::channels::DeliveryReport {
        provider_reference: format!("ref-{}-mere", document_id(notice)),
        outcome,
    };
    use ic_documents::model::AttemptOutcome;
    assert!(world.driver.report(report(&reminders[0].0, AttemptOutcome::Delivered)).await.unwrap());
    assert!(world
        .driver
        .report(report(
            &reminders[1].0,
            AttemptOutcome::Failed {
                reason: "unknown number".into()
            }
        ))
        .await
        .unwrap());
    let reported = world.relay.pass(&world.driver).await;
    assert_eq!((reported.reported, reported.errors.clone()), (2, vec![]));
    let states: Vec<NoticeState> = world.notices(StepKind::Reminder).into_iter().map(|(_, s)| s).collect();
    assert_eq!(states, [NoticeState::Delivered, NoticeState::Undeliverable]);
    assert_eq!(world.notices(StepKind::SuspensionNotice)[0].1, NoticeState::Sent);
    assert_eq!(world.relay.pass(&world.driver).await.reported, 0);
}

/// Purpose: a notice whose template has no wording waits and is reported
/// once however many need it; an outage of collections or of the
/// documents module is reported, marks nothing sent, and the next pass
/// finishes the work.
/// Why it matters: a final notice marked sent that was never issued would
/// let collections lapse a policy on a notice the customer never got; and
/// wording arrives after the code, so notices must queue until it does.
#[tokio::test]
async fn notices_wait_for_wording_and_survive_outages() {
    let world = World::new(
        collections_world::d(2027, 5, 1),
        &[("collections/suspension_notice", DocumentKind::SuspensionNotice, SUSPENSION)],
    )
    .await;
    world.relay.collections.set_down(true);
    let down = world.relay.pass(&world.driver).await;
    assert!(down.errors[0].starts_with("notices due"));
    world.relay.collections.set_down(false);

    world.driver.module.set_down(true);
    let down = world.relay.pass(&world.driver).await;
    assert_eq!(down.issued, 0);
    assert!(down.errors.iter().all(|e| e.starts_with("collections:")));
    world.driver.module.set_down(false);
    assert!(world.notices(StepKind::SuspensionNotice).iter().all(|(_, s)| *s == NoticeState::Due));
    // Collections cannot take the report: the error says so.
    world.relay.collections.set_down(true);
    let envelope = ic_collections::Envelope {
        key: "k".into(),
        caller: "delivery".into(),
        actor: "a".into(),
        roles: vec![],
        at: chrono::Utc::now(),
        command: ic_collections::Command::RecordNoticeSent { notice: "n".into() },
    };
    assert!(matches!(
        world.relay.collections.propose(envelope).await,
        Err(ic_documents::driver::DriverError::Unavailable(_))
    ));
    world.relay.collections.set_down(false);

    let stop = async { tokio::time::sleep(std::time::Duration::from_millis(30)).await };
    let mut passes = Vec::new();
    run_notices(
        NoticeRelay {
            collections: world.relay.collections.clone(),
            actor: "documents-service".into(),
            clock: world.relay.clock.clone(),
            batch: 100,
        },
        world.driver.clone(),
        std::time::Duration::from_millis(5),
        stop,
        |p| passes.push(p.clone()),
    )
    .await;
    // The suspension notice goes; the two reminders and the final notice
    // have no wording yet, each template said once a pass.
    assert_eq!(passes[0].issued, 1);
    assert_eq!(passes[0].left, 3);
    assert_eq!(passes[0].errors.iter().filter(|e| e.contains("has no active version")).count(), 2);
    assert!(passes.len() >= 2 && passes[1].issued == 0);
    assert!(world.notices(StepKind::SuspensionNotice).iter().all(|(_, s)| *s == NoticeState::Sent));
    assert!(world.notices(StepKind::Reminder).iter().all(|(_, s)| *s == NoticeState::Due));
}

/// Purpose: each step's channel maps to the documents channel it names,
/// and a notice on a call step, which should not happen, goes by post.
/// Why it matters: a reminder the sequence says goes by SMS must not go by
/// email the customer no longer reads, and no notice may be dropped
/// because its channel had no mapping.
#[test]
fn each_step_channel_has_a_documents_channel() {
    use ic_collections::Channel as C;
    let mapped: Vec<Vec<Channel>> = [C::Email, C::Sms, C::App, C::Post, C::Queue].into_iter().map(channels_for).collect();
    assert_eq!(
        mapped,
        [
            vec![Channel::Email],
            vec![Channel::Sms],
            vec![Channel::InApp],
            vec![Channel::Post],
            vec![Channel::Post]
        ]
    );
}

/// Purpose: the relay reaches collections over the cluster's routes for
/// module `collections`; a refusal, an unreadable answer and a cluster
/// with no leader are errors that say which.
/// Why it matters: in production every notice report goes over these
/// routes, and an unreachable cluster must read as "try again", never as
/// a notice that was recorded.
#[tokio::test]
async fn the_relay_reaches_collections_over_the_cluster() {
    use axum::routing::post;
    use axum::{Json, Router};
    use ic_documents::driver::DriverError;
    use ic_raft::ClusterClient;
    async fn commands(Json(command): Json<Value>) -> Json<Value> {
        match command["key"].as_str() {
            Some("odd") => Json(json!({ "log_index": 4, "result": { "Ok": "not an output" } })),
            _ => Json(json!({ "log_index": 7, "result": { "Ok": { "index": 7, "result": { "Ok": null }, "events": [], "duplicate": false } } })),
        }
    }
    async fn query(Json(question): Json<Value>) -> Json<Value> {
        if question["q"] == "notices_due" {
            Json(json!({ "Ok": [] }))
        } else {
            Json(json!({ "Err": "refused" }))
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new()
        .route("/v1/modules/collections/commands", post(commands))
        .route("/v1/modules/collections/query", post(query));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let envelope = |key: &str| ic_collections::Envelope {
        key: key.into(),
        caller: "delivery".into(),
        actor: "svc".into(),
        roles: vec![],
        at: chrono::Utc::now(),
        command: ic_collections::Command::RecordNoticeSent { notice: "n1".into() },
    };
    let module = ClusterCollections::new(ClusterClient::new(vec![addr]), std::time::Duration::from_millis(200));
    assert_eq!(module.propose(envelope("k1")).await.unwrap().index, 7);
    assert!(matches!(module.propose(envelope("odd")).await, Err(DriverError::Answer(_))));
    assert_eq!(module.ask(json!({ "q": "notices_due", "limit": 1 })).await.unwrap(), json!([]));
    assert_eq!(module.ask(json!({ "q": "tasks" })).await, Err(DriverError::Answer("refused".into())));

    let nowhere = ClusterCollections::new(ClusterClient::new(vec!["127.0.0.1:1".into()]), std::time::Duration::from_millis(50));
    assert!(matches!(nowhere.propose(envelope("k2")).await, Err(DriverError::Unavailable(_))));
    assert!(matches!(
        nowhere.ask(json!({ "q": "notices_due", "limit": 1 })).await,
        Err(DriverError::Unavailable(_))
    ));
}

/// Collections behind switches that fail the delivery question or every
/// command, to play a module that answers one thing and not the next.
#[derive(Clone, Default)]
struct Flaky {
    inner: LocalCollections,
    fail_sent_question: Arc<std::sync::Mutex<bool>>,
    fail_commands: Arc<std::sync::Mutex<bool>>,
}

impl Collections for Flaky {
    async fn propose(&self, envelope: ic_collections::Envelope) -> Result<ic_collections::Applied, ic_documents::driver::DriverError> {
        if *self.fail_commands.lock().unwrap() {
            return Err(ic_documents::driver::DriverError::Unavailable("no leader".into()));
        }
        self.inner.propose(envelope).await
    }

    async fn ask(&self, question: Value) -> Result<Value, ic_documents::driver::DriverError> {
        if question["q"] == "notices_sent" && *self.fail_sent_question.lock().unwrap() {
            return Err(ic_documents::driver::DriverError::Unavailable("no leader".into()));
        }
        self.inner.ask(question).await
    }
}

/// Purpose: a delivery outcome collections cannot take is reported and
/// sent again on the next pass; a failed question about sent notices is
/// reported; and a sent notice whose document cannot be read while the
/// documents module is down is reported, not guessed at.
/// Why it matters: the outcome decides whether collections opens a task
/// to reach a customer before lapse. Losing it in an outage would let a
/// policy lapse on an undelivered notice, and guessing an outcome while
/// the documents module is down could report a notice delivered that was not.
#[tokio::test]
async fn delivery_outcomes_survive_outages_and_strays_are_reported() {
    let world = World::all(collections_world::d(2027, 4, 10)).await;
    let flaky = Flaky {
        inner: world.relay.collections.clone(),
        ..Flaky::default()
    };
    let relay = NoticeRelay {
        collections: flaky.clone(),
        actor: "documents-service".into(),
        clock: world.relay.clock.clone(),
        batch: 100,
    };
    assert_eq!(relay.pass(&world.driver).await.issued, 3);
    world.driver.deliver().await.unwrap();
    let (reminder, _) = world.notices(StepKind::Reminder).remove(0);
    let reference = format!("ref-{}-mere", document_id(&reminder));
    let delivered = ic_documents::channels::DeliveryReport {
        provider_reference: reference,
        outcome: ic_documents::model::AttemptOutcome::Delivered,
    };
    assert!(world.driver.report(delivered).await.unwrap());

    *flaky.fail_sent_question.lock().unwrap() = true;
    let pass = relay.pass(&world.driver).await;
    assert!(pass.errors.iter().any(|e| e.starts_with("notices sent")));
    *flaky.fail_sent_question.lock().unwrap() = false;

    *flaky.fail_commands.lock().unwrap() = true;
    let pass = relay.pass(&world.driver).await;
    assert_eq!(pass.reported, 0);
    assert!(pass.errors.iter().any(|e| e.starts_with(&document_id(&reminder))));
    *flaky.fail_commands.lock().unwrap() = false;
    assert_eq!(relay.pass(&world.driver).await.reported, 1);

    // The other reminder is recorded sent again, and the documents
    // module cannot be read when its outcome is looked up.
    let stray = ic_collections::Envelope {
        key: "stray".into(),
        caller: "delivery".into(),
        actor: "someone".into(),
        roles: vec![],
        at: chrono::Utc::now(),
        command: ic_collections::Command::RecordNoticeSent {
            notice: world.notices(StepKind::Reminder)[1].0.clone(),
        },
    };
    assert_eq!(flaky.inner.propose(stray).await.unwrap().result, Ok(()));
    let documents_down = world.driver.module.clone();
    documents_down.set_down(true);
    let pass = relay.pass(&world.driver).await;
    documents_down.set_down(false);
    assert!(pass.errors.iter().any(|e| e.contains("unavailable")));
}
