//! The questions the hosted module answers, and its spec.

mod support;

use std::collections::BTreeMap;

use ic_pricing::command::*;
use ic_pricing::host::{answer, module_spec, NAME};
use serde_json::{json, Value};
use support::*;

fn ask(h: &Harness, q: Value) -> Value {
    answer(&h.state, q).unwrap_or_else(|e| panic!("{e}"))
}

/// Purpose: every question is answered from the state in the shape the
/// console, the driver and the finance close read, and anything else is refused.
/// Why it matters: these answers are the module's read interface; the
/// driver writes extracts from one and the close is blocked or cleared by another.
#[test]
fn answers_each_question() {
    let mut h = Harness::new();
    let d2 = h.price_day(day(2), "2000000", "0", None);
    h.receive("F3", valuation(day(3), "2010000", UNITS, "0.005"));
    h.core_units(Some(day(2)), UNITS / 2);
    let d3 = h.prepared_id(day(3));

    let status = ask(&h, json!({"q": "status"}));
    assert_eq!(status["partitions"], 2);
    assert_eq!(status["state_hash"].as_str().map(str::len), Some(64));
    assert_eq!(ask(&h, json!({"q": "funds"}))[0]["fund"], "growth");
    assert_eq!(ask(&h, json!({"q": "price", "id": d2}))["status"], "active");
    let prices = ask(&h, json!({"q": "prices", "fund": "growth", "from": "2026-11-01", "to": "2026-11-30"}));
    assert_eq!(prices.as_array().map(Vec::len), Some(1));
    assert_eq!(prices[0]["mid"], "2.0000");
    assert_eq!(
        ask(&h, json!({"q": "valuations", "fund": "growth", "pricing_date": "2026-11-03"}))[0]["revision"],
        1
    );
    assert_eq!(ask(&h, json!({"q": "work"}))["prices"][0]["id"], d3);
    assert_eq!(ask(&h, json!({"q": "late", "as_at": "2026-11-05"}))[0][1], "2026-11-03");
    assert_eq!(ask(&h, json!({"q": "outbox", "limit": 5})), json!([]));
    assert_eq!(ask(&h, json!({"q": "events", "after": 0, "limit": 5})).as_array().map(Vec::len), Some(1));
    assert_eq!(ask(&h, json!({"q": "holding", "fund": "growth", "policy": "P1"}))["units_micro"], 0);

    let check = ask(&h, json!({"q": "close_check", "as_at": "2026-11-05"}));
    assert_eq!(check["green"], false);
    let kinds: Vec<&str> = check["items"].as_array().into_iter().flatten().filter_map(|i| i["kind"].as_str()).collect();
    assert!(kinds.contains(&"late_price") && kinds.contains(&"price_awaiting_action"));

    h.approve(&d3, BTreeMap::new());
    h.deliver_all();
    h.ok(
        CALLER_CONSOLE,
        "lead",
        &[ROLE_LEAD],
        Command::RaisePriceError {
            error: "E1".into(),
            prices: vec![d3.clone()],
            cause: "bond".into(),
            detected_on: day(4),
        },
    );
    for partition in 0..2 {
        h.ok(
            CALLER_INTAKE,
            "driver",
            &[],
            Command::RecordCoreUnits {
                fund: fund_id("growth"),
                partition,
                executed_through: Some(day(3)),
                units_micro: UNITS / 2,
            },
        );
    }
    assert_eq!(ask(&h, json!({"q": "errors"}))[0]["id"], "E1");
    assert_eq!(ask(&h, json!({"q": "error", "id": "E1"}))["cause"], "bond");
    let due = ask(&h, json!({"q": "extracts_due", "limit": 50}));
    assert_eq!(due[0]["currency"], "NZD");
    assert_eq!(due[0]["extract"]["id"], "growth:2026-11-02");
    assert_eq!(due[1]["extract"]["id"], "growth:2026-11-03");
    let check = ask(&h, json!({"q": "close_check", "as_at": "2026-11-04"}));
    let kinds: Vec<&str> = check["items"].as_array().into_iter().flatten().filter_map(|i| i["kind"].as_str()).collect();
    assert_eq!(kinds, vec!["open_price_error", "extract_not_written", "extract_not_written"]);

    assert!(answer(&h.state, json!({"q": "nonsense"})).unwrap_err().contains("not a question"));
}

/// Purpose: the hosted module takes a command as JSON, answers a question,
/// and survives a snapshot: saved state loads into a new member with the
/// same answer. The spec carries the permanent name.
/// Why it matters: this is the exact path the ledger nodes use; a state
/// that does not round-trip through its snapshot would split the group
/// after the first compaction.
#[test]
fn hosted_module_applies_answers_and_snapshots() {
    let spec = module_spec().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(spec.name, NAME);
    assert_eq!(NAME, "unit-pricing");
    let mut module = (spec.make)();
    let envelope = json!({
        "key": "k1",
        "caller": CALLER_INTAKE,
        "actor": "driver",
        "at": "2026-11-02T04:00:00Z",
        "command": {"record_partitions": {"count": 4}},
    });
    assert_eq!(module.apply(1, envelope), Ok(json!("done")));
    assert_eq!(module.query(json!({"q": "status"})).map(|s| s["partitions"].clone()), Ok(json!(4)));
    let saved = module.save().unwrap_or_else(|e| panic!("{e}"));
    let mut copy = (spec.make)();
    copy.load(saved).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(copy.query(json!({"q": "status"})), module.query(json!({"q": "status"})));
}

