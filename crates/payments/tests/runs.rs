//! Payment and collection runs, from preparation to close.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::PayableKind;
use ic_payments::model::{AuthorityStatus, Counterparty, Instruction, ItemStatus, NameCheck, PayeeKind, ReturnReason, RunStatus, RunType, SubmissionStatus};
use ic_payments::query::answer;
use ic_payments::{Command, Event};
use serde_json::json;

fn run(id: &str) -> String {
    id.to_string()
}

fn ack(submission: &str, rejected: &[(&str, &str)]) -> Command {
    Command::RecordAcknowledgement {
        submission: submission.to_string(),
        refused: None,
        rejected: rejected.iter().map(|(i, r)| (i.to_string(), r.to_string())).collect(),
    }
}

/// A claim payment of policy money to a payee.
fn claim(item: &str, payee: &str, pol: &str, amount: i64) -> ic_payments::model::NewItem {
    let mut n = pay(item, payee, amount);
    n.policy = Some(policy(pol));
    n.payable = Some(PayableKind::Benefit);
    n
}

/// Purpose: a claims run goes from prepared to closed, and at settlement the
/// module tells the core to discharge each item's payable from the ledger
/// account that mirrors the bank account the money left, and expects one
/// debit for the file on the statement.
/// Why it matters: this is the path every claim payment takes. The ledger
/// must show the benefit as owed until the bank confirms it, and paid from
/// the right statutory fund's account afterwards.
#[test]
fn a_claims_run_is_paid_settled_and_closed() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.payee("p2", PayeeKind::Provider, "0207654321");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![claim("i1", "p1", "pol-1", 50_000_00), pay("i2", "p2", 1_200_00)]);
    let submission = desk.approve("r1");
    assert_eq!(submission, "r1-1");
    assert!(desk.state.submission_in_progress());
    let s = &desk.state.submissions["r1-1"];
    assert_eq!((s.channel.as_str(), s.account.as_str()), ("anz-file", "anz-sf1-claims"));

    let events = desk.settle(&submission, d(2027, 11, 15));
    assert!(!desk.state.submission_in_progress());
    assert!(events.contains(&Event::ItemSettled {
        item: "i2".to_string(),
        source: "source-i2".to_string(),
        amount_minor: 1_200_00
    }));
    assert!(events.contains(&Event::SubmissionSettled {
        submission: "r1-1".to_string(),
        total_minor: 51_200_00
    }));
    // Only the policy money goes to the core.
    let instructions: Vec<_> = desk.state.outbox.values().collect();
    assert_eq!(instructions.len(), 1);
    assert_eq!(instructions[0].key, "settle-i1");
    let Instruction::Core { caller, command } = &instructions[0].instruction else {
        panic!("a core instruction")
    };
    assert_eq!(caller, "payments");
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"settle_payable": {"policy": "pol-1", "payment": "i1", "payable": "benefit",
               "amount": {"minor": 50_000_00, "currency": "NZD"}, "bank_account": "anz-sf1-claims"}})
    );
    assert_eq!(desk.state.books["anz-sf1-claims"].expected["r1-1"].amount_minor, -51_200_00);
    assert_eq!(desk.state.available("anz-sf1-claims"), Some(10_000_000_00 - 51_200_00));

    let totals = desk.state.control_totals("r1").unwrap();
    assert_eq!(totals["settled"], 51_200_00);
    assert_eq!(totals["acknowledged"], 51_200_00);
    desk.ok(&mia(), Command::CloseRun { run: run("r1") });
    assert_eq!(desk.state.runs["r1"].status, RunStatus::Closed);
    let done = desk.state.outbox.keys().next().copied().unwrap();
    desk.ok(&svc(), Command::CompleteInstruction { entry: done, refused: None });
    assert!(desk.state.outbox.is_empty());
    assert_eq!(
        desk.code(&svc(), Command::CompleteInstruction { entry: done, refused: None }),
        "unknown_instruction"
    );
}

/// Purpose: the person who prepared a run, or added to it, cannot approve
/// it; an approver approves once; a run with held items is not approved.
/// Why it matters: maker and checker being different people is the control
/// that stops one person paying themselves.
#[test]
fn the_maker_never_approves_their_own_run() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00)]);
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("r1") }), "wrong_state");
    desk.ok(&max(), Command::CheckRun { run: run("r1") });
    assert_eq!(desk.code(&mia(), Command::ApproveRun { run: run("r1") }), "not_permitted");
    for maker in ["mia", "max"] {
        let both = person(maker, &[ic_payments::Role::Approver]);
        assert_eq!(desk.code(&both, Command::ApproveRun { run: run("r1") }), "same_person");
    }
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("zz") }), "unknown_run");
    let events = desk.ok(&ari(), Command::ApproveRun { run: run("r1") });
    assert!(events.contains(&Event::RunApproval {
        run: run("r1"),
        by: "ari".to_string(),
        approved: true
    }));
    assert_eq!(desk.code(&ben(), Command::ApproveRun { run: run("r1") }), "wrong_state");
}

/// Purpose: an ex gratia run needs two different approvers, and the same
/// approver cannot supply both.
/// Why it matters: a payment outside the policy terms has no entitlement
/// behind it to check against, so it takes two people to release.
#[test]
fn a_run_type_can_need_two_approvers() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::ExGratia, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00)]);
    desk.ok(&mia(), Command::CheckRun { run: run("r1") });
    let first = desk.ok(&ari(), Command::ApproveRun { run: run("r1") });
    assert_eq!(first.len(), 1, "one approval queues nothing");
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("r1") }), "already_approved");
    assert!(desk.state.submissions.is_empty());
    desk.ok(&ben(), Command::ApproveRun { run: run("r1") });
    assert_eq!(desk.state.runs["r1"].status, RunStatus::Approved);
    assert_eq!(desk.state.submissions.len(), 1);
}

