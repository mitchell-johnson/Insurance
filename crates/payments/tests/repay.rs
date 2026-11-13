//! Money paid back against a claim overpayment (`repay.rs`).

mod common;

use common::*;
use ic_log::StateMachine;
use ic_payments::model::{ExceptionKind, Instruction, StatementLine};
use ic_payments::query::{answer, react};
use ic_payments::Command;
use serde_json::{json, Value};

fn claims_event(id: &str, kind: &str, overpayment: &str, outstanding: i64) -> ic_core::EventRecord {
    let event = json!({"module": {"module": "claims", "kind": kind, "subject": "clm-1", "detail": {
        "claim": "clm-1", "policy": "pol-1", "overpayment": overpayment, "outstanding_minor": outstanding.to_string(),
        "status": "pursued", "party": "party-1"
    }}});
    serde_json::from_value(json!({"id": id, "partition": 1, "seq": 1, "business_date": "2027-11-15", "log_index": 1, "event": event})).unwrap()
}

fn hear(desk: &mut Desk, record: &ic_core::EventRecord) {
    for envelope in react(record) {
        desk.sent += 1;
        desk.state.apply(desk.sent, envelope).unwrap();
    }
}

fn credit(id: &str, amount: i64, reference: &str) -> StatementLine {
    StatementLine {
        id: id.to_string(),
        amount_minor: amount,
        reference: reference.to_string(),
        payer_account: Some("0601111111".to_string()),
    }
}

fn reference(desk: &Desk, overpayment: &str) -> Value {
    answer(&desk.state, json!({"q": "repayment_reference", "overpayment": overpayment})).unwrap()
}

