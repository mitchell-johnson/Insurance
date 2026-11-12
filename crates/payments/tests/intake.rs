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
        policy: Some(policy("pol-1")),
        claim: None,
        amount_minor: amount,
        payable: Some(PayableKind::Benefit),
    }
}

fn offer(waiting: Waiting) -> Command {
    Command::OfferPayment { waiting: Box::new(waiting) }
}

fn take(run: &str, payments: &[&str]) -> Command {
    Command::TakeWaiting {
        run: run.to_string(),
        payments: payments.iter().map(|p| p.to_string()).collect(),
        payees: Default::default(),
    }
}

fn waiting(desk: &Desk) -> Vec<String> {
    let listed = answer(&desk.state, json!({"q": "waiting"})).unwrap();
    listed.as_array().unwrap().iter().map(|w| w["payment"].as_str().unwrap().to_string()).collect()
}

fn claim_paid(id: &str, detail: serde_json::Value) -> ic_core::EventRecord {
    claims_event(id, "claim_paid", detail)
}

fn claims_event(id: &str, kind: &str, detail: serde_json::Value) -> ic_core::EventRecord {
    let event = json!({"module": {"module": "claims", "kind": kind, "subject": "clm-1", "detail": detail}});
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

/// Purpose: a provider's evidence or rehabilitation cost that claims
/// releases waits like a benefit, under its own reference and marked as a
/// cost, for the provider's payee, with no account reference; other claims
/// events offer nothing.
/// Why it matters: these costs leave the same benefit payable and the core
/// settles them only under the reference claims gave. Left out of the
/// intake they are paid by hand, which is where a reference is mistyped;
/// taking every claims event in would pay things nobody released.
#[test]
fn a_released_provider_cost_waits_like_a_benefit() {
    let mut desk = Desk::new();
    desk.payee("prov-1", PayeeKind::Provider, "0207654321");
    let detail = json!({"policy": "pol-1", "payment_ref": "clm-1-cost-1", "net_minor": "45000", "payee": "party-prov-1",
        "kind": "evidence", "item": "gp-report"});
    for envelope in react(&claims_event("3-80", "claim_cost_paid", detail.clone())) {
        desk.state.apply(4000, envelope).unwrap();
    }
    let held = &desk.state.waiting["clm-1-cost-1"];
    assert_eq!(
        (held.source.as_str(), held.amount_minor, held.account_ref.as_str()),
        ("claim-cost:clm-1:clm-1-cost-1", 450_00, "")
    );
    assert_eq!(held.payable, Some(PayableKind::Benefit));
    assert!(react(&claims_event("3-81", "claim_admitted", detail)).is_empty());

    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.ok(&mia(), take("r1", &["clm-1-cost-1"]));
    let item = &desk.state.runs["r1"].items["clm-1-cost-1"].new;
    assert_eq!(item.counterparty, ic_payments::model::Counterparty::Payee("prov-1".to_string()));
}

/// Purpose: a unit-linked payout the core owes waits under the payout's
/// ID for the policyholder and can go only to a payee of that party; a
/// release that names nobody needs the maker to name the payee; a payout
/// sent back by the bank and instructed again waits under its new
/// reference; a payment released to a named party cannot be redirected to
/// someone else's payee.
/// Why it matters: the core settles a payout only under the reference it
/// issued and for the exact amount. A surrender value paid to a payee who
/// is not the policyholder is the fraud this binding prevents.
#[test]
fn a_unit_linked_payout_waits_for_a_named_payee() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.payee("p2", PayeeKind::Customer, "0207654321");
    let record = |id: &str, event: serde_json::Value| -> ic_core::EventRecord {
        serde_json::from_value(json!({"id": id, "partition": 3, "seq": 90, "business_date": "2027-11-15", "log_index": 950, "event": {"investment": event}}))
            .unwrap()
    };
    let raised = record(
        "3-90",
        json!({"payout_raised": {"policy": "pol-1", "policyholder": "party-p1", "payout": "po-1", "amount_minor": 12_345_00, "reason": "surrender"}}),
    );
    let again = record(
        "3-91",
        json!({"payout_reinstructed": {"policy": "pol-1", "policyholder": "party-p1", "payout": "po-1", "payment": "po-1-r2", "amount_minor": 12_345_00}}),
    );
    let returned = record(
        "3-92",
        json!({"payout_returned": {"policy": "pol-1", "payout": "po-1", "payment": "po-1", "amount_minor": 12_345_00}}),
    );
    assert!(react(&returned).is_empty());
    for (index, envelope) in react(&raised).into_iter().enumerate() {
        desk.state.apply(5000 + index as u64, envelope).unwrap();
    }
    let held = &desk.state.waiting["po-1"];
    assert_eq!(
        (held.source.as_str(), held.payee.as_str(), held.amount_minor),
        ("payout:pol-1:po-1", "party-p1", 12_345_00)
    );

    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    let to = |payment: &str, payee: &str| Command::TakeWaiting {
        run: "r1".to_string(),
        payments: [payment.to_string()].into(),
        payees: [(payment.to_string(), payee.to_string())].into(),
    };
    // It is the policyholder's: nobody else's payee can be named for it.
    assert_eq!(desk.code(&mia(), to("po-1", "nobody")), "unknown_payee");
    assert_eq!(desk.code(&mia(), to("po-1", "p2")), "payee_mismatch");
    desk.ok(&mia(), take("r1", &["po-1"]));
    // A release that names nobody needs the maker to say who is paid.
    let mut anonymous = released("pay-0", "", 5_00);
    anonymous.source = "manual:pay-0".to_string();
    desk.ok(&svc(), offer(anonymous));
    assert_eq!(desk.code(&mia(), take("r1", &["pay-0"])), "payee_required");
    let item = &desk.state.runs["r1"].items["po-1"].new;
    assert_eq!((item.amount_minor, item.payable), (12_345_00, Some(PayableKind::Benefit)));

    // Sent back by the bank and instructed again: a new reference waits.
    for envelope in react(&again) {
        desk.state.apply(5010, envelope).unwrap();
    }
    assert_eq!(waiting(&desk), vec!["pay-0", "po-1-r2"]);

    // A release that names its party goes to that party's payee only.
    desk.ok(&svc(), offer(released("pay-1", "party-p1", 100_00)));
    assert_eq!(desk.code(&mia(), to("pay-1", "p2")), "payee_mismatch");
    desk.ok(&mia(), to("pay-1", "p1"));
}