/// Purpose: the check holds an item whose payee cannot be paid, an item over
/// the item limit, and an item that looks like a payment already made; the
/// run is not approved until each is removed or, for a possible duplicate
/// only, released by an approver who did not prepare the run.
/// Why it matters: these are the controls that stop a payment to an
/// unverified account, an oversized payment, and the same payment twice.
#[test]
fn the_check_holds_what_must_not_be_paid() {
    let mut desk = Desk::new();
    desk.payee("good", PayeeKind::Customer, "0101234567");
    desk.payee("twin", PayeeKind::Customer, "0101234567");
    desk.ok(
        &pam(),
        Command::RegisterPayee {
            payee: "raw".to_string(),
            party: "x".to_string(),
            kind: PayeeKind::Customer,
            account: account("Raw", "0309999999"),
            name_check: NameCheck::Match,
        },
    );
    // An earlier run paid the same account the same amount two days before.
    desk.prepare("r0", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r0", vec![pay("i0", "good", 700_00)]);
    desk.approve("r0");

    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 16));
    desk.add(
        "r1",
        vec![
            pay("i1", "raw", 100_00),
            pay("i2", "good", 100_000_01),
            pay("i3", "twin", 700_00),
            pay("i4", "good", 5_00),
        ],
    );
    let events = desk.ok(&mia(), Command::CheckRun { run: run("r1") });
    assert_eq!(events, vec![Event::RunChecked { run: run("r1"), held: 3 }]);
    let status = |desk: &Desk, item: &str| desk.state.runs["r1"].items[item].status.clone();
    assert_eq!(status(&desk, "i1"), ItemStatus::Held("payee_not_confirmed".to_string()));
    assert_eq!(status(&desk, "i2"), ItemStatus::Held("over_item_limit".to_string()));
    assert_eq!(status(&desk, "i3"), ItemStatus::Held("possible_duplicate".to_string()));
    assert_eq!(status(&desk, "i4"), ItemStatus::Pending);
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("r1") }), "items_held");

    let release = |item: &str| Command::ReleaseItem {
        run: run("r1"),
        item: item.to_string(),
    };
    assert_eq!(desk.code(&mia(), release("i3")), "not_permitted");
    assert_eq!(desk.code(&person("mia", &[ic_payments::Role::Approver]), release("i3")), "same_person");
    assert_eq!(desk.code(&ari(), release("i2")), "not_releasable");
    assert_eq!(desk.code(&ari(), release("zz")), "unknown_item");
    desk.ok(&ari(), release("i3"));
    assert_eq!(status(&desk, "i3"), ItemStatus::Pending);

    desk.ok(
        &mia(),
        Command::RemoveRunItem {
            run: run("r1"),
            item: "i1".to_string(),
        },
    );
    desk.ok(
        &mia(),
        Command::RemoveRunItem {
            run: run("r1"),
            item: "i2".to_string(),
        },
    );
    assert_eq!(desk.state.runs["r1"].status, RunStatus::Prepared);
    // The release survives the second check: the approver has decided.
    let events = desk.ok(&mia(), Command::CheckRun { run: run("r1") });
    assert_eq!(events, vec![Event::RunChecked { run: run("r1"), held: 0 }]);
    desk.ok(&ari(), Command::ApproveRun { run: run("r1") });
    assert_eq!(desk.state.submissions["r1-1"].items, vec!["i3", "i4"]);
}

/// Purpose: an item is refused at the door when it reuses an item ID, pays a
/// source that is already being paid, has no amount, names an unknown payee,
/// or mixes up policy money and its payable; and a batch that repeats itself
/// is refused whole.
/// Why it matters: duplicate protection starts with the request. Two items
/// for the same claim payment must never both reach a bank.
#[test]
fn an_item_that_duplicates_or_contradicts_is_refused() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.authority("a1", "pol-1", "0401234567", false);
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00)]);
    let add = |items| Command::AddRunItems { run: run("r1"), items };
    assert_eq!(desk.code(&ari(), add(vec![pay("i2", "p1", 1)])), "not_permitted");
    assert_eq!(desk.code(&mia(), Command::AddRunItems { run: run("zz"), items: vec![] }), "unknown_run");
    assert_eq!(desk.code(&mia(), add(vec![pay("i1", "p1", 100_00)])), "duplicate_item");
    let mut same_source = pay("i2", "p1", 100_00);
    same_source.source = "source-i1".to_string();
    assert_eq!(desk.code(&mia(), add(vec![same_source])), "duplicate_source");
    assert_eq!(desk.code(&mia(), add(vec![pay("i2", "p1", 0)])), "invalid_item");
    assert_eq!(desk.code(&mia(), add(vec![pay("i2", "nobody", 5)])), "unknown_payee");
    assert_eq!(desk.code(&mia(), add(vec![pay("i2", "p1", 5), pay("i2", "p1", 6)])), "duplicate_item");
    let mut no_payable = pay("i2", "p1", 5);
    no_payable.policy = Some(policy("pol-1"));
    assert_eq!(desk.code(&mia(), add(vec![no_payable])), "invalid_item");
    let mut refund_without_id = claim("i2", "p1", "pol-1", 5);
    refund_without_id.payable = Some(PayableKind::Refund);
    assert_eq!(desk.code(&mia(), add(vec![refund_without_id])), "invalid_item");
    let mut to_authority = pay("i2", "p1", 5);
    to_authority.counterparty = Counterparty::Authority("a1".to_string());
    assert_eq!(desk.code(&mia(), add(vec![to_authority])), "invalid_item");
    let mut represents = pay("i2", "p1", 5);
    represents.represents = Some("i1".to_string());
    assert_eq!(desk.code(&mia(), add(vec![represents])), "invalid_item");
    let mut unknown_refund = claim("i2", "p1", "pol-1", 5);
    unknown_refund.payable = Some(PayableKind::Refund);
    unknown_refund.refund = Some("nope".to_string());
    assert_eq!(desk.code(&mia(), add(vec![unknown_refund])), "unknown_refund");
    assert_eq!(desk.state.runs["r1"].items.len(), 1);
}

/// Purpose: a run is refused when it is dated in the past, has no route or
/// account, is empty, is over its run limit or would take the day over its
/// day limit.
/// Why it matters: limits cap what one mistake or one compromised approver
/// can release in a day.
#[test]
fn run_and_day_limits_cap_what_can_be_released() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Reinsurer, "0101234567");
    let prepare = |id: &str, fund: &str, on, urgent, run_type| Command::PrepareRun {
        run: run(id),
        run_type,
        fund: fund.to_string(),
        currency: NZD,
        value_date: on,
        urgent,
    };
    let reinsurer = RunType::Reinsurer;
    assert_eq!(desk.code(&ari(), prepare("r1", "sf1", d(2027, 11, 15), false, reinsurer)), "not_permitted");
    assert_eq!(desk.code(&mia(), prepare("r1", "sf1", d(2027, 11, 12), false, reinsurer)), "value_date_passed");
    assert_eq!(desk.code(&mia(), prepare("r1", "sf2", d(2027, 11, 15), false, reinsurer)), "no_account");
    assert_eq!(
        desk.code(&mia(), prepare("r1", "sf1", d(2027, 11, 15), true, RunType::Collection)),
        "no_urgent_channel"
    );
    desk.prepare("r1", reinsurer, d(2027, 11, 15));
    assert_eq!(desk.code(&mia(), prepare("r1", "sf1", d(2027, 11, 15), false, reinsurer)), "duplicate_run");
    assert_eq!(desk.code(&mia(), Command::CheckRun { run: run("r1") }), "empty_run");
    let items = |prefix: &str, n: usize| (0..n).map(|i| pay(&format!("{prefix}{i}"), "p1", 90_000_00 + i as i64)).collect::<Vec<_>>();
    desk.add("r1", items("a", 6));
    assert_eq!(desk.code(&mia(), Command::CheckRun { run: run("r1") }), "over_run_limit");
    desk.ok(
        &mia(),
        Command::RemoveRunItem {
            run: run("r1"),
            item: "a5".to_string(),
        },
    );
    desk.approve("r1");
    // 450,000 is approved for the day. Another 450,000 would pass 800,000.
    desk.prepare("r2", reinsurer, d(2027, 11, 15));
    desk.add("r2", (0..5).map(|i| pay(&format!("b{i}"), "p1", 80_000_00 + i)).collect());
    desk.ok(&mia(), Command::CheckRun { run: run("r2") });
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("r2") }), "over_day_limit");
    // A large run is pre-advised to treasury.
    let notice = desk.state.outbox.values().find(|e| e.key == "pre-advice-r1").unwrap();
    assert!(matches!(
        &notice.instruction,
        Instruction::Notify {
            to: ic_payments::Role::Treasury,
            ..
        }
    ));
}

