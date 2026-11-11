//! The feed from the core and the daily control check.

mod common;

use common::*;
use ic_core::event::{Event, EventRecord, RoleTotal};
use ic_finance::books::{Anchor, FinEvent, JournalKind, FINANCE_PARTITION, MAX_INGEST_EVENTS};
use ic_finance::close::CheckStatus;
use ic_finance::{FinCommand, FinanceState};
use ic_ledger::{AccountClass, AccountUnit, BalanceFamily, Hash32};
use ic_money::Currency::NZD;

/// Ten days of one partition with one paying policy, not yet fed to any books.
fn ten_days() -> Vec<EventRecord> {
    let mut w = World::new(1, d(2027, 11, 15));
    let (p, id) = w.issue(1, d(2027, 11, 15));
    w.pay(p, &id, "r1", MONTHLY);
    for _ in 0..10 {
        w.core.end_of_day().unwrap();
    }
    w.core.take_events()
}

fn ingest(events: Vec<EventRecord>) -> FinCommand {
    FinCommand::Ingest { partition: 0, events }
}

/// Feed tampered events to fresh books and return the refusal's code. The
/// books must be exactly as they were before.
fn refused(events: Vec<EventRecord>) -> String {
    let mut b = Books::ready(1);
    let before = b.state.state_hash();
    let applied = b.send_keyed("t", "feed", None, ingest(events));
    assert!(applied.events.is_empty() && applied.entries.is_empty());
    assert_eq!(b.state.state_hash(), before, "a refused batch left something behind");
    applied.result.unwrap_err().code
}

fn position(events: &[EventRecord], nth: usize, pick: impl Fn(&Event) -> bool) -> usize {
    events.iter().enumerate().filter(|(_, r)| pick(&r.event)).nth(nth).unwrap().0
}

fn is_entry(e: &Event) -> bool {
    matches!(e, Event::EntryPosted { .. })
}

fn is_close(e: &Event) -> bool {
    matches!(e, Event::BusinessDateAdvanced { from: Some(_), .. })
}

fn is_totals(e: &Event) -> bool {
    matches!(e, Event::ControlTotals { .. })
}

/// Purpose: twenty business days of a two-partition core, fed day by day, leave the finance
/// ledger equal to the core on every partition day, each day tied to the Merkle root the core
/// published, with a balanced trial balance and one summarised journal per partition day
/// that names the core entries it carries.
/// Why it matters: this is the daily control check the whole close rests on. If the books
/// could drift from the core without a day turning red, month end would be the first time
/// anyone found out, and the lineage from a ledger figure to the core entries behind it
/// would be a guess.
#[test]
fn the_books_equal_the_core_every_day_and_tie_to_the_merkle_root() {
    let mut w = World::new(2, d(2027, 11, 15));
    let (p, id) = w.issue(1, d(2027, 11, 15));
    w.pay(p, &id, "r1", MONTHLY);
    w.days(20);
    let state = &w.books.state;

    let controlled: Vec<_> = w
        .books
        .applied
        .iter()
        .flat_map(|a| &a.events)
        .filter(|e| matches!(e.event, FinEvent::DayControlled { .. }))
        .collect();
    assert_eq!(controlled.len(), 40);
    assert!(controlled.iter().all(|e| matches!(e.event, FinEvent::DayControlled { equal: true, .. })));
    let seqs: Vec<u64> = w.books.applied.iter().flat_map(|a| &a.events).map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=state.last_event_seq()).collect::<Vec<_>>(), "finance events are gapless");

    for date in d(2027, 11, 15).iter_days().take(20) {
        assert_eq!(state.anchor(date), Anchor::Anchored, "{date}");
    }
    let control = state.dashboard(period(2027, 11), d(2027, 12, 5)).control;
    assert_eq!((control.expected, control.checked, control.different), (32, 32, 0));
    assert!(control.unanchored.is_empty());

    // The books balance, and the core's earned premium is the ledger's.
    let rows = state.trial_balance(period(2027, 12), None);
    assert_eq!(rows.iter().map(|r| r.balance).sum::<i128>(), 0);
    let earned: i128 = rows.iter().filter(|r| r.account == "4000").map(|r| r.balance).sum();
    // The core has also earned the day just closed. That batch posts on the open business
    // date and reaches the books when that date closes.
    let ahead = earned - w.core.state(p).ledger.role_total("earned_premium");
    assert!((206_133_333..=206_133_334).contains(&ahead), "{ahead}");
    let journal: Vec<_> = w.books.applied.iter().flat_map(|a| a.entries.clone()).collect();
    assert_eq!(ic_ledger::verify_chain(FINANCE_PARTITION, &journal), Ok(state.ledger.head()));

    // Each day's journal says which core entries it summarises.
    let sources = state.sources(period(2027, 11));
    assert_eq!(sources.len(), 32);
    let day = sources.iter().find(|s| s.partition == p && s.date == d(2027, 11, 15)).unwrap();
    assert!(day.equal && day.anchor == Anchor::Anchored);
    assert_eq!(day.first_seq, Some(1));
    assert_eq!(day.journals.len(), 1);
    let quiet = sources.iter().find(|s| s.partition != p).unwrap();
    assert_eq!(
        (quiet.first_seq, quiet.last_seq, quiet.journals.len()),
        (None, None, 0),
        "a day with no entries posts no journal"
    );
    assert_eq!(state.day(d(2027, 11, 15), p).unwrap().closing_head, day.closing_head);
    assert_eq!(state.next_event_seq(7), 1, "a partition never heard from starts at its first event");
}

