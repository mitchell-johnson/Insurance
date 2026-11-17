//! Premium refunds: back to source, or with the claim when the payer died.

mod common;

use common::*;
use ic_core::PayableKind;
use ic_payments::model::{Counterparty, ExceptionKind, Instruction, ItemStatus, NewItem, PayeeKind, RefundDestination, RefundStatus, RunStatus, RunType};
use ic_payments::query::answer;
use ic_payments::{Command, Event};
use serde_json::json;

fn request(refund: &str, pol: &str, amount: i64, destination: RefundDestination) -> Command {
    Command::RequestRefund {
        refund: refund.to_string(),
        policy: policy(pol),
        amount_minor: amount,
        currency: NZD,
        destination,
    }
}

fn source(authority: &str) -> RefundDestination {
    RefundDestination::Source {
        authority: authority.to_string(),
    }
}

fn to_payee(payee: &str) -> RefundDestination {
    RefundDestination::Payee { payee: payee.to_string() }
}

fn with_claim(claim: &str, payee: &str) -> RefundDestination {
    RefundDestination::WithClaim {
        claim: claim.to_string(),
        payee: payee.to_string(),
    }
}

/// The item that pays a refund.
fn refund_item(item: &str, refund: &str, to: Counterparty, pol: &str, amount: i64) -> NewItem {
    NewItem {
        item: item.to_string(),
        source: format!("refund-{refund}"),
        counterparty: to,
        amount_minor: amount,
        policy: Some(policy(pol)),
        payable: Some(PayableKind::Refund),
        refund: Some(refund.to_string()),
        represents: None,
    }
}

/// Tell the module the core did what the oldest outbox entry asked.
fn core_ok(desk: &mut Desk) -> Vec<Event> {
    let entry = *desk.state.outbox.keys().next().expect("an instruction is waiting");
    desk.ok(&svc(), Command::CompleteInstruction { entry, refused: None })
}

/// Purpose: a refund is asked of the core, owed once the core has raised
/// it, paid in a refund run to the account the premium came from, and the
/// core is told to settle it against the refund it pays.
/// Why it matters: refunding to the source account is the anti-money
/// laundering default: premium paid in must not come out somewhere else.
#[test]
fn a_refund_goes_back_to_the_account_it_came_from() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", false);
    assert_eq!(desk.code(&ari(), request("rf1", "pol-1", 70_00, source("a1"))), "not_permitted");
    let events = desk.ok(&mia(), request("rf1", "pol-1", 70_00, source("a1")));
    assert_eq!(
        events,
        vec![Event::RefundRequested {
            refund: "rf1".to_string(),
            policy: policy("pol-1"),
            amount_minor: 70_00
        }]
    );
    let entry = &desk.state.outbox[&0];
    assert_eq!(entry.key, "refund-rf1-raise");
    let Instruction::Core { caller, command } = &entry.instruction else {
        panic!("a core instruction")
    };
    assert_eq!(caller, "payments");
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"raise_refund_payable": {"policy": "pol-1", "refund": "rf1", "amount": {"minor": 70_00, "currency": "NZD"}}})
    );
    desk.prepare("r1", RunType::PremiumRefunds, d(2027, 11, 15));
    let item = || refund_item("i1", "rf1", Counterparty::Authority("a1".to_string()), "pol-1", 70_00);
    let add = |items| Command::AddRunItems { run: "r1".to_string(), items };
    // Not owed until the core says so.
    assert_eq!(desk.code(&mia(), add(vec![item()])), "refund_not_owed");
    assert_eq!(core_ok(&mut desk)[0], Event::RefundRaised { refund: "rf1".to_string() });

    let mut part = item();
    part.amount_minor = 60_00;
    assert_eq!(desk.code(&mia(), add(vec![part])), "invalid_item");
    desk.payee("p1", PayeeKind::Customer, "03-0001-9999993-00");
    let elsewhere = refund_item("i1", "rf1", Counterparty::Payee("p1".to_string()), "pol-1", 70_00);
    assert_eq!(desk.code(&mia(), add(vec![elsewhere])), "wrong_destination");
    let mut twice = item();
    twice.item = "i2".to_string();
    twice.source = "other".to_string();
    assert_eq!(desk.code(&mia(), add(vec![item(), twice])), "duplicate_item");
    desk.add("r1", vec![item()]);
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::InRun("i1".to_string()));
    assert_eq!(desk.code(&mia(), Command::ReleaseRefund { refund: "rf1".to_string() }), "refund_not_owed");

    let s = desk.approve("r1");
    assert_eq!(desk.state.runs["r1"].items["i1"].account.as_ref().unwrap().number, "01-0001-1234579-00");
    assert_eq!(desk.state.submissions[&s].account, "anz-sf1-refunds");
    desk.settle(&s, d(2027, 11, 15));
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::Paid);
    let Instruction::Core { command, .. } = &desk.state.outbox.values().next().unwrap().instruction else {
        panic!("a core instruction")
    };
    assert_eq!(serde_json::to_value(command).unwrap()["settle_payable"]["refund"], json!("rf1"));
    assert_eq!(answer(&desk.state, json!({"q": "refund", "refund": "rf1"})).unwrap()["status"], json!("paid"));
}

