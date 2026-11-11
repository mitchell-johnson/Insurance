//! The simulator's end-of-run audit, shown a cluster that is sound and then
//! one fault at a time.

use chrono::NaiveDate;
use ic_core::event::{Event, EventRecord};
use ic_core::{PolicyId, PolicyStatus, APPLY_BUDGET};
use ic_product::{Frequency, Sex};
use ic_readmodel::Projections;
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::year::audit;
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// A two-partition cluster with four policies and a journal, the events it produced, and the
/// policies' identities.
fn business() -> (Cluster, Vec<EventRecord>, Vec<PolicyId>) {
    let mut cluster = Cluster::in_memory(2, 5);
    cluster.keep_journal = true;
    cluster.open(d(2027, 11, 15)).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let mut policies = Vec::new();
    for n in 1..=4u64 {
        let issue = term_life(n, d(2027, 11, 15), d(1985, 6, 1), Sex::Male, false, 300_000_00, None, Frequency::Monthly, None);
        let partition = cluster.partition_of(&issue.group);
        policies.push(issue.policy.clone());
        cluster.submit(partition, "new-business", issue_command(issue)).unwrap();
    }
    let events = cluster.take_events();
    (cluster, events, policies)
}

fn view_of(events: &[EventRecord]) -> Projections {
    let mut view = Projections::new();
    for event in events {
        view.apply(event).unwrap();
    }
    view
}

/// Purpose: the audit that ends every simulated year passes a sound cluster and names the fault
/// when shown each one in turn: a journal entry altered after the fact, a journal missing its
/// last entry, a policy entry with a line outside the policy's statutory fund, a state that the log does not replay to, a read model that stopped following
/// the stream, a command that broke the apply budget, a directory that does not list a policy
/// where it really is, and a directory holding the wrong status.
/// Why it matters: the year-long simulation is the evidence that the ledger, the stream and
/// the read models stay consistent, and that evidence is only as good as the audit's ability
/// to fail. Every other test shows it passing. If one of these checks compared a thing with
/// itself, a real divergence would end a run with a clean report.
#[test]
fn the_audit_names_each_kind_of_divergence() {
    let (cluster, events, policies) = business();
    let view = view_of(&events);
    audit(&cluster, &view, &policies, true).unwrap();

    // A journal entry rewritten after it was sealed.
    let (mut cluster, events, policies) = business();
    let view = view_of(&events);
    let (partition, node) = cluster.nodes.iter_mut().find(|(_, n)| n.journal.len() > 1).unwrap();
    let partition = *partition;
    node.journal[0].salt.push('x');
    let err = audit(&cluster, &view, &policies, true).unwrap_err();
    assert!(err.starts_with(&format!("partition {partition}: ")), "{err}");
    // The same cluster passes when the journal is not being verified.
    audit(&cluster, &view, &policies, false).unwrap();

    // A policy's entry with a line outside the policy's statutory fund.
    let (mut cluster, events, policies) = business();
    let view = view_of(&events);
    let (partition, node) = cluster.nodes.iter_mut().find(|(_, n)| n.journal.iter().any(|e| e.body.policy.is_some())).unwrap();
    let partition = *partition;
    let entry = node.journal.iter_mut().find(|e| e.body.policy.is_some()).unwrap();
    entry.body.postings.last_mut().unwrap().dims.statutory_fund = None;
    let seq = entry.id.seq;
    assert_eq!(
        audit(&cluster, &view, &policies, true).unwrap_err(),
        format!("partition {partition}: entry {seq} does not sit in one statutory fund")
    );

    // A journal that lost its newest entry: the chain is whole but ends short of the ledger.
    let (mut cluster, events, policies) = business();
    let view = view_of(&events);
    let (partition, node) = cluster.nodes.iter_mut().find(|(_, n)| n.journal.len() > 1).unwrap();
    let partition = *partition;
    node.journal.pop();
    assert_eq!(
        audit(&cluster, &view, &policies, true).unwrap_err(),
        format!("partition {partition}: chain head does not match")
    );

    // A state that is not what the log replays to.
    let (mut cluster, events, policies) = business();
    let view = view_of(&events);
    cluster.nodes.get_mut(&1).unwrap().replica.state.business_date = Some(d(2030, 1, 1));
    assert_eq!(
        audit(&cluster, &view, &policies, true).unwrap_err(),
        "partition 1: replay produced a different state hash"
    );

    // A read model that missed the newest event of a partition.
    let (cluster, events, policies) = business();
    let last = events.iter().rposition(|e| e.partition == 0).unwrap();
    let mut short = events.clone();
    short.remove(last);
    let err = audit(&cluster, &view_of(&short), &policies, true).unwrap_err();
    assert!(err.starts_with("partition 0: read model is at "), "{err}");

    // A command that did more work than one apply may.
    let (mut cluster, events, policies) = business();
    cluster.nodes.get_mut(&0).unwrap().max_work_units = APPLY_BUDGET + 1;
    let err = audit(&cluster, &view_of(&events), &policies, true).unwrap_err();
    assert!(err.contains("the apply budget is"), "{err}");

    // A directory that learned of a policy under another identity.
    let (cluster, mut events, policies) = business();
    for record in &mut events {
        if let Event::PolicyIssued { policy, .. } = &mut record.event {
            if *policy == policies[0] {
                *policy = PolicyId("pol-someone-else".to_string());
            }
        }
    }
    let err = audit(&cluster, &view_of(&events), &policies, true).unwrap_err();
    assert!(err.starts_with(&format!("directory lists {} on None", policies[0])), "{err}");

    // A directory told of a lapse that never happened.
    let (cluster, mut events, policies) = business();
    let template = events.iter().rfind(|e| matches!(e.event, Event::PolicyIssued { .. })).unwrap().clone();
    let Event::PolicyIssued { policy, .. } = &template.event else {
        unreachable!()
    };
    let policy = policy.clone();
    let record = events.iter_mut().rfind(|e| e.partition == template.partition).unwrap();
    let from = cluster.state(template.partition).policies[&policy].status;
    record.event = Event::PolicyStatusChanged {
        policy: policy.clone(),
        from,
        to: PolicyStatus::Lapsed,
    };
    let err = audit(&cluster, &view_of(&events), &policies, true).unwrap_err();
    assert_eq!(err, format!("directory has the wrong status for {policy}"));
}
