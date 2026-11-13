//! A policy owner's bankruptcy (decision 4, `JNT-029`): the core records the
//! insolvency interest; this module freezes the owner's payout accounts and
//! stops value going out to them while it stands.

mod common;

use common::*;
use ic_core::PayableKind;
use ic_log::StateMachine;
use ic_payments::model::{ItemStatus, NameCheck, NewItem, PayeeKind, RunType};
use ic_payments::query::{answer, react};
use ic_payments::{Command, Event};
use serde_json::{json, Value};

fn core_event(id: &str, event: Value) -> ic_core::EventRecord {
    serde_json::from_value(json!({"id": id, "partition": 1, "seq": 1, "business_date": "2027-11-15", "log_index": 1, "event": event})).unwrap()
}

fn recorded(policy: &str, interest: &str, kind: &str, owner: Option<&str>) -> Value {
    json!({"interest": {"recorded": {
        "policy": policy, "interest": interest, "kind": kind, "owner": owner, "party": "official-assignee",
        "notice_received": "2027-11-10", "extent_minor": null, "effective_from": "2027-11-10"
    }}})
}

fn released(policy: &str, interest: &str, owner: &str, effective: &str) -> Value {
    json!({"interest": {"released": {
        "policy": policy, "interest": interest, "kind": "insolvency", "owner": owner, "party": "official-assignee",
        "effective": effective
    }}})
}

/// Take a core event into the desk's state as the host would.
fn hear(desk: &mut Desk, id: &str, event: Value) -> Vec<Event> {
    let mut events = Vec::new();
    for envelope in react(&core_event(id, event)) {
        events.extend(desk.state.apply(desk.sent + 1, envelope).unwrap());
        desk.sent += 1;
    }
    events
}

/// A unit-linked withdrawal paid to a payee.
fn payout(item: &str, payee: &str, amount: i64) -> NewItem {
    NewItem {
        source: format!("payout:pol-1:{item}"),
        policy: Some(policy("pol-1")),
        payable: Some(PayableKind::Benefit),
        ..pay(item, payee, amount)
    }
}

/// A claim payment to a payee.
fn claim(item: &str, payee: &str, amount: i64) -> NewItem {
    NewItem {
        source: format!("claim:cl-1:{item}"),
        policy: Some(policy("pol-1")),
        payable: Some(PayableKind::Benefit),
        ..pay(item, payee, amount)
    }
}

fn new_account() -> Command {
    Command::ChangePayeeAccount {
        payee: "p1".to_string(),
        account: account("p1", "0109999999"),
        name_check: NameCheck::Match,
    }
}

/// Purpose: only an insolvency interest that names the policy owner reaches
/// payments; a lender's assignment, and an old event without the owner, do
/// not. A release carries its own effective date.
/// Why it matters: an assignment restricts the core's own commands and must
/// not freeze a customer's accounts here; and an event that cannot say whose
/// accounts to freeze must not freeze someone else's.
#[test]
fn only_an_owners_insolvency_is_taken_in() {
    let commands = react(&core_event("1-1", recorded("pol-1", "int-1", "insolvency", Some("party-p1"))));
    assert_eq!(
        commands.iter().map(|e| &e.command).collect::<Vec<_>>(),
        [&Command::RecordInsolvency {
            owner: "party-p1".to_string(),
            policy: "pol-1".to_string(),
            interest: "int-1".to_string(),
            released: None,
        }]
    );
    let commands = react(&core_event("1-2", released("pol-1", "int-1", "party-p1", "2027-12-01")));
    assert!(matches!(&commands[0].command, Command::RecordInsolvency { released: Some(on), .. } if *on == d(2027, 12, 1)));
    assert!(react(&core_event("1-3", recorded("pol-1", "int-2", "assignment", Some("party-p1")))).is_empty());
    assert!(react(&core_event("1-4", recorded("pol-1", "int-3", "insolvency", None))).is_empty());
}

