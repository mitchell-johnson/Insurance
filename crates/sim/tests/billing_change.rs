//! Changing the billing frequency and the billing day after issue.

use chrono::NaiveDate;
use ic_core::billing_change::ChangeBilling;
use ic_core::event::Event;
use ic_core::{Command, PolicyId, Reject};
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, health_family, issue_command, member, term_life};
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

fn until(c: &mut Cluster, date: NaiveDate) {
    while c.state(0).business_date.unwrap() < date {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

/// A term life policy with plenty paid ahead.
fn policy(c: &mut Cluster, n: u64, frequency: Frequency) -> PolicyId {
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, frequency, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let receipt = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: format!("r-{n}"),
        amount: Money::new(20_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    id
}

fn change(c: &mut Cluster, policy: &PolicyId, reference: &str, frequency: Option<Frequency>, billing_day: Option<u32>) -> Result<(), Reject> {
    let spec = ChangeBilling {
        policy: policy.clone(),
        reference: reference.into(),
        frequency,
        billing_day,
    };
    c.submit(0, "policy-servicing", Command::ChangeBilling(Box::new(spec)))
}

/// Each bill raised for the policy since last asked: the period and the total.
fn bills(c: &mut Cluster, id: &PolicyId) -> Vec<(NaiveDate, NaiveDate, i64)> {
    c.take_events()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::PremiumDue {
                policy,
                period_start,
                period_end,
                total_minor,
                ..
            } if policy == *id => Some((period_start, period_end, total_minor)),
            _ => None,
        })
        .collect()
}

/// Purpose: matrix BIL-004. On a product that changes frequency at the anniversary, a monthly
/// policy asked to pay yearly goes on being billed monthly on its usual day until the
/// anniversary, is then billed once for the whole year, and is not billed again the month
/// after; the request is announced with the day it takes effect, only policy servicing may
/// send it, and asking for the present frequency before the day takes the change back.
/// Why it matters: a change of frequency moves when a year's premium is asked for. Applied at
/// once it would bill a year on top of a month already paid; lost at the anniversary it would
/// leave the customer on a frequency they asked to leave, paying the loading for it.
#[test]
fn a_change_of_frequency_waits_for_the_anniversary() {
    let mut c = cluster();
    let id = policy(&mut c, 1, Frequency::Monthly);
    let kept = policy(&mut c, 2, Frequency::Monthly);
    until(&mut c, d(2028, 1, 20));
    let monthly = bills(&mut c, &id).last().unwrap().2;

    let spec = ChangeBilling {
        policy: id.clone(),
        reference: "bill-1".into(),
        frequency: Some(Frequency::Yearly),
        billing_day: None,
    };
    assert_eq!(code(c.submit(0, "payments", Command::ChangeBilling(Box::new(spec)))), "caller_not_allowed");
    change(&mut c, &id, "bill-1", Some(Frequency::Yearly), None).unwrap();
    let announced = c.take_events().into_iter().find_map(|e| match e.event {
        Event::BillingChanged {
            frequency,
            first_day,
            first_full_period,
            ..
        } => Some((frequency, first_day, first_full_period)),
        _ => None,
    });
    assert_eq!(announced, Some((Frequency::Yearly, d(2028, 11, 15), d(2028, 11, 15))));
    // The same request again is accepted and changes nothing.
    change(&mut c, &id, "bill-1", Some(Frequency::Yearly), None).unwrap();
    assert_eq!(c.state(0).policies[&id].billing_changes.len(), 1);
    assert_eq!(c.state(0).policies[&id].frequency, Frequency::Monthly, "monthly until the anniversary");

    // The other policy asks, thinks again, and stays monthly.
    change(&mut c, &kept, "bill-1", Some(Frequency::Yearly), None).unwrap();
    change(&mut c, &kept, "bill-2", Some(Frequency::Monthly), None).unwrap();
    assert!(c.state(0).policies[&kept].billing_changes.is_empty());
    assert_eq!(code(change(&mut c, &kept, "bill-3", Some(Frequency::Monthly), None)), "nothing_to_change");

    until(&mut c, d(2028, 11, 14));
    let before = bills(&mut c, &id);
    assert_eq!(before.len(), 9, "February to October, monthly: {before:?}");
    assert!(before
        .iter()
        .all(|(from, to, total)| from.checked_add_months(chrono::Months::new(1)) == Some(*to) && *total == monthly));

    until(&mut c, d(2029, 1, 20));
    let after = bills(&mut c, &id);
    assert_eq!(after.len(), 1, "one bill for the year: {after:?}");
    let (from, to, yearly) = after[0];
    assert_eq!((from, to), (d(2028, 11, 15), d(2029, 11, 15)));
    assert!(yearly > 10 * monthly, "a year's premium, {yearly}, against {monthly} a month");
    assert_eq!(c.state(0).policies[&id].frequency, Frequency::Yearly);
    let still_monthly = bills(&mut c, &kept);
    assert!(still_monthly.is_empty(), "already read above with the other policy's events");
    assert_eq!(c.state(0).policies[&kept].frequency, Frequency::Monthly);
    assert_eq!(c.state(0).policies[&kept].periods_billed, 15);
}

/// Purpose: matrix BIL-003. Moving the billing day from the 15th to the 1st leaves the period
/// already billed as it was, bills the days from its end to the first of the month as one short
/// period at that share of a month's premium, and bills whole months from the 1st afterwards.
/// Why it matters: customers move the due day to their pay day. Editing what was billed would
/// break the ledger's history, a gap would give days of cover nobody paid for, and a whole
/// month's bill for seventeen days would overcharge.
#[test]
fn a_new_billing_day_bills_the_days_between_as_a_short_period() {
    let mut c = cluster();
    let id = policy(&mut c, 1, Frequency::Monthly);
    let first = bills(&mut c, &id);
    let monthly = first[0].2;
    until(&mut c, d(2027, 12, 1));
    let billed_before = c.state(0).policies[&id].receivables.clone();

    change(&mut c, &id, "day-1", None, Some(1)).unwrap();
    assert_eq!(c.state(0).policies[&id].receivables, billed_before, "nothing already billed is edited");
    until(&mut c, d(2028, 3, 10));
    let after = bills(&mut c, &id);
    let periods: Vec<_> = after.iter().map(|(from, to, _)| (*from, *to)).collect();
    assert_eq!(
        periods,
        vec![
            (d(2027, 12, 15), d(2028, 1, 1)),
            (d(2028, 1, 1), d(2028, 2, 1)),
            (d(2028, 2, 1), d(2028, 3, 1)),
            (d(2028, 3, 1), d(2028, 4, 1)),
        ]
    );
    // Seventeen days of December's thirty-one, each line of the bill rounded on its own.
    let short = after[0].2;
    let expected = (monthly as i128 * 17 / 31) as i64;
    assert!((short - expected).abs() <= 3, "billed {short} for 17 days of a month at {monthly}");
    assert!(after[1..].iter().all(|(_, _, total)| *total == monthly));
    // Every day from inception is in exactly one period.
    let p = &c.state(0).policies[&id];
    assert_eq!((0..5).map(|n| p.period_start(n).unwrap()).collect::<Vec<_>>(), {
        let mut days = vec![start()];
        days.extend(periods.iter().map(|(from, _)| *from));
        days
    });
}

/// Purpose: each rule on a change of billing refuses the request that breaks it: a day past
/// the 28th, a day of the month on a fortnightly policy, a frequency the product does not
/// sell, a product that allows no change, a scheme, a policy no longer in force, and a
/// reference that is not a plain name.
/// Why it matters: billing is the one schedule every customer is on. A change the product did
/// not agree to, or a due day some months do not have, would surface months later as a bill
/// on the wrong day for the wrong amount.
#[test]
fn a_change_of_billing_the_product_does_not_allow_is_refused() {
    let mut c = cluster();
    let id = policy(&mut c, 1, Frequency::Monthly);
    assert_eq!(code(change(&mut c, &id, "x", None, Some(29))), "invalid_billing_day");
    assert_eq!(code(change(&mut c, &id, "x", Some(Frequency::Fortnightly), Some(5))), "invalid_billing_day");
    assert_eq!(code(change(&mut c, &id, "x", Some(Frequency::Quarterly), None)), "frequency_not_offered");
    assert_eq!(code(change(&mut c, &id, "not a name", Some(Frequency::Yearly), None)), "invalid_request");
    assert_eq!(code(change(&mut c, &id, "x", None, None)), "nothing_to_change");

    let health = health_family(2, start(), &[d(1988, 5, 1)], Frequency::Monthly);
    let health_id = health.policy.clone();
    c.submit(0, "new-business", issue_command(health)).unwrap();
    assert_eq!(code(change(&mut c, &health_id, "x", Some(Frequency::Yearly), None)), "change_not_offered");
    assert_eq!(code(change(&mut c, &health_id, "x", None, Some(1))), "change_not_offered");

    let scheme = group_scheme(3, start(), vec![member("m1", d(1980, 1, 1), "staff", 80_000_00)]);
    let scheme_id = scheme.policy.clone();
    c.submit(0, "group-admin", issue_command(scheme)).unwrap();
    assert_eq!(code(change(&mut c, &scheme_id, "x", None, Some(1))), "scheme_policy");

    let cancel = Command::CancelPolicy { policy: id.clone() };
    c.submit(0, "policy-servicing", cancel).unwrap();
    assert_eq!(code(change(&mut c, &id, "x", Some(Frequency::Yearly), None)), "not_in_force");
}

/// Purpose: on a product that changes frequency from the next period, a monthly policy asked
/// to pay fortnightly is billed fortnightly from the end of the month already billed, and a
/// second change to monthly on the 1st then starts from the end of the fortnight billed, with
/// the days to the 1st as a short period; the frequency the policy shows follows the period
/// last billed.
/// Why it matters: changes stack over a policy's life. Each must start exactly where the
/// billing before it stopped, or a day is billed twice or not at all, and the premium of a
/// period must be worked out at the frequency that period was on.
#[test]
fn changes_from_the_next_period_follow_one_another_without_a_gap() {
    let mut c = cluster();
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-flex\"")
        .replace("frequency_change = \"anniversary\"", "frequency_change = \"next_period\"");
    let product = ic_product::load_product("term_life_flex.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let mut issue = term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    issue.product_code = "term-life-flex".into();
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let receipt = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "r-1".into(),
        amount: Money::new(20_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    let monthly = bills(&mut c, &id)[0].2;

    until(&mut c, d(2027, 12, 1));
    change(&mut c, &id, "to-fortnightly", Some(Frequency::Fortnightly), None).unwrap();
    until(&mut c, d(2028, 1, 5));
    let fortnights = bills(&mut c, &id);
    assert_eq!(
        fortnights.iter().map(|(from, to, _)| (*from, *to)).collect::<Vec<_>>(),
        vec![(d(2027, 12, 15), d(2027, 12, 29)), (d(2027, 12, 29), d(2028, 1, 12))]
    );
    let fortnightly = fortnights[0].2;
    assert!(fortnightly < monthly && fortnightly > monthly / 3);
    assert_eq!(c.state(0).policies[&id].frequency, Frequency::Fortnightly);

    change(&mut c, &id, "to-monthly", Some(Frequency::Monthly), Some(1)).unwrap();
    until(&mut c, d(2028, 3, 5));
    let months = bills(&mut c, &id);
    assert_eq!(
        months.iter().map(|(from, to, _)| (*from, *to)).collect::<Vec<_>>(),
        vec![(d(2028, 1, 12), d(2028, 2, 1)), (d(2028, 2, 1), d(2028, 3, 1)), (d(2028, 3, 1), d(2028, 4, 1))]
    );
    // Twenty days of January's thirty-one, then whole months at the monthly premium.
    assert!((months[0].2 - (monthly as i128 * 20 / 31) as i64).abs() <= 3, "{months:?} at {monthly} a month");
    assert_eq!((months[1].2, months[2].2), (monthly, monthly));
    let p = &c.state(0).policies[&id];
    assert_eq!(p.frequency, Frequency::Monthly);
    assert_eq!(p.frequency_on(d(2027, 11, 20)), Frequency::Monthly);
    assert_eq!(p.frequency_on(d(2027, 12, 20)), Frequency::Fortnightly);
}
