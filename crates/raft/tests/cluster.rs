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