/// Purpose: while an insolvency interest names a party as owner, a payee
/// cannot be registered for them nor a payee's account changed, and a
/// withdrawal to them cannot join a run, but a claim payment can; only the
/// service role records the interest, and a change is an event and shows on
/// the party question.
/// Why it matters: decision 4 stops value leaving a bankrupt's policies
/// until legal says to whom it belongs. A changed payout account is how it
/// would leave unnoticed. Claims carry on, and the claims module holds its
/// own money.
#[test]
fn a_bankrupt_owners_accounts_are_frozen() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    let by_hand = Command::RecordInsolvency {
        owner: "party-p1".to_string(),
        policy: "pol-1".to_string(),
        interest: "int-1".to_string(),
        released: None,
    };
    assert_eq!(desk.code(&pam(), by_hand), "not_permitted");
    let blank = Command::RecordInsolvency {
        owner: String::new(),
        policy: "pol-1".to_string(),
        interest: "int-1".to_string(),
        released: None,
    };
    assert_eq!(desk.code(&svc(), blank), "invalid_interest");

    let events = hear(&mut desk, "1-1", recorded("pol-1", "int-1", "insolvency", Some("party-p1")));
    assert_eq!(
        events,
        [Event::OwnerInsolvencyChanged {
            party: "party-p1".to_string(),
            insolvent: true,
        }]
    );
    assert_eq!(desk.code(&pam(), new_account()), "owner_insolvent");
    let second = Command::RegisterPayee {
        payee: "p1b".to_string(),
        party: "party-p1".to_string(),
        kind: PayeeKind::Customer,
        account: account("p1", "0108888888"),
        name_check: NameCheck::Match,
    };
    assert_eq!(desk.code(&pam(), second), "owner_insolvent");

    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    let add = Command::AddRunItems {
        run: "r1".to_string(),
        items: vec![payout("w1", "p1", 500_00)],
    };
    assert_eq!(desk.code(&mia(), add), "owner_insolvent");
    desk.add("r1", vec![claim("c1", "p1", 900_00)]);

    let party = answer(&desk.state, json!({"q": "party", "party": "party-p1"})).unwrap();
    assert_eq!(party["insolvency"]["interests"], json!({"pol-1/int-1": null}));
}

/// Purpose: the freeze lasts while any of the owner's insolvency interests
/// stands: one policy's release leaves the other's in force, a release dated
/// ahead takes effect on its day, and only then may the account change.
/// Why it matters: the Official Assignee releases policies one at a time,
/// sometimes from a future date. Ending the freeze at the first release, or
/// on the day it was recorded, would let value out early.
#[test]
fn the_freeze_ends_when_the_last_release_takes_effect() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    hear(&mut desk, "1-1", recorded("pol-1", "int-1", "insolvency", Some("party-p1")));
    hear(&mut desk, "1-2", recorded("pol-2", "int-9", "insolvency", Some("party-p1")));
    assert!(hear(&mut desk, "1-3", released("pol-1", "int-1", "party-p1", "2027-11-01")).is_empty());
    assert_eq!(desk.code(&pam(), new_account()), "owner_insolvent");
    assert!(hear(&mut desk, "1-4", released("pol-2", "int-9", "party-p1", "2027-11-20")).is_empty());
    assert_eq!(desk.code(&pam(), new_account()), "owner_insolvent");
    assert!(desk.state.insolvent("party-p1", d(2027, 11, 19)));
    assert!(!desk.state.insolvent("party-p1", d(2027, 11, 20)));

    desk.at(t(2027, 11, 20, 9, 0));
    desk.ok(&pam(), new_account());
    // The next record forgets releases already in effect.
    hear(&mut desk, "1-5", released("pol-2", "int-9", "party-p1", "2027-11-20"));
    assert!(!desk.state.insolvencies.contains_key("party-p1"));
}

/// Purpose: a withdrawal that joined a run before the bankruptcy was heard
/// is held at the check (`owner_insolvent`) and not sent.
/// Why it matters: a run is prepared hours before it is approved. The
/// Official Assignee's notice can arrive in between, and the money must not
/// leave because the run was already built.
#[test]
fn a_bankruptcy_heard_after_the_item_joined_holds_it_at_the_check() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![payout("w1", "p1", 500_00)]);
    hear(&mut desk, "1-1", recorded("pol-1", "int-1", "insolvency", Some("party-p1")));
    desk.ok(&mia(), Command::CheckRun { run: "r1".to_string() });
    assert_eq!(desk.state.runs["r1"].items["w1"].status, ItemStatus::Held("owner_insolvent".to_string()));
}
