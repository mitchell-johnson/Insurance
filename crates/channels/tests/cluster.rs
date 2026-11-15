//! End to end on real ledger nodes: the channel module hosted in its own
//! Raft group, fed the core's events by the host, behind the portal service
//! and the shipped `ic-portal` binary.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command as Process, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{d, open};
use http_body_util::BodyExt;
use ic_api::auth::Auth;
use ic_channels::http::{router, AppState, Channel, Hosted};
use ic_channels::ops::{Level, Live, RegionSource, Regions, StatusPage};
use ic_channels::query::{module_spec, Question, MODULE};
use ic_channels::serve::{beat, heartbeat, module_current, wall_clock};
use ic_channels::ui::UiConfig;
use ic_core::{Command, Envelope};
use ic_product::{Frequency, Sex};
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Running, Settings};
use ic_raft::{ClusterAuth, ClusterClient, NodeId};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use serde_json::{json, Value};
use tower::ServiceExt;

const TOKEN: &str = "test-cluster-token-0123456789abcdef";
const KIRI: &str = "party-00000001";

struct Nodes {
    dir: PathBuf,
    peers: BTreeMap<NodeId, String>,
    running: BTreeMap<NodeId, Running>,
    client: ClusterClient,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Nodes {
    /// Three nodes, each hosting one partition and the channel module.
    async fn start(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let peers: BTreeMap<NodeId, String> = (1..=3u64).map(|id| (id, format!("127.0.0.1:{}", free_port()))).collect();
        let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
        let client = ClusterClient::new(peers.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(auth.clone());
        let mut running = BTreeMap::new();
        for id in 1..=3u64 {
            let settings = Settings {
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
                snapshot_every: 1000,
                witnesses: Default::default(),
            };
            running.insert(id, server::start_with(settings, vec![module_spec().unwrap()]).await.unwrap());
        }
        Self { dir, peers, running, client }
    }

    async fn core(&self, key: &str, caller: &str, command: Command) {
        let envelope = Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        };
        let reply = self.client.propose_until(0, &envelope, Duration::from_secs(30)).await.unwrap();
        assert_eq!(reply.rejected, None);
    }

    /// Open the ledger, activate the shipped products and issue one policy.
    async fn business(&self) {
        self.core("open", "leader", Command::OpenBusinessDate { date: open() }).await;
        for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
            self.core(&format!("activate-{n}"), "config", activation).await;
        }
        let application = term_life(1, open(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        self.core("issue-1", "new-business", issue_command(application)).await;
    }

    fn hosted(&self, patience: Duration) -> Arc<Hosted> {
        Arc::new(Hosted {
            client: self.client.clone(),
            patience,
        })
    }

    async fn stop(self) {
        for (_, node) in self.running {
            node.stop().await.unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Wait until the module has been fed the policy and Kiri's feed holds it.
async fn fed(channel: &dyn Channel) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(sync) = channel
            .ask(Question::Sync {
                party: KIRI.into(),
                have: None,
            })
            .await
        {
            if sync["rows"].as_array().is_some_and(|rows| rows.len() == 4) {
                return sync;
            }
        }
        assert!(Instant::now() < deadline, "the module was not fed the policy's events");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn call(app: &axum::Router, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let response = app.clone().oneshot(request.body(Body::from(body)).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Purpose: on real nodes the module is fed the core's events by the host, the portal service reads and writes it through the group's leader, the gateway's heartbeat keeps it fresh, and the loss of the node leading it is ridden through.
/// Why it matters: this is the production path in one piece: Raft group, event feed, JSON commands, queries, and failover. A break in any link shows customers nothing or stale data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_portal_service_on_real_nodes() {
    let mut nodes = Nodes::start("portal-nodes").await;
    nodes.business().await;
    let channel = nodes.hosted(Duration::from_secs(30));
    let sync = fed(channel.as_ref()).await;
    assert_eq!(sync["mode"], "snapshot");

    let regions = Regions::parse(
        &format!("auckland={},{};sydney={}", nodes.peers[&1], nodes.peers[&2], nodes.peers[&3]),
        "auckland",
    )
    .unwrap();
    let live = Live::new(nodes.client.clone(), regions);
    let app = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: channel.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(wall_clock),
        regions: live.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::pricing::Down),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));

    // The identity service binds Kiri's sign-in, through the service, as a module.
    let bind = json!({ "directory": { "bind_subject": { "subject": "sub-kiri", "party": KIRI } } });
    let identity = [
        ("x-dev-subject", "svc"),
        ("x-dev-roles", "identity"),
        ("x-module", "identity"),
        ("idempotency-key", "bind-1"),
    ];
    let (status, result) = call(&app, "POST", "/internal/v1/commands", &identity, Some(bind.clone())).await;
    assert_eq!((status, &result["duplicate"]), (StatusCode::OK, &json!(false)));
    // Offered again, as after a timeout: the module answers that it has it.
    assert_eq!(call(&app, "POST", "/internal/v1/commands", &identity, Some(bind)).await.1["duplicate"], true);

    let kiri = [("x-dev-subject", "sub-kiri")];
    let (status, me) = call(&app, "GET", "/app/v1/me", &kiri, None).await;
    assert_eq!((status, &me["policies"][0]["number"]), (StatusCode::OK, &json!("KTL00000001")));
    assert_eq!(call(&app, "GET", "/readyz", &[], None).await.0, StatusCode::OK);
    // A refusal by the module comes back as the module's own reason.
    let request = json!({ "id": "r1", "policy": "pol-nope", "kind": "x", "module": "claims" });
    let write = [("x-dev-subject", "sub-kiri"), ("idempotency-key", "w-1")];
    assert_eq!(call(&app, "POST", "/app/v1/requests", &write, Some(request)).await.0, StatusCode::NOT_FOUND);

    // The claims console's reach into the core: questions, where a policy lives, and a person's command on its partition.
    {
        use ic_channels::claims::Partitions;
        use ic_core::claims_module::report::{Question as Claims, Reply};
        let claims = ic_channels::claims::Hosted {
            client: nodes.client.clone(),
            partitions: 1,
            patience: Duration::from_secs(10),
        };
        assert_eq!(claims.count(), 1);
        assert!(matches!(claims.ask(0, Claims::Book).await.unwrap(), Some(Reply::Book(_))));
        assert_eq!(claims.locate(0, ic_core::PolicyId::from("pol-00000001")).await.unwrap(), Some(0));
        assert_eq!(claims.locate(0, ic_core::PolicyId::from("pol-nope")).await.unwrap(), None);
        let staff = ic_core::PartyId::from("cl-1");
        let (_, _, command) = ic_channels::claims::command(&staff, "first contact", None, json!({ "record_contact": { "claim": "c-1" } })).unwrap();
        let envelope = ic_channels::claims::envelope("cluster-claims-1".into(), command);
        // No claims rules or authority are shipped, so the partition refuses claims work; it is the core's answer that comes back.
        let first = claims.propose(0, envelope.clone()).await.unwrap();
        assert!(first.rejected.is_some() && !first.duplicate, "{first:?}");
        assert!(claims.propose(0, envelope).await.unwrap().duplicate);
        // New business and the core's own view: a dry run of an application, a policy by number, the end-of-day view.
        let application = ic_sim::persona::term_life(90, open(), d(1985, 6, 1), Sex::Male, false, 250_000_00, None, Frequency::Monthly, None);
        assert!(
            claims.quote(0, application.clone()).await.unwrap().is_some(),
            "the date is open, so the core prices it"
        );
        let mut unknown = application.clone();
        unknown.product_code = "no-such-product".into();
        assert!(matches!(claims.quote(0, unknown).await.unwrap(), Some(Err(_))));
        let held = claims.policy(0, "KTL00000001".into()).await.unwrap().expect("the seeded policy");
        assert_eq!(held["policy"]["number"], "KTL00000001");
        assert_eq!(claims.policy(0, "NOPE".into()).await.unwrap(), None);
        let closing = claims.closing(0).await.unwrap();
        assert_eq!(closing["partition"], 0);
        assert!(ic_channels::claims::reconciliation(&[closing]).unwrap()["business_date"].is_string());
        let unreachable = ic_channels::claims::Hosted {
            client: ClusterClient::new(vec![format!("127.0.0.1:{}", free_port())]).with_attempt_timeout(Duration::from_millis(200)),
            partitions: 1,
            patience: Duration::from_millis(300),
        };
        assert!(unreachable.ask(0, Claims::Book).await.is_err());
        assert!(unreachable.locate(0, ic_core::PolicyId::from("pol-00000001")).await.is_err());
        let (_, _, command) = ic_channels::claims::command(&staff, "p", None, json!({ "record_contact": { "claim": "c-1" } })).unwrap();
        assert!(unreachable.propose(0, ic_channels::claims::envelope("k".into(), command)).await.is_err());
        assert!(unreachable.quote(0, application).await.is_err());
        assert!(unreachable.policy(0, "KTL00000001".into()).await.is_err());
        assert!(unreachable.closing(0).await.is_err());
    }

    // The consoles' reach into another module on the nodes: a module these nodes do not host refuses, and nodes that
    // cannot be reached are an error, for a question and for a command alike.
    {
        use ic_channels::forward::{Hosted as Module, Source};
        let nowhere = Module {
            client: nodes.client.clone(),
            module: "no-such-module",
            patience: Duration::from_secs(5),
        };
        assert!(nowhere.ask(json!("status")).await.is_err());
        assert!(nowhere.tell(json!({ "key": "k" })).await.is_err());
        let unreachable = Module {
            client: ClusterClient::new(vec![format!("127.0.0.1:{}", free_port())]).with_attempt_timeout(Duration::from_millis(200)),
            module: "distribution",
            patience: Duration::from_millis(300),
        };
        assert!(unreachable.ask(json!("status")).await.is_err());
        assert!(unreachable.tell(json!({ "key": "k" })).await.is_err());
    }

    // The heartbeat: the module is current, so the gateway says so and the data is fresh.
    assert!(module_current(&nodes.client).await);
    assert!(beat(channel.as_ref(), &nodes.client, wall_clock()).await);
    let (_, view) = call(&app, "GET", "/app/v1/policies/pol-00000001", &kiri, None).await;
    assert_eq!(view["fresh"], true);
    let beating = tokio::spawn(heartbeat(
        channel.clone(),
        nodes.client.clone(),
        Duration::from_millis(50),
        Arc::new(wall_clock),
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    beating.abort();

    // Every member holds the same state.
    let mut hashes = Vec::new();
    for _ in 0..100 {
        hashes.clear();
        for addr in nodes.peers.values() {
            let status = nodes
                .client
                .module_status(addr)
                .await
                .unwrap()
                .into_iter()
                .find(|s| s.module == MODULE)
                .unwrap();
            hashes.push((status.last_applied, status.state_hash));
        }
        if hashes.iter().all(|h| h == &hashes[0]) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(hashes.iter().all(|h| h == &hashes[0]), "the members' state hashes differ: {hashes:?}");

    // Regions: everything answers. Whether leaders are at home depends on the election, so only the level's floor is asserted.
    let report = live.report().await;
    assert!(report.quorum && report.partitions == 1 && report.partitions_with_leader == 1);
    assert_ne!(StatusPage::from(&report).level, Level::Outage);
    assert_eq!(live.report().await, report, "a second ask within the keep time is answered from memory");

    // The node leading the module's group is lost. The service rides through it.
    let mut leader = None;
    for (id, addr) in &nodes.peers {
        let status = nodes
            .client
            .module_status(addr)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.module == MODULE)
            .unwrap();
        if status.role == "leader" {
            leader = Some(*id);
        }
    }
    let leader = leader.expect("the module's group has a leader");
    nodes.running.remove(&leader).unwrap().kill().await;
    let (status, me) = call(&app, "GET", "/app/v1/me", &kiri, None).await;
    assert_eq!((status, &me["party"]), (StatusCode::OK, &json!(KIRI)));
    let request = json!({ "id": "r2", "policy": "pol-00000001", "kind": "decline_indexation", "module": "policy-servicing" });
    let write = [("x-dev-subject", "sub-kiri"), ("idempotency-key", "w-2")];
    assert_eq!(call(&app, "POST", "/app/v1/requests", &write, Some(request)).await.0, StatusCode::ACCEPTED);

    // A second node is lost: no majority. The service says it is unavailable and the status page says outage.
    let another = *nodes.running.keys().next().unwrap();
    nodes.running.remove(&another).unwrap().kill().await;
    let impatient = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: nodes.hosted(Duration::from_millis(300)),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(wall_clock),
        regions: Live::new(nodes.client.clone(), Regions::single(nodes.peers.values().cloned().collect())),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::pricing::Down),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    let (status, problem) = call(&impatient, "GET", "/app/v1/me", &kiri, None).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("unavailable")));
    assert_eq!(call(&impatient, "GET", "/readyz", &[], None).await.0, StatusCode::SERVICE_UNAVAILABLE);
    let write = [("x-dev-subject", "sub-kiri"), ("idempotency-key", "w-3")];
    assert_eq!(
        call(&impatient, "DELETE", "/app/v1/requests/r2", &write, None).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(call(&impatient, "GET", "/status.json", &[], None).await.1["level"], "outage");
    // A member cut off from the majority may still call itself leader for a moment;
    // the heartbeat is a command, so it cannot be committed without a majority.
    assert!(!beat(nodes.hosted(Duration::from_millis(100)).as_ref(), &nodes.client, wall_clock()).await);
    nodes.stop().await;
}

/// Purpose: the shipped `ic-portal` binary starts from environment alone against real nodes, serves the app, and stops cleanly on SIGTERM; with a bad configuration it exits at once and says why.
/// Why it matters: this is what the container runs; a binary that ignores SIGTERM is killed mid-request on every rollout, and one that starts on a bad configuration fails later and less clearly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shipped_binary_serves_and_stops() {
    let binary = || {
        let mut p = Process::new(env!("CARGO_BIN_EXE_ic-portal"));
        p.env_clear()
            .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        p
    };
    let refused = binary().env("IC_AUTH", "dev").output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("IC_NODES"));

    let nodes = Nodes::start("portal-binary").await;
    nodes.business().await;
    fed(nodes.hosted(Duration::from_secs(30)).as_ref()).await;
    let port = free_port();
    let list = nodes.peers.values().cloned().collect::<Vec<_>>().join(",");
    let mut child = binary()
        .env("IC_NODES", &list)
        .env("IC_CLUSTER_TOKEN", TOKEN)
        .env("IC_AUTH", "dev")
        .env("IC_PORTAL_LISTEN", format!("127.0.0.1:{port}"))
        .env("IC_PORTAL_HEARTBEAT_SECS", "1")
        .spawn()
        .unwrap();
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(60);
    while http.get(format!("{base}/healthz")).send().await.is_err() {
        assert!(child.try_wait().unwrap().is_none(), "ic-portal exited at start");
        assert!(Instant::now() < deadline, "ic-portal did not start listening");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(http.get(format!("{base}/readyz")).send().await.unwrap().status(), 200);
    let status: Value = http.get(format!("{base}/status.json")).send().await.unwrap().json().await.unwrap();
    assert_eq!(status["level"], "operational");
    assert!(http
        .get(format!("{base}/staff"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
        .contains("<!doctype html>"));
    let unknown = http.get(format!("{base}/app/v1/me")).header("x-dev-subject", "nobody").send().await.unwrap();
    assert_eq!(unknown.status(), 403);
    // Its own heartbeat reaches the module.
    let channel = nodes.hosted(Duration::from_secs(10));
    let deadline = Instant::now() + Duration::from_secs(30);
    while channel.ask(Question::Status).await.unwrap()["heard"].as_i64().unwrap() < wall_clock() - 30 {
        assert!(Instant::now() < deadline, "the binary's heartbeat did not reach the module");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert!(Process::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(20);
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        assert!(Instant::now() < deadline, "ic-portal ignored SIGTERM");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(exit.success(), "ic-portal exited with {exit}");
    nodes.stop().await;
}
