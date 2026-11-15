//! `ic-admin watch` reads the nodes' statuses into the findings the alert rules act on.

use std::collections::BTreeMap;

use ic_api::admin::AdminCommand;
use ic_api::watch::{findings, log, Watch, DEFAULT_BACKLOG_LIMIT};
use ic_raft::NodeStatus;
use serde_json::json;

fn row(partition: u16, role: &str, leader: Option<u64>, applied: u64, hash: &str, journal: u64) -> NodeStatus {
    serde_json::from_value(json!({
        "id": 1, "partition": partition, "role": role, "leader": leader, "term": 3,
        "last_log_index": applied, "last_applied": applied, "business_date": null,
        "state_hash": hash, "ledger_head": "h", "ledger_entries": 0, "policies": 0,
        "voters": [1, 2, 3], "first_log_index": 1,
        "backlog": { "messages": 1, "journal": journal, "events": 2 }
    }))
    .unwrap()
}

/// Purpose: from every node's status the watch finds the unreachable nodes, the partitions no
/// node names a leader for, the partitions whose leader holds more than the backlog limit (the
/// leader's backlog, not a follower's), and the partitions where members at the same applied
/// index disagree on state; a healthy cluster gives no findings. `watch` parses with and
/// without a limit, and its log lines are written without failing.
/// Why it matters: these findings are the only signal the "no leader", "node down" and
/// "housekeeping backlog" alerts have; a false negative leaves a partition refusing writes
/// with nobody told, and counting a follower's stale backlog would page people for nothing.
#[test]
fn the_watch_finds_what_needs_a_person() {
    let mut status = BTreeMap::new();
    status.insert(
        "ic-node-1".to_string(),
        Ok(vec![
            row(0, "leader", Some(1), 10, "a", 10),
            row(1, "follower", None, 7, "b", 0),
            row(2, "leader", Some(1), 5, "c", 9_000),
            row(3, "follower", Some(2), 4, "x", 9_000),
        ]),
    );
    status.insert(
        "ic-node-2".to_string(),
        Ok(vec![
            row(0, "follower", Some(1), 10, "a", 10),
            row(1, "candidate", None, 7, "b", 0),
            row(2, "follower", Some(1), 5, "c", 0),
            row(3, "leader", Some(2), 4, "y", 3),
        ]),
    );
    status.insert("ic-node-3".to_string(), Err("connection refused".to_string()));
    let watch = findings(&status, DEFAULT_BACKLOG_LIMIT);
    assert_eq!(watch.nodes, 3);
    assert_eq!(watch.unreachable, vec!["ic-node-3".to_string()]);
    assert_eq!(watch.partitions, 4);
    assert_eq!(watch.leaderless, vec![1]);
    assert_eq!(watch.backlogged, BTreeMap::from([(2, 9_003)]));
    assert_eq!(watch.largest_backlog, 9_003);
    assert_eq!(watch.diverged, vec![3]);
    assert!(!watch.healthy());
    log(&watch, DEFAULT_BACKLOG_LIMIT);

    let healthy = BTreeMap::from([("ic-node-1".to_string(), Ok(vec![row(0, "leader", Some(1), 3, "a", 0)]))]);
    let watch = findings(&healthy, 10);
    assert!(watch.healthy());
    assert_eq!(
        watch,
        Watch {
            nodes: 1,
            partitions: 1,
            largest_backlog: 3,
            ..Watch::default()
        }
    );
    log(&watch, 10);

    let args = |line: &str| line.split_whitespace().map(String::from).collect::<Vec<_>>();
    assert_eq!(
        AdminCommand::parse(&args("watch")),
        Ok(AdminCommand::Watch {
            backlog_limit: DEFAULT_BACKLOG_LIMIT
        })
    );
    assert_eq!(AdminCommand::parse(&args("watch 200")), Ok(AdminCommand::Watch { backlog_limit: 200 }));
    assert!(AdminCommand::parse(&args("watch lots")).unwrap_err().contains("backlog limit"));
}
