//! What the core publishes for the finance ledger: every posted entry as
//! lines by role, and the partition's balances by role at each close.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::event::{Event, EventRecord};
use ic_core::Command;
use ic_ledger::{AccountClass, AccountUnit, BalanceFamily};
use ic_money::{Currency::NZD, Money, FINE_PER_MINOR};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// A month of one policy: issued, billed, paid, earned daily.
fn month() -> Vec<EventRecord> {
    let start = d(2027, 11, 15);
    let mut c = Cluster::in_memory(1, 7);
    c.open(start).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let issue = term_life(1, start, d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let policy = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    c.submit(
        0,
        "cash-matching",
        Command::ApplyReceipt {
            policy,
            receipt: "r1".to_string(),
            amount: Money::new(61_84, NZD),
        },
    )
    .unwrap();
    for _ in 0..30 {
        assert!(c.end_of_day().unwrap().breaks.is_empty());
    }
    c.take_events()
}

fn fine(unit: &AccountUnit, amount: i128) -> i128 {
    match unit {
        AccountUnit::Minor(_) => amount * FINE_PER_MINOR,
        _ => amount,
    }
}

/// Purpose: every posted entry is published with its value date and with lines that net to
/// zero in each currency, and summing the lines by role from the first entry reproduces the
/// control totals the partition publishes at every close.
/// Why it matters: the finance ledger is built from these lines alone and checked against
/// these totals daily. If a line were dropped or a total drifted from the postings, the
/// daily control check would either miss a real difference or fail on a day that is clean.
#[test]
fn posted_lines_rebuild_the_control_totals_of_every_close() {
    let events = month();
    let mut running: BTreeMap<(String, AccountClass, AccountUnit), i128> = BTreeMap::new();
    let (mut entries, mut closes) = (0, 0);
    let mut last_hash = None;
    for record in &events {
        match &record.event {
            Event::EntryPosted { lines, value_date, hash, .. } => {
                entries += 1;
                last_hash = Some(*hash);
                assert!(value_date.is_some_and(|v| v <= record.business_date));
                assert!(!lines.is_empty());
                let mut nets: BTreeMap<BalanceFamily, i128> = BTreeMap::new();
                for line in lines {
                    *nets.entry(line.unit.family()).or_insert(0) += fine(&line.unit, line.signed);
                    *running.entry((line.role.clone(), line.class, line.unit.clone())).or_insert(0) += line.signed;
                }
                assert!(nets.values().all(|net| *net == 0), "entry lines do not balance: {nets:?}");
            }
            Event::ControlTotals { date, closing_head, totals } => {
                closes += 1;
                assert_eq!(*date, record.business_date);
                assert_eq!(Some(*closing_head), last_hash, "the totals are taken at the head the day closed on");
                let published: BTreeMap<_, _> = totals.iter().map(|t| ((t.role.clone(), t.class, t.unit.clone()), t.balance)).collect();
                let rebuilt: BTreeMap<_, _> = running.iter().filter(|(_, v)| **v != 0).map(|(k, v)| (k.clone(), *v)).collect();
                assert_eq!(published, rebuilt, "control totals on {date}");
            }
            _ => {}
        }
    }
    assert_eq!(closes, 30);
    assert!(entries > 30, "a month of daily earning posts an entry a day");
    // The receivable was raised and paid in full, so it nets to nothing and is left out.
    let Event::ControlTotals { totals, .. } = &events.iter().rev().find(|r| matches!(r.event, Event::ControlTotals { .. })).unwrap().event else {
        unreachable!()
    };
    assert!(totals.iter().all(|t| t.role != "premium_receivable" && t.balance != 0));
    assert!(totals
        .iter()
        .any(|t| t.role == "earned_premium" && t.class == AccountClass::Income && t.balance < 0));
}

/// Purpose: the control totals event follows the business date event of the same close
/// directly, carrying the same date and closing head.
/// Why it matters: the finance ledger pairs the two to know which chain head its day's
/// total was checked at. That head is what the day's signed Merkle root covers, so a
/// mismatch would anchor the check to the wrong point in the chain.
#[test]
fn control_totals_follow_the_close_they_belong_to() {
    let events = month();
    let mut pairs = 0;
    for pair in events.windows(2) {
        if let Event::BusinessDateAdvanced {
            from: Some(from),
            closing_head,
            ..
        } = &pair[0].event
        {
            let Event::ControlTotals { date, closing_head: at, .. } = &pair[1].event else {
                panic!("a close was not followed by its control totals: {:?}", pair[1].event)
            };
            assert_eq!((date, at), (from, closing_head));
            assert_eq!(pair[1].seq, pair[0].seq + 1);
            pairs += 1;
        }
    }
    assert_eq!(pairs, 30);
}

/// Purpose: the event that announces a policy carries, for each life, the rating factors
/// it was priced on (date of birth, sex and smoker status), and an event written before
/// those fields existed still reads, with the factors absent.
/// Why it matters: the in-force, movement and experience extracts group by age, sex and
/// smoker status and are built from events alone. Without the factors on the event the
/// experience study cannot be produced; and if old events stopped reading, every consumer
/// replaying from the start would fail on the first policy.
#[test]
fn the_issue_event_carries_each_lifes_rating_factors() {
    let events = month();
    let issued = events.iter().find(|e| matches!(e.event, Event::PolicyIssued { .. })).unwrap();
    let Event::PolicyIssued { lives, .. } = &issued.event else { unreachable!() };
    assert_eq!(lives.len(), 1);
    assert_eq!(
        (lives[0].date_of_birth, lives[0].sex, lives[0].smoker),
        (Some(d(1990, 3, 10)), Some(Sex::Female), Some(false))
    );

    let mut older = serde_json::to_value(&lives[0]).unwrap();
    for field in ["date_of_birth", "sex", "smoker"] {
        assert!(older.as_object_mut().unwrap().remove(field).is_some(), "{field} is not on the event");
    }
    let read: ic_core::event::LifeInfo = serde_json::from_value(older).unwrap();
    assert_eq!(read, ic_core::event::LifeInfo::new(lives[0].life.clone(), lives[0].party.clone()));
}
