//! Money nobody could identify, applied by a person (COL-020, COL-022).

mod common;

use common::*;
use ic_payments::model::{Instruction, Role, StatementLine};
use ic_payments::query::answer;
use ic_payments::{Command, Event};
use serde_json::json;

fn rex() -> ic_payments::Actor {
    person("rex", &[Role::Reconciler])
}

/// Read one credit nobody can identify and return its exception ID.
fn unidentified(desk: &mut Desk, line: &str, amount: i64) -> String {
    let a = "anz-sf1-collection";
    let opening = desk.state.books.get(a).map_or(10_000_000_00, |b| b.closing_minor);
    let date = desk
        .state
        .books
        .get(a)
        .and_then(|b| b.statement_date)
        .map_or(d(2027, 11, 15), |d| d.succ_opt().unwrap());
    desk.ok(
        &svc(),
        Command::RecordStatement {
            account: a.into(),
            date,
            opening_minor: opening,
            closing_minor: opening + amount,
            lines: vec![StatementLine {
                id: line.into(),
                amount_minor: amount,
                reference: "family premiums".into(),
                payer_account: Some("0601111111".into()),
            }],
        },
    );
    // Cash matching could not say whose it is.
    let entry = *desk.state.outbox.keys().next().unwrap();
    desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry,
            refused: Some("no_match".into()),
        },
    );
    format!("{a}-{date}-{line}")
}

fn parts(desk: &Desk) -> Vec<(String, i64)> {
    desk.state
        .outbox
        .values()
        .filter_map(|e| match &e.instruction {
            Instruction::CoreAt { command, caller, .. } if caller == "cash-matching" => match command.as_ref() {
                ic_core::Command::ApplyReceipt { receipt, amount, .. } => Some((receipt.clone(), amount.minor)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Purpose: one transfer is split by a reconciler across the four policies
/// it pays; the parts must name each policy once and add up to the
/// receipt; at or above the threshold a second reconciler must approve and
/// the proposer cannot; each part goes to the core as its own receipt and
/// the exception is resolved only when all are applied, and what the core
/// refuses stays open to be proposed again; the payer is
/// remembered for each part so the payer-not-owner rule sees every policy.
/// Why it matters: a grandmother paying four grandchildren's premiums in
/// one transfer must have each paid, not the whole sum sit in suspense; and
/// putting someone's money on a policy is a payment decision that needs two
/// people once it is large.
#[test]
fn a_receipt_is_split_across_policies_with_a_second_person() {
    let mut desk = Desk::new();
    let exception = unidentified(&mut desk, "l1", 2_000_00);
    let split = |amounts: &[(&str, i64)]| Command::ProposeReceiptMatch {
        exception: exception.clone(),
        parts: amounts.iter().map(|(p, a)| (policy(p), *a)).collect(),
    };
    assert_eq!(desk.code(&mia(), split(&[("pol-1", 2_000_00)])), "not_permitted");
    assert_eq!(desk.code(&rae(), split(&[("pol-1", 1_000_00)])), "invalid_match", "must add up");
    assert_eq!(
        desk.code(&rae(), split(&[("pol-1", 1_000_00), ("pol-1", 1_000_00)])),
        "invalid_match",
        "each policy once"
    );
    assert_eq!(desk.code(&rae(), split(&[("pol-1", 2_100_00), ("pol-2", -100_00)])), "invalid_match");
    let unknown = Command::ProposeReceiptMatch {
        exception: "nothing".into(),
        parts: vec![(policy("pol-1"), 1)],
    };
    assert_eq!(desk.code(&rae(), unknown), "unknown_exception");

    let four = [("pol-1", 500_00), ("pol-2", 500_00), ("pol-3", 600_00), ("pol-4", 400_00)];
    assert_eq!(desk.ok(&rae(), split(&four)), [Event::ReceiptMatchProposed { exception: exception.clone() }]);
    assert!(parts(&desk).is_empty(), "waits for a second person");
    let waiting = answer(&desk.state, json!({"q": "matches_to_approve"})).unwrap();
    assert_eq!(waiting[&exception]["by"], "rae");
    let approve = Command::ApproveReceiptMatch { exception: exception.clone() };
    assert_eq!(desk.code(&rae(), approve.clone()), "same_person");
    assert_eq!(desk.ok(&rex(), approve.clone()), [Event::ReceiptMatchApproved { exception: exception.clone() }]);
    assert_eq!(desk.code(&rex(), approve), "match_approved");
    assert_eq!(desk.code(&rae(), split(&four)), "match_approved");
    let sent = parts(&desk);
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[0], (format!("{exception}/1"), 500_00));
    assert!(desk.state.payer_checks.pending.contains_key(&format!("{exception}/3")));

    // The core refuses one part: the exception stays open and reconcilers hear.
    let entries: Vec<u64> = desk
        .state
        .outbox
        .iter()
        .filter(|(_, e)| e.key.starts_with("manual-match-"))
        .map(|(k, _)| *k)
        .collect();
    desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: entries[0],
            refused: Some("policy_not_in_force".into()),
        },
    );
    assert!(desk.state.exceptions[&exception].resolution.is_none());
    assert!(answer(&desk.state, json!({"q": "notices", "role": "reconciler"}))
        .unwrap()
        .to_string()
        .contains("manual_match_refused"));
    for entry in &entries[1..3] {
        desk.ok(&svc(), Command::CompleteInstruction { entry: *entry, refused: None });
    }
    assert!(desk.state.exceptions[&exception].resolution.is_none(), "one part is still with the core");
    desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: entries[3],
            refused: None,
        },
    );
    // What the core refused stays open, and is proposed again.
    assert!(answer(&desk.state, json!({"q": "manual_match", "exception": exception})).unwrap().is_null());
    assert_eq!(desk.state.exceptions[&exception].amount_minor, 500_00);
    assert!(desk.state.exceptions[&exception].resolution.is_none());
    desk.ok(&rae(), split(&[("pol-5", 500_00)]));
    let entry = *desk.state.outbox.iter().find(|(_, e)| e.key.starts_with("manual-match-")).unwrap().0;
    let last = desk.ok(&svc(), Command::CompleteInstruction { entry, refused: None });
    assert!(last.iter().any(|e| matches!(e, Event::ExceptionResolved { .. })));
}

