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
        account_ref: String::new(),
        policy: Some(policy("pol-1")),
        claim: None,
        run: None,
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
    let detail = json!({"policy": "pol-1", "account_ref": "01-0123-4567", "payment_ref": "clm-1-pay-1", "amount_minor": "5200000",
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
    assert_eq!((held.amount_minor, held.account_ref.as_str()), (50_000_00, "01-0123-4567"));

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
/// asking module's source is on the event, nothing is sent to the core, and
/// the reinsurance module is told in its own command that the statement is
/// paid, or that the bank refused or returned it; a release that names a
/// policy without its payable is refused.
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
    assert!(desk.state.waiting.is_empty());
    // Nothing for the core, because this is not policy money. The
    // reinsurance module is told, in its own command, that its statement is paid.
    let told = |desk: &Desk| -> Vec<(String, serde_json::Value)> {
        let entries = desk.state.outbox.values();
        entries
            .filter_map(|e| match &e.instruction {
                Instruction::Module { module, command } => Some((module.clone(), command.clone())),
                Instruction::Notify { .. } => None,
                other => panic!("not for the core: {other:?}"),
            })
            .collect()
    };
    assert_eq!(
        told(&desk),
        vec![(
            "reinsurance".to_string(),
            json!({"key": "payments:settled:ri-stmt-7", "caller": "payments", "on": "2027-11-15",
                   "command": {"record_settlement": {"number": 7, "payment_ref": "ri-stmt-7", "amount_minor": 90_000_00, "bank_account": "anz-sf1-third"}}})
        )]
    );

    // The bank sends it back two days later: the statement is owed again.
    desk.at(t(2027, 11, 17, 9, 0));
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "ri-stmt-7".to_string(),
            reason: ic_payments::model::ReturnReason::Other("beneficiary bank closed".to_string()),
            returned_on: d(2027, 11, 17),
        },
    );
    let returned = &told(&desk)[1].1;
    assert_eq!(returned["key"], json!("payments:returned:ri-stmt-7:2027-11-17"));
    assert_eq!(returned["command"]["settlement_returned"]["number"], json!(7));
    assert!(returned["command"]["settlement_returned"]["reason"]
        .as_str()
        .unwrap()
        .contains("beneficiary bank closed"));
    // The reinsurance module offers it again under a new reference and the
    // same source, and it is paid like the first.
    let mut again = released("ri-stmt-7-2", "re-1", 90_000_00);
    (again.source, again.policy, again.payable) = ("reinsurance:statement:7".to_string(), None, None);
    desk.ok(&svc(), offer(again));
    desk.prepare("r0", RunType::Reinsurer, d(2027, 11, 18));
    desk.ok(&mia(), take("r0", &["ri-stmt-7-2"]));
    desk.approve("r0");

    // One the bank refuses outright is reported the same way, and a source
    // that is not a statement number tells nobody.
    for (payment, source) in [("ri-stmt-8", "reinsurance:statement:8"), ("ri-other", "reinsurance:statement:latest")] {
        let mut offered = released(payment, "re-1", 40_000_00 + payment.len() as i64);
        (offered.source, offered.policy, offered.payable) = (source.to_string(), None, None);
        desk.ok(&svc(), offer(offered));
    }
    desk.prepare("r2", RunType::Reinsurer, d(2027, 11, 18));
    desk.ok(&mia(), take("r2", &["ri-stmt-8", "ri-other"]));
    let submission = desk.approve("r2");
    desk.deliver(&submission);
    desk.ok(
        &svc(),
        Command::RecordAcknowledgement {
            submission,
            refused: None,
            rejected: [
                ("ri-stmt-8".to_string(), "account_closed".to_string()),
                ("ri-other".to_string(), "account_closed".to_string()),
            ]
            .into(),
        },
    );
    let all = told(&desk);
    assert_eq!(all.len(), 3);
    assert_eq!(
        all[2].1["command"],
        json!({"settlement_returned": {"number": 8, "payment_ref": "ri-stmt-8", "reason": "account_closed"}})
    );
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

