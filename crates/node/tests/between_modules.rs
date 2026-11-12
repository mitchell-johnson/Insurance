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
/// administration module hosted on the same node, and its facultative answer by the
/// underwriting module, each of which answers in its own words: here they refuse, because
/// no such scheme or reservation exists, and a repeat gets the same refusal. A settlement
/// due to a reinsurer becomes one payment waiting in the payments module, and that module's
/// word that it settled or came back is read by reinsurance.
/// Why it matters: the two modules are built apart and share only JSON. If a field were
/// named differently the group module could not read the command at all, and every excess
/// over the retention on a scheme would wait for underwriting that never starts. A refusal
/// that names the policy proves the command was read and understood.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn other_modules_read_reinsurance_notices_and_it_reads_theirs() {
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
    assert_eq!(audiences.reached(), vec![Audience::Group, Audience::Underwriting, Audience::Payments]);

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
    // Underwriting reads the facultative answer the same way, offered or declined.
    for offered in [true, false] {
        let terms = Notice::FacultativeTermsRecorded {
            reservation_id: format!("P-9/life-1/life/{}", u8::from(offered)),
            offered,
            reference: "FAC-1".into(),
        };
        match audiences.hand_on(9, &terms, on).await.unwrap() {
            Handed::Refused(refusal) => assert_eq!(refusal.code, "unknown_reservation", "{}", refusal.message),
            Handed::Taken => panic!("underwriting holds no such reservation"),
        }
    }
    // Payments keeps a settlement due to a reinsurer as a payment waiting for a maker,
    // once however often it is offered. Money due from a reinsurer is not a payment.
    let due = |statement: u32, net_minor: i64| Notice::SettlementDue {
        statement,
        treaty: "retail".into(),
        reinsurer: "north-re".into(),
        currency: ic_money::Currency::NZD,
        net_minor,
    };
    for _ in 0..2 {
        assert_eq!(audiences.hand_on(11, &due(7, 90_000_00), on).await.unwrap(), Handed::Taken);
    }
    assert_eq!(audiences.hand_on(12, &due(8, -4_00), on).await.unwrap(), Handed::Taken);
    let waiting = client.query_module("payments", &serde_json::json!({"q": "waiting"})).await.unwrap().unwrap();
    assert_eq!(
        waiting,
        serde_json::json!([{"payment": "ri-stmt-7", "source": "reinsurance:statement:7", "payee": "north-re", "amount_minor": 90_000_00}])
    );
    // What payments says back about a settlement is read by the reinsurance module,
    // which here knows no such statement.
    for command in [
        serde_json::json!({"record_settlement": {"number": 7, "payment_ref": "ri-stmt-7", "amount_minor": 90_000_00}}),
        serde_json::json!({"settlement_returned": {"number": 7, "payment_ref": "ri-stmt-7", "reason": "account closed"}}),
    ] {
        let told = serde_json::json!({"key": format!("payments:{command}"), "caller": "payments", "on": on, "command": command});
        let reply = client.propose_module_until("reinsurance", &told, Duration::from_secs(20)).await.unwrap();
        assert_eq!(reply.result.unwrap()["result"]["Err"]["code"], "unknown_statement");
    }
    // A notice for someone else is never sent to either.
    let other = Notice::ExceptionClosed { id: "x".into() };
    assert!(matches!(audiences.hand_on(8, &other, on).await, Err(DriverError::Answer(_))));

    node.stop().await.unwrap();
    let gone = ClusterAudiences::new(client, Duration::from_millis(300));
    assert!(matches!(gone.hand_on(7, &notice, on).await, Err(DriverError::Unavailable(_))));
    let _ = std::fs::remove_dir_all(&dir);
}
