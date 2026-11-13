//! The driver as it is deployed: its settings, its connections to a real
//! node hosting the underwriting, reinsurance, group and channel modules and the
//! core, and the shipped binary.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::*;
use ic_core::PolicyStatus;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_sim::config::shipped_activations;
use ic_underwriting::command::Command;
use ic_underwriting::driver::{DriverError, NothingConnected, Underwriting};
use ic_underwriting::model::{Entry, Proposal, ProposalStatus};
use ic_underwriting::service::{connect, serve, Cluster, Config};
use ic_underwriting::special::Query;

const RETAIL: &str = include_str!("../../../treaties/retail_life_surplus.star");
const RATES: &str = include_str!("../../../treaties/rates/ri_life_base.star");
const TOKEN: &str = "test-cluster-token-0123456789abcdef";

/// Collections as far as this driver uses it: the list it is given, answered as it was given.
/// Until a list is set it cannot answer, as when collections is behind.
#[derive(Default)]
struct Collections(Option<serde_json::Value>);

impl ic_raft::module::ModuleState for Collections {
    fn apply(&mut self, _index: u64, command: serde_json::Value) -> Result<serde_json::Value, String> {
        self.0 = Some(command);
        Ok(serde_json::Value::Null)
    }
    fn query(&self, _question: serde_json::Value) -> Result<serde_json::Value, String> {
        self.0.clone().ok_or_else(|| "no list yet".to_string())
    }
    fn save(&self) -> Result<serde_json::Value, String> {
        Ok(self.0.clone().unwrap_or_default())
    }
    fn load(&mut self, saved: serde_json::Value) -> Result<(), String> {
        self.0 = (!saved.is_null()).then_some(saved);
        Ok(())
    }
}

fn clock() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2027-02-01T09:00:00Z").unwrap().with_timezone(&Utc)
}

/// Purpose: the driver's settings come from the environment with sensible defaults; the nodes'
/// addresses and the Event Hubs namespace are required; and a pause, patience, batch or sweep page that is not a whole
/// number in range is refused by name, with the value that was given.
/// Why it matters: a driver that started with a zero pause would spin against the cluster, and
/// one that started with a sweep page the module refuses would never lapse a proposal or chase
/// evidence. Refusing at start, naming the setting, turns a silent fault into a failed rollout.
#[test]
fn settings_have_defaults_and_bad_ones_are_refused_by_name() {
    let from = |pairs: &[(&str, &str)]| {
        let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Config::from_lookup(|key| map.get(key).cloned())
    };
    const HUBS: (&str, &str) = ("IC_EVENT_HUBS_ENDPOINT", "https://hubs.example");
    assert_eq!(
        from(&[("IC_NODES", " n1:7000,n2:7000 "), HUBS]).unwrap(),
        Config {
            cluster: "n1:7000,n2:7000".into(),
            cycle_every: Duration::from_millis(2_000),
            patience: Duration::from_millis(10_000),
            batch: 100,
            sweep_page: 200,
            event_hubs: "https://hubs.example".into(),
            event_hub: "underwriting-notices".into(),
            evidence_hub: "underwriting-evidence".into(),
            delegates_hub: "underwriting-delegates".into(),
        }
    );
    let set = from(&[
        ("IC_NODES", "n1:7000"),
        HUBS,
        ("IC_UNDERWRITING_EVENT_HUB", "uw-staff"),
        ("IC_UNDERWRITING_CYCLE_MS", "250"),
        ("IC_UNDERWRITING_PATIENCE_MS", "500"),
        ("IC_UNDERWRITING_BATCH", "7"),
        ("IC_UNDERWRITING_SWEEP_PAGE", "1000"),
    ])
    .unwrap();
    assert_eq!(
        (set.cycle_every, set.patience, set.batch, set.sweep_page, set.event_hub.as_str()),
        (Duration::from_millis(250), Duration::from_millis(500), 7, 1000, "uw-staff")
    );

    assert!(from(&[HUBS]).unwrap_err().contains("IC_NODES"));
    assert!(from(&[("IC_NODES", "  "), HUBS]).unwrap_err().contains("IC_NODES"));
    assert!(from(&[("IC_NODES", "n1:7000")]).unwrap_err().contains("IC_EVENT_HUBS_ENDPOINT"));
    for (key, value) in [
        ("IC_UNDERWRITING_CYCLE_MS", "0"),
        ("IC_UNDERWRITING_PATIENCE_MS", "soon"),
        ("IC_UNDERWRITING_BATCH", "-1"),
        ("IC_UNDERWRITING_SWEEP_PAGE", "1001"),
    ] {
        let refused = from(&[("IC_NODES", "n1:7000"), HUBS, (key, value)]).unwrap_err();
        assert!(refused.contains(key) && refused.contains(value), "{refused}");
    }
}

