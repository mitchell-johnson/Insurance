//! The service against a real node: the module hosted in its own replicated
//! group beside a partition, reached over HTTP with the cluster token.

#[path = "../../payments/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use ic_azure::blob::MemoryWormStore;
use ic_azure::credential::AzureCredential;
use ic_azure::testing::FakeAzure;
use ic_cashmatch::{IncomingPayment, Matcher};
use ic_core::Envelope as CoreEnvelope;
use ic_money::Money;
use ic_payments::model::RefundDestination;
use ic_payments::{Command, Envelope};
use ic_payments_service::bank::HttpBank;
use ic_payments_service::fake::FakeBank;
use ic_payments_service::ports::{ClusterCore, Core, Keys, LogNotifier, Module, VaultKeys};
use ic_payments_service::pump::Pump;
use ic_product::{Frequency, Sex};
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use serde_json::json;

const PATIENCE: Duration = Duration::from_secs(30);

async fn core_run(client: &ClusterClient, key: &str, caller: &str, command: ic_core::Command) {
    let envelope = CoreEnvelope {
        key: key.to_string(),
        caller: caller.to_string(),
        salt: fresh_salt(),
        traceparent: None,
        command,
    };
    let reply = client.propose_until(0, &envelope, PATIENCE).await.unwrap();
    assert_eq!(reply.rejected, None);
}

