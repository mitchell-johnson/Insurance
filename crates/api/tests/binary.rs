//! The shipped `ic-api` binary, started as a process the way a container
//! runtime starts it: configured by environment alone, called over real
//! HTTP, and stopped with SIGTERM.

mod common;

use std::path::Path;
use std::process::{Child, Command as Process, Output, Stdio};
use std::time::{Duration, Instant};

use common::{d, free_port, scratch, Ledger, TOKEN};
use ic_core::Command;
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::term_life;
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};

const OPENING: &str = "2027-11-15";

/// The binary with nothing inherited from the test's own environment except
/// where coverage data goes.
fn binary() -> Process {
    let mut p = Process::new(env!("CARGO_BIN_EXE_ic-api"));
    p.env_clear()
        .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    p
}

struct Api {
    child: Child,
    base: String,
    http: Client,
}

impl Api {
    /// Start the binary and wait until it answers.
    async fn start(env: &[(&str, &str)]) -> Self {
        let port = free_port();
        let child = binary()
            .envs(env.iter().copied())
            .env("IC_LISTEN", format!("127.0.0.1:{port}"))
            .spawn()
            .unwrap();
        let mut api = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
            http: Client::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(r) = api.http.get(format!("{}/healthz", api.base)).send().await {
                assert_eq!(r.status(), StatusCode::OK);
                return api;
            }
            if let Some(exit) = api.child.try_wait().unwrap() {
                let output = api.child.wait_with_output().unwrap();
                panic!("ic-api exited at start ({exit}): {}", String::from_utf8_lossy(&output.stderr));
            }
            assert!(Instant::now() < deadline, "ic-api did not start listening");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn call(&self, method: Method, path: &str, roles: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("x-dev-subject", "svc")
            .header("x-dev-roles", roles);
        if let Some(key) = key {
            req = req.header("idempotency-key", key);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    /// Ask the process to stop the way Kubernetes does, and collect what it
    /// wrote.
    async fn terminate(self) -> Output {
        let signalled = Process::new("kill").args(["-TERM", &self.child.id().to_string()]).status().unwrap();
        assert!(signalled.success());
        tokio::task::spawn_blocking(move || self.child.wait_with_output().unwrap()).await.unwrap()
    }
}

fn application(n: u64) -> Value {
    let inception = d(2027, 11, 15);
    serde_json::to_value(term_life(
        n,
        inception,
        d(1991, 2, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    ))
    .unwrap()
}

fn in_a_box(data: &Path) -> Vec<(&str, &str)> {
    vec![
        ("IC_AUTH", "dev"),
        ("IC_PARTITIONS", "2"),
        ("IC_OPENING_DATE", OPENING),
        ("IC_DATA_DIR", data.to_str().unwrap()),
    ]
}

/// Purpose: start the shipped binary as the insurance in a box on a data directory, issue
/// a policy over HTTP, stop it with SIGTERM, start it again on the same directory, and
/// find the policy, the business date and the idempotency record all still there.
/// Why it matters: this is the only test of the process an operator actually runs: its
/// environment parsing, its listener, its shutdown and its recovery from disk. Every other
/// API test calls the router in memory and would pass with a binary that lost its ledger
/// on restart or ignored SIGTERM until Kubernetes killed it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shipped_binary_keeps_its_ledger_across_a_restart() {
    let data = scratch("api-binary-box");
    let api = Api::start(&in_a_box(&data)).await;
    let (status, ready) = api.call(Method::GET, "/readyz", "", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["business_date"], OPENING);
    let (status, issued) = api
        .call(Method::POST, "/v1/policies", "new-business", Some("app-1"), Some(application(1)))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{issued}");
    let (status, before) = api.call(Method::GET, "/v1/policies/KTL00000001", "policy-servicing", None, None).await;
    assert_eq!(status, StatusCode::OK, "{before}");

    let stopped = api.terminate().await;
    assert!(stopped.status.success(), "SIGTERM did not lead to a clean exit: {:?}", stopped.status);
    let log = String::from_utf8_lossy(&stopped.stdout);
    assert!(log.contains("development identity"), "no warning that callers are trusted by header: {log}");
    assert!(
        log.lines().all(|l| serde_json::from_str::<Value>(l).is_ok()),
        "the log is not JSON lines: {log}"
    );

    // A different opening date is ignored: the ledger already has its date.
    let mut env = in_a_box(&data);
    env[2] = ("IC_OPENING_DATE", "2030-01-01");
    let api = Api::start(&env).await;
    let (_, ready) = api.call(Method::GET, "/readyz", "", None, None).await;
    assert_eq!(ready["business_date"], OPENING);
    let (status, after) = api.call(Method::GET, "/v1/policies/KTL00000001", "policy-servicing", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after, before);
    let (status, again) = api
        .call(Method::POST, "/v1/policies", "new-business", Some("app-1"), Some(application(1)))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(again["duplicate"], true, "the restarted process forgot the key: {again}");
    assert!(api.terminate().await.status.success());
    let _ = std::fs::remove_dir_all(&data);
}

/// Purpose: start the shipped binary as a gateway to a three-node ledger, configured by
/// `IC_NODES` and the cluster token alone, and issue and read a policy through it; then
/// show that a gateway configured for the wrong number of partitions refuses to start.
/// Why it matters: the gateway is the production arrangement on AKS. A gateway that
/// routed with a different partition count from the nodes would send every policy to the
/// wrong Raft group, so the mismatch has to stop the process at start, with a message that
/// names the setting, before it serves a single request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shipped_binary_fronts_ledger_nodes() {
    let ledger = Ledger::start("api-binary-gateway", 3, 2).await;
    let nodes = ledger.nodes().join(",");
    let env = [
        ("IC_AUTH", "dev"),
        ("IC_PARTITIONS", "2"),
        ("IC_NODES", nodes.as_str()),
        ("IC_CLUSTER_TOKEN", TOKEN),
    ];
    let api = Api::start(&env).await;
    assert_eq!(api.call(Method::GET, "/readyz", "", None, None).await.0, StatusCode::SERVICE_UNAVAILABLE);
    for p in 0..2 {
        ledger
            .run(p, &format!("open-{p}"), "leader", Command::OpenBusinessDate { date: d(2027, 11, 15) })
            .await;
    }
    for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
        let body = json!({ "command": activation });
        let (status, body) = api
            .call(Method::POST, "/v1/commands", "config", Some(&format!("activate-{n}")), Some(body))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }
    let (status, issued) = api
        .call(Method::POST, "/v1/policies", "new-business", Some("app-1"), Some(application(1)))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{issued}");
    let (status, view) = api.call(Method::GET, "/v1/policies/KTL00000001", "policy-servicing", None, None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let (status, close) = api.call(Method::POST, "/v1/admin/end-of-day", "operations", None, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{close}");
    assert!(api.terminate().await.status.success());

    let mismatched = binary()
        .envs([
            ("IC_AUTH", "dev"),
            ("IC_PARTITIONS", "4"),
            ("IC_NODES", nodes.as_str()),
            ("IC_CLUSTER_TOKEN", TOKEN),
        ])
        .env("IC_LISTEN", format!("127.0.0.1:{}", free_port()))
        .output()
        .unwrap();
    assert!(!mismatched.status.success());
    assert!(
        String::from_utf8_lossy(&mismatched.stderr).contains("IC_PARTITIONS"),
        "the error does not name the setting"
    );
    ledger.stop().await;
}

/// Purpose: the shipped binary exits at once, with a message naming the cause, when it is
/// given no caller identity, a product pack it cannot open its ledger with, or a listening
/// address already in use.
/// Why it matters: a pod that fails at start is visible in the rollout and never takes
/// traffic. The dangerous alternative is a process that starts anyway: with no identity
/// configured that would be an API open to anyone who can reach it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shipped_binary_refuses_to_start_misconfigured() {
    let no_identity = binary().output().unwrap();
    assert!(!no_identity.status.success());
    assert!(String::from_utf8_lossy(&no_identity.stderr).contains("IC_ENTRA_TENANT_ID"));

    // A file where the data directory should be.
    let dir = scratch("api-binary-bad");
    let file = dir.join("not-a-directory");
    std::fs::write(&file, "x").unwrap();
    let bad_data = binary().envs(in_a_box(&file)).env("IC_LISTEN", "127.0.0.1:0").output().unwrap();
    assert!(!bad_data.status.success());
    assert!(!bad_data.stderr.is_empty());

    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let in_use = binary().envs([("IC_AUTH", "dev"), ("IC_LISTEN", addr.as_str())]).output().unwrap();
    assert!(!in_use.status.success());
    assert!(
        String::from_utf8_lossy(&in_use.stderr).contains("bind"),
        "{}",
        String::from_utf8_lossy(&in_use.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
