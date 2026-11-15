//! Documents shown in the channels: a row for each document on a policy,
//! written to the channel module and taken down when the document is
//! destroyed.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ic_core::PolicyId;
use ic_documents::channels::{Channels, MemoryDirectory, Outbound, SendError};
use ic_documents::command::Command;
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, DriverError, IssueRequest, LocalModule, Module};
use ic_documents::kinds::DataClass;
use ic_documents::listing::{envelope, run_listing, ClusterRows, RowSink, RowsRefused};
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::vault::MemoryStore;
use ic_log::StateMachine;
use serde_json::{json, Value};
use support::*;

#[derive(Clone, Default)]
struct Quiet;

impl Channels for Quiet {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}", message.document))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Quiet, MemoryDirectory>;

/// The real channel module's state machine, behind switches that play an
/// outage or a refusal.
#[derive(Clone)]
struct Portal {
    state: Arc<Mutex<ic_channels::state::ChannelState>>,
    down: Arc<Mutex<bool>>,
    refuse: Arc<Mutex<bool>>,
}

impl Default for Portal {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(ic_channels::state::ChannelState::shipped().unwrap())),
            down: Arc::default(),
            refuse: Arc::default(),
        }
    }
}

impl Portal {
    fn rows(&self, policy: &str) -> BTreeMap<String, ic_channels::feed::Row> {
        let state = self.state.lock().unwrap();
        state.data.published.get(&PolicyId::from(policy)).cloned().unwrap_or_default()
    }
}

impl RowSink for Portal {
    async fn publish(&self, envelope: Value) -> Result<Result<(), RowsRefused>, DriverError> {
        if *self.down.lock().unwrap() {
            return Err(DriverError::Unavailable("no leader".into()));
        }
        if *self.refuse.lock().unwrap() {
            return Ok(Err(RowsRefused {
                code: "bad_key".into(),
                message: "not yours".into(),
            }));
        }
        let envelope: ic_channels::state::Envelope = serde_json::from_value(envelope).unwrap();
        let output = self.state.lock().unwrap().apply(0, envelope);
        Ok(output.result.map(|_| ()).map_err(|r| RowsRefused {
            code: r.code,
            message: r.message,
        }))
    }
}

fn request(id: &str, policies: &[&str], class: DataClass) -> IssueRequest {
    IssueRequest {
        document: id.into(),
        caller: "documents".into(),
        template: "receipt".into(),
        template_version: None,
        policy: policies.first().map(|p| policy(p)),
        other_policies: policies.iter().skip(1).map(|p| policy(p)).collect(),
        data_class: class,
        lives: vec![],
        prove_delivery: false,
        owner: party("mere"),
        recipients: vec![recipient("mere", RecipientRole::Addressee, &[Channel::Email])],
        language: "en".into(),
        format: None,
        data: sample_data(),
        trigger_event: None,
        case: None,
        supersedes: None,
        reason: None,
        wordings: vec![],
        components: vec![],
        notice: None,
        copy_of: None,
    }
}

async fn driver() -> Arc<TestDriver> {
    let module = LocalModule::default();
    let ready = Kit::ready();
    for envelope in &ready.log {
        assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
    }
    let now = ready.now;
    let driver = Arc::new(Driver {
        module,
        objects: MemoryStore::default(),
        wrap: LocalWrap::new(crypto::new_key()),
        channels: Quiet,
        directory: MemoryDirectory::default(),
        actor: "documents-service".into(),
        clock: Arc::new(move || now),
        batch: 50,
        composer: Default::default(),
    });
    driver.issue(request("r1", &["pol-1"], DataClass::Personal)).await.unwrap();
    driver.issue(request("household", &["pol-1", "pol-2"], DataClass::Financial)).await.unwrap();
    driver.issue(request("health", &["pol-1"], DataClass::Clinical)).await.unwrap();
    driver.issue(request("no-policy", &[], DataClass::Personal)).await.unwrap();
    driver
}

/// Purpose: every document on a policy becomes a row in the channel
/// module, one per policy it covers, under module `documents`, marked
/// sensitive when it is insurance-sensitive or clinical; a document on no
/// policy is not listed; the rows are written once.
/// Why it matters: without the row the app and portals cannot show a
/// customer the letters they were sent. A household notice listed under
/// only one policy would be missing from the other's page, and a clinical
/// letter not marked sensitive would be shown to an accountant who may
/// see the policy but not its health facts.
#[tokio::test]
async fn documents_on_a_policy_are_listed_once() {
    let driver = driver().await;
    let portal = Portal::default();
    assert_eq!(driver.list_documents(&portal).await.unwrap(), 3);
    let first = portal.rows("pol-1");
    assert_eq!(
        first.keys().cloned().collect::<Vec<_>>(),
        ["pol-1/documents/health", "pol-1/documents/household", "pol-1/documents/r1"]
    );
    let row = &first["pol-1/documents/r1"];
    assert_eq!(
        (row.kind.as_str(), row.subject.clone(), row.sensitive),
        ("document", Some(party("mere")), false)
    );
    assert_eq!(row.body["title"], "receipt");
    assert_eq!(row.body["kind"], "premium_receipt");
    assert_eq!(row.body["document"], "r1");
    assert!(first["pol-1/documents/health"].sensitive);
    assert_eq!(portal.rows("pol-2").keys().cloned().collect::<Vec<_>>(), ["pol-2/documents/household"]);
    assert!(driver.module.state().listing.is_empty());
    assert_eq!(driver.list_documents(&portal).await.unwrap(), 0);
}

