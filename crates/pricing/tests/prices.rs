//! Preparing, approving and activating prices: maker-checker, the checks,
//! the order of pricing dates and suspension.

mod support;

use std::collections::BTreeMap;

use ic_pricing::command::*;
use ic_pricing::state::{slot, PriceStatus};
use ic_pricing::strike::CheckKind;
use support::*;

/// Purpose: a price goes from valuation to prepared, is refused to its own
/// preparer, approved by a second person, published once per partition, and
/// becomes active only when every partition has applied it.
/// Why it matters: this is the design's whole path for a price; maker-checker
/// and "active on every partition or not at all" are what make a published
/// price trustworthy.
#[test]
fn price_runs_from_valuation_to_every_partition() {
    let mut h = Harness::new();
    h.receive("F1", valuation(day(2), "2000000", UNITS, "0"));
    h.core_units(None, UNITS / 2);
    let id = h.prepared_id(day(2));
    assert_eq!(h.state.prices[&id].status, PriceStatus::Prepared);
    assert_eq!(h.state.prices[&id].struck.mid.as_str(), "2.0000");
    let own = h.send(
        CALLER_CONSOLE,
        "maker",
        &[ROLE_APPROVER],
        Command::ApprovePrice {
            price: id.clone(),
            accepted: BTreeMap::new(),
        },
    );
    assert!(matches!(own, Output::Refused { ref code, .. } if code == "separation"), "{own:?}");
    assert_eq!(h.approve(&id, BTreeMap::new()), Output::Done);
    assert_eq!(h.state.prices[&id].status, PriceStatus::Activating);
    let outbox: Vec<_> = h.state.outbox(10).into_iter().cloned().collect();
    assert_eq!(outbox.len(), 2);
    assert_eq!((outbox[0].partition, outbox[1].partition), (0, 1));
    assert_eq!(outbox[0].version, 1);
    assert_eq!(outbox[0].supersedes, None);
    // One partition applied: still activating, nothing published.
    h.ok(
        CALLER_OUTBOX,
        "host",
        &[],
        Command::RecordDelivery {
            publication: outbox[0].id.clone(),
            delivery: Delivery::Applied,
        },
    );
    assert_eq!(h.state.prices[&id].status, PriceStatus::Activating);
    assert!(h.state.events.is_empty());
    h.deliver_all();
    assert_eq!(h.state.prices[&id].status, PriceStatus::Active);
    assert_eq!(h.state.active[&slot(&fund_id("growth"), day(2))], vec![id.clone()]);
    assert!(matches!(h.state.events[0].event, Event::PriceActivated { version: 1, .. }));
    // The carrier hearing an outcome twice changes nothing.
    let again = h.send(
        CALLER_OUTBOX,
        "host",
        &[],
        Command::RecordDelivery {
            publication: outbox[0].id.clone(),
            delivery: Delivery::Applied,
        },
    );
    assert_eq!(again, Output::Done);
    assert_eq!(h.state.events.len(), 1);
}

/// Purpose: commands from the wrong caller, without the needed role or
/// without an actor are refused, and a repeated key gets the first answer.
/// Why it matters: the console, the driver and the host each speak for a
/// different authority; and delivery is at least once, so a retry must not
/// prepare a second draft.
#[test]
fn callers_roles_and_repeats() {
    let mut h = Harness::new();
    let cmd = Command::PreparePrice {
        fund: fund_id("growth"),
        pricing_date: day(2),
    };
    assert_eq!(h.refused(CALLER_INTAKE, "driver", &[ROLE_PREPARER], cmd.clone()), "unauthorised");
    assert_eq!(h.refused(CALLER_CONSOLE, "maker", &[ROLE_APPROVER], cmd.clone()), "unauthorised");
    assert_eq!(h.refused(CALLER_CONSOLE, " ", &[ROLE_PREPARER], cmd.clone()), "unauthorised");
    h.receive("F1", valuation(day(2), "2000000", UNITS, "0"));
    let e = Envelope {
        key: "same".into(),
        caller: CALLER_CONSOLE.into(),
        actor: "maker".into(),
        roles: vec![ROLE_PREPARER.into()],
        at: h.at,
        command: cmd,
    };
    let first = h.state.apply_envelope(e.clone());
    let second = h.state.apply_envelope(e);
    assert_eq!(first, second);
    assert_eq!(h.state.prices.len(), 1);
}

