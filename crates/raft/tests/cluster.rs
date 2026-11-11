//! Resilience of a ledger partition's Raft group: what happens to committed
//! money when members crash, lose their network, or all stop at once.
//!
//! Every test runs real openraft members over durable logs in one process.
//! Crashing a member stops its tasks and throws its memory away; only its
//! disk survives, as with a killed container.

mod common;

use std::time::Duration;

use common::{receipt, replayed_hash, Group, PATIENCE};
use ic_raft::ProposeError;

/// Purpose: every member of a healthy group applies the same commands and ends with the
/// same state hash, and replaying a member's committed log from nothing gives that hash too.
/// Why it matters: this is the property the whole design rests on. If replicas could differ,
/// a failover would silently change balances, and an auditor could not rebuild the ledger.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replicas_hold_identical_state_and_the_log_reproduces_it() {
    let mut g = Group::start("identical", 3).await;
    g.configure().await;
    let policy = g.issue(1).await;
    g.run("cash-matching", receipt(&policy, "r-1", 61_84)).await;

    let hash = g.settle().await;
    for node in g.reachable() {
        let status = node.status();
        assert_eq!(status.state_hash, hash);
        assert_eq!(status.policies, 1);
        assert_eq!(status.business_date, Some(common::start_date()));
        assert_eq!(replayed_hash(&node.log), hash, "node {} log does not reproduce its state", node.id);
    }
    g.stop().await;
}

/// Purpose: a five-member group that loses two members keeps accepting business, and the
/// two catch up to the same state when they come back.
/// Why it matters: five voters across three regions exist so that one region can be lost
/// outright. This proves the loss costs no availability and the returning members need no
/// manual repair.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn losing_a_minority_does_not_stop_the_ledger() {
    let mut g = Group::start("minority", 5).await;
    g.configure().await;
    let leader = g.leader().await;
    let lost: Vec<_> = g.followers_of(leader).into_iter().take(2).collect();
    for id in &lost {
        g.crash(*id).await;
    }

    let policy = g.issue(1).await;
    g.run("cash-matching", receipt(&policy, "r-1", 61_84)).await;
    let with_three = g.settle().await;

    for id in &lost {
        g.boot(*id).await;
    }
    let with_five = g.settle().await;
    assert_eq!(with_five, with_three, "the returning members changed the state");
    assert_eq!(g.reachable().len(), 5);
    assert!(g.statuses().iter().all(|s| s.policies == 1));
    g.stop().await;
}

