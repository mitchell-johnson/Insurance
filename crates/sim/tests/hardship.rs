//! A hardship override: a premium holiday or a reduced premium for a dated period.

use chrono::{Duration, NaiveDate};
use ic_core::event::Event;
use ic_core::{Command, PolicyId, Reject, Standing};
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

/// Close days until the business date is `date`.
fn until(c: &mut Cluster, date: NaiveDate) {
    while c.state(0).business_date.unwrap() < date {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

/// A term life policy with plenty paid ahead, and its monthly premium.
fn policy(c: &mut Cluster, n: u64) -> (PolicyId, i64) {
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.take_events();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let premium = billed(c, start()).expect("the first period is billed at issue");
    let receipt = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: format!("r-{n}"),
        amount: Money::new(10_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    (id, premium)
}

/// What was billed for the period starting on `period`, from the events since last asked.
fn billed(c: &mut Cluster, period: NaiveDate) -> Option<i64> {
    c.take_events().into_iter().find_map(|e| match e.event {
        Event::PremiumDue { period_start, total_minor, .. } if period_start == period => Some(total_minor),
        _ => None,
    })
}

fn grant(c: &mut Cluster, policy: &PolicyId, id: &str, from: NaiveDate, to: NaiveDate, share_bps: u32) -> Result<(), Reject> {
    let command = Command::GrantHardship {
        policy: policy.clone(),
        hardship: id.to_string(),
        from,
        to,
        share_bps,
    };
    c.submit(0, "collections", command)
}

fn end(c: &mut Cluster, policy: &PolicyId, id: &str) -> Result<(), Reject> {
    let command = Command::EndHardship {
        policy: policy.clone(),
        hardship: id.to_string(),
    };
    c.submit(0, "collections", command)
}

fn near(actual: i64, expected: i64) -> bool {
    (actual - expected).abs() <= 2
}

/// Purpose: a reduced premium approved for six months takes effect on its date: the days
/// left of the period already billed are credited at the reduction, later periods are
/// billed at the reduced share, and when the override is ended early in its fourth month
/// the full premium is charged from that day only, for the rest of the period already
/// billed and for every period after. Nothing is charged for the days behind it.
/// Why it matters: a hardship override is relief the insurer has agreed to. Billing the
/// full premium during it would put a customer in difficulty into arrears the insurer
/// promised they would not have, and charging back the relief when their circumstances
/// improve would punish them for telling the insurer so.
#[test]
fn a_reduced_premium_is_billed_for_its_days_and_ends_early_without_charging_back() {
    let mut c = cluster();
    let (id, premium) = policy(&mut c, 1);
    let from = d(2027, 11, 25);
    let to = d(2028, 5, 25);
    until(&mut c, from);

    assert_eq!(code(grant(&mut c, &id, "h 1", from, to, 5_000)), "invalid_hardship");
    assert_eq!(code(grant(&mut c, &id, "h-1", from - Duration::days(1), to, 5_000)), "invalid_hardship");
    assert_eq!(code(grant(&mut c, &id, "h-1", from, from, 5_000)), "invalid_hardship");
    assert_eq!(code(grant(&mut c, &id, "h-1", from, from + Duration::days(800), 5_000)), "invalid_hardship");
    assert_eq!(code(grant(&mut c, &id, "h-1", from, to, 10_000)), "invalid_hardship");
    assert_eq!(code(end(&mut c, &id, "h-1")), "unknown_hardship");
    let by_servicing = Command::GrantHardship {
        policy: id.clone(),
        hardship: "h-1".into(),
        from,
        to,
        share_bps: 5_000,
    };
    assert_eq!(c.submit(0, "policy-servicing", by_servicing).unwrap_err().code, "caller_not_allowed");

    // Granted on the eleventh day of a thirty-day period: twenty days at half.
    c.take_events();
    grant(&mut c, &id, "h-1", from, to, 5_000).unwrap();
    grant(&mut c, &id, "h-1", from, to, 5_000).unwrap();
    let granted: Vec<i64> = c
        .take_events()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::HardshipGranted { adjusted_minor, .. } => Some(adjusted_minor),
            _ => None,
        })
        .collect();
    assert_eq!(granted.len(), 1);
    assert!(near(granted[0], -premium / 3), "credit {} for a premium of {premium}", granted[0]);
    assert_eq!(code(grant(&mut c, &id, "h-1", from, to, 2_500)), "duplicate_hardship");
    assert_eq!(code(grant(&mut c, &id, "h-2", from, to, 5_000)), "hardship_exists");

    // Whole periods inside the override are billed at half.
    until(&mut c, d(2027, 12, 15));
    assert!(near(billed(&mut c, d(2027, 12, 15)).unwrap(), premium / 2));
    until(&mut c, d(2028, 2, 15));
    assert!(near(billed(&mut c, d(2028, 2, 15)).unwrap(), premium / 2));

    // Ended in its fourth month, on the ninth day of a 29-day period.
    let early = d(2028, 2, 23);
    until(&mut c, early);
    c.take_events();
    end(&mut c, &id, "h-1").unwrap();
    let ended: Vec<(NaiveDate, NaiveDate, i64)> = c
        .take_events()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::HardshipEnded {
                on,
                planned_to,
                adjusted_minor,
                ..
            } => Some((on, planned_to, adjusted_minor)),
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1);
    assert_eq!((ended[0].0, ended[0].1), (early, to));
    // Twenty-one days back at the full premium: half the premium for 21 of 29 days.
    assert!(near(ended[0].2, premium * 21 / 58), "charge {} for a premium of {premium}", ended[0].2);
    end(&mut c, &id, "h-1").unwrap();
    assert!(c.take_events().into_iter().all(|e| !matches!(e.event, Event::HardshipEnded { .. })));

    // The full premium from the next period, and the policy was never behind.
    until(&mut c, d(2028, 3, 15));
    assert_eq!(billed(&mut c, d(2028, 3, 15)), Some(premium));
    assert_eq!(c.state(0).policies[&id].standing, Some(Standing::Paid));
    // A new override can be granted once the last has ended.
    assert_eq!(code(grant(&mut c, &id, "h-2", d(2028, 4, 15), d(2028, 5, 15), 0)), "accepted");
}

