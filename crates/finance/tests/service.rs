//! The close service against a real node and the Azure stand-in.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::*;
use ic_azure::blob::BlobContainer;
use ic_azure::credential::AzureCredential;
use ic_azure::keyvault::DevSigner;
use ic_azure::merkle::publish_root;
use ic_azure::testing::{FakeAzure, TOKEN};
use ic_core::Command;
use ic_finance::driver::{Books as _, Driver, DriverError, Modules as _};
use ic_finance::service::{run_passes, ClusterBooks, ClusterModules, Config};
use ic_finance::{FinCommand, FinEnvelope};
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_sim::config::shipped_activations;
use serde_json::json;

/// Purpose: the service reads its settings from the environment, refuses to start without
/// the cluster or either storage account, defaults the containers and the timer, and
/// refuses a timer that is not a positive number of seconds.
/// Why it matters: a close service that started with no storage configured would look
/// healthy for a month and then have nowhere to put the extracts on the day they are due.
#[test]
fn the_service_refuses_to_start_half_configured() {
    let full = BTreeMap::from([
        ("IC_NODES", "n1:7000, n2:7000"),
        ("IC_FINANCE_ROOTS_ENDPOINT", " https://stledger.blob.core.windows.net "),
        ("IC_FINANCE_EXTRACTS_ENDPOINT", "https://stfinance.blob.core.windows.net/"),
    ]);
    let read = |vars: &BTreeMap<&str, &str>| Config::from_lookup(|key| vars.get(key).map(|v| v.to_string()));
    let config = read(&full).unwrap();
    assert_eq!(config.cluster, "n1:7000, n2:7000");
    assert_eq!(config.roots_endpoint, "https://stledger.blob.core.windows.net");
    assert_eq!(
        (config.roots_container.as_str(), config.extracts_container.as_str()),
        ("merkle-roots", "finance-extracts")
    );
    assert_eq!(config.pass_every, Duration::from_secs(60));
    assert_eq!(config.location(), "https://stfinance.blob.core.windows.net/finance-extracts");

    for (missing, named) in [
        ("IC_NODES", "IC_CLUSTER_NODES"),
        ("IC_FINANCE_ROOTS_ENDPOINT", "IC_FINANCE_ROOTS_ENDPOINT"),
        ("IC_FINANCE_EXTRACTS_ENDPOINT", "IC_FINANCE_EXTRACTS_ENDPOINT"),
    ] {
        let mut vars = full.clone();
        vars.insert(missing, "  ");
        assert_eq!(read(&vars).unwrap_err(), format!("{named} is not set"));
    }
    let mut vars = full.clone();
    vars.extend([
        ("IC_CLUSTER_NODES", "a:1"),
        ("IC_FINANCE_PASS_SECONDS", "5"),
        ("IC_FINANCE_EXTRACTS_CONTAINER", "x"),
    ]);
    let config = read(&vars).unwrap();
    assert_eq!(
        (config.cluster.as_str(), config.pass_every, config.extracts_container.as_str()),
        ("a:1", Duration::from_secs(5), "x")
    );
    for bad in ["0", "soon"] {
        vars.insert("IC_FINANCE_PASS_SECONDS", bad);
        assert!(read(&vars).unwrap_err().starts_with("IC_FINANCE_PASS_SECONDS must be"));
    }
}

