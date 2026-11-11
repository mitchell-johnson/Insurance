//! A death notified late, as the finance ledger sees it.

mod common;

use common::*;
use ic_core::Command;
use ic_finance::books::FinEvent;
use ic_finance::close::PeriodStatus;

/// Purpose: a death on 31 October 2028 is notified on 16 December, after October and
/// November have closed. The reversals reach the finance ledger as December entries with
/// their original value dates. They are reported in December as prior-period items under
/// October and November, apart from December's own activity; each reversal can be walked
/// back to the entry it reverses; the closed months keep the trial balances they closed
/// with; and December still closes normally.
/// Why it matters: this is the finance side of the design addendum's first decision. The
/// unwind must not reopen a reported month, must not be mistaken for December trading, and
/// must leave an auditor a path from the restated figure to each original posting.
#[test]
fn a_late_notified_death_is_a_prior_period_movement_and_closed_months_stay_frozen() {
    let mut w = World::new(1, d(2027, 11, 15));
    let (p, id) = w.issue(1, d(2027, 11, 15));
    w.pay(p, &id, "ahead", 14 * MONTHLY + 50_00);
    let mut month = period(2027, 11);
    while month < period(2028, 12) {
        w.until(month.end() + chrono::Days::new(3));
        w.close(month);
        month = month.next_month();
    }
    w.until(d(2028, 12, 16));
    let (oct, nov, dec) = (period(2028, 10), period(2028, 11), period(2028, 12));
    let frozen = [oct, nov].map(|m| w.books.state.periods[&m].closed.clone().unwrap());
    assert!(
        w.books.state.backdated.is_empty(),
        "thirteen months of routine business produced no prior-period item"
    );
    let earned_before: i128 = w.books.state.movements(dec).iter().filter(|r| r.account == "4000").map(|r| r.closing).sum();

    let terminate = Command::TerminateOnClaim {
        policy: id.clone(),
        event_date: d(2028, 10, 31),
        claim: "clm-1".to_string(),
        payer_deceased: true,
    };
    w.core.submit(p, "claims", terminate).unwrap();
    w.days(1);

    let state = &w.books.state;
    // The closed months are exactly as they were reported.
    for (m, was) in [oct, nov].into_iter().zip(&frozen) {
        assert_eq!(state.periods[&m].closed.as_ref(), Some(was));
        assert_eq!(state.trial_balance(m, None), was.trial_balance);
        assert_eq!(state.periods[&m].status, PeriodStatus::Closed);
    }

    // Every backdated entry is listed, and a reversal names what it reverses.
    let items = state.backdated_in(dec, None);
    assert_eq!(items.len(), 4);
    assert!(items
        .iter()
        .all(|i| i.business_date == d(2028, 12, 16) && i.policy.as_deref() == Some(id.0.as_str())));
    let november_bill = state.backdated_in(dec, Some(nov));
    assert_eq!(november_bill.len(), 1);
    assert_eq!(
        (november_bill[0].kind.as_str(), november_bill[0].value_date),
        ("premium_due_unwound", d(2028, 11, 15))
    );
    let reversed = november_bill[0].reverses.clone().unwrap();
    assert_eq!(state.reversals_of(&reversed), november_bill);
    // The December bill is reversed too, but that is December's own business.
    let own: Vec<_> = items.iter().filter(|i| i.origin.is_none()).collect();
    assert_eq!((own.len(), own[0].value_date), (1, d(2028, 12, 15)));
    assert_eq!(state.backdated_in(dec, Some(oct)).len(), 2);
    let received = w
        .books
        .applied
        .iter()
        .flat_map(|a| &a.events)
        .filter(|e| matches!(e.event, FinEvent::BackdatedEntryReceived { .. }))
        .count();
    assert_eq!(received, 4);

    // Premium earned after the death comes back out of income as prior-period movement.
    let rows = state.movements(dec);
    let income = rows.iter().find(|r| r.account == "4000").unwrap();
    assert_eq!(income.prior_period.keys().copied().collect::<Vec<_>>(), vec![oct, nov]);
    assert!(income.prior_period.values().all(|v| *v > 0), "income is reduced by a debit");
    assert_eq!(income.closing, income.opening + income.activity + income.prior_period.values().sum::<i128>());
    assert!(income.closing > earned_before);
    assert!(rows.iter().all(|r| r.closing == r.opening + r.activity + r.prior_period.values().sum::<i128>()));
    let summary = state.dashboard(dec, d(2028, 12, 17)).prior_period;
    assert_eq!(
        summary.iter().map(|s| (s.origin, s.entries, s.reversals)).collect::<Vec<_>>(),
        vec![(oct, 2, 0), (nov, 1, 1)]
    );
    assert!(summary.iter().all(|s| s.result_effect["NZD"] < 0), "each restated month's result goes down");
    let day = state.sources(dec).into_iter().find(|s| s.date == d(2028, 12, 16)).unwrap();
    assert_eq!(day.journals.len(), 3, "one journal for the day's own lines and one for each earlier period");
    assert!(day.equal);

    // December closes in the ordinary way with the items in it.
    w.until(d(2029, 1, 4));
    w.close(dec);
    assert_eq!(w.books.state.periods[&dec].status, PeriodStatus::Closed);
}