/// Purpose: a refund is refused when it has no amount, repeats an ID, names
/// an authority of another policy or an unknown payee, tries to leave by
/// another account while a source account exists, or names a source that
/// can no longer be used.
/// Why it matters: each of these is a way premium could be paid out to
/// someone other than the person who paid it in.
#[test]
fn a_refund_to_the_wrong_place_is_refused() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", false);
    desk.authority("a2", "pol-2", "02-0001-7654338-00", false);
    desk.payee("p1", PayeeKind::Customer, "03-0001-9999993-00");
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 0, source("a1"))), "invalid_amount");
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 5, source("a2"))), "unknown_authority");
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 5, to_payee("nobody"))), "unknown_payee");
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 5, to_payee("p1"))), "refund_to_source");
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 5, with_claim("clm-1", "p1"))), "wrong_destination");
    desk.ok(&mia(), request("rf1", "pol-1", 5, source("a1")));
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 5, source("a1"))), "duplicate_refund");
    // The account was closed: the source is gone, so a verified account is named.
    desk.ok(
        &pam(),
        Command::CancelAuthority {
            authority: "a2".to_string(),
            reason: "closed".to_string(),
        },
    );
    assert_eq!(desk.code(&mia(), request("rf2", "pol-2", 5, source("a2"))), "source_unavailable");
    desk.ok(
        &svc(),
        Command::PolicyOwner {
            policy: "pol-2".into(),
            owner: "party-p1".into(),
        },
    );
    desk.ok(&mia(), request("rf2", "pol-2", 5, to_payee("p1")));
    core_ok(&mut desk);
    core_ok(&mut desk);
    desk.prepare("r1", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add("r1", vec![refund_item("i2", "rf2", Counterparty::Payee("p1".to_string()), "pol-2", 5)]);
    desk.approve("r1");
    assert_eq!(desk.state.runs["r1"].items["i2"].status, ItemStatus::Submitted);
}

/// Purpose: the core's refusal to raise a refund is recorded on the refund;
/// an owed refund can be released back to suspense, and is released only
/// when the core confirms; a refusal to release raises an exception.
/// Why it matters: the module's view of what is owed must follow the
/// ledger's, in both directions, and a disagreement must be visible.
#[test]
fn the_cores_answers_decide_what_is_owed() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", false);
    desk.ok(&mia(), request("rf1", "pol-1", 70_00, source("a1")));
    let events = desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: 0,
            refused: Some("insufficient_suspense".to_string()),
        },
    );
    assert_eq!(
        events[0],
        Event::RefundRefused {
            refund: "rf1".to_string(),
            code: "insufficient_suspense".to_string()
        }
    );
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::Refused("insufficient_suspense".to_string()));
    assert_eq!(desk.code(&mia(), Command::CompleteInstruction { entry: 9, refused: None }), "not_permitted");

    desk.ok(&mia(), request("rf2", "pol-1", 70_00, source("a1")));
    core_ok(&mut desk);
    assert_eq!(desk.code(&ari(), Command::ReleaseRefund { refund: "rf2".to_string() }), "not_permitted");
    assert_eq!(desk.code(&mia(), Command::ReleaseRefund { refund: "zz".to_string() }), "unknown_refund");
    desk.ok(&mia(), Command::ReleaseRefund { refund: "rf2".to_string() });
    assert_eq!(desk.state.refunds["rf2"].status, RefundStatus::Releasing);
    let events = core_ok(&mut desk);
    assert_eq!(events[0], Event::RefundReleased { refund: "rf2".to_string() });

    desk.ok(&mia(), request("rf3", "pol-1", 70_00, source("a1")));
    core_ok(&mut desk);
    desk.ok(&mia(), Command::ReleaseRefund { refund: "rf3".to_string() });
    let entry = *desk.state.outbox.keys().next().unwrap();
    let events = desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry,
            refused: Some("unknown_refund".to_string()),
        },
    );
    assert!(matches!(
        &events[0],
        Event::ExceptionOpened {
            kind: ExceptionKind::CoreRefused,
            ..
        }
    ));
    assert_eq!(desk.state.refunds["rf3"].status, RefundStatus::Releasing);
}

