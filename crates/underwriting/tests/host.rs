//! The module in the form a ledger node hosts it.

mod common;

use common::*;
use ic_underwriting::host::{module_spec, MODULE};
use ic_underwriting::special::answer;
use serde_json::json;

/// Purpose: the hosted module takes the same commands as JSON, reaches exactly the state the
/// state machine reaches directly, answers a repeated command with its first result, refuses
/// what it cannot decode without changing anything, answers queries, and restores from a
/// snapshot to the same state.
/// Why it matters: this is the production wiring. If the JSON form of a command, a result or the
/// saved state did not round-trip, members of the group would diverge or a restored node would
/// make different underwriting decisions from the same log.
#[test]
fn the_hosted_module_matches_the_state_machine_and_survives_a_snapshot() {
    let mut w = World::new();
    let opened = w.open("P1", 500_000_00);
    assert!(!opened.duplicate);
    let disclosed = w.disclose("P1", "L1", &clean());
    w.automatic(&disclosed);

    let spec = module_spec().unwrap();
    assert_eq!((spec.name.as_str(), spec.events), (MODULE, false));
    let mut hosted = (spec.make)();
    for (i, envelope) in w.log.iter().enumerate() {
        let reply = hosted.apply(i as u64 + 1, serde_json::to_value(envelope).unwrap()).unwrap();
        assert!(reply.get("Ok").is_some(), "command {i} refused: {reply}");
    }
    let direct = serde_json::to_value(&w.s).unwrap();
    assert_eq!(hosted.save().unwrap(), direct);

    // Delivered again, as after a change of leader: the first result, marked as a repeat.
    let again = hosted.apply(900, serde_json::to_value(&w.log[w.log.len() - 1]).unwrap()).unwrap();
    assert_eq!(again["Ok"]["duplicate"], true);
    // Not a command at all: refused, and nothing changes.
    assert!(hosted.apply(901, json!({ "key": "x" })).is_err());
    assert_eq!(hosted.save().unwrap(), direct);

    let question = json!({ "query": "proposal", "proposal": "P1" });
    assert_eq!(hosted.query(question.clone()).unwrap(), answer(&w.s, question.clone()).unwrap());
    assert!(hosted.query(json!({ "query": "nonsense" })).is_err());

    let mut restored = (spec.make)();
    restored.load(direct.clone()).unwrap();
    assert_eq!(restored.save().unwrap(), direct);
    assert_eq!(restored.query(question.clone()).unwrap(), hosted.query(question).unwrap());
    assert!(restored.load(json!("not a state")).is_err());
}
