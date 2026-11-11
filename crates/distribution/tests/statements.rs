//! Statements and payment: what a payee is shown, what is swept and paid,
//! and what happens when a payment may not or does not go through.

mod common;

use common::*;
use ic_core::counterparty::{InternalLedger, PartyLedger};
use ic_core::PolicyStatus;
use ic_distribution::config::{AdjustmentKind, Change};
use ic_distribution::model::{InstructionKind, PaymentInstruction};
use ic_distribution::rules::Component;
use ic_distribution::state::{CaseKind, Settings};
use ic_distribution::{BookId, Command, Event};
use ic_money::Currency::NZD;

fn instruction(id: &str, kind: InstructionKind, amount_minor: Option<i64>, share_ppm: Option<u32>) -> PaymentInstruction {
    PaymentInstruction {
        id: id.into(),
        kind,
        to_account_ref: format!("acct-{id}"),
        amount_minor,
        share_ppm,
        from: d(2020, 1, 1),
        until: None,
        document_ref: format!("doc-{id}"),
    }
}

fn adjustment(reference: &str, payee: &str, amount_minor: i64, kind: AdjustmentKind) -> Change {
    Change::Adjustment {
        reference: reference.into(),
        payee: party(payee),
        partition: 0,
        currency: NZD,
        amount_minor,
        kind,
    }
}

fn book_level() -> AdjustmentKind {
    AdjustmentKind::BookLevel {
        product: "term-life".into(),
        cover_type: "life".into(),
    }
}

/// Purpose: a statement lists what the ledger confirmed since the last cut with the layer,
/// book and component behind each amount, opens where the last one closed, and its one net
/// payment sweeps the payable to clearing and clears when the bank settles.
/// Why it matters: the statement is what a payee checks their income against, and the clearing
/// account equalling the statement is the reconciliation that proves they were paid what it says.
#[test]
fn a_statement_shows_the_detail_and_its_payment_clears_the_ledger() {
    let (mut w, policy, renewal) = advised_in(worked_example());
    w.days(6);
    let owed = w.payable("a-co");
    let cash = w.internal(InternalLedger::Settlement);
    let events = w.cut("a-co").unwrap();
    let s = w.statement("a-co", 1).clone();
    assert_eq!((s.opening_minor, s.closing_minor), (0, owed));
    assert_eq!(s.lines.len(), 2, "initial and renewal");
    assert!(s.lines.iter().all(|l| l.book == Some(BookId::from("a-at-fap-one"))
        && l.layer.as_deref() == Some(&format!("{policy}/life/1")[..])
        && l.component == Some(Component::Base)
        && l.policy.as_ref() == Some(&policy)));
    assert_eq!(s.lines.iter().map(|l| l.amount_minor).sum::<i64>(), owed);
    assert_eq!(s.payments.len(), 1);
    assert_eq!(
        (s.payments[0].reference.as_str(), s.payments[0].account_ref.as_str(), s.payments[0].amount_minor),
        ("pay:a-co.1:1", "acct-a-co", owed)
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::PaymentRequested { amount_minor, .. } if *amount_minor == owed)));
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::StatementIssued { hash, .. } if *hash == s.hash && !hash.is_empty())));
    assert_eq!(w.payable("a-co"), 0);
    assert_eq!(w.ledger("a-co", PartyLedger::Clearing), owed);
    assert_eq!(code(w.cut("a-co")), "payment_in_progress");

    w.settle("pay:a-co.1:1");
    assert_eq!(w.ledger("a-co", PartyLedger::Clearing), 0);
    assert_eq!(w.internal(InternalLedger::Settlement), cash - owed);
    assert_eq!(
        code(w.send(
            module("payments"),
            Command::PaymentSettled {
                reference: "pay:a-co.1:1".into()
            }
        )),
        "unknown_payment"
    );

    // The next statement opens at zero, shows the sweep and the new premium, and pays the new amount.
    w.days(31);
    w.pay(&policy, "r2");
    w.cut("a-co").unwrap();
    let next = w.statement("a-co", 2);
    assert_eq!(next.closing_minor, renewal - ic_distribution::rules::portion(renewal, pct(25.0)));
    assert_eq!(
        next.opening_minor
            + next
                .lines
                .iter()
                .filter(|l| l.ledger == PartyLedger::CommissionPayable)
                .map(|l| l.amount_minor)
                .sum::<i64>(),
        next.closing_minor
    );
    assert_eq!(w.statement("a-co", 1), &s, "an issued statement never changes");
    assert_eq!(code(w.cut("ghost")), "unknown_payee");
    assert_eq!(code(w.send(admin(), Command::CutStatement { payee: party("a-co") })), "not_authorised");
    w.settle("pay:a-co.2:1");
    w.assert_reconciled();
}