/// Purpose: a destroyed document's rows are taken down from every policy
/// it was listed on.
/// Why it matters: a row left for a destroyed document would offer the
/// customer a download that can only fail, and keep showing that a letter
/// about them existed after the retention rules said it must go.
#[tokio::test]
async fn a_destroyed_document_is_taken_down() {
    let driver = driver().await;
    let portal = Portal::default();
    driver.list_documents(&portal).await.unwrap();
    let shred = ic_documents::Envelope {
        key: "shred".into(),
        caller: "privacy".into(),
        actor: "privacy-officer".into(),
        at: (driver.clock)(),
        command: Command::ShredParty { party: party("mere") },
    };
    assert!(driver.module.propose(shred).await.unwrap().result.is_ok());
    assert_eq!(driver.list_documents(&portal).await.unwrap(), 3);
    assert!(portal.rows("pol-1").is_empty() && portal.rows("pol-2").is_empty());
}

/// Purpose: while the channel module cannot be reached or refuses, the
/// changes wait and are carried on a later pass; the loop keeps going
/// until stopped.
/// Why it matters: a row change dropped in an outage would leave a
/// document missing from the customer's list for good, or a destroyed one
/// shown for ever.
#[tokio::test]
async fn row_changes_wait_out_an_outage() {
    let driver = driver().await;
    let portal = Portal::default();
    *portal.down.lock().unwrap() = true;
    assert!(matches!(driver.list_documents(&portal).await, Err(DriverError::Unavailable(_))));
    *portal.down.lock().unwrap() = false;
    *portal.refuse.lock().unwrap() = true;
    assert!(matches!(driver.list_documents(&portal).await, Err(DriverError::Refused(r)) if r.code == "bad_key"));
    *portal.refuse.lock().unwrap() = false;
    assert_eq!(driver.module.state().listing.len(), 3);
    driver.module.set_down(true);
    assert!(driver.list_documents(&portal).await.is_err());
    driver.module.set_down(false);

    let stop = async { tokio::time::sleep(std::time::Duration::from_millis(30)).await };
    let mut seen = Vec::new();
    run_listing(driver.clone(), portal.clone(), std::time::Duration::from_millis(5), stop, |r| {
        seen.push(r.as_ref().ok().copied())
    })
    .await;
    assert_eq!(seen[0], Some(3));
    assert!(seen.len() >= 2 && seen[1] == Some(0));
    assert_eq!(portal.rows("pol-1").len(), 3);
}

/// Purpose: the rows reach the channel module over the cluster's route for
/// module `channels`; its refusal, an unreadable answer and a cluster with
/// no leader are errors that say which.
/// Why it matters: in production every row goes over this route, and an
/// unreachable cluster must read as "try again", never as written.
#[tokio::test]
async fn rows_reach_the_channel_module_over_the_cluster() {
    use axum::routing::post;
    use axum::{Json, Router};
    use ic_raft::ClusterClient;
    async fn commands(Json(command): Json<Value>) -> Json<Value> {
        match command["key"].as_str() {
            Some("refused") => {
                Json(json!({ "log_index": 3, "result": { "Ok": { "result": { "Err": { "code": "bad_key", "message": "no" } }, "duplicate": false } } }))
            }
            Some("odd") => Json(json!({ "log_index": 3, "result": { "Ok": "not an output" } })),
            _ => Json(json!({ "log_index": 3, "result": { "Ok": { "result": { "Ok": [] }, "duplicate": false } } })),
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new().route("/v1/modules/channels/commands", post(commands));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let at = chrono::Utc::now();
    let rows = ClusterRows::new(ClusterClient::new(vec![addr]), std::time::Duration::from_millis(200));
    assert_eq!(rows.publish(envelope("ok", at, vec![], vec![])).await.unwrap(), Ok(()));
    assert_eq!(
        rows.publish(envelope("refused", at, vec![], vec![])).await.unwrap().unwrap_err().code,
        "bad_key"
    );
    assert!(matches!(rows.publish(envelope("odd", at, vec![], vec![])).await, Err(DriverError::Answer(_))));
    let nowhere = ClusterRows::new(ClusterClient::new(vec!["127.0.0.1:1".into()]), std::time::Duration::from_millis(50));
    assert!(matches!(
        nowhere.publish(envelope("x", at, vec![], vec![])).await,
        Err(DriverError::Unavailable(_))
    ));
}
