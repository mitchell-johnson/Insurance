//! A proposal's progress, kept for the portals.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_underwriting::command::Command;
use ic_underwriting::progress::Pending;
use ic_underwriting::{answer, Query};
use serde_json::{json, Value};

fn pending(w: &World) -> Vec<Pending> {
    let query = serde_json::to_value(Query::Progress { max: 10 }).unwrap();
    serde_json::from_value(answer(&w.s, query).unwrap()).unwrap()
}

fn published(w: &mut World, version: u64) {
    w.ok(
        "effects-driver",
        Command::ProgressPublished {
            proposal: "P1".into(),
            version,
        },
    );
}

/// The proposal row's status and whose move it is, and the life's stage and whose move it is.
fn shown(p: &Pending) -> (Value, Value, Value, Value) {
    (
        p.rows[0].body["status"].clone(),
        p.rows[0].body["waiting_on"].clone(),
        p.rows[1].body["stage"].clone(),
        p.rows[1].body["waiting_on"].clone(),
    )
}

/// Purpose: a command that changes what a proposal's rows say marks the proposal for
/// publishing under a new version; one that changes nothing they say does not; the driver's
/// word that a version went clears it only if no later change has come in; and a declined
/// proposal is shown to everyone on the policy as ended, with the decline on the life's own row
/// alone, and is forgotten once published. Rows are keyed under the policy and this module.
/// Why it matters: the portal is where an applicant and their adviser see whether the next move
/// is theirs. A change published under an old version would be lost when the older answer
/// arrived; a decline on the shared row would tell a policyholder about another adult's health;
/// and remembering every ended proposal would grow the module's snapshot for ever.
#[test]
fn progress_is_marked_by_version_and_a_decline_is_shown_only_to_the_life() {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let first = pending(&w);
    assert_eq!(first.len(), 1);
    let policy = w.p("P1").policy.clone();
    assert_eq!(first[0].rows[0].key, format!("{policy}/underwriting/P1"));
    assert_eq!(first[0].rows[1].key, format!("{policy}/underwriting/P1/L1"));
    assert_eq!(
        (first[0].rows[0].sensitive, first[0].rows[1].sensitive, first[0].rows[1].subject.clone()),
        (false, true, Some("PTY-L1".into()))
    );
    assert_eq!(
        (first[0].rows[0].body["entry"].clone(), first[0].rows[0].body["proposal"].clone()),
        (json!("new_business"), json!("P1"))
    );
    assert_eq!(
        shown(&first[0]),
        (json!("open"), json!("applicant"), json!("awaiting_disclosure"), json!("applicant"))
    );

    published(&mut w, first[0].version - 1);
    assert_eq!(pending(&w), first, "an older version leaves it waiting");
    published(&mut w, first[0].version);
    assert!(pending(&w).is_empty());
    w.ok("effects-driver", Command::AcknowledgeEffects { through: 0 });
    assert!(pending(&w).is_empty(), "nothing the rows say changed");

    w.disclose("P1", "L1", &yes("heart_condition"));
    let assessing = pending(&w);
    assert!(assessing[0].version > first[0].version);
    assert_eq!(
        shown(&assessing[0]),
        (json!("open"), json!("insurer"), json!("in_assessment"), json!("insurer"))
    );
    let case = w.take("P1", "L1", "sam");
    w.propose(&case, "sam", vec![decline("life")]).unwrap();
    let countersigning = pending(&w);
    assert!(countersigning[0].version > assessing[0].version);
    published(&mut w, assessing[0].version);
    assert_eq!(pending(&w), countersigning, "a change after the version read is still to go");

    w.take("P1", "L1", "sue");
    w.send(
        "underwriting-console",
        Command::Countersign {
            case,
            underwriter: "sue".into(),
            agree: true,
            reasons: set(&["RC-0101"]),
            withheld_reasons: BTreeSet::new(),
        },
    )
    .unwrap();
    let ended = pending(&w);
    assert_eq!(shown(&ended[0]), (json!("ended"), Value::Null, json!("declined"), Value::Null));
    published(&mut w, ended[0].version);
    assert!(pending(&w).is_empty());
    assert!(
        !serde_json::to_value(&w.s).unwrap()["progress"].as_object().unwrap().contains_key("P1"),
        "an ended proposal is not remembered"
    );
}
