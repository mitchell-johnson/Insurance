//! The deployed node: the binary that hosts every module.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};

/// Purpose: a node started with the deployed list of modules forms a group for each of
/// them beside its partitions, every one elects a leader, no two modules share a name,
/// and the node stops cleanly.
/// Why it matters: this list is the only place a module is attached to the node that is
/// deployed. A module left off it runs in tests and nowhere else; two modules with one name
/// would share a directory on disk. Either mistake would otherwise first be seen in a
/// deployed cluster.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_deployed_node_hosts_every_module() {
    let modules = ic_node::modules().unwrap();
    let mut names: Vec<String> = modules.iter().map(|m| m.name.clone()).collect();
    names.sort();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(names, unique, "two modules share a name");
    assert!(names.contains(&"channels".to_string()));

    let dir = std::env::temp_dir().join(format!("ic-node-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec!["test-cluster-token-0123456789abcdef".to_string()]).unwrap();
    let settings = Settings {
        id: 1,
        listen: addr.parse().unwrap(),
        data_dir: dir.join("node-1"),
        partitions: 2,
        peers: BTreeMap::from([(1, addr.clone())]),
        bootstrap: true,
        heartbeat_ms: 50,
        auth: auth.clone(),
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 10,
        snapshot_every: 1000,
        witnesses: Default::default(),
    };
    let node = server::start_with(settings, modules).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    let mut led = Vec::new();
    for _ in 0..100 {
        let statuses = client.module_status(&addr).await.unwrap();
        led = statuses.iter().filter(|s| s.role == "leader").map(|s| s.module.clone()).collect();
        if led.len() == names.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(led, names, "not every module's group elected a leader");
    assert_eq!(client.status(&addr).await.unwrap().len(), 2);
    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: the node binary started with no settings exits with an error that names the
/// first setting it needs, and does not start listening.
/// Why it matters: a node that started on defaults would join no cluster, or the wrong
/// one. A misconfigured pod must fail where the operator can read why.
#[test]
fn the_binary_says_what_it_needs() {
    let output = Command::new(env!("CARGO_BIN_EXE_ic-node")).env_clear().output().unwrap();
    assert!(!output.status.success());
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.contains("IC_CLUSTER") || said.contains("IC_NODE_ID"), "{said}");
}

/// A local port that no other test in this process has been given: two
/// nodes starting at the same moment must not be handed the same one.
fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<std::collections::BTreeSet<u16>> = std::sync::Mutex::new(std::collections::BTreeSet::new());
    let mut given = GIVEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        if given.insert(port) {
            return port;
        }
    }
}
