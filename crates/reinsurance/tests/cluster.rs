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
/// cluster's routes, and the policy on the partition then records what was ceded. The
/// shipped binary does the same from its environment alone, and under its workload identity
/// files a statement and its release in the records container and tells staff of an
/// exception on the module's event hub.
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
        witnesses: Default::default(),
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
    assert_eq!(ask(Query::Outbox { limit: 10, after: None }).await, json!([]));
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

    // The deployed process does the same from its settings alone: a second policy on the
    // same life is ceded in full by the binary, which then stops cleanly when told to.
    let mut issue = term_life(2, today, dob, Sex::Female, false, 100_000_00, None, Frequency::Monthly, None);
    issue.policyholder = PartyId("party-kiri".into());
    issue.lives = vec![named_life("life-1", "party-kiri", dob, Sex::Female, false)];
    let (p2, id2, number2) = (home_partition(&issue.group) % 2, issue.policy.clone(), issue.number.clone());
    let factors = Command::RecordRiskFactors {
        policy: id2,
        life: "life-1".into(),
        factors: RiskFactors {
            date_of_birth: Some(dob),
            sex: Some(Sex::Female),
            smoker: Some(false),
        },
    };
    assert_eq!(driver.module.propose(tell("factors-2", "new-business", factors)).await.unwrap().result, Ok(()));
    let reply = client
        .propose_until(p2, &core_envelope("issue-2", "new-business", issue_command(issue)), patience)
        .await
        .unwrap();
    assert_eq!(reply.rejected, None);
    // Its Azure identity is a workload identity, as in the pod; Azure is a stand-in.
    let azure = ic_azure::testing::FakeAzure::start().await;
    let token_file = dir.join("federated-token");
    std::fs::write(&token_file, "a-projected-service-account-token").unwrap();
    let authority = format!("{}/", azure.base);
    // Month-end leaves it a statement to file and, the bank having paid the wrong amount,
    // an exception to tell staff of.
    let month_end = [
        (
            "month-end",
            Command::CutStatement {
                treaty: "retail-life-surplus".into(),
                reinsurer: "reinsurer-north".into(),
                period_end: d(2027, 2, 28),
            },
        ),
        (
            "reinsurance-ops",
            Command::AgreeStatement {
                number: 1,
                reference: "RE-1".into(),
            },
        ),
        (
            "finance",
            Command::InstructSettlement {
                number: 1,
                prepared_by: "kim".into(),
                approved_by: "lee".into(),
            },
        ),
        (
            "payments",
            Command::RecordSettlement {
                number: 1,
                payment_ref: "ri-stmt-1".into(),
                amount_minor: 1,
                bank_account: None,
            },
        ),
    ];
    for (n, (caller, command)) in month_end.into_iter().enumerate() {
        let applied = driver.module.propose(tell(&format!("month-end-{n}"), caller, command)).await.unwrap();
        assert_eq!(applied.result, Ok(()));
    }
    let waiting = |audience| Query::Notices {
        audience: Some(audience),
        limit: 100,
    };
    use ic_reinsurance::machine::Audience;
    assert_eq!(driver.module.ask(waiting(Audience::Records)).await.unwrap().as_array().unwrap().len(), 2);
    assert_eq!(driver.module.ask(waiting(Audience::Staff)).await.unwrap().as_array().unwrap().len(), 1);
    let mut process = Process::new(env!("CARGO_BIN_EXE_ic-reinsurance"))
        .env_clear()
        .envs([
            ("IC_NODES", addr.as_str()),
            ("IC_CLUSTER_TOKEN", "test-cluster-token-0123456789abcdef"),
            ("IC_REINSURANCE_CYCLE_MS", "50"),
            ("IC_BLOB_ENDPOINT", azure.base.as_str()),
            ("IC_EVENT_HUBS_ENDPOINT", azure.base.as_str()),
            ("AZURE_FEDERATED_TOKEN_FILE", token_file.to_str().unwrap()),
            ("AZURE_TENANT_ID", "tenant-1"),
            ("AZURE_CLIENT_ID", "client-1"),
            ("AZURE_AUTHORITY_HOST", authority.as_str()),
        ])
        .envs(std::env::vars().filter(|(k, _)| k.starts_with("LLVM_") || k.starts_with("CARGO_LLVM_COV")))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut second = None;
    for _ in 0..200 {
        let view = client
            .policy_by_number(p2, &number2)
            .await
            .unwrap()
            .expect("the second policy is on its partition");
        second = cession_on(&view.policy, "life", 1, &"life-1".into(), today).cloned();
        if second.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let second = second.expect("the driver process carried the cession");
    assert_eq!((second.retained_minor, second.ceded.iter().map(|(_, a)| a).sum::<i64>()), (0, 100_000_00));
    // And it filed the statement and its release under its own identity, and told staff.
    for _ in 0..200 {
        let left = driver.module.ask(Query::Notices { audience: None, limit: 100 }).await.unwrap();
        if left.as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        azure.blobs().keys().cloned().collect::<Vec<_>>(),
        vec![
            format!("reinsurance-records/{}", ic_reinsurance::carriers::release_record(1)),
            "reinsurance-records/reinsurance/retail-life-surplus/reinsurer-north/statement-1.json".to_string(),
        ]
    );
    assert!(azure.events_sent("reinsurance-events", 0) >= 1);
    assert!(azure.seen().iter().any(|line| line.starts_with("token ") && line.contains("client-1")));
    assert_eq!(driver.module.ask(waiting(Audience::Staff)).await.unwrap(), json!([]));
    assert!(Process::new("kill").args(["-TERM", &process.id().to_string()]).status().unwrap().success());
    assert!(process.wait().unwrap().success(), "the driver stops cleanly on SIGTERM");

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
    let late = gone.module.propose(tell("late", "driver", Command::AcknowledgeNotices { seqs: vec![] })).await;
    assert!(matches!(late, Err(DriverError::Unavailable(_))));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: the driver binary started with no settings exits with an error naming the
/// setting it needs; one given the cluster but no storage account, no token, or half an
/// Azure identity says that instead.
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
    assert!(run(&[("IC_NODES", "127.0.0.1:1")]).contains("IC_BLOB_ENDPOINT"));
    let azure = [("IC_BLOB_ENDPOINT", "http://127.0.0.1:1"), ("IC_EVENT_HUBS_ENDPOINT", "http://127.0.0.1:1")];
    let with = |more: &[(&'static str, &'static str)]| run(&[&azure[..], more].concat());
    assert!(with(&[("IC_NODES", "127.0.0.1:1")]).contains("IC_CLUSTER_TOKEN"));
    let token = ("IC_CLUSTER_TOKEN", "test-cluster-token-0123456789abcdef");
    assert!(with(&[("IC_NODES", " , "), token]).contains("no node addresses"));
    // Half a workload identity is a mistake in the pod, not a reason to try another identity.
    assert!(with(&[("IC_NODES", "127.0.0.1:1"), token, ("AZURE_FEDERATED_TOKEN_FILE", "/nowhere")]).contains("AZURE_TENANT_ID"));
}

