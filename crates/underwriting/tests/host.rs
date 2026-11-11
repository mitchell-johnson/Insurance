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

/// Purpose: every effect a command produces is also kept in the replicated outbox, in order and
/// numbered, until the driver acknowledges it; an acknowledgement removes exactly what it
/// covers, a repeated command adds nothing, only the driver may acknowledge, and a number never
/// produced is refused.
/// Why it matters: the reply to a command can be lost with the process that asked. Without the
/// outbox a lost reply would be a capacity reservation never made or a policy never issued,
/// with nothing left to say so.
#[test]
fn effects_wait_in_the_outbox_until_the_driver_acknowledges_them() {
    use ic_underwriting::command::Command;

    let mut w = World::new();
    assert!(w.s.outbox(0, 10).is_empty());
    w.open("P1", 500_000_00);
    let disclosed = w.disclose("P1", "L1", &clean());
    assert!(!disclosed.effects.is_empty());

    let waiting = w.s.outbox(0, 100);
    let numbers: Vec<u64> = waiting.iter().map(|(n, _)| *n).collect();
    assert_eq!(numbers, (1..=waiting.len() as u64).collect::<Vec<_>>());
    let tail: Vec<_> = waiting[waiting.len() - disclosed.effects.len()..].iter().map(|(_, e)| e.clone()).collect();
    assert_eq!(tail, disclosed.effects);
    // Read in pages, as the driver does.
    assert_eq!(w.s.outbox(0, 1), waiting[..1].to_vec());
    assert_eq!(w.s.outbox(1, 100), waiting[1..].to_vec());
    let question = json!({ "query": "outbox", "after": 0, "max": 100 });
    assert_eq!(answer(&w.s, question).unwrap(), serde_json::to_value(&waiting).unwrap());

    // The same command delivered again produces nothing new.
    let again = w.log[w.log.len() - 1].clone();
    assert!(ic_log::StateMachine::apply(&mut w.s, 999, again).unwrap().duplicate);
    assert_eq!(w.s.outbox(0, 100), waiting);

    assert_eq!(w.err("new-business", Command::AcknowledgeEffects { through: 1 }), "caller");
    let last = waiting.len() as u64;
    assert_eq!(w.err("effects-driver", Command::AcknowledgeEffects { through: last + 1 }), "unknown_effect");
    assert_eq!(w.s.outbox(0, 100), waiting);

    w.ok("effects-driver", Command::AcknowledgeEffects { through: 1 });
    assert_eq!(w.s.outbox(0, 100), waiting[1..].to_vec());
    w.ok("effects-driver", Command::AcknowledgeEffects { through: last });
    assert!(w.s.outbox(0, 100).is_empty());
    // Acknowledged twice, as after a retry: nothing to remove, and not an error.
    w.ok("effects-driver", Command::AcknowledgeEffects { through: last });

    // Numbers are never reused.
    let next = w.automatic(&disclosed);
    assert!(!next.effects.is_empty());
    assert_eq!(w.s.outbox(0, 1)[0].0, last + 1);
}