/// Purpose: once the core reports the payer's death, a refund on that policy
/// is accepted only with the claim, to an estate or beneficiary account, and
/// is paid in the claims run beside the benefit; the core is asked to raise
/// it with the claim named.
/// Why it matters: this is the exception to refund-to-source in the
/// addendum. Premium collected after an undiscovered death belongs to the
/// estate, and must not go back to an account nobody can lawfully operate.
#[test]
fn after_the_payers_death_a_refund_is_paid_with_the_claim() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", true);
    desk.payee("estate", PayeeKind::Estate, "03-0001-1234579-00");
    desk.payee("cust", PayeeKind::Customer, "03-0001-9999993-00");
    desk.ok(
        &svc(),
        Command::PayerDeceased {
            policy: policy("pol-1"),
            claim: "clm-1".to_string(),
        },
    );
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 300_00, source("a1"))), "refund_with_claim");
    assert_eq!(desk.code(&mia(), request("rf1", "pol-1", 300_00, to_payee("cust"))), "refund_with_claim");
    desk.ok(&mia(), request("rf1", "pol-1", 300_00, with_claim("clm-1", "estate")));
    let Instruction::Core { command, .. } = &desk.state.outbox.values().last().unwrap().instruction else {
        panic!("a core instruction")
    };
    assert_eq!(serde_json::to_value(command).unwrap()["raise_refund_payable"]["with_claim"], json!("clm-1"));
    let raise = *desk.state.outbox.keys().last().unwrap();
    let events = desk.ok(&svc(), Command::CompleteInstruction { entry: raise, refused: None });
    assert_eq!(events[0], Event::RefundRaised { refund: "rf1".to_string() });
    assert_eq!(events.len(), 2, "a refund already going with the claim is left alone");

    let item = || refund_item("i1", "rf1", Counterparty::Payee("estate".to_string()), "pol-1", 300_00);
    desk.prepare("refunds", RunType::PremiumRefunds, desk.now.date());
    assert_eq!(
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: "refunds".to_string(),
                items: vec![item()]
            }
        ),
        "wrong_destination"
    );
    desk.prepare("claims", RunType::LumpSumClaims, desk.now.date());
    let mut benefit = pay("i0", "estate", 200_000_00 / 4);
    benefit.policy = Some(policy("pol-1"));
    benefit.payable = Some(PayableKind::Benefit);
    desk.add("claims", vec![benefit, item()]);
    let s = desk.approve("claims");
    desk.settle(&s, desk.now.date());
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::Paid);

    // Another policy paid from the same sole-name account refunds nowhere
    // but with a claim either.
    desk.authority("a9", "pol-9", "04-0001-1234579-00", false);
    desk.state.blocked_accounts.insert("04-0001-1234579-00".to_string());
    assert_eq!(desk.code(&mia(), request("rf9", "pol-9", 5, source("a9"))), "refund_with_claim");
}