/// Purpose: the driver binary, running with no cluster to reach, keeps running, and exits
/// cleanly when it is sent SIGINT, the signal a terminal sends, as it does for the SIGTERM
/// the cluster test sends.
/// Why it matters: a driver that died when the ledger was briefly away would need restarting
/// by hand, and one that ignored a stop signal would be killed part way through a pass at
/// every deployment. Both signals are registered before the process says it has started, so
/// neither can be missed.
#[test]
fn the_binary_outlasts_a_missing_cluster_and_stops_on_an_interrupt() {
    use std::io::{BufRead, BufReader};
    let mut process = Process::new(env!("CARGO_BIN_EXE_ic-reinsurance"))
        .env_clear()
        .envs([
            ("IC_NODES", "127.0.0.1:1"),
            ("IC_CLUSTER_TOKEN", "test-cluster-token-0123456789abcdef"),
            ("IC_BLOB_ENDPOINT", "http://127.0.0.1:1"),
            ("IC_EVENT_HUBS_ENDPOINT", "http://127.0.0.1:1"),
            ("IC_REINSURANCE_CYCLE_MS", "20"),
            ("IC_REINSURANCE_PATIENCE_MS", "50"),
        ])
        .envs(std::env::vars().filter(|(k, _)| k.starts_with("LLVM_") || k.starts_with("CARGO_LLVM_COV")))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(process.stdout.take().unwrap()).lines().map(Result::unwrap);
    assert!(lines.any(|line| line.contains("reinsurance driver started")));
    // A pass against the missing cluster fails and is logged; the process carries on.
    assert!(lines.any(|line| line.contains("could not finish")));
    assert_eq!(process.try_wait().unwrap(), None, "still running without a cluster");
    assert!(Process::new("kill").args(["-INT", &process.id().to_string()]).status().unwrap().success());
    assert!(lines.any(|line| line.contains("reinsurance driver stopped")));
    assert!(process.wait().unwrap().success(), "the driver stops cleanly on SIGINT");
}

/// Purpose: the driver loop as the process runs it, started in this process against a
/// cluster that is not there, returns cleanly when its stop future resolves, and refuses to
/// start on a node list with no addresses in it.
/// Why it matters: this is the function the binary is a thin call into. Stopping must not
/// depend on the cluster answering, or a pod could not be drained during an outage.
#[tokio::test]
async fn the_served_loop_stops_when_told_even_with_no_cluster() {
    let config = |cluster: &str| ic_reinsurance::service::Config {
        cluster: cluster.to_string(),
        cycle_every: Duration::from_millis(5),
        patience: Duration::from_millis(20),
        batch: 10,
        records_endpoint: String::new(),
        records_container: String::new(),
        event_hubs: String::new(),
        event_hub: String::new(),
        drops_endpoint: None,
        api: None,
    };
    let records = ic_azure::blob::MemoryWormStore::default;
    let staff = || ic_azure::eventhubs::EventHubsSink::new("http://127.0.0.1:1", "h", ic_azure::credential::AzureCredential::static_token("t"));
    let auth = || ClusterAuth::tokens(vec!["test-cluster-token-0123456789abcdef".to_string()]).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(ic_reinsurance::service::serve(
        config("127.0.0.1:1"),
        auth(),
        records(),
        staff(),
        ic_reinsurance::carriers::ReinsurerDrops::Off,
        async {
            let _ = stopped.await;
        },
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!served.is_finished(), "the loop runs until it is told to stop");
    stop.send(()).unwrap();
    assert_eq!(served.await.unwrap(), Ok(()));
    let nowhere = ic_reinsurance::service::serve(
        config(","),
        auth(),
        records(),
        staff(),
        ic_reinsurance::carriers::ReinsurerDrops::Off,
        std::future::ready(()),
    )
    .await;
    assert_eq!(nowhere, Err("no node addresses given".to_string()));
}