/// Purpose: a held price needs every failed check accepted with a reason and
/// no others; a blocked price can only be rejected; a rejection needs a reason.
/// Why it matters: accepting a check is a person taking responsibility for a
/// price that failed it, so it must be explicit and recorded; and a units
/// break must never be waved through.
#[test]
fn held_and_blocked_prices() {
    let mut h = Harness::new();
    h.price_day(day(2), "2000000", "0", None);
    // Up 10% against a 1% benchmark: previous move and benchmark fail.
    h.receive("F2", valuation(day(3), "2200000", UNITS, "0.01"));
    h.core_units(Some(day(2)), UNITS / 2);
    let id = h.prepared_id(day(3));
    assert_eq!(h.state.prices[&id].status, PriceStatus::Held);
    assert!(matches!(h.approve(&id, BTreeMap::new()), Output::Refused { ref code, .. } if code == "checks"));
    let blank: BTreeMap<_, _> = [(CheckKind::PreviousMove, " ".to_string()), (CheckKind::Benchmark, "x".to_string())].into();
    assert!(matches!(h.approve(&id, blank), Output::Refused { ref code, .. } if code == "reason_required"));
    let accepted: BTreeMap<_, _> = [
        (CheckKind::PreviousMove, "market rally confirmed with the custodian".to_string()),
        (CheckKind::Benchmark, "benchmark lags the fund's overseas holdings".to_string()),
    ]
    .into();
    assert_eq!(h.approve(&id, accepted.clone()), Output::Done);
    assert_eq!(h.state.prices[&id].accepted, accepted);

    // The core is a unit short on the next day: blocked.
    h.deliver_all();
    h.receive("F3", valuation(day(4), "2200000", UNITS, "0"));
    h.core_units(Some(day(3)), UNITS / 2 - 1);
    let blocked = h.prepared_id(day(4));
    assert_eq!(h.state.prices[&blocked].status, PriceStatus::Blocked);
    assert!(matches!(h.approve(&blocked, BTreeMap::new()), Output::Refused { ref code, .. } if code == "blocked"));
    let reject = |reason: &str| Command::RejectPrice {
        price: blocked.clone(),
        reason: reason.into(),
    };
    assert_eq!(h.refused(CALLER_CONSOLE, "checker", &[ROLE_APPROVER], reject("")), "reason_required");
    h.ok(CALLER_CONSOLE, "checker", &[ROLE_APPROVER], reject("units break sent to the administrator"));
    assert_eq!(h.state.prices[&blocked].status, PriceStatus::Rejected);
    assert_eq!(h.refused(CALLER_CONSOLE, "checker", &[ROLE_APPROVER], reject("again")), "wrong_status");
    assert!(matches!(h.approve(&blocked, BTreeMap::new()), Output::Refused { ref code, .. } if code == "wrong_status"));
    assert!(matches!(h.approve("nope", BTreeMap::new()), Output::Refused { ref code, .. } if code == "unknown_price"));
}

/// Purpose: a price prepared before a newer valuation or a change in the
/// core's units is refused at approval, preparing again replaces the old
/// draft, and no second price for a date is approved while one is on its way.
/// Why it matters: the approver must approve exactly the figures the price
/// will carry; approving a price struck from superseded inputs would publish
/// a number nobody checked.
#[test]
fn stale_drafts_are_refused() {
    let mut h = Harness::new();
    h.receive("F1", valuation(day(2), "2000000", UNITS, "0"));
    h.core_units(None, UNITS / 2);
    let first = h.prepared_id(day(2));
    h.receive("F1b", valuation(day(2), "2000100", UNITS, "0"));
    assert!(matches!(h.approve(&first, BTreeMap::new()), Output::Refused { ref code, .. } if code == "stale"));
    let second = h.prepared_id(day(2));
    assert_eq!(h.state.prices[&first].status, PriceStatus::Replaced);
    h.core_units(None, UNITS / 2 + 1);
    assert!(matches!(h.approve(&second, BTreeMap::new()), Output::Refused { ref code, .. } if code == "stale"));
    h.core_units(None, UNITS / 2);
    assert_eq!(h.approve(&second, BTreeMap::new()), Output::Done);
    // A draft prepared while that price is on its way cannot be approved,
    // during or after its activation.
    let third = h.prepared_id(day(2));
    assert!(matches!(h.approve(&third, BTreeMap::new()), Output::Refused { ref code, .. } if code == "in_flight"));
    h.deliver_all();
    assert!(matches!(h.approve(&third, BTreeMap::new()), Output::Refused { ref code, .. } if code == "stale"));
}