/// Purpose: refunds already on their way to the source account when the
/// death is reported are stopped: one that is owed is released, one in an
/// unapproved run is taken out and released, one whose raise is still with
/// the core is released when the core answers, and one already with the
/// bank becomes an exception for a person to chase.
/// Why it matters: a late-notified death arrives while ordinary servicing
/// is in flight. Each refund must end up with the estate, whatever stage it
/// had reached, and the one that cannot be stopped must not go unnoticed.
#[test]
fn refunds_in_flight_are_stopped_when_the_payer_dies() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", true);
    let to_source = || Counterparty::Authority("a1".to_string());
    for id in ["owed", "in-run", "at-bank"] {
        desk.ok(&mia(), request(id, "pol-1", 10_00 + id.len() as i64, source("a1")));
        core_ok(&mut desk);
    }
    desk.ok(&mia(), request("asked", "pol-1", 40_00, source("a1")));
    desk.prepare("r1", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add("r1", vec![refund_item("i1", "in-run", to_source(), "pol-1", 10_06)]);
    desk.ok(&mia(), Command::CheckRun { run: "r1".to_string() });
    desk.prepare("r2", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add("r2", vec![refund_item("i2", "at-bank", to_source(), "pol-1", 10_07)]);
    let s = desk.approve("r2");
    desk.deliver(&s);

    let events = desk.ok(
        &svc(),
        Command::PayerDeceased {
            policy: policy("pol-1"),
            claim: "clm-1".to_string(),
        },
    );
    let rerouted: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Event::RefundRerouted { refund, .. } = e {
                Some(refund.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(rerouted, vec!["in-run", "owed"]);
    assert_eq!(desk.state.refunds["owed"].status, RefundStatus::Releasing);
    assert_eq!(desk.state.refunds["in-run"].status, RefundStatus::Releasing);
    assert!(desk.state.runs["r1"].items.is_empty());
    assert_eq!(desk.state.runs["r1"].status, RunStatus::Prepared);
    assert_eq!(desk.state.refunds["at-bank"].status, RefundStatus::InRun("i2".to_string()));
    let exception = &desk.state.exceptions["deceased-i2"];
    assert_eq!(
        (exception.kind, exception.amount_minor, exception.account.as_str()),
        (ExceptionKind::PaidToBlockedAccount, 10_07, "anz-sf1-refunds")
    );
    assert_eq!(desk.state.refunds["asked"].status, RefundStatus::Requested);

    // The core raised the last one before it heard of the death.
    let raise = desk.state.outbox.iter().find(|(_, e)| e.key == "refund-asked-raise").map(|(k, _)| *k).unwrap();
    let events = desk.ok(&svc(), Command::CompleteInstruction { entry: raise, refused: None });
    assert!(events.contains(&Event::RefundRerouted {
        refund: "asked".to_string(),
        claim: "clm-1".to_string()
    }));
    assert_eq!(desk.state.refunds["asked"].status, RefundStatus::Releasing);
    // A new run cannot pay the blocked account even if someone tries.
    assert!(desk.state.blocked_accounts.contains("01-0001-1234579-00"));
}

/// Purpose: a refund item waiting in an approved run whose file failed is
/// taken out and released when the payer dies, and a refund to the source
/// is held at the check if the account was blocked after it joined the run.
/// Why it matters: the gap between approval and the bank is where a death
/// notice is most likely to be missed.
#[test]
fn an_unsent_refund_in_an_approved_run_is_still_stopped() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", true);
    desk.authority("a2", "pol-2", "01-0001-1234579-00", true);
    desk.authority("a3", "pol-3", "02-0001-7654338-00", false);
    for (id, pol, a) in [("rf1", "pol-1", "a1"), ("rf2", "pol-2", "a2"), ("rf3", "pol-3", "a3")] {
        desk.ok(&mia(), request(id, pol, 10_00 + pol.len() as i64 + id.as_bytes()[2] as i64, source(a)));
        core_ok(&mut desk);
    }
    desk.prepare("r1", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add(
        "r1",
        vec![refund_item(
            "i1",
            "rf1",
            Counterparty::Authority("a1".to_string()),
            "pol-1",
            desk.state.refunds["rf1"].amount_minor,
        )],
    );
    let s = desk.approve("r1");
    desk.ok(
        &svc(),
        Command::RecordNotReceived {
            submission: s,
            reason: "channel down".to_string(),
        },
    );
    desk.prepare("r2", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add(
        "r2",
        vec![
            refund_item(
                "i2",
                "rf2",
                Counterparty::Authority("a2".to_string()),
                "pol-2",
                desk.state.refunds["rf2"].amount_minor,
            ),
            refund_item(
                "i3",
                "rf3",
                Counterparty::Authority("a3".to_string()),
                "pol-3",
                desk.state.refunds["rf3"].amount_minor,
            ),
        ],
    );
    desk.ok(
        &svc(),
        Command::PayerDeceased {
            policy: policy("pol-1"),
            claim: "clm-1".to_string(),
        },
    );
    assert!(desk.state.runs["r1"].items.is_empty());
    assert_eq!(desk.state.runs["r1"].status, RunStatus::Approved);
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::Releasing);
    // pol-2 drew on the same account. Its refund is held at the check, and
    // so is one whose source account has since been closed.
    desk.ok(
        &pam(),
        Command::CancelAuthority {
            authority: "a3".to_string(),
            reason: "closed".to_string(),
        },
    );
    desk.ok(&mia(), Command::CheckRun { run: "r2".to_string() });
    assert_eq!(desk.state.runs["r2"].items["i2"].status, ItemStatus::Held("account_blocked".to_string()));
    assert_eq!(desk.state.runs["r2"].items["i3"].status, ItemStatus::Held("source_unavailable".to_string()));
}

/// Purpose: a refund payment the bank rejects or returns is owed again and
/// can be paid by a later run.
/// Why it matters: the customer is still owed the money. Marking it paid
/// because a file once carried it would lose it.
#[test]
fn a_refund_the_bank_sends_back_is_owed_again() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", false);
    desk.ok(&mia(), request("rf1", "pol-1", 70_00, source("a1")));
    core_ok(&mut desk);
    let item = |id: &str| refund_item(id, "rf1", Counterparty::Authority("a1".to_string()), "pol-1", 70_00);
    desk.prepare("r1", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add("r1", vec![item("i1")]);
    let s = desk.approve("r1");
    desk.deliver(&s);
    desk.ok(
        &svc(),
        Command::RecordAcknowledgement {
            submission: s,
            refused: None,
            rejected: [("i1".to_string(), "invalid".to_string())].into(),
        },
    );
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::Raised);
    desk.prepare("r2", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add("r2", vec![item("i2")]);
    let s = desk.approve("r2");
    desk.settle(&s, d(2027, 11, 15));
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::Paid);
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "i2".to_string(),
            reason: ic_payments::model::ReturnReason::AccountClosed,
            returned_on: d(2027, 11, 16),
        },
    );
    assert_eq!(desk.state.refunds["rf1"].status, RefundStatus::Raised);
}