/// Purpose: an outbound run is not approved against an account with no
/// statement, or one that will not hold enough once everything already on
/// its way has gone.
/// Why it matters: a file the account cannot cover is rejected whole by the
/// bank, and every claimant in it is paid late.
#[test]
fn a_run_is_not_released_without_the_funds_to_cover_it() {
    let mut desk = Desk::bare();
    desk.ok(
        &cal(),
        Command::ActivateConfig {
            version: 1,
            config: Box::new(config()),
        },
    );
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::Commission, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 60_000_00)]);
    desk.ok(&mia(), Command::CheckRun { run: run("r1") });
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("r1") }), "balance_unknown");
    desk.statement("anz-sf1-third", d(2027, 11, 12), 100_000_00, &[]);
    desk.ok(&ari(), Command::ApproveRun { run: run("r1") });
    assert_eq!(desk.state.available("anz-sf1-third"), Some(40_000_00));
    desk.prepare("r2", RunType::TaxAndLevies, d(2027, 11, 15));
    desk.add("r2", vec![pay("i2", "p1", 40_000_01)]);
    desk.ok(&mia(), Command::CheckRun { run: run("r2") });
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("r2") }), "insufficient_funds");
}

/// Purpose: a run is not approved after the channel's cutoff on its value
/// date, or for a weekend or a bank holiday.
/// Why it matters: a file sent after cutoff is processed a day late while
/// the ledger and the customer were told it was paid today.
#[test]
fn the_bank_cutoff_and_calendar_are_respected() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    let ready = |desk: &mut Desk, id: &str, on| {
        desk.prepare(id, RunType::BenefitPayments, on);
        desk.add(id, vec![pay(&format!("i-{id}"), "p1", 100_00 + id.len() as i64)]);
        desk.ok(&mia(), Command::CheckRun { run: run(id) });
    };
    ready(&mut desk, "sat", d(2027, 11, 20));
    ready(&mut desk, "holiday", d(2027, 11, 17));
    ready(&mut desk, "today", d(2027, 11, 15));
    ready(&mut desk, "tomorrow-x", d(2027, 11, 16));
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("sat") }), "after_cutoff");
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("holiday") }), "after_cutoff");
    desk.at(t(2027, 11, 15, 16, 1));
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("today") }), "after_cutoff");
    desk.ok(&ari(), Command::ApproveRun { run: run("tomorrow-x") });
}

/// Purpose: an urgent run goes by the API channel at any hour, and only
/// when two different people on the on-call list approve it.
/// Why it matters: an urgent claim payment at night is legitimate and also
/// exactly what a fraudster would ask for. Two named people must agree.
#[test]
fn an_urgent_run_needs_two_on_call_approvers() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.at(t(2027, 11, 20, 23, 30));
    desk.ok(
        &mia(),
        Command::PrepareRun {
            run: run("u1"),
            run_type: RunType::LumpSumClaims,
            fund: "sf1".to_string(),
            currency: NZD,
            value_date: d(2027, 11, 20),
            urgent: true,
        },
    );
    desk.add("u1", vec![pay("i1", "p1", 9_000_00)]);
    desk.ok(&mia(), Command::CheckRun { run: run("u1") });
    let off_duty = person("zoe", &[ic_payments::Role::Approver]);
    assert_eq!(desk.code(&off_duty, Command::ApproveRun { run: run("u1") }), "not_on_call");
    desk.ok(&ari(), Command::ApproveRun { run: run("u1") });
    assert_eq!(desk.state.runs["u1"].status, RunStatus::Checked);
    desk.ok(&ben(), Command::ApproveRun { run: run("u1") });
    assert_eq!(desk.state.submissions["u1-1"].channel, "anz-api");
}

/// Purpose: a driver takes a queued submission once, from a region that is
/// provisioned today, and the module records what was sent.
/// Why it matters: two drivers sending the same file is a double payment.
/// A driver in a region whose certificate has lapsed must be told before it
/// tries, not by the bank's silence.
#[test]
fn a_submission_is_taken_once_from_a_provisioned_region() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00)]);
    let s = desk.approve("r1");
    let begin = |region: &str| Command::BeginSubmission {
        submission: "r1-1".to_string(),
        region: region.to_string(),
    };
    let sent = || Command::RecordSubmitted {
        submission: "r1-1".to_string(),
        content_hash: "abc".to_string(),
        bank_reference: None,
    };
    assert_eq!(desk.code(&mia(), begin("akl")), "not_permitted");
    assert_eq!(desk.code(&svc(), begin("lax")), "region_not_provisioned");
    assert_eq!(desk.code(&svc(), sent()), "wrong_state");
    assert_eq!(answer(&desk.state, json!({"q": "queued_submissions"})).unwrap().as_array().unwrap().len(), 1);
    desk.ok(&svc(), begin("syd"));
    assert_eq!(desk.code(&svc(), begin("akl")), "wrong_state");
    assert_eq!(
        answer(&desk.state, json!({"q": "unconfirmed_submissions"})).unwrap().as_array().unwrap().len(),
        1
    );
    assert_eq!(answer(&desk.state, json!({"q": "submission_in_progress"})).unwrap(), json!(true));
    assert_eq!(
        answer(&desk.state, json!({"failback_quiet": true})).unwrap(),
        json!(false),
        "leaders stay put while a file is going out"
    );
    desk.ok(&svc(), sent());
    assert_eq!(desk.state.submissions[&s].region.as_deref(), Some("syd"));
    assert_eq!(desk.state.submissions[&s].status, SubmissionStatus::Sent);
    assert_eq!(answer(&desk.state, json!({"q": "submission_in_progress"})).unwrap(), json!(false));
    assert_eq!(answer(&desk.state, json!({"failback_quiet": true})).unwrap(), json!(true));
    assert_eq!(
        desk.code(
            &svc(),
            Command::BeginSubmission {
                submission: "zz".to_string(),
                region: "akl".to_string()
            }
        ),
        "unknown_submission"
    );
    // After the certificate lapses nothing is taken from that region.
    desk.prepare("r2", RunType::BenefitPayments, d(2028, 7, 3));
    desk.add("r2", vec![pay("i2", "p1", 200_00)]);
    desk.at(t(2028, 7, 3, 9, 0));
    desk.statement("anz-sf1-claims", d(2028, 6, 30), 10_000_000_00, &[]);
    desk.approve("r2");
    assert_eq!(
        desk.code(
            &svc(),
            Command::BeginSubmission {
                submission: "r2-1".to_string(),
                region: "akl".to_string()
            }
        ),
        "region_not_provisioned"
    );
}