/// Purpose: prices go in date order. A date after a gap is refused until the
/// gap is suspended; a suspended date cannot be approved; suspension and
/// resumption publish events and refuse nonsense.
/// Why it matters: orders wait for the price of their own date, so a skipped
/// pricing day would strand them; only a suspension explains a day with no price.
#[test]
fn date_order_and_suspension() {
    let mut h = Harness::new();
    h.price_day(day(2), "2000000", "0", None);
    h.receive("F4", valuation(day(4), "2000000", UNITS, "0"));
    h.core_units(Some(day(2)), UNITS / 2);
    let d4 = h.prepared_id(day(4));
    assert!(matches!(h.approve(&d4, BTreeMap::new()), Output::Refused { ref code, .. } if code == "gap"));

    let lead = |c: Command| (CALLER_CONSOLE, "lead", [ROLE_LEAD], c);
    let suspend = |from, reason: &str| Command::SuspendFund {
        fund: fund_id("growth"),
        from,
        reason: reason.into(),
    };
    let resume = |from, reason: &str| Command::ResumeFund {
        fund: fund_id("growth"),
        from,
        reason: reason.into(),
    };
    let (c, a, r, cmd) = lead(suspend(day(3), ""));
    assert_eq!(h.refused(c, a, &r, cmd), "reason_required");
    let (c, a, r, cmd) = lead(suspend(day(3), "offshore market closed after an outage"));
    h.ok(c, a, &r, cmd);
    let (c, a, r, cmd) = lead(suspend(day(5), "again"));
    assert_eq!(h.refused(c, a, &r, cmd), "suspended");
    // Still suspended on the 4th.
    assert!(matches!(h.approve(&d4, BTreeMap::new()), Output::Refused { ref code, .. } if code == "suspended"));
    let (c, a, r, cmd) = lead(resume(day(3), "too early"));
    assert_eq!(h.refused(c, a, &r, cmd), "invalid");
    let (c, a, r, cmd) = lead(resume(day(4), ""));
    assert_eq!(h.refused(c, a, &r, cmd), "reason_required");
    let (c, a, r, cmd) = lead(resume(day(4), "market reopened"));
    h.ok(c, a, &r, cmd);
    let (c, a, r, cmd) = lead(resume(day(5), "twice"));
    assert_eq!(h.refused(c, a, &r, cmd), "not_suspended");
    let (c, a, r, cmd) = lead(suspend(day(3), "inside the last one"));
    assert_eq!(h.refused(c, a, &r, cmd), "overlap");
    let (c, a, r, cmd) = lead(Command::SuspendFund {
        fund: fund_id("unknown"),
        from: day(3),
        reason: "x".into(),
    });
    assert_eq!(h.refused(c, a, &r, cmd), "unknown_fund");
    assert_eq!(h.approve(&d4, BTreeMap::new()), Output::Done);
    let kinds: Vec<&Event> = h.state.events.iter().map(|e| &e.event).collect();
    assert!(matches!(kinds[1], Event::FundSuspended { .. }));
    assert!(matches!(kinds[2], Event::FundResumed { .. }));
    // An earlier date cannot be priced once a later one is.
    h.deliver_all();
    h.receive("F3", valuation(day(3), "2000000", UNITS, "0"));
    assert!(matches!(h.prepare(day(3)), Output::Refused { ref code, .. } if code == "out_of_order"));
}

