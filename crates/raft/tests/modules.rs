//! A module's own replicated group on the ledger nodes: three nodes in this
//! process, talking over real HTTP with the cluster token, hosting a small
//! module beside a partition.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{d, scratch, start_date};
use ic_core::{Command, Envelope, EventRecord};
use ic_log::StateMachine;
use ic_product::{Frequency, Sex};
use ic_raft::client::fresh_salt;
use ic_raft::client::ClientError;
use ic_raft::module::{ModuleMachine, ModuleStatus};
use ic_raft::server::{self, ColdTarget, EventTarget, Running, Settings};
use ic_raft::{hosted, hosted_reading, ClusterAuth, ClusterClient, ModuleSpec, ModuleState, NodeId, Proposal};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const TOKEN: &str = "test-cluster-token-0123456789abcdef";

/// A module small enough to reason about: running totals by account, where a
/// command seen before is answered with what it was answered the first time.
#[derive(Default, Serialize, Deserialize)]
struct Tally {
    totals: BTreeMap<String, i64>,
    answered: BTreeMap<String, i64>,
}

#[derive(Serialize, Deserialize)]
struct Add {
    key: String,
    account: String,
    amount: i64,
}

impl StateMachine for Tally {
    type Command = Add;
    type Output = i64;

    fn apply(&mut self, _index: u64, add: Add) -> i64 {
        if let Some(original) = self.answered.get(&add.key) {
            return *original;
        }
        let total = self.totals.entry(add.account).or_default();
        *total += add.amount;
        self.answered.insert(add.key, *total);
        *total
    }
}

fn ask(tally: &Tally, question: Value) -> Result<Value, String> {
    let account = question["account"].as_str().ok_or("ask for an account")?;
    Ok(json!(tally.totals.get(account).copied().unwrap_or(0)))
}

fn tally() -> Box<dyn ModuleState> {
    hosted(Tally::default(), ask)
}

fn other() -> Box<dyn ModuleState> {
    hosted(Tally::default(), ask)
}

/// A module that reads the core's events and keeps the sequence numbers it
/// was shown, in the order it was shown them.
#[derive(Default, Serialize, Deserialize)]
struct Watch {
    seen: BTreeMap<u16, Vec<u64>>,
}

#[derive(Serialize, Deserialize)]
struct Seen {
    partition: u16,
    seq: u64,
}

impl StateMachine for Watch {
    type Command = Seen;
    type Output = ();

    fn apply(&mut self, _index: u64, seen: Seen) {
        self.seen.entry(seen.partition).or_default().push(seen.seq);
    }
}

fn shown(watch: &Watch, question: Value) -> Result<Value, String> {
    let partition = question["partition"].as_u64().ok_or("ask for a partition")?;
    Ok(json!(watch.seen.get(&(partition as u16)).cloned().unwrap_or_default()))
}

fn react(event: &EventRecord) -> Vec<Seen> {
    vec![Seen {
        partition: event.partition,
        seq: event.seq,
    }]
}

fn watch() -> Box<dyn ModuleState> {
    hosted_reading(Watch::default(), shown, react)
}

fn specs() -> Vec<ModuleSpec> {
    vec![
        ModuleSpec::new("tally", tally).unwrap(),
        ModuleSpec::new("watch", watch).unwrap().reading_events(),
    ]
}

struct Cluster {
    dir: std::path::PathBuf,
    peers: BTreeMap<NodeId, String>,
    auth: ClusterAuth,
    snapshot_every: u64,
    running: BTreeMap<NodeId, Running>,
    client: ClusterClient,
}