/// Purpose: when the leader dies, another member takes over without losing anything the
/// old leader confirmed, and a client that retries its last command with the same
/// idempotency key gets the original result instead of a second posting.
/// Why it matters: a leader crash is the common failure. The client cannot know whether its
/// in-flight command landed, so it must be able to retry blindly. A double-applied receipt
/// is a customer's money counted twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leader_crash_fails_over_and_a_retry_applies_once() {
    let mut g = Group::start("failover", 3).await;
    g.configure().await;
    let policy = g.issue(1).await;
    let first = g.run_keyed("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84)).await;
    assert!(!first.duplicate);
    let before = g.settle().await;
    let entries_before = g.statuses()[0].ledger_entries;

    let old = g.leader().await;
    g.crash(old).await;
    let new = g.leader().await;
    assert_ne!(new, old);

    let retry = g.run_keyed("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84)).await;
    assert!(retry.duplicate, "the retry was treated as a new receipt");
    let after = g.settle().await;
    assert_eq!(after, before, "the retry changed the state");
    assert_eq!(g.statuses()[0].ledger_entries, entries_before);

    // The group still does new work, and the old leader rejoins as a follower.
    g.run_keyed("receipt:r-2", "cash-matching", receipt(&policy, "r-2", 61_84)).await;
    g.boot(old).await;
    let healed = g.settle().await;
    assert_ne!(healed, before);
    assert_eq!(g.reachable().len(), 3);
    g.stop().await;
}

/// Purpose: a group that has lost its majority confirms nothing, changes nothing, and
/// resumes by itself when a majority is back; the command the client was retrying
/// throughout is then applied exactly once.
/// Why it matters: with three of five members gone the survivors cannot know they are not
/// the ones cut off. Confirming a payment here could later be undone by the other side,
/// which is the one thing a ledger must never do. Refusing is correct; losing the command
/// or applying it twice afterwards is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn losing_quorum_stops_writes_and_loses_nothing() {
    let mut g = Group::start("quorum", 5).await;
    g.configure().await;
    let policy = g.issue(1).await;
    let before = g.settle().await;
    let entries_before = g.statuses()[0].ledger_entries;

    let leader = g.leader().await;
    let mut lost = g.followers_of(leader);
    lost.truncate(2);
    lost.push(leader);
    for id in &lost {
        g.crash(*id).await;
    }

    let stranded = g.envelope("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84));
    assert_eq!(g.submit_within(&stranded, Duration::from_secs(3)).await, None, "a minority confirmed a write");
    assert_eq!(g.leader_now(), None);
    for status in g.statuses() {
        assert_eq!(status.state_hash, before, "node {} changed state without a quorum", status.id);
    }

    for id in &lost {
        g.boot(*id).await;
    }
    let reply = g.submit_within(&stranded, PATIENCE).await.expect("the healed group did not accept the retry");
    assert_eq!(reply.rejected, None);
    let after = g.settle().await;
    assert_ne!(after, before);
    let entries_after = g.statuses()[0].ledger_entries;
    assert!(entries_after > entries_before);

    // However many times the receipt reached the log while the group was
    // broken, the core applied it once.
    let again = g.submit_within(&stranded, PATIENCE).await.unwrap();
    assert!(again.duplicate);
    assert_eq!(g.settle().await, after);
    assert_eq!(g.statuses()[0].ledger_entries, entries_after);
    g.stop().await;
}

/// Purpose: a leader cut off from the others cannot confirm a command; the majority elects
/// a new leader and carries on; and when the network heals the old leader discards what it
/// never committed and follows.
/// Why it matters: this is split brain. Two leaders each confirming payments would fork the
/// ledger. The test proves only the majority side makes progress and the stale side's
/// unconfirmed entry is erased from its disk, not merged in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_isolated_leader_confirms_nothing_and_rejoins_as_a_follower() {
    let mut g = Group::start("split", 3).await;
    g.configure().await;
    let policy = g.issue(1).await;
    g.settle().await;

    let old = g.leader().await;
    g.isolate(old);

    // The old leader still believes it leads. It takes the command into its
    // own log but can never get a majority for it.
    let stranded = g.envelope("receipt:stranded", "cash-matching", receipt(&policy, "stranded", 10_00));
    let outcome = tokio::time::timeout(Duration::from_secs(2), g.nodes[&old].propose(stranded.clone())).await;
    assert!(!matches!(outcome, Ok(Ok(_))), "an isolated leader confirmed a write: {outcome:?}");

    let new = g.leader().await;
    assert_ne!(new, old);
    g.run_keyed("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84)).await;
    let majority = g.settle().await;

    g.reconnect(old);
    let healed = g.settle().await;
    assert_eq!(healed, majority, "the stale leader's entry leaked into the state");
    let status = g.nodes[&old].status();
    assert_ne!(status.role, "leader", "the stale leader kept its role in an old term");
    assert_eq!(status.state_hash, majority);
    assert_eq!(replayed_hash(&g.nodes[&old].log), majority);

    // The stranded receipt was never applied anywhere, so a retry is new work.
    let retry = g.submit_within(&stranded, PATIENCE).await.unwrap();
    assert!(!retry.duplicate);
    assert_eq!(retry.rejected, None);
    g.stop().await;
}

/// Purpose: after every member stops at once, the group comes back from its disks with
/// exactly the state it had and goes on accepting work.
/// Why it matters: a whole-cluster outage (a bad rollout, a power event) must cost time,
/// never data. The state machine lives in memory, so this proves the durable log alone is
/// enough to rebuild it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_restart_rebuilds_the_same_state_from_disk() {
    let mut g = Group::start("restart", 3).await;
    g.configure().await;
    let policy = g.issue(1).await;
    g.run_keyed("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84)).await;
    let before = g.settle().await;

    for id in 1..=3 {
        g.crash(id).await;
    }
    assert!(g.reachable().is_empty());
    for id in 1..=3 {
        g.boot(id).await;
    }
    assert_eq!(g.settle().await, before);

    let retry = g.run_keyed("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84)).await;
    assert!(retry.duplicate, "the idempotency record did not survive the restart");
    g.run_keyed("receipt:r-2", "cash-matching", receipt(&policy, "r-2", 61_84)).await;
    assert_ne!(g.settle().await, before);
    g.stop().await;
}

/// Purpose: a member whose final log record was half written when it died starts cleanly,
/// drops the torn record, and catches up from its peers.
/// Why it matters: a kill in the middle of a write is the normal way a process dies. A
/// node that refuses to start on a torn tail turns a routine crash into an outage with a
/// manual repair.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_torn_final_record_is_dropped_on_restart() {
    use std::io::Write as _;

    let mut g = Group::start("torn", 3).await;
    g.configure().await;
    let leader = g.leader().await;
    let victim = g.followers_of(leader)[0];
    g.settle().await;
    g.crash(victim).await;

    let journal = g.disk(victim).join("log.jsonl");
    let whole = std::fs::metadata(&journal).unwrap().len();
    let mut file = std::fs::OpenOptions::new().append(true).open(&journal).unwrap();
    file.write_all(br#"{"append":{"log_id":{"leader_id":{"term":9"#).unwrap();
    drop(file);

    let policy = g.issue(1).await;
    g.boot(victim).await;
    let hash = g.settle().await;
    assert_eq!(g.nodes[&victim].status().state_hash, hash);
    assert_eq!(g.nodes[&victim].status().policies, 1);
    assert!(
        std::fs::metadata(&journal).unwrap().len() > whole,
        "the member did not append after the torn record"
    );
    let text = std::fs::read_to_string(&journal).unwrap();
    assert!(
        text.lines().all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok()),
        "the torn record is still in the journal"
    );
    g.run("cash-matching", receipt(&policy, "r-1", 61_84)).await;
    g.stop().await;
}

/// Purpose: a broken link between two followers does not disturb the group, and a client
/// that asks a follower is told who the leader is.
/// Why it matters: partial network faults are more common than clean ones. Followers do
/// not need to talk to each other, so this must be a non-event; and a client must be able
/// to find the leader from any member rather than time out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_severed_follower_link_is_harmless_and_followers_redirect() {
    let mut g = Group::start("link", 3).await;
    g.configure().await;
    let leader = g.leader().await;
    let followers = g.followers_of(leader);
    g.net.set_link(followers[0], followers[1], false);

    let policy = g.issue(1).await;
    let envelope = g.envelope("receipt:r-1", "cash-matching", receipt(&policy, "r-1", 61_84));
    match g.nodes[&followers[0]].propose(envelope.clone()).await {
        Err(ProposeError::NotLeader { leader: told, addr }) => {
            assert_eq!(told, Some(leader));
            assert_eq!(addr, Some(format!("local-{leader}")));
        }
        other => panic!("a follower should redirect to the leader, got {other:?}"),
    }
    assert!(g.nodes[&leader].propose(envelope).await.is_ok());
    assert_eq!(g.leader().await, leader, "the leader changed over a link it does not use");
    g.net.set_link(followers[0], followers[1], true);
    g.settle().await;
    g.stop().await;
}

