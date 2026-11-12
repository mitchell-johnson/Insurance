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
            CALLER_EVENTS,
            "relay",
            &[],
            Command::RecordExecuted {
                fund: fund_id("growth"),
                partition,
                pricing_date: day(3),
            },
        );
    }
    assert_eq!(ask(&h, json!({"q": "errors"}))[0]["id"], "E1");
    assert_eq!(ask(&h, json!({"q": "error", "id": "E1"}))["cause"], "bond");
    let due = ask(&h, json!({"q": "extracts_due", "limit": 50}));
    assert_eq!(due[0]["currency"], "NZD");
    assert_eq!(due[0]["extract"]["id"], "growth@2026-11-03");
    let check = ask(&h, json!({"q": "close_check", "as_at": "2026-11-04"}));
    let kinds: Vec<&str> = check["items"].as_array().into_iter().flatten().filter_map(|i| i["kind"].as_str()).collect();
    assert_eq!(kinds, vec!["open_price_error", "extract_not_written"]);

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
