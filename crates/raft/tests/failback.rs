//! Leadership returning to the preferred nodes after they come back: three
//! nodes in this process over real HTTP, node 1 standing for the preferred region.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::{NaiveTime, TimeZone, Utc};
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
const PARTITIONS: u16 = 4;

/// A module with nothing in it but a switch that says whether it may be disturbed.
#[derive(Default, Serialize, Deserialize)]
struct Desk {
    busy: bool,
}

impl StateMachine for Desk {
    type Command = bool;
    type Output = ();

    fn apply(&mut self, _index: u64, busy: bool) {
        self.busy = busy;
    }
}

fn ask(desk: &Desk, question: Value) -> Result<Value, String> {
    if question.get("failback_quiet").is_some() {
        Ok(json!(!desk.busy))
    } else {
        Err("not a question for the desk".to_string())
    }
}

fn desk() -> Box<dyn ModuleState> {
    hosted(Desk::default(), ask)
}

fn failback(share_percent: u32, latency_budget: Duration, frozen: Vec<(NaiveTime, NaiveTime)>) -> FailbackSettings {
    FailbackSettings {
        homes: vec![BTreeSet::from([1])],
        share_percent,
        latency_budget,
        settle: Duration::from_millis(400),
        frozen,
        timezone: chrono_tz::Pacific::Auckland,
    }
}

struct Cluster {
    dir: std::path::PathBuf,
    peers: BTreeMap<NodeId, String>,
    auth: ClusterAuth,
    failback: FailbackSettings,
    running: BTreeMap<NodeId, Running>,
    client: ClusterClient,
}