/// Wait until a member has dropped the start of its log behind a snapshot.
async fn wait_for_purge(g: &Group, id: u64) -> u64 {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        match g.nodes[&id].log.first_index() {
            Some(first) if first > 1 => return first,
            _ => assert!(tokio::time::Instant::now() < deadline, "node {id} never dropped its log"),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Purpose: with snapshots on, every member drops the log behind its snapshot, and a
/// restart of the whole group from snapshot plus remaining log gives exactly the state it
/// had, including the record of which idempotency keys were already applied.
/// Why it matters: the log cannot grow for fifty years. Dropping it is only safe if the
/// snapshot really is the whole state; a field left out would surface months later as a
/// replica that disagrees after a restart, or as a retried payment applied twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_from_snapshot_and_log_rebuilds_the_same_state() {
    let mut g = Group::start_snapshotting("snapshot", 3, 20).await;
    g.configure().await;
    let policy = g.issue(1).await;
    for n in 0..70 {
        g.run_keyed(&format!("receipt:r-{n}"), "cash-matching", receipt(&policy, &format!("r-{n}"), 1_00))
            .await;
    }
    let before = g.settle().await;
    for id in 1..=3 {
        let first = wait_for_purge(&g, id).await;
        assert!(first > 20, "node {id} kept its log from index {first}");
        assert!(g.disk(id).join("snapshot.json").exists());
        let journal = std::fs::read_to_string(g.disk(id).join("log.jsonl")).unwrap();
        assert!(journal.lines().count() < 60, "node {id} did not shrink its journal file");
    }

    for id in 1..=3 {
        g.crash(id).await;
    }
    for id in 1..=3 {
        g.boot(id).await;
    }
    assert_eq!(g.settle().await, before);
    let retry = g.run_keyed("receipt:r-5", "cash-matching", receipt(&policy, "r-5", 1_00)).await;
    assert!(retry.duplicate, "a receipt applied before the snapshot was applied again after it");
    g.run_keyed("receipt:r-new", "cash-matching", receipt(&policy, "r-new", 1_00)).await;
    assert_ne!(g.settle().await, before);
    g.stop().await;
}

/// Purpose: a member that was away while the others moved on and dropped their logs is
/// brought up to date with a snapshot when it returns with its disk intact.
/// Why it matters: a node can be down for longer than the log is kept: a failed zone, a
/// long maintenance window. It must rejoin without anyone copying files by hand, and must
/// not be left permanently behind because the entries it needs no longer exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_left_behind_the_log_catches_up_from_a_snapshot() {
    let mut g = Group::start_snapshotting("behind", 3, 20).await;
    g.configure().await;
    let policy = g.issue(1).await;
    let leader = g.leader().await;
    let away = g.followers_of(leader)[0];
    g.crash(away).await;
    for n in 0..60 {
        g.run_keyed(&format!("receipt:a-{n}"), "cash-matching", receipt(&policy, &format!("a-{n}"), 1_00))
            .await;
    }
    let first_kept = wait_for_purge(&g, leader).await;
    assert!(
        first_kept > g.nodes[&leader].log.last_index().unwrap() - 60,
        "the leader still holds what the absent member missed"
    );

    g.boot(away).await;
    let hash = g.settle().await;
    let status = g.nodes[&away].status();
    assert_eq!(status.state_hash, hash);
    assert!(
        status.first_log_index.unwrap_or(0) > 20,
        "the member replayed entries the leader no longer had: no snapshot was used"
    );
    g.stop().await;
}

