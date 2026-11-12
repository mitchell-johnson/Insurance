//! Reversal of admission (design addendum, entry 5) and what follows it:
//! the overpayment held as a fact, and recovery as a conduct decision.

use ic_claims::command::ReinsuranceNotice;
use ic_claims::model::{Finding, OverpaymentCause, RecoveryStatus};
use ic_claims::{Command, CoverFacts, DecisionKind, Instruction, Queue, Request, RunKind, Status, TaskKind};

use crate::world::*;

const REVERSE: Request = Request::ReverseAdmission { false_declaration: false };

/// Two monthly benefits paid on `ip-1`, then the claim taken to the senior
/// queue and given to sam, on 10 April 2027.
fn paid_two_months(w: &mut World) {
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    w.on(d(2027, 4, 10));
}

fn to_senior(w: &mut World, id: &str) {
    w.ok(Command::Escalate {
        claim: id.into(),
        actor: actor("sam"),
    });
    w.assign(id, "sam");
}

fn status(overpayment: &str, status: RecoveryStatus) -> Request {
    Request::RecoveryStatus {
        overpayment: overpayment.into(),
        status,
        offset_pct: 0,
        instalment_minor: 0,
    }
}

/// Purpose: reversing an admission is its own decision type, heavier than a decline: senior
/// queue, a case conference and a conversation with the claimant held since the admission, a
/// proposer who holds the power, and always a second senior signature.
/// Why it matters: the addendum's case is a customer paid for months and then told the insurer
/// was wrong. Every gate here exists so that this is never done by one person, never without
/// the customer being spoken to first, and never while a payment run is mid-flight.
#[test]
fn a_reversal_is_heavier_than_a_decline() {
    let mut w = World::new();
    w.open_claim("c-open");
    assert_eq!(w.undecided("c-open", "ana", REVERSE, "R-REV"), "not_admitted");

    paid_two_months(&mut w);
    assert_eq!(w.undecided("ip-1", "ana", REVERSE, "R-REV"), "senior_queue_required");
    to_senior(&mut w, "ip-1");
    assert_eq!(w.claim("ip-1").queue, Queue::ComplexLarge);
    assert_eq!(w.undecided("ip-1", "sam", REVERSE, "R-REV"), "case_conference_required");
    w.conference("ip-1", "sam");
    assert_eq!(w.undecided("ip-1", "sam", REVERSE, "R-REV"), "conversation_required");
    w.conversation("ip-1", "talk-1");
    assert_eq!(w.undecided("ip-1", "sam", REVERSE, "R-NO"), "unknown_reason");

    // Not while the claim is waiting in a payment run.
    w.ok(Command::PrepareRun {
        run: "run-3".into(),
        kind: RunKind::Benefit,
        benefit_date: d(2027, 5, 1),
        actor: actor("ana"),
    });
    assert_eq!(w.undecided("ip-1", "sam", REVERSE, "R-REV"), "in_run");
    w.ok(Command::CancelRun {
        run: "run-3".into(),
        actor: actor("ana"),
    });

    w.decided("ip-1", "sam", REVERSE, "R-REV");
    assert_eq!(w.claim("ip-1").status, Status::InPayment);
    assert_eq!(w.unapproved("ip-1", "ben"), "authority");
    assert_eq!(w.unapproved("ip-1", "sam"), "second_signer");
    w.approved("ip-1", "sue");
    assert_eq!(w.claim("ip-1").status, Status::Reversed);
}

/// Purpose: an assessor without the reversal power cannot propose a reversal even when every
/// other gate is met.
/// Why it matters: the power to overturn the insurer's own promise is held by named seniors. It
/// must not be borrowed from the second signer the way an amount limit can be.
#[test]
fn only_a_senior_with_the_power_proposes_a_reversal() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    w.decided("c-1", "ana", World::admit_lump(Some(100_000)), "R-OK");
    // A lead escalates; the claim stays with nobody until a senior takes it.
    w.ok(Command::Escalate {
        claim: "c-1".into(),
        actor: actor("sam"),
    });
    assert_eq!(w.claim("c-1").owner, None);
    let mut book = authority();
    book.version = 2;
    book.profiles.get_mut("assessor").expect("in the book").senior = true;
    book.staff.get_mut("ana").expect("in the book").queues.insert(Queue::ComplexLarge);
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    w.assign("c-1", "ana");
    w.conference("c-1", "ana");
    w.conversation("c-1", "talk-1");
    assert_eq!(w.undecided("c-1", "ana", REVERSE, "R-REV"), "authority");
}

