//! What the core does for underwriting and the migration load: ending
//! interim cover, and policies that arrive from another system.

use chrono::NaiveDate;
use ic_core::{Command, PolicyId, PolicyStatus, Reject};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn code(r: Result<(), Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

/// Purpose: underwriting can cancel a policy, as it must to end interim cover when the proposal
/// it stood in for ends; a module with no business cancelling still cannot; and the books
/// reconcile on the days after.
/// Why it matters: interim cover left running beside the policy that replaced it, or after a
/// declined proposal, is cover nobody priced. If underwriting could not end it, it would run
/// until somebody noticed.
#[test]
fn underwriting_can_end_interim_cover() {
    let mut c = cluster();
    let issue = term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 250_000_00, None, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    days(&mut c, 3);

    assert_eq!(code(c.submit(0, "claims", Command::CancelPolicy { policy: id.clone() })), "caller_not_allowed");
    c.submit(0, "underwriting", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(c.state(0).policies[&id].status, PolicyStatus::Cancelled);
    assert_eq!(code(c.submit(0, "underwriting", Command::CancelPolicy { policy: id.clone() })), "not_in_force");
    days(&mut c, 3);
}

/// Purpose: a policy from another system is issued by the migration load through the ordinary
/// issue command, marked as migrated and with its original inception; nobody else can mark a
/// policy as migrated, the migration load cannot issue one that is not, and a command written
/// before the flag existed still reads as an ordinary issue.
/// Why it matters: there is no back door for migrated business, so the flag is the only thing
/// that tells a migrated policy from one written here. If new business could set it, or the
/// migration load could leave it off, that record would be wrong for the life of the policy.
#[test]
fn only_the_migration_load_issues_a_migrated_policy() {
    let mut c = cluster();
    let mut legacy = term_life(2, d(2027, 9, 15), d(1985, 6, 1), Sex::Male, false, 300_000_00, None, Frequency::Monthly, None);
    let id: PolicyId = legacy.policy.clone();

    assert_eq!(code(c.submit(0, "migration", issue_command(legacy.clone()))), "migrated");
    legacy.migrated = true;
    assert_eq!(code(c.submit(0, "new-business", issue_command(legacy.clone()))), "migrated");
    assert!(!c.state(0).policies.contains_key(&id));

    c.submit(0, "migration", issue_command(legacy.clone())).unwrap();
    let policy = &c.state(0).policies[&id];
    assert!(policy.migrated);
    assert_eq!(policy.inception, d(2027, 9, 15));
    days(&mut c, 3);

    // The flag is absent from an ordinary command on the wire, and absence reads as false.
    let ordinary = term_life(3, start(), d(1990, 3, 10), Sex::Female, false, 250_000_00, None, Frequency::Monthly, None);
    let wire = serde_json::to_value(&ordinary).unwrap();
    assert!(wire.get("migrated").is_none());
    let read: ic_core::command::IssuePolicy = serde_json::from_value(wire).unwrap();
    assert!(!read.migrated);
    c.submit(0, "new-business", issue_command(ordinary.clone())).unwrap();
    assert!(!c.state(0).policies[&ordinary.policy].migrated);
}