/// Purpose: a refund named to a payee goes only to the policy owner's own account: a payee that
/// belongs to someone else is refused, and so is any payee while payments has not been told who
/// owns the policy; the owner's own payee is accepted.
/// Why it matters: found by the scenario run, SCN2-16 (F2-16-5, a blocker): a maker requested a
/// customer's refund to another customer's registered account and it was accepted, which is a
/// path for paying a policy's money to anyone.
#[test]
fn a_refund_goes_only_to_the_owners_own_account() {
    let mut desk = Desk::new();
    desk.payee("mine", PayeeKind::Customer, "03-0001-9999993-00");
    desk.payee("theirs", PayeeKind::Customer, "01-0001-1111114-00");
    let told = |desk: &mut Desk, payee: &str| desk.send(&mia(), request("rf1", "pol-5", 116_87, to_payee(payee))).expect_err("refused");
    let unknown = told(&mut desk, "mine");
    assert_eq!(unknown.code, "owner_unknown");
    desk.ok(
        &svc(),
        Command::PolicyOwner {
            policy: "pol-5".into(),
            owner: "party-mine".into(),
        },
    );
    let wrong = told(&mut desk, "theirs");
    assert_eq!(wrong.code, "not_owners_account");
    assert!(wrong.message.contains("another person's account"), "{}", wrong.message);
    assert_eq!(desk.code(&mia(), request("rf1", "pol-5", 116_87, to_payee("theirs"))), "not_owners_account");
    desk.ok(&mia(), request("rf1", "pol-5", 116_87, to_payee("mine")));
}