/// Purpose: nothing is paid to an account inside its cooling-off or to a payee with a payment
/// hold; the statement still issues and says why, and the money is paid on a later statement.
/// Why it matters: a changed bank account is the classic redirection fraud. The cooling-off
/// only works if the statement run itself refuses, with no one having to remember.
#[test]
fn a_new_account_and_a_payment_hold_stop_payment_but_not_the_statement() {
    let (mut w, _, _) = advised_in(worked_example());
    let owed = w.payable("a-co");
    w.cut("a-co").unwrap();
    let first = w.statement("a-co", 1);
    assert_eq!(first.payment_held.as_deref(), Some("no verified account past its cooling-off"));
    assert!(first.payments.is_empty());
    assert_eq!(w.payable("a-co"), owed);

    w.days(6);
    let hold = |reason: Option<&str>| Command::SetPaymentHold {
        party: party("a-co"),
        reason: reason.map(str::to_string),
    };
    assert_eq!(code(w.send(admin(), hold(Some(" ")))), "invalid_hold");
    w.send(admin(), hold(Some("bank details under review"))).unwrap();
    w.cut("a-co").unwrap();
    assert_eq!(w.statement("a-co", 2).payment_held.as_deref(), Some("bank details under review"));
    assert_eq!(
        w.dist.position(&party("a-co"), w.today()).payment_hold.as_deref(),
        Some("bank details under review")
    );
    w.send(admin(), hold(None)).unwrap();
    w.cut("a-co").unwrap();
    let third = w.statement("a-co", 3);
    assert_eq!((third.opening_minor, third.closing_minor, third.payments[0].amount_minor), (owed, owed, owed));
    assert!(third.lines.is_empty(), "nothing new happened; the balance carried");
    w.settle("pay:a-co.3:1");
    w.assert_reconciled();
}

/// Purpose: payment instructions take their part of a statement first: a court order entered
/// by legal before a lender's assignment, then the payee. A statutory instruction cannot be
/// removed by hand, an assignment only with the lender's consent, and a part the bank returns
/// goes back to the payee's payable account.
/// Why it matters: an insurer that pays an adviser past a court order or a registered
/// assignment pays twice. The order of priority and who may enter or remove each is the law.
#[test]
fn court_orders_and_assignments_are_paid_first_in_order_of_priority() {
    let (mut w, _, _) = advised_in(worked_example());
    w.days(6);
    let add = |i: PaymentInstruction| Command::AddPaymentInstruction {
        party: party("a-co"),
        instruction: i,
    };
    let lender = instruction("lender", InstructionKind::Assignment, None, Some(pct(50.0)));
    let order = instruction("court", InstructionKind::Statutory, Some(100_00), None);
    w.send(admin(), add(lender.clone())).unwrap();
    assert_eq!(code(w.send(admin(), add(lender.clone()))), "duplicate_instruction");
    assert_eq!(code(w.send(admin(), add(order.clone()))), "not_authorised");
    w.send(legal(), add(order)).unwrap();
    for bad in [
        instruction("x", InstructionKind::Assignment, None, None),
        instruction("x", InstructionKind::Assignment, Some(1), Some(1)),
        instruction("x", InstructionKind::Assignment, Some(0), None),
        instruction("x", InstructionKind::Assignment, None, Some(1_000_001)),
        PaymentInstruction {
            document_ref: " ".into(),
            ..instruction("x", InstructionKind::Assignment, Some(1), None)
        },
    ] {
        assert_eq!(code(w.send(admin(), add(bad))), "invalid_instruction");
    }
    assert_eq!(
        code(w.send(
            admin(),
            Command::AddPaymentInstruction {
                party: party("ghost"),
                instruction: lender
            }
        )),
        "unknown_payee"
    );
    assert_eq!(w.dist.position(&party("a-co"), w.today()).instructions[0].id, "court", "legal priority first");

    let owed = w.payable("a-co");
    w.cut("a-co").unwrap();
    let parts: Vec<(String, i64)> = w
        .statement("a-co", 1)
        .payments
        .iter()
        .map(|p| (p.account_ref.clone(), p.amount_minor))
        .collect();
    let to_lender = ic_distribution::rules::portion(owed, pct(50.0));
    assert_eq!(
        parts,
        vec![
            ("acct-court".to_string(), 100_00),
            ("acct-lender".to_string(), to_lender),
            ("acct-a-co".to_string(), owed - 100_00 - to_lender)
        ]
    );
    let cash = w.internal(InternalLedger::Settlement);
    w.settle("pay:a-co.1:1");
    assert_eq!(w.ledger("a-co", PartyLedger::Clearing), owed, "clearing waits for every part");
    assert_eq!(
        code(w.send(
            module("payments"),
            Command::PaymentSettled {
                reference: "pay:a-co.1:1".into()
            }
        )),
        "payment_settled"
    );
    w.settle("pay:a-co.1:2");
    w.send(
        module("payments"),
        Command::PaymentReturned {
            reference: "pay:a-co.1:3".into(),
            reason: "closed".into(),
        },
    )
    .unwrap();
    w.sync();
    assert_eq!(w.ledger("a-co", PartyLedger::Clearing), 0);
    assert_eq!(w.payable("a-co"), owed - 100_00 - to_lender, "the returned part is owed again");
    assert_eq!(w.internal(InternalLedger::Settlement), cash - 100_00 - to_lender);

    let remove = |id: &str, consent: &str| Command::RemovePaymentInstruction {
        party: party("a-co"),
        id: id.into(),
        consent_ref: consent.into(),
    };
    assert_eq!(code(w.send(admin(), remove("court", "consent"))), "restricted_instruction");
    assert_eq!(code(w.send(admin(), remove("lender", " "))), "consent_required");
    assert_eq!(code(w.send(admin(), remove("ghost", "consent"))), "unknown_instruction");
    w.send(admin(), remove("lender", "lender-consent-letter")).unwrap();
    assert_eq!(w.dist.payees[&party("a-co")].instructions.len(), 1);
    w.assert_reconciled();
}