/// What the outbox asks of claims, in order: (overpayment, amount).
fn repayments(desk: &Desk) -> Vec<(String, i64)> {
    desk.state
        .outbox
        .values()
        .filter_map(|e| match &e.instruction {
            Instruction::CoreAt { command, .. } => match command.as_ref() {
                ic_core::Command::Claims(c) => match c.as_ref() {
                    ic_core::claims_module::Command::RecordRepayment { overpayment, amount_minor, .. } => Some((overpayment.clone(), *amount_minor)),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Purpose: while claims says money is owed on an overpayment it has a
/// short reference; a credit quoting it, however spaced, for no more than is
/// owed is sent to claims as a repayment and resolved when claims records
/// it; more than is owed, or a credit naming two overpayments, waits for a
/// person or goes to cash matching; once nothing is owed the reference is
/// forgotten. Only the service records what claims says.
/// Why it matters: an overpaid customer who pays back must have the money
/// taken off what they owe, not left as an unidentified receipt while
/// claims keeps chasing them. Taking more than is owed, or guessing between
/// two debts, would misstate what they still owe.
#[test]
fn a_repayment_quoting_its_reference_goes_to_claims() {
    let mut desk = Desk::new();
    let by_hand = Command::OverpaymentChanged {
        claim: "clm-1".into(),
        overpayment: "clm-1/overpayment/1".into(),
        policy: "pol-1".into(),
        outstanding_minor: 500_00,
    };
    assert_eq!(desk.code(&rae(), by_hand), "not_permitted");
    let blank = Command::OverpaymentChanged {
        claim: "clm-1".into(),
        overpayment: String::new(),
        policy: "pol-1".into(),
        outstanding_minor: 500_00,
    };
    assert_eq!(desk.code(&svc(), blank), "invalid_overpayment");

    hear(&mut desk, &claims_event("1-1", "overpayment_raised", "clm-1/overpayment/1", 500_00));
    hear(&mut desk, &claims_event("1-2", "overpayment_raised", "clm-1/overpayment/2", 80_00));
    hear(&mut desk, &claims_event("1-3", "overpayment_status_changed", "clm-1/overpayment/1", 500_00));
    assert_eq!(reference(&desk, "clm-1/overpayment/1"), json!("OP000001"));
    assert_eq!(reference(&desk, "clm-1/overpayment/2"), json!("OP000002"));

    let a = "anz-sf1-collection";
    let open = 10_000_000_00;
    let lines = vec![
        credit("l1", 300_00, "op 000001 smith"),
        credit("l2", 90_00, "OP000002"),
        credit("l3", 10_00, "OP000001 OP000002"),
    ];
    let total = lines.iter().map(|l| l.amount_minor).sum::<i64>();
    desk.ok(
        &svc(),
        Command::RecordStatement {
            account: a.into(),
            date: d(2027, 11, 15),
            opening_minor: open,
            closing_minor: open + total,
            lines,
        },
    );
    assert_eq!(repayments(&desk), [("clm-1/overpayment/1".to_string(), 300_00)]);
    let over = &desk.state.exceptions[&format!("{a}-2027-11-15-l2")];
    assert_eq!((over.kind, over.resolution.as_ref()), (ExceptionKind::UnidentifiedReceipt, None));
    assert!(desk
        .state
        .outbox
        .values()
        .any(|e| matches!(&e.instruction, Instruction::Match { reference, .. } if reference == "OP000001 OP000002")));

    let entry = *desk
        .state
        .outbox
        .iter()
        .find(|(_, e)| matches!(e.instruction, Instruction::CoreAt { .. }))
        .unwrap()
        .0;
    desk.ok(&svc(), Command::CompleteInstruction { entry, refused: None });
    let paid = &desk.state.exceptions[&format!("{a}-2027-11-15-l1")];
    assert_eq!(paid.resolution.as_deref(), Some("recorded by claims against its overpayment"));

    hear(&mut desk, &claims_event("1-4", "overpayment_recovered", "clm-1/overpayment/1", 0));
    assert_eq!(reference(&desk, "clm-1/overpayment/1"), Value::Null);
    assert!(desk.state.repayables.named_by("OP000001").is_none());
    assert!(desk.state.repayables.named_by("OP000002").is_some());
}

fn core_event(id: &str, event: Value) -> ic_core::EventRecord {
    serde_json::from_value(json!({"id": id, "partition": 1, "seq": 1, "business_date": "2027-11-15", "log_index": 1, "event": event})).unwrap()
}

/// Purpose: while the core says a policy loan is owed it has a reference;
/// a credit quoting it is sent to the core as cash matching's loan
/// repayment, whatever the amount (the core knows today's interest), and
/// resolved when the core applies it; a loan repaid in full or written off
/// loses its reference. Only the service records what the core says.
/// Why it matters: an owner paying down a policy loan by direct credit must
/// have it taken off the loan, not left as unidentified money while
/// interest keeps accruing on a debt they have paid.
#[test]
fn a_loan_repayment_quoting_its_reference_goes_to_the_core() {
    let mut desk = Desk::new();
    let by_hand = Command::LoanChanged {
        policy: "pol-1".into(),
        outstanding_minor: 1,
    };
    assert_eq!(desk.code(&rae(), by_hand), "not_permitted");
    let blank = Command::LoanChanged {
        policy: String::new(),
        outstanding_minor: 1,
    };
    assert_eq!(desk.code(&svc(), blank), "invalid_loan");

    let drawn = json!({"policy_loan_drawn": {"policy": "pol-1", "reference": "ln-1", "amount_minor": 5_000_00, "outstanding_minor": 5_000_00}});
    hear(&mut desk, &core_event("1-1", drawn));
    let drawn = json!({"policy_loan_drawn": {"policy": "pol-2", "reference": "ln-2", "amount_minor": 900_00, "outstanding_minor": 900_00}});
    hear(&mut desk, &core_event("1-2", drawn));
    let reference = |desk: &Desk, policy: &str| answer(&desk.state, json!({"q": "loan_repayment_reference", "policy": policy})).unwrap();
    assert_eq!(reference(&desk, "pol-1"), json!("LN000001"));

    let a = "anz-sf1-collection";
    let open = 10_000_000_00;
    desk.ok(
        &svc(),
        Command::RecordStatement {
            account: a.into(),
            date: d(2027, 11, 15),
            opening_minor: open,
            closing_minor: open + 5_010_00,
            lines: vec![credit("l1", 5_010_00, "LN 000001")],
        },
    );
    let (entry, sent) = desk.state.outbox.iter().next().map(|(k, e)| (*k, e.instruction.clone())).unwrap();
    let id = format!("{a}-2027-11-15-l1");
    assert_eq!(
        sent,
        Instruction::CoreAt {
            policy: policy("pol-1"),
            caller: "cash-matching".into(),
            command: Box::new(ic_core::Command::Loan(Box::new(ic_core::loan::LoanCommand::Repay {
                policy: policy("pol-1"),
                receipt: id.clone(),
                amount: ic_money::Money::new(5_010_00, ic_money::Currency::NZD),
            }))),
        }
    );
    desk.ok(&svc(), Command::CompleteInstruction { entry, refused: None });
    assert_eq!(desk.state.exceptions[&id].resolution.as_deref(), Some("applied to the policy loan"));

    let repaid = json!({"policy_loan_repaid": {"policy": "pol-1", "reference": id, "amount_minor": 5_010_00, "interest_minor": 10_00, "principal_minor": 5_000_00, "outstanding_minor": 0, "from": "receipt"}});
    hear(&mut desk, &core_event("1-3", repaid));
    assert_eq!(reference(&desk, "pol-1"), Value::Null);
    let written_off = json!({"policy_loan_written_off": {"policy": "pol-2", "reference": "sur-1", "amount_minor": 900_00, "cause": "surrender"}});
    hear(&mut desk, &core_event("1-4", written_off));
    assert_eq!(reference(&desk, "pol-2"), Value::Null);
}