/// Purpose: claims is given a run's settled total once, when the last of
/// the run's payments has settled or failed, and the total counts only what
/// settled; a payment released by hand belongs to no run and confirms
/// nothing.
/// Why it matters: claims reconciles each run it sent out against what the
/// bank paid. A confirmation sent early, or one that counted a refused
/// payment, would mark a run reconciled while a claimant was still unpaid.
#[test]
fn claims_is_given_a_runs_settled_total_when_all_of_it_is_done() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    let paid = |n: u64, payment: &str, net: i64, run: &str| {
        let detail = json!({"policy": "pol-1", "payment_ref": payment, "net_minor": net.to_string(), "payee": "p1", "run": run});
        react(&claim_paid(&format!("3-{}", 200 + n), detail)).remove(0)
    };
    for (n, (payment, net, run)) in [
        ("run-9/clm-1/a", 100_00, "run-9"),
        ("run-9/clm-1/b", 200_00, "run-9"),
        ("run-9/clm-1/c", 300_00, "run-9"),
        ("by-hand", 400_00, ""),
    ]
    .into_iter()
    .enumerate()
    {
        desk.state.apply(8000 + n as u64, paid(n as u64, payment, net, run)).unwrap();
    }
    assert_eq!(desk.state.claims_runs["run-9"].outstanding.len(), 3);
    assert_eq!(desk.state.waiting["by-hand"].run, None);

    let confirmations = |desk: &Desk| -> Vec<Instruction> {
        let entries = desk.state.outbox.values().filter(|e| e.key.starts_with("claims-run-confirmed-"));
        entries.map(|e| e.instruction.clone()).collect()
    };
    // Two go out today; the bank refuses one of them.
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.ok(&mia(), take("r1", &["run-9/clm-1/a", "run-9/clm-1/b", "by-hand"]));
    let first = desk.approve("r1");
    desk.deliver(&first);
    desk.ok(
        &svc(),
        Command::RecordAcknowledgement {
            submission: first.clone(),
            refused: None,
            rejected: [("run-9/clm-1/b".to_string(), "account_closed".to_string())].into(),
        },
    );
    desk.ok(
        &svc(),
        Command::RecordSettlement {
            submission: first,
            settled_on: d(2027, 11, 15),
        },
    );
    assert!(confirmations(&desk).is_empty(), "one payment of the run has not gone yet");
    assert_eq!(desk.state.claims_runs["run-9"].settled_minor, 100_00);

    // The last one settles the next day: the run is confirmed for what settled.
    desk.at(t(2027, 11, 16, 9, 0));
    desk.prepare("r2", RunType::BenefitPayments, d(2027, 11, 16));
    desk.ok(&mia(), take("r2", &["run-9/clm-1/c"]));
    let second = desk.approve("r2");
    desk.settle(&second, d(2027, 11, 16));
    assert_eq!(
        confirmations(&desk),
        vec![Instruction::CoreAt {
            policy: policy("pol-1"),
            caller: "payments".to_string(),
            command: Box::new(ic_core::Command::Claims(Box::new(ic_core::claims_module::Command::ConfirmRun {
                run: "run-9".to_string(),
                settled_minor: 400_00,
            }))),
        }]
    );
    assert!(desk.state.claims_runs.is_empty());
}

/// Purpose: once claims says what a submitted run released (`run_submitted`),
/// the run is confirmed only when that much has settled: a payment released
/// late, after the others had all settled, is waited for.
/// Why it matters: claims reconciles the run against the bank's total; a
/// confirmation sent before the late payment was even released would mark
/// the run reconciled while a claimant was unpaid.
#[test]
fn a_run_with_its_total_waits_for_a_late_release() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    let paid = |n: u64, payment: &str| {
        let net = if n == 0 { "10000" } else { "15000" };
        let detail = json!({"policy": "pol-1", "payment_ref": payment, "net_minor": net, "payee": "p1", "run": "run-7"});
        react(&claim_paid(&format!("3-{}", 300 + n), detail)).remove(0)
    };
    desk.state.apply(9000, paid(0, "run-7/a")).unwrap();
    // Claims says what the run released once it is submitted.
    let submitted = |id: &str, detail: serde_json::Value| {
        let event = json!({"module": {"module": "claims", "kind": "run_submitted", "subject": "run-7", "detail": detail}});
        let record = serde_json::from_value(json!({"id": id, "partition": 3, "seq": 79, "business_date": "2027-11-15", "log_index": 902, "event": event}));
        react(&record.unwrap()).remove(0)
    };
    desk.state
        .apply(9002, submitted("3-310", json!({"submitted_minor": "25000", "count": "2"})))
        .unwrap();
    // A run none of whose payments are known has nothing to confirm.
    let unknown = json!({"module": {"module": "claims", "kind": "run_submitted", "subject": "run-8", "detail": {"submitted_minor": "x"}}});
    let unknown = serde_json::from_value(json!({"id": "3-311", "partition": 3, "seq": 80, "business_date": "2027-11-15", "log_index": 903, "event": unknown}));
    desk.state.apply(9003, react(&unknown.unwrap()).remove(0)).unwrap();
    assert!(!desk.state.claims_runs.contains_key("run-8"));
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.ok(&mia(), take("r1", &["run-7/a"]));
    let first = desk.approve("r1");
    desk.settle(&first, d(2027, 11, 15));
    let confirmed = |desk: &Desk| desk.state.outbox.values().any(|e| e.key == "claims-run-confirmed-run-7");
    assert!(!confirmed(&desk), "half the run has not been released yet");

    desk.state.apply(9001, paid(1, "run-7/b")).unwrap();
    desk.at(t(2027, 11, 16, 9, 0));
    desk.prepare("r2", RunType::BenefitPayments, d(2027, 11, 16));
    desk.ok(&mia(), take("r2", &["run-7/b"]));
    let second = desk.approve("r2");
    desk.settle(&second, d(2027, 11, 16));
    assert!(confirmed(&desk));
}

/// Purpose: a released payment whose verified account is not the account
/// the chosen payee holds is refused when taken into a run; the payee's ID
/// or the same number written with spaces or dashes is accepted.
/// Why it matters: claims verified one account with the claimant; paying a
/// different one, after a payee record was changed or the wrong payee was
/// picked, is how a benefit goes to the wrong person.
#[test]
fn a_payment_goes_only_to_the_account_that_was_verified() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    let mut other = released("pay-x", "p1", 100_00);
    other.account_ref = "02-9999-9999".to_string();
    let mut by_id = released("pay-y", "p1", 100_00);
    by_id.account_ref = "p1".to_string();
    for w in [other, by_id] {
        desk.ok(&svc(), Command::OfferPayment { waiting: Box::new(w) });
    }
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    assert_eq!(desk.code(&mia(), take("r1", &["pay-x"])), "account_mismatch");
    desk.ok(&mia(), take("r1", &["pay-y"]));
}
