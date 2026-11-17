//! A payment that went into the wrong account (CLM-022, `recall.rs`): the
//! bank is asked to recall it, the core holds what the holder owes and owes
//! the right person again, collections arranges repayment as a case, and
//! nothing is ever taken from another payment.

mod common;

use common::*;
use ic_core::PayableKind;
use ic_log::StateMachine;
use ic_payments::file::BankMessage;
use ic_payments::model::{AuthorityStatus, Counterparty, Instruction, ItemStatus, NewItem, PayeeKind, RefundDestination, RefundStatus, ReturnReason, RunType};
use ic_payments::query::{answer, react};
use ic_payments::recall::RecallStatus;
use ic_payments::{Command, Event};
use serde_json::{json, Value};

fn core_event(id: &str, event: Value) -> ic_core::EventRecord {
    serde_json::from_value(json!({"id": id, "partition": 1, "seq": 1, "business_date": "2027-11-15", "log_index": 1, "event": event})).unwrap()
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

/// Answer waiting instructions as done, in order, until one whose key
/// starts with `refuse` is refused, or none is left.
fn drain(desk: &mut Desk, refuse: &str) {
    while let Some((&entry, e)) = desk.state.outbox.iter().next() {
        let refused = (!refuse.is_empty() && e.key.starts_with(refuse)).then(|| "refused_here".to_string());
        let stop = refused.is_some();
        desk.ok(&svc(), Command::CompleteInstruction { entry, refused });
        if stop {
            return;
        }
    }
}

fn waiting(desk: &Desk) -> Vec<(String, Instruction)> {
    desk.state.outbox.values().map(|e| (e.key.clone(), e.instruction.clone())).collect()
}

fn told<'a>(work: &'a [(String, Instruction)], subject: &str) -> Option<&'a String> {
    work.iter().find_map(|(_, i)| match i {
        Instruction::Notify { subject: s, detail, .. } if s == subject => Some(detail),
        _ => None,
    })
}

fn report(id: &str, item: &str, amount: i64, note: &str) -> Command {
    Command::ReportMisdirected {
        misdirection: id.to_string(),
        item: item.to_string(),
        amount_minor: amount,
        holder: Some("party-x".to_string()),
        note: note.to_string(),
    }
}

fn debt(kind: &str, outstanding: i64) -> Value {
    let mut body = json!({"policy": "pol-1", "debt": "md-1", "amount_minor": 1, "outstanding_minor": outstanding});
    match kind {
        "payee_debt_raised" => {
            body["payment"] = json!("i1");
            body["payable"] = json!("refund");
            body["refund"] = json!("rf1");
            body["party"] = json!("party-x");
        }
        "payee_debt_recovered" => body["receipt"] = json!("r"),
        _ => body["reason"] = json!("holder_untraceable"),
    }
    json!({ kind: body })
}

fn credit(desk: &mut Desk, day: u32, line: &str, amount: i64, reference: &str) {
    let opening = desk.state.books.get("anz-sf1-collection").map_or(0, |b| b.closing_minor);
    desk.statement("anz-sf1-collection", d(2027, 11, day), opening, &[(line, amount, reference)]);
}