/// Purpose: when the bank refuses a whole file, or confirms it never
/// arrived, the items are unpaid again and go in a new submission with a
/// new ID; the failed one is never sent again.
/// Why it matters: resending a file under the same ID after a timeout is
/// how a whole run gets paid twice. The rule is: ask the bank, then send
/// what is unpaid as something new.
#[test]
fn a_failed_file_is_replaced_never_resent() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00), pay("i2", "p1", 200_00)]);
    let first = desk.approve("r1");
    let resubmit = |on| Command::Resubmit {
        run: run("r1"),
        value_date: on,
    };
    assert_eq!(desk.code(&mia(), resubmit(d(2027, 11, 15))), "wrong_state");
    desk.deliver(&first);
    assert_eq!(
        desk.code(
            &svc(),
            Command::RecordNotReceived {
                submission: first.clone(),
                reason: "x".to_string()
            }
        ),
        "wrong_state"
    );
    let events = desk.ok(
        &svc(),
        Command::RecordAcknowledgement {
            submission: first.clone(),
            refused: Some("signature_invalid".to_string()),
            rejected: BTreeMap::new(),
        },
    );
    assert_eq!(
        events,
        vec![Event::SubmissionFailed {
            submission: first.clone(),
            reason: "signature_invalid".to_string()
        }]
    );
    assert_eq!(desk.state.runs["r1"].items["i1"].status, ItemStatus::Pending);
    assert_eq!(desk.code(&svc(), ack(&first, &[])), "wrong_state");
    assert_eq!(desk.code(&mia(), Command::CloseRun { run: run("r1") }), "run_not_finished");

    assert_eq!(desk.code(&ari(), resubmit(d(2027, 11, 15))), "not_permitted");
    assert_eq!(desk.code(&mia(), resubmit(d(2027, 11, 14))), "value_date_passed");
    desk.at(t(2027, 11, 15, 17, 0));
    assert_eq!(desk.code(&mia(), resubmit(d(2027, 11, 15))), "after_cutoff");
    desk.ok(&mia(), resubmit(d(2027, 11, 16)));
    assert_eq!(desk.state.runs["r1"].submissions, vec!["r1-1", "r1-2"]);
    assert_eq!(desk.state.runs["r1"].value_date, d(2027, 11, 16));
    // The second never arrives either: the bank confirms it holds nothing.
    desk.ok(
        &svc(),
        Command::BeginSubmission {
            submission: "r1-2".to_string(),
            region: "mel".to_string(),
        },
    );
    desk.ok(
        &svc(),
        Command::RecordNotReceived {
            submission: "r1-2".to_string(),
            reason: "not held by bank".to_string(),
        },
    );
    // One item is withdrawn rather than sent a third time.
    desk.ok(
        &mia(),
        Command::RemoveRunItem {
            run: run("r1"),
            item: "i2".to_string(),
        },
    );
    assert_eq!(desk.state.runs["r1"].status, RunStatus::Approved);
    desk.ok(&mia(), resubmit(d(2027, 11, 16)));
    desk.settle("r1-3", d(2027, 11, 16));
    assert_eq!(desk.state.submissions["r1-3"].items, vec!["i1"]);
    desk.ok(&mia(), Command::CloseRun { run: run("r1") });
}

/// Purpose: an acknowledgement proves the bank holds a file even when the
/// driver that sent it died before saying so; items the bank refuses are
/// rejected and can be paid again later, the rest wait for settlement; an
/// acknowledgement naming an item that was never in the file is refused.
/// Why it matters: submitted must equal acknowledged plus rejected, item by
/// item, or money is in limbo with nobody looking for it.
#[test]
fn a_partial_acknowledgement_accounts_for_every_item() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00), pay("i2", "p1", 200_00)]);
    let s = desk.approve("r1");
    assert_eq!(desk.code(&svc(), ack(&s, &[])), "wrong_state");
    desk.ok(
        &svc(),
        Command::BeginSubmission {
            submission: s.clone(),
            region: "akl".to_string(),
        },
    );
    assert_eq!(desk.code(&svc(), ack(&s, &[("i9", "x")])), "unknown_item");
    assert_eq!(desk.code(&mia(), ack(&s, &[])), "not_permitted");
    assert_eq!(
        desk.code(
            &svc(),
            Command::RecordSettlement {
                submission: s.clone(),
                settled_on: d(2027, 11, 15)
            }
        ),
        "wrong_state"
    );
    let events = desk.ok(&svc(), ack(&s, &[("i2", "account_invalid")]));
    assert_eq!(
        events,
        vec![Event::SubmissionAcknowledged {
            submission: s.clone(),
            accepted: 1,
            rejected: vec!["i2".to_string()]
        }]
    );
    let totals = answer(&desk.state, json!({"q": "control_totals", "run": "r1"})).unwrap();
    assert_eq!(
        (totals["rejected"].clone(), totals["awaiting_settlement"].clone()),
        (json!(200_00), json!(100_00))
    );
    assert_eq!(
        desk.code(
            &mia(),
            Command::RemoveRunItem {
                run: run("r1"),
                item: "i1".to_string()
            }
        ),
        "wrong_state"
    );
    // The rejected payment can go in a later run under a new item ID.
    desk.prepare("r2", RunType::BenefitPayments, d(2027, 11, 16));
    let mut again = pay("i3", "p1", 200_00);
    again.source = "source-i2".to_string();
    desk.add("r2", vec![again]);
    desk.ok(
        &svc(),
        Command::RecordSettlement {
            submission: s.clone(),
            settled_on: d(2027, 11, 15),
        },
    );
    assert_eq!(desk.state.books["anz-sf1-claims"].expected[&s].amount_minor, -100_00);
    assert_eq!(
        desk.code(
            &svc(),
            Command::RecordSettlement {
                submission: s,
                settled_on: d(2027, 11, 15)
            }
        ),
        "wrong_state"
    );
    desk.ok(&mia(), Command::CloseRun { run: run("r1") });
    assert_eq!(desk.code(&mia(), Command::CloseRun { run: run("r1") }), "wrong_state");
}