/// Purpose: when the reversal takes effect the benefit stops from the decision date, everything
/// paid is recorded as an overpayment that is the insurer's own error, held, with nothing asked
/// of the customer; the cause of the wrong admission is read from the record and sent for
/// quality review; and the facts other modules need are published.
/// Why it matters: this is addendum entry 5 end to end. The customer must not be sent a demand
/// by default, the books must show the receivable, reinsurance must hear of the reversal to
/// raise its disputed items, and the insurer must learn why its rules let the claim through.
#[test]
fn a_reversal_stops_the_benefit_and_holds_the_overpayment() {
    let mut w = World::new();
    paid_two_months(&mut w);
    to_senior(&mut w, "ip-1");
    w.conference("ip-1", "sam");
    w.conversation("ip-1", "talk-1");
    w.decided("ip-1", "sam", REVERSE, "R-REV");
    w.approved("ip-1", "sue");

    let c = w.claim("ip-1");
    assert_eq!((c.status, c.reserve_minor, c.closed_on), (Status::Reversed, 0, Some(d(2027, 4, 10))));
    assert_eq!(c.schedule.as_ref().expect("the schedule is kept").end, d(2027, 4, 10));
    let o = &c.overpayments[0];
    assert_eq!((o.id.as_str(), o.amount_minor, o.status), ("ip-1/overpayment/1", 6_000, RecoveryStatus::Held));
    assert_eq!((o.cause, o.good_faith, o.protected()), (OverpaymentCause::InsurerError, true, true));
    let decision = c.decisions.last().expect("recorded");
    assert_eq!(
        (decision.kind, decision.finding),
        (DecisionKind::ReverseAdmission, Some(Finding::RulesUnmapped))
    );
    assert_eq!(decision.signers, vec!["sam".to_string(), "sue".to_string()]);
    assert!(c.tasks.iter().any(|t| t.id == "finding/rules_unmapped" && t.kind == TaskKind::QualityReview));

    let Some(Instruction::Close { outcome, overpayment, .. }) = w.core.instructions.last() else {
        panic!("the reversal did not close the claim in the ledger");
    };
    assert_eq!(outcome, "admission_reversed");
    assert_eq!(overpayment, &Some(("ip-1/overpayment/1".to_string(), 6_000)));
    let reversed = w.core.noticed("admission_reversed");
    assert_eq!(reversed[0]["decided_on"], "2027-04-10");
    assert_eq!(
        (reversed[0]["reason_code"].as_str(), reversed[0]["cause"].as_str()),
        ("R-REV", "rules_unmapped")
    );
    // The event carries the whole position, so the held balance workbench needs no other source.
    let raised = &w.core.noticed("overpayment_raised")[0];
    assert_eq!(raised["amount_minor"], "6000");
    assert_eq!(
        (
            raised["outstanding_minor"].as_str(),
            raised["status"].as_str(),
            raised["insurer_error"].as_str()
        ),
        ("6000", "held", "true")
    );
    assert!(raised.contains_key("party") && raised.contains_key("overpayment") && raised.contains_key("good_faith"));
    assert_eq!(w.core.noticed("claim_closed").last().expect("published")["outcome"], "admission_reversed");

    // Nothing is asked of the customer, and no further benefit is prepared.
    assert_eq!(w.state.claimant_view("ip-1").expect("the claim exists").repayment_asked_minor, 0);
    assert!(w.core.noticed("recovery_status_changed").is_empty());
    w.on(d(2027, 5, 1));
    let next = Command::PrepareRun {
        run: "run-3".into(),
        kind: RunKind::Benefit,
        benefit_date: d(2027, 5, 1),
        actor: actor("ana"),
    };
    assert_eq!(w.refused(next), "nothing_to_pay");

    // Reported apart from declines, and the held amount is visible.
    let report = w.state.report();
    assert_eq!((report.admissions_reversed, report.decisions.get(&DecisionKind::Decline)), (1, None));
    assert_eq!((report.overpaid_minor, report.held_minor), (6_000, 6_000));

    // A reversed claim cannot be reversed again.
    assert_eq!(w.undecided("ip-1", "sam", REVERSE, "R-REV"), "not_admitted");
}