/// Purpose: a premium refund paid to the wrong account is reported, the
/// bank is asked to recall it under the case's ID, the core is asked to
/// hold the debt, the source authority is suspended, collections gets a case
/// with a payment reference, and the refund waits to be paid again. A
/// refused recall tells collections; money quoting the reference is
/// recovered against the debt, more than is owed waits for a person, and an
/// approver other than the reporter writes off the rest, closing it.
/// Why it matters: this is CLM-022 end to end. The customer is still owed
/// their refund, the money is chased from whoever holds it, and the ledger
/// never nets one person's debt from another's payment.
#[test]
fn a_refund_paid_to_the_wrong_account_is_recalled_owed_again_and_chased() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", false);
    desk.ok(
        &mia(),
        Command::RequestRefund {
            refund: "rf1".to_string(),
            policy: policy("pol-1"),
            amount_minor: 70_00,
            currency: NZD,
            destination: RefundDestination::Source { authority: "a1".to_string() },
        },
    );
    drain(&mut desk, "");
    desk.prepare("r1", RunType::PremiumRefunds, d(2027, 11, 15));
    let item = NewItem {
        item: "i1".to_string(),
        source: "refund-rf1".to_string(),
        counterparty: Counterparty::Authority("a1".to_string()),
        amount_minor: 70_00,
        policy: Some(policy("pol-1")),
        payable: Some(PayableKind::Refund),
        refund: Some("rf1".to_string()),
        represents: None,
    };
    desk.add("r1", vec![item]);
    let s1 = desk.approve("r1");
    desk.settle(&s1, d(2027, 11, 15));
    drain(&mut desk, "");

    assert_eq!(desk.code(&mia(), report("md-1", "i1", 70_00, "customer rang")), "not_permitted");
    assert_eq!(desk.code(&rae(), report("md 1", "i1", 70_00, "customer rang")), "invalid_misdirection");
    assert_eq!(desk.code(&rae(), report("md-1", "i1", 70_00, " ")), "invalid_misdirection");
    assert_eq!(desk.code(&rae(), report("md-1", "i9", 70_00, "customer rang")), "unknown_item");
    assert_eq!(desk.code(&rae(), report("md-1", "i1", 70_01, "customer rang")), "invalid_amount");
    assert_eq!(desk.code(&rae(), report("md-1", "i1", 0, "customer rang")), "invalid_amount");

    let events = desk.ok(&rae(), report("md-1", "i1", 70_00, "customer rang: not their account"));
    assert!(events.contains(&Event::MisdirectionReported {
        misdirection: "md-1".to_string(),
        item: "i1".to_string(),
        amount_minor: 70_00
    }));
    assert_eq!(desk.code(&rae(), report("md-1", "i1", 70_00, "again")), "duplicate_misdirection");
    assert_eq!(desk.code(&rae(), report("md-2", "i1", 10_00, "again")), "already_reported");
    let a1 = &desk.state.authorities["a1"];
    assert_eq!((a1.status, a1.reason.as_deref()), (AuthorityStatus::Suspended, Some("misdirected_payment")));
    let work = waiting(&desk);
    let Instruction::CoreAt { caller, command, .. } = &work[0].1 else {
        panic!("the debt goes to the core")
    };
    assert_eq!((work[0].0.as_str(), caller.as_str()), ("payee-debt-md-1", "payments"));
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"payee_debt": {"kind": "raise", "policy": "pol-1", "debt": "md-1", "payment": "i1",
               "amount": {"minor": 70_00, "currency": "NZD"}, "party": "party-x"}})
    );
    let Instruction::Recall { recall } = &work[1].1 else {
        panic!("the bank is asked to recall it")
    };
    assert_eq!(
        (recall.id.as_str(), recall.submission.as_str(), recall.item.as_str()),
        ("md-1", s1.as_str(), "i1")
    );
    assert_eq!(
        (recall.amount_minor, recall.reason.as_str(), recall.settled_on),
        (70_00, "AC03", d(2027, 11, 15))
    );
    assert!(told(&work, "payee_debt_case").unwrap().contains("RP000001"));
    assert!(told(&work, "refund_owed_again").is_some());
    let reference = answer(&desk.state, json!({"q": "misdirection_reference", "misdirection": "md-1"})).unwrap();
    assert_eq!(reference, json!("RP000001"));

    drain(&mut desk, "");
    assert_eq!(
        desk.state.refunds["rf1"].status,
        RefundStatus::Raised,
        "owed again, to be paid once the account is right"
    );
    assert_eq!(desk.state.misdirections["md-1"].recall, RecallStatus::Asked);
    hear(&mut desk, "1-1", debt("payee_debt_raised", 70_00));

    let refused = BankMessage::RecallAnswer {
        recall: "md-1".to_string(),
        accepted: false,
        reason: Some("NOAS".to_string()),
    };
    assert_eq!(
        desk.code(
            &svc(),
            Command::RecordRecallAnswer {
                recall: "md-9".to_string(),
                accepted: true,
                reason: None
            }
        ),
        "unknown_recall"
    );
    let events = desk.ok(&svc(), refused.command());
    assert!(events.contains(&Event::RecallAnswered {
        misdirection: "md-1".to_string(),
        accepted: false
    }));
    assert_eq!(desk.state.misdirections["md-1"].recall, RecallStatus::Refused("NOAS".to_string()));
    assert!(told(&waiting(&desk), "recall_refused").unwrap().contains("RP000001"));
    drain(&mut desk, "");

    // The holder pays back 30.00 quoting the reference.
    credit(&mut desk, 16, "l1", 30_00, "rp 000001 nikau");
    let work = waiting(&desk);
    let Instruction::CoreAt { command, .. } = &work[0].1 else {
        panic!("recovered through the core")
    };
    assert_eq!(work[0].0, "debt-repaid-anz-sf1-collection-2027-11-16-l1");
    assert_eq!(serde_json::to_value(command).unwrap()["payee_debt"]["kind"], "recover");
    drain(&mut desk, "");
    let resolved = desk.state.exceptions["anz-sf1-collection-2027-11-16-l1"].resolution.as_deref();
    assert_eq!(resolved, Some("recovered against the payee debt"));
    hear(&mut desk, "1-2", debt("payee_debt_recovered", 40_00));
    assert_eq!(desk.state.misdirections["md-1"].outstanding_minor, 40_00);

    // More than is owed waits for a person.
    credit(&mut desk, 17, "l2", 50_00, "RP000001");
    assert!(waiting(&desk).is_empty());
    assert!(desk.state.exceptions["anz-sf1-collection-2027-11-17-l2"].resolution.is_none());

    let write_off = |amount, reason: &str| Command::WriteOffMisdirected {
        misdirection: "md-1".to_string(),
        amount_minor: amount,
        reason: reason.to_string(),
    };
    assert_eq!(desk.code(&rae(), write_off(40_00, "holder_untraceable")), "not_permitted");
    let rae_approving = person("rae", &[ic_payments::Role::Approver]);
    assert_eq!(desk.code(&rae_approving, write_off(40_00, "holder_untraceable")), "same_person");
    assert_eq!(desk.code(&ari(), write_off(40_00, "Holder gone")), "invalid_reason");
    assert_eq!(desk.code(&ari(), write_off(40_01, "holder_untraceable")), "invalid_amount");
    desk.ok(&ari(), write_off(20_00, "holder_untraceable"));
    drain(&mut desk, "payee-debt-write-off-");
    assert!(told(&waiting(&desk), "misdirection_refused").unwrap().contains("the core refused it"));
    drain(&mut desk, "");
    desk.ok(&ari(), write_off(40_00, "holder_untraceable"));
    assert_eq!(waiting(&desk)[0].0, "payee-debt-write-off-md-1-2");
    drain(&mut desk, "");
    let events = hear(&mut desk, "1-3", debt("payee_debt_written_off", 0));
    assert!(events.contains(&Event::MisdirectionClosed {
        misdirection: "md-1".to_string()
    }));
    assert_eq!(
        answer(&desk.state, json!({"q": "misdirection_reference", "misdirection": "md-1"})).unwrap(),
        Value::Null
    );
    assert_eq!(answer(&desk.state, json!({"q": "open_misdirections"})).unwrap(), json!({}));
    assert_eq!(
        answer(&desk.state, json!({"q": "misdirection", "misdirection": "md-1"})).unwrap()["closed"],
        true
    );
    assert_eq!(desk.code(&ari(), write_off(1, "holder_untraceable")), "unknown_misdirection");
    assert!(desk
        .ok(
            &svc(),
            Command::PayeeDebtChanged {
                policy: "pol-1".into(),
                debt: "md-1".into(),
                outstanding_minor: 0
            }
        )
        .is_empty());
    assert_eq!(
        desk.code(
            &svc(),
            Command::PayeeDebtChanged {
                policy: String::new(),
                debt: "md-1".into(),
                outstanding_minor: 0
            }
        ),
        "invalid_debt"
    );
}