/// Purpose: a batch that skips an event, carries another partition's event, names a partition
/// that does not exist, is too large, or comes from anything but the feed is refused whole,
/// while a batch that repeats events already taken changes nothing.
/// Why it matters: the feed is at-least-once and the books are built by summing. A skipped
/// event would understate the ledger silently, and a repeated one applied twice would double
/// it. Either would surface only as a control difference with no clue to its cause.
#[test]
fn a_stream_with_a_gap_or_from_the_wrong_partition_is_refused_whole() {
    let events = ten_days();

    let mut skipped = events.clone();
    skipped.remove(position(&events, 2, is_entry));
    assert_eq!(refused(skipped), "gap");

    let mut foreign = events.clone();
    foreign[3].partition = 1;
    assert_eq!(refused(foreign), "partition");

    let mut b = Books::ready(1);
    let elsewhere = FinCommand::Ingest { partition: 1, events: vec![] };
    assert_eq!(b.code("feed", None, elsewhere), "partition");
    assert_eq!(b.code("feed", None, ingest(vec![events[0].clone(); MAX_INGEST_EVENTS + 1])), "batch");
    assert_eq!(b.code("finance-console", Some("olive"), ingest(events.clone())), "caller");

    let mut unconfigured = Books::empty();
    assert_eq!(unconfigured.code("feed", None, ingest(events.clone())), "chart");

    // The same events again, whole or overlapping, are skipped.
    b.feed(&events).unwrap();
    let after = b.state.state_hash();
    b.feed(&events).unwrap();
    b.feed(&events[events.len() - 5..]).unwrap();
    assert_eq!(b.state.state_hash(), after);
    assert_eq!(b.state.next_event_seq(0), events.last().unwrap().seq + 1);
}

/// Purpose: the feed refuses a day whose entries do not chain to the head the core closed
/// on: a close at another head, an entry out of sequence or on the wrong date, an entry
/// published without its lines, control totals taken at another head, and a close that is
/// not followed by its control totals.
/// Why it matters: the control check is only worth something if it is taken at the exact
/// point of the chain the day's signed Merkle root covers. Each of these is a way the books
/// could be compared at some other point and still look equal.
#[test]
fn entries_that_do_not_chain_to_the_close_are_refused() {
    let events = ten_days();
    let other = Hash32([9; 32]);

    let mut wrong_head = events.clone();
    let at = position(&events, 1, is_close);
    if let Event::BusinessDateAdvanced { closing_head, .. } = &mut wrong_head[at].event {
        *closing_head = other;
    }
    assert_eq!(refused(wrong_head), "head_mismatch");

    let at = position(&events, 1, is_entry);
    let mut out_of_order = events.clone();
    if let Event::EntryPosted { entry, .. } = &mut out_of_order[at].event {
        entry.seq += 1;
    }
    assert_eq!(refused(out_of_order), "entry_gap");

    let mut wrong_day = events.clone();
    wrong_day[at].business_date = d(2027, 12, 25);
    assert_eq!(refused(wrong_day), "date_mismatch");

    let mut bare = events.clone();
    if let Event::EntryPosted { lines, .. } = &mut bare[at].event {
        lines.clear();
    }
    assert_eq!(refused(bare), "no_lines");

    let at = position(&events, 0, is_totals);
    let mut wrong_totals = events.clone();
    if let Event::ControlTotals { closing_head, .. } = &mut wrong_totals[at].event {
        *closing_head = other;
    }
    assert_eq!(refused(wrong_totals), "control_mismatch");

    let mut no_totals = events.clone();
    no_totals.remove(at);
    for (i, record) in no_totals.iter_mut().enumerate() {
        record.seq = i as u64 + 1;
    }
    assert_eq!(refused(no_totals), "control_missing");
}

