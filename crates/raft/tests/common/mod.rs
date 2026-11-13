//! A Raft group inside one process, with the faults the resilience tests need:
//! crash a member (its task stops, its disk stays), cut it off the network
//! while it keeps running, and bring it back.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::{Command, CoreState, Envelope, PolicyId};
use ic_log::StateMachine as _;
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_raft::network::{LocalNet, Network};
use ic_raft::{Identity, LogStore, Machine, Node, NodeId, NodeStatus, Reply};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use openraft::{BasicNode, Config};

pub const PARTITION: u16 = 0;
/// How long a test waits for the group to do something it should do.
pub const PATIENCE: Duration = Duration::from_secs(20);

static RUN: AtomicU64 = AtomicU64::new(0);

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn start_date() -> NaiveDate {
    d(2027, 11, 15)
}

/// A local port that no other test in this process has been given. Asking
/// the system for a free port and letting it go can give two clusters that
/// start at the same moment the same port: one then fails to listen, and its
/// nodes send their log to the other cluster's.
pub fn free_port() -> u16 {
    static GIVEN: std::sync::Mutex<BTreeSet<u16>> = std::sync::Mutex::new(BTreeSet::new());
    let mut given = GIVEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        if given.insert(port) {
            return port;
        }
    }
}

/// A fresh directory for one test's node disks.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct Group {
    pub net: LocalNet,
    pub config: Arc<Config>,
    pub dir: PathBuf,
    pub size: u64,
    /// Members whose process is running.
    pub nodes: BTreeMap<NodeId, Node>,
    /// Running members that are cut off the network.
    pub cut_off: BTreeSet<NodeId>,
    counter: u64,
}

impl Group {
    /// Start `size` members with durable logs and form the group.
    pub async fn start(name: &str, size: u64) -> Self {
        // Far more entries than any test writes: no snapshot is taken.
        Self::start_snapshotting(name, size, 100_000).await
    }

    /// Start a group that snapshots, and drops the log behind the snapshot,
    /// every `snapshot_every` entries.
    pub async fn start_snapshotting(name: &str, size: u64, snapshot_every: u64) -> Self {
        let mut g = Self {
            net: LocalNet::default(),
            config: ic_raft::node::config("test", 50, snapshot_every).unwrap(),
            dir: scratch(name),
            size,
            nodes: BTreeMap::new(),
            cut_off: BTreeSet::new(),
            counter: 0,
        };
        for id in 1..=size {
            g.boot(id).await;
        }
        let members = (1..=size).map(|id| (id, BasicNode::new(format!("local-{id}")))).collect();
        g.nodes[&1].initialize(members).await.unwrap();
        g.leader().await;
        g
    }

    pub fn disk(&self, id: NodeId) -> PathBuf {
        self.dir.join(format!("node-{id}"))
    }

    /// Start a member's process over whatever its disk holds.
    pub async fn boot(&mut self, id: NodeId) {
        let log = LogStore::open(self.disk(id)).unwrap();
        let machine = Machine::open(PARTITION, self.disk(id)).unwrap();
        let identity = Identity::open(id, self.disk(id)).unwrap();
        let node = Node::start(
            identity.clone(),
            PARTITION,
            log,
            machine,
            Network::local(identity, self.net.clone()),
            self.config.clone(),
        )
        .await
        .unwrap();
        self.net.register(node.identity.clone(), node.raft.clone());
        self.cut_off.remove(&id);
        self.nodes.insert(id, node);
    }

    /// Kill a member's process. Everything in memory is lost; its disk stays.
    pub async fn crash(&mut self, id: NodeId) {
        self.net.isolate(id);
        if let Some(node) = self.nodes.remove(&id) {
            node.shutdown().await;
        }
        self.cut_off.remove(&id);
    }

    /// Cut a running member off the network: it keeps running and keeps its
    /// memory, but no message reaches it or leaves it.
    pub fn isolate(&mut self, id: NodeId) {
        self.net.isolate(id);
        self.cut_off.insert(id);
    }

    pub fn reconnect(&mut self, id: NodeId) {
        self.net.restore(id);
        self.cut_off.remove(&id);
    }