/// Purpose: a premium holiday bills nothing for the periods it covers, so the policy stays
/// in good standing with nothing paid, and an override ended before its first day never
/// applies at all.
/// Why it matters: a holiday that still raised a bill would lapse the policy it was meant
/// to protect, and an override withdrawn before it started must leave no trace in what the
/// customer is asked to pay.
#[test]
fn a_premium_holiday_bills_nothing_and_one_ended_before_it_starts_never_applies() {
    let mut c = cluster();
    let issue = term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let holiday = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let first = billed(&mut c, start()).unwrap();
    let receipt = Command::ApplyReceipt {
        policy: holiday.clone(),
        receipt: "r-first".into(),
        amount: Money::new(first, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    let (withdrawn, premium) = policy(&mut c, 2);

    let from = d(2027, 12, 15);
    let to = d(2028, 2, 15);
    grant(&mut c, &holiday, "h-1", from, to, 0).unwrap();
    grant(&mut c, &withdrawn, "h-1", from, to, 0).unwrap();
    until(&mut c, d(2027, 11, 20));
    c.take_events();
    end(&mut c, &withdrawn, "h-1").unwrap();
    let ended = c.take_events().into_iter().find_map(|e| match e.event {
        Event::HardshipEnded { on, adjusted_minor, .. } => Some((on, adjusted_minor)),
        _ => None,
    });
    assert_eq!(ended, Some((from, 0)));

    until(&mut c, d(2028, 2, 14));
    let events = c.take_events();
    let bills = |policy: &PolicyId| -> Vec<i64> {
        events
            .iter()
            .filter_map(|e| match &e.event {
                Event::PremiumDue { policy: p, total_minor, .. } if p == policy => Some(*total_minor),
                _ => None,
            })
            .collect()
    };
    // Two periods fall in the holiday: nothing billed on one policy, both in full on the other.
    assert_eq!(bills(&holiday), Vec::<i64>::new());
    assert_eq!(bills(&withdrawn), vec![premium, premium]);
    assert_eq!(c.state(0).policies[&holiday].standing, Some(Standing::Paid));
    until(&mut c, to);
    assert_eq!(billed(&mut c, to), Some(first));
}
