//! The loadings question finance asks: loaded layers, a page of policies at
//! a time.

use chrono::NaiveDate;
use ic_core::loadings::{page, MAX_PAGE};
use ic_core::{Command, PolicyId};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// Purpose: the loadings question returns every layer that carries a loading and no layer
/// that does not, in policy order, a page of policies at a time with a cursor that reaches
/// the end of the book exactly once; with a date it returns only layers in force on that
/// date; a page size of zero or beyond the limit is brought into range; and a policy that
/// has moved to another partition is left out.
/// Why it matters: loadings are kept off events because they imply a person's health, so
/// this is the only way finance can check loaded premium. A layer missed or returned twice
/// across pages would make that check wrong without anyone seeing why, and an unbounded
/// answer would put a whole book of health-related data in one reply.
#[test]
fn loadings_are_read_a_page_of_policies_at_a_time() {
    let start = d(2027, 11, 15);
    let mut c = Cluster::in_memory(1, 5);
    c.open(start).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    // Five policies: the second and fourth are loaded, the fourth on two covers.
    let mut ids: Vec<PolicyId> = Vec::new();
    for n in 1..=5u64 {
        let (trauma, loading) = match n {
            2 => (None, Some("0.50")),
            4 => (Some(100_000_00), Some("0.25")),
            _ => (Some(100_000_00), None),
        };
        let issue = term_life(n, start, d(1985, 6, 1), Sex::Male, false, 300_000_00, trauma, Frequency::Monthly, loading);
        ids.push(issue.policy.clone());
        c.submit(0, "new-business", issue_command(issue)).unwrap();
    }
    let policies = &c.state(0).policies;

    let all = page(policies, None, 100, None);
    let seen: Vec<(&str, &str, &str)> = all
        .rows
        .iter()
        .map(|r| (r.policy.0.as_str(), r.cover.as_str(), r.loadings[0].as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            (ids[1].0.as_str(), "life", "0.50"),
            (ids[3].0.as_str(), "life", "0.25"),
            (ids[3].0.as_str(), "trauma", "0.25")
        ]
    );
    assert_eq!(all.next, None);
    assert!(all.rows.iter().all(|r| r.start == start && r.end.is_none()));

    // Two policies a page: three pages, each row once, and the cursor ends.
    let mut rows = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let p = page(policies, after.as_ref(), 2, None);
        pages += 1;
        rows.extend(p.rows);
        match p.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!((pages, &rows), (3, &all.rows));
    assert_eq!(page(policies, Some(&ids[4]), 2, None).rows.len(), 0, "nothing lies after the last policy");

    // Sizes out of range are brought into it.
    assert_eq!(page(policies, None, 0, None).next, Some(ids[0].clone()));
    assert_eq!(page(policies, None, MAX_PAGE + 1, None), all);

    // As at a date: nothing was in force the day before the policies began.
    assert!(page(policies, None, 100, start.pred_opt()).rows.is_empty());
    assert_eq!(page(policies, None, 100, Some(start)), all);

    // A policy that has left for another partition is answered for there.
    let moved = Command::BeginTransfer {
        policy: ids[1].clone(),
        transfer_id: "t-1".into(),
        target_group: ic_core::GroupId::from("grp-elsewhere"),
        target_partition: 1,
    };
    c.submit(0, "transfer", moved).unwrap();
    let complete = Command::CompleteTransfer {
        policy: ids[1].clone(),
        transfer_id: "t-1".into(),
    };
    c.submit(0, "transfer", complete).unwrap();
    let left = page(&c.state(0).policies, None, 100, None);
    assert!(left.rows.iter().all(|r| r.policy != ids[1]));
    assert_eq!(left.rows.len(), 2);
}
