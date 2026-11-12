//! Ledger nodes in this process, the business they are given, and the
//! simulator's answer for the same business: shared by the end-of-day tests.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::NaiveDate;
use ic_azure::blob::MemoryWormStore;
use ic_azure::keyvault::DevSigner;
use ic_core::{Command, CoreState, Envelope, PolicyId};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Running, Settings};
use ic_raft::{ClusterAuth, ClusterClient, NodeId};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;
use ic_workflows::{EndOfDayInput, LedgerSteps};

/// The cluster token of the test ledgers.
pub const TOKEN: &str = "test-cluster-token-0123456789abcdef";

pub const PATIENCE: Duration = Duration::from_secs(30);

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// The day the book opens. The first monthly bill after issue falls 30 days on.
pub fn opening() -> NaiveDate {
    d(2027, 11, 15)
}

pub fn day(n: u64) -> NaiveDate {
    opening() + chrono::Days::new(n)
}

/// A port below the range the operating system gives outgoing connections,
/// so no client socket can take it between choosing it and binding it.
pub fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    loop {
        let offset = NEXT.fetch_add(1, Ordering::Relaxed);
        let port = 10_000 + ((std::process::id() as u16).wrapping_mul(53).wrapping_add(offset) % 9_000);
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

pub fn scratch(name: &str) -> PathBuf {
    static RUN: AtomicU64 = AtomicU64::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The business both the nodes and the simulator are given: a few policies
/// on each partition, one of them paid.
pub fn business(partitions: PartitionId) -> Vec<(PartitionId, &'static str, Command)> {
    let mut commands = Vec::new();
    for partition in 0..partitions {
        commands.push((partition, "leader", Command::OpenBusinessDate { date: opening() }));
        for activation in shipped_activations().unwrap() {
            commands.push((partition, "config", activation));
        }
        for n in 1..=3u64 {
            let number = u64::from(partition) * 10 + n;
            let cover = 250_000_00 * n as i64;
            let issue = term_life(number, opening(), d(1985, 6, 1), Sex::Male, n == 2, cover, None, Frequency::Monthly, None);
            let policy = issue.policy.clone();
            commands.push((partition, "new-business", issue_command(issue)));
            if n == 1 {
                let receipt = Command::ApplyReceipt {
                    policy,
                    receipt: format!("r-{number}"),
                    amount: Money::new(40_00, NZD),
                };
                commands.push((partition, "cash-matching", receipt));
            }
        }
    }
    commands
}

/// The paid policy on partition 0.
pub fn first_policy() -> PolicyId {
    term_life(1, opening(), d(1985, 6, 1), Sex::Male, false, 250_000_00, None, Frequency::Monthly, None).policy
}

/// What the book looks like, in terms that do not depend on salts or on how
/// the day's work was chunked: the date, the number of journal entries, and
/// each policy's status, balance owed and periods billed.
pub type Book = (Option<NaiveDate>, u64, Vec<(PolicyId, String, i64, u32)>);

pub fn book(state: &CoreState) -> Book {
    let policies = state
        .policies
        .values()
        .map(|p| (p.id.clone(), format!("{:?}", p.status), p.outstanding_minor(), p.periods_billed))
        .collect();
    (state.business_date, state.ledger.entry_count(), policies)
}

/// The simulator's book after the same business and `days` closed days.
pub fn simulated(partitions: PartitionId, days: u64) -> BTreeMap<PartitionId, Book> {
    let mut sim = Cluster::in_memory(partitions, 7);
    for (partition, caller, command) in business(partitions) {
        sim.submit(partition, caller, command).unwrap();
    }
    for _ in 0..days {
        assert!(sim.end_of_day().unwrap().breaks.is_empty());
    }
    (0..partitions).map(|p| (p, book(sim.state(p)))).collect()
}

/// A cluster of ledger nodes in this process.
pub struct Ledger {
    pub partitions: PartitionId,
    pub settings: BTreeMap<NodeId, Settings>,
    pub running: BTreeMap<NodeId, Running>,
    pub client: ClusterClient,
    pub dir: PathBuf,
}

impl Ledger {
    /// A cluster with the standard business on its books.
    pub async fn start(name: &str, size: u64, partitions: PartitionId) -> Self {
        let ledger = Self::start_empty(name, size, partitions).await;
        for (n, (partition, caller, command)) in business(partitions).into_iter().enumerate() {
            ledger.run(partition, &format!("setup-{n}"), caller, command).await;
        }
        ledger
    }

    pub async fn run(&self, partition: PartitionId, key: &str, caller: &str, command: Command) {
        let envelope = Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        };
        let reply = self.client.propose_until(partition, &envelope, PATIENCE).await.unwrap();
        assert_eq!(reply.rejected, None, "{key}");
    }

    /// Move one partition's business date on, as the cutoff step does.
    pub async fn advance(&self, partition: PartitionId, to: NaiveDate) {
        self.run(partition, &format!("advance-{partition}-{to}"), "leader", Command::AdvanceBusinessDate { to })
            .await;
    }

    /// A cluster nobody has opened.
    pub async fn start_empty(name: &str, size: u64, partitions: PartitionId) -> Self {
        let dir = scratch(name);
        let peers: BTreeMap<NodeId, String> = (1..=size).map(|id| (id, format!("127.0.0.1:{}", free_port()))).collect();
        let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
        let settings = peers
            .iter()
            .map(|(id, addr)| {
                let s = Settings {
                    id: *id,
                    listen: addr.parse().unwrap(),
                    data_dir: dir.join(format!("node-{id}")),
                    partitions,
                    peers: peers.clone(),
                    bootstrap: *id == 1,
                    heartbeat_ms: 50,
                    auth: auth.clone(),
                    cold: ColdTarget::Dir(dir.join("cold")),
                    events: EventTarget::Discard,
                    segment_entries: 50,
                    snapshot_every: 200,
                };
                (*id, s)
            })
            .collect();
        let client = ClusterClient::new(peers.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(auth.clone());
        let mut ledger = Self {
            partitions,
            settings,
            running: BTreeMap::new(),
            client,
            dir,
        };
        for id in 1..=size {
            ledger.boot(id).await;
        }
        ledger
    }

    pub async fn boot(&mut self, id: NodeId) {
        let running = server::start(self.settings[&id].clone()).await.unwrap();
        self.running.insert(id, running);
    }

    /// Crash a node: its requests are dropped and its memory is gone.
    pub async fn kill(&mut self, id: NodeId) {
        self.running.remove(&id).expect("the node is not running").kill().await;
    }

    pub fn leader_of(&self, partition: PartitionId) -> Option<NodeId> {
        self.running.iter().find(|(_, r)| r.host.groups[&partition].raft.is_leader()).map(|(id, _)| *id)
    }

    /// The book as the partition's leader holds it.
    pub async fn book(&self, partition: PartitionId) -> Book {
        // A linearizable read first: it returns once the leader is confirmed
        // and has applied everything committed.
        self.client.end_of_day_view(partition).await.unwrap();
        let leader = self.leader_of(partition).expect("no leader");
        self.running[&leader].host.groups[&partition].machine.read(book)
    }

    pub async fn books(&self) -> BTreeMap<PartitionId, Book> {
        let mut books = BTreeMap::new();
        for partition in 0..self.partitions {
            books.insert(partition, self.book(partition).await);
        }
        books
    }

    pub fn steps(&self) -> LedgerSteps<DevSigner, MemoryWormStore> {
        LedgerSteps::new(self.client.clone(), DevSigner, MemoryWormStore::default()).with_patience(PATIENCE)
    }

    pub fn input(&self, closing: NaiveDate) -> EndOfDayInput {
        EndOfDayInput::new(closing, self.partitions)
    }

    pub async fn stop(mut self) {
        for id in self.running.keys().copied().collect::<Vec<_>>() {
            self.running.remove(&id).unwrap().stop().await.unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
