//! Money leaving through a payable: a refund is owed in refund payable until
//! the bank confirms it, a settled payment discharges the payable from the
//! bank account's mirror, and a payment the bank sends back is owed again.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::event::{Event, MessageBody};
use ic_core::{Command, GroupId, PayableKind, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_money::{Currency::AUD, Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::{Cluster, Delivery};

const MONTHLY: i64 = 61_84;
const REFUNDS: &str = "nz-main-refunds";

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn cluster(partitions: PartitionId) -> Cluster {
    let mut c = Cluster::in_memory(partitions, 42);
    c.keep_journal = true;
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

fn code(r: Result<(), Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

fn clean(c: &Cluster) {
    let breaks = c.reconcile();
    assert!(breaks.is_empty(), "{breaks:?}");
}

/// A policy on partition 0 that has paid its first premium and 100.00 more.
fn overpaid(c: &mut Cluster) -> PolicyId {
    let n = (1..).find(|n| c.partition_of(&GroupId(format!("grp-{n:08}"))) == 0).unwrap();
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    c.submit(
        0,
        "cash-matching",
        Command::ApplyReceipt {
            policy: id.clone(),
            receipt: "r1".into(),
            amount: Money::new(MONTHLY + 100_00, NZD),
        },
    )
    .unwrap();
    id
}

fn raise(id: &PolicyId, refund: &str, minor: i64) -> Command {
    Command::RaiseRefundPayable {
        policy: id.clone(),
        refund: refund.into(),
        amount: Money::new(minor, NZD),
        with_claim: None,
    }
}

fn settle(id: &PolicyId, payment: &str, refund: Option<&str>, minor: i64) -> Command {
    Command::SettlePayable {
        policy: id.clone(),
        payment: payment.into(),
        payable: PayableKind::Refund,
        refund: refund.map(str::to_string),
        amount: Money::new(minor, NZD),
        bank_account: REFUNDS.into(),
    }
}

fn balances(c: &Cluster, p: PartitionId, id: &PolicyId) -> (i128, i128, i128) {
    let l = &c.state(p).ledger;
    (
        l.natural_balance(&accounts::policy_suspense(id, NZD).id),
        l.natural_balance(&accounts::refund_payable(id, NZD).id),
        l.balance(&accounts::bank(REFUNDS, NZD).id),
    )
}

/// Purpose: a refund goes from suspense to refund payable when raised, and from refund payable
/// to the bank account's mirror only when the bank confirms settlement, in one or several
/// payments, with reconciliation clean at every step.
/// Why it matters: this is the whole outbound seam between the payments module and the ledger.
/// If the cash leg posted at any other moment, the mirror account could never equal the bank's
/// statement and a refund in flight would not show as owed.
#[test]
fn a_refund_is_owed_until_the_bank_settles_it() {
    let mut c = cluster(1);
    let id = overpaid(&mut c);
    assert_eq!(balances(&c, 0, &id), (100_00, 0, 0));

    c.submit(0, "payments", raise(&id, "rf-1", 100_00)).unwrap();
    assert_eq!(balances(&c, 0, &id), (0, 100_00, 0));
    clean(&c);

    // Paid in two parts: each names the refund, and what is still owed follows.
    c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 60_00)).unwrap();
    assert_eq!(balances(&c, 0, &id), (0, 40_00, -60_00));
    assert_eq!(c.state(0).policies[&id].refunds_raised["rf-1"], 40_00);
    clean(&c);
    c.submit(0, "payments", settle(&id, "pay-2", Some("rf-1"), 40_00)).unwrap();
    assert_eq!(balances(&c, 0, &id), (0, 0, -100_00));
    assert!(c.state(0).policies[&id].refunds_raised.is_empty());
    clean(&c);

    let events: Vec<Event> = c.take_events().into_iter().map(|e| e.event).collect();
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::RefundPayableRaised { refund, amount_minor: 100_00, with_claim: None, .. } if refund == "rf-1")));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::PaymentSettled { payable: PayableKind::Refund, bank_account, .. } if bank_account == REFUNDS))
            .count(),
        2
    );
    // The cash leg carries the policy's statutory fund, so the finance ledger can split bank
    // movements by fund.
    let entry = c.nodes[&0].journal.iter().rev().find(|e| e.body.kind == "payment_settled").unwrap();
    assert!(entry.body.postings.iter().all(|p| p.dims.statutory_fund.is_some()));
    assert_eq!(entry.body.detail["payment"], "pay-2");
}