/// Purpose: recovery of an overpayment that was the insurer's own error from a customer in good
/// faith changes status only with the conduct officer and legal both signing; holding it needs
/// neither; every status change is on the overpayment's history.
/// Why it matters: this is the addendum's central conduct rule. A senior claims manager alone
/// must not be able to start pursuing a customer for the insurer's mistake.
#[test]
fn recovering_the_insurers_own_error_needs_conduct_and_legal() {
    let mut w = World::new();
    paid_two_months(&mut w);
    to_senior(&mut w, "ip-1");
    w.conference("ip-1", "sam");
    w.conversation("ip-1", "talk-1");
    w.decided("ip-1", "sam", REVERSE, "R-REV");
    w.approved("ip-1", "sue");
    let op = "ip-1/overpayment/1";

    assert_eq!(w.undecided("ip-1", "ana", status(op, RecoveryStatus::Pursued), "R-OK"), "authority");
    assert_eq!(
        w.undecided("ip-1", "sam", status("nope", RecoveryStatus::Pursued), "R-OK"),
        "unknown_overpayment"
    );
    assert_eq!(
        w.undecided("ip-1", "sam", status(op, RecoveryStatus::WrittenOff), "R-OK"),
        "invalid_recovery_status"
    );
    assert_eq!(
        w.undecided("ip-1", "sam", status(op, RecoveryStatus::Offset), "R-OK"),
        "invalid_recovery_status"
    );
    assert_eq!(
        w.undecided("ip-1", "sam", status(op, RecoveryStatus::Arrangement), "R-OK"),
        "invalid_recovery_status"
    );
    let too_much = Request::RecoveryStatus {
        overpayment: op.into(),
        status: RecoveryStatus::Offset,
        offset_pct: 26,
        instalment_minor: 0,
    };
    assert_eq!(w.undecided("ip-1", "sam", too_much, "R-OK"), "invalid_recovery_status");

    // Confirming the hold is an ordinary decision.
    w.decided("ip-1", "sam", status(op, RecoveryStatus::Held), "R-OK");
    assert!(w.claim("ip-1").pending.is_none());

    w.decided("ip-1", "sam", status(op, RecoveryStatus::Pursued), "R-OK");
    assert_eq!(w.claim("ip-1").overpayments[0].status, RecoveryStatus::Held);
    assert_eq!(w.unapproved("ip-1", "sue"), "authority");
    w.approved("ip-1", "cody");
    assert_eq!(w.claim("ip-1").overpayments[0].status, RecoveryStatus::Held);
    assert_eq!(w.unapproved("ip-1", "cody"), "second_signer");
    w.approved("ip-1", "lex");
    let o = &w.claim("ip-1").overpayments[0];
    assert_eq!(o.status, RecoveryStatus::Pursued);
    assert_eq!(
        o.history.iter().map(|(_, s, _)| *s).collect::<Vec<_>>(),
        vec![RecoveryStatus::Held, RecoveryStatus::Held, RecoveryStatus::Pursued]
    );
    let changed = w.core.noticed("recovery_status_changed");
    let changed = changed.last().expect("published");
    assert_eq!((changed["status"].as_str(), changed["outstanding_minor"].as_str()), ("pursued", "6000"));
    assert_eq!(w.state.claimant_view("ip-1").expect("the claim exists").repayment_asked_minor, 6_000);

    // An arrangement is a case of its own on the recoveries queue.
    let arrangement = Request::RecoveryStatus {
        overpayment: op.into(),
        status: RecoveryStatus::Arrangement,
        offset_pct: 0,
        instalment_minor: 250,
    };
    w.decided("ip-1", "sam", arrangement, "R-OK");
    w.approved("ip-1", "lex");
    w.approved("ip-1", "cody");
    let c = w.claim("ip-1");
    assert_eq!(
        (c.overpayments[0].status, c.overpayments[0].instalment_minor),
        (RecoveryStatus::Arrangement, 250)
    );
    let task = c.task(TaskKind::RepaymentArrangement).next().expect("opened").id.clone();
    assert_eq!(
        w.complete("ip-1", &task, "agreed", "ana").expect_err("not on recoveries").code,
        "not_authorised"
    );
    w.complete("ip-1", &task, "agreed", "rec").expect("the recoveries queue works it");
}