impl Cluster {
    async fn start(name: &str, snapshot_every: u64) -> Self {
        let dir = scratch(name);
        let peers: BTreeMap<NodeId, String> = (1..=3u64)
            .map(|id| {
                let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
                (id, format!("127.0.0.1:{port}"))
            })
            .collect();
        let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
        let client = ClusterClient::new(peers.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(auth.clone());
        let mut cluster = Self {
            dir,
            peers,
            auth,
            snapshot_every,
            running: BTreeMap::new(),
            client,
        };
        for id in 1..=3 {
            cluster.boot(id).await;
        }
        cluster
    }

    fn settings(&self, id: NodeId) -> Settings {
        Settings {
            id,
            listen: self.peers[&id].parse().unwrap(),
            data_dir: self.dir.join(format!("node-{id}")),
            partitions: 1,
            peers: self.peers.clone(),
            bootstrap: id == 1,
            heartbeat_ms: 50,
            auth: self.auth.clone(),
            cold: ColdTarget::Dir(self.dir.join("cold")),
            events: EventTarget::Discard,
            segment_entries: 10,
            snapshot_every: self.snapshot_every,
        }
    }

    async fn boot(&mut self, id: NodeId) {
        self.running.insert(id, server::start_with(self.settings(id), specs()).await.unwrap());
    }

    async fn kill(&mut self, id: NodeId) {
        self.running.remove(&id).unwrap().kill().await;
    }

    async fn add(&self, key: &str, account: &str, amount: i64) -> Result<Value, String> {
        let command = json!({ "key": key, "account": account, "amount": amount });
        self.client
            .propose_module_until("tally", &command, Duration::from_secs(30))
            .await
            .unwrap()
            .result
    }

    async fn total(&self, account: &str) -> i64 {
        for _ in 0..100 {
            if let Ok(answer) = self.client.query_module("tally", &json!({ "account": account })).await {
                return answer.unwrap().as_i64().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("no leader answered a question about {account}");
    }

    async fn status(&self, id: NodeId) -> ModuleStatus {
        self.client.module_status(&self.peers[&id]).await.unwrap().remove(0)
    }

    async fn watch_status(&self, id: NodeId) -> ModuleStatus {
        self.client.module_status(&self.peers[&id]).await.unwrap().remove(1)
    }

    /// Run a command on partition 0 and insist it was accepted.
    async fn run(&self, key: &str, caller: &str, command: Command) {
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

    async fn issue(&self, n: u64) {
        let application = term_life(n, start_date(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        self.run(&format!("issue-{n}"), "new-business", issue_command(application)).await;
    }

    /// The partition's last event number and how many events its leader still holds.
    async fn partition_events(&self) -> (u64, u64) {
        for _ in 0..100 {
            for id in self.running.keys() {
                let status = self.client.status(&self.peers[id]).await.unwrap().remove(0);
                if status.role == "leader" {
                    let last = self.running[id].host.groups[&0].machine.read(|s| s.last_event_seq());
                    return (last, status.backlog.events);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("partition 0 elected no leader");
    }

    /// What the watching module was shown of partition 0, once it has
    /// been shown everything up to `last`.
    async fn watched(&self, last: u64) -> Vec<u64> {
        let mut seen = Vec::new();
        for _ in 0..300 {
            if let Ok(Ok(answer)) = self.client.query_module("watch", &json!({ "partition": 0 })).await {
                seen = serde_json::from_value(answer).unwrap();
                if seen.last() == Some(&last) {
                    return seen;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the module was not shown events through {last}; it has {:?}", seen.last());
    }

    async fn watch_leader(&self) -> NodeId {
        for _ in 0..100 {
            for id in self.running.keys() {
                if self.watch_status(*id).await.role == "leader" {
                    return *id;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the watching module's group elected no leader");
    }

    async fn leader(&self) -> NodeId {
        for _ in 0..100 {
            for id in self.running.keys() {
                if self.status(*id).await.role == "leader" {
                    return *id;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the module's group elected no leader");
    }

    /// Wait until every running member has applied the same entries, and
    /// return the state hash they share.
    async fn level(&self) -> String {
        let mut seen = Vec::new();
        for _ in 0..300 {
            seen.clear();
            for id in self.running.keys() {
                seen.push(self.status(*id).await);
            }
            let first = &seen[0];
            if seen.iter().all(|s| s.last_applied == first.last_applied && s.state_hash == first.state_hash) {
                return first.state_hash.clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the members did not come level: {seen:?}");
    }

    async fn stop(self) {
        for (_, node) in self.running {
            node.stop().await.unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Purpose: a command sent to a module is applied by every member of the module's group and
/// they end with the same state hash; the same command sent again gets its first answer and
/// changes nothing; a command the module cannot read is refused by the module and changes
/// nothing; a question is answered by the leader. The operator's routes name a module the
/// node does not host, refuse an empty voter set, and accept the voter set already in force.
/// Why it matters: reinsurance accumulation, commission accounts and the finance ledger live
/// in groups like this one. If a member could apply a command differently, or a retried
/// command could count twice, those balances would differ between nodes and change with
/// whichever node happened to lead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_module_command_is_applied_once_by_every_member() {
    let cluster = Cluster::start("module-replicates", 1000).await;

    assert_eq!(cluster.add("a-1", "life-7", 300).await, Ok(json!(300)));
    assert_eq!(cluster.add("a-2", "life-7", 200).await, Ok(json!(500)));
    assert_eq!(cluster.add("a-3", "life-9", 40).await, Ok(json!(40)));
    // The caller did not hear the answer to a-2 and sends it again.
    assert_eq!(cluster.add("a-2", "life-7", 200).await, Ok(json!(500)));
    assert_eq!(cluster.total("life-7").await, 500);
    let before = cluster.level().await;

    // Not a command of this module: refused by the module, on every member alike.
    let refused = cluster.client.propose_module("tally", &json!({ "account": 12 })).await.unwrap();
    assert!(
        refused.result.as_ref().is_err_and(|e| e.contains("not a command of this module")),
        "{refused:?}"
    );
    assert_eq!(cluster.level().await, before, "a refused command changed the module's state");
    // A question the module cannot answer is the module's refusal, not a failed node.
    assert_eq!(
        cluster.client.query_module("tally", &json!({})).await.unwrap(),
        Err("ask for an account".to_string())
    );

    for id in 1..=3 {
        let status = cluster.status(id).await;
        assert_eq!(status.module, "tally");
        assert_eq!(status.voters, vec![1, 2, 3]);
        assert_eq!(status.state_hash, before);
    }

    // The operator's routes.
    let unknown = cluster.client.propose_module("ghost", &json!({})).await;
    assert!(
        matches!(&unknown, Err(ClientError::ModuleUnavailable { module, detail }) if module == "ghost" && detail.contains("hosts no module ghost")),
        "{unknown:?}"
    );
    assert!(cluster.client.query_module("ghost", &json!({})).await.is_err());
    assert!(cluster.client.set_module_members("ghost", &cluster.peers).await.is_err());
    let nobody = cluster.client.set_module_members("tally", &BTreeMap::new()).await;
    assert!(
        matches!(&nobody, Err(ClientError::ModuleUnavailable { detail, .. }) if detail.contains("at least one member")),
        "{nobody:?}"
    );
    cluster.client.set_module_members("tally", &cluster.peers).await.unwrap();
    assert_eq!(cluster.level().await, before);
    // The partition beside it is untouched by any of this and still answers.
    assert_eq!(cluster.client.status(&cluster.peers[&1]).await.unwrap().len(), 1);
    cluster.stop().await;
}

/// Purpose: when the node leading a module's group is lost, the two that remain elect a
/// leader, keep every total that was confirmed, and accept new commands; a command whose
/// answer was lost with the leader, sent again, is not counted twice; and the lost node,
/// started again on its disk, replays its log and comes level with the same state hash.
/// Why it matters: a module's group has no other copy of its state. A confirmed cession or
/// commission entry that vanished with a node, or was applied a second time on retry, is a
/// wrong balance with a reinsurer or an adviser.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_module_survives_the_loss_of_its_leader() {
    let mut cluster = Cluster::start("module-leader-loss", 1000).await;
    for n in 1..=10 {
        cluster.add(&format!("k-{n}"), "adviser-3", 10).await.unwrap();
    }
    let leader = cluster.leader().await;
    cluster.kill(leader).await;

    // The caller cannot know whether k-10 was confirmed, and sends it again.
    assert_eq!(cluster.add("k-10", "adviser-3", 10).await, Ok(json!(100)));
    assert_eq!(cluster.add("k-11", "adviser-3", 10).await, Ok(json!(110)));
    assert_eq!(cluster.total("adviser-3").await, 110);
    assert_ne!(cluster.leader().await, leader);

    cluster.boot(leader).await;
    let hash = cluster.level().await;
    assert_eq!(cluster.status(leader).await.state_hash, hash);
    assert_eq!(cluster.total("adviser-3").await, 110);
    cluster.stop().await;
}

/// Purpose: a member that was down while the others wrote past a snapshot and dropped the
/// log behind it is sent the snapshot, then the entries after it, and ends with the same
/// state hash. Every node is then stopped and started: each comes back from its snapshot
/// and its log with the totals it had.
/// Why it matters: a module's log is not kept forever, so a replacement node or a node back
/// from a long outage can only rejoin through a snapshot; and a full restart, as in a
/// region evacuation, must bring the state back from disk alone. If either failed, the
/// group would lose a member for good or lose its state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_module_member_that_fell_behind_is_caught_up_by_snapshot() {
    let mut cluster = Cluster::start("module-snapshot", 15).await;
    cluster.add("s-0", "fund", 1).await.unwrap();
    cluster.kill(3).await;
    for n in 1..=40 {
        cluster.add(&format!("s-{n}"), "fund", 1).await.unwrap();
    }
    let survivor = cluster.leader().await;
    let mut purged = false;
    for _ in 0..100 {
        if cluster.status(survivor).await.first_log_index.is_some_and(|first| first > 20) {
            purged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(purged, "the log was not purged behind a snapshot: {:?}", cluster.status(survivor).await);

    cluster.boot(3).await;
    let hash = cluster.level().await;
    assert_eq!(cluster.total("fund").await, 41);

    for id in 1..=3 {
        cluster.kill(id).await;
    }
    for id in 1..=3 {
        cluster.boot(id).await;
    }
    assert_eq!(cluster.total("fund").await, 41);
    assert_eq!(cluster.level().await, hash, "the state after a full restart is not the state before it");
    cluster.stop().await;
}

/// Purpose: a node refuses to start a module on a directory that holds another module's
/// snapshot, or a file that is not a snapshot; a module's name is checked when it is
/// declared; a node told to host the same module twice does not start; and a member given
/// an entry that is not a module command answers that it is not one and stays unchanged.
/// Why it matters: these are the mistakes of a rushed deployment, a disk attached to the
/// wrong workload or a copied line of configuration. Starting anyway would put one
/// module's balances under another's name, where nothing later would notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_module_refuses_state_that_is_not_its_own() {
    let dir = scratch("module-wrong-state");
    assert!(ModuleSpec::new("Tally", tally).is_err());
    assert!(ModuleSpec::new("", tally).is_err());
    assert!(ModuleSpec::new("re insurance", tally).is_err());

    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let settings = Settings {
        id: 1,
        listen: addr.parse().unwrap(),
        data_dir: dir.join("node-1"),
        partitions: 1,
        peers: BTreeMap::from([(1, addr.clone())]),
        bootstrap: true,
        heartbeat_ms: 50,
        auth: ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap(),
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 10,
        snapshot_every: 5,
    };
    let twice = vec![ModuleSpec::new("tally", tally).unwrap(), ModuleSpec::new("tally", tally).unwrap()];
    let refused = server::start_with(settings.clone(), twice).await.err().unwrap();
    assert!(refused.contains("listed twice"), "{refused}");

    // One node, one module, enough commands for a snapshot to be on disk.
    let node = server::start_with(settings.clone(), vec![ModuleSpec::new("tally", tally).unwrap()])
        .await
        .unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(settings.auth.clone());
    for n in 0..12 {
        let command = json!({ "key": format!("w-{n}"), "account": "x", "amount": 1 });
        client.propose_module_until("tally", &command, Duration::from_secs(30)).await.unwrap();
    }
    let snapshot = settings.data_dir.join("module-tally").join("snapshot.json");
    for _ in 0..100 {
        if snapshot.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(snapshot.exists(), "no snapshot was written");

    // An entry of a partition's kind in a module's log is answered as what it is.
    let member = &node.host.modules["tally"];
    let before = member.status().state_hash;
    let stray = Proposal::Housekeeping(ic_raft::types::Housekeeping::Published { through_seq: 1 });
    let reply = member.raft.client_write(stray).await.unwrap().data;
    assert_eq!(reply.module, Some(Err("this group is a module, not a ledger partition".to_string())));
    assert_eq!(member.status().state_hash, before);
    // And a module's entry in a partition's log likewise.
    let partition = &node.host.groups[&0];
    let hash = partition.status().state_hash;
    let reply = partition.raft.client_write(Proposal::Module(json!({}))).await.unwrap().data;
    assert_eq!(reply.module, Some(Err("this group is a ledger partition, not a module".to_string())));
    assert_eq!(partition.status().state_hash, hash);
    assert_eq!(Proposal::Module(json!({})).to_string(), "a module command");
    node.stop().await.unwrap();

    // The same disk offered to a module of another name.
    let elsewhere = dir.join("other");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::copy(&snapshot, elsewhere.join("snapshot.json")).unwrap();
    let wrong = ModuleMachine::open(ModuleSpec::new("other", other).unwrap(), &elsewhere).err().unwrap();
    assert!(wrong.to_string().contains("is of module tally, not other"), "{wrong}");
    // A snapshot whose state the module cannot read.
    let saved: Value = serde_json::from_slice(&std::fs::read(&snapshot).unwrap()).unwrap();
    let mut broken = saved.clone();
    broken["state"] = json!("not a tally");
    std::fs::write(elsewhere.join("snapshot.json"), serde_json::to_vec(&broken).unwrap()).unwrap();
    let wrong = ModuleMachine::open(ModuleSpec::new("tally", tally).unwrap(), &elsewhere).err().unwrap();
    assert!(wrong.to_string().contains("cannot load its snapshot"), "{wrong}");
    // A file that is no snapshot at all.
    std::fs::write(elsewhere.join("snapshot.json"), b"{").unwrap();
    let wrong = ModuleMachine::open(ModuleSpec::new("tally", tally).unwrap(), &elsewhere).err().unwrap();
    assert!(wrong.to_string().contains("is not a module snapshot"), "{wrong}");
    // The real one opens, with the totals it was written with.
    std::fs::copy(&snapshot, elsewhere.join("snapshot.json")).unwrap();
    let opened = ModuleMachine::open(ModuleSpec::new("tally", tally).unwrap(), &elsewhere).unwrap();
    assert!(opened.query(json!({ "account": "x" })).unwrap().as_i64().unwrap() >= 5);
    assert!(opened.last_applied_index().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: a module that reads the core's events is shown every event of a partition
/// exactly once and in sequence, on every member, including across the loss of the node
/// feeding it; the partition keeps its events until the module has them and lets them go
/// afterwards; a reader outside the cluster can register, read from its own checkpoint,
/// acknowledge and unsubscribe; and a run of events that would leave a gap, is out of
/// sequence or belongs to another partition is refused whole.
/// Why it matters: commission, reinsurance accumulation and the finance ledger are built
/// from these events. One event missed is a premium with no cession or a sale with no
/// commission; one event seen twice is the same money counted twice. Neither would show
/// up as an error anywhere, only as balances that drift from the ledger.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_module_is_shown_every_event_once_and_in_order() {
    let mut cluster = Cluster::start("module-events", 1000).await;
    cluster.run("open", "leader", Command::OpenBusinessDate { date: start_date() }).await;
    for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
        cluster.run(&format!("activate-{n}"), "config", activation).await;
    }
    for n in 1..=5 {
        cluster.issue(n).await;
    }
    let (last, _) = cluster.partition_events().await;
    assert!(last >= 5, "five policies were issued and {last} events produced");
    let everything = |last: u64| (1..=last).collect::<Vec<u64>>();
    assert_eq!(cluster.watched(last).await, everything(last));

    // The node feeding the module is lost part-way through more work.
    let feeder = cluster.watch_leader().await;
    cluster.issue(6).await;
    cluster.kill(feeder).await;
    for n in 7..=10 {
        cluster.issue(n).await;
    }
    let (last, _) = cluster.partition_events().await;
    assert_eq!(
        cluster.watched(last).await,
        everything(last),
        "an event was missed or shown twice across the loss of the feeder"
    );
    cluster.boot(feeder).await;

    // Every member of the module's group holds the same checkpoint and state.
    let mut level = false;
    for _ in 0..300 {
        let (a, b, c) = (cluster.watch_status(1).await, cluster.watch_status(2).await, cluster.watch_status(3).await);
        if a.state_hash == b.state_hash && b.state_hash == c.state_hash && a.cursors.get(&0) == Some(&last) {
            level = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(level, "the module's members did not come level at event {last}");
    // Once the module has acknowledged them, the partition holds no events back.
    let mut released = false;
    for _ in 0..100 {
        if cluster.partition_events().await.1 == 0 {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(released, "the partition still holds events the module has acknowledged");

    // A reader outside the cluster. Registering makes the partition keep events for it.
    let client = &cluster.client;
    assert_eq!(client.events(0, "outside", None, 10).await.unwrap().acknowledged, None);
    client.acknowledge_events(0, "outside", 0).await.unwrap();
    let registered = client.events(0, "outside", None, 10).await.unwrap();
    assert_eq!((registered.after, registered.acknowledged, registered.events.len()), (last, Some(last), 0));
    cluster.issue(11).await;
    let (newest, _) = cluster.partition_events().await;
    cluster.watched(newest).await;
    let page = client.events(0, "outside", None, 500).await.unwrap();
    assert_eq!(page.events.iter().map(|e| e.seq).collect::<Vec<_>>(), ((last + 1)..=newest).collect::<Vec<_>>());
    assert_eq!(client.events(0, "outside", Some(newest - 1), 500).await.unwrap().events.len(), 1);
    assert_eq!(client.events(0, "outside", Some(last), 1).await.unwrap().events.len(), 1);
    // The module has them and the stream has them, but the outside reader does not: kept.
    assert!(cluster.partition_events().await.1 >= newest - last);
    // An acknowledgement beyond what exists is held to what exists.
    client.acknowledge_events(0, "outside", newest + 50).await.unwrap();
    assert_eq!(client.events(0, "outside", None, 10).await.unwrap().acknowledged, Some(newest));
    cluster.issue(12).await;
    let (newest, _) = cluster.partition_events().await;
    cluster.watched(newest).await;
    assert!(cluster.partition_events().await.1 >= 1);
    client.unsubscribe(0, "outside").await.unwrap();
    let mut released = false;
    for _ in 0..100 {
        if cluster.partition_events().await.1 == 0 {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(released, "events were kept for a reader that had unsubscribed");
    assert!(client.acknowledge_events(7, "outside", 1).await.is_err());
    assert!(client.unsubscribe(7, "outside").await.is_err());
    assert!(client.events(7, "outside", None, 1).await.is_err());

    // Runs the group must refuse, offered straight to its leader.
    let leader = cluster.watch_leader().await;
    let group = &cluster.running[&leader].host.modules["watch"];
    let before = group.status().state_hash;
    let event = |partition: u16, seq: u64| {
        let mut event = page.events[0].clone();
        event.partition = partition;
        event.seq = seq;
        event
    };
    let gap = group.feed(0, newest + 3, vec![event(0, newest + 4)]).await.unwrap();
    assert!(gap.as_ref().is_err_and(|e| e.contains("would leave a gap")), "{gap:?}");
    let jumbled = group.feed(0, newest, vec![event(0, newest + 2)]).await.unwrap();
    assert!(jumbled.as_ref().is_err_and(|e| e.contains("not in sequence")), "{jumbled:?}");
    let foreign = group.feed(0, newest, vec![event(1, newest + 1)]).await.unwrap();
    assert!(foreign.as_ref().is_err_and(|e| e.contains("not in sequence")), "{foreign:?}");
    // A run it already has is taken as read and changes nothing.
    assert_eq!(group.feed(0, newest - 1, vec![event(0, newest)]).await.unwrap(), Ok(newest));
    assert_eq!(group.status().state_hash, before);
    // A partition it has never heard from starts where its first run starts.
    assert_eq!(group.feed(9, 40, vec![event(9, 41)]).await.unwrap(), Ok(41));
    assert_eq!(group.machine.cursors().get(&9), Some(&41));
    // A partition's own log answers a run of events as not its business.
    let partition = &cluster.running[&leader].host.groups[&0];
    let stray = Proposal::Feed {
        partition: 0,
        after: 0,
        events: vec![],
    };
    assert_eq!(stray.to_string(), "0 events of partition 0 after 0");
    if let Ok(reply) = partition.raft.client_write(stray).await {
        assert!(reply.data.module.is_some_and(|m| m.is_err()));
    }
    cluster.stop().await;
}