/// Purpose: below the threshold one reconciler's match is applied at once;
/// the threshold is a setting that must be positive; only money received
/// can be applied to a policy.
/// Why it matters: small receipts are the bulk of suspense work and need
/// not wait for two people, but nobody should be able to switch the second
/// person off by setting the threshold to nothing.
#[test]
fn a_small_receipt_is_applied_by_one_person() {
    let mut desk = Desk::new();
    let exception = unidentified(&mut desk, "l1", 150_00);
    let one = Command::ProposeReceiptMatch {
        exception: exception.clone(),
        parts: vec![(policy("pol-1"), 150_00)],
    };
    let events = desk.ok(&rae(), one);
    assert!(events.contains(&Event::ReceiptMatchApproved { exception: exception.clone() }));
    assert_eq!(parts(&desk), [(format!("{exception}/1"), 150_00)]);

    let mut c = config();
    c.rules.second_person_match_minor = Some(0);
    let activate = Command::ActivateConfig {
        version: 2,
        config: Box::new(c),
    };
    assert_eq!(desk.code(&cal(), activate), "invalid_config");

    let a = "anz-sf1-collection";
    desk.ok(
        &svc(),
        Command::RecordStatement {
            account: a.into(),
            date: d(2027, 11, 16),
            opening_minor: desk.state.books[a].closing_minor,
            closing_minor: desk.state.books[a].closing_minor - 5_00,
            lines: vec![StatementLine {
                id: "fee".into(),
                amount_minor: -5_00,
                reference: "bank fee".into(),
                payer_account: None,
            }],
        },
    );
    let debit = Command::ProposeReceiptMatch {
        exception: format!("{a}-2027-11-16-fee"),
        parts: vec![(policy("pol-1"), 5_00)],
    };
    assert_eq!(desk.code(&rae(), debit), "invalid_match");
}
