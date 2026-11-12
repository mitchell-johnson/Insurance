//! Modules talking to each other on the deployed node, each in its own
//! replicated group, in the wire format the other one publishes.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::NaiveDate;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_reinsurance::driver::{Audiences, ClusterAudiences, DriverError, Handed};
use ic_reinsurance::machine::Audience;
use ic_reinsurance::Notice;

/// Purpose: the reinsurance driver's underwritten-excess notice is read by the group
/// administration module hosted on the same node, which answers in its own words: here it
/// refuses, because no such scheme is registered, and a repeat gets the same refusal.
/// Why it matters: the two modules are built apart and share only JSON. If a field were
/// named differently the group module could not read the command at all, and every excess
/// over the retention on a scheme would wait for underwriting that never starts. A refusal
/// that names the policy proves the command was read and understood.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_administration_reads_the_reinsurance_notice() {
    let dir = std::env::temp_dir().join(format!("ic-node-between-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec!["test-cluster-token-0123456789abcdef".to_string()]).unwrap();
    let settings = Settings {
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
        segment_entries: 10,
        snapshot_every: 1000,
    };
    let node = server::start_with(settings, ic_node::modules().unwrap()).await.unwrap();
    let client = ClusterClient::new(vec![addr]).with_auth(auth);
    let audiences = ClusterAudiences::new(client.clone(), Duration::from_secs(20));
    assert_eq!(audiences.reached(), vec![Audience::Group]);

    let on = NaiveDate::from_ymd_opt(2027, 3, 1).unwrap();
    let notice = Notice::RequireUnderwrittenExcess {
        policy: ic_core::PolicyId("S1".into()),
        life: ic_core::LifeId("M1".into()),
        cover: "group-life".into(),
        excess_minor: 200_000_00,
        reference: "UWX/S1/group-life/0/M1/20000000".into(),
    };
    for _ in 0..2 {
        match audiences.hand_on(7, &notice, on).await.unwrap() {
            Handed::Refused(refusal) => {
                assert_eq!(refusal.code, "unknown_policy");
                assert!(refusal.message.contains("S1"), "{}", refusal.message);
            }
            Handed::Taken => panic!("no scheme is registered, so the notice cannot have been taken"),
        }
    }
    // A notice for someone else is never sent to group administration.
    let other = Notice::ExceptionClosed { id: "x".into() };
    assert!(matches!(audiences.hand_on(8, &other, on).await, Err(DriverError::Answer(_))));

    node.stop().await.unwrap();
    let gone = ClusterAudiences::new(client, Duration::from_millis(300));
    assert!(matches!(gone.hand_on(7, &notice, on).await, Err(DriverError::Unavailable(_))));
    let _ = std::fs::remove_dir_all(&dir);
}