/// Purpose: a member that returns under its old number with an empty disk is shut out by
/// every other member, the group carries on without it, and the lost machine is replaced
/// by a new member with a new number, which is caught up by snapshot and then votes.
/// Why it matters: a member that has forgotten what it acknowledged can make Raft lose a
/// committed entry or elect two leaders. On Kubernetes this is one deleted volume claim
/// away: the pod restarts with the same name and nothing on its disk. The cluster must
/// refuse it by itself, and there must be a safe, tested way to restore full strength.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_that_lost_its_disk_is_shut_out_and_replaced() {
    let mut g = Group::start_snapshotting("lost-disk", 3, 20).await;
    g.configure().await;
    let policy = g.issue(1).await;
    let leader = g.leader().await;
    let followers = g.followers_of(leader);
    let (healthy, lost) = (followers[0], followers[1]);
    g.settle().await;

    g.crash(lost).await;
    std::fs::remove_dir_all(g.disk(lost)).unwrap();
    g.boot(lost).await;
    for n in 0..40 {
        g.run_keyed(&format!("receipt:r-{n}"), "cash-matching", receipt(&policy, &format!("r-{n}"), 1_00))
            .await;
    }
    // The impostor is running and connected, and has been given nothing.
    let impostor = g.nodes[&lost].status();
    assert_eq!(impostor.last_applied, None, "a member with an empty disk was fed the log under its old number");
    assert_eq!(impostor.policies, 0);
    assert_eq!(g.leader().await, leader, "the impostor disturbed the group");
    g.crash(lost).await;

    // Replace it: a new machine, a new number.
    let replacement = 4;
    g.boot(replacement).await;
    let members = [leader, healthy, replacement].into_iter().map(|id| (id, format!("local-{id}"))).collect();
    g.nodes[&leader].set_members(members).await.unwrap();
    let hash = g.settle().await;
    let status = g.nodes[&replacement].status();
    assert_eq!(status.state_hash, hash);
    assert!(status.first_log_index.unwrap_or(0) > 1, "the new member was not caught up by snapshot");
    let mut voters = g.nodes[&leader].status().voters;
    voters.sort_unstable();
    let mut expected = vec![leader, healthy, replacement];
    expected.sort_unstable();
    assert_eq!(voters, expected);

    // The new member counts: with the other follower gone, it and the leader are the majority.
    g.crash(healthy).await;
    g.run_keyed("receipt:after", "cash-matching", receipt(&policy, "after", 1_00)).await;
    // Asking for the membership the group already has changes nothing.
    let same = [leader, healthy, replacement].into_iter().map(|id| (id, format!("local-{id}"))).collect();
    g.nodes[&leader].set_members(same).await.unwrap();
    g.stop().await;
}

