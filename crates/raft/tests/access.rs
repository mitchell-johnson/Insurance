//! Each workload calls a node with its own token and may do only what its
//! grant in the access map says.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::scratch;
use ic_log::StateMachine;
use ic_raft::access::{Access, Ask, Guard, Mode, Who};
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{hosted, ClusterAuth, ClusterClient, ModuleSpec, ModuleState};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const NODE: &str = "node-token-0123456789abcdef0123456789";
const PORTAL: &str = "portal-token-0123456789abcdef01234567";
const PORTAL_NEXT: &str = "portal-next-token-0123456789abcdef0123";
const READER: &str = "reader-token-0123456789abcdef01234567";
const CLAIMS: &str = "claims-token-0123456789abcdef01234567";
const SHARED: &str = "shared-token-0123456789abcdef01234567";

#[derive(Default, Serialize, Deserialize)]
struct Count {
    n: i64,
}

impl StateMachine for Count {
    type Command = Value;
    type Output = i64;

    fn apply(&mut self, _index: u64, _command: Value) -> i64 {
        self.n += 1;
        self.n
    }
}

fn count() -> Box<dyn ModuleState> {
    hosted(Count::default(), |c: &Count, _q: Value| Ok(json!(c.n)))
}

fn sha(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn map(mode: &str, shared: &str) -> Value {
    json!({
        "mode": mode,
        "shared_token": shared,
        "workloads": {
            "ic-node": { "token_sha256": [sha(NODE)], "peer": true },
            "ic-portal": { "token_sha256": [sha(PORTAL), sha(PORTAL_NEXT)], "write_modules": ["count"] },
            "ic-reader": { "token_sha256": [sha(READER)], "read_partitions": true, "read_modules": ["count"] },
            "ic-claims": { "token_sha256": [sha(CLAIMS)], "callers": ["claims"] },
        }
    })
}

struct Call {
    http: reqwest::Client,
    base: String,
}

impl Call {
    async fn send(&self, token: Option<&str>, method: Method, path: &str, body: Option<Value>) -> StatusCode {
        let mut request = self.http.request(method, format!("{}{path}", self.base));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.unwrap().status()
    }
}

async fn start(dir: &std::path::Path, file: &std::path::Path) -> (server::Running, String) {
    let port = common::free_port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec![NODE.to_string(), SHARED.to_string()])
        .unwrap()
        .with_access_file(file.to_string_lossy());
    let settings = Settings {
        id: 1,
        listen: addr.parse().unwrap(),
        data_dir: dir.join("node-1"),
        partitions: 1,
        peers: BTreeMap::from([(1, addr.clone())]),
        bootstrap: true,
        heartbeat_ms: 50,
        auth,
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 10,
        snapshot_every: 50,
        witnesses: Default::default(),
    };
    let node = server::start_with(settings, vec![ModuleSpec::new("count", count).unwrap()]).await.unwrap();
    // Wait, as the node itself, until the groups have leaders: a busy machine
    // can take a while, and a call before then is answered 421, not judged.
    let as_node = ClusterClient::new(vec![addr.clone()]).with_auth(ClusterAuth::tokens(vec![NODE.to_string()]).unwrap());
    as_node.propose_module_until("count", &json!({}), Duration::from_secs(60)).await.unwrap();
    as_node.partitions(Duration::from_secs(60)).await.unwrap();
    (node, addr)
}

/// Purpose: in report mode a call the grant does not cover still goes through (and is logged),
/// and while the shared token is accepted it works with full access.
/// Why it matters: this is how the map is checked against a live cluster before enforcing it,
/// and how workloads keep working while they move to their own tokens.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn report_mode_refuses_nothing_and_the_shared_token_works_while_accepted() {
    let dir = scratch("access-report");
    let file = dir.join("access.json");
    std::fs::write(&file, map("report", "accept").to_string()).unwrap();
    let (node, addr) = start(&dir, &file).await;
    let call = Call {
        http: reqwest::Client::new(),
        base: format!("http://{addr}"),
    };
    assert_eq!(
        call.send(Some(READER), Method::POST, "/v1/modules/count/commands", Some(json!({}))).await,
        StatusCode::OK
    );
    assert_eq!(call.send(Some(SHARED), Method::GET, "/v1/status", None).await, StatusCode::OK);
    // The caller of a command is read only from a workload's own token, never the shared one's.
    assert_ne!(
        call.send(Some(SHARED), Method::POST, "/v1/partitions/0/commands", Some(command("claims")))
            .await,
        StatusCode::FORBIDDEN
    );
    assert_ne!(
        call.send(Some(PORTAL), Method::POST, "/v1/partitions/0/commands", Some(command("claims")))
            .await,
        StatusCode::FORBIDDEN
    );
    node.stop().await.unwrap();
}