/// Purpose: an advance is recovered from statements at no more than the agreed share, a
/// retention is kept back and shown until released, and a book-level item pays or takes back
/// through the same approved change.
/// Why it matters: advances, retentions and negotiated one-offs are where commission systems
/// grow spreadsheets. Each being a posted, approved change keeps them on the statement and
/// inside the reconciliation.
#[test]
fn advances_retentions_and_book_level_items_flow_through_the_statement() {
    let (mut w, _, _) = advised_in(worked_example());
    w.days(6);
    let earned = w.payable("a-co");
    let expense = w.internal(InternalLedger::CommissionExpense {
        product: "term-life".into(),
        cover_type: "life".into(),
    });
    w.activate(
        "items",
        vec![
            adjustment("adv-1", "a-co", 1_000_00, AdjustmentKind::Advance),
            adjustment("ret-1", "a-co", 200_00, AdjustmentKind::RetentionHold),
            adjustment("persistency-q1", "a-co", 50_00, book_level()),
            adjustment("quality-q1", "a-co", -20_00, book_level()),
        ],
    )
    .unwrap();
    assert_eq!(w.ledger("a-co", PartyLedger::Advance), -1_000_00, "the advance is owed back");
    assert_eq!(w.ledger("a-co", PartyLedger::Retention), 200_00);
    let closing = earned + 1_000_00 - 200_00 + 50_00 - 20_00;
    assert_eq!(w.payable("a-co"), closing);
    assert_eq!(
        w.internal(InternalLedger::CommissionExpense {
            product: "term-life".into(),
            cover_type: "life".into(),
        }),
        expense + 30_00
    );
    w.cut("a-co").unwrap();
    let s = w.statement("a-co", 1).clone();
    let recovered = ic_distribution::rules::portion(closing, pct(20.0));
    assert_eq!(
        (s.advance_outstanding_minor, s.advance_recovered_minor, s.retention_minor),
        (1_000_00, recovered, 200_00)
    );
    assert_eq!(s.payments[0].amount_minor, closing - recovered);
    assert_eq!(w.ledger("a-co", PartyLedger::Advance), recovered - 1_000_00);
    w.settle("pay:a-co.1:1");
    assert_eq!(w.ledger("a-co", PartyLedger::Clearing), 0);

    w.activate("release", vec![adjustment("ret-1-release", "a-co", 200_00, AdjustmentKind::RetentionRelease)])
        .unwrap();
    assert_eq!((w.ledger("a-co", PartyLedger::Retention), w.payable("a-co")), (0, 200_00));

    // What a change set may not carry.
    assert_eq!(
        code(w.propose("bad", vec![adjustment("adv-1", "a-co", 1, AdjustmentKind::Advance)])),
        "duplicate_reference"
    );
    let twice = vec![adjustment("n", "a-co", 1, book_level()), adjustment("n", "a-co", 1, book_level())];
    assert_eq!(code(w.propose("bad", twice)), "duplicate_reference");
    assert_eq!(code(w.propose("bad", vec![adjustment("n", "a-co", 0, book_level())])), "invalid_amount");
    assert_eq!(
        code(w.propose("bad", vec![adjustment("n", "a-co", -1, AdjustmentKind::Advance)])),
        "invalid_amount"
    );
    assert_eq!(code(w.propose("bad", vec![adjustment("n", "adviser-a", 1, book_level())])), "unknown_payee");
    // Above the approver's authority: the approval is refused with the limit.
    w.propose("large", vec![adjustment("bonus", "a-co", 60_000_00, book_level())]).unwrap();
    let denied = w
        .send(
            approver(),
            Command::ApproveChange {
                id: "large".into(),
                compliance: false,
            },
        )
        .unwrap_err();
    assert!(denied.message.contains("above your limit of 5000000"), "{}", denied.message);
    w.assert_reconciled();
}