/// Purpose: a settled payment the bank sends back is owed again: the core is
/// told to reverse the settlement, the statement is expected to show the
/// credit, a payee whose account is unusable is marked so it cannot be paid
/// until its details change, and people are told.
/// Why it matters: a returned claim payment is money still owed to a
/// claimant. It must reappear as unpaid, not vanish into a bank account.
#[test]
fn a_returned_payment_is_owed_again() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![claim("i1", "p1", "pol-1", 80_000_00)]);
    let s = desk.approve("r1");
    let back = |item: &str| Command::RecordReturn {
        item: item.to_string(),
        reason: ReturnReason::AccountClosed,
        returned_on: d(2027, 11, 17),
    };
    assert_eq!(desk.code(&svc(), back("i1")), "wrong_state");
    assert_eq!(desk.code(&svc(), back("zz")), "unknown_item");
    assert_eq!(desk.code(&mia(), back("i1")), "not_permitted");
    desk.settle(&s, d(2027, 11, 15));
    let events = desk.ok(&svc(), back("i1"));
    assert!(matches!(&events[0], Event::ItemReturned { item, .. } if item == "i1"));
    assert_eq!(desk.code(&svc(), back("i1")), "wrong_state");
    let keys: Vec<_> = desk.state.outbox.values().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["settle-i1", "return-i1", "returned-i1"]);
    let Instruction::Core { command, .. } = &desk.state.outbox[&1].instruction else {
        panic!("a core instruction")
    };
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"return_payment": {"policy": "pol-1", "payment": "i1"}})
    );
    assert_eq!(desk.state.books["anz-sf1-claims"].expected["i1"].amount_minor, 80_000_00);
    assert!(desk.state.payees["p1"].suspect);
    assert_eq!(desk.state.payees["p1"].blocked(desk.now), Some("payee_account_returned"));
    // The claim payment can be made again once the payee has new details.
    desk.prepare("r2", RunType::LumpSumClaims, d(2027, 11, 18));
    let mut again = claim("i2", "p1", "pol-1", 80_000_00);
    again.source = "source-i1".to_string();
    desk.add("r2", vec![again]);
    let events = desk.ok(&mia(), Command::CheckRun { run: run("r2") });
    assert_eq!(events, vec![Event::RunChecked { run: run("r2"), held: 1 }]);
    desk.ok(&mia(), Command::CloseRun { run: run("r1") });
}

/// Purpose: a payment that comes back before it settled was never in the
/// ledger, so the core is told nothing and the settlement that follows
/// counts only what stayed.
/// Why it matters: reversing in the ledger something that was never posted
/// would be refused by the core and leave a break for someone to chase.
#[test]
fn a_payment_returned_before_settlement_never_reaches_the_ledger() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![claim("i1", "p1", "pol-1", 500_00), claim("i2", "p1", "pol-2", 700_00)]);
    let s = desk.approve("r1");
    desk.deliver(&s);
    desk.ok(&svc(), ack(&s, &[]));
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "i1".to_string(),
            reason: ReturnReason::Other("recalled".to_string()),
            returned_on: d(2027, 11, 15),
        },
    );
    assert!(!desk.state.payees["p1"].suspect);
    desk.ok(
        &svc(),
        Command::RecordSettlement {
            submission: s.clone(),
            settled_on: d(2027, 11, 15),
        },
    );
    let keys: Vec<_> = desk.state.outbox.values().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["returned-i1", "settle-i2"]);
    assert_eq!(desk.state.books["anz-sf1-claims"].expected[&s].amount_minor, -700_00);
    assert!(!desk.state.books["anz-sf1-claims"].expected.contains_key("i1"));
}

/// Purpose: a direct debit collection run draws on active authorities only,
/// and a settled collection becomes a receipt applied to the policy by the
/// cash matching path, with one credit expected on the collection account.
/// Why it matters: this is how most premium arrives. A collection on an
/// authority that is suspended or cancelled is an unauthorised debit.
#[test]
fn a_collection_run_draws_on_active_authorities() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.authority("a2", "pol-2", "0207654321", false);
    desk.ok(
        &pam(),
        Command::SuspendAuthority {
            authority: "a2".to_string(),
            reason: "customer".to_string(),
        },
    );
    desk.prepare("dd", RunType::Collection, d(2027, 11, 15));
    let add = |items| Command::AddRunItems { run: run("dd"), items };
    assert_eq!(desk.code(&mia(), add(vec![collect("c0", "zz", "pol-1", 5)])), "unknown_authority");
    assert_eq!(desk.code(&mia(), add(vec![collect("c0", "a1", "pol-2", 5)])), "invalid_item");
    desk.payee("p1", PayeeKind::Customer, "0301234567");
    assert_eq!(desk.code(&mia(), add(vec![pay("c0", "p1", 5)])), "invalid_item");
    desk.add("dd", vec![collect("c1", "a1", "pol-1", 150_00), collect("c2", "a2", "pol-2", 90_00)]);
    let events = desk.ok(&mia(), Command::CheckRun { run: run("dd") });
    assert_eq!(events, vec![Event::RunChecked { run: run("dd"), held: 1 }]);
    assert_eq!(desk.state.runs["dd"].items["c2"].status, ItemStatus::Held("authority_not_active".to_string()));
    desk.ok(
        &mia(),
        Command::RemoveRunItem {
            run: run("dd"),
            item: "c2".to_string(),
        },
    );
    let s = desk.approve("dd");
    assert_eq!(desk.state.submissions[&s].account, "anz-sf1-collection");
    desk.settle(&s, d(2027, 11, 16));
    let entry = desk.state.outbox.values().next().unwrap();
    let Instruction::Core { caller, command } = &entry.instruction else {
        panic!("a core instruction")
    };
    assert_eq!(caller, "cash-matching");
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"apply_receipt": {"policy": "pol-1", "receipt": "c1", "amount": {"minor": 150_00, "currency": "NZD"}}})
    );
    assert_eq!(desk.state.books["anz-sf1-collection"].expected[&s].amount_minor, 150_00);
}