/// Purpose: another module's driver offers a payment that is not policy
/// money (a reinsurer's net settlement) with a plain JSON command; it waits,
/// joins a reinsurer run for the reinsurer's payee, and when it settles the
/// asking module's source is on the event and nothing is sent to the core;
/// a release that names a policy without its payable is refused.
/// Why it matters: this is the wire format the reinsurance driver sends.
/// If it drifts, settlements due sit in an outbox and no reinsurer is paid;
/// if a non-policy payment produced a core instruction it would be refused
/// there and block the outbox behind it.
#[test]
fn a_reinsurer_settlement_is_offered_by_its_module_and_paid() {
    let mut desk = Desk::new();
    desk.payee("re-1", PayeeKind::Reinsurer, "0301112223");
    let envelope: ic_payments::Envelope = serde_json::from_value(json!({
        "key": "reinsurance-notice-41",
        "actor": {"id": "reinsurance", "roles": ["service"]},
        "at": "1970-01-01T00:00:00",
        "command": {"type": "offer_payment", "waiting": {
            "payment": "ri-stmt-7", "source": "reinsurance:statement:7", "payee": "party-re-1", "amount_minor": 90_000_00}}
    }))
    .unwrap();
    desk.state.apply(6000, envelope.clone()).unwrap();
    desk.state.apply(6001, envelope).unwrap();
    assert_eq!(desk.state.clock, Some(monday()), "an offer does not move the clock");
    assert_eq!(waiting(&desk), vec!["ri-stmt-7"]);
    let mut half = released("pay-1", "p1", 5_00);
    half.payable = None;
    assert_eq!(desk.code(&svc(), offer(half)), "invalid_payment");

    desk.prepare("r1", RunType::Reinsurer, d(2027, 11, 15));
    desk.ok(&mia(), take("r1", &["ri-stmt-7"]));
    let submission = desk.approve("r1");
    let events = desk.settle(&submission, d(2027, 11, 15));
    assert!(events.contains(&Event::ItemSettled {
        item: "ri-stmt-7".to_string(),
        source: "reinsurance:statement:7".to_string(),
        amount_minor: 90_000_00
    }));
    assert!(desk.state.outbox.is_empty(), "nothing for the core: this is not policy money");
    assert!(desk.state.waiting.is_empty());
}

/// Purpose: when the bank refuses a claim payment, or sends it back before
/// it settled, claims is told that exact payment failed, at the policy's
/// partition, and the spent reference stops waiting; a payment that was not
/// released by claims tells nobody.
/// Why it matters: claims holds a released payment as "with the bank" until
/// it hears otherwise. Without this a benefit the bank bounced stays owed
/// to a claimant with nobody prompted to pay it again.
#[test]
fn claims_is_told_when_the_bank_refuses_a_released_payment() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    for (n, payment) in ["pay-a", "pay-b"].into_iter().enumerate() {
        // Different amounts: two equal payments to one payee are held as a possible duplicate.
        let detail = json!({"policy": "pol-1", "payment_ref": payment, "net_minor": (10_000 * (n + 1)).to_string(), "payee": "p1"});
        for envelope in react(&claim_paid(&format!("3-{}", 100 + n), detail)) {
            desk.state.apply(7000 + n as u64, envelope).unwrap();
        }
    }
    desk.ok(&svc(), offer(released("pay-c", "p1", 300_00)));
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.ok(&mia(), take("r1", &["pay-a", "pay-b", "pay-c"]));
    let submission = desk.approve("r1");
    desk.deliver(&submission);
    desk.ok(
        &svc(),
        Command::RecordAcknowledgement {
            submission: submission.clone(),
            refused: None,
            rejected: [
                ("pay-a".to_string(), "account_closed".to_string()),
                ("pay-c".to_string(), "account_closed".to_string()),
            ]
            .into(),
        },
    );
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "pay-b".to_string(),
            reason: ic_payments::model::ReturnReason::Other("recalled".to_string()),
            returned_on: d(2027, 11, 15),
        },
    );
    // The maker is also told of the return, as for any returned payment.
    let told: Vec<_> = desk.state.outbox.values().filter(|e| e.key.starts_with("claim-payment-failed-")).collect();
    assert_eq!(told.len(), 2, "{:?}", desk.state.outbox);
    assert!(desk.state.outbox.values().any(|e| e.key == "returned-pay-b"));
    let failed = |payment: &str| Instruction::CoreAt {
        policy: policy("pol-1"),
        caller: "payments".to_string(),
        command: Box::new(ic_core::Command::Claims(Box::new(ic_core::claims_module::Command::PaymentReturned {
            claim: "clm-1".to_string(),
            payment_ref: payment.to_string(),
        }))),
    };
    assert_eq!((&told[0].instruction, &told[1].instruction), (&failed("pay-a"), &failed("pay-b")));
    assert!(desk.state.waiting.is_empty(), "a reference the bank has seen is spent");
    assert!(waiting(&desk).is_empty());
}