/// Purpose: a node refuses to start on a disk that belongs to a different node, keeps the
/// same incarnation across restarts on its own disk, and remembers across restarts which
/// incarnation it first met for each peer.
/// Why it matters: these files are what the impostor check rests on. A mis-mounted volume
/// must stop the node rather than let it vote as someone else, and the memory of peers
/// must survive a restart or the check would lapse exactly when a cluster is recovering.
#[test]
fn a_node_knows_its_own_disk_and_remembers_its_peers() {
    let dir = common::scratch("identity");
    let first = ic_raft::Identity::open(1, &dir).unwrap();
    assert!(first.admit(2, "aaaa").is_ok());
    assert!(first.admit(2, "aaaa").is_ok());
    assert_eq!(first.admit(2, "bbbb").unwrap_err().peer, 2);

    let again = ic_raft::Identity::open(1, &dir).unwrap();
    assert_eq!(again.incarnation, first.incarnation);
    assert!(again.admit(2, "aaaa").is_ok(), "a known peer was forgotten over a restart");
    assert!(again.admit(2, "bbbb").is_err(), "an impostor was admitted after a restart");
    assert!(again.admit(3, "cccc").is_ok());

    let wrong = ic_raft::Identity::open(2, &dir).err().expect("node 2 started on node 1's disk").to_string();
    assert!(wrong.contains("wrong disk"), "{wrong}");
    std::fs::write(dir.join("identity.json"), b"not json").unwrap();
    assert!(ic_raft::Identity::open(1, &dir).is_err());

    let (a, b) = (ic_raft::Identity::ephemeral(1), ic_raft::Identity::ephemeral(1));
    assert_ne!(a.incarnation, b.incarnation);
    let _ = std::fs::remove_dir_all(dir);
}

/// Purpose: a member refuses to start on a snapshot file that is damaged or belongs to
/// another partition, and says which file.
/// Why it matters: starting from an empty state instead would look healthy and then have
/// the member vote and serve reads with no history. Refusing makes the operator replace
/// the disk, after which the leader sends a good snapshot.
#[test]
fn a_damaged_or_foreign_snapshot_stops_the_member() {
    let dir = common::scratch("bad-snapshot");
    std::fs::write(dir.join("snapshot.json"), b"{\"core\": tru").unwrap();
    let error = ic_raft::Machine::open(0, &dir).err().expect("a damaged snapshot was accepted").to_string();
    assert!(error.contains("snapshot.json"), "{error}");

    // A healthy member's snapshot, offered to a member of another partition.
    let source = common::scratch("foreign-snapshot");
    assert!(ic_raft::Machine::open(3, &source).is_ok());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        use openraft::storage::RaftSnapshotBuilder as _;
        let mut machine = ic_raft::Machine::open(3, &source).unwrap();
        machine.build_snapshot().await.unwrap();
    });
    assert!(ic_raft::Machine::open(3, &source).is_ok(), "a member could not restart from its own snapshot");
    let error = ic_raft::Machine::open(4, &source)
        .err()
        .expect("another partition's snapshot was accepted")
        .to_string();
    assert!(error.contains("partition 3"), "{error}");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(source);
}
