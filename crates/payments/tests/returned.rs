//! Money received that goes back (GRP-015, `returned.rs`): an employer's
//! remittance paid twice is recalled by its bank as a duplicate.

mod common;

use common::*;
use ic_log::StateMachine;
use ic_payments::model::Instruction;
use ic_payments::query::react;
use ic_payments::{Command, Event};
use serde_json::json;

fn applied(desk: &mut Desk, id: &str, policy: &str, receipt: &str) {
    let event = json!({"receipt_applied": {"policy": policy, "receipt": receipt, "allocated_minor": 600_00, "to_suspense_minor": 0, "fund": "sf1"}});
    let record = serde_json::from_value(json!({"id": id, "partition": 1, "seq": 1, "business_date": "2027-11-15", "log_index": 1, "event": event})).unwrap();
    for envelope in react(&record) {
        desk.sent += 1;
        desk.state.apply(desk.sent, envelope).unwrap();
    }
}

fn give_back(exception: &str, reason: &str) -> Command {
    Command::ReturnCredit {
        exception: exception.to_string(),
        reason: reason.to_string(),
    }
}

/// Purpose: a remittance that cash matching applied across a scheme's
/// policies is given back by a reconciler: each policy is told to reverse
/// that receipt, the bank's debit is expected under the credit's own
/// reference and reconciles, and the same credit cannot be given back
/// twice; a debit, an unknown exception and a missing reason are refused.
/// Why it matters: a recalled duplicate that stays applied leaves members'
/// cover paid with money the insurer no longer has; reversing it by new
/// entries lets the core work out standing from the original due dates.
#[test]
fn a_remittance_recalled_as_a_duplicate_is_given_back_and_reversed() {
    let mut desk = Desk::new();
    let opening = desk.state.books["anz-sf1-collection"].closing_minor;
    desk.statement(
        "anz-sf1-collection",
        d(2027, 11, 15),
        opening,
        &[("l1", 1_200_00, "ACME INV-1 INV-2"), ("l2", -5_00, "fee")],
    );
    let credit = "anz-sf1-collection-2027-11-15-l1";
    while let Some(&entry) = desk.state.outbox.keys().next() {
        desk.ok(&svc(), Command::CompleteInstruction { entry, refused: None });
    }
    let receipt = format!("match-{credit}");
    applied(&mut desk, "1-1", "grp-1", &receipt);
    applied(&mut desk, "1-2", "grp-2", &receipt);

    assert_eq!(desk.code(&mia(), give_back(credit, "duplicate")), "not_permitted");
    assert_eq!(desk.code(&rae(), give_back("nope", "duplicate")), "unknown_exception");
    assert_eq!(desk.code(&rae(), give_back("anz-sf1-collection-2027-11-15-l2", "duplicate")), "invalid_return");
    assert_eq!(desk.code(&rae(), give_back(credit, " ")), "invalid_return");
    let events = desk.ok(&rae(), give_back(credit, "recalled by the payer's bank as a duplicate"));
    assert!(events.contains(&Event::CreditReturned {
        exception: credit.to_string(),
        policies: vec!["grp-1".to_string(), "grp-2".to_string()],
    }));
    let reversals: Vec<_> = desk
        .state
        .outbox
        .values()
        .map(|e| match &e.instruction {
            Instruction::Core { caller, command } => (e.key.clone(), caller.clone(), serde_json::to_value(command).unwrap()),
            other => panic!("only reversals: {other:?}"),
        })
        .collect();
    assert_eq!(reversals.len(), 2);
    assert_eq!(reversals[0].0, format!("credit-returned-{credit}-grp-1"));
    assert_eq!(reversals[0].1, "cash-matching");
    assert_eq!(reversals[1].2, json!({"reverse_receipt": {"policy": "grp-2", "receipt": receipt}}));
    assert_eq!(desk.code(&rae(), give_back(credit, "again")), "already_returned");

    let opening = desk.state.books["anz-sf1-collection"].closing_minor;
    desk.statement("anz-sf1-collection", d(2027, 11, 16), opening, &[("l3", -1_200_00, "ACME INV-1 INV-2")]);
    assert!(
        !desk.state.exceptions.contains_key("anz-sf1-collection-2027-11-16-l3"),
        "the debit was expected"
    );
}