/// Purpose: a payment the bank sends back reverses to refund payable by a new entry that names
/// the entry it reverses and carries the original value date, and can then be paid again or
/// released to suspense.
/// Why it matters: the design's rule is that a returned payment never lands in suspense and
/// nothing posted is edited. A return that lost its link or its value date would break the
/// audit trail from the bank's return file to the original payment.
#[test]
fn a_returned_payment_is_owed_again_and_names_what_it_reverses() {
    let mut c = cluster(1);
    let id = overpaid(&mut c);
    c.submit(0, "payments", raise(&id, "rf-1", 100_00)).unwrap();
    c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 100_00)).unwrap();
    let settled_on = c.business_date().unwrap();
    let settled = c.nodes[&0].journal.iter().rev().find(|e| e.body.kind == "payment_settled").unwrap().id;
    c.end_of_day().unwrap();
    c.end_of_day().unwrap();

    let back = Command::ReturnPayment {
        policy: id.clone(),
        payment: "pay-1".into(),
    };
    c.submit(0, "payments", back.clone()).unwrap();
    assert_eq!(balances(&c, 0, &id), (0, 100_00, 0));
    clean(&c);
    let reversal = c.nodes[&0].journal.iter().rev().find(|e| e.body.kind == "payment_returned").unwrap();
    assert_eq!(reversal.body.detail["reverses"], settled.to_string());
    assert_eq!(reversal.body.value_date, settled_on);
    assert_eq!(reversal.business_date, c.business_date().unwrap());
    assert!(c
        .take_events()
        .iter()
        .any(|e| matches!(&e.event, Event::PaymentReturned { payment, amount_minor: 100_00, .. } if payment == "pay-1")));

    // Under a new key the same return is refused: money comes back once.
    assert_eq!(code(c.submit(0, "payments", back)), "already_returned");
    // A payment ID is used once, even after its payment came back.
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 100_00))), "duplicate_payment");

    // The customer decides to leave the credit on the policy after all.
    let release = Command::ReleaseRefundPayable {
        policy: id.clone(),
        refund: "rf-1".into(),
    };
    c.submit(0, "payments", release.clone()).unwrap();
    assert_eq!(balances(&c, 0, &id), (100_00, 0, 0));
    assert_eq!(code(c.submit(0, "payments", release)), "unknown_refund");
    clean(&c);
}

/// Purpose: every refusal of the four commands, each leaving the ledger head where it was.
/// Why it matters: these commands move money out. A wrong currency, a refund raised twice, more
/// paid than is owed or a caller other than the payments module must be refused before anything
/// is written, or a retry or a defect in a module becomes a second payment.
#[test]
fn the_payment_commands_refuse_what_they_must() {
    let mut c = cluster(1);
    let id = overpaid(&mut c);
    let head = c.state(0).ledger.head();

    assert_eq!(code(c.submit(0, "cash-matching", raise(&id, "rf-1", 1_00))), "caller_not_allowed");
    assert_eq!(code(c.submit(0, "payments", raise(&id, "rf-1", 0))), "invalid_amount");
    assert_eq!(code(c.submit(0, "payments", raise(&id, "rf-1", 100_01))), "insufficient_suspense");
    let in_aud = Command::RaiseRefundPayable {
        policy: id.clone(),
        refund: "rf-1".into(),
        amount: Money::new(1_00, AUD),
        with_claim: None,
    };
    assert_eq!(code(c.submit(0, "payments", in_aud)), "invalid_amount");
    // No claim takes this policy's refunds, so a refund cannot be routed to one.
    let with_claim = Command::RaiseRefundPayable {
        policy: id.clone(),
        refund: "rf-1".into(),
        amount: Money::new(1_00, NZD),
        with_claim: Some("clm-1".into()),
    };
    assert_eq!(code(c.submit(0, "payments", with_claim)), "refund_to_source");
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 1_00))), "insufficient_payable");
    assert_eq!(
        code(c.submit(
            0,
            "payments",
            Command::ReturnPayment {
                policy: id.clone(),
                payment: "pay-1".into()
            }
        )),
        "unknown_payment"
    );
    assert_eq!(c.state(0).ledger.head(), head);

    c.submit(0, "payments", raise(&id, "rf-1", 50_00)).unwrap();
    assert_eq!(code(c.submit(0, "payments", raise(&id, "rf-1", 10_00))), "duplicate_refund");
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", None, 10_00))), "refund_required");
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 50_01))), "insufficient_payable");
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), -1))), "invalid_amount");
    for bad in ["", "has space", "a/b", &"x".repeat(65)] {
        let cmd = Command::SettlePayable {
            policy: id.clone(),
            payment: "pay-1".into(),
            payable: PayableKind::Refund,
            refund: Some("rf-1".into()),
            amount: Money::new(10_00, NZD),
            bank_account: bad.to_string(),
        };
        assert_eq!(code(c.submit(0, "payments", cmd)), "invalid_bank_account", "{bad:?}");
    }
    // A benefit payment names no refund, and nothing is owed in benefit payable here.
    let benefit = |refund: Option<&str>| Command::SettlePayable {
        policy: id.clone(),
        payment: "pay-b".into(),
        payable: PayableKind::Benefit,
        refund: refund.map(str::to_string),
        amount: Money::new(10_00, NZD),
        bank_account: "nz-main-claims".into(),
    };
    assert_eq!(code(c.submit(0, "payments", benefit(Some("rf-1")))), "refund_not_expected");
    assert_eq!(code(c.submit(0, "payments", benefit(None))), "insufficient_payable");

    // A refund once fully paid cannot be raised again under the same ID.
    c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 50_00)).unwrap();
    assert_eq!(code(c.submit(0, "payments", raise(&id, "rf-1", 10_00))), "duplicate_refund");
    clean(&c);
}