/// Purpose: a collection dishonoured for insufficient funds reverses its
/// receipt and may be presented once more; a second dishonour in a row
/// suspends the authority and nothing more is presented; a collection that
/// is still settled when its run closes clears the count.
/// Why it matters: presenting again and again to an empty account costs the
/// customer bank fees each time. Once more, then stop and talk to them.
#[test]
fn a_dishonour_is_presented_once_more_then_the_authority_is_suspended() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.prepare("d1", RunType::Collection, d(2027, 11, 15));
    desk.add("d1", vec![collect("c1", "a1", "pol-1", 150_00)]);
    let s = desk.approve("d1");
    desk.settle(&s, d(2027, 11, 15));
    let dishonour = |item: &str| Command::RecordReturn {
        item: item.to_string(),
        reason: ReturnReason::InsufficientFunds,
        returned_on: d(2027, 11, 16),
    };
    desk.ok(&svc(), dishonour("c1"));
    let keys: Vec<_> = desk.state.outbox.values().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["settle-c1", "return-c1"]);
    let Instruction::Core { caller, command } = &desk.state.outbox[&1].instruction else {
        panic!("a core instruction")
    };
    assert_eq!(caller, "cash-matching");
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"reverse_receipt": {"policy": "pol-1", "receipt": "c1"}})
    );
    assert_eq!(desk.state.books["anz-sf1-collection"].expected["c1"].amount_minor, -150_00);
    assert_eq!(answer(&desk.state, json!({"q": "represent"})).unwrap(), json!(["c1"]));
    assert_eq!(desk.state.authorities["a1"].dishonours, 1);

    // Presenting again: same authority, same amount, once.
    desk.prepare("d2", RunType::Collection, d(2027, 11, 18));
    let mut wrong = collect("c2", "a1", "pol-1", 151_00);
    wrong.represents = Some("c1".to_string());
    assert_eq!(
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: run("d2"),
                items: vec![wrong]
            }
        ),
        "cannot_represent"
    );
    let mut again = collect("c2", "a1", "pol-1", 150_00);
    again.represents = Some("c1".to_string());
    let mut twice = collect("c3", "a1", "pol-1", 150_00);
    twice.represents = Some("c1".to_string());
    assert_eq!(
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: run("d2"),
                items: vec![again.clone(), twice.clone()]
            }
        ),
        "duplicate_item"
    );
    desk.add("d2", vec![again]);
    assert!(desk.state.represent.is_empty());
    assert_eq!(
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: run("d2"),
                items: vec![twice]
            }
        ),
        "cannot_represent"
    );
    desk.at(t(2027, 11, 18, 9, 0));
    let s = desk.approve("d2");
    desk.settle(&s, d(2027, 11, 18));
    let events = desk.ok(&svc(), dishonour("c2"));
    assert!(events.contains(&Event::AuthoritySuspended {
        authority: "a1".to_string(),
        reason: "repeated_dishonour".to_string()
    }));
    assert!(desk.state.represent.is_empty(), "a second presentment is never presented again");
    assert_eq!(desk.state.authorities["a1"].status, AuthorityStatus::Suspended);
    // Reinstated after talking to the customer, a clean collection clears the count.
    desk.ok(&pam(), Command::ReinstateAuthority { authority: "a1".to_string() });
    desk.prepare("d3", RunType::Collection, d(2027, 11, 19));
    desk.add("d3", vec![collect("c4", "a1", "pol-1", 150_00)]);
    desk.at(t(2027, 11, 19, 9, 0));
    let s = desk.approve("d3");
    desk.settle(&s, d(2027, 11, 19));
    assert_eq!(desk.state.authorities["a1"].dishonours, 0, "reinstating starts the count again");
    desk.ok(&svc(), dishonour("c4"));
    assert_eq!(desk.state.authorities["a1"].dishonours, 1);
    desk.prepare("d4", RunType::Collection, d(2027, 11, 22));
    desk.add("d4", vec![collect("c5", "a1", "pol-1", 150_00)]);
    desk.at(t(2027, 11, 22, 9, 0));
    let s = desk.approve("d4");
    desk.settle(&s, d(2027, 11, 22));
    desk.ok(&mia(), Command::CloseRun { run: run("d4") });
    assert_eq!(desk.state.authorities["a1"].dishonours, 0);
}

/// Purpose: a collection the bank returns because the account is closed or
/// the customer cancelled the authority at their bank cancels the authority
/// here and is not presented again.
/// Why it matters: the customer has withdrawn permission. Trying again is a
/// breach of the direct debit rules.
#[test]
fn a_closed_account_cancels_the_authority() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.prepare("d1", RunType::Collection, d(2027, 11, 15));
    desk.add("d1", vec![collect("c1", "a1", "pol-1", 150_00)]);
    let s = desk.approve("d1");
    desk.settle(&s, d(2027, 11, 15));
    let events = desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "c1".to_string(),
            reason: ReturnReason::AuthorityCancelled,
            returned_on: d(2027, 11, 16),
        },
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::AuthorityCancelled { reason, .. } if reason == "account_unusable")));
    assert!(desk.state.represent.is_empty());
    // Removing a dropped re-presentment puts the original back in the queue.
    desk.authority("a2", "pol-2", "0207654321", false);
    desk.prepare("d2", RunType::Collection, d(2027, 11, 16));
    desk.add("d2", vec![collect("c5", "a2", "pol-2", 10_00)]);
    desk.at(t(2027, 11, 16, 9, 0));
    let s = desk.approve("d2");
    desk.settle(&s, d(2027, 11, 16));
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "c5".to_string(),
            reason: ReturnReason::InsufficientFunds,
            returned_on: d(2027, 11, 16),
        },
    );
    desk.prepare("d3", RunType::Collection, d(2027, 11, 18));
    let mut again = collect("c6", "a2", "pol-2", 10_00);
    again.represents = Some("c5".to_string());
    desk.add("d3", vec![again]);
    desk.ok(&mia(), Command::CancelRun { run: run("d3") });
    assert!(desk.state.represent.contains("c5"));
    assert_eq!(desk.state.runs["d3"].status, RunStatus::Cancelled);
    assert_eq!(desk.code(&mia(), Command::CancelRun { run: run("d3") }), "wrong_state");
    assert_eq!(desk.code(&ari(), Command::CancelRun { run: run("d2") }), "not_permitted");
}

/// Purpose: redirecting a run type sends new and still-queued submissions
/// through the second bank and its accounts, leaves anything a driver has
/// already taken where it is, and can be switched back.
/// Why it matters: when one bank's channel is down, claims are still paid
/// that day through the other, without anyone editing a file by hand.
#[test]
fn a_route_is_redirected_to_the_second_bank() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    for (id, item, amount) in [("r1", "i1", 100_00), ("r2", "i2", 200_00)] {
        desk.prepare(id, RunType::BenefitPayments, d(2027, 11, 15));
        desk.add(id, vec![pay(item, "p1", amount)]);
        desk.approve(id);
    }
    desk.ok(
        &svc(),
        Command::BeginSubmission {
            submission: "r1-1".to_string(),
            region: "akl".to_string(),
        },
    );
    let redirect = |secondary| Command::RedirectRoute {
        run_type: RunType::BenefitPayments,
        secondary,
    };
    assert_eq!(desk.code(&mia(), redirect(true)), "not_permitted");
    desk.ok(&cal(), redirect(true));
    assert_eq!(desk.state.submissions["r1-1"].channel, "anz-file");
    let moved = &desk.state.submissions["r2-1"];
    assert_eq!((moved.channel.as_str(), moved.account.as_str()), ("bnz-file", "bnz-sf1-claims"));
    // The second bank closes at three.
    desk.prepare("r3", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r3", vec![pay("i3", "p1", 300_00)]);
    desk.ok(&mia(), Command::CheckRun { run: run("r3") });
    desk.at(t(2027, 11, 15, 15, 30));
    assert_eq!(desk.code(&ari(), Command::ApproveRun { run: run("r3") }), "after_cutoff");
    desk.ok(&cal(), redirect(false));
    assert_eq!(desk.state.submissions["r2-1"].channel, "anz-file");
    desk.ok(&ari(), Command::ApproveRun { run: run("r3") });

    let mut one_bank = config();
    one_bank.routes.get_mut(&RunType::Commission).unwrap().secondary = None;
    one_bank.routes.get_mut(&RunType::Commission).unwrap().urgent = None;
    one_bank.routes.remove(&RunType::Reinsurer);
    desk.ok(
        &cal(),
        Command::ActivateConfig {
            version: 2,
            config: Box::new(one_bank),
        },
    );
    assert_eq!(
        desk.code(
            &cal(),
            Command::RedirectRoute {
                run_type: RunType::Commission,
                secondary: true
            }
        ),
        "no_secondary"
    );
    let prepare = |run_type, urgent| Command::PrepareRun {
        run: run("r9"),
        run_type,
        fund: "sf1".to_string(),
        currency: NZD,
        value_date: d(2027, 11, 16),
        urgent,
    };
    assert_eq!(desk.code(&mia(), prepare(RunType::Commission, true)), "no_urgent_channel");
    assert_eq!(desk.code(&mia(), prepare(RunType::Reinsurer, false)), "no_route");
}