/// Purpose: on a real node hosting the core and the underwriting, reinsurance, group and channel
/// modules, the driver built from its settings carries a clean application to a policy in the
/// ledger: commands as JSON through Raft, the reinsurance module's capacity question, the
/// core's reply with the layers it wrote. A group excess answer for a case group administration
/// does not know is refused by that module and parked with its reason. The channel module
/// takes the proposal's progress rows under this module's name. The daily sweep runs
/// once for the day. With no node to reach the pass stops as unavailable. The process itself
/// reads collections' list of lapsed policies through the core in either shape it may come in;
/// refuses to start without its settings, runs its cycles and stops when told; and the shipped
/// binary does the same from its environment and exits cleanly on SIGTERM.
/// Why it matters: every other driver test runs the modules in the test's own process. This is
/// the deployed arrangement, and the one that fails if a wire format, a module name or a caller
/// differs between what this driver sends and what the hosted modules accept. A binary that
/// ignored SIGTERM would be killed mid-delivery on every rollout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_driver_carries_a_proposal_to_a_policy_on_a_real_node() {
    let dir = std::env::temp_dir().join(format!("ic-underwriting-service-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 1000,
        snapshot_every: 1000,
    };
    let modules = vec![
        ic_underwriting::module_spec().unwrap(),
        ic_reinsurance::host::module_spec().unwrap(),
        ic_group::module_spec().unwrap(),
        ic_channels::query::module_spec().unwrap(),
        ic_raft::module::ModuleSpec::new("collections", || Box::new(Collections::default())).unwrap(),
    ];
    let node = server::start_with(settings, modules).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    let patience = Duration::from_secs(20);

    // The core: a business date and the shipped products.
    let mut setup = vec![("leader", ic_core::Command::OpenBusinessDate { date: clock().date_naive() })];
    setup.extend(shipped_activations().unwrap().into_iter().map(|c| ("config", c)));
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
    // The version of the product the core now holds, worked out the same way.
    let product_hash = {
        let mut reference = ic_sim::Cluster::in_memory(1, 7);
        reference.open(clock().date_naive()).unwrap();
        for activation in shipped_activations().unwrap() {
            reference.activate_everywhere(activation).unwrap();
        }
        reference
            .state(0)
            .catalog
            .product_on("term-life", clock().date_naive())
            .unwrap()
            .content_hash
            .clone()
    };

    // Reinsurance: the retail treaty and its rates.
    let treaties = [
        ic_reinsurance::Command::ActivateTreaty {
            treaty: Box::new(load_treaty("retail_life_surplus.star", RETAIL).unwrap()),
        },
        ic_reinsurance::Command::ActivateRateTable {
            table: Box::new(load_rate_table("ri_life_base.star", RATES).unwrap()),
        },
    ];
    for (n, command) in treaties.into_iter().enumerate() {
        let envelope = ic_reinsurance::Envelope {
            key: format!("config-{n}"),
            caller: "config".into(),
            on: clock().date_naive(),
            command,
        };
        let reply = client
            .propose_module_until("reinsurance", &serde_json::to_value(&envelope).unwrap(), patience)
            .await
            .unwrap();
        assert!(reply.result.unwrap()["result"].get("Ok").is_some());
    }

    // Underwriting: the shipped configuration, then an application, sent as the channels would.
    let cluster = Cluster::new(client.clone(), 1, patience);
    let mut w = World::new();
    w.now = clock();
    let mut sent = 0;
    let mut open = w.open_command("P1", &["L1"], &[("L1", "life", "life", 500_000_00)], false);
    open.quote.product_hash = product_hash;
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    w.disclose("P1", "L1", &clean());
    let mut excess = w.open_command("grp-uw:acme:1", &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    excess.product_code = "group-life".into();
    excess.entry = Entry::GroupExcess {
        policy: excess.policy.clone(),
        takeover_accepted_minor: None,
        reinsurance_reference: None,
    };
    w.ok("group-admin", Command::OpenProposal(Box::new(excess)));
    w.ok(
        "group-admin",
        Command::Withdraw {
            proposal: "grp-uw:acme:1".into(),
        },
    );
    for envelope in &w.log[sent..] {
        cluster.propose(envelope.clone()).await.unwrap().unwrap();
    }
    sent = w.log.len();
    assert!(sent > 0);

    let config = Config::from_lookup(|key| match key {
        "IC_NODES" => Some(addr.clone()),
        "IC_EVENT_HUBS_ENDPOINT" => Some("https://hubs.example".into()),
        _ => None,
    })
    .unwrap();
    let carrier = connect(client.clone(), &config, clock, NothingConnected).await.unwrap();
    let mut swept = None;
    let (pass, sweep) = carrier.cycle(&mut swept, config.sweep_page).await;
    let pass = pass.unwrap();
    let sweep = sweep.unwrap().unwrap();
    assert_eq!(sweep.pages, 1);
    // Collections cannot answer yet: the day's sweep stands and says why no reinstatements
    // were looked for.
    assert!(sweep.reinstatements_unavailable.unwrap().contains("no list yet"));
    assert_eq!(swept, Some(clock().date_naive()));

    let proposal: Option<Proposal> = serde_json::from_value(cluster.ask(Query::Proposal { proposal: "P1".into() }).await.unwrap()).unwrap();
    let proposal = proposal.unwrap();
    assert!(matches!(proposal.status, ProposalStatus::Issued { .. }), "{:?} {pass:?}", proposal.status);
    let policy = client.locate(0, &proposal.policy).await.unwrap();
    assert!(policy.is_some(), "the policy is in the ledger's directory");
    let view = client.policy_by_number(0, &proposal.policy_number).await.unwrap().unwrap();
    assert_eq!(view.policy.status, PolicyStatus::InForce);

    // Collections' list read through the core: a policy with its partition, a bare ID found by
    // asking each partition, and one the ledger does not hold, which is left out.
    let listed = serde_json::json!([{"policy": proposal.policy, "partition": 0}, proposal.policy, "POL-NOBODY"]);
    client.propose_module_until("collections", &listed, patience).await.unwrap();
    let read = ic_underwriting::driver::Core::lapsed_for_underwriting(&cluster).await.unwrap();
    assert_eq!(
        read.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
        vec![proposal.policy.clone(), proposal.policy.clone()]
    );
    client
        .propose_module_until("collections", &serde_json::json!({"not": "a list"}), patience)
        .await
        .unwrap();
    assert!(matches!(
        ic_underwriting::driver::Core::lapsed_for_underwriting(&cluster).await,
        Err(DriverError::Answer(_))
    ));

    // Group administration has no such scheme: its refusal is parked with its code.
    let refused: Vec<_> = pass
        .parked
        .iter()
        .filter(|(_, why)| why.starts_with("group administration refused the decision"))
        .collect();
    assert_eq!(refused.len(), 1, "{pass:?}");
    // The hosted channel module took the proposals' progress rows under this module's name.
    assert!(pass.published >= 1 && pass.progress_refused == 0, "{pass:?}");
    // Letters to the customer and adviser have no receiver yet.
    assert!(pass.parked.iter().any(|(_, why)| why == "correspondence is not connected to underwriting"));

    // The same day again: no sweep, nothing to carry.
    let (pass, sweep) = carrier.cycle(&mut swept, config.sweep_page).await;
    assert!(sweep.is_none());
    let pass = pass.unwrap();
    assert_eq!((pass.delivered, pass.parked.len(), pass.published), (0, 0, 0));

    // Nobody there: the driver does not start, and one already running stops each pass and
    // each sweep as unavailable without marking the day swept.
    let nowhere = ClusterClient::new(vec!["127.0.0.1:1".to_string()]).with_auth(ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap());
    let impatient = Config {
        patience: Duration::from_millis(200),
        ..config.clone()
    };
    assert!(connect(nowhere.clone(), &impatient, clock, NothingConnected).await.is_err());
    let mut lost = connect(client.clone(), &impatient, clock, NothingConnected).await.unwrap();
    let away = Cluster::new(nowhere, 1, impatient.patience);
    (lost.underwriting, lost.reinsurer, lost.core, lost.schemes) = (away.clone(), away.clone(), away.clone(), away);
    let mut never = None;
    let (pass, sweep) = lost.cycle(&mut never, 10).await;
    assert!(matches!(pass, Err(DriverError::Unavailable(_))), "{pass:?}");
    assert!(matches!(sweep, Some(Err(DriverError::Unavailable(_)))));
    assert_eq!(never, None);

    // The process: refuses to start without settings or a token, and runs until told to stop.
    let no_file = |_: &str| -> std::io::Result<String> { Err(std::io::Error::other("no file")) };
    assert!(serve(|_| None, no_file, async {}).await.unwrap_err().contains("IC_NODES"));
    assert!(serve(|key| (key == "IC_NODES").then(|| addr.clone()), no_file, async {})
        .await
        .unwrap_err()
        .contains("IC_EVENT_HUBS_ENDPOINT"));
    let env = |key: &str| match key {
        "IC_NODES" => Some(addr.clone()),
        "IC_EVENT_HUBS_ENDPOINT" => Some("https://hubs.example".into()),
        "IC_CLUSTER_TOKEN" => Some(TOKEN.to_string()),
        "IC_UNDERWRITING_CYCLE_MS" => Some("50".to_string()),
        _ => None,
    };
    serve(env, no_file, tokio::time::sleep(Duration::from_millis(400))).await.unwrap();

    // The shipped binary, from its environment alone.
    let binary = || {
        let mut p = std::process::Command::new(env!("CARGO_BIN_EXE_ic-underwriting"));
        p.env_clear()
            .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        p
    };
    let refused = binary().output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("IC_NODES"));

    let mut child = binary()
        .env("IC_NODES", &addr)
        .env("IC_EVENT_HUBS_ENDPOINT", "https://hubs.example")
        .env("IC_CLUSTER_TOKEN", TOKEN)
        .env("IC_UNDERWRITING_CYCLE_MS", "50")
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let started = tokio::task::spawn_blocking(move || {
        BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .any(|line| line.contains("underwriting driver started"))
    });
    assert!(
        tokio::time::timeout(Duration::from_secs(60), started).await.unwrap().unwrap(),
        "ic-underwriting did not start"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(child.try_wait().unwrap().is_none(), "ic-underwriting exited while running");
    assert!(std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let exit = tokio::task::spawn_blocking(move || child.wait().unwrap());
    let exit = tokio::time::timeout(Duration::from_secs(30), exit).await.unwrap().unwrap();
    assert!(exit.success(), "ic-underwriting did not stop cleanly on SIGTERM: {exit:?}");

    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