/// Purpose: where the payer is the person a claim is for, a refund can be raised only with
/// that claim, exactly as an immediate refund can.
/// Why it matters: addendum entry 1 says such a refund is paid with the claim to the estate or
/// the beneficiary, never back to the deceased's account. The rule must hold on the payable
/// route as well as the immediate one, or the payments module could route around it.
#[test]
fn a_refund_on_a_deceased_payers_policy_is_raised_only_with_the_claim() {
    let mut c = cluster(1);
    let id = overpaid(&mut c);
    c.end_of_day().unwrap();
    c.submit(
        0,
        "claims",
        Command::TerminateOnClaim {
            policy: id.clone(),
            event_date: start(),
            claim: "clm-9".into(),
            payer_deceased: true,
        },
    )
    .unwrap();
    let suspense = balances(&c, 0, &id).0;
    assert!(suspense >= 100_00, "the unearned premium came back to suspense as well: {suspense}");
    let amount = Money::new(i64::try_from(suspense).unwrap(), NZD);
    let to_source = Command::RaiseRefundPayable {
        policy: id.clone(),
        refund: "rf-1".into(),
        amount,
        with_claim: None,
    };
    assert_eq!(code(c.submit(0, "payments", to_source)), "refund_with_claim");
    let other_claim = Command::RaiseRefundPayable {
        policy: id.clone(),
        refund: "rf-1".into(),
        amount,
        with_claim: Some("clm-other".into()),
    };
    assert_eq!(code(c.submit(0, "payments", other_claim)), "refund_with_claim");
    c.submit(
        0,
        "payments",
        Command::RaiseRefundPayable {
            policy: id.clone(),
            refund: "rf-1".into(),
            amount,
            with_claim: Some("clm-9".into()),
        },
    )
    .unwrap();
    let entry = c.nodes[&0].journal.iter().rev().find(|e| e.body.kind == "refund_payable_raised").unwrap();
    assert_eq!(entry.body.detail["with_claim"], "clm-9");
    assert!(c
        .take_events()
        .iter()
        .any(|e| matches!(&e.event, Event::RefundPayableRaised { with_claim: Some(claim), .. } if claim == "clm-9")));
    clean(&c);
}

