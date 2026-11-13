//! Telling claims of deaths, on a real ledger node: the command the relay
//! builds is one the core takes from the customer master.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::NaiveDate;
use ic_customers::model::PartyId;
use ic_customers::relay::{self, ClusterCore, Core};
use ic_customers::store::DeathNotice;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::ClusterAuth;

/// Purpose: on a real node with two core partitions, a death is refused until the core has
/// opened a business date; then it goes to both from caller
/// `customer-master` and is accepted on each, even where the person has no claim, and the
/// same death again is accepted without effect, and so is its retraction; a cluster that cannot be reached is an
/// error, not a silent success.
/// Why it matters: claims' agreed contract is that every partition accepts the command and
/// finds its own claims; if the core refused this caller or this shape, no death recorded
/// outside claims would ever stop a benefit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_death_is_accepted_on_every_partition_of_a_real_node() {
    let dir = std::env::temp_dir().join(format!("ic-customers-relay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec!["test-cluster-token-0123456789abcdef".to_string()]).unwrap();
    let node = server::start(Settings {
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
        segment_entries: 1000,
        snapshot_every: 1000,
    })
    .await
    .unwrap();
    let core = ClusterCore::new(&addr, auth.clone(), Duration::from_secs(20)).unwrap();
    assert_eq!(core.partitions().await.unwrap(), 2);
    assert_eq!(core.partitions().await.unwrap(), 2, "remembered");
    let party = PartyId("pty_relay".into());
    let date = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
    let notice = |retract: bool, attempts: i32| DeathNotice {
        party: party.clone(),
        date_of_death: date,
        retract,
        attempts,
    };
    // Before the core opens a business date it refuses, and the relay keeps the death.
    let e = relay::send(&core, 2, &notice(false, 0)).await.unwrap_err();
    assert!(matches!(&e, relay::Failure::Refused(r) if r.contains("business_date")), "{e:?}");
    let client = ic_raft::ClusterClient::new(vec![addr.clone()]).with_auth(auth.clone());
    for partition in 0..2 {
        let open = ic_core::Envelope {
            key: format!("open-{partition}"),
            caller: "leader".into(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command: ic_core::Command::OpenBusinessDate { date },
        };
        let reply = client.propose_until(partition, &open, Duration::from_secs(20)).await.unwrap();
        assert!(reply.rejected.is_none(), "{:?}", reply.rejected);
    }
    // The core remembers the refusal under its key; the next attempt has its own.
    assert!(relay::send(&core, 2, &notice(false, 0)).await.is_err());
    relay::send(&core, 2, &notice(false, 1)).await.unwrap();
    relay::send(&core, 2, &notice(false, 2)).await.unwrap();
    let again = core
        .propose(1, &relay::key(&party, date, false, 1, 1), &relay::record_death(&party, date))
        .await
        .unwrap();
    assert_eq!(again, None, "a repeat is accepted");
    // Withdrawn: the retraction is accepted everywhere, and again.
    relay::send(&core, 2, &notice(true, 0)).await.unwrap();
    relay::send(&core, 2, &notice(true, 1)).await.unwrap();
    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let nowhere = ClusterCore::new("127.0.0.1:9", auth, Duration::from_millis(300)).unwrap();
    assert!(nowhere.partitions().await.is_err());
    assert!(matches!(relay::send(&nowhere, 1, &notice(false, 0)).await, Err(relay::Failure::Unreached(_))));
}