/// Purpose: money repaid reduces the overpayment through the ledger and marks it recovered when
/// nothing is left; the rest can be written off within authority, with a second signature at
/// the threshold, and the write-off says whether it was the insurer's error.
/// Why it matters: the receivable in the ledger and the overpayment on the claim must move
/// together, and reinsurance needs the insurer-error flag to decide whether a reinsurer shares
/// the loss under the treaty.
#[test]
fn repayment_and_write_off_move_the_ledger_and_the_record_together() {
    let mut w = World::new();
    paid_two_months(&mut w);
    to_senior(&mut w, "ip-1");
    w.conference("ip-1", "sam");
    w.conversation("ip-1", "talk-1");
    w.decided("ip-1", "sam", REVERSE, "R-REV");
    w.approved("ip-1", "sue");
    let op = "ip-1/overpayment/1";
    let repay = |amount: i64| Command::RecordRepayment {
        claim: "ip-1".into(),
        overpayment: op.into(),
        amount_minor: amount,
    };
    assert_eq!(w.refused(repay(0)), "invalid_repayment");
    assert_eq!(w.refused(repay(6_001)), "invalid_repayment");
    w.core.refuse_next = 1;
    assert_eq!(w.refused(repay(1_000)), "ledger");
    w.ok(repay(1_000));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::RecoverOverpayment { amount_minor: 1_000, .. })
    ));
    assert_eq!(w.claim("ip-1").overpayments[0].outstanding_minor(), 5_000);
    let recovered = &w.core.noticed("overpayment_recovered")[0];
    assert_eq!((recovered["amount_minor"].as_str(), recovered["outstanding_minor"].as_str()), ("1000", "5000"));

    let write_off = Request::WriteOff { overpayment: op.into() };
    assert_eq!(
        w.undecided("ip-1", "sam", Request::WriteOff { overpayment: "nope".into() }, "R-OK"),
        "unknown_overpayment"
    );
    // The assessor may propose, but 5,000 is beyond their limit and over the two-signature amount.
    w.decided("ip-1", "ben", write_off.clone(), "R-OK");
    assert_eq!(w.unapproved("ip-1", "ana"), "authority");
    w.core.refuse_next = 1;
    assert_eq!(w.unapproved("ip-1", "sue"), "ledger");
    w.approved("ip-1", "sue");
    let o = &w.claim("ip-1").overpayments[0];
    assert_eq!((o.status, o.written_off_minor, o.outstanding_minor()), (RecoveryStatus::WrittenOff, 5_000, 0));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::WriteOffOverpayment { amount_minor: 5_000, .. })
    ));
    let written = w.core.noticed("overpayment_written_off");
    assert_eq!((written[0]["amount_minor"].as_str(), written[0]["insurer_error"].as_str()), ("5000", "true"));
    assert_eq!((written[0]["status"].as_str(), written[0]["outstanding_minor"].as_str()), ("written_off", "0"));
    assert_eq!(w.undecided("ip-1", "sam", write_off, "R-OK"), "unknown_overpayment");
    assert_eq!(w.undecided("ip-1", "sam", status(op, RecoveryStatus::Pursued), "R-OK"), "unknown_overpayment");
    let report = w.state.report();
    assert_eq!((report.recovered_minor, report.written_off_minor, report.held_minor), (1_000, 5_000, 0));
}

/// Purpose: a repayment in full marks the overpayment recovered.
/// Why it matters: a recovered overpayment that still showed as outstanding would keep a
/// customer on the recoveries list after they had paid.
#[test]
fn a_full_repayment_closes_the_overpayment() {
    let mut w = World::new();
    paid_two_months(&mut w);
    to_senior(&mut w, "ip-1");
    w.conference("ip-1", "sam");
    w.conversation("ip-1", "talk-1");
    w.decided("ip-1", "sam", REVERSE, "R-REV");
    w.approved("ip-1", "sue");
    w.ok(Command::RecordRepayment {
        claim: "ip-1".into(),
        overpayment: "ip-1/overpayment/1".into(),
        amount_minor: 6_000,
    });
    assert_eq!(w.claim("ip-1").overpayments[0].status, RecoveryStatus::Recovered);
}