/// Purpose: a policy that moves to another group takes a refund in flight with it: the refund
/// payable balance, what is owed per refund and the settled payments all arrive on the target,
/// where the refund can be settled and a payment returned.
/// Why it matters: an ownership change can happen while a refund waits for a payment run. If
/// the payable stayed behind, the source partition would hold money for a policy it no longer
/// has and the bank's settlement would have nowhere to post.
#[test]
fn a_refund_in_flight_moves_with_the_policy() {
    let mut c = cluster(2);
    let id = overpaid(&mut c);
    c.submit(0, "payments", raise(&id, "rf-1", 100_00)).unwrap();
    c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 30_00)).unwrap();
    let target = (1..).map(|n| GroupId(format!("grp-t-{n}"))).find(|g| c.partition_of(g) == 1).unwrap();
    c.submit(
        0,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t1".into(),
            target_group: target,
            target_partition: 1,
        },
    )
    .unwrap();
    assert!(matches!(c.nodes[&0].messages[0].body, MessageBody::TransferRecord(_)));
    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    assert!(c.relay_errors.is_empty(), "{:?}", c.relay_errors);
    assert_eq!(c.locate(&id), Some(1));
    clean(&c);

    assert_eq!(c.state(1).ledger.natural_balance(&accounts::refund_payable(&id, NZD).id), 70_00);
    assert_eq!(c.state(0).ledger.natural_balance(&accounts::refund_payable(&id, NZD).id), 0);
    assert!(c.state(0).policies[&id].refunds_raised.is_empty() && c.state(0).policies[&id].payments.is_empty());
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-2", Some("rf-1"), 70_00))), "policy_transferred");

    c.submit(1, "payments", settle(&id, "pay-2", Some("rf-1"), 70_00)).unwrap();
    // The first payment was settled on the old partition; its return posts here, where the
    // policy now lives, and still names the entry on the old partition's chain.
    c.submit(
        1,
        "payments",
        Command::ReturnPayment {
            policy: id.clone(),
            payment: "pay-1".into(),
        },
    )
    .unwrap();
    let reversal = c.nodes[&1].journal.iter().rev().find(|e| e.body.kind == "payment_returned").unwrap();
    assert!(reversal.body.detail["reverses"].starts_with("je-00-"));
    assert_eq!(c.state(1).ledger.natural_balance(&accounts::refund_payable(&id, NZD).id), 30_00);
    clean(&c);
}

/// Purpose: a policy with no refund in flight lands on its new partition without a refund
/// payable account, as before this account existed.
/// Why it matters: the account is opened on first use. Opening it on every landing would change
/// every transferred policy's state for no reason and bloat partitions with empty accounts.
#[test]
fn a_transfer_without_a_refund_opens_no_refund_account() {
    let mut c = cluster(2);
    let id = overpaid(&mut c);
    let target = (1..).map(|n| GroupId(format!("grp-t-{n}"))).find(|g| c.partition_of(g) == 1).unwrap();
    c.submit(
        0,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t1".into(),
            target_group: target,
            target_partition: 1,
        },
    )
    .unwrap();
    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    assert_eq!(c.locate(&id), Some(1));
    assert!(!c.state(1).ledger.has_account(&accounts::refund_payable(&id, NZD).id));
    assert!(c.state(1).ledger.has_account(&accounts::policy_suspense(&id, NZD).id));
    clean(&c);
}

/// Purpose: the reconciliation notices refund payable disagreeing with the refunds owed.
/// Why it matters: the rule is only worth having if it fails when the two differ. A check that
/// cannot fail would let a lost or doubled refund pass every daily close.
#[test]
fn reconciliation_catches_refund_payable_out_of_step() {
    let mut c = cluster(1);
    let id = overpaid(&mut c);
    c.submit(0, "payments", raise(&id, "rf-1", 100_00)).unwrap();
    let mut state = c.state(0).clone();
    state.policies.get_mut(&id).unwrap().refunds_raised.insert("rf-1".into(), 99_00);
    let breaks = ic_core::recon::reconcile(&state);
    assert_eq!(breaks.len(), 1, "{breaks:?}");
    assert_eq!(breaks[0].check, "refund_payable");
    assert!(breaks[0].detail.contains("ledger 10000, refunds owed 9900"), "{}", breaks[0].detail);
}

/// Purpose: settled payments older than the pruning horizon are dropped from policy state at
/// end-of-day, and a return for one is then refused for a person to handle.
/// Why it matters: policies last decades and state must stay bounded. A return a year after
/// settlement is not a routine bank return, and refusing it sends it to an exception queue
/// instead of reversing against a record that is no longer there.
#[test]
fn old_settled_payments_are_pruned_from_state() {
    let mut c = cluster(1);
    let id = overpaid(&mut c);
    c.submit(0, "payments", raise(&id, "rf-1", 100_00)).unwrap();
    c.submit(0, "payments", settle(&id, "pay-1", Some("rf-1"), 100_00)).unwrap();
    let mut premium = 2;
    for day in 0..402 {
        // Keep the policy in force: pay each month's premium as it falls due.
        if day % 28 == 0 {
            c.submit(
                0,
                "cash-matching",
                Command::ApplyReceipt {
                    policy: id.clone(),
                    receipt: format!("r{premium}"),
                    amount: Money::new(MONTHLY + 5_00, NZD),
                },
            )
            .unwrap();
            premium += 1;
        }
        c.end_of_day().unwrap();
    }
    assert_eq!(c.state(0).policies[&id].status, ic_core::PolicyStatus::InForce);
    assert!(c.state(0).policies[&id].payments.is_empty());
    let back = Command::ReturnPayment {
        policy: id.clone(),
        payment: "pay-1".into(),
    };
    assert_eq!(code(c.submit(0, "payments", back)), "unknown_payment");
    clean(&c);
}

