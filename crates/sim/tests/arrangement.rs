//! An arrangement to clear overdue premium by instalments.

use chrono::{Duration, NaiveDate};
use ic_core::arrangement::{ArrangementState, Instalment};
use ic_core::event::Event;
use ic_core::{Command, PolicyId, PolicyStatus, Reject, Standing};
use ic_money::{Currency::NZD, Money};
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

/// Close days until the business date is `start + n`.
fn until(c: &mut Cluster, n: i64) {
    while c.state(0).business_date.unwrap() < start() + Duration::days(n) {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

fn policy(c: &mut Cluster, n: u64) -> PolicyId {
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

fn pay(c: &mut Cluster, policy: &PolicyId, receipt: &str, minor: i64) {
    let cmd = Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: receipt.into(),
        amount: Money::new(minor, NZD),
    };
    c.submit(0, "cash-matching", cmd).unwrap();
}

/// What the policyholder owes on premium due before today.
fn overdue(c: &Cluster, policy: &PolicyId) -> i64 {
    let today = c.state(0).business_date.unwrap();
    c.state(0).policies[policy]
        .receivables
        .iter()
        .filter(|r| r.due < today)
        .map(|r| r.outstanding_minor)
        .sum()
}

fn make(c: &mut Cluster, policy: &PolicyId, id: &str, instalments: &[(i64, i64)]) -> Result<(), Reject> {
    let command = Command::MakeArrangement {
        policy: policy.clone(),
        arrangement: id.to_string(),
        instalments: instalments
            .iter()
            .map(|(day, amount_minor)| Instalment {
                due: start() + Duration::days(*day),
                amount_minor: *amount_minor,
            })
            .collect(),
    };
    c.submit(0, "collections", command)
}

fn state(c: &Cluster, policy: &PolicyId) -> (PolicyStatus, Option<Standing>, Option<ArrangementState>) {
    let p = &c.state(0).policies[policy];
    (p.status, p.standing, p.arrangement.as_ref().map(|a| a.state))
}

/// Purpose: an arrangement is recorded only for the premium that is overdue today, in
/// instalments that add up to it, on dates from today in order; while each instalment is
/// paid by its date the policy's standing is held where it stood and the policy does not
/// lapse, even past the day it otherwise would have; and when the overdue premium is
/// cleared the arrangement is complete and standing is derived in the ordinary way again.
/// Why it matters: an arrangement is a promise the insurer makes to a customer in
/// difficulty. A policy that lapsed while the customer was keeping their side of it would
/// take away cover the insurer had agreed to continue, at the moment a claim is likeliest.
#[test]
fn an_arrangement_that_is_kept_holds_standing_and_stops_the_lapse() {
    let mut c = cluster();
    let kept = policy(&mut c, 1);
    let paid = policy(&mut c, 2);
    pay(&mut c, &paid, "r-paid", 10_000_00);
    until(&mut c, 45);
    assert_eq!(state(&c, &kept), (PolicyStatus::InForce, Some(Standing::Suspended), None));
    let arrears = overdue(&c, &kept);
    assert!(arrears > 0);
    let half = arrears / 2;

    assert_eq!(code(make(&mut c, &paid, "arr-1", &[(50, 1_00)])), "not_in_arrears");
    assert_eq!(code(make(&mut c, &kept, "arr 1", &[(50, arrears)])), "invalid_arrangement");
    assert_eq!(code(make(&mut c, &kept, "arr-1", &[])), "invalid_arrangement");
    assert_eq!(code(make(&mut c, &kept, "arr-1", &[(50, arrears - 1)])), "invalid_arrangement");
    assert_eq!(code(make(&mut c, &kept, "arr-1", &[(44, half), (50, arrears - half)])), "invalid_arrangement");
    assert_eq!(code(make(&mut c, &kept, "arr-1", &[(60, half), (50, arrears - half)])), "invalid_arrangement");
    assert_eq!(code(make(&mut c, &kept, "arr-1", &[(50, 0), (60, arrears)])), "invalid_arrangement");
    let by_servicing = Command::MakeArrangement {
        policy: kept.clone(),
        arrangement: "arr-1".into(),
        instalments: vec![],
    };
    assert_eq!(c.submit(0, "policy-servicing", by_servicing).unwrap_err().code, "caller_not_allowed");

    let plan = [(55, half), (85, arrears - half)];
    c.take_events();
    make(&mut c, &kept, "arr-1", &plan).unwrap();
    make(&mut c, &kept, "arr-1", &plan).unwrap();
    let made: Vec<_> = c
        .take_events()
        .into_iter()
        .filter(|e| matches!(&e.event, Event::ArrangementMade { arrears_minor, .. } if *arrears_minor == arrears))
        .collect();
    assert_eq!(made.len(), 1);
    assert_eq!(code(make(&mut c, &kept, "arr-1", &[(55, arrears)])), "duplicate_arrangement");
    assert_eq!(code(make(&mut c, &kept, "arr-2", &plan)), "arrangement_exists");

    // The first instalment with a little more, then the rest.
    until(&mut c, 50);
    pay(&mut c, &kept, "r-1", half + 10_00);
    until(&mut c, 80);
    assert_eq!(
        state(&c, &kept),
        (PolicyStatus::InForce, Some(Standing::Suspended), Some(ArrangementState::Active))
    );
    let still = overdue(&c, &kept).min(arrears - half);
    c.take_events();
    pay(&mut c, &kept, "r-2", still);
    let completed = c.take_events().into_iter().any(|e| matches!(e.event, Event::ArrangementCompleted { .. }));
    assert!(completed);
    // Standing is derived in the ordinary way again, from the premium billed since.
    assert_ne!(state(&c, &kept).1, Some(Standing::Suspended));
    // Past the day it would have lapsed with nothing paid.
    until(&mut c, 95);
    let (status, _, arrangement) = state(&c, &kept);
    assert_eq!(status, PolicyStatus::InForce);
    assert!(matches!(arrangement, Some(ArrangementState::Completed { .. })));
    assert!(c.state(0).policies[&kept].arrears_pause.is_none());
}

/// Purpose: when an instalment is missed the arrangement is broken on the day after it
/// fell due, and the lapse clock carries on from where it stood when the arrangement was
/// made: the policy lapses later than it would have by exactly the days the arrangement
/// ran, and no later. Collections can also end an arrangement, which breaks it that day,
/// and a new arrangement can then be made.
/// Why it matters: the days a customer spent under an agreed arrangement are days the
/// insurer told them they were safe, so counting them towards lapse would lapse the policy
/// the morning after a missed instalment. Never restarting the clock would instead let a
/// string of broken arrangements keep an unpaid policy in force without end.
#[test]
fn a_broken_arrangement_resumes_the_lapse_clock_from_where_it_stood() {
    let mut c = cluster();
    let broken = policy(&mut c, 1);
    let ended = policy(&mut c, 2);
    until(&mut c, 45);
    let arrears = overdue(&c, &broken);
    make(&mut c, &broken, "arr-1", &[(55, arrears)]).unwrap();
    make(&mut c, &ended, "arr-1", &[(55, arrears)]).unwrap();

    until(&mut c, 55);
    assert_eq!(state(&c, &broken).2, Some(ArrangementState::Active));
    c.take_events();
    until(&mut c, 56);
    assert_eq!(
        state(&c, &broken),
        (
            PolicyStatus::InForce,
            Some(Standing::Suspended),
            Some(ArrangementState::Broken {
                on: start() + Duration::days(56)
            })
        )
    );
    let told = c
        .take_events()
        .into_iter()
        .filter(|e| matches!(&e.event, Event::ArrangementBroken { outstanding_minor, .. } if *outstanding_minor == arrears))
        .count();
    assert_eq!(told, 2, "both policies missed the same instalment");
    assert_eq!(c.state(0).policies[&broken].arrears_pause.map(|p| p.days), Some(11));

    // Collections ends a second arrangement on the day it is made: no days are added.
    let again = overdue(&c, &ended);
    make(&mut c, &ended, "arr-2", &[(70, again)]).unwrap();
    let end = |id: &str| Command::EndArrangement {
        policy: ended.clone(),
        arrangement: id.to_string(),
    };
    assert_eq!(code(c.submit(0, "collections", end("arr-9"))), "unknown_arrangement");
    c.submit(0, "collections", end("arr-2")).unwrap();
    c.submit(0, "collections", end("arr-2")).unwrap();
    assert_eq!(c.state(0).policies[&ended].arrears_pause.map(|p| p.days), Some(11));

    // Without the arrangement both would have lapsed on day 91. With it, 11 days later.
    until(&mut c, 101);
    assert_eq!(state(&c, &broken).0, PolicyStatus::InForce);
    assert_eq!(state(&c, &ended).0, PolicyStatus::InForce);
    until(&mut c, 102);
    assert_eq!(state(&c, &broken).0, PolicyStatus::Lapsed);
    assert_eq!(state(&c, &ended).0, PolicyStatus::Lapsed);
}