/// Purpose: preparing refuses an unknown fund, a non-pricing day, a date with
/// no valuation and a date already priced with no error raised.
/// Why it matters: each would otherwise produce a draft that can only be
/// wrong, and a second price for a date outside the error process would
/// bypass the remediation the design requires.
#[test]
fn prepare_refusals() {
    let mut h = Harness::new();
    let p = |h: &mut Harness, fund: &str, date| {
        h.send(
            CALLER_CONSOLE,
            "maker",
            &[ROLE_PREPARER],
            Command::PreparePrice {
                fund: fund_id(fund),
                pricing_date: date,
            },
        )
    };
    let code = |o: Output| match o {
        Output::Refused { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert_eq!(code(p(&mut h, "cash", day(2))), "unknown_fund");
    assert_eq!(code(p(&mut h, "growth", day(7))), "not_a_pricing_day");
    assert_eq!(code(p(&mut h, "growth", day(2))), "no_valuation");
    h.price_day(day(2), "2000000", "0", None);
    assert_eq!(code(p(&mut h, "growth", day(2))), "already_priced");
    // An impossible valuation is refused at striking.
    h.receive("F3", valuation(day(3), "0", UNITS, "0"));
    assert_eq!(code(p(&mut h, "growth", day(3))), "cannot_strike");
}

/// Purpose: a partition that refuses a price leaves it activating and on the
/// work list; republishing offers only the refused partitions again, under a
/// new attempt, and the price goes active once they apply it.
/// Why it matters: a refusal is final for that offer, so without a
/// republish a price half-active across partitions could never be completed.
#[test]
fn refused_partition_is_republished() {
    let mut h = Harness::new();
    h.receive("F1", valuation(day(2), "2000000", UNITS, "0"));
    h.core_units(None, UNITS / 2);
    let id = h.prepared_id(day(2));
    h.approve(&id, BTreeMap::new());
    let rep = |h: &mut Harness| h.send(CALLER_CONSOLE, "lead", &[ROLE_LEAD], Command::Republish { price: id.clone() });
    assert!(matches!(rep(&mut h), Output::Refused { ref code, .. } if code == "nothing_refused"));
    let items: Vec<String> = h.state.outbox(10).iter().map(|p| p.id.clone()).collect();
    h.ok(
        CALLER_OUTBOX,
        "host",
        &[],
        Command::RecordDelivery {
            publication: items[0].clone(),
            delivery: Delivery::Applied,
        },
    );
    h.ok(
        CALLER_OUTBOX,
        "host",
        &[],
        Command::RecordDelivery {
            publication: items[1].clone(),
            delivery: Delivery::Refused {
                code: "unknown_fund".into(),
                message: "growth is not active on partition 1".into(),
            },
        },
    );
    assert_eq!(h.state.work().len(), 1);
    assert_eq!(rep(&mut h), Output::Done);
    let again: Vec<_> = h.state.outbox(10).into_iter().cloned().collect();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].partition, 1);
    assert!(again[0].id.ends_with("/2"));
    h.deliver_all();
    assert_eq!(h.state.prices[&id].status, PriceStatus::Active);
    assert!(h.state.work().is_empty());
    assert!(matches!(rep(&mut h), Output::Refused { ref code, .. } if code == "wrong_status"));
}

/// Purpose: a price is late once its fund's lag has passed with no active
/// price and no suspension; a suspended date is not late.
/// Why it matters: a late price delays every order of that date; the
/// operations team needs the list, and the design says a suspension
/// explains the wait.
#[test]
fn late_prices_are_listed() {
    let mut h = Harness::new();
    h.price_day(day(2), "2000000", "0", None);
    // Lag one pricing day: the 3rd is late on the 5th, not on the 4th.
    assert!(h.state.late(day(4)).is_empty());
    assert_eq!(h.state.late(day(5)), vec![(fund_id("growth"), day(3))]);
    h.ok(
        CALLER_CONSOLE,
        "lead",
        &[ROLE_LEAD],
        Command::SuspendFund {
            fund: fund_id("growth"),
            from: day(3),
            reason: "market closed".into(),
        },
    );
    assert!(h.state.late(day(5)).is_empty());
    // A fund that has never priced is late from the day it was activated
    // (the 2nd), and not before.
    let fresh = Harness::new();
    assert!(fresh.state.late(day(3)).is_empty());
    assert_eq!(fresh.state.late(day(4)), vec![(fund_id("growth"), day(2))]);
}