    /// Running, connected members.
    pub fn reachable(&self) -> Vec<&Node> {
        self.nodes.values().filter(|n| !self.cut_off.contains(&n.id)).collect()
    }

    pub fn statuses(&self) -> Vec<NodeStatus> {
        self.reachable().into_iter().map(Node::status).collect()
    }

    /// The leader a majority of the whole group follows, if there is one now.
    pub fn leader_now(&self) -> Option<NodeId> {
        let statuses = self.statuses();
        let leader = statuses.iter().find(|s| s.role == "leader")?.id;
        let followers = statuses.iter().filter(|s| s.leader == Some(leader)).count() as u64;
        (followers > self.size / 2).then_some(leader)
    }

    /// Wait for a leader that a majority follows.
    pub async fn leader(&self) -> NodeId {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            if let Some(leader) = self.leader_now() {
                return leader;
            }
            assert!(tokio::time::Instant::now() < deadline, "no leader was elected: {:?}", self.statuses());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn followers_of(&self, leader: NodeId) -> Vec<NodeId> {
        self.reachable().into_iter().map(|n| n.id).filter(|id| *id != leader).collect()
    }

    pub fn envelope(&mut self, key: &str, caller: &str, command: Command) -> Envelope {
        self.counter += 1;
        Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: format!("{:032x}", self.counter),
            traceparent: None,
            command,
        }
    }

    /// What a well-behaved client does: offer the command to each member in
    /// turn, and on any failure or silence try again with the same key, until
    /// some leader confirms it or `patience` runs out.
    pub async fn submit_within(&self, envelope: &Envelope, patience: Duration) -> Option<Reply> {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            for node in self.reachable() {
                if let Ok(Ok(reply)) = tokio::time::timeout(Duration::from_millis(1500), node.propose(envelope.clone())).await {
                    return Some(reply);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Submit with a fresh key and insist the core accepted the command.
    pub async fn run(&mut self, caller: &str, command: Command) -> Reply {
        let key = format!("test-{}", self.counter + 1);
        self.run_keyed(&key, caller, command).await
    }

    pub async fn run_keyed(&mut self, key: &str, caller: &str, command: Command) -> Reply {
        let envelope = self.envelope(key, caller, command);
        let reply = self.submit_within(&envelope, PATIENCE).await.expect("the group did not confirm the command");
        assert_eq!(reply.rejected, None, "the core refused {key}");
        reply
    }

    /// Open the business date and activate the shipped products and tables.
    pub async fn configure(&mut self) {
        self.run("leader", Command::OpenBusinessDate { date: start_date() }).await;
        for command in shipped_activations().unwrap() {
            self.run("config", command).await;
        }
    }

    /// Issue the standard term life policy of the scenario tests.
    pub async fn issue(&mut self, n: u64) -> PolicyId {
        let issue = term_life(n, start_date(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        let id = issue.policy.clone();
        self.run("new-business", issue_command(issue)).await;
        id
    }

    /// Wait until every reachable member has applied the same entries, and
    /// return the state hash they share.
    pub async fn settle(&self) -> String {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let statuses = self.statuses();
            let first = &statuses[0];
            let same = statuses
                .iter()
                .all(|s| s.last_applied == first.last_applied && s.last_log_index == first.last_applied && s.state_hash == first.state_hash);
            if same && self.leader_now().is_some() {
                return first.state_hash.clone();
            }
            assert!(tokio::time::Instant::now() < deadline, "the members did not converge: {statuses:#?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn stop(mut self) {
        for id in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.crash(id).await;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn receipt(policy: &PolicyId, id: &str, minor: i64) -> Command {
    Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: id.to_string(),
        amount: Money::new(minor, NZD),
    }
}

/// What an auditor does with a member's log: replay the committed commands
/// through a fresh state machine and hash the result.
pub fn replayed_hash(log: &LogStore) -> String {
    let mut state = CoreState::new(PARTITION);
    for (index, envelope) in log.commands() {
        state.apply(index, envelope);
    }
    state.state_hash().to_hex()
}