/// Purpose: the module is hosted by a real node under its own name; a
/// person's commands and the pump's reach it through the cluster client; the
/// pump finds the policy's partition, has the core raise a refund, and the
/// module hears the answer; a credit is offered to every partition for
/// matching; a command about a policy nobody holds is refused as such; and
/// signing goes through Key Vault.
/// Why it matters: everything else is tested against stand-ins for the
/// cluster. This is the proof that the real wiring (module host, cluster
/// client, partition lookup, idempotent proposals) carries the same flow.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_service_works_against_a_real_node() {
    let dir = std::env::temp_dir().join(format!("ic-payments-cluster-{}", std::process::id()));
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
    let node = server::start_with(settings, vec![ic_payments_service::module_spec().unwrap()]).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()])
        .with_auth(auth)
        .with_attempt_timeout(Duration::from_secs(2));

    // A policy that has overpaid by 100.00.
    core_run(&client, "open", "leader", ic_core::Command::OpenBusinessDate { date: d(2027, 11, 15) }).await;
    for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
        core_run(&client, &format!("activate-{n}"), "config", activation).await;
    }
    let issue = term_life(
        1,
        d(2027, 11, 15),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    let policy = issue.policy.clone();
    core_run(&client, "issue", "new-business", issue_command(issue)).await;
    let paid = ic_core::Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: "first".into(),
        amount: Money::new(61_84 + 100_00, NZD),
    };
    core_run(&client, "first", "cash-matching", paid).await;

    // People reach the module through the same client.
    let bank = FakeBank::start(&["token-akl"]).await.unwrap();
    let mut cfg = config();
    for channel in cfg.channels.values_mut() {
        for endpoint in channel.endpoints.values_mut() {
            endpoint.url = bank.url.clone();
            endpoint.credential = "bank".to_string();
        }
    }
    let mut sent = 0;
    let mut tell = |who: &ic_payments::Actor, command: Command| {
        sent += 1;
        Envelope {
            key: format!("person-{sent}"),
            actor: who.clone(),
            at: monday(),
            command,
        }
    };
    let mut asked = Err("never asked".to_string());
    for _ in 0..100 {
        asked = Module::query(&client, json!({"q": "submission_in_progress"})).await;
        if asked.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(asked, Ok(json!(false)));
    Module::propose(
        &client,
        tell(
            &cal(),
            Command::ActivateConfig {
                version: 1,
                config: Box::new(cfg),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let create = Command::CreateAuthority {
        authority: "a1".to_string(),
        policy: policy.clone(),
        party: "payer".to_string(),
        account: account("Ana Payer", "0101234567"),
        sole_name: false,
    };
    Module::propose(&client, tell(&pam(), create)).await.unwrap().unwrap();
    let refund = |id: &str, amount| Command::RequestRefund {
        refund: id.to_string(),
        policy: policy.clone(),
        amount_minor: amount,
        currency: NZD,
        destination: RefundDestination::Source { authority: "a1".to_string() },
    };
    Module::propose(&client, tell(&mia(), refund("rf1", 100_00))).await.unwrap().unwrap();
    Module::propose(&client, tell(&mia(), refund("rf2", 500_00))).await.unwrap().unwrap();
    // A refusal by the module comes back as the module's own answer.
    let refused = Module::propose(&client, tell(&mia(), refund("rf1", 100_00))).await.unwrap();
    assert_eq!(refused.unwrap_err().code, "duplicate_refund");

    let core = ClusterCore {
        client: client.clone(),
        matcher: Matcher {
            auto_threshold: 90,
            amount_tolerance_minor: 0,
            date_tolerance_days: 3,
            authorities: BTreeMap::new(),
        },
    };
    let pump = Pump {
        module: client.clone(),
        core: core.clone(),
        bank: HttpBank::default(),
        vault: MemoryWormStore::default(),
        keys: VaultKeys {
            vault: "http://127.0.0.1:1".to_string(),
            credential: AzureCredential::static_token("t"),
        },
        secrets: BTreeMap::from([("bank".to_string(), "token-akl".to_string())]),
        notifier: LogNotifier,
        region: "akl".to_string(),
        instance: "test".to_string(),
        clock: Arc::new(monday),
        grace: chrono::Duration::minutes(10),
        cursors: Mutex::default(),
    };
    let pass = pump.pass().await;
    assert_eq!((pass.instructions, pass.problems.len()), (2, 0), "{:?}", pass.problems);
    // The ledger owed 100.00 and no more: one refund is raised, one refused.
    let status = |id: &'static str| {
        let client = client.clone();
        async move { Module::query(&client, json!({"q": "refund", "refund": id})).await.unwrap()["status"].clone() }
    };
    assert_eq!(status("rf1").await, json!("raised"));
    assert_eq!(status("rf2").await, json!({"refused": "insufficient_suspense"}));

    // A credit nobody recognises is offered to the partition and comes back unmatched.
    let stray = IncomingPayment {
        id: "stray-1".to_string(),
        amount: Money::new(12_34, NZD),
        reference: Some("no such policy".to_string()),
        payer_account: None,
        received_on: d(2027, 11, 15),
    };
    assert!(core.match_receipt(stray).await.unwrap().is_some());
    let nobody = ic_core::Command::ReturnPayment {
        policy: serde_json::from_value(json!("pol-nobody")).unwrap(),
        payment: "x".to_string(),
    };
    assert_eq!(core.send("k1", "payments", nobody).await.unwrap().as_deref(), Some("unknown_policy"));
    let other = ic_core::Command::OpenBusinessDate { date: d(2027, 11, 16) };
    assert_eq!(core.send("k2", "payments", other).await.unwrap().as_deref(), Some("not_a_payments_command"));

    // Signing goes to Key Vault under the name configured for the endpoint.
    let azure = FakeAzure::start().await;
    let token_file = dir.join("federated-token");
    std::fs::write(&token_file, "projected-token").unwrap();
    let keys = VaultKeys {
        vault: azure.base.clone(),
        credential: AzureCredential::workload_identity(&azure.base, "tenant-1", "client-payments", token_file.to_str().unwrap()),
    };
    let signature = keys.sign("anz-file-akl-signing", b"file").await.unwrap();
    assert!(signature.key_id.contains("anz-file-akl-signing"));
    assert!(pump.keys.sign("k", b"file").await.is_err(), "a vault that is not there signs nothing");

    assert!(azure.seen().iter().any(|line| line == "sign anz-file-akl-signing"));

    node.stop().await.unwrap();
    // With the node gone, the module and the core both say so.
    let gone = ClusterClient::new(vec![addr]).with_attempt_timeout(Duration::from_millis(200));
    assert!(Module::query(&gone, json!({"q": "outbox"})).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
