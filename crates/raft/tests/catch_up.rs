//! Three ledger nodes in this process, talking to each other over real HTTP
//! with the cluster token, where one falls so far behind that the log it
//! needs is gone.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{d, scratch, start_date};
use ic_core::{Command, Envelope};
use ic_product::{Frequency, Sex};
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient, NodeId};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};

const TOKEN: &str = "test-cluster-token-0123456789abcdef";

/// Purpose: a node that was down while its peers wrote, snapshotted and dropped the log
/// behind the snapshot is brought level again over HTTP: it receives the snapshot, then
/// the entries after it, and ends with the same state hash as the others. It then takes
/// part again: with another node stopped, the group still accepts a command.
/// Why it matters: this is every restart after a long outage, and every replacement node,
/// in production. The entries the node missed no longer exist as log, so if the snapshot
/// could not cross the network the node could never rejoin, and the group would be one
/// failure from stopping.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_that_fell_behind_the_log_is_caught_up_by_snapshot_over_http() {
    let dir = scratch("catch-up");
    let peers: BTreeMap<NodeId, String> = (1..=3u64)
        .map(|id| {
            let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            (id, format!("127.0.0.1:{port}"))
        })
        .collect();
    let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
    let settings = |id: NodeId| Settings {
        id,
        listen: peers[&id].parse().unwrap(),
        data_dir: dir.join(format!("node-{id}")),
        partitions: 1,
        peers: peers.clone(),
        bootstrap: id == 1,
        heartbeat_ms: 50,
        auth: auth.clone(),
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 10,
        snapshot_every: 15,
    };
    let mut running = BTreeMap::new();
    for id in 1..=3 {
        running.insert(id, server::start(settings(id)).await.unwrap());
    }
    let client = ClusterClient::new(peers.values().cloned().collect())
        .with_attempt_timeout(Duration::from_secs(2))
        .with_auth(auth.clone());
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
            let reply = client.propose_until(0, &envelope, Duration::from_secs(30)).await.unwrap();
            assert_eq!(reply.rejected, None);
        }
    };
    let issue = |n: u64| {
        let application = term_life(n, start_date(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        run(format!("issue-{n}"), "new-business", issue_command(application))
    };
    let status = |id: NodeId| {
        let client = client.clone();
        let addr = peers[&id].clone();
        async move { client.status(&addr).await.unwrap().remove(0) }
    };

    run("open".to_string(), "leader", Command::OpenBusinessDate { date: start_date() }).await;
    for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
        run(format!("activate-{n}"), "config", activation).await;
    }
    issue(1).await;

    // Node 3 goes down. The others write well past a snapshot and drop the log behind it.
    running.remove(&3).unwrap().kill().await;
    for n in 2..=40 {
        issue(n).await;
    }
    let survivor = if status(1).await.role == "leader" { 1 } else { 2 };
    // Node 3 stopped near entry 12. Snapshots are written in the background:
    // wait until the survivors hold nothing that old.
    let behind_at = 20;
    let mut purged = false;
    for _ in 0..100 {
        if status(survivor).await.first_log_index.is_some_and(|first| first > behind_at) {
            purged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(purged, "the log was not purged behind a snapshot: {:?}", status(survivor).await);

    running.insert(3, server::start(settings(3)).await.unwrap());
    let mut level = false;
    for _ in 0..300 {
        let (one, three) = (status(survivor).await, status(3).await);
        if three.last_applied == one.last_applied && three.state_hash == one.state_hash {
            assert_eq!(three.policies, 40);
            assert_eq!(three.ledger_entries, one.ledger_entries);
            level = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(level, "node 3 was not caught up: {:?} against {:?}", status(3).await, status(survivor).await);

    // It counts again: with one of the others gone, there is still a majority.
    running.remove(&survivor).unwrap().kill().await;
    issue(41).await;
    assert_eq!(status(3).await.policies, 41);
    for (_, node) in running {
        node.stop().await.unwrap();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: a ledger node's Raft endpoints turn away a caller with the cluster token but no
/// node identity, a caller that expects this node to be a different incarnation, and a
/// caller claiming a node number this node already knows under another identity. A caller
/// with no token is turned away before any of that.
/// Why it matters: Raft messages decide who leads and what is committed. A node rebuilt on
/// an empty disk that kept its old number could vote twice in one term; these refusals are
/// what stop it, and they sit on the network path where the only other test of them is
/// the container drill.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raft_endpoints_refuse_callers_they_cannot_place() {
    let dir = scratch("raft-door");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
    let node = server::start(Settings {
        id: 1,
        listen: addr.parse().unwrap(),
        data_dir: dir.join("node-1"),
        partitions: 1,
        peers: BTreeMap::from([(1, addr.clone()), (2, "127.0.0.1:1".to_string())]),
        bootstrap: true,
        heartbeat_ms: 50,
        auth,
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 10,
        snapshot_every: 1000,
    })
    .await
    .unwrap();
    let http = reqwest::Client::new();
    let vote = |headers: Vec<(&'static str, &'static str)>, token: bool| {
        let mut request = http.post(format!("http://{addr}/raft/0/vote")).json(&serde_json::json!({}));
        if token {
            request = request.bearer_auth(TOKEN);
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        async move {
            let response = request.send().await.unwrap();
            (response.status().as_u16(), response.text().await.unwrap())
        }
    };

    assert_eq!(vote(vec![("x-ic-node", "2"), ("x-ic-incarnation", "first")], false).await.0, 401);
    let (status, said) = vote(vec![], true).await;
    assert_eq!(status, 400);
    assert!(said.contains("identify"), "{said}");

    // Node 2 as first met: let in as far as the handler, which rejects the empty body.
    let (status, _) = vote(vec![("x-ic-node", "2"), ("x-ic-incarnation", "first")], true).await;
    assert!(status == 422 || status == 400, "a known peer was stopped at the door: {status}");
    // The same number under another identity: an impostor, or a wiped disk.
    let (status, said) = vote(vec![("x-ic-node", "2"), ("x-ic-incarnation", "second")], true).await;
    assert_eq!(status, 409, "{said}");
    // A caller that remembers this node as someone else.
    let (status, said) = vote(vec![("x-ic-node", "2"), ("x-ic-incarnation", "first"), ("x-ic-expects", "not-this-node")], true).await;
    assert_eq!(status, 409);
    assert!(said.contains("not the member"), "{said}");
    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
