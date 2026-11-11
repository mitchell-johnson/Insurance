//! A ledger node wired to Azure: sealed journal segments to Blob Storage and
//! events to Event Hubs, against a local stand-in for both.
//!
//! This file holds one test because it sets the process environment the
//! workload identity webhook would set.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{d, scratch, start_date};
use ic_azure::blob::BlobContainer;
use ic_azure::cold_tier::fetch_segment;
use ic_azure::credential::AzureCredential;
use ic_azure::testing::FakeAzure;
use ic_core::{Command, Envelope};
use ic_product::{Frequency, Sex};
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};

const TOKEN: &str = "test-cluster-token-0123456789abcdef";

/// Purpose: a ledger node configured for Azure seals its journal to Blob Storage in
/// segments that verify against their manifests, and publishes its events to the hub
/// partition of its ledger partition, under its workload identity. During an Azure outage
/// it keeps accepting commands and holds what it could not move; when Azure returns, the
/// backlog drains and nothing is missing or duplicated in the cold tier.
/// Why it matters: the ledger must not depend on Azure being up to take a payment, and
/// must not let a sealed-and-forgotten gap open in the journal's cold copy, which is the
/// record an auditor replays. This is the only test of the node's real Azure wiring; the
/// other node tests seal to a directory and discard events.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ledger_node_holds_its_output_through_an_azure_outage() {
    let azure = FakeAzure::start().await;
    let dir = scratch("node-azure");
    let token_file = dir.join("federated-token");
    std::fs::write(&token_file, "projected-token").unwrap();
    std::env::set_var("AZURE_FEDERATED_TOKEN_FILE", &token_file);
    std::env::set_var("AZURE_TENANT_ID", "tenant-1");
    std::env::set_var("AZURE_CLIENT_ID", "client-node");
    std::env::set_var("AZURE_AUTHORITY_HOST", &azure.base);

    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
    let settings = Settings {
        id: 1,
        listen: addr.parse().unwrap(),
        data_dir: dir.join("node-1"),
        partitions: 1,
        peers: BTreeMap::from([(1, addr.clone())]),
        bootstrap: true,
        heartbeat_ms: 50,
        auth: auth.clone(),
        cold: ColdTarget::Azure(azure.base.clone(), "cold-segments".to_string()),
        events: EventTarget::EventHubs(azure.base.clone(), "policy-events".to_string()),
        segment_entries: 4,
        snapshot_every: 10_000,
    };
    let node = server::start(settings).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    let run = |key: String, caller: &'static str, command: Command| {
        let client = client.clone();
        async move {
            let envelope = Envelope {
                key,
                caller: caller.to_string(),
                salt: fresh_salt(),
                traceparent: None,
                command,
            };
            let reply = client.propose_until(0, &envelope, Duration::from_secs(20)).await.unwrap();
            assert_eq!(reply.rejected, None);
        }
    };
    let issue = |n: u64| {
        let application = term_life(n, start_date(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        run(format!("issue-{n}"), "new-business", issue_command(application))
    };
    let backlog = || async {
        let status = client.status(&addr).await.unwrap();
        (status[0].backlog.journal, status[0].backlog.events, status[0].ledger_entries)
    };
    // Wait until the leader's housekeeping has left fewer than one segment
    // of journal and no events behind.
    let drained = || async {
        for _ in 0..200 {
            let (journal, events, _) = backlog().await;
            if journal < 4 && events == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the backlog did not drain: {:?}", backlog().await);
    };

    run("open".to_string(), "leader", Command::OpenBusinessDate { date: start_date() }).await;
    for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
        run(format!("activate-{n}"), "config", activation).await;
    }
    for n in 1..=4 {
        issue(n).await;
    }
    drained().await;
    let sealed_before = azure.blobs().len();
    assert!(sealed_before >= 2, "no segment and manifest reached Blob Storage: {:?}", azure.blobs().keys());
    assert!(azure.blobs().keys().all(|k| k.starts_with("cold-segments/")));
    assert!(azure.events_sent("policy-events", 0) > 0);
    assert!(azure.seen().iter().any(|s| s.contains("client_id=client-node")));

    // The outage. Commands are still accepted; the output waits on the node.
    azure.set_down(true);
    let sent_before = azure.events_sent("policy-events", 0);
    for n in 5..=8 {
        issue(n).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (journal, events, _) = backlog().await;
    assert!(
        journal >= 4 && events > 0,
        "nothing was held back during the outage: journal {journal}, events {events}"
    );
    assert_eq!(azure.blobs().len(), sealed_before);
    assert_eq!(azure.events_sent("policy-events", 0), sent_before);

    azure.set_down(false);
    drained().await;
    assert!(azure.blobs().len() > sealed_before);
    assert!(azure.events_sent("policy-events", 0) > sent_before);

    // The cold tier holds the journal from its first entry with no gap:
    // each segment starts where the one before stopped and verifies.
    let (_, _, ledger_entries) = backlog().await;
    let store = BlobContainer::new(&azure.base, "cold-segments", AzureCredential::static_token(ic_azure::testing::TOKEN));
    let mut next = 1u64;
    while next + 3 <= ledger_entries {
        let segment = fetch_segment(&store, 0, next, next + 3)
            .await
            .unwrap_or_else(|e| panic!("segment from {next}: {e}"));
        assert_eq!(segment.len(), 4);
        next += 4;
    }
    assert!(next > 8, "fewer than two segments were sealed");
    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