/// Purpose: a real node hosts the finance ledger. Through the cluster the ledger is
/// configured, a refusal comes back as the ledger's own reason, questions are answered, and
/// the node's event feed brings a closed business day into the books. The shipped binary,
/// started from its environment alone, finds that day's Merkle root in Blob Storage,
/// records it, and stops on SIGTERM. With the node gone, a pass fails as unavailable.
/// Why it matters: the tests of the driver use stand-ins for the ledger and for storage.
/// This is the deployed arrangement: JSON through Raft, the cluster client's routing, a
/// workload identity and the Blob protocol. It fails if the close cannot run where it is
/// hosted, and a binary that ignored SIGTERM would be killed mid-pass on every rollout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_close_service_runs_against_a_real_node() {
    let dir = std::env::temp_dir().join(format!("ic-finance-service-{}", std::process::id()));
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
        segment_entries: 1000,
        snapshot_every: 1000,
    };
    let node = server::start_with(settings, vec![ic_finance::host::module_spec().unwrap()]).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    let patience = Duration::from_secs(20);
    let books = ClusterBooks::new(client.clone(), patience);
    let modules = ClusterModules::new(client.clone(), 1);

    // The ledger, through the cluster.
    let envelope = |key: &str, caller: &str, actor: Option<&str>, command: FinCommand| FinEnvelope {
        key: key.to_string(),
        caller: caller.to_string(),
        actor: actor.map(str::to_string),
        roles: actor.map(roles).unwrap_or_default(),
        command,
    };
    let configure = || FinCommand::Configure {
        partitions: 1,
        year_end_month: 3,
    };
    assert_eq!(books.propose(envelope("c1", "operator", None, configure())).await.unwrap(), Ok(()));
    let again = books.propose(envelope("c2", "operator", None, configure())).await.unwrap().unwrap_err();
    assert_eq!(again.code, "configured");
    let chart = chart();
    let id = format!("chart:{}:{}", chart.version, chart.content_hash);
    for (key, actor, command) in [
        ("c3", "olive", FinCommand::ProposeChart { chart }),
        ("c4", "lena", FinCommand::ApproveConfig { id }),
    ] {
        assert_eq!(books.propose(envelope(key, "finance-console", Some(actor), command)).await.unwrap(), Ok(()));
    }
    assert_eq!(books.ask(json!({ "q": "status" })).await.unwrap()["configured"], json!(true));
    assert!(matches!(books.ask(json!({ "q": "nonsense" })).await, Err(DriverError::Answer(_))));
    assert!(modules.ask("no-such-module", json!("x")).await.is_err());
    // The core's partitions, asked together under one name. Nothing is waiting to be invested.
    assert_eq!(
        modules.ask("core", json!({ "waiting_cash": { "as_at": "2027-11-30" } })).await.unwrap(),
        json!({ "as_at": "2027-11-30", "items": [], "total_minor": 0, "count": 0 })
    );
    assert!(matches!(modules.ask("core", json!("anything else")).await, Err(DriverError::Answer(_))));

    // The core closes one business day.
    let (day, next) = (d(2027, 11, 15), d(2027, 11, 16));
    let mut setup = vec![("leader", Command::OpenBusinessDate { date: day })];
    setup.extend(shipped_activations().unwrap().into_iter().map(|c| ("config", c)));
    setup.push(("leader", Command::AdvanceBusinessDate { to: next }));
    for (n, (who, command)) in setup.into_iter().enumerate() {
        let envelope = ic_core::Envelope {
            key: format!("setup-{n}"),
            caller: who.to_string(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command,
        };
        assert_eq!(client.propose_until(0, &envelope, patience).await.unwrap().rejected, None);
    }
    let wanted = || async { books.ask(json!({ "q": "work" })).await.unwrap()["roots_wanted"].clone() };
    let mut tries = 0;
    while wanted().await != json!([day]) {
        tries += 1;
        assert!(tries < 600, "the closed day did not reach the ledger");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The core's publisher: the day's signed root, in Blob Storage.
    let fake = FakeAzure::start().await;
    let credential = AzureCredential::static_token(TOKEN);
    let roots = BlobContainer::new(&fake.base, "merkle-roots", credential.clone());
    let sources = books.ask(json!({ "q": "sources", "period": "2027-11" })).await.unwrap();
    let head: ic_ledger::Hash32 = serde_json::from_value(sources[0]["closing_head"].clone()).unwrap();
    publish_root(&DevSigner, &roots, day, &BTreeMap::from([(0, head)])).await.unwrap();

    // The shipped binary.
    let binary = || {
        let mut p = std::process::Command::new(env!("CARGO_BIN_EXE_ic-finance-close"));
        p.env_clear()
            .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        p
    };
    let refused = binary().output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("IC_CLUSTER_NODES"));

    let token_file = dir.join("federated-token");
    std::fs::write(&token_file, "a-projected-service-account-token").unwrap();
    let mut child = binary()
        .env("IC_NODES", &addr)
        .env("IC_CLUSTER_TOKEN", "test-cluster-token-0123456789abcdef")
        .env("AZURE_FEDERATED_TOKEN_FILE", &token_file)
        .env("AZURE_TENANT_ID", "tenant-1")
        .env("AZURE_CLIENT_ID", "client-1")
        .env("AZURE_AUTHORITY_HOST", format!("{}/", fake.base))
        .env("IC_FINANCE_ROOTS_ENDPOINT", &fake.base)
        .env("IC_FINANCE_EXTRACTS_ENDPOINT", &fake.base)
        .env("IC_FINANCE_PASS_SECONDS", "1")
        .spawn()
        .unwrap();
    let mut tries = 0;
    while wanted().await != json!([]) {
        assert!(child.try_wait().unwrap().is_none(), "ic-finance-close exited at start");
        tries += 1;
        assert!(tries < 1200, "the binary did not record the root");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let mut tries = 0;
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        tries += 1;
        assert!(tries < 600, "ic-finance-close did not stop on SIGTERM");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(exit.success());

    // The same driver in this process: nothing is left to do, and the timer loop stops when told.
    let driver = Driver {
        books,
        modules,
        roots,
        extracts: BlobContainer::new(&fake.base, "finance-extracts", credential),
        location: fake.base.clone(),
        sources: vec![],
    };
    let mut passes = Vec::new();
    run_passes(&driver, Duration::from_millis(20), tokio::time::sleep(Duration::from_millis(200)), |p| {
        passes.push(p.clone())
    })
    .await;
    assert!(passes.len() >= 2, "the timer ran {} passes", passes.len());
    assert!(passes.iter().all(|p| p == &Ok(ic_finance::driver::Pass::default())));

    node.stop().await.unwrap();
    let gone = Driver {
        books: ClusterBooks::new(client, Duration::from_millis(300)),
        ..driver
    };
    let mut seen = Vec::new();
    run_passes(&gone, Duration::from_secs(60), async {}, |p| seen.push(p.clone())).await;
    assert!(matches!(seen[0], Err(DriverError::Unavailable(_))), "{:?}", seen[0]);
    let _ = std::fs::remove_dir_all(&dir);
}