/// Purpose: a refund that settled into the wrong account is put right in three steps. Raising
/// the debt makes the refund owed again to the right person and records what the holder owes
/// back; a recovery brings money in against the debt; the rest is written off to its own
/// expense account; and the right person can then be paid. Every refusal leaves the ledger
/// alone: a payment that never settled, more than the payment, a different debt under a used
/// ID, more recovered or written off than is outstanding, a receipt already used, and the
/// bank's ordinary return of a payment that has a debt against it. The books reconcile at
/// every step.
/// Why it matters: money sent to the wrong person is owed twice over, once by the insurer to
/// the customer who never got it and once by the stranger who did. With no account for the
/// second, the first can only be paid again by pretending the payment never settled, and
/// what was lost, recovered or written off is nowhere on the books.
#[test]
fn a_payment_to_the_wrong_person_is_owed_again_and_owed_back() {
    use ic_core::payee_debt::PayeeDebtCommand;
    let mut c = cluster(1);
    let id = overpaid(&mut c);
    let nzd = |minor| Money::new(minor, NZD);
    let send = |c: &mut Cluster, command: PayeeDebtCommand| c.submit(0, "payments", Command::PayeeDebt(Box::new(command)));
    let raise_debt = |debt: &str, payment: &str, minor: i64| PayeeDebtCommand::Raise {
        policy: id.clone(),
        debt: debt.to_string(),
        payment: payment.to_string(),
        amount: nzd(minor),
        party: Some(ic_core::PartyId("party-stranger".to_string())),
    };
    let recover = |debt: &str, receipt: &str, minor: i64| PayeeDebtCommand::Recover {
        policy: id.clone(),
        debt: debt.to_string(),
        receipt: receipt.to_string(),
        amount: nzd(minor),
    };
    let write_off = |debt: &str, minor: i64| PayeeDebtCommand::WriteOff {
        policy: id.clone(),
        debt: debt.to_string(),
        amount: nzd(minor),
        reason: "holder_untraceable".to_string(),
    };
    let debt_account = |c: &Cluster| c.state(0).ledger.balance(&accounts::payee_debt(&id, NZD).id);
    let refund_owed = |c: &Cluster| c.state(0).ledger.natural_balance(&accounts::refund_payable(&id, NZD).id);

    assert_eq!(code(send(&mut c, raise_debt("debt-1", "pay-1", 100_00))), "unknown_payment");
    c.submit(0, "payments", raise(&id, "ref-1", 100_00)).unwrap();
    c.submit(0, "payments", settle(&id, "pay-1", Some("ref-1"), 100_00)).unwrap();
    assert_eq!(refund_owed(&c), 0);
    let head = c.state(0).ledger.head();
    assert_eq!(code(send(&mut c, raise_debt("debt-1", "pay-1", 100_01))), "over_payment");
    assert_eq!(code(send(&mut c, raise_debt("debt 1", "pay-1", 100_00))), "invalid_debt");
    assert_eq!(code(send(&mut c, raise_debt("debt-1", "pay-1", 0))), "invalid_amount");
    assert_eq!(
        code(c.submit(0, "cash-matching", Command::PayeeDebt(Box::new(raise_debt("debt-1", "pay-1", 100_00))))),
        "caller_not_allowed"
    );
    assert_eq!(c.state(0).ledger.head(), head, "a refusal posts nothing");

    // The whole payment went astray: owed again to the customer, owed back by the holder.
    send(&mut c, raise_debt("debt-1", "pay-1", 100_00)).unwrap();
    send(&mut c, raise_debt("debt-1", "pay-1", 100_00)).unwrap();
    assert_eq!((refund_owed(&c), debt_account(&c)), (100_00, 100_00));
    assert_eq!(c.state(0).policies[&id].refunds_raised["ref-1"], 100_00);
    clean(&c);
    assert_eq!(code(send(&mut c, raise_debt("debt-1", "pay-1", 50_00))), "duplicate_debt");
    assert_eq!(code(send(&mut c, raise_debt("debt-2", "pay-1", 1))), "over_payment");
    assert_eq!(
        code(c.submit(
            0,
            "payments",
            Command::ReturnPayment {
                policy: id.clone(),
                payment: "pay-1".into()
            }
        )),
        "payment_has_debt"
    );

    // The bank's recall brings back 60.00.
    assert_eq!(code(send(&mut c, recover("debt-9", "recall-1", 60_00))), "unknown_debt");
    assert_eq!(code(send(&mut c, recover("debt-1", "r1", 60_00))), "duplicate_receipt");
    assert_eq!(code(send(&mut c, recover("debt-1", "recall-1", 100_01))), "over_recovery");
    send(&mut c, recover("debt-1", "recall-1", 60_00)).unwrap();
    send(&mut c, recover("debt-1", "recall-1", 60_00)).unwrap();
    assert_eq!(code(send(&mut c, recover("debt-1", "recall-1", 10_00))), "duplicate_receipt");
    assert_eq!(debt_account(&c), 40_00);
    clean(&c);

    // The rest will not come back.
    assert_eq!(code(send(&mut c, write_off("debt-1", 40_01))), "over_recovery");
    send(&mut c, write_off("debt-1", 40_00)).unwrap();
    assert_eq!(debt_account(&c), 0);
    let lost = c.state(0).ledger.balance(&accounts::payee_debt_written_off("term-life", NZD).id);
    assert_eq!(lost, 40_00);
    let record = &c.state(0).policies[&id].payee_debts["debt-1"];
    assert_eq!((record.raised_minor, record.outstanding_minor, record.written_off_minor), (100_00, 0, 40_00));
    clean(&c);

    // And the customer is paid, this time to the right account.
    c.submit(0, "payments", settle(&id, "pay-2", Some("ref-1"), 100_00)).unwrap();
    assert_eq!(refund_owed(&c), 0);
    clean(&c);
    let outstanding: Vec<i64> = c.nodes[&0]
        .outbox
        .iter()
        .filter_map(|e| match &e.event {
            Event::PayeeDebtRaised { outstanding_minor, .. }
            | Event::PayeeDebtRecovered { outstanding_minor, .. }
            | Event::PayeeDebtWrittenOff { outstanding_minor, .. } => Some(*outstanding_minor),
            _ => None,
        })
        .collect();
    assert_eq!(outstanding, vec![100_00, 40_00, 0]);
}

