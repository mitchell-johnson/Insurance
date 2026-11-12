//! Price errors and their correction, and the core's unit movements feeding
//! the remediation record, the holdings extract and the net flow.

mod support;

use std::collections::BTreeMap;

use ic_pricing::command::*;
use ic_pricing::state::{slot, PriceStatus, PricingState};
use support::*;

fn lead(h: &mut Harness, command: Command) -> Output {
    h.send(CALLER_CONSOLE, "lead", &[ROLE_LEAD], command)
}

fn code(o: Output) -> String {
    match o {
        Output::Refused { code, .. } => code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn raise(error: &str, prices: &[&str], cause: &str) -> Command {
    Command::RaisePriceError {
        error: error.into(),
        prices: prices.iter().map(|p| p.to_string()).collect(),
        cause: cause.into(),
        detected_on: day(5),
    }
}

/// Two days priced: the 2nd at 2.0000 and the 3rd at 2.0100.
fn two_days() -> (Harness, String, String) {
    let mut h = Harness::new();
    let d2 = h.price_day(day(2), "2000000", "0", None);
    let d3 = h.price_day(day(3), "2010000", "0.005", Some(day(2)));
    (h, d2, d3)
}

fn restated(h: &mut Harness, nav: &str) {
    let mut v = valuation(day(3), nav, UNITS, "0.005");
    v.restated = true;
    h.receive("F3-restated", v);
}

/// Purpose: the design's remediation path for a material error. Raising it
/// holds the fund's next price; the restated valuation gives a corrected
/// price that becomes version 2 of the date, superseding the wrong one; the
/// error is marked corrected, the held price must be prepared again against
/// the corrected previous price, and the replay's movements build the
/// remediation record before the error is closed.
/// Why it matters: a wrong price that is edited, or a later price struck
/// from it, would leave investors with the wrong units; the containment and
/// the version chain are how the platform proves every affected order was
/// put right.
#[test]
fn material_error_is_corrected_by_a_new_version() {
    let (mut h, _d2, d3) = two_days();
    h.receive("F4", valuation(day(4), "2010000", UNITS, "0"));
    h.core_units(Some(day(3)), UNITS / 2);
    let d4 = h.prepared_id(day(4));

    assert_eq!(code(lead(&mut h, raise("E1", &[&d3], ""))), "reason_required");
    assert_eq!(lead(&mut h, raise("E1", &[&d3], "administrator mispriced a bond")), Output::Done);
    assert_eq!(code(h.approve(&d4, BTreeMap::new())), "held_by_error");
    assert_eq!(
        code(lead(
            &mut h,
            Command::CloseError {
                error: "E1".into(),
                summary: "x".into()
            }
        )),
        "not_corrected"
    );

    // A new valuation for an active date must say it is restated.
    let plain = h.send(
        CALLER_INTAKE,
        "driver",
        &[],
        Command::ReceiveValuations {
            administrator: "acme".into(),
            file_id: "F3-plain".into(),
            valuations: vec![valuation(day(3), "2030000", UNITS, "0.005")],
        },
    );
    assert!(matches!(plain, Output::Valuations { ref refused, .. } if refused.len() == 1), "{plain:?}");
    restated(&mut h, "2030000");
    let fix = h.prepared_id(day(3));
    assert_eq!(h.state.prices[&fix].corrects.as_deref(), Some("E1"));
    assert_eq!(h.approve(&fix, BTreeMap::new()), Output::Done);
    let out = h.state.outbox(10)[0].clone();
    assert_eq!((out.version, out.supersedes.as_deref()), (2, Some(d3.as_str())));
    h.deliver_all();
    assert_eq!(h.state.prices[&fix].status, PriceStatus::Active);
    assert_eq!(h.state.prices[&d3].status, PriceStatus::Superseded);
    assert_eq!(h.state.active[&slot(&fund_id("growth"), day(3))], vec![d3.clone(), fix.clone()]);
    let err = &h.state.errors["E1"];
    assert!(err.corrected());
    let correction = err.prices[&d3].clone().unwrap_or_else(|| panic!("no correction"));
    assert!(correction.material);
    assert_eq!(correction.size.as_str(), "0.00985222");
    assert!(h
        .state
        .events
        .iter()
        .any(|e| matches!(e.event, Event::PriceErrorCorrected { material: true, .. })));

    // The draft for the 4th compared itself with the wrong price.
    assert_eq!(code(h.approve(&d4, BTreeMap::new())), "stale");
    let d4b = h.prepared_id(day(4));
    assert_eq!(h.state.prices[&d4b].previous.as_ref().map(|p| p.price_id.clone()), Some(fix.clone()));

    // The core replays two orders at the corrected price.
    for (id, policy, units) in [("R1", "P1", -4_926), ("R2", "P2", -9_852)] {
        h.ok(
            CALLER_EVENTS,
            "relay",
            &[],
            Command::RecordMovement(Box::new(UnitMovement {
                id: id.into(),
                partition: 0,
                policy: policy.into(),
                fund: fund_id("growth"),
                kind: MovementKind::Correction,
                units_micro: units,
                cash_minor: 0,
                price_id: Some(fix.clone()),
                pricing_date: day(3),
            })),
        );
    }
    let r = &h.state.errors["E1"].remediation;
    assert_eq!((r.policies.len(), r.movements, r.units_micro), (2, 2, -14_778));

    let close = |s: &str| Command::CloseError {
        error: "E1".into(),
        summary: s.into(),
    };
    assert_eq!(code(lead(&mut h, close(" "))), "reason_required");
    assert_eq!(lead(&mut h, close("2 policies adjusted; fund made whole")), Output::Done);
    assert_eq!(code(lead(&mut h, close("again"))), "closed");
    assert_eq!(
        code(lead(
            &mut h,
            Command::CloseError {
                error: "E9".into(),
                summary: "x".into()
            }
        )),
        "unknown_error"
    );
    assert!(matches!(h.state.events.last().map(|e| &e.event), Some(Event::PriceErrorClosed { .. })));
}

/// Purpose: an error below the fund's materiality threshold is recorded with
/// its size and threshold and marked corrected, but the corrected price is
/// never sent to the core, so no order is replayed.
/// Why it matters: the design makes the compensation threshold a recorded
/// policy setting; replaying immaterial errors would churn every policy in
/// the fund for fractions of a cent.
#[test]
fn immaterial_error_is_recorded_not_replayed() {
    let (mut h, _d2, d3) = two_days();
    lead(&mut h, raise("E2", &[&d3], "rounding in the administrator's accrual"));
    restated(&mut h, "2010100");
    let fix = h.prepared_id(day(3));
    assert_eq!(h.approve(&fix, BTreeMap::new()), Output::Done);
    assert_eq!(h.state.prices[&fix].status, PriceStatus::Immaterial);
    assert!(h.state.outbox(10).is_empty());
    assert_eq!(h.state.active[&slot(&fund_id("growth"), day(3))], vec![d3.clone()]);
    let err = &h.state.errors["E2"];
    assert!(err.corrected());
    let c = err.prices[&d3].clone().unwrap_or_else(|| panic!("no correction"));
    assert!(!c.material);
    assert_eq!(c.threshold.as_str(), "0.003");
    assert!(h
        .state
        .events
        .iter()
        .any(|e| matches!(e.event, Event::PriceErrorCorrected { material: false, .. })));
}

/// Purpose: raising an error refuses an existing ID, no prices, an unknown
/// or inactive price, a price already in an open error, and prices of two funds.
/// Why it matters: an error case is the record a regulator reads; it must
/// name exactly the prices in force that were wrong, once each.
#[test]
fn raising_an_error_is_checked() {
    let (mut h, d2, d3) = two_days();
    assert_eq!(code(lead(&mut h, raise("E1", &[], "x"))), "invalid");
    assert_eq!(code(lead(&mut h, raise("", &[&d3], "x"))), "invalid");
    assert_eq!(code(lead(&mut h, raise("E1", &["nope"], "x"))), "unknown_price");
    assert_eq!(code(lead(&mut h, raise("E1", &[&d3, "nope"], "x"))), "unknown_price");
    h.receive("F4", valuation(day(4), "2010000", UNITS, "0"));
    h.core_units(Some(day(3)), UNITS / 2);
    let draft = h.prepared_id(day(4));
    assert_eq!(code(lead(&mut h, raise("E1", &[&draft], "x"))), "not_active");
    assert_eq!(lead(&mut h, raise("E1", &[&d2], "x")), Output::Done);
    assert_eq!(code(lead(&mut h, raise("E1", &[&d3], "x"))), "invalid");
    assert_eq!(code(lead(&mut h, raise("E2", &[&d2], "x"))), "already_raised");

    // A second fund, priced once.
    let mut cash = fund("cash");
    cash.pricing.remove("benchmark_gap");
    h.ok(CALLER_CONFIG, "config", &[], Command::ActivateFund(Box::new(cash)));
    let mut v = valuation(day(2), "500000", UNITS, "0");
    v.fund = fund_id("cash");
    v.benchmark_return = None;
    h.receive("C2", v);
    for partition in 0..2 {
        h.ok(
            CALLER_INTAKE,
            "driver",
            &[],
            Command::RecordCoreUnits {
                fund: fund_id("cash"),
                partition,
                executed_through: None,
                units_micro: UNITS / 2,
            },
        );
    }
    let c2 = match h.send(
        CALLER_CONSOLE,
        "maker",
        &[ROLE_PREPARER],
        Command::PreparePrice {
            fund: fund_id("cash"),
            pricing_date: day(2),
        },
    ) {
        Output::Prepared { price, .. } => price,
        other => panic!("{other:?}"),
    };
    h.approve(&c2, BTreeMap::new());
    h.deliver_all();
    assert_eq!(code(lead(&mut h, raise("E3", &[&d3, &c2], "x"))), "invalid");
}

/// Purpose: the core's buys, sells and openings build each policy's holding
/// and the day's flow; a movement offered twice counts once; once every
/// partition has executed a date the extract is taken, and writing it drops
/// the per-policy detail but keeps the totals.
/// Why it matters: the holdings extract is how the administrator's register
/// is kept in step with the core, and the net flow is the cash the fund
/// settles; counting an order twice would put both out by its amount.
#[test]
fn movements_build_holdings_and_extracts() {
    let (mut h, _d2, d3) = two_days();
    let mv = |id: &str, partition, policy: &str, kind, units, cash| {
        Command::RecordMovement(Box::new(UnitMovement {
            id: id.into(),
            partition,
            policy: policy.into(),
            fund: fund_id("growth"),
            kind,
            units_micro: units,
            cash_minor: cash,
            price_id: Some(d3.clone()),
            pricing_date: day(3),
        }))
    };
    h.ok(CALLER_EVENTS, "relay", &[], mv("O1", 0, "P1", MovementKind::Opening, 5_000_000, 0));
    h.ok(CALLER_EVENTS, "relay", &[], mv("B1", 0, "P1", MovementKind::Buy, 497_265, 1_000_00));
    h.ok(CALLER_EVENTS, "relay", &[], mv("B1", 0, "P1", MovementKind::Buy, 497_265, 1_000_00));
    h.ok(CALLER_EVENTS, "relay", &[], mv("S1", 1, "P2", MovementKind::Sell, -250_000, -500_00));
    h.ok(CALLER_EVENTS, "relay", &[], mv("S2", 1, "P3", MovementKind::Sell, 0, 0));
    // A correction movement outside any error changes holdings only.
    h.ok(CALLER_EVENTS, "relay", &[], mv("C1", 1, "P2", MovementKind::Correction, 250_000, 0));
    assert_eq!(
        code(h.send(CALLER_EVENTS, "relay", &[], mv("X", 7, "P1", MovementKind::Buy, 1, 1))),
        "unknown_partition"
    );
    let holdings = &h.state.holdings[&fund_id("growth")];
    assert_eq!(holdings.get("P1"), Some(&5_497_265));
    assert_eq!(holdings.get("P2"), None, "a holding back to zero is dropped");

    let executed = |partition| Command::RecordExecuted {
        fund: fund_id("growth"),
        partition,
        pricing_date: day(3),
    };
    h.ok(CALLER_EVENTS, "relay", &[], executed(0));
    assert!(h.state.extracts_due(10).is_empty());
    h.ok(CALLER_EVENTS, "relay", &[], executed(1));
    let x = h.state.extracts_due(10)[0].clone();
    assert_eq!((x.units_on_issue_micro, x.holders), (5_497_265, 1));
    assert_eq!((x.flow.applications_minor, x.flow.redemptions_minor, x.flow.orders), (1_000_00, 500_00, 3));
    assert_eq!(x.price.as_deref(), Some(d3.as_str()));
    // A late report for the same date does not take the extract again.
    h.ok(CALLER_EVENTS, "relay", &[], executed(1));
    assert_eq!(h.state.extracts.len(), 1);
    let written = |extract: &str| Command::RecordExtractWritten {
        extract: extract.into(),
        name: "holdings/growth/2026-11-03.csv".into(),
        sha256: "ab".into(),
    };
    h.ok(CALLER_INTAKE, "driver", &[], written(&x.id));
    assert!(h.state.extracts[&x.id].holdings.is_empty());
    assert_eq!(h.state.extracts[&x.id].units_on_issue_micro, 5_497_265);
    assert!(h.state.extracts_due(10).is_empty());
    assert_eq!(code(h.send(CALLER_INTAKE, "driver", &[], written(&x.id))), "written");
    assert_eq!(code(h.send(CALLER_INTAKE, "driver", &[], written("nope"))), "unknown_extract");
}

/// Purpose: fund definitions activate once per version with the same
/// content, follow in order, and refuse invalid content; the partition
/// count is fixed once recorded.
/// Why it matters: definitions and partitions are what every price is
/// struck and published against; changing either underneath active prices
/// would make yesterday's price unreproducible.
#[test]
fn definitions_and_partitions() {
    let mut h = Harness::new();
    let act = |h: &mut Harness, f| h.send(CALLER_CONFIG, "config", &[], Command::ActivateFund(Box::new(f)));
    assert_eq!(act(&mut h, fund("growth")), Output::Done);
    let mut other = fund("growth");
    other.name = "renamed".into();
    assert_eq!(code(act(&mut h, other)), "config_conflict");
    let mut skip = fund("growth");
    skip.version = 3;
    assert_eq!(code(act(&mut h, skip)), "invalid_definition");
    let mut late = fund("cash");
    late.version = 2;
    assert_eq!(code(act(&mut h, late)), "invalid_definition");
    let mut bad = fund("cash");
    bad.price_decimals = 0;
    assert_eq!(code(act(&mut h, bad)), "invalid_definition");
    let mut v2 = fund("growth");
    v2.version = 2;
    v2.effective_from = day(9);
    v2.buy_spread_bps = 30;
    assert_eq!(act(&mut h, v2), Output::Done);
    assert_eq!(h.state.definition_on(&fund_id("growth"), day(6)).map(|d| d.version), Some(1));
    assert_eq!(h.state.definition_on(&fund_id("growth"), day(9)).map(|d| d.version), Some(2));

    let parts = |h: &mut Harness, count| h.send(CALLER_INTAKE, "driver", &[], Command::RecordPartitions { count });
    assert_eq!(parts(&mut h, 2), Output::Done);
    assert_eq!(code(parts(&mut h, 3)), "partitions_fixed");
    assert_eq!(code(parts(&mut h, 0)), "invalid");
    let units = |fund: &str, partition| Command::RecordCoreUnits {
        fund: fund_id(fund),
        partition,
        executed_through: None,
        units_micro: 1,
    };
    assert_eq!(code(h.send(CALLER_INTAKE, "driver", &[], units("growth", 2))), "unknown_partition");
    assert_eq!(code(h.send(CALLER_INTAKE, "driver", &[], units("cash", 0))), "unknown_fund");

    // Before the partitions are known nothing can be published or counted.
    let mut empty = PricingState::new();
    let e = Envelope {
        key: "k".into(),
        caller: CALLER_INTAKE.into(),
        actor: "driver".into(),
        roles: vec![],
        at: h.at,
        command: Command::RecordCoreUnits {
            fund: fund_id("growth"),
            partition: 0,
            executed_through: None,
            units_micro: 1,
        },
    };
    assert_eq!(code(empty.apply_envelope(e)), "no_partitions");
}

/// Purpose: a file's valuations are accepted or refused one by one with
/// reasons, an unreadable file is recorded, and an empty file is refused.
/// Why it matters: one bad line from the administrator must not stop every
/// other fund pricing that day, but nothing unknown may slip in.
#[test]
fn valuations_are_taken_one_by_one() {
    let mut h = Harness::new();
    let mut unknown = valuation(day(2), "1", UNITS, "0");
    unknown.fund = fund_id("cash");
    let file = |id: &str, v| Command::ReceiveValuations {
        administrator: "acme".into(),
        file_id: id.into(),
        valuations: v,
    };
    let out = h.ok(
        CALLER_INTAKE,
        "driver",
        &[],
        file("F1", vec![valuation(day(2), "2000000", UNITS, "0"), unknown]),
    );
    match out {
        Output::Valuations { accepted, refused } => {
            assert_eq!(accepted, vec!["growth@2026-11-02".to_string()]);
            assert_eq!(refused.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    let again = h.ok(CALLER_INTAKE, "driver", &[], file("F1", vec![valuation(day(2), "2000000", UNITS, "0")]));
    assert!(matches!(again, Output::Valuations { ref refused, .. } if refused[0].1.contains("already delivered")));
    assert_eq!(code(h.send(CALLER_INTAKE, "driver", &[], file("F2", vec![]))), "invalid");
    h.ok(
        CALLER_INTAKE,
        "driver",
        &[],
        Command::RefuseValuationFile {
            file_id: "F3".into(),
            reason: "no trailer".into(),
        },
    );
    assert_eq!(h.state.refused_files["F3"], "no trailer");
}

/// Purpose: two replicas applying the same commands hold the same state and
/// hash; events are read in pages after a sequence number; old idempotency
/// keys are forgotten after the retention period.
/// Why it matters: the module runs as a replicated group whose members
/// compare hashes; and a state that remembered every key forever would grow
/// without bound.
#[test]
fn replicas_agree_and_state_stays_bounded() {
    let (a, _, d3) = two_days();
    let (b, _, _) = two_days();
    assert_eq!(a.state.state_hash(), b.state.state_hash());
    let mut a = a;
    lead(&mut a, raise("E1", &[&d3], "x"));
    assert_ne!(a.state.state_hash(), b.state.state_hash());
    let all = a.state.events_after(0, 100);
    assert_eq!(all.len(), 3);
    let page = a.state.events_after(all[0].seq, 1);
    assert_eq!(page[0].seq, all[1].seq);
    let keys = a.state.applied.len();
    a.at += chrono::Duration::days(40);
    a.ok(CALLER_INTAKE, "driver", &[], Command::RecordPartitions { count: 2 });
    assert!(a.state.applied.len() < keys);
    assert_eq!(a.state.applied.len(), 1);
}