/// Purpose: an executed unit order from the core becomes one movement with
/// the sign of its direction, a replayed order becomes a correction keyed by
/// the order and the corrected price, and any other event is ignored.
/// Why it matters: holdings, the net flow and the remediation record are
/// built only from these; a sell counted as a buy would put the
/// administrator's register out by twice the order.
#[test]
fn core_events_become_movements() {
    use ic_core::invest::{Direction, InvestEvent, OrderKind};
    use ic_pricing::host::react;
    let record = |event| ic_core::EventRecord {
        id: "3-17".into(),
        partition: 3,
        seq: 17,
        business_date: day(3),
        log_index: 40,
        traceparent: None,
        event: ic_core::Event::Investment(event),
    };
    let sell = record(InvestEvent::UnitOrderExecuted {
        order_id: "O-9".into(),
        policy: "POL-1".into(),
        fund: fund_id("growth"),
        direction: Direction::Sell,
        kind: OrderKind::Withdrawal,
        cash_minor: 1_000_00,
        units_micro: 497_512,
        price_id: "growth:2026-11-03.1".into(),
        pricing_date: day(3),
    });
    let envelopes = react(&sell);
    assert_eq!(envelopes.len(), 1);
    let e = &envelopes[0];
    assert_eq!((e.key.as_str(), e.caller.as_str()), ("event:3-17", CALLER_EVENTS));
    assert_eq!(e.at.date_naive(), day(3));
    match &e.command {
        Command::RecordMovement(m) => {
            assert_eq!((m.kind, m.units_micro, m.cash_minor, m.partition), (MovementKind::Sell, -497_512, -1_000_00, 3));
            assert_eq!(m.policy, "POL-1");
        }
        other => panic!("{other:?}"),
    }
    let buy = record(InvestEvent::UnitOrderExecuted {
        order_id: "O-10".into(),
        policy: "POL-1".into(),
        fund: fund_id("growth"),
        direction: Direction::Buy,
        kind: OrderKind::PremiumAllocation,
        cash_minor: 1_000_00,
        units_micro: 497_265,
        price_id: "growth:2026-11-03.1".into(),
        pricing_date: day(3),
    });
    assert!(matches!(&react(&buy)[0].command, Command::RecordMovement(m) if m.units_micro == 497_265 && m.kind == MovementKind::Buy));
    let replayed = record(InvestEvent::UnitOrderReplayed {
        order_id: "O-10".into(),
        policy: "POL-1".into(),
        fund: fund_id("growth"),
        pricing_date: day(3),
        from_price: "growth:2026-11-03.1".into(),
        to_price: "growth:2026-11-03.2".into(),
        units_delta_micro: -2_450,
        cash_delta_minor: 0,
        borne_minor: 5_00,
        compensated: false,
    });
    match &react(&replayed)[0].command {
        Command::RecordMovement(m) => {
            assert_eq!(m.id, "O-10/replay/growth:2026-11-03.2");
            assert_eq!(
                (m.kind, m.units_micro, m.borne_minor, m.uncompensated),
                (MovementKind::Correction, -2_450, 5_00, true)
            );
            assert_eq!(m.price_id.as_deref(), Some("growth:2026-11-03.2"));
        }
        other => panic!("{other:?}"),
    }
    let other = record(InvestEvent::FundActivated {
        fund: fund_id("growth"),
        version: 1,
        content_hash: "x".into(),
        definition: None,
    });
    assert!(react(&other).is_empty());
}

/// Purpose: the outbox carrier reads each publication as the core's
/// `ActivateUnitPrice` for its own partition with a stable key, and turns
/// the core's answer (applied, refused, unroutable) into the module's
/// delivery record.
/// Why it matters: this is the only way a price reaches the core; the key
/// must not change between offers or a retry could apply twice, and a
/// refusal must reach the work list with the core's reason.
#[test]
fn outbox_carries_prices_to_the_core() {
    use ic_core::invest::InvestCommand;
    use ic_pricing::host::OUTBOX;
    use ic_raft::{CoreOutcome, Route};
    let mut h = Harness::new();
    h.receive("F1", valuation(day(2), "2000000", UNITS, "0"));
    h.core_units(None, UNITS / 2);
    let id = h.prepared_id(day(2));
    h.approve(&id, BTreeMap::new());
    let answer = ask(&h, (OUTBOX.pending)(10));
    let postings = (OUTBOX.read)(answer).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(postings.len(), 2);
    assert_eq!(postings[1].route, Route::Partition(1));
    assert_eq!(postings[1].envelope.key, format!("unit-pricing:{id}/1/1"));
    assert_eq!(postings[1].envelope.caller, "unit-pricing");
    match &postings[1].envelope.command {
        ic_core::Command::Investment(c) => match c.as_ref() {
            InvestCommand::ActivateUnitPrice(p) => {
                assert_eq!(
                    (p.price_id.as_str(), p.version, p.mid.as_str(), p.supersedes.clone()),
                    (id.as_str(), 1, "2.0000", None)
                );
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert!((OUTBOX.read)(json!({"x": 1})).is_err());

    let outcomes = [
        CoreOutcome::Applied {
            partition: 0,
            log_index: 9,
            duplicate: false,
        },
        CoreOutcome::Refused(ic_core::Reject::new("replay_required", "orders executed at the superseded price")),
        CoreOutcome::Unroutable("no such partition".into()),
    ];
    let mut deliveries = Vec::new();
    for outcome in &outcomes {
        let envelope: Envelope = serde_json::from_value((OUTBOX.outcome)(&postings[0], outcome)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(envelope.caller, CALLER_OUTBOX);
        assert_eq!(envelope.key, format!("outcome:{}", postings[0].id));
        match envelope.command {
            Command::RecordDelivery { delivery, .. } => deliveries.push(delivery),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(deliveries[0], Delivery::Applied);
    assert!(matches!(&deliveries[1], Delivery::Refused { code, .. } if code == "replay_required"));
    assert!(matches!(&deliveries[2], Delivery::Refused { code, .. } if code == "unroutable"));
}