/// Purpose: tax withheld on a partition is discharged against the bank account when
/// payments pays the revenue authority: the liability falls by the amount, the bank is
/// credited, the event carries what is still held, a repeat changes nothing, and the same
/// payment on other terms, more than the partition holds, a bad period or account and a
/// return of an unknown payment are refused. When the bank sends the payment back, the
/// liability is on again in full and a second return changes nothing.
/// Why it matters: withheld tax is other people's money owed to Inland Revenue. If paying it
/// did not clear the liability the books would show tax owed that was paid; if a partition
/// could remit more than it withheld, its liability would go into debit and hide a shortfall
/// elsewhere; and a returned payment not put back would leave tax unpaid and nobody chasing it.
#[test]
fn withheld_tax_paid_to_the_revenue_is_discharged_and_comes_back_if_the_bank_returns_it() {
    use ic_core::counterparty::{CounterpartyAccount, CounterpartyLine, CounterpartyPosting, InternalLedger, PartyLedger};
    use ic_core::remittance::Remittance;
    let mut c = cluster(1);
    // Commission earned with 20.00 of schedular tax kept back.
    let earned = CounterpartyPosting {
        reference: "stmt-1".into(),
        kind: "commission_paid".into(),
        currency: NZD,
        value_date: None,
        policy: None,
        reverses: None,
        lines: vec![
            CounterpartyLine {
                account: CounterpartyAccount::Party {
                    party: ic_core::PartyId("adviser-1".into()),
                    ledger: PartyLedger::CommissionPayable,
                },
                amount_minor: 20_00,
            },
            CounterpartyLine {
                account: CounterpartyAccount::Internal(InternalLedger::WithholdingTax { kind: "schedular".into() }),
                amount_minor: -20_00,
            },
        ],
        detail: Default::default(),
    };
    c.submit(0, "distribution", Command::PostCounterparty(Box::new(earned))).unwrap();
    let payable = accounts::withholding_tax_payable("schedular", NZD).id;
    let bank = accounts::bank("main", NZD).id;
    let held = |c: &Cluster| c.state(0).ledger.natural_balance(&payable);
    assert_eq!(held(&c), 20_00);
    let by_fund = |c: &Cluster| {
        ic_core::remittance::held(&c.state(0).withheld, "schedular", NZD)
            .into_iter()
            .map(|h| (h.statutory_fund, h.minor))
            .collect::<Vec<_>>()
    };
    assert_eq!(by_fund(&c), vec![(None, 20_00)]);
    // No fund pays another's tax: nothing was withheld in SF1.
    let wrong_fund = Command::RemitWithholding(Box::new(Remittance {
        kind: "schedular".into(),
        period: "2027-11".into(),
        payment: "ir-0".into(),
        amount: Money::new(1_00, NZD),
        bank_account: "main".into(),
        statutory_fund: Some("SF1".into()),
    }));
    assert_eq!(code(c.submit(0, "payments", wrong_fund)), "over_remittance");
    let remit = |payment: &str, minor: i64, period: &str, account: &str| {
        Command::RemitWithholding(Box::new(Remittance {
            kind: "schedular".into(),
            period: period.into(),
            payment: payment.into(),
            amount: Money::new(minor, NZD),
            bank_account: account.into(),
            statutory_fund: None,
        }))
    };
    let send = |c: &mut Cluster, command: Command| c.submit(0, "payments", command);
    let back = |payment: &str| Command::ReturnRemittance {
        kind: "schedular".into(),
        payment: payment.into(),
    };

    assert_eq!(code(send(&mut c, remit("ir-1", 20_01, "2027-11", "main"))), "over_remittance");
    assert_eq!(code(send(&mut c, remit("ir-1", 0, "2027-11", "main"))), "invalid_remittance");
    assert_eq!(code(send(&mut c, remit("ir-1", 15_00, "Nov 2027", "main"))), "invalid_remittance");
    assert_eq!(code(send(&mut c, remit("ir 1", 15_00, "2027-11", "main"))), "invalid_remittance");
    assert_eq!(code(send(&mut c, remit("ir-1", 15_00, "2027-11", "main account"))), "invalid_bank_account");
    assert_eq!(code(send(&mut c, back("ir-1"))), "unknown_payment");
    assert_eq!(held(&c), 20_00);

    c.take_events();
    send(&mut c, remit("ir-1", 15_00, "2027-11", "main")).unwrap();
    send(&mut c, remit("ir-1", 15_00, "2027-11", "main")).unwrap();
    assert_eq!(code(send(&mut c, remit("ir-1", 5_00, "2027-11", "main"))), "duplicate_payment");
    assert_eq!((held(&c), c.state(0).ledger.natural_balance(&bank)), (5_00, -15_00));
    assert_eq!(by_fund(&c), vec![(None, 5_00)]);
    let events = c.take_events();
    let remitted: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::WithholdingRemitted {
                payment,
                amount_minor,
                outstanding_minor,
                ..
            } => Some((payment.as_str(), *amount_minor, *outstanding_minor)),
            _ => None,
        })
        .collect();
    assert_eq!(remitted, vec![("ir-1", 15_00, 5_00)]);
    assert_eq!(code(send(&mut c, remit("ir-2", 5_01, "2027-11", "main"))), "over_remittance");

    // The bank sends it back: owed again, once.
    send(&mut c, back("ir-1")).unwrap();
    send(&mut c, back("ir-1")).unwrap();
    assert_eq!((held(&c), c.state(0).ledger.natural_balance(&bank)), (20_00, 0));
    assert!(c
        .take_events()
        .iter()
        .any(|e| matches!(&e.event, Event::WithholdingRemittanceReturned { payment, outstanding_minor: 20_00, .. } if payment == "ir-1")));
    // A returned payment's ID is spent; the tax is paid again under a new one.
    assert_eq!(code(send(&mut c, remit("ir-1", 15_00, "2027-11", "main"))), "duplicate_payment");
    send(&mut c, remit("ir-3", 20_00, "2027-11", "main")).unwrap();
    assert_eq!(held(&c), 0);
    assert!(by_fund(&c).is_empty() && c.state(0).withheld.is_empty());
    clean(&c);
}
