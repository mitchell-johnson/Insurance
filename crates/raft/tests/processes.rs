//! Resilience of real `ic-node` processes talking HTTP on localhost.
//!
//! These tests start the shipped binary, kill it with SIGKILL the way an
//! out-of-memory killer or a failed host would, and check what clients see.
//! The Docker chaos tests do the same to containers; these run anywhere
//! `cargo test` does.

mod common;

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command as Process, Stdio};
use std::time::Duration;

use common::{d, receipt, scratch, start_date};
use ic_azure::blob::DirWormStore;
use ic_azure::cold_tier::fetch_segment;
use ic_core::{Command, Envelope, GroupId, PolicyId};
use ic_ledger::verify_chain;
use ic_product::{Frequency, Sex};
use ic_raft::client::ClientError;
use ic_raft::server::{ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient, NodeId, NodeStatus};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};

const PATIENCE: Duration = Duration::from_secs(30);
/// The cluster token of the test fleets.
const TOKEN: &str = "test-cluster-token-0123456789abcdef";

/// A port for a node to listen on. Taken from below the range the operating
/// system hands to outgoing connections: a port from that range can be
/// claimed by some client socket between choosing it here and the node
/// binding it, which leaves a fleet one node short.
fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    loop {
        let offset = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let port = 20_000 + ((std::process::id() as u16).wrapping_mul(37).wrapping_add(offset) % 10_000);
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

/// A cluster of node processes on this machine.
struct Fleet {
    dir: PathBuf,
    partitions: u16,
    addrs: BTreeMap<NodeId, String>,
    running: BTreeMap<NodeId, Child>,
    client: ClusterClient,
    counter: u64,
    /// Log entries between snapshots.
    snapshot_every: u64,
}

impl Fleet {
    async fn start(name: &str, size: u64, partitions: u16) -> Self {
        // Far more entries than most tests write: no snapshot is taken.
        Self::start_snapshotting(name, size, partitions, 100_000).await
    }

    async fn start_snapshotting(name: &str, size: u64, partitions: u16, snapshot_every: u64) -> Self {
        let addrs: BTreeMap<NodeId, String> = (1..=size).map(|id| (id, format!("127.0.0.1:{}", free_port()))).collect();
        let client = ClusterClient::new(addrs.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap());
        let mut fleet = Self {
            dir: scratch(name),
            partitions,
            addrs,
            running: BTreeMap::new(),
            client,
            counter: 0,
            snapshot_every,
        };
        for id in 1..=size {
            fleet.spawn(id);
        }
        fleet
    }

    fn spawn(&mut self, id: NodeId) {
        let peers = self.addrs.iter().map(|(id, addr)| format!("{id}={addr}")).collect::<Vec<_>>().join(",");
        let child = Process::new(env!("CARGO_BIN_EXE_ic-node"))
            .env("IC_NODE_ID", id.to_string())
            .env("IC_NODE_PEERS", peers)
            .env("IC_NODE_LISTEN", &self.addrs[&id])
            .env("IC_NODE_DATA_DIR", self.dir.join(format!("node-{id}")))
            .env("IC_PARTITIONS", self.partitions.to_string())
            .env("IC_NODE_BOOTSTRAP", if id == 1 { "true" } else { "false" })
            .env("IC_RAFT_HEARTBEAT_MS", "50")
            .env("IC_CLUSTER_TOKEN", TOKEN)
            .env("IC_COLD_DIR", self.dir.join("cold"))
            .env("IC_EVENT_SINK", "discard")
            .env("IC_SEGMENT_ENTRIES", "20")
            .env("IC_RAFT_SNAPSHOT_EVERY", self.snapshot_every.to_string())
            .env("RUST_LOG", "info,openraft=warn")
            .stdout(self.log_file(id))
            .stderr(self.log_file(id))
            .spawn()
            .unwrap();
        self.running.insert(id, child);
    }

    /// Each node's output goes to a file kept when a test fails.
    fn log_file(&self, id: NodeId) -> Stdio {
        let path = self.dir.join(format!("node-{id}.log"));
        Stdio::from(std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap())
    }

    /// What every node says about itself, for a failure message.
    async fn diagnosis(&mut self) -> String {
        let mut lines = vec![format!("node logs are in {}", self.dir.display())];
        for id in self.addrs.keys().copied().collect::<Vec<_>>() {
            let process = match self.running.get_mut(&id).map(Child::try_wait) {
                None => "killed by the test".to_string(),
                Some(Ok(None)) => "running".to_string(),
                Some(exit) => format!("exited: {exit:?}"),
            };
            lines.push(format!("node {id} ({process}): {:?}", self.client.status(&self.addrs[&id]).await));
        }
        lines.join("\n")
    }

    /// SIGKILL: no warning, no cleanup.
    fn kill(&mut self, id: NodeId) {
        let mut child = self.running.remove(&id).expect("the node is not running");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    fn envelope(&mut self, key: &str, caller: &str, command: Command) -> Envelope {
        self.counter += 1;
        Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: format!("{:032x}", self.counter),
            traceparent: None,
            command,
        }
    }

    async fn run_keyed(&mut self, partition: u16, key: &str, caller: &str, command: Command) -> ic_raft::Reply {
        let envelope = self.envelope(key, caller, command);
        let reply = match self.client.propose_until(partition, &envelope, PATIENCE).await {
            Ok(reply) => reply,
            Err(e) => panic!("the cluster did not confirm {key}: {e}\n{}", self.diagnosis().await),
        };
        assert_eq!(reply.rejected, None, "the core refused {key}");
        reply
    }

    async fn run(&mut self, partition: u16, caller: &str, command: Command) -> ic_raft::Reply {
        let key = format!("test-{}", self.counter + 1);
        self.run_keyed(partition, &key, caller, command).await
    }

    async fn configure(&mut self) {
        for partition in 0..self.partitions {
            self.run(partition, "leader", Command::OpenBusinessDate { date: start_date() }).await;
            for command in shipped_activations().unwrap() {
                self.run(partition, "config", command).await;
            }
        }
    }

    async fn issue(&mut self, partition: u16, n: u64) -> PolicyId {
        let issue = term_life(n, start_date(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        let id = issue.policy.clone();
        self.run(partition, "new-business", issue_command(issue)).await;
        id
    }

    async fn status_of(&self, id: NodeId) -> Option<Vec<NodeStatus>> {
        self.client.status(&self.addrs[&id]).await.ok()
    }

    /// Wait until every running node reports the same applied state for
    /// every partition, and return those statuses from one node.
    async fn settle(&self) -> Vec<NodeStatus> {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let mut seen = Vec::new();
            for id in self.running.keys() {
                seen.push(self.status_of(*id).await);
            }
            let fingerprint = |s: &Vec<NodeStatus>| {
                s.iter()
                    .map(|p| (p.partition, p.last_applied, p.last_log_index, p.state_hash.clone()))
                    .collect::<Vec<_>>()
            };
            if let Some(Some(first)) = seen.first() {
                let led = first.iter().all(|p| p.leader.is_some() && p.last_applied == p.last_log_index);
                if led && seen.iter().all(|s| s.as_ref().is_some_and(|s| fingerprint(s) == fingerprint(first))) {
                    return first.clone();
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "the nodes did not converge: {seen:#?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn leader_of(&self, partition: u16) -> NodeId {
        self.settle().await[usize::from(partition)].leader.unwrap()
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        // The survivors are asked to stop, as a rollout would ask them, so
        // that each exits through its shutdown path; one that has not gone
        // in ten seconds is killed.
        for child in self.running.values() {
            let _ = Process::new("kill").args(["-TERM", &child.id().to_string()]).status();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        for child in self.running.values_mut() {
            while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Purpose: SIGKILL the leader's process while a client is paying in a run of receipts.
/// Every receipt must be confirmed, each applied exactly once, and the killed node must
/// return to the same state as the others when it is started again.
/// Why it matters: this is the failure the Raft layer exists for, exercised end to end
/// through the real binary, real sockets and real disks. A client that loses a payment or
/// books it twice during a failover is the outage customers actually notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killing_the_leader_process_loses_no_payment_and_doubles_none() {
    let mut fleet = Fleet::start("kill-leader", 3, 1).await;
    fleet.configure().await;
    let policy = fleet.issue(0, 1).await;
    let entries_before = fleet.settle().await[0].ledger_entries;

    let leader = fleet.leader_of(0).await;
    let mut entries_per_receipt = 0;
    for n in 0..12 {
        if n == 4 {
            fleet.kill(leader);
        }
        // Every receipt is sent twice, as a client unsure of the outcome would.
        let key = format!("receipt:r-{n}");
        let first = fleet.run_keyed(0, &key, "cash-matching", receipt(&policy, &format!("r-{n}"), 10_00)).await;
        let again = fleet.run_keyed(0, &key, "cash-matching", receipt(&policy, &format!("r-{n}"), 10_00)).await;
        assert!(again.duplicate, "receipt {n} was applied a second time");
        entries_per_receipt = entries_per_receipt.max(first.entries);
    }
    let survivors = fleet.settle().await;
    assert_ne!(survivors[0].leader, Some(leader));
    assert_eq!(
        survivors[0].ledger_entries,
        entries_before + 12 * entries_per_receipt,
        "a receipt was lost or doubled"
    );

    fleet.spawn(leader);
    let healed = fleet.settle().await;
    assert_eq!(fleet.running.len(), 3);
    assert_eq!(healed[0].state_hash, survivors[0].state_hash, "the restarted node rebuilt a different state");
}

/// Purpose: with two of three node processes dead the cluster refuses writes and reads,
/// and it serves both again, with nothing lost, once one of them is back.
/// Why it matters: a client must get a clear "unavailable, retry" rather than a
/// confirmation the cluster cannot honour, and operators must be able to recover by
/// simply restarting processes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cluster_without_a_majority_refuses_and_then_recovers() {
    let mut fleet = Fleet::start("no-majority", 3, 1).await;
    fleet.configure().await;
    let policy = fleet.issue(0, 1).await;
    let before = fleet.settle().await;

    let leader = fleet.leader_of(0).await;
    let other = (1..=3).find(|id| *id != leader).unwrap();
    fleet.kill(leader);
    fleet.kill(other);

    let stranded = fleet.envelope("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84));
    let refused = fleet.client.propose_until(0, &stranded, Duration::from_secs(2)).await;
    assert!(
        matches!(refused, Err(ClientError::Unavailable { partition: 0, .. })),
        "a lone node confirmed a write: {refused:?}"
    );
    assert!(fleet.client.end_of_day_view(0).await.is_err(), "a lone node served a read it cannot vouch for");

    fleet.spawn(other);
    let reply = fleet.client.propose_until(0, &stranded, PATIENCE).await.unwrap();
    assert_eq!(reply.rejected, None);
    let view = fleet.client.end_of_day_view(0).await.unwrap();
    assert_eq!(view.business_date, Some(start_date()));
    assert_eq!(view.open, vec![policy.clone()]);
    assert_eq!(view.breaks, vec![]);

    fleet.spawn(leader);
    let after = fleet.settle().await;
    assert!(after[0].ledger_entries > before[0].ledger_entries);
    let again = fleet.client.propose_until(0, &stranded, PATIENCE).await.unwrap();
    assert!(again.duplicate);
    assert_eq!(fleet.settle().await[0].ledger_entries, after[0].ledger_entries);
}

/// Purpose: a policy transfer between two partitions completes through the nodes' own
/// relay, and still completes when the source partition's leader is killed just after the
/// transfer begins.
/// Why it matters: a transfer is the one operation that spans Raft groups. Its messages
/// are held in memory by the leader, so a leader crash at the wrong moment is exactly
/// where a policy could be left frozen forever with its money in transit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transfer_between_partitions_survives_a_leader_kill() {
    let mut fleet = Fleet::start("transfer", 3, 2).await;
    fleet.configure().await;
    let policy = fleet.issue(0, 1).await;
    fleet.run(0, "cash-matching", receipt(&policy, "r-1", 61_84)).await;
    let source_leader = fleet.leader_of(0).await;

    fleet
        .run(
            0,
            "transfer",
            Command::BeginTransfer {
                policy: policy.clone(),
                transfer_id: "t-1".to_string(),
                target_group: GroupId("grp-new".to_string()),
                target_partition: 1,
            },
        )
        .await;
    fleet.kill(source_leader);

    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let source = fleet.client.end_of_day_view(0).await;
        let target = fleet.client.end_of_day_view(1).await;
        if let (Ok(source), Ok(target)) = (&source, &target) {
            if !source.transfer_in_flight && target.open.contains(&policy) {
                // What left one partition's books is exactly what arrived on the other's.
                let net = |v: &ic_raft::EndOfDayView| v.transfer_in_transit.parse::<i128>().unwrap();
                assert_ne!(net(source), 0);
                assert_eq!(net(source) + net(target), 0, "money went missing in transit");
                assert!(!source.open.contains(&policy), "the policy is still open on the partition it left");
                assert_eq!(source.breaks, vec![]);
                assert_eq!(target.breaks, vec![]);
                break;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "the transfer did not complete: {source:?} {target:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Every member has let go of the messages once they were confirmed.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let statuses = fleet.settle().await;
        if statuses.iter().all(|p| p.backlog.messages == 0) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "delivered messages were never released: {statuses:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The policy takes payments in its new home.
    fleet.run(1, "cash-matching", receipt(&policy, "r-2", 61_84)).await;

    // Each partition's read models followed the policy: the source no longer
    // lists it, the target lists it under its number and new group.
    assert_eq!(fleet.client.locate(0, &policy).await.unwrap(), None);
    let entry = fleet
        .client
        .locate(1, &policy)
        .await
        .unwrap()
        .expect("the target partition does not list the policy it holds");
    assert_eq!((entry.partition, entry.group.clone()), (1, GroupId("grp-new".to_string())));
    assert_eq!(fleet.client.policy_by_number(0, &entry.number).await.unwrap(), None);
    let view = fleet
        .client
        .policy_by_number(1, &entry.number)
        .await
        .unwrap()
        .expect("the target partition cannot find the policy by its number");
    assert_eq!(view.policy.id, policy);
    assert_eq!(view.business_date, Some(start_date()));
    assert!(view.billing.is_some(), "the billing position did not follow the policy");
    assert_eq!(fleet.client.business_date(1).await.unwrap(), Some(start_date()));
    // A partition no node hosts cannot be asked anything.
    assert!(fleet.client.locate(9, &policy).await.is_err());
}

/// Purpose: the node's HTTP surface answers the way clients and probes rely on: health,
/// status for every partition, a redirect from a follower, a clear refusal for a partition
/// it does not host, and a repeatable bootstrap.
/// Why it matters: Kubernetes probes, the cluster client and the workflow worker are all
/// written against these answers. A follower that swallowed a command, or a second
/// bootstrap that re-formed a live group, would be silent corruption.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_node_api_redirects_refuses_and_reports() {
    let mut fleet = Fleet::start("api", 3, 1).await;
    fleet.configure().await;
    let statuses = fleet.settle().await;
    let leader = statuses[0].leader.unwrap();
    let follower = (1..=3).find(|id| *id != leader).unwrap();
    let http = authorised();
    let addrs = fleet.addrs.clone();
    let base = |id: NodeId| format!("http://{}", addrs[&id]);

    let health = http.get(format!("{}/healthz", base(follower))).send().await.unwrap();
    assert_eq!(health.text().await.unwrap(), "ok");

    // A follower does not take the command; it names the leader.
    let envelope = fleet.envelope("k-1", "leader", Command::OpenBusinessDate { date: start_date() });
    let redirected = http
        .post(format!("{}/v1/partitions/0/commands", base(follower)))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(redirected.status(), 421);
    let body: serde_json::Value = redirected.json().await.unwrap();
    assert_eq!(body["error"]["NotLeader"]["leader"], leader);
    assert_eq!(body["error"]["NotLeader"]["addr"], fleet.addrs[&leader]);
    let read = http.get(format!("{}/v1/partitions/0/end-of-day", base(follower))).send().await.unwrap();
    assert_eq!(read.status(), 421);

    // A partition this cluster does not have.
    let missing = http
        .post(format!("{}/v1/partitions/7/commands", base(leader)))
        .json(&envelope)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    assert_eq!(
        http.get(format!("{}/v1/partitions/7/end-of-day", base(leader))).send().await.unwrap().status(),
        404
    );
    assert!(matches!(
        fleet.client.propose(7, &envelope).await,
        Err(ClientError::Unavailable { partition: 7, .. })
    ));

    // Bootstrapping a live group again changes nothing.
    let before = fleet.settle().await;
    let init = http.post(format!("{}/v1/init", base(leader))).send().await.unwrap();
    assert_eq!(init.status(), 204);
    assert_eq!(fleet.settle().await[0].state_hash, before[0].state_hash);
}

/// Purpose: a node asked to stop with SIGTERM exits cleanly with status zero, and a node
/// started with a broken configuration exits at once with a message naming the setting.
/// Why it matters: Kubernetes stops pods with SIGTERM on every rollout; an unclean exit
/// there shows up as crash loops and noisy alerts. A misconfigured node must fail loudly
/// at start rather than join a cluster under the wrong identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_stops_cleanly_and_refuses_bad_configuration() {
    let mut fleet = Fleet::start("stop", 3, 1).await;
    fleet.configure().await;
    fleet.settle().await;

    let mut child = fleet.running.remove(&2).unwrap();
    let signalled = Process::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(signalled.success());
    let exit = tokio::task::spawn_blocking(move || child.wait().unwrap()).await.unwrap();
    assert!(exit.success(), "SIGTERM did not lead to a clean exit: {exit:?}");

    let bad = Process::new(env!("CARGO_BIN_EXE_ic-node"))
        .env_clear()
        .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
        .env("IC_CLUSTER_AUTH", "open")
        .env("IC_COLD_DIR", "/tmp")
        .env("IC_EVENT_SINK", "discard")
        .env("IC_NODE_ID", "one")
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("IC_NODE_ID"),
        "the error does not name the setting"
    );
}

/// Purpose: every full stretch of the journal is sealed into the cold tier exactly once,
/// as a segment that verifies on its own and links to its neighbours, even though the
/// leader doing the sealing is killed part-way; and once sealed or published, entries and
/// events are released by every member, so nothing piles up.
/// Why it matters: the journal is the legal record and must outlive the nodes. A leader
/// can die between writing a segment and recording that it did; its successor must be
/// able to repeat the write without forking the archive, and must not skip a segment
/// either. Unreleased outputs would otherwise grow every member's memory and snapshot
/// without bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_journal_is_sealed_once_across_a_leader_kill() {
    const SEGMENT: u64 = 20;
    let mut fleet = Fleet::start("seal", 3, 1).await;
    fleet.configure().await;
    let policy = fleet.issue(0, 1).await;
    let leader = fleet.leader_of(0).await;
    for n in 0..60 {
        if n == 25 {
            fleet.kill(leader);
        }
        fleet
            .run_keyed(0, &format!("receipt:r-{n}"), "cash-matching", receipt(&policy, &format!("r-{n}"), 1_00))
            .await;
    }
    fleet.spawn(leader);

    // Housekeeping runs a few times a second; wait for it to catch up.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let statuses = loop {
        let statuses = fleet.settle().await;
        let p = &statuses[0];
        if p.backlog.journal < SEGMENT && p.backlog.events == 0 {
            break statuses;
        }
        assert!(tokio::time::Instant::now() < deadline, "housekeeping did not catch up: {:?}", p.backlog);
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let entries = statuses[0].ledger_entries;
    assert!(entries > 3 * SEGMENT, "too few entries to exercise sealing");
    assert_eq!(
        statuses[0].backlog.journal,
        entries % SEGMENT,
        "members hold entries that are sealed, or lost some that are not"
    );
    for id in fleet.running.keys() {
        assert_eq!(
            fleet.status_of(*id).await.unwrap()[0].backlog,
            statuses[0].backlog,
            "node {id} holds a different backlog"
        );
    }

    // The archive, read back with no help from the nodes, is one unbroken chain.
    let cold = DirWormStore::new(fleet.dir.join("cold"));
    let mut archived = Vec::new();
    for k in 0..entries / SEGMENT {
        archived.extend(
            fetch_segment(&cold, 0, k * SEGMENT + 1, (k + 1) * SEGMENT)
                .await
                .expect("a segment is missing or does not verify"),
        );
    }
    assert_eq!(archived.len() as u64, entries - entries % SEGMENT);
    verify_chain(0, &archived).expect("the archived journal is not one chain");
    let files = std::fs::read_dir(fleet.dir.join("cold/ledger/p00")).unwrap().count() as u64;
    assert_eq!(
        files,
        2 * (entries / SEGMENT),
        "the archive holds something other than one segment and one manifest per stretch"
    );
}

/// Purpose: a node process restarted on an empty disk under its old number is shut out,
/// and a new node with a new number, added through the membership API, is sent a snapshot
/// over HTTP, ends in the same state as the others, and keeps the cluster's memory of
/// which payments were already applied.
/// Why it matters: this is the disk-replacement runbook, run for real: processes, sockets,
/// the snapshot crossing the wire. Without snapshots a replacement would replay the
/// insurer's whole history; without the identity check the wiped node would rejoin as if
/// nothing had happened and put committed entries at risk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_disk_is_shut_out_and_a_new_node_takes_its_place() {
    let mut fleet = Fleet::start_snapshotting("lost-disk", 3, 1, 30).await;
    fleet.configure().await;
    let policy = fleet.issue(0, 1).await;
    let leader = fleet.leader_of(0).await;
    let healthy = (1..=3).find(|id| *id != leader).unwrap();
    let lost = (1..=3).find(|id| *id != leader && *id != healthy).unwrap();

    fleet.kill(lost);
    std::fs::remove_dir_all(fleet.dir.join(format!("node-{lost}"))).unwrap();
    fleet.spawn(lost);
    for n in 0..120 {
        fleet
            .run_keyed(0, &format!("receipt:r-{n}"), "cash-matching", receipt(&policy, &format!("r-{n}"), 1_00))
            .await;
    }
    let impostor = &fleet.status_of(lost).await.expect("the wiped node is not running")[0];
    assert_eq!(impostor.last_applied, None, "a wiped node was fed the log under its old number");
    assert_eq!(fleet.status_of(leader).await.unwrap()[0].role, "leader", "the wiped node disturbed the leader");
    fleet.kill(lost);

    let replacement: NodeId = 4;
    fleet.addrs.insert(replacement, format!("127.0.0.1:{}", free_port()));
    fleet.spawn(replacement);
    let members: BTreeMap<NodeId, String> = [leader, healthy, replacement].into_iter().map(|id| (id, fleet.addrs[&id].clone())).collect();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while let Err(e) = fleet.client.set_members(0, &members).await {
        assert!(tokio::time::Instant::now() < deadline, "the membership change did not go through: {e}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let healed = fleet.settle().await;
    let rebuilt = &fleet.status_of(replacement).await.unwrap()[0];
    assert_eq!(rebuilt.state_hash, healed[0].state_hash);
    assert_eq!(rebuilt.backlog, healed[0].backlog);
    assert!(
        rebuilt.first_log_index.unwrap() > 100,
        "the new node replayed the whole log instead of taking a snapshot"
    );
    assert!(fleet.dir.join(format!("node-{replacement}/partition-0/snapshot.json")).exists());
    let mut voters = healed[0].voters.clone();
    voters.sort_unstable();
    let mut expected = vec![leader, healthy, replacement];
    expected.sort_unstable();
    assert_eq!(voters, expected);

    // The idempotency records crossed with the snapshot: with the old leader
    // gone, an old key is still a duplicate.
    fleet.kill(leader);
    let again = fleet.run_keyed(0, "receipt:r-3", "cash-matching", receipt(&policy, "r-3", 1_00)).await;
    assert!(again.duplicate, "the rebuilt cluster forgot a receipt it had applied");
}

/// An HTTP client that presents the fleet's cluster token.
fn authorised() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::AUTHORIZATION, format!("Bearer {TOKEN}").parse().unwrap());
    reqwest::Client::builder().default_headers(headers).build().unwrap()
}

/// Purpose: a node turns away every caller without the cluster token, on the client routes
/// and on Raft's own routes alike, while its health probe stays open; a client holding the
/// wrong token is told so; and a node started with a different token can neither read nor
/// vote, so it cannot disturb the group.
/// Why it matters: the node's port is the ledger. Anything in the cluster network that can
/// reach it could otherwise post journal entries or, worse, cast Raft votes and append log
/// entries as if it were a member. The token is what stands between a compromised
/// neighbouring pod and the books.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_admits_only_callers_with_the_cluster_token() {
    let mut fleet = Fleet::start("auth", 3, 1).await;
    fleet.configure().await;
    let before = fleet.settle().await;
    let leader = before[0].leader.unwrap();
    let base = format!("http://{}", fleet.addrs[&leader]);
    let envelope = fleet.envelope("intruder", "leader", Command::AdvanceBusinessDate { to: d(2027, 11, 16) });

    let anonymous = reqwest::Client::new();
    for path in ["/v1/status", "/v1/partitions/0/end-of-day"] {
        assert_eq!(
            anonymous.get(format!("{base}{path}")).send().await.unwrap().status(),
            401,
            "{path} answered a caller with no token"
        );
    }
    for path in [
        "/v1/partitions/0/commands",
        "/v1/partitions/0/query",
        "/v1/init",
        "/raft/0/append",
        "/raft/0/vote",
        "/raft/0/pre-vote",
        "/raft/0/snapshot",
    ] {
        let no_token = anonymous.post(format!("{base}{path}")).json(&envelope).send().await.unwrap();
        assert_eq!(no_token.status(), 401, "{path} answered a caller with no token");
        assert_eq!(no_token.headers()["www-authenticate"], "Bearer");
        let wrong = anonymous
            .post(format!("{base}{path}"))
            .bearer_auth("x".repeat(40))
            .json(&envelope)
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 401, "{path} answered a caller with the wrong token");
        let basic = anonymous
            .post(format!("{base}{path}"))
            .basic_auth(TOKEN, None::<&str>)
            .json(&envelope)
            .send()
            .await
            .unwrap();
        assert_eq!(basic.status(), 401, "{path} accepted the token in the wrong scheme");
    }
    assert_eq!(anonymous.get(format!("{base}/healthz")).send().await.unwrap().text().await.unwrap(), "ok");

    let outsider = ClusterClient::new(fleet.addrs.values().cloned().collect()).with_auth(ClusterAuth::tokens(vec!["y".repeat(40)]).unwrap());
    match outsider.propose(0, &envelope).await {
        Err(ClientError::Unavailable { detail, .. }) => assert!(detail.contains("cluster token"), "{detail}"),
        other => panic!("a client with the wrong token got {other:?}"),
    }
    assert!(outsider.status(&fleet.addrs[&leader]).await.is_err());

    // Nothing the intruders sent changed the ledger. (The log itself may
    // have grown: the leader's own housekeeping goes through it.)
    let after = fleet.settle().await;
    assert_eq!(after[0].state_hash, before[0].state_hash);
    assert_eq!(after[0].ledger_entries, before[0].ledger_entries);
    assert_eq!(after[0].business_date, Some(start_date()));
}

/// Purpose: the cluster token setting accepts a file or an inline list, supports two
/// tokens at once for rotation, never prints a token, and refuses a missing, short or
/// contradictory configuration.
/// Why it matters: authentication that can be switched off by leaving a variable unset is
/// not authentication. A node must refuse to start rather than run open by accident, and
/// rotation must be possible without stopping the ledger.
#[test]
fn the_cluster_token_is_required_and_can_be_rotated() {
    let old = "o".repeat(32);
    let new = "n".repeat(40);
    let lookup = |pairs: &[(&str, &str)], file: Option<&str>| {
        let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        let file = file.map(str::to_string);
        ClusterAuth::from_lookup(
            |key| map.get(key).cloned(),
            |_| file.clone().ok_or_else(|| std::io::Error::other("no such file")),
        )
    };

    // Rotation: the first token is sent, both are accepted.
    let rotating = lookup(&[("IC_CLUSTER_TOKEN_FILE", "/secrets/token")], Some(&format!("{new}\n{old}\n"))).unwrap();
    assert_eq!(rotating.outgoing(), Some(new.as_str()));
    assert!(rotating.admits(Some(&format!("Bearer {old}"))));
    assert!(rotating.admits(Some(&format!("Bearer {new}"))));
    assert!(!rotating.admits(Some(&format!("Bearer {old}x"))));
    assert!(!rotating.admits(Some(&old)), "a token without the Bearer scheme was accepted");
    assert!(!rotating.admits(None));
    assert!(!format!("{rotating:?}").contains(&new), "a token was printed");

    let inline = lookup(&[("IC_CLUSTER_TOKEN", &format!("{old}, {new}")), ("IC_CLUSTER_AUTH", "token")], None).unwrap();
    assert_eq!(inline.outgoing(), Some(old.as_str()));

    let open = lookup(&[("IC_CLUSTER_AUTH", "open")], None).unwrap();
    assert!(open.is_open());
    assert!(open.admits(None));
    assert_eq!(open.outgoing(), None);

    let refused = |pairs: &[(&str, &str)], file: Option<&str>, names: &str| {
        let error = lookup(pairs, file).expect_err("the configuration should have been refused");
        assert!(error.contains(names), "{error:?} does not mention {names:?}");
    };
    refused(&[], None, "IC_CLUSTER_TOKEN_FILE");
    refused(&[("IC_CLUSTER_AUTH", "token")], None, "IC_CLUSTER_TOKEN_FILE");
    refused(&[("IC_CLUSTER_TOKEN", "short")], None, "too short");
    refused(&[("IC_CLUSTER_TOKEN", " , ")], None, "no cluster token");
    refused(&[("IC_CLUSTER_TOKEN_FILE", "/missing")], None, "/missing");
    refused(&[("IC_CLUSTER_TOKEN_FILE", "/f"), ("IC_CLUSTER_TOKEN", &old)], Some(&old), "not both");
    refused(&[("IC_CLUSTER_TOKEN", &old), ("IC_CLUSTER_AUTH", "open")], None, "contradicts");
    refused(&[("IC_CLUSTER_AUTH", "maybe")], None, "token or open");
    refused(&[("IC_CLUSTER_TOKEN", &old), ("IC_CLUSTER_AUTH", "maybe")], None, "token or open");
}

fn settings(pairs: &[(&str, &str)]) -> Result<Settings, String> {
    let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
    Settings::from_lookup(|key| map.get(key).cloned())
}

/// Purpose: node settings parse from the environment with the documented defaults, and
/// every malformed or contradictory value is refused with the name of the setting.
/// Why it matters: node identity and the peer list decide who can vote. A typo that was
/// silently accepted (a duplicate ID, a node missing from its own peer list, zero
/// partitions) could split a cluster or strand data on the wrong member.
#[test]
fn node_settings_have_defaults_and_refuse_nonsense() {
    let base = [
        ("IC_NODE_ID", "2"),
        ("IC_NODE_PEERS", "1=a:7000, 2=b:7000 ,3=c:7000"),
        ("IC_NODE_DATA_DIR", "/data"),
        ("IC_CLUSTER_TOKEN", TOKEN),
        ("IC_COLD_DIR", "/cold"),
        ("IC_EVENT_SINK", "discard"),
    ];
    let s = settings(&base).unwrap();
    assert_eq!(s.cold, ColdTarget::Dir(PathBuf::from("/cold")));
    assert_eq!(s.events, EventTarget::Discard);
    assert_eq!((s.segment_entries, s.snapshot_every), (1000, 5000));
    assert!(!s.auth.is_open());
    assert_eq!(s.auth.outgoing(), Some(TOKEN));
    assert_eq!((s.id, s.partitions, s.bootstrap, s.heartbeat_ms), (2, 1, false, 250));
    assert_eq!(s.listen.to_string(), "0.0.0.0:7000");
    assert_eq!(s.data_dir, PathBuf::from("/data"));
    assert_eq!(s.peers[&2], "b:7000");
    assert_eq!(s.peers.len(), 3);

    let with = |extra: &[(&str, &str)]| {
        let mut all: Vec<(&str, &str)> = base.iter().copied().filter(|(k, _)| !extra.iter().any(|(e, _)| e == k)).collect();
        all.extend_from_slice(extra);
        settings(&all)
    };
    let tuned = with(&[
        ("IC_PARTITIONS", "4"),
        ("IC_NODE_BOOTSTRAP", "true"),
        ("IC_RAFT_HEARTBEAT_MS", "500"),
        ("IC_NODE_LISTEN", "127.0.0.1:9"),
    ])
    .unwrap();
    assert_eq!((tuned.partitions, tuned.bootstrap, tuned.heartbeat_ms), (4, true, 500));
    assert_eq!(tuned.listen.port(), 9);

    let refused = |extra: &[(&str, &str)], names: &str| {
        let error = with(extra).expect_err("the settings should have been refused");
        assert!(error.contains(names), "{error:?} does not mention {names}");
    };
    refused(&[("IC_NODE_ID", "")], "IC_NODE_ID is required");
    refused(&[("IC_NODE_ID", "two")], "IC_NODE_ID");
    refused(&[("IC_NODE_ID", "9")], "does not list this node");
    refused(&[("IC_NODE_PEERS", "1=a:7000,2")], "expected id=host:port");
    refused(&[("IC_NODE_PEERS", "1=a:7000,x=b:7000")], "IC_NODE_PEERS");
    refused(&[("IC_NODE_PEERS", "2=a:7000,2=b:7000")], "exactly one address");
    refused(&[("IC_NODE_PEERS", "2=")], "exactly one address");
    refused(&[("IC_NODE_DATA_DIR", " ")], "IC_NODE_DATA_DIR is required");
    refused(&[("IC_PARTITIONS", "0")], "IC_PARTITIONS must be between");
    refused(&[("IC_PARTITIONS", "many")], "IC_PARTITIONS");
    refused(&[("IC_RAFT_HEARTBEAT_MS", "0")], "must be positive");
    refused(&[("IC_RAFT_HEARTBEAT_MS", "fast")], "IC_RAFT_HEARTBEAT_MS");
    refused(&[("IC_NODE_LISTEN", "nowhere")], "IC_NODE_LISTEN");
    refused(&[("IC_COLD_DIR", "")], "the journal must have somewhere to be sealed");
    refused(&[("IC_BLOB_ENDPOINT", "https://x.blob.core.windows.net")], "not both");
    refused(&[("IC_EVENT_SINK", "")], "IC_EVENT_HUBS_ENDPOINT");
    refused(&[("IC_EVENT_SINK", "stdout")], "must be discard");
    refused(&[("IC_EVENT_HUBS_ENDPOINT", "https://x.servicebus.windows.net")], "not both");
    refused(&[("IC_SEGMENT_ENTRIES", "0")], "IC_SEGMENT_ENTRIES must be positive");
    refused(&[("IC_RAFT_SNAPSHOT_EVERY", "often")], "IC_RAFT_SNAPSHOT_EVERY");

    let azure = with(&[
        ("IC_COLD_DIR", ""),
        ("IC_BLOB_ENDPOINT", "https://x.blob.core.windows.net"),
        ("IC_EVENT_SINK", ""),
        ("IC_EVENT_HUBS_ENDPOINT", "https://x.servicebus.windows.net"),
    ])
    .unwrap();
    assert_eq!(
        azure.cold,
        ColdTarget::Azure("https://x.blob.core.windows.net".to_string(), "cold-segments".to_string())
    );
    assert_eq!(
        azure.events,
        EventTarget::EventHubs("https://x.servicebus.windows.net".to_string(), "policy-events".to_string())
    );
}

/// Purpose: the client's address list parses, and an empty list is refused.
/// Why it matters: the worker is configured with this list. An empty one would make every
/// activity fail with a confusing "no nodes" error long after start instead of at start.
#[test]
fn the_client_needs_at_least_one_address() {
    let client = ClusterClient::from_list(" a:7000, b:7000 ,").unwrap();
    assert_eq!(client.nodes(), ["a:7000", "b:7000"]);
    assert!(ClusterClient::from_list(" , ").is_err());
}