impl Cluster {
    async fn start(name: &str, failback: FailbackSettings) -> Self {
        let dir = scratch(name);
        let peers: BTreeMap<NodeId, String> = (1..=3u64)
            .map(|id| {
                let port = common::free_port();
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
            failback,
            running: BTreeMap::new(),
            client,
        };
        for id in 1..=3 {
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
            witnesses: Default::default(),
        };
        let modules = vec![ModuleSpec::new("desk", desk).unwrap()];
        let node = server::start_full(settings, modules, Some(self.failback.clone())).await.unwrap();
        self.running.insert(id, node);
    }

    /// Who leads each of the five groups (four partitions and the module), as the running nodes report it.
    async fn leaders(&self) -> Vec<NodeId> {
        let mut leaders = Vec::new();
        for id in self.running.keys() {
            let addr = &self.peers[id];
            let partitions = self.client.status(addr).await.unwrap();
            leaders.extend(partitions.iter().filter(|s| s.role == "leader").map(|_| *id));
            let modules = self.client.module_status(addr).await.unwrap();
            leaders.extend(modules.iter().filter(|s| s.role == "leader").map(|_| *id));
        }
        leaders
    }

    /// Wait until every group has a leader and `settled` holds of the leaders.
    async fn until(&self, what: &str, settled: impl Fn(&[NodeId]) -> bool) -> Vec<NodeId> {
        let mut leaders = Vec::new();
        for _ in 0..400 {
            leaders = self.leaders().await;
            if leaders.len() == PARTITIONS as usize + 1 && settled(&leaders) {
                return leaders;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let mut said = Vec::new();
        for id in self.running.keys() {
            said.push((*id, self.tell(*id, None).await));
        }
        panic!("{what}: the leaders are {leaders:?}; fail-back says {said:?}");
    }

    /// Lose node 1 and wait for the others to lead everything.
    async fn lose_the_preferred_node(&mut self) {
        // Every member knows its groups before one is taken away.
        self.serves("first").await;
        self.running.remove(&1).unwrap().kill().await;
        self.until("the survivors did not take over", |l| l.iter().all(|id| *id != 1)).await;
    }

    /// A write to every partition and the module: the cluster is serving.
    async fn serves(&self, key: &str) {
        for partition in 0..PARTITIONS {
            let envelope = Envelope {
                key: format!("{key}-{partition}"),
                caller: "leader".to_string(),
                salt: fresh_salt(),
                traceparent: None,
                command: Command::OpenBusinessDate { date: start_date() },
            };
            self.client.propose_until(partition, &envelope, Duration::from_secs(30)).await.unwrap();
        }
        self.client.propose_module_until("desk", &json!(false), Duration::from_secs(30)).await.unwrap();
    }

    async fn tell(&self, id: NodeId, action: Option<&str>) -> ic_raft::failback::FailbackStatus {
        self.client.failback(&self.peers[&id], action).await.unwrap()
    }

    async fn stop(self) {
        for (_, node) in self.running {
            node.stop().await.unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Purpose: after the preferred node is lost and comes back, every group's leadership
/// returns to it without anyone asking: partitions and the module's group alike, a share
/// at a time, with the cluster serving writes before, during and after. While an operator
/// has fail-back paused nothing moves; resumed, it carries on. An operator can also move
/// one partition's leader by hand, and a move to a node that is not a voter, or of a
/// partition that does not exist, is refused.
/// Why it matters: this is addition three of the region-loss decision. Leaders left in the
/// far region after an outage mean every New Zealand write crosses the Tasman until someone
/// notices; a fail-back that needs an operator will be done late, by hand, at night. And an
/// operator must still be able to stop it or undo it when they know something it does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leadership_returns_to_the_preferred_node_without_an_operator() {
    let mut cluster = Cluster::start("failback-returns", failback(40, Duration::from_secs(5), vec![])).await;
    assert!(cluster.tell(2, None).await.enabled);
    cluster.serves("before").await;
    cluster.lose_the_preferred_node().await;
    cluster.serves("during").await;

    // An operator holds fail-back on the nodes that now lead; node 1 returns and waits.
    for id in [2, 3] {
        assert!(cluster.tell(id, Some("pause")).await.paused);
    }
    cluster.boot(1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let held = cluster.until("leaders moved while paused", |_| true).await;
    assert!(held.iter().all(|id| *id != 1), "leadership moved while fail-back was paused: {held:?}");

    for id in [2, 3] {
        assert!(!cluster.tell(id, Some("resume")).await.paused);
    }
    cluster.until("leadership did not return to node 1", |l| l.iter().all(|id| *id == 1)).await;
    // Each handover is counted once the group has answered under its new leader. A group
    // that an election took away from node 1 again on a busy machine is handed back a
    // second time, so there are at least five and there may be more.
    let mut moved = 0;
    for _ in 0..100 {
        moved = cluster.tell(2, None).await.moved + cluster.tell(3, None).await.moved;
        if moved > PARTITIONS as u64 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        moved > PARTITIONS as u64,
        "five groups came back, each by a confirmed handover; {moved} were confirmed"
    );
    assert_eq!(cluster.tell(2, None).await.halted, None);
    // Node 1 is where leaders belong: it hands nothing on.
    assert_eq!(cluster.tell(1, None).await.moved, 0);
    cluster.serves("after").await;

    // An operator's own move, with fail-back held so it is not undone.
    for id in [2, 3] {
        cluster.tell(id, Some("pause")).await;
    }
    assert!(cluster.client.move_leader(0, 9).await.is_err(), "a node that is not a voter was accepted");
    assert!(cluster.client.move_leader(7, 2).await.is_err());
    cluster.client.move_leader(0, 2).await.unwrap();
    let after = cluster
        .until("partition 0 did not move to node 2", |l| l.iter().filter(|id| **id == 2).count() == 1)
        .await;
    assert_eq!(after.iter().filter(|id| **id == 1).count(), PARTITIONS as usize);
    cluster.serves("moved").await;
    cluster.stop().await;
}

/// Purpose: fail-back does not move leaders inside a frozen period of the freeze calendar,
/// nor while a module says it must not be disturbed, and says which is holding it; once
/// the module is free again, leadership returns.
/// Why it matters: a leader change in the middle of a payment run submission or the nightly
/// close is a burst of retried commands at the worst moment of the day. The addendum allows
/// fail-back only in a quiet window, and an operator looking at a cluster that has not
/// failed back needs to be told why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fail_back_waits_for_a_quiet_window() {
    // Frozen around the clock, in two periods, one of them running over midnight.
    let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
    let mut cluster = Cluster::start(
        "failback-frozen",
        failback(100, Duration::from_secs(5), vec![(midnight, noon), (noon, midnight)]),
    )
    .await;
    cluster.lose_the_preferred_node().await;
    cluster.boot(1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let leaders = cluster.until("no leaders", |_| true).await;
    assert!(leaders.iter().all(|id| *id != 1), "leaders moved inside a frozen period: {leaders:?}");
    let reasons: Vec<_> = [cluster.tell(2, None).await, cluster.tell(3, None).await]
        .into_iter()
        .filter_map(|s| s.waiting)
        .collect();
    assert!(reasons.iter().any(|r| r.contains("freeze calendar")), "{reasons:?}");
    cluster.stop().await;

    // No freeze, but the module is in the middle of something.
    let mut cluster = Cluster::start("failback-busy", failback(100, Duration::from_secs(5), vec![])).await;
    cluster.lose_the_preferred_node().await;
    cluster
        .client
        .propose_module_until("desk", &json!(true), Duration::from_secs(30))
        .await
        .unwrap();
    cluster.boot(1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let leaders = cluster.until("no leaders", |_| true).await;
    assert!(leaders.iter().all(|id| *id != 1), "leaders moved while a module was busy: {leaders:?}");
    let reasons: Vec<_> = [cluster.tell(2, None).await, cluster.tell(3, None).await]
        .into_iter()
        .filter_map(|s| s.waiting)
        .collect();
    assert!(reasons.iter().any(|r| r == "module desk is busy"), "{reasons:?}");
    // The module finishes; leadership comes home.
    cluster
        .client
        .propose_module_until("desk", &json!(false), Duration::from_secs(30))
        .await
        .unwrap();
    cluster
        .until("leadership did not return once the module was free", |l| l.iter().all(|id| *id == 1))
        .await;
    cluster.stop().await;
}

/// Purpose: when a group that was just handed back does not answer within its latency
/// budget, the node stops handing groups back, says why, and leaves the rest where they
/// are until an operator resumes it.
/// Why it matters: a preferred region that is back but slow, on a degraded link, would
/// otherwise be given every leader one after another, turning a recovered outage into a
/// new one. Stopping after the first bad move bounds the damage to one share of the groups.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fail_back_stops_itself_when_a_moved_group_is_slow() {
    // A budget no group can meet: the first move's check must fail.
    let mut cluster = Cluster::start("failback-halts", failback(20, Duration::ZERO, vec![])).await;
    cluster.lose_the_preferred_node().await;
    cluster.boot(1).await;
    let mut halted = Vec::new();
    for _ in 0..200 {
        halted = [cluster.tell(2, None).await, cluster.tell(3, None).await]
            .into_iter()
            .filter(|s| s.halted.is_some())
            .collect();
        if !halted.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!halted.is_empty(), "fail-back did not stop itself");
    for status in &halted {
        assert_eq!(status.moved, 0, "a handover that was too slow was counted as sound");
        assert!(status.halted.as_ref().unwrap().contains("the budget is 0 ms"), "{status:?}");
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let leaders = cluster.until("no leaders", |_| true).await;
    assert!(
        leaders.iter().filter(|id| **id == 1).count() <= 2,
        "more than one share per node moved after the halt: {leaders:?}"
    );
    // An operator looks, and resumes: the stop is cleared.
    let node = if cluster.tell(2, None).await.halted.is_some() { 2 } else { 3 };
    assert_eq!(cluster.tell(node, Some("resume")).await.halted, None);
    cluster.serves("after-halt").await;
    cluster.stop().await;
}

/// Purpose: fail-back's settings are read strictly. With no preferred nodes it is off; a
/// bad node number, share, period or time zone is an error that names the setting; and a
/// frozen period is judged in the calendar's own time zone, including one that runs over
/// midnight.
/// Why it matters: these settings decide when leaders may move in production. A typo that
/// silently became "never frozen" would let fail-back run through the nightly close; one
/// that became "off" would leave leaders abroad with nobody told.
#[test]
fn fail_back_settings_are_read_strictly() {
    let read = |pairs: &[(&str, &str)]| {
        let pairs: BTreeMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        FailbackSettings::from_lookup(|key| pairs.get(key).cloned())
    };
    assert_eq!(read(&[]), Ok(None));
    assert_eq!(read(&[("IC_PREFERRED_NODES", "  ")]), Ok(None));
    let settings = read(&[("IC_PREFERRED_NODES", "1, 2"), ("IC_FAILBACK_FROZEN", "23:30-02:30, 06:00-08:00")])
        .unwrap()
        .unwrap();
    assert_eq!(settings.homes, vec![BTreeSet::from([1, 2])]);
    assert_eq!(
        (settings.share_percent, settings.latency_budget, settings.settle),
        (20, Duration::from_secs(2), Duration::from_secs(120))
    );
    assert_eq!(settings.timezone, chrono_tz::Pacific::Auckland);
    for (bad, names) in [
        (vec![("IC_PREFERRED_NODES", "one")], "IC_PREFERRED_NODES"),
        (
            vec![("IC_PREFERRED_NODES", "1"), ("IC_FAILBACK_SHARE_PERCENT", "0")],
            "IC_FAILBACK_SHARE_PERCENT",
        ),
        (
            vec![("IC_PREFERRED_NODES", "1"), ("IC_FAILBACK_SHARE_PERCENT", "150")],
            "IC_FAILBACK_SHARE_PERCENT",
        ),
        (vec![("IC_PREFERRED_NODES", "1"), ("IC_FAILBACK_LATENCY_MS", "soon")], "IC_FAILBACK_LATENCY_MS"),
        (vec![("IC_PREFERRED_NODES", "1"), ("IC_FAILBACK_FROZEN", "23:30")], "IC_FAILBACK_FROZEN"),
        (vec![("IC_PREFERRED_NODES", "1"), ("IC_FAILBACK_FROZEN", "23:30-25:00")], "IC_FAILBACK_FROZEN"),
        (
            vec![("IC_PREFERRED_NODES", "1"), ("IC_FAILBACK_TIMEZONE", "Mars/Olympus")],
            "IC_FAILBACK_TIMEZONE",
        ),
    ] {
        let error = read(&bad).unwrap_err();
        assert!(error.contains(names), "{error}");
    }
    // 11:00 UTC on 11 November is midnight in Auckland (UTC+13 in summer).
    let at = |hour: u32, minute: u32| Utc.with_ymd_and_hms(2026, 11, 11, hour, minute, 0).unwrap();
    assert!(settings.is_frozen(at(11, 0)), "00:00 in Auckland is inside 23:30-02:30");
    assert!(settings.is_frozen(at(10, 45)), "23:45 in Auckland");
    assert!(!settings.is_frozen(at(14, 0)), "03:00 in Auckland");
    assert!(settings.is_frozen(at(18, 0)), "07:00 in Auckland is inside 06:00-08:00");
    assert!(!settings.is_frozen(at(19, 0)), "08:00 in Auckland: the period has ended");
}

/// Purpose: with two leading regions the groups are shared between them: partitions take
/// the regions in turn and each module always falls to the same one; the witness's region,
/// named in the node map but not among the leading regions, leads nothing. Settings that
/// cannot be right (both kinds of home, a leading region with no nodes, no node map, a home
/// with no nodes) are refused, naming the setting.
/// Why it matters: this split is what makes both regions take writes. A group with no home
/// in a leading region would be left wherever an election put it, and two nodes that read the
/// map differently would hand a group back and forth.
#[test]
fn two_leading_regions_share_the_groups() {
    use ic_raft::failback::GroupKey;
    let read = |pairs: &[(&str, &str)]| {
        let pairs: BTreeMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        FailbackSettings::from_lookup(|key| pairs.get(key).cloned())
    };
    let map = ("IC_REGION_NODES", "australiaeast=1,2; australiasoutheast=3,4; australiacentral=5");
    let settings = read(&[("IC_LEADER_REGIONS", "australiaeast, australiasoutheast"), map]).unwrap().unwrap();
    let (sydney, melbourne) = (BTreeSet::from([1, 2]), BTreeSet::from([3, 4]));
    assert_eq!(settings.homes, vec![sydney.clone(), melbourne.clone()]);
    let homes: Vec<&BTreeSet<NodeId>> = (0..4).map(|p| settings.home_of(&GroupKey::Partition(p))).collect();
    assert_eq!(homes, vec![&sydney, &melbourne, &sydney, &melbourne]);
    let finance = settings.home_of(&GroupKey::Module("finance".to_string())).clone();
    assert_eq!(
        settings.home_of(&GroupKey::Module("finance".to_string())),
        &finance,
        "a module's home is stable"
    );
    assert!(!finance.contains(&5));
    for (bad, names) in [
        (vec![("IC_LEADER_REGIONS", "australiaeast"), ("IC_PREFERRED_NODES", "1")], "not both"),
        (vec![("IC_LEADER_REGIONS", "australiaeast")], "IC_REGION_NODES"),
        (vec![("IC_LEADER_REGIONS", "westus"), map], "westus"),
        (
            vec![("IC_LEADER_REGIONS", "australiaeast"), ("IC_REGION_NODES", "australiaeast")],
            "IC_REGION_NODES",
        ),
        (
            vec![("IC_LEADER_REGIONS", "australiaeast"), ("IC_REGION_NODES", "australiaeast=,")],
            "at least one node",
        ),
    ] {
        let error = read(&bad).unwrap_err();
        assert!(error.contains(names), "{error}");
    }
}
