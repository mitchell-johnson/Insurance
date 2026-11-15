//! Two regions and a witness: five nodes in this process over real HTTP.
//! Nodes 1 and 2 stand for Sydney, 3 and 4 for Melbourne, and node 5 for
//! the witness in a third region, which votes and keeps the log but holds
//! no state.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use common::{scratch, start_date};
use ic_core::{Command, Envelope};
use ic_log::StateMachine;
use ic_raft::client::fresh_salt;
use ic_raft::failback::FailbackSettings;
use ic_raft::server::{self, ColdTarget, EventTarget, Running, Settings};
use ic_raft::{hosted, ClusterAuth, ClusterClient, ModuleSpec, ModuleState, NodeId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const TOKEN: &str = "test-cluster-token-0123456789abcdef";
const PARTITIONS: u16 = 2;
const WITNESS: NodeId = 5;
/// The module's name falls to the first home, Sydney (`FailbackSettings::home_of`).
const MODULE: &str = "tally";

/// A running total, with every addition answered once by its key.
#[derive(Default, Serialize, Deserialize)]
struct Tally {
    total: i64,
    seen: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
struct Add {
    key: String,
    amount: i64,
}

impl StateMachine for Tally {
    type Command = Add;
    type Output = i64;

    fn apply(&mut self, _index: u64, add: Add) -> i64 {
        if self.seen.insert(add.key) {
            self.total += add.amount;
        }
        self.total
    }
}

fn ask(tally: &Tally, _question: Value) -> Result<Value, String> {
    Ok(json!(tally.total))
}

fn tally() -> Box<dyn ModuleState> {
    hosted(Tally::default(), ask)
}

fn sydney() -> BTreeSet<NodeId> {
    BTreeSet::from([1, 2])
}

fn melbourne() -> BTreeSet<NodeId> {
    BTreeSet::from([3, 4])
}

struct Cluster {
    dir: std::path::PathBuf,
    peers: BTreeMap<NodeId, String>,
    auth: ClusterAuth,
    running: BTreeMap<NodeId, Running>,
    client: ClusterClient,
    added: u64,
}

impl Cluster {
    async fn start(name: &str) -> Self {
        let dir = scratch(name);
        let peers: BTreeMap<NodeId, String> = (1..=5u64).map(|id| (id, format!("127.0.0.1:{}", common::free_port()))).collect();
        let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
        let client = ClusterClient::new(peers.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(auth.clone());
        let mut cluster = Self {
            dir,
            peers,
            auth,
            running: BTreeMap::new(),
            client,
            added: 0,
        };
        for id in 1..=5 {
            cluster.boot(id).await;
        }
        cluster
    }

    async fn boot(&mut self, id: NodeId) {
        let settings = Settings {
            id,
            listen: self.peers[&id].parse().unwrap(),
            data_dir: self.dir.join(format!("node-{id}")),
            partitions: PARTITIONS,
            peers: self.peers.clone(),
            bootstrap: id == 1,
            heartbeat_ms: 50,
            auth: self.auth.clone(),
            cold: ColdTarget::Dir(self.dir.join("cold")),
            events: EventTarget::Discard,
            segment_entries: 10,
            snapshot_every: 1000,
            witnesses: BTreeSet::from([WITNESS]),
        };
        let failback = FailbackSettings {
            homes: vec![sydney(), melbourne()],
            share_percent: 100,
            latency_budget: Duration::from_secs(5),
            settle: Duration::from_millis(400),
            frozen: vec![],
            timezone: chrono_tz::Pacific::Auckland,
        };
        let modules = vec![ModuleSpec::new(MODULE, tally).unwrap()];
        let node = server::start_full(settings, modules, Some(failback)).await.unwrap();
        self.running.insert(id, node);
    }

    async fn lose(&mut self, ids: &BTreeSet<NodeId>) {
        for id in ids {
            self.running.remove(id).unwrap().kill().await;
        }
    }

    /// Each group's leader, partitions first and the module last, as the
    /// running nodes report it. A group whose leader no running node names is left out.
    async fn leaders(&self) -> BTreeMap<String, NodeId> {
        let mut leaders = BTreeMap::new();
        for id in self.running.keys() {
            let addr = &self.peers[id];
            let Ok(partitions) = self.client.status(addr).await else { continue };
            for s in partitions.iter().filter(|s| s.role == "leader") {
                leaders.insert(format!("partition {}", s.partition), *id);
            }
            let Ok(modules) = self.client.module_status(addr).await else { continue };
            for s in modules.iter().filter(|s| s.role == "leader") {
                leaders.insert(format!("module {}", s.module), *id);
            }
        }
        leaders
    }

    /// Wait until every group has a leader and `settled` holds of them.
    async fn until(&self, what: &str, settled: impl Fn(&BTreeMap<String, NodeId>) -> bool) -> BTreeMap<String, NodeId> {
        let mut leaders = BTreeMap::new();
        for _ in 0..600 {
            leaders = self.leaders().await;
            if leaders.len() == PARTITIONS as usize + 1 && settled(&leaders) {
                return leaders;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("{what}: the leaders are {leaders:?}");
    }

    /// A write to every partition and one addition to the tally.
    async fn serves(&mut self, key: &str, amount: i64) {
        for partition in 0..PARTITIONS {
            let envelope = Envelope {
                key: format!("{key}-{partition}"),
                caller: "leader".to_string(),
                salt: fresh_salt(),
                traceparent: None,
                command: Command::OpenBusinessDate { date: start_date() },
            };
            self.client.propose_until(partition, &envelope, Duration::from_secs(60)).await.unwrap();
        }
        self.add(key, amount).await;
    }

    async fn add(&mut self, key: &str, amount: i64) {
        let reply = self
            .client
            .propose_module_until(MODULE, &json!({ "key": key, "amount": amount }), Duration::from_secs(60))
            .await
            .unwrap();
        reply.result.unwrap();
        self.added += 1;
    }

    async fn total(&self) -> i64 {
        self.client.query_module(MODULE, &Value::Null).await.unwrap().unwrap().as_i64().unwrap()
    }

    async fn stop(self) {
        for (_, node) in self.running {
            node.stop().await.unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn led_from(leaders: &BTreeMap<String, NodeId>, group: &str, home: &BTreeSet<NodeId>) -> bool {
    leaders.get(group).is_some_and(|id| home.contains(id))
}

/// Partition 0 and the module are Sydney's; partition 1 is Melbourne's.
fn at_home(leaders: &BTreeMap<String, NodeId>) -> bool {
    led_from(leaders, "partition 0", &sydney()) && led_from(leaders, "partition 1", &melbourne()) && led_from(leaders, "module tally", &sydney())
}

/// Purpose: with two leading regions, the groups' leadership is shared between them: Sydney
/// leads partition 0 and the module, Melbourne partition 1, both taking writes. The witness
/// votes in every group but holds no state: it reports itself a witness, has applied the log
/// without holding a policy, refuses every caller with "not the leader" so the caller tries
/// another node, and refuses to be handed a group's lead by an operator. As the data voters
/// record how far they all hold the log, the witness lets go of the log behind that point.
/// Why it matters: this is the placement Mitchell approved (two voters in each Australian
/// region and a tiebreaker in a third). Both regions taking writes is the "active-active" he
/// asked for; a witness that kept policy data, served reads or let its log grow without end
/// would be a sixth copy of the ledger to secure and size, not a small tiebreaker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_regions_lead_and_the_witness_holds_no_state() {
    let mut cluster = Cluster::start("witness-shares").await;
    cluster.serves("first", 1).await;
    cluster.until("the groups did not settle in their home regions", at_home).await;
    for n in 0..40 {
        cluster.add(&format!("more-{n}"), 1).await;
    }
    assert_eq!(cluster.total().await, 41);

    let witness = cluster.peers[&WITNESS].clone();
    let rows = cluster.client.status(&witness).await.unwrap();
    assert_eq!(rows.len(), PARTITIONS as usize);
    assert!(
        rows.iter().all(|r| r.witness && r.policies == 0 && r.ledger_entries == 0 && r.role != "leader"),
        "{rows:?}"
    );
    let data = cluster.client.status(&cluster.peers[&1]).await.unwrap();
    assert!(data.iter().all(|r| !r.witness));

    // A caller that reaches the witness is sent elsewhere.
    let http = reqwest::Client::new();
    for path in ["v1/partitions/0/commands", "v1/modules/tally/query"] {
        let answer = http
            .post(format!("http://{witness}/{path}"))
            .bearer_auth(TOKEN)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(answer.status(), reqwest::StatusCode::MISDIRECTED_REQUEST, "{path}");
    }
    assert!(cluster.client.move_leader(0, WITNESS).await.is_err(), "the witness was handed a group");

    // The data voters mark how far they all hold the log; the witness drops what is behind.
    let mut first = None;
    for _ in 0..300 {
        let module = cluster.client.module_status(&witness).await.unwrap();
        first = module[0].first_log_index;
        if first.is_some_and(|f| f > 20) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(first.is_some_and(|f| f > 20), "the witness kept its whole log: it starts at {first:?}");
    assert!(cluster.client.module_status(&witness).await.unwrap()[0].witness);
    cluster.stop().await;
}

/// Purpose: losing a whole region loses no acknowledged write and stops nothing: the other
/// region takes over its groups at once. Then the harder case: Melbourne is down while Sydney
/// and the witness commit writes, and Sydney is lost before Melbourne returns. The witness is
/// the only survivor holding those writes; it takes the lead, sends them to Melbourne and hands
/// the lead to a Melbourne voter, so every write is there and the cluster serves. When Sydney
/// returns, its groups go back to it by themselves, and the data voters agree on every state.
/// Why it matters: this is the automatic fail-over between regions that the placement exists
/// for, and the case that decides whether the witness may keep no log at all: it may not, or
/// the writes committed by Sydney and the witness alone would be lost with Sydney.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_whole_region_can_be_lost_and_the_witness_carries_what_only_it_holds() {
    let mut cluster = Cluster::start("witness-region").await;
    cluster.serves("before", 1).await;
    cluster.until("the groups did not settle in their home regions", at_home).await;

    // Sydney is lost: Melbourne leads everything and serves.
    cluster.lose(&sydney()).await;
    cluster
        .until("Melbourne did not take over", |l| l.values().all(|id| melbourne().contains(id)))
        .await;
    cluster.serves("sydney-lost", 10).await;
    assert_eq!(cluster.total().await, 11);
    cluster.boot(1).await;
    cluster.boot(2).await;
    cluster.until("the groups did not go home after Sydney returned", at_home).await;

    // Melbourne goes down; Sydney and the witness carry on alone.
    cluster.lose(&melbourne()).await;
    cluster.serves("melbourne-down", 100).await;
    assert_eq!(cluster.total().await, 111);

    // Sydney is lost before Melbourne returns: only the witness holds the last writes.
    cluster.lose(&sydney()).await;
    cluster.boot(3).await;
    cluster.boot(4).await;
    cluster
        .until("Melbourne did not take over from the witness", |l| {
            l.values().all(|id| melbourne().contains(id))
        })
        .await;
    assert_eq!(cluster.total().await, 111, "a write held only by Sydney and the witness was lost");
    let handed = cluster.client.failback(&cluster.peers[&WITNESS], None).await.unwrap().moved;
    assert!(handed > 0, "Melbourne took over without the witness leading: the case was not staged");
    cluster.serves("witness-carried", 1000).await;
    assert_eq!(cluster.total().await, 1111);

    // Sydney returns and takes its groups back by itself.
    cluster.boot(1).await;
    cluster.boot(2).await;
    let leaders = cluster.until("the groups did not go home", at_home).await;
    assert!(!leaders.values().any(|id| *id == WITNESS));
    cluster.serves("after", 10_000).await;
    assert_eq!(cluster.total().await, 11_111);

    // Every data voter that has applied the same entries holds the same state.
    let mut hashes: BTreeMap<(String, Option<u64>), BTreeSet<String>> = BTreeMap::new();
    for _ in 0..100 {
        hashes.clear();
        for id in [1, 2, 3, 4] {
            for row in cluster.client.status(&cluster.peers[&id]).await.unwrap() {
                hashes
                    .entry((format!("partition {}", row.partition), row.last_applied))
                    .or_default()
                    .insert(row.state_hash);
            }
            for row in cluster.client.module_status(&cluster.peers[&id]).await.unwrap() {
                hashes.entry((row.module, row.last_applied)).or_default().insert(row.state_hash);
            }
        }
        if hashes.len() == PARTITIONS as usize + 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(hashes.values().all(|h| h.len() == 1), "data voters differ: {hashes:?}");
    cluster.stop().await;
}

/// Purpose: a data voter's partition and module members both refuse to install a witness's
/// marker as their state, and the network refuses to carry one as a snapshot at all.
/// Why it matters: a witness that leads for a moment could be asked by Raft for a snapshot to
/// send to a lagging data voter. Installing its marker would replace that voter's ledger with
/// nothing; these are the two places that stop it.
#[tokio::test]
async fn a_data_voter_never_takes_the_witness_marker_for_a_state() {
    use ic_raft::module::ModuleMachine;
    use ic_raft::network::{LocalNet, Network};
    use ic_raft::witness::MARKER;
    use ic_raft::{Identity, Machine};
    use openraft::alias::{SnapshotMetaOf, SnapshotOf, VoteOf};
    use openraft::network::{RPCOption, RaftNetworkFactory, RaftNetworkV2};
    use openraft::storage::RaftStateMachine;
    use openraft::BasicNode;

    let dir = scratch("witness-marker");
    let meta = SnapshotMetaOf::<ic_raft::TypeConfig>::default();
    let mut machine = Machine::open(0, dir.join("partition")).unwrap();
    let refused = machine.install_snapshot(&meta, MARKER.to_vec()).await.unwrap_err();
    assert!(refused.to_string().contains("never installs"), "{refused}");
    let mut module = ModuleMachine::open(ModuleSpec::new(MODULE, tally).unwrap(), dir.join("module")).unwrap();
    let refused = module.install_snapshot(&meta, MARKER.to_vec()).await.unwrap_err();
    assert!(refused.to_string().contains("never installs"), "{refused}");

    let mut network = Network::local(Identity::open(5, dir.join("identity")).unwrap(), LocalNet::default());
    let mut connection = network.new_client(1, &BasicNode::default()).await;
    let snapshot = SnapshotOf::<ic_raft::TypeConfig, Vec<u8>> {
        meta,
        snapshot: MARKER.to_vec(),
    };
    let sent = connection
        .full_snapshot(
            VoteOf::<ic_raft::TypeConfig>::new(1, 5),
            snapshot,
            std::future::pending(),
            RPCOption::new(Duration::from_secs(1)),
        )
        .await
        .unwrap_err();
    assert!(sent.to_string().contains("no state to send"), "{sent}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: as the state machine Raft drives, a witness's partition member
/// and module member take no snapshot while the data voters have marked
/// nothing new, and take a leader's snapshot as a position: the witness
/// moves to it and keeps no state. A durable mark names what it covers.
/// Why it matters: these are the calls openraft makes on a witness. A
/// snapshot taken without a new mark would cut log a lost region may still
/// need; one installed as state would put the ledger on the one machine
/// meant to hold none.
#[tokio::test]
async fn a_witness_member_snapshots_only_at_a_mark_and_installs_only_a_position() {
    use openraft::storage::RaftStateMachine;
    use openraft::testing::log_id;
    use openraft::{Membership, StoredMembership};

    let meta = openraft::SnapshotMeta {
        last_log_id: Some(log_id::<ic_raft::TypeConfig>(2, 1, 40)),
        last_membership: StoredMembership::new(
            Some(log_id::<ic_raft::TypeConfig>(1, 1, 3)),
            Membership::new_with_defaults(vec![BTreeSet::from([1, 2, 3, 4, 5])], []),
        ),
    };

    let mut partition = ic_raft::Machine::open_witness(0, scratch("witness-machine")).unwrap();
    assert!(partition.try_create_snapshot_builder(false).await.is_none(), "no mark, no marker");
    partition.install_snapshot(&meta, b"a whole partition".to_vec()).await.unwrap();
    assert_eq!(partition.witness().unwrap().last_applied_index(), Some(40));

    let spec = ModuleSpec::new(MODULE, tally).unwrap();
    let mut module = ic_raft::module::ModuleMachine::open_witness(spec, scratch("witness-module")).unwrap();
    assert!(module.try_create_snapshot_builder(false).await.is_none(), "no mark, no marker");
    module.install_snapshot(&meta, b"a whole module".to_vec()).await.unwrap();
    assert_eq!(module.witness().unwrap().last_applied_index(), Some(40));

    let mark = ic_raft::Proposal::Durable {
        through: log_id::<ic_raft::TypeConfig>(2, 1, 40),
    };
    assert_eq!(mark.to_string(), "every data voter holds the log through 40");
}

/// Purpose: a witness whose marker file cannot be read for any reason but
/// its absence refuses to start, rather than starting from nothing.
/// Why it matters: starting from nothing would forget the membership and
/// the point the log was cut at, and the witness would vote as a member of
/// a group it no longer knows.
#[test]
fn a_witness_marker_that_cannot_be_read_stops_the_start() {
    let dir = scratch("witness-unreadable");
    std::fs::create_dir_all(dir.join("witness.json")).unwrap();
    assert!(ic_raft::witness::Witness::open(&dir).is_err());
}