/// Purpose: finished runs, old answers and resolved exceptions are forgotten
/// once they are older than the date given, and nothing unfinished is.
/// Why it matters: every replica carries the module's state. It must stay
/// bounded without ever dropping money that is still in motion.
#[test]
fn pruning_forgets_only_what_is_finished() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    for (id, item) in [("old", "i1"), ("open", "i2")] {
        desk.prepare(id, RunType::BenefitPayments, d(2027, 11, 15));
        desk.add(id, vec![pay(item, "p1", 100_00 + item.len() as i64 + id.len() as i64)]);
    }
    let s = desk.approve("old");
    desk.settle(&s, d(2027, 11, 15));
    desk.ok(&mia(), Command::CloseRun { run: run("old") });
    assert_eq!(desk.code(&mia(), Command::Prune { before: d(2027, 12, 1) }), "not_permitted");
    desk.at(t(2027, 12, 1, 9, 0));
    desk.ok(&svc(), Command::Prune { before: d(2027, 12, 1) });
    assert!(!desk.state.runs.contains_key("old"));
    assert!(!desk.state.submissions.contains_key(&s));
    assert!(!desk.state.items.contains_key("i1"));
    assert!(desk.state.runs.contains_key("open"));
    assert!(desk.state.items.contains_key("i2"));
    assert_eq!(desk.state.answered.len(), 1, "only the prune itself is remembered");
}

/// Purpose: a collection under a scheme member's own authority reaches the
/// core as that member's receipt, beside an employer's collection on the
/// same scheme policy that stays an ordinary receipt; a dishonour reverses
/// the member's receipt like any other; and an authority recorded before
/// members could pay still reads as the policy's own.
/// Why it matters: a member's premium can include an underwriting loading.
/// Applied as the policy's money it would pay down the employer's bill and
/// show on the employer's statement, which is both the wrong debt settled
/// and a disclosure of the member's health to their employer.
#[test]
fn a_members_own_collection_is_applied_to_that_member_alone() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.ok(
        &pam(),
        Command::CreateAuthority {
            authority: "a2".to_string(),
            policy: policy("pol-1"),
            party: "party-member".to_string(),
            life: Some(ic_core::LifeId::from("life-7")),
            account: account("Mere Member", "0201234567"),
            sole_name: true,
        },
    );
    desk.prepare("dd", RunType::Collection, d(2027, 11, 15));
    desk.add("dd", vec![collect("c1", "a1", "pol-1", 900_00), collect("c2", "a2", "pol-1", 41_50)]);
    let s = desk.approve("dd");
    desk.settle(&s, d(2027, 11, 16));
    let sent = |desk: &Desk, key: &str| {
        let entry = desk.state.outbox.values().find(|e| e.key == key).expect("an instruction under that key");
        let Instruction::Core { caller, command } = &entry.instruction else {
            panic!("a core instruction")
        };
        assert_eq!(caller, "cash-matching");
        serde_json::to_value(command).unwrap()
    };
    assert_eq!(
        sent(&desk, "settle-c1"),
        json!({"apply_receipt": {"policy": "pol-1", "receipt": "c1", "amount": {"minor": 900_00, "currency": "NZD"}}})
    );
    assert_eq!(
        sent(&desk, "settle-c2"),
        json!({"apply_member_receipt": {"policy": "pol-1", "life": "life-7", "receipt": "c2", "amount": {"minor": 41_50, "currency": "NZD"}}})
    );
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "c2".to_string(),
            reason: ReturnReason::InsufficientFunds,
            returned_on: d(2027, 11, 17),
        },
    );
    assert_eq!(sent(&desk, "return-c2"), json!({"reverse_receipt": {"policy": "pol-1", "receipt": "c2"}}));

    let stored = serde_json::to_value(&desk.state.authorities["a1"]).unwrap();
    assert!(stored.get("life").is_none(), "an authority without a member is stored as it always was");
    let old: ic_payments::model::Authority = serde_json::from_value(stored).unwrap();
    assert_eq!(old.life, None);
}

/// Purpose: a collection dishonoured for want of funds from a payer marked
/// as in financial hardship is not queued to be presented again; a
/// collections officer is asked to call and offer hardship help; only once
/// the conversation is recorded, with what was said, may it be presented,
/// and only if that was agreed; the marker is set by collections or a payee
/// maintainer and nobody else; a payer without the marker is presented
/// again as before.
/// Why it matters: taking money again from someone who has just told us
/// they cannot pay costs them a dishonour fee and the insurer their trust.
/// The design notes put a conversation before any re-presentment for a
/// customer in hardship (COL-016).
#[test]
fn a_payer_in_hardship_is_talked_to_before_anything_is_presented_again() {
    use ic_payments::model::ConversationOutcome;
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.authority("a2", "pol-2", "0201234567", false);
    let mark = |hardship| Command::MarkHardship {
        authority: "a1".to_string(),
        hardship,
    };
    assert_eq!(desk.code(&mia(), mark(true)), "not_permitted");
    assert_eq!(
        desk.code(
            &col(),
            Command::MarkHardship {
                authority: "zz".to_string(),
                hardship: true
            }
        ),
        "unknown_authority"
    );
    desk.ok(&pam(), mark(false));
    let events = desk.ok(&col(), mark(true));
    assert_eq!(
        events,
        vec![Event::HardshipMarked {
            authority: "a1".to_string(),
            hardship: true
        }]
    );

    desk.prepare("d1", RunType::Collection, d(2027, 11, 15));
    desk.add("d1", vec![collect("c1", "a1", "pol-1", 150_00), collect("c2", "a2", "pol-2", 90_00)]);
    let s = desk.approve("d1");
    desk.settle(&s, d(2027, 11, 15));
    for item in ["c1", "c2"] {
        desk.ok(
            &svc(),
            Command::RecordReturn {
                item: item.to_string(),
                reason: ReturnReason::InsufficientFunds,
                returned_on: d(2027, 11, 16),
            },
        );
    }
    assert_eq!(
        answer(&desk.state, json!({"q": "represent"})).unwrap(),
        json!(["c2"]),
        "only the payer not in hardship"
    );
    assert_eq!(answer(&desk.state, json!({"q": "awaiting_conversation"})).unwrap(), json!({"c1": "a1"}));
    let call = desk
        .state
        .outbox
        .values()
        .find(|e| e.key == "hardship-call-c1")
        .expect("collections is asked to call");
    assert!(matches!(&call.instruction, Instruction::Notify { to: ic_payments::Role::Collections, subject, .. } if subject == "hardship_conversation"));
    desk.prepare("d2", RunType::Collection, d(2027, 11, 22));
    let mut again = collect("c1-2", "a1", "pol-1", 150_00);
    again.represents = Some("c1".to_string());
    let early = Command::AddRunItems {
        run: "d2".to_string(),
        items: vec![again.clone()],
    };
    assert_eq!(desk.code(&mia(), early.clone()), "cannot_represent", "not before the conversation");

    let talk = |outcome, note: &str| Command::RecordConversation {
        item: "c1".to_string(),
        outcome,
        note: note.to_string(),
    };
    assert_eq!(desk.code(&mia(), talk(ConversationOutcome::Represent, "agreed")), "not_permitted");
    assert_eq!(desk.code(&col(), talk(ConversationOutcome::Represent, " ")), "no_note");
    desk.ok(
        &col(),
        talk(ConversationOutcome::Represent, "pay day is Thursday; agreed to try again on the 22nd"),
    );
    assert_eq!(desk.code(&col(), talk(ConversationOutcome::Represent, "again")), "no_conversation_due");
    desk.ok(&mia(), early);

    // Agreed not to present: it never is.
    let s2 = desk.approve("d2");
    desk.settle(&s2, d(2027, 11, 22));
    desk.authority("a3", "pol-3", "0301234567", false);
    desk.ok(
        &col(),
        Command::MarkHardship {
            authority: "a3".to_string(),
            hardship: true,
        },
    );
    desk.prepare("d3", RunType::Collection, d(2027, 12, 15));
    desk.add("d3", vec![collect("c3", "a3", "pol-3", 150_00)]);
    let s3 = desk.approve("d3");
    desk.settle(&s3, d(2027, 12, 15));
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "c3".to_string(),
            reason: ReturnReason::InsufficientFunds,
            returned_on: d(2027, 12, 16),
        },
    );
    desk.ok(
        &col(),
        Command::RecordConversation {
            item: "c3".to_string(),
            outcome: ConversationOutcome::DoNotRepresent,
            note: "hardship application started".to_string(),
        },
    );
    assert!(!desk.state.represent.contains("c3"));
}