/// Purpose: an entry on a role the chart maps nowhere, on a role whose class differs between
/// the core and the chart, or whose lines do not balance, stops the feed with the books
/// untouched.
/// Why it matters: a new account role in the core must be placed in the chart by finance
/// before money on it reaches the ledger. Posting it to a default account, or dropping it,
/// would produce a ledger that balances and is wrong.
#[test]
fn a_role_the_chart_cannot_place_stops_the_feed() {
    let events = ten_days();
    let at = position(&events, 0, is_entry);

    let mut unknown = events.clone();
    if let Event::EntryPosted { lines, .. } = &mut unknown[at].event {
        lines[0].role = "mystery".to_string();
    }
    assert_eq!(refused(unknown), "unmapped_role");

    let mut reclassed = events.clone();
    if let Event::EntryPosted { lines, .. } = &mut reclassed[at].event {
        lines[0].class = AccountClass::Expense;
    }
    assert_eq!(refused(reclassed), "class_mismatch");

    let mut lopsided = events.clone();
    if let Event::EntryPosted { lines, .. } = &mut lopsided[at].event {
        lines[0].signed += 1;
    }
    assert_eq!(refused(lopsided), "feed_unbalanced");
}

/// Purpose: when the core's own balance for a role differs from the books built from its
/// lines, or the core reports a role the books have never seen, the day is recorded as
/// different and the period's control check is red, naming the date, partition and role.
/// Why it matters: this is the one check that says the ledger finance reports from is the
/// ledger the core keeps. A difference must stop the close and tell the person looking
/// exactly where to start.
#[test]
fn a_core_total_that_differs_turns_the_control_check_red() {
    let mut events = ten_days();
    let at = position(&events, 3, is_totals);
    if let Event::ControlTotals { totals, .. } = &mut events[at].event {
        totals[0].balance += 500;
        totals.push(RoleTotal {
            role: "phantom".to_string(),
            class: AccountClass::Asset,
            unit: AccountUnit::Minor(NZD),
            balance: 1,
        });
    }
    let mut b = Books::ready(1);
    b.feed(&events).unwrap();
    let unequal: Vec<_> = b
        .applied
        .iter()
        .flat_map(|a| &a.events)
        .filter_map(|e| match e.event {
            FinEvent::DayControlled { date, equal: false, .. } => Some(date),
            _ => None,
        })
        .collect();
    assert_eq!(unequal, vec![d(2027, 11, 18)]);
    let differences = &b.state.day(d(2027, 11, 18), 0).unwrap().differences;
    assert_eq!(differences.len(), 2);
    assert!(differences.iter().any(|x| x.role == "phantom" && (x.core, x.finance) == (1, 0)));
    assert!(differences.iter().any(|x| x.core - x.finance == 500));

    let checks = b.state.checks(period(2027, 11));
    let CheckStatus::Red { items } = &checks.iter().find(|c| c.id == "control_check").unwrap().status else {
        panic!("the control check is not red")
    };
    assert!(items.iter().any(|i| i.starts_with("2027-11-18 partition 0: phantom")), "{items:?}");
    assert_eq!(b.state.dashboard(period(2027, 11), d(2027, 11, 26)).control.different, 1);
}