/// Purpose: a payee whose balance is a debt is not paid, and when the debt has stood across
/// statements for longer than the threshold a debt case opens once, for a repayment arrangement.
/// Why it matters: clawback debts are recovered from future commission first, but one that just
/// sits there must become someone's job before it becomes uncollectable.
#[test]
fn a_debt_that_outlasts_the_threshold_becomes_a_case_once() {
    let (mut w, policy, _) = advised_in(worked_example());
    w.until_status(&policy, PolicyStatus::Lapsed);
    assert!(w.payable("fap-one") < 0);
    w.cut("fap-one").unwrap();
    let first = w.statement("fap-one", 1);
    assert!(first.closing_minor < 0 && first.payments.is_empty() && first.payment_held.is_none());
    assert!(w.dist.cases.is_empty());
    w.days(30);
    w.cut("fap-one").unwrap();
    assert!(w.dist.cases.is_empty(), "thirty days is inside the threshold");
    w.days(61);
    let events = w.cut("fap-one").unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::CaseOpened { kind: CaseKind::Debt, .. })));
    let case = &w.dist.cases["debt-fap-one"];
    assert_eq!(case.workstreams, vec!["repayment_arrangement".to_string()]);
    w.days(1);
    assert!(!w.cut("fap-one").unwrap().iter().any(|e| matches!(e, Event::CaseOpened { .. })));
    assert_eq!(w.dist.cases.len(), 1);
}

/// Purpose: a statement is not cut while postings for the payee are still on their way to the
/// ledger.
/// Why it matters: the statement and the sweep must cover exactly the same entries. Cutting
/// across an in-flight posting would sweep an amount the statement does not explain.
#[test]
fn a_statement_waits_for_postings_in_flight() {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.auto_deliver = false;
    w.pay(&policy, "r1");
    assert_eq!(w.dist.unconfirmed().len(), 1);
    assert_eq!(code(w.cut("a-co")), "postings_in_flight");
    assert!(w.cut("fap-two").is_ok(), "another payee is not held up");
    w.auto_deliver = true;
    w.sync();
    w.cut("a-co").unwrap();
    assert!(w.statement("a-co", 1).closing_minor > 0);
}

/// Purpose: a payee with balances on several partitions, one of them a debt, gets one net
/// payment; each partition is swept in its own direction and every clearing account returns
/// to zero when the payment settles.
/// Why it matters: payees are paid net across the whole book. A sweep that ignored the debit
/// partition would overpay by exactly the clawback.
#[test]
fn one_net_payment_covers_credits_and_debts_on_different_partitions() {
    let mut w = worked_example_in(World::with(4, Settings::default()));
    let (small, p1) = w.issue(1, 500_000_00, None);
    let mut n = 2;
    let (large, p2) = loop {
        let (policy, p) = w.issue(n, 500_000_00, None);
        if p != p1 {
            break (policy, p);
        }
        n += 1;
    };
    w.write(&small, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.write(&large, "life", 1, "eng-a-one", "a-at-fap-one", 12_000_00).unwrap();
    w.pay(&small, "r1");
    w.pay(&large, "r1");
    for _ in 0..200 {
        if w.status(&small) == PolicyStatus::Lapsed {
            break;
        }
        w.days(1);
        if w.owing(&large) > 0 {
            let receipt = format!("r-{}", w.today());
            w.pay(&large, &receipt);
        }
    }
    let by_partition = w.dist.by_partition(&party("fap-one"), PartyLedger::CommissionPayable);
    assert_eq!(by_partition.len(), 2);
    assert!(by_partition.iter().any(|(p, v)| *p == p1 && *v < 0) && by_partition.iter().any(|(p, v)| *p == p2 && *v > 0));
    let net = w.payable("fap-one");
    assert!(net > 0);
    let cash = w.internal(InternalLedger::Settlement);
    w.cut("fap-one").unwrap();
    assert_eq!(w.statement("fap-one", 1).payments[0].amount_minor, net);
    assert_eq!(w.payable("fap-one"), 0);
    assert_eq!(w.dist.by_partition(&party("fap-one"), PartyLedger::CommissionPayable), vec![]);
    assert_eq!(w.ledger("fap-one", PartyLedger::Clearing), net);
    w.settle("pay:fap-one.1:1");
    assert_eq!(w.dist.by_partition(&party("fap-one"), PartyLedger::Clearing), vec![]);
    assert_eq!(w.internal(InternalLedger::Settlement), cash - net);
    w.assert_reconciled();
}