fn command(caller: &str) -> Value {
    json!({ "key": format!("k-{caller}"), "caller": caller, "salt": "00", "command": { "NotACommand": {} } })
}

/// Purpose: on a node with an access map, each workload is held to its grant: a module writer
/// may write and read its module and nothing else; a partition reader may read partitions but
/// not write; a workload granted the caller `claims` may send core commands only as `claims`;
/// none of them may reach Raft's RPCs or the admin routes; every one may read status; a
/// rotated-in second token works at once; an unknown token, and the shared token once
/// refused, get nothing. The node itself, a peer, does everything.
/// Why it matters: before this, any pod holding the one cluster token could send any command
/// to any module under any caller name, so a compromise of the least important pod was a
/// compromise of the ledger. The core decides who may send a command by the caller name on
/// it; this is what makes that name true.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_workload_may_do_only_what_its_grant_says() {
    let dir = scratch("access-enforced");
    let file = dir.join("access.json");
    std::fs::write(&file, map("enforce", "refuse").to_string()).unwrap();
    let (node, addr) = start(&dir, &file).await;

    let call = Call {
        http: reqwest::Client::new(),
        base: format!("http://{addr}"),
    };
    use Method as M;
    let one = Some(json!({}));
    // The module writer.
    for token in [PORTAL, PORTAL_NEXT] {
        assert_eq!(call.send(Some(token), M::POST, "/v1/modules/count/commands", one.clone()).await, StatusCode::OK);
        assert_eq!(call.send(Some(token), M::POST, "/v1/modules/count/query", one.clone()).await, StatusCode::OK);
    }
    assert_eq!(
        call.send(Some(PORTAL), M::POST, "/v1/modules/other/commands", one.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(call.send(Some(PORTAL), M::GET, "/v1/partitions/0/events", None).await, StatusCode::FORBIDDEN);
    assert_eq!(
        call.send(Some(PORTAL), M::POST, "/v1/partitions/0/commands", Some(command("claims"))).await,
        StatusCode::FORBIDDEN
    );
    // The reader.
    assert_eq!(call.send(Some(READER), M::POST, "/v1/modules/count/query", one.clone()).await, StatusCode::OK);
    assert_eq!(
        call.send(Some(READER), M::POST, "/v1/modules/count/commands", one.clone()).await,
        StatusCode::FORBIDDEN
    );
    assert_ne!(
        call.send(Some(READER), M::GET, "/v1/partitions/0/end-of-day", None).await,
        StatusCode::FORBIDDEN
    );
    // The core command sender: its own caller name gets to the core (which judges the command); another does not.
    assert_ne!(
        call.send(Some(CLAIMS), M::POST, "/v1/partitions/0/commands", Some(command("claims"))).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call.send(Some(CLAIMS), M::POST, "/v1/partitions/0/commands", Some(command("new-business")))
            .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call.send(Some(CLAIMS), M::POST, "/v1/partitions/0/commands", Some(json!("no caller"))).await,
        StatusCode::FORBIDDEN
    );
    // Nobody but a peer reaches Raft or the admin routes; everyone reads status.
    for token in [PORTAL, READER, CLAIMS] {
        assert_eq!(call.send(Some(token), M::POST, "/raft/0/vote", one.clone()).await, StatusCode::FORBIDDEN);
        assert_eq!(
            call.send(Some(token), M::POST, "/v1/partitions/0/leader", Some(json!(1))).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(call.send(Some(token), M::POST, "/v1/failback/pause", None).await, StatusCode::FORBIDDEN);
        assert_eq!(call.send(Some(token), M::GET, "/v1/nowhere", None).await, StatusCode::FORBIDDEN);
        assert_eq!(call.send(Some(token), M::GET, "/v1/status", None).await, StatusCode::OK);
    }
    assert_eq!(call.send(Some(NODE), M::GET, "/v1/failback", None).await, StatusCode::OK);
    // No token, an unknown one, and the shared one while it is refused.
    for token in [None, Some("someone-else-0123456789abcdef0123456"), Some(SHARED)] {
        assert_eq!(call.send(token, M::GET, "/v1/status", None).await, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(call.send(None, M::GET, "/healthz", None).await, StatusCode::OK);
    node.stop().await.unwrap();
}

/// Purpose: the map is read again from its file: switching it to report mode lets a refused
/// call through (to be logged, not refused), accepting the shared token lets that through
/// with full access, and a file that no longer parses leaves the last good map in force.
/// Why it matters: rolling out per-workload tokens on a live ledger has to be safe. Report
/// mode shows what enforcement would refuse before it refuses anything; the shared token keeps
/// working while workloads move; and a typo in the map must never open the ledger or stop it.
#[tokio::test]
async fn the_map_is_reloaded_and_a_bad_one_never_replaces_a_good_one() {
    let dir = scratch("access-reload");
    let file = dir.join("access.json");
    std::fs::write(&file, map("enforce", "refuse").to_string()).unwrap();
    let shared = ClusterAuth::tokens(vec![NODE.to_string(), SHARED.to_string()])
        .unwrap()
        .with_access_file(file.to_string_lossy());
    let guard = Guard::for_node(&shared, |p| std::fs::read_to_string(p)).unwrap();
    let access = |g: &Guard| g.map().unwrap();
    let portal = Who::Workload("ic-portal".into());
    assert!(access(&guard).decide(&portal, &Ask::WriteModule("other".into()), None).is_err());
    assert_eq!(access(&guard).who(SHARED, &shared), None);
    assert_eq!(guard.reload(), Ok(false));

    std::fs::write(&file, map("report", "accept").to_string()).unwrap();
    assert_eq!(guard.reload(), Ok(true));
    assert_eq!(access(&guard).mode, Mode::Report);
    assert_eq!(access(&guard).who(SHARED, &shared), Some(Who::Shared));
    assert!(access(&guard).decide(&Who::Shared, &Ask::Raft, None).is_ok());

    std::fs::write(&file, "{ not json").unwrap();
    assert!(guard.reload().is_err());
    assert_eq!(access(&guard).mode, Mode::Report);
    std::fs::remove_file(&file).unwrap();
    assert!(guard.reload().is_err());

    // A guard without a map has nothing to reload and no map to show.
    let plain = Guard::for_node(&ClusterAuth::tokens(vec![NODE.to_string()]).unwrap(), |p| std::fs::read_to_string(p)).unwrap();
    assert_eq!(plain.reload(), Ok(false));
    assert!(plain.map().is_none() && plain.watch().is_none());
    assert!(guard.watch().is_some());
    // A workload dropped from the map loses everything at once.
    let map = Access::parse(&self::map("enforce", "refuse").to_string()).unwrap();
    assert!(map.decide(&Who::Workload("gone".into()), &Ask::Status, None).is_err());
}

/// Purpose: a map is refused at start when it could not work or would be unsafe: not JSON, an
/// unknown field, a workload with no token, a digest that is not a lower-case SHA-256, two
/// workloads sharing a token, no peer, a node whose own token is not a peer, a node with no
/// token at all, and a file that cannot be read.
/// Why it matters: a node that started with a broken map would either lock its peers out (a
/// cluster that cannot elect) or let a token do more than meant; failing at start, with the
/// reason, is the only safe answer.
#[test]
fn a_map_that_could_not_work_is_refused_at_start() {
    let base = map("enforce", "refuse");
    let with = |change: &dyn Fn(&mut Value)| {
        let mut m = base.clone();
        change(&mut m);
        Access::parse(&m.to_string()).unwrap_err()
    };
    assert!(Access::parse("[").unwrap_err().contains("not valid"));
    assert!(with(&|m| m["extra"] = json!(1)).contains("not valid"));
    assert!(with(&|m| m["workloads"]["ic-portal"]["token_sha256"] = json!([])).contains("no token"));
    assert!(with(&|m| m["workloads"]["ic-portal"]["token_sha256"] = json!(["ABC"])).contains("hex SHA-256"));
    assert!(with(&|m| m["workloads"]["ic-portal"]["token_sha256"] = json!([sha(PORTAL).to_uppercase()])).contains("hex SHA-256"));
    assert!(with(&|m| m["workloads"]["ic-reader"]["token_sha256"] = json!([sha(PORTAL)])).contains("share a token"));
    assert!(with(&|m| m["workloads"]["ic-node"]["peer"] = json!(false)).contains("no workload is a peer"));
    // Defaults: enforce, shared token refused.
    let parsed = Access::parse(&json!({ "workloads": { "n": { "token_sha256": [sha(NODE)], "peer": true } } }).to_string()).unwrap();
    assert_eq!(parsed.mode, Mode::Enforce);

    let text = base.to_string();
    let read = |_: &str| Ok(text.clone());
    let as_portal = ClusterAuth::tokens(vec![PORTAL.to_string()]).unwrap().with_access_file("m");
    assert!(Guard::for_node(&as_portal, read).err().unwrap().contains("not a peer"));
    let open = ClusterAuth::open().with_access_file("m");
    assert!(Guard::for_node(&open, read).err().unwrap().contains("not IC_CLUSTER_AUTH=open"));
    let missing = ClusterAuth::tokens(vec![NODE.to_string()]).unwrap().with_access_file("/no/such/file");
    assert!(Guard::for_node(&missing, |p| std::fs::read_to_string(p))
        .err()
        .unwrap()
        .contains("/no/such/file"));
    assert_eq!(missing.access_file(), Some("/no/such/file"));
    assert!(format!("{missing:?}").contains("1 tokens"));
}

/// Purpose: each route is classed as what it does, so a grant names kinds of work, not paths.
/// Why it matters: a route classed too generously (an admin route as status, a write as a
/// read) would hand a workload a power its grant never named.
#[test]
fn routes_are_classed_by_what_they_do() {
    let of = |m: Method, p: &str| Ask::of(&m, p);
    assert_eq!(of(Method::GET, "/v1/status"), Ask::Status);
    assert_eq!(of(Method::GET, "/v1/modules"), Ask::Status);
    assert_eq!(of(Method::POST, "/raft/module-x/append"), Ask::Raft);
    assert_eq!(of(Method::POST, "/v1/init"), Ask::Admin);
    assert_eq!(of(Method::PUT, "/v1/partitions/3/members"), Ask::Admin);
    assert_eq!(of(Method::PUT, "/v1/modules/x/members"), Ask::Admin);
    assert_eq!(of(Method::POST, "/v1/failback/resume"), Ask::Admin);
    assert_eq!(of(Method::POST, "/v1/partitions/3/commands"), Ask::Command);
    assert_eq!(of(Method::POST, "/v1/partitions/3/query"), Ask::ReadPartition);
    assert_eq!(of(Method::PUT, "/v1/partitions/3/consumers/c"), Ask::ReadPartition);
    assert_eq!(of(Method::DELETE, "/v1/partitions/3/consumers/c"), Ask::ReadPartition);
    assert_eq!(of(Method::POST, "/v1/modules/x/commands"), Ask::WriteModule("x".into()));
    assert_eq!(of(Method::POST, "/v1/modules/x/query"), Ask::ReadModule("x".into()));
    // A write sent as a read is not a read.
    assert_eq!(of(Method::GET, "/v1/modules/x/commands"), Ask::Unknown);
    assert_eq!(of(Method::POST, "/v1/status"), Ask::Unknown);
}

/// Purpose: the shipped grants (`deploy/cluster-access/grants.json`), with a digest for each
/// workload as `issue.sh` adds them, are a map a node accepts; it starts in report mode, the
/// nodes are the only peer, and no other workload may reach the admin routes.
/// Why it matters: the nodes refuse to start on a map they cannot read, and a grant file that
/// had drifted from the format would be found on the day of deployment instead of here.
#[test]
fn the_shipped_grants_make_a_map_the_nodes_accept() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/cluster-access/grants.json");
    let mut grants: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for (name, grant) in grants["workloads"].as_object_mut().unwrap() {
        grant["token_sha256"] = json!([sha(name)]);
    }
    let access = Access::parse(&grants.to_string()).unwrap();
    assert_eq!(access.mode, Mode::Report);
    let peers: Vec<_> = access.workloads.iter().filter(|(_, g)| g.peer).map(|(n, _)| n.as_str()).collect();
    assert_eq!(peers, ["ic-node"]);
    for name in access.workloads.keys().filter(|n| *n != "ic-node") {
        assert!(
            access.decide(&Who::Workload(name.clone()), &Ask::Admin, None).is_err(),
            "{name} may administer the cluster"
        );
        assert!(access.decide(&Who::Workload(name.clone()), &Ask::Raft, None).is_err(), "{name} may reach Raft");
    }
}

/// Purpose: every cluster token in `deploy/k8s` belongs to a workload with a grant in
/// `grants.json`, every grant has its token there, and a token is mounted only by the
/// workload it is named for, apart from the pods listed here that are the same workload by
/// design (the worker's scheduled runs; the payments pump and its drill; the admin pod and bootstrap job, which administer the
/// cluster with the nodes' token).
/// Why it matters: a process that borrows another's token is held to the other's grant and
/// logged under its name, so its calls cannot be told apart or limited. The underwriting
/// driver once ran on the worker's token that way; this stops the next one.
#[test]
fn every_process_calling_the_nodes_has_its_own_token_and_grant() {
    const SAME_WORKLOAD: [(&str, &str); 5] = [
        ("ic-close", "ic-worker"),
        ("ic-payments-pump", "ic-payments"),
        ("ic-unit-orders", "ic-worker"),
        ("ic-payments-drill", "ic-payments"),
        ("ic-bootstrap", "ic-node"),
    ];
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy");
    let grants: Value = serde_json::from_str(&std::fs::read_to_string(format!("{root}/cluster-access/grants.json")).unwrap()).unwrap();
    let granted: Vec<String> = grants["workloads"].as_object().unwrap().keys().cloned().collect();
    let value = |line: &str, key: &str| line.trim().strip_prefix(key).map(|v| v.trim().to_string());

    // Each SecretProviderClass and the cluster token it carries; then each pod's app and the classes it mounts.
    let mut token_of: BTreeMap<String, String> = BTreeMap::new();
    let mut pods: Vec<(String, Vec<String>)> = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(format!("{root}/k8s")).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    for file in files.iter().filter(|p| p.extension().is_some_and(|e| e == "yaml")) {
        for doc in std::fs::read_to_string(file).unwrap().split("\n---") {
            let lines: Vec<&str> = doc.lines().collect();
            let kind = lines.iter().find_map(|l| l.strip_prefix("kind: ")).unwrap_or_default();
            if kind == "SecretProviderClass" {
                let name = lines.iter().find_map(|l| value(l, "name:")).unwrap();
                if let Some(token) = lines.iter().find_map(|l| value(l, "objectName: cluster-token-")) {
                    assert!(token_of.insert(name.clone(), token).is_none(), "{name} is defined twice");
                }
            } else if ["Deployment", "StatefulSet", "CronJob", "Job"].contains(&kind) {
                let app = lines.iter().find_map(|l| value(l, "app:")).unwrap();
                let mounts = lines.iter().filter_map(|l| value(l, "secretProviderClass:")).collect();
                pods.push((app, mounts));
            }
        }
    }

    let mut tokens: Vec<&String> = token_of.values().collect();
    tokens.sort();
    let mut expected: Vec<&String> = granted.iter().collect();
    expected.sort();
    assert_eq!(tokens, expected, "the tokens in deploy/k8s and the grants differ");

    for (app, mounts) in &pods {
        let owner = SAME_WORKLOAD.iter().find(|(a, _)| a == app).map_or(app.as_str(), |(_, w)| w);
        for class in mounts {
            if let Some(token) = token_of.get(class) {
                assert_eq!(token, owner, "{app} mounts the cluster token of {token}");
            }
        }
    }
    assert!(pods.iter().any(|(app, _)| app == "ic-underwriting"), "the underwriting driver is in deploy/k8s");
}