/// Purpose: a collection the channel could not take before the bank's
/// cutoff, because it was down, goes for the bank's next window when a
/// driver takes it, skipping the weekend; each of its items is recorded as
/// deferred with its policy, its due date and the date it went for, and the
/// population for a range of due dates is a question anyone can ask; makers
/// are told; a submission taken inside its window is not deferred.
/// Why it matters: an outage must not lose a collection or send it for a
/// day the bank has closed, and every customer whose standing or fee moved
/// because we collected late has to be found by query and put right, not
/// discovered by complaint (COL-025).
#[test]
fn a_collection_held_up_by_an_outage_goes_next_window_and_is_found() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.authority("a2", "pol-2", "0201234567", false);
    // Friday, with the file channel's cutoff at 16:00.
    desk.at(t(2027, 11, 19, 9, 0));
    desk.prepare("d1", RunType::Collection, d(2027, 11, 19));
    desk.add("d1", vec![collect("c1", "a1", "pol-1", 150_00), collect("c2", "a2", "pol-2", 90_00)]);
    let s = desk.approve("d1");
    // The channel is down until 20:00.
    desk.at(t(2027, 11, 19, 20, 0));
    desk.deliver(&s);
    let state = &desk.state;
    assert_eq!(state.submissions[&s].deferred_to, Some(d(2027, 11, 22)), "Monday, the next window");
    assert_eq!(state.batch(&s).unwrap().value_date, d(2027, 11, 22));
    let found = answer(state, json!({"q": "deferred", "from": "2027-11-19", "to": "2027-11-19"})).unwrap();
    assert_eq!(found["c1"]["policy"], "pol-1");
    assert_eq!(
        (found["c1"]["due"].clone(), found["c1"]["sent_for"].clone()),
        (json!("2027-11-19"), json!("2027-11-22"))
    );
    assert_eq!(found["c2"]["amount_minor"], 90_00);
    assert_eq!(
        answer(state, json!({"q": "deferred", "from": "2027-11-20", "to": "2027-12-31"})).unwrap(),
        json!({})
    );
    assert!(answer(state, json!({"q": "deferred", "from": "soon", "to": "2027-12-31"}))
        .unwrap_err()
        .contains("from is a date"));
    let told = state
        .outbox
        .values()
        .find(|e| e.key.starts_with(&format!("deferred-{s}")))
        .expect("makers are told");
    assert!(matches!(&told.instruction, Instruction::Notify { subject, detail, .. } if subject == "submission_deferred" && detail.contains("2 items, 24000")));

    // Taken on Saturday morning it still goes for Monday; inside its window
    // a run is sent for its own day.
    desk.at(t(2027, 11, 20, 8, 0));
    desk.prepare("d2", RunType::Collection, d(2027, 11, 22));
    desk.add("d2", vec![collect("c3", "a1", "pol-1", 10_00)]);
    let early = desk.approve("d2");
    desk.deliver(&early);
    assert_eq!(desk.state.submissions[&early].deferred_to, None);
    desk.at(t(2027, 11, 22, 8, 0));
    desk.prepare("d3", RunType::Collection, d(2027, 11, 22));
    desk.add("d3", vec![collect("c4", "a2", "pol-2", 10_00)]);
    let late = desk.approve("d3");
    desk.at(t(2027, 11, 23, 9, 0));
    desk.deliver(&late);
    assert_eq!(
        desk.state.submissions[&late].deferred_to,
        Some(d(2027, 11, 23)),
        "before Tuesday's cutoff it goes Tuesday"
    );
}

/// Purpose: a run takes items and is checked only before it is approved,
/// and an item naming a payee or authority the module does not know is
/// refused when it is added.
/// Why it matters: an approved run is what the approver saw; adding to it
/// or re-checking it afterwards would send a file nobody approved, and an
/// item naming an account that is not on record must never reach a bank.
#[test]
fn an_approved_run_is_closed_to_change_and_unknown_accounts_are_refused() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00)]);
    desk.approve("r1");
    assert_eq!(
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: run("r1"),
                items: vec![pay("i2", "p1", 1_00)]
            }
        ),
        "wrong_state"
    );
    assert_eq!(desk.code(&mia(), Command::CheckRun { run: run("r1") }), "wrong_state");

    desk.prepare("r2", RunType::LumpSumClaims, d(2027, 11, 15));
    assert_eq!(
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: run("r2"),
                items: vec![pay("i3", "ghost", 100_00)]
            }
        ),
        "unknown_payee"
    );
    desk.prepare("r3", RunType::Collection, d(2027, 11, 15));
    let items = vec![collect("i4", "no-such-authority", "pol-1", 100_00)];
    assert_eq!(desk.code(&mia(), Command::AddRunItems { run: run("r3"), items }), "unknown_authority");
}
