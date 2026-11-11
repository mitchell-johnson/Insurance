//! The simulated cluster over a log whose disk can fail.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::NaiveDate;
use ic_core::{Command, GroupId};
use ic_log::{LogError, MemoryLog, OrderedLog};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::{Cluster, Delivery};

/// A log that refuses appends while its switch is on.
struct Failing {
    inner: MemoryLog,
    broken: Arc<AtomicBool>,
}

impl OrderedLog for Failing {
    fn append(&mut self, record: &[u8]) -> Result<u64, LogError> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(LogError::Io(std::io::Error::other("disk full")));
        }
        self.inner.append(record)
    }
    fn read(&self, from: u64, max: usize) -> Result<Vec<(u64, Vec<u8>)>, LogError> {
        self.inner.read(from, max)
    }
    fn last_index(&self) -> u64 {
        self.inner.last_index()
    }
}

/// Purpose: a command whose log append fails is refused with the log's error and changes
/// nothing, the same request succeeds once the log is writable again, a command for a partition
/// that does not exist is refused, and a transfer message that could not be delivered because
/// the receiving partition's log failed is recorded as a relay error.
/// Why it matters: the log is the only durable thing. A command applied to state without being
/// logged would vanish on restart while its effects had already been reported, so a failed
/// append must leave the state untouched and say so. The simulator's year-long runs treat any
/// relay error as a failed run, which only means something if a failed delivery is recorded.
#[test]
fn a_command_that_cannot_be_logged_changes_nothing() {
    let open = NaiveDate::from_ymd_opt(2027, 11, 15).unwrap();
    let switches: BTreeMap<u16, Arc<AtomicBool>> = (0..2).map(|p| (p, Arc::new(AtomicBool::new(false)))).collect();
    let logs = switches
        .iter()
        .map(|(p, broken)| {
            let log = Failing {
                inner: MemoryLog::new(),
                broken: broken.clone(),
            };
            (*p, log)
        })
        .collect();
    let mut cluster = Cluster::recover(logs, 3).unwrap();
    cluster.open(open).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let issue = term_life(
        1,
        open,
        NaiveDate::from_ymd_opt(1988, 4, 2).unwrap(),
        Sex::Female,
        false,
        250_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    let (home, policy) = (cluster.partition_of(&issue.group), issue.policy.clone());
    let other = 1 - home;

    // The disk under the policy's partition fails: the application is refused and nothing changed.
    let before = cluster.state(home).state_hash();
    switches[&home].store(true, Ordering::SeqCst);
    let refused = cluster.submit_keyed(home, "app-1", "new-business", issue_command(issue.clone()));
    assert_eq!(refused.result.unwrap_err().code, "log");
    assert_eq!(cluster.state(home).state_hash(), before);
    assert_eq!(cluster.locate(&policy), None);
    // The same request, once the disk is back, is applied as new.
    switches[&home].store(false, Ordering::SeqCst);
    let applied = cluster.submit_keyed(home, "app-1", "new-business", issue_command(issue));
    assert!(applied.result.is_ok() && !applied.duplicate);
    assert_eq!(cluster.locate(&policy), Some(home));

    assert_eq!(
        cluster
            .submit(9, "policy-servicing", Command::CancelPolicy { policy: policy.clone() })
            .unwrap_err()
            .code,
        "unknown_partition"
    );

    // A transfer whose record cannot be written on the receiving partition.
    let target_group = (0..)
        .map(|i| GroupId(format!("grp-away-{i}")))
        .find(|g| cluster.partition_of(g) == other)
        .unwrap();
    let begin = Command::BeginTransfer {
        policy: policy.clone(),
        transfer_id: "t-1".to_string(),
        target_group,
        target_partition: other,
    };
    cluster.submit(home, "transfer", begin).unwrap();
    switches[&other].store(true, Ordering::SeqCst);
    assert_eq!(cluster.relay_messages(|_| Delivery::Deliver), 1);
    assert_eq!(cluster.relay_errors.len(), 1, "{:?}", cluster.relay_errors);
    assert!(cluster.relay_errors[0].contains("disk full"), "{:?}", cluster.relay_errors);
    assert_eq!(cluster.locate(&policy), Some(home), "the policy has not left its partition");
}
