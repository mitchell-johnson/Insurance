//! A command's trace on a ledger node: the node continues the caller's trace and records it on
//! the command and on every event the command makes. Alone in its file, because it installs the
//! process's tracing subscriber.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{scratch, start_date};
use ic_core::{Command, Envelope};
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient, Reply};
use ic_sim::config::shipped_activations;
use ic_telemetry::Output;

const TOKEN: &str = "test-cluster-token-0123456789abcdef";
const CALLER: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
const CALLER_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const CARRIED: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

/// The trace ID and span ID of a `traceparent`.
fn ids(traceparent: &str) -> (&str, &str) {
    let parts: Vec<&str> = traceparent.split('-').collect();
    (parts[1], parts[2])
}

/// Purpose: a command proposed over HTTP with a `traceparent` header is recorded with a
/// `traceparent` in the caller's trace (the node's own server span, so the same trace ID and a
/// span ID of its own), and the event it makes carries that same `traceparent`, which is what
/// the node publishes to Event Hubs beside each event. A command whose envelope already carries
/// a `traceparent` (a module or worker that set it) keeps that one whatever the header says.
/// Why it matters: this is the step that ties a business event back to the request that caused
/// it: without it a trace would stop at the node, and an event on Event Hubs or in a module's
/// feed could not be followed back to the portal page or service call that made it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_command_and_its_events_carry_the_callers_trace() {
    let settings = ic_telemetry::Settings::from_lookup("ic-node-trace-test", |_| None).unwrap();
    let (subscriber, _telemetry) = ic_telemetry::subscriber(&settings, Output::Stderr).unwrap();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let dir = scratch("node-trace");
    let port = common::free_port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
    let node = server::start(Settings {
        id: 1,
        listen: addr.parse().unwrap(),
        data_dir: dir.join("node-1"),
        partitions: 1,
        peers: BTreeMap::from([(1, addr.clone())]),
        bootstrap: true,
        heartbeat_ms: 50,
        auth: auth.clone(),
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 1_000,
        snapshot_every: 10_000,
        witnesses: Default::default(),
    })
    .await
    .unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    // Wait for the node to lead its partition, then register a consumer so the partition keeps
    // its events for this test to read.
    let warm_up = Envelope {
        key: "warm-up".to_string(),
        caller: "leader".to_string(),
        salt: fresh_salt(),
        traceparent: None,
        command: Command::OpenBusinessDate { date: start_date() },
    };
    client.propose_until(0, &warm_up, Duration::from_secs(20)).await.unwrap();
    client.acknowledge_events(0, "trace-test", 0).await.unwrap();

    let http = reqwest::Client::new();
    let mut activations = shipped_activations().unwrap().into_iter();
    let mut propose = |key: &str, traceparent: Option<&str>| {
        let envelope = Envelope {
            key: key.to_string(),
            caller: "config".to_string(),
            salt: fresh_salt(),
            traceparent: traceparent.map(str::to_string),
            command: activations.next().expect("an activation that makes an event"),
        };
        http.post(format!("http://{addr}/v1/partitions/0/commands"))
            .bearer_auth(TOKEN)
            .header("traceparent", CALLER)
            .json(&envelope)
            .send()
    };
    let traced: Reply = propose("traced", None).await.unwrap().json().await.unwrap();
    assert_eq!(traced.rejected, None);
    let carried: Reply = propose("carried", Some(CARRIED)).await.unwrap().json().await.unwrap();
    assert_eq!(carried.rejected, None);

    // What the proposer was told, and what the partition keeps for its consumers (and publishes
    // to Event Hubs): the same trace on every event of each command.
    let page = client.events(0, "trace-test", Some(0), 1_000).await.unwrap();
    let kept = |index: u64| page.events.iter().filter(|e| e.log_index == index).cloned().collect::<Vec<_>>();
    let traced_events = [traced.events.clone(), kept(traced.log_index)].concat();
    assert!(!traced.events.is_empty() && kept(traced.log_index).len() == traced.events.len());
    for event in &traced_events {
        let tp = event.traceparent.as_deref().expect("the event carries a trace");
        let (trace, span) = ids(tp);
        assert_eq!(trace, CALLER_TRACE, "the event is in the caller's trace");
        assert_ne!(span, ids(CALLER).1, "under the node's own server span, a child of the caller's");
    }
    let carried_events = [carried.events.clone(), kept(carried.log_index)].concat();
    assert!(!carried_events.is_empty());
    assert!(
        carried_events.iter().all(|e| e.traceparent.as_deref() == Some(CARRIED)),
        "an envelope's own trace is kept"
    );

    node.stop().await.unwrap();
}