/// Purpose: the finding on a reversal tells apart the three ways an admission goes wrong: the
/// assessor overrode a raised exclusion, the cause was recorded in a form the rules did not
/// match, or the rules had no mapping; and a false declaration by the claimant is not the
/// insurer's error, so its recovery is an ordinary senior decision.
/// Why it matters: each finding has a different fix (training, coding, rules), and the conduct
/// protection must not shelter someone who lied to obtain the benefit.
#[test]
fn the_finding_says_why_the_admission_was_wrong() {
    // The assessor overrode the exclusion: a lump sum, admitted and paid.
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            exclusions: vec!["EX-0001".into()],
            ..facts("trauma")
        },
    );
    w.open_claim("c-1");
    w.assign("c-1", "sam");
    w.assess("c-1", "CC-0001");
    let over = Request::Admit {
        amount_minor: Some(200_000),
        periodic: None,
        exclusion_override: Some("R-OVR".into()),
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    w.decided("c-1", "sam", over, "R-OK");
    w.verify_payee("c-1", "sam");
    w.ok(Command::Pay {
        claim: "c-1".into(),
        payment_ref: "p-1".into(),
        amount_minor: 200_000,
        reissue_of: None,
        actor: actor("sam"),
    });
    assert_eq!(w.claim("c-1").status, Status::Paid);
    w.ok(Command::Escalate {
        claim: "c-1".into(),
        actor: actor("sam"),
    });
    w.conference("c-1", "sam");
    w.conversation("c-1", "talk-1");
    let lied = Request::ReverseAdmission { false_declaration: true };
    w.decided("c-1", "sam", lied, "R-REV");
    w.approved("c-1", "sue");
    let c = w.claim("c-1");
    assert_eq!(c.decisions.last().expect("recorded").finding, Some(Finding::AssessorOverride));
    let o = c.overpayments[0].clone();
    assert_eq!(
        (o.amount_minor, o.cause, o.protected()),
        (200_000, OverpaymentCause::ClaimantFalseDeclaration, false)
    );
    assert_eq!(w.core.noticed("overpayment_raised")[0]["cause"], "ClaimantFalseDeclaration");
    // Not protected: a senior alone may pursue it.
    w.decided("c-1", "sam", status(&o.id, RecoveryStatus::Pursued), "R-OK");
    assert_eq!(w.claim("c-1").overpayments[0].status, RecoveryStatus::Pursued);

    // The cause was corrected after admission: the first coding was the fault.
    let mut w = World::new();
    paid_two_months(&mut w);
    to_senior(&mut w, "ip-1");
    w.ok(Command::RecordCause {
        claim: "ip-1".into(),
        cause_code: "CC-0001".into(),
        actor: actor("sam"),
    });
    w.conference("ip-1", "sam");
    w.conversation("ip-1", "talk-1");
    w.decided("ip-1", "sam", REVERSE, "R-REV");
    w.approved("ip-1", "sue");
    assert_eq!(w.claim("ip-1").decisions.last().expect("recorded").finding, Some(Finding::CauseMiscoded));
    assert_eq!(w.core.noticed("admission_reversed")[0]["cause"], "cause_miscoded");
}

/// Purpose: reversing an admission nothing was paid on raises no overpayment and releases what
/// was admitted, and a ledger refusal leaves the admission standing.
/// Why it matters: a zero overpayment would put an empty receivable in front of the recoveries
/// team, and a half-applied reversal would leave the claim closed with money still payable.
#[test]
fn a_reversal_before_payment_has_nothing_to_recover() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    w.decided("c-1", "ana", World::admit_lump(Some(100_000)), "R-OK");
    to_senior(&mut w, "c-1");
    w.conference("c-1", "sam");
    w.conversation("c-1", "talk-1");
    w.decided("c-1", "sam", REVERSE, "R-REV");
    w.core.refuse_next = 1;
    assert_eq!(w.unapproved("c-1", "sue"), "ledger");
    assert_eq!(w.claim("c-1").status, Status::Admitted);
    w.approved("c-1", "sue");
    let c = w.claim("c-1");
    assert_eq!((c.status, c.payable_minor, c.overpayments.len()), (Status::Reversed, 0, 0));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Close { overpayment: None, .. })));
    assert!(w.core.noticed("overpayment_raised").is_empty());
}

/// Purpose: a disputed item the reinsurance module raises from a reversal is recorded on the
/// claim, and reinsurance notices about an unknown claim are refused.
/// Why it matters: the recovery already taken from a reinsurer on the reversed claim is settled
/// by people under the treaty. The claim file must show that a dispute exists so nobody
/// treats the reinsurer's share as final.
#[test]
fn a_disputed_reinsurance_item_is_recorded_on_the_claim() {
    let mut w = World::new();
    w.open_claim("c-1");
    let dispute = |claim: &str| {
        Command::Reinsurance(ReinsuranceNotice::DisputedItemRaised {
            claim: claim.into(),
            reinsurer: "re-1".into(),
            reference: "DSP/c-1/treaty-1/re-1".into(),
        })
    };
    assert_eq!(w.refused(dispute("c-9")), "unknown_claim");
    w.ok(dispute("c-1"));
    assert_eq!(
        w.claim("c-1").reinsurance.disputed_items,
        vec![("re-1".to_string(), "DSP/c-1/treaty-1/re-1".to_string())]
    );
    w.ok(Command::Reinsurance(ReinsuranceNotice::LargeClaimNotification {
        claim: "c-1".into(),
        treaty: "treaty-1".into(),
    }));
    assert!(w.claim("c-1").reinsurance.large_claim_notified);
}
