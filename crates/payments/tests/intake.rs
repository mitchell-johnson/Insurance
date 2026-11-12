//! Payments released by other modules: kept until paid, never paid twice.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_core::PayableKind;
use ic_log::StateMachine;
use ic_payments::intake::Waiting;
use ic_payments::model::{Instruction, PayeeKind, RunType};
use ic_payments::query::{answer, react};
use ic_payments::{Command, Event};
use serde_json::json;

fn released(payment: &str, payee: &str, amount: i64) -> Waiting {
    Waiting {
        payment: payment.to_string(),
        source: format!("claim:clm-1:{payment}"),
        payee: payee.to_string(),
        account_ref: "acct-ref-9".to_string(),
        policy: policy("pol-1"),
        amount_minor: amount,
        payable: PayableKind::Benefit,
    }
}

fn offer(waiting: Waiting) -> Command {
    Command::OfferPayment { waiting: Box::new(waiting) }
}

fn take(run: &str, payments: &[&str]) -> Command {
    Command::TakeWaiting {
        run: run.to_string(),
        payments: payments.iter().map(|p| p.to_string()).collect(),
    }
}

fn waiting(desk: &Desk) -> Vec<String> {
    let listed = answer(&desk.state, json!({"q": "waiting"})).unwrap();
    listed.as_array().unwrap().iter().map(|w| w["payment"].as_str().unwrap().to_string()).collect()
}

fn claim_paid(id: &str, detail: serde_json::Value) -> ic_core::EventRecord {
    let event = json!({"module": {"module": "claims", "kind": "claim_paid", "subject": "clm-1", "detail": detail}});
    serde_json::from_value(json!({"id": id, "partition": 3, "seq": 78, "business_date": "2027-11-15", "log_index": 901, "event": event})).unwrap()
}

/// Purpose: a benefit payment claims releases arrives as the claims event,
/// waits under the claims payment reference, is put into a run by a maker
/// for the payee of that party, and settles to the core as exactly the
/// payment claims released; after that it waits no longer.
/// Why it matters: the core refuses a settlement that is not the payment
/// claims instructed, by reference and amount. Retyping the reference or the
/// amount by hand is how a benefit is paid and then cannot be settled.
#[test]
fn a_released_claim_payment_waits_then_is_paid_as_released() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    let detail = json!({"policy": "pol-1", "account_ref": "acct-ref-9", "payment_ref": "clm-1-pay-1", "amount_minor": "5200000",
        "net_minor": "5000000", "payee": "party-p1", "period_end": "", "ex_gratia": "false"});
    let event = claim_paid("3-78", detail);
    // Offered twice by the feed, taken in once, and the clock does not move.
    for (index, envelope) in react(&event).into_iter().chain(react(&event)).enumerate() {
        assert_eq!(envelope.key, "core-3-78");
        desk.state.apply(2000 + index as u64, envelope).unwrap();
    }
    assert_eq!(desk.state.clock, Some(monday()));
    assert_eq!(waiting(&desk), vec!["clm-1-pay-1"]);
    let held = &desk.state.waiting["clm-1-pay-1"];
    assert_eq!((held.amount_minor, held.account_ref.as_str()), (50_000_00, "acct-ref-9"));

    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    let events = desk.ok(&mia(), take("r1", &["clm-1-pay-1"]));
    assert!(events.contains(&Event::RunItemsAdded {
        run: "r1".to_string(),
        count: 1
    }));
    assert!(waiting(&desk).is_empty(), "a payment in a run is not offered to another");
    assert_eq!(desk.code(&mia(), take("r1", &["clm-1-pay-1"])), "not_waiting");

    let submission = desk.approve("r1");
    desk.settle(&submission, d(2027, 11, 15));
    let Instruction::Core { caller, command } = &desk.state.outbox.values().next().unwrap().instruction else {
        panic!("a core instruction")
    };
    assert_eq!(caller, "payments");
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"settle_payable": {"policy": "pol-1", "payment": "clm-1-pay-1", "payable": "benefit",
               "amount": {"minor": 50_000_00, "currency": "NZD"}, "bank_account": "anz-sf1-claims"}})
    );
    assert!(desk.state.waiting.is_empty(), "a settled payment is finished with");
    // The event arriving again long after cannot bring it back.
    assert_eq!(desk.code(&svc(), offer(released("clm-1-pay-1", "party-p1", 50_000_00))), "duplicate_item");
}