/// Purpose: a day with no Merkle root recorded, or with a root the checked chain heads do
/// not produce, is not anchored and keeps the control check red. Recording the same root
/// again is accepted and a different root for the same date is refused.
/// Why it matters: equality with the core proves nothing to an auditor unless it is tied to
/// the root that was signed and published that day. A root that could be replaced later
/// would let the anchor be moved to fit.
#[test]
fn a_missing_or_wrong_merkle_root_leaves_the_day_unanchored() {
    let mut w = World::new(1, d(2027, 11, 15));
    w.days(2);
    let state = &w.books.state;
    assert_eq!(state.anchor(d(2027, 11, 16)), Anchor::Anchored);
    assert_eq!(state.anchor(d(2027, 11, 17)), Anchor::NoRoot);

    // A third day arrives with the wrong root.
    let close = w.core.end_of_day().unwrap();
    let events = w.core.take_events();
    w.books.feed(&events).unwrap();
    let root = |root: Hash32| FinCommand::RecordMerkleRoot {
        date: close.closed,
        root,
        reference: "worm://roots/x".to_string(),
    };
    let applied = w.books.send_keyed("bad-root", "feed", None, root(Hash32([1; 32])));
    assert!(matches!(applied.events[0].event, FinEvent::MerkleRootRecorded { anchored: false, .. }));
    assert_eq!(w.books.state.anchor(close.closed), Anchor::Mismatch);
    assert_eq!(w.books.send("feed", None, root(Hash32([1; 32]))), Ok(()));
    assert_eq!(w.books.code("feed", None, root(close.merkle_root)), "root_conflict");

    w.days(1);
    let CheckStatus::Red { items } = &w.books.state.checks(period(2027, 11))[0].status else {
        panic!("the control check is not red")
    };
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0], "the core has not closed the days after 2027-11-18");
    assert!(items[1].contains("do not produce the published Merkle root"), "{items:?}");
    assert_eq!(
        w.books.state.dashboard(period(2027, 11), d(2027, 11, 20)).control.unanchored,
        vec![close.closed]
    );
}

/// Purpose: applying the same log to an empty ledger gives the same state hash, the state
/// survives being saved and loaded, and a command sent twice under one key returns its
/// first result without being applied again.
/// Why it matters: the finance ledger runs as a replicated state machine. Replicas that
/// disagree after the same log, a snapshot that does not restore exactly, or a retried
/// command that posts twice would each corrupt the books without any single step failing.
#[test]
fn replaying_the_log_rebuilds_the_same_books() {
    let mut w = World::new(2, d(2027, 11, 20));
    let (p, id) = w.issue(1, d(2027, 11, 20));
    w.pay(p, &id, "r1", MONTHLY);
    w.until(d(2027, 12, 3));
    let accrual = journal("j1", JournalKind::Accrual, period(2027, 11), "6500", "2500", 250_00);
    w.books.console("olive", FinCommand::ProposeJournal { journal: accrual }).unwrap();
    let first = w.books.send_keyed(
        "approve-j1",
        "finance-console",
        Some("ana"),
        FinCommand::ApproveJournal { id: "j1".to_string() },
    );
    assert_eq!((first.result.clone(), first.entries.len(), first.duplicate), (Ok(()), 1, false));
    let again = w.books.send_keyed(
        "approve-j1",
        "finance-console",
        Some("ana"),
        FinCommand::ApproveJournal { id: "j1".to_string() },
    );
    assert_eq!((again.result, again.index, again.duplicate), (Ok(()), first.index, true));
    assert!(again.events.is_empty() && again.entries.is_empty());
    // A refusal is remembered under its key too.
    let refused = w
        .books
        .send_keyed("bad", "finance-console", Some("ana"), FinCommand::ApproveJournal { id: "none".to_string() });
    let refused_again = w
        .books
        .send_keyed("bad", "finance-console", Some("ana"), FinCommand::ApproveJournal { id: "none".to_string() });
    assert_eq!(refused.result, refused_again.result);
    assert!(refused_again.duplicate);

    let mut replica = FinanceState::default();
    for (i, env) in w.books.log.iter().enumerate() {
        ic_log::StateMachine::apply(&mut replica, i as u64 + 1, env.clone());
    }
    assert_eq!(replica.state_hash(), w.books.state.state_hash());
    assert_eq!(replica, w.books.state);

    let saved = serde_json::to_vec(&w.books.state).unwrap();
    let loaded: FinanceState = serde_json::from_slice(&saved).unwrap();
    assert_eq!(loaded, w.books.state);
    assert_eq!(loaded.state_hash(), w.books.state.state_hash());
    assert!(matches!(loaded.trial_balance(period(2027, 11), None)[0].unit, BalanceFamily::Currency(NZD)));
}
