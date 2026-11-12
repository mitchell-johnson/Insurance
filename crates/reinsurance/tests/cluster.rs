//! The module as it is deployed: hosted in its own replicated group on a
//! real ledger node, fed the partitions' events by the host, with the driver
//! carrying its instructions over the cluster's own routes.

mod common;

use std::collections::BTreeMap;
use std::process::Command as Process;
use std::time::Duration;

use common::*;
use ic_core::reinsurance::cession_on;
use ic_core::{home_partition, PartyId};
use ic_product::{Frequency, Sex};
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_reinsurance::driver::{ClusterCore, ClusterModule, Core, Driver, DriverError, Module};
use ic_reinsurance::host::{module_spec, Query};
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_reinsurance::treaty::RiskFactors;
use ic_reinsurance::{Command, Envelope};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, named_life, term_life};
use serde_json::json;

fn core_envelope(key: &str, caller: &str, command: ic_core::Command) -> ic_core::Envelope {
    ic_core::Envelope {
        key: key.to_string(),
        caller: caller.to_string(),
        salt: ic_raft::client::fresh_salt(),
        traceparent: None,
        command,
    }
}

/// Purpose: on a real node the host shows the module the events of a policy issued on a
/// partition, the driver carries the cession and the premium entries back over the
/// cluster's routes, and the policy on the partition then records what was ceded.
/// Why it matters: every other test drives the module in process. This is the one that
/// proves the deployed pieces fit: the module's name and wire format, the host's event
/// feed, the outbox read through a linearizable query, and the core's two commands
/// accepted from caller `reinsurance` over the network.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosted_module_and_its_driver_cede_a_policy_on_a_real_node() {
    let dir = std::env::temp_dir().join(format!("ic-reinsurance-cluster-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
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
    };
    let node = server::start_with(settings, vec![module_spec().unwrap()]).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    let patience = Duration::from_secs(20);
    assert_eq!(client.partitions(patience).await.unwrap(), 2);
    let module = ClusterModule::new(client.clone(), patience);
    let core = ClusterCore::new(client.clone(), patience);

    // Before its first day a partition has no business date for the driver to work on.
    assert_eq!(core.business_date(0).await, Ok(None));
    let today = d(2027, 3, 1);
    for p in 0..2 {
        let open = core_envelope(&format!("open-{p}"), "leader", ic_core::Command::OpenBusinessDate { date: today });
        assert_eq!(client.propose_until(p, &open, patience).await.unwrap().rejected, None);
        for (n, cmd) in shipped_activations().unwrap().into_iter().enumerate() {
            let reply = client
                .propose_until(p, &core_envelope(&format!("config-{p}-{n}"), "config", cmd), patience)
                .await
                .unwrap();
            assert_eq!(reply.rejected, None);
        }
    }
    assert_eq!(core.business_date(1).await, Ok(Some(today)));

    let tell = |key: &str, caller: &str, command: Command| Envelope {
        key: key.to_string(),
        caller: caller.to_string(),
        on: today,
        command,
    };
    let treaty = Box::new(load_treaty("retail_life_surplus.star", RETAIL).unwrap());
    let table = Box::new(load_rate_table("ri_life_base.star", RATES).unwrap());
    assert_eq!(
        module
            .propose(tell("treaty", "config", Command::ActivateTreaty { treaty }))
            .await
            .unwrap()
            .result,
        Ok(())
    );
    assert_eq!(
        module
            .propose(tell("rates", "config", Command::ActivateRateTable { table }))
            .await
            .unwrap()
            .result,
        Ok(())
    );
    // A command from a caller the module does not allow is the module's own refusal.
    let refused = module
        .propose(tell("nope", "claims", Command::AcknowledgeInstructions { through: 0 }))
        .await
        .unwrap();
    assert_eq!(refused.result.unwrap_err().code, "caller_not_allowed");
    // One the module cannot read at all is the host's.
    let unreadable = client
        .propose_module_until("reinsurance", &json!({"not": "an envelope"}), patience)
        .await
        .unwrap();
    assert!(unreadable.result.is_err());

    let dob = d(1987, 6, 15);
    let mut issue = term_life(1, today, dob, Sex::Female, false, 800_000_00, None, Frequency::Monthly, None);
    issue.policyholder = PartyId("party-kiri".into());
    issue.lives = vec![named_life("life-1", "party-kiri", dob, Sex::Female, false)];
    let (p, id, number) = (home_partition(&issue.group) % 2, issue.policy.clone(), issue.number.clone());
    let factors = Command::RecordRiskFactors {
        policy: id.clone(),
        life: "life-1".into(),
        factors: RiskFactors {
            date_of_birth: Some(dob),
            sex: Some(Sex::Female),
            smoker: Some(false),
        },
    };
    assert_eq!(module.propose(tell("factors", "new-business", factors)).await.unwrap().result, Ok(()));
    let reply = client
        .propose_until(p, &core_envelope("issue-1", "new-business", issue_command(issue)), patience)
        .await
        .unwrap();
    assert_eq!(reply.rejected, None);

    // The host feeds the module; the driver carries what the module then issues.
    let driver = Driver { module, core, batch: 50 };
    let mut delivered = 0;
    let mut recorded = None;
    for _ in 0..200 {
        let pass = driver.deliver().await.unwrap();
        assert!(pass.clear(), "{pass:?}");
        delivered += pass.delivered;
        let view = client.policy_by_number(p, &number).await.unwrap().expect("the policy is on its partition");
        recorded = cession_on(&view.policy, "life", 1, &"life-1".into(), today).cloned();
        if recorded.is_some() && delivered >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let recorded = recorded.expect("the cession reached the policy");
    assert_eq!(
        (recorded.retained_minor, recorded.ceded.iter().map(|(_, a)| a).sum::<i64>()),
        (500_000_00, 300_000_00)
    );
    assert!(delivered >= 5, "a cession, and premium and commission for two reinsurers: {delivered}");
    let ask = |q: Query| {
        let module = &driver.module;
        async move { module.ask(q).await.unwrap() }
    };
    assert_eq!(ask(Query::Outbox { limit: 10 }).await, json!([]));
    let balance = ask(Query::Balance {
        treaty: retail(),
        reinsurer: north(),
    })
    .await;
    assert!(balance.as_i64().unwrap() > 0);
    assert_eq!(ask(Query::Reconcile).await, json!([]));
    assert_eq!(ask(Query::Exceptions).await, json!({}));
    // A question the module does not know is refused by the module, not lost.
    let unknown = client.query_module("reinsurance", &json!({"everything": {}})).await.unwrap();
    assert!(unknown.unwrap_err().starts_with("not a question of this module"));

    node.stop().await.unwrap();
    // With the node gone the driver reports the cluster as unavailable, and nothing else.
    let gone = Driver {
        module: ClusterModule::new(client.clone(), Duration::from_millis(300)),
        core: ClusterCore::new(client, Duration::from_millis(300)),
        batch: 50,
    };
    assert!(matches!(gone.deliver().await, Err(DriverError::Unavailable(_))));
    assert!(matches!(gone.core.business_date(0).await, Err(DriverError::Unavailable(_))));
    let delivery = ic_reinsurance::delivery::CoreDelivery {
        partition: 0,
        key: "k".into(),
        caller: "reinsurance",
        command: ic_core::Command::OpenBusinessDate { date: today },
    };
    assert!(matches!(gone.core.send(&delivery).await, Err(DriverError::Unavailable(_))));
    let late = gone.module.propose(tell("late", "driver", Command::AcknowledgeNotices { through: 0 })).await;
    assert!(matches!(late, Err(DriverError::Unavailable(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: the driver binary started with no settings exits with an error naming the
/// setting it needs, and one given the cluster but no token says that instead.
/// Why it matters: a driver that started on defaults would talk to no cluster and look
/// healthy. A misconfigured pod must fail where the operator can read why.
#[test]
fn the_binary_says_what_it_needs() {
    let run = |envs: &[(&str, &str)]| {
        let output = Process::new(env!("CARGO_BIN_EXE_ic-reinsurance"))
            .env_clear()
            .envs(envs.iter().copied())
            .output()
            .unwrap();
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).to_string()
    };
    assert!(run(&[]).contains("IC_NODES"));
    assert!(run(&[("IC_NODES", "127.0.0.1:1")]).contains("IC_CLUSTER_TOKEN"));
}