/// Purpose: a benefit payment to a payee, partly misdirected, marks the
/// payee suspect; the bank's return of the recalled payment is recovered
/// against the debt (no more than is owed, the rest for a reconciler) and
/// is expected on the statement, not treated as a failed payment; a recall
/// the bank will not take is recorded; only a settled outbound policy
/// payment can be reported.
/// Why it matters: a recalled payment coming back is the holder's money
/// returned, not a payment to retry; telling the core it failed would pay
/// the customer twice.
#[test]
fn a_recalled_payment_coming_back_is_recovered_not_retried() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0201111111");
    desk.authority("a1", "pol-1", "01-0001-1234579-00", false);
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    let benefit = NewItem {
        source: "claim:cl-1:i1".to_string(),
        policy: Some(policy("pol-1")),
        payable: Some(PayableKind::Benefit),
        ..pay("i1", "p1", 100_00)
    };
    desk.add("r1", vec![benefit, pay("i2", "p1", 5_00)]);
    let s1 = desk.approve("r1");
    desk.settle(&s1, d(2027, 11, 15));
    desk.prepare("c1", RunType::Collection, d(2027, 11, 15));
    desk.add("c1", vec![collect("c-1", "a1", "pol-1", 10_00)]);
    let s2 = desk.approve("c1");
    desk.settle(&s2, d(2027, 11, 15));
    drain(&mut desk, "");

    assert_eq!(desk.code(&rae(), report("md-c", "c-1", 10_00, "x")), "wrong_state");
    assert_eq!(desk.code(&rae(), report("md-n", "i2", 5_00, "x")), "not_policy_money");
    desk.ok(&rae(), report("md-2", "i1", 60_00, "half went to a stranger"));
    assert!(desk.state.payees["p1"].suspect);
    drain(&mut desk, "recall-");
    assert_eq!(desk.state.misdirections["md-2"].recall, RecallStatus::NotSent("refused_here".to_string()));
    assert!(told(&waiting(&desk), "misdirection_refused").unwrap().contains("the bank refused it"));
    drain(&mut desk, "");

    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "i1".to_string(),
            reason: ReturnReason::Other("FRAD".to_string()),
            returned_on: d(2027, 11, 17),
        },
    );
    assert!(matches!(desk.state.runs["r1"].items["i1"].status, ItemStatus::Returned(_)));
    let work = waiting(&desk);
    let Instruction::CoreAt { command, .. } = &work[0].1 else {
        panic!("recovered through the core")
    };
    assert_eq!(work[0].0, "payee-debt-recovered-i1");
    let command = serde_json::to_value(command).unwrap();
    assert_eq!(
        (command["payee_debt"]["receipt"].clone(), command["payee_debt"]["amount"]["minor"].clone()),
        (json!("recall-i1"), json!(60_00))
    );
    assert!(told(&work, "recall_excess").unwrap().contains("brought back 10000"));
    assert!(work.iter().all(|(k, _)| !k.starts_with("return-")), "the core is not told the payment failed");
    let expected = &desk.state.books["anz-sf1-claims"].expected["recall-i1"];
    assert_eq!(expected.amount_minor, 100_00);
}
