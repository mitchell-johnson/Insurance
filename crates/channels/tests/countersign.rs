//! Countersigned commands in the channel state: one person prepares a
//! command for another module, a different person signs it, and the
//! record of both survives in the replicated state until it is settled.

mod common;

use common::*;
use ic_channels::classes::DataClass;
use ic_channels::countersign::{CountersignCommand as C, MAX_OPEN};
use ic_channels::reinsurance::actions;
use ic_channels::staff::{actions as staff_actions, Profile, StaffCommand, Template};
use ic_channels::Command;
use serde_json::{json, Value};

fn template(id: &str, actions: &[&str]) -> Template {
    Template {
        id: id.to_string(),
        actions: actions.iter().map(|a| a.to_string()).collect(),
        classes: [DataClass::Ordinary, DataClass::Financial].into(),
        default_profile: "none".to_string(),
        ceilings: Default::default(),
        break_glass: false,
    }
}

/// Two operators, a lead who signs, and a reader.
fn staffed() -> World {
    let mut w = World::new();
    w.send(
        module("entitlements"),
        Command::Staff(StaffCommand::Bootstrap {
            templates: vec![
                template("administrator", &[staff_actions::MANAGE_STAFF]),
                template("ops", &[actions::OPERATE]),
                template("lead", &[actions::OPERATE, actions::APPROVE]),
                template("reader", &[actions::VIEW]),
            ],
            profiles: vec![Profile {
                id: "none".into(),
                version: 1,
                limits: Default::default(),
            }],
            admin: p("alice"),
            admin_templates: vec!["administrator".into()],
        }),
    )
    .unwrap();
    for (who, role) in [("op-1", "ops"), ("op-2", "ops"), ("lead-1", "lead"), ("read-1", "reader")] {
        w.send(
            staff("alice"),
            Command::Staff(StaffCommand::Employ {
                party: p(who),
                team: "reinsurance".into(),
                manager: None,
                templates: vec![role.into()],
            }),
        )
        .unwrap();
    }
    w
}

fn prepare(id: &str, command: Value, reason: &str) -> Command {
    Command::Countersign(C::Prepare {
        id: id.into(),
        module: "reinsurance".into(),
        command,
        reason: reason.into(),
    })
}

fn abandon(seq: u64) -> Value {
    json!({ "abandon_notice": { "seq": seq, "reason": "raised in error" } })
}

fn cs(command: C) -> Command {
    Command::Countersign(command)
}

/// Purpose: only a person who may prepare does so, only a command of the module's that needs two people, with a reason,
/// once per ID, and never from a module; a full store takes no more.
/// Why it matters: the store is what the second signature is checked against. Anything that slipped into it unchecked would
/// be sent with an approver's name on it.
#[test]
fn what_may_be_prepared() {
    let mut w = staffed();
    assert_eq!(code(w.send(module("reinsurance"), prepare("x", abandon(1), "r"))), "forbidden");
    assert_eq!(code(w.send(staff("read-1"), prepare("x", abandon(1), "r"))), "forbidden");
    assert_eq!(
        code(w.send(staff("op-1"), prepare("x", json!({ "retry_notice": { "seq": 1 } }), "r"))),
        "bad_command"
    );
    assert_eq!(code(w.send(staff("op-1"), prepare("x", json!("abandon_notice"), "r"))), "bad_command");
    assert_eq!(code(w.send(staff("op-1"), prepare("x", abandon(1), " "))), "reason_required");
    let other = Command::Countersign(C::Prepare {
        id: "x".into(),
        module: "finance".into(),
        command: abandon(1),
        reason: "r".into(),
    });
    assert_eq!(code(w.send(staff("op-1"), other)), "unknown_module");
    w.send(staff("op-1"), prepare("x", abandon(1), "raised in error")).unwrap();
    assert_eq!(code(w.send(staff("op-2"), prepare("x", abandon(2), "r"))), "exists");
    for n in 1..MAX_OPEN {
        w.send(staff("op-1"), prepare(&format!("fill-{n}"), abandon(n as u64), "r")).unwrap();
    }
    assert_eq!(code(w.send(staff("op-1"), prepare("one-more", abandon(9), "r"))), "full");
    assert_eq!(w.ch.data.countersign.waiting("reinsurance").len(), MAX_OPEN);
}

/// Purpose: the second person must differ from the first and hold the signing action; once signed, nobody else can sign or
/// withdraw it, the signer may sign again to finish a send, and only the signer settles it.
/// Why it matters: these are the rules the reinsurance module relies on when it records `approved_by`, and the repeat
/// signature is how a send lost to a leader change is finished without a third person.
#[test]
fn who_signs_and_settles() {
    let mut w = staffed();
    w.send(staff("op-1"), prepare("a", abandon(1), "raised in error")).unwrap();
    assert_eq!(code(w.send(staff("op-1"), cs(C::Sign { id: "a".into() }))), "separation");
    assert_eq!(code(w.send(staff("op-2"), cs(C::Sign { id: "a".into() }))), "forbidden");
    assert_eq!(code(w.send(staff("lead-1"), cs(C::Settle { id: "a".into() }))), "forbidden");
    assert_eq!(code(w.send(staff("lead-1"), cs(C::Sign { id: "nope".into() }))), "not_found");
    w.send(staff("lead-1"), cs(C::Sign { id: "a".into() })).unwrap();
    w.send(staff("lead-1"), cs(C::Sign { id: "a".into() })).unwrap();
    let held = &w.ch.data.countersign.open["a"];
    assert_eq!((held.signed_by.clone(), held.by.clone()), (Some(p("lead-1")), p("op-1")));
    assert_eq!(code(w.send(staff("op-1"), cs(C::Withdraw { id: "a".into() }))), "signed");

    // A second signer, after the first.
    w.send(staff("op-2"), prepare("b", abandon(2), "raised in error")).unwrap();
    w.send(staff("lead-1"), cs(C::Sign { id: "b".into() })).unwrap();
    w.send(
        staff("alice"),
        Command::Staff(StaffCommand::Employ {
            party: p("lead-2"),
            team: "reinsurance".into(),
            manager: None,
            templates: vec!["lead".into()],
        }),
    )
    .unwrap();
    assert_eq!(code(w.send(staff("lead-2"), cs(C::Sign { id: "b".into() }))), "signed");

    assert_eq!(code(w.send(staff("op-1"), cs(C::Settle { id: "a".into() }))), "forbidden");
    w.send(staff("lead-1"), cs(C::Settle { id: "a".into() })).unwrap();
    assert_eq!(code(w.send(staff("lead-1"), cs(C::Settle { id: "a".into() }))), "not_found");
    assert_eq!(code(w.send(staff("op-1"), cs(C::Withdraw { id: "a".into() }))), "not_found");

    // Withdrawn by the one who prepared it, and only by them.
    w.send(staff("op-1"), prepare("c", abandon(3), "raised in error")).unwrap();
    assert_eq!(code(w.send(staff("op-2"), cs(C::Withdraw { id: "c".into() }))), "forbidden");
    w.send(staff("op-1"), cs(C::Withdraw { id: "c".into() })).unwrap();
    assert_eq!(w.ch.data.countersign.waiting("reinsurance").len(), 1);
}