/// Purpose: a waiting payment taken out of a run, or whose run is
/// cancelled, waits again and can join another run; one release cannot be
/// changed by a second with the same reference; a release with no amount,
/// payee or reference is refused; only the service offers and only a maker
/// takes.
/// Why it matters: a released benefit is money the insurer owes. If
/// removing it from a run lost it, nobody would be prompted to pay it; if a
/// second message could change it, the amount paid could differ from the
/// amount claims approved.
#[test]
fn a_waiting_payment_is_neither_lost_nor_changed() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.ok(&svc(), offer(released("pay-1", "p1", 700_00)));
    desk.ok(&svc(), offer(released("pay-2", "party-p1", 300_00)));
    assert_eq!(desk.code(&mia(), offer(released("pay-3", "p1", 1))), "not_permitted");
    assert_eq!(desk.code(&svc(), offer(released("pay-1", "p1", 900_00))), "payment_conflict");
    assert_eq!(desk.code(&svc(), offer(released("pay-3", "p1", 0))), "invalid_payment");
    assert_eq!(desk.code(&svc(), offer(released("", "p1", 5))), "invalid_payment");
    assert_eq!(desk.code(&svc(), offer(released("pay-3", "", 5))), "invalid_payment");
    // An event whose amount cannot be read offers nothing.
    let unreadable = claim_paid("3-79", json!({"policy": "pol-1", "payment_ref": "pay-9", "net_minor": "lots", "payee": "p1"}));
    let envelope = react(&unreadable).remove(0);
    assert_eq!(desk.state.apply(3000, envelope).unwrap_err().code, "invalid_payment");

    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    assert_eq!(desk.code(&ari(), take("r1", &["pay-1"])), "not_permitted");
    assert_eq!(desk.code(&mia(), take("r1", &["pay-1", "pay-7"])), "not_waiting");
    assert_eq!(desk.code(&mia(), take("zz", &["pay-1"])), "unknown_run");
    desk.ok(&mia(), take("r1", &["pay-1", "pay-2"]));
    desk.ok(
        &mia(),
        Command::RemoveRunItem {
            run: "r1".to_string(),
            item: "pay-1".to_string(),
        },
    );
    assert_eq!(waiting(&desk), vec!["pay-1"]);
    desk.ok(&mia(), Command::CancelRun { run: "r1".to_string() });
    assert_eq!(waiting(&desk), vec!["pay-1", "pay-2"]);

    desk.prepare("r2", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.ok(&mia(), take("r2", &["pay-1", "pay-2"]));
    let run = &desk.state.runs["r2"];
    assert_eq!(run.items.keys().collect::<BTreeSet<_>>().len(), 2);
    assert_eq!(run.items["pay-2"].new.source, "claim:clm-1:pay-2");
}

/// Purpose: a waiting payment names a payee by its ID or by its party; a
/// party with no payee here, or with more than one, is refused and the
/// payment keeps waiting.
/// Why it matters: claims knows who is to be paid as a party. Guessing
/// which of a party's accounts to pay sends a benefit to the wrong account.
#[test]
fn a_waiting_payment_is_paid_only_to_a_payee_that_is_certain() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.ok(&svc(), offer(released("pay-1", "party-nobody", 100_00)));
    desk.ok(&svc(), offer(released("pay-2", "party-p1", 200_00)));
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    assert_eq!(desk.code(&mia(), take("r1", &["pay-1"])), "unknown_payee");

    // The same party registers a second account.
    desk.ok(
        &pam(),
        Command::RegisterPayee {
            payee: "p1-other".to_string(),
            party: "party-p1".to_string(),
            kind: PayeeKind::Beneficiary,
            account: account("p1", "0309999999"),
            name_check: ic_payments::model::NameCheck::Match,
        },
    );
    assert_eq!(desk.code(&mia(), take("r1", &["pay-2"])), "ambiguous_payee");
    assert_eq!(waiting(&desk), vec!["pay-1", "pay-2"]);
}
