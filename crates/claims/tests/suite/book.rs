//! Paying claims: lump sums, benefits in payment, payment runs, declared
//! income, offsets, the end of a benefit and a death notified late.

use ic_claims::command::ReopenBasis;
use ic_claims::model::{EndReason, OverpaymentCause, RecoveryStatus, RunStatus, TimerKind};
use ic_claims::{ClaimType, Command, CoverFacts, Instruction, PayeeKind, Request, RunKind, Status, TaskKind};

use crate::world::*;

fn pay(id: &str, who: &str, payment: &str, amount: i64, reissue_of: Option<&str>) -> Command {
    Command::Pay {
        claim: id.into(),
        payment_ref: payment.into(),
        amount_minor: amount,
        reissue_of: reissue_of.map(str::to_string),
        actor: actor(who),
    }
}

fn prepare(run: &str, benefit_date: chrono::NaiveDate) -> Command {
    Command::PrepareRun {
        run: run.into(),
        kind: RunKind::Benefit,
        benefit_date,
        actor: actor("ana"),
    }
}

fn approve_run(run: &str, who: &str) -> Command {
    Command::ApproveRun {
        run: run.into(),
        actor: actor(who),
    }
}

fn admitted(w: &mut World, id: &str, amount: i64) {
    w.open_claim(id);
    w.assess(id, "CC-0003");
    w.decided(id, "ana", World::admit_lump(Some(amount)), "R-OK");
}

fn income(id: &str, from: chrono::NaiveDate, other: i64) -> Command {
    Command::DeclareIncome {
        claim: id.into(),
        from,
        other_income_minor: other,
        actor: actor("ana"),
    }
}

/// Purpose: a payee is recorded only with an account, a passed name check and a passed
/// screening, and an estate above the simplified limit needs proof of legal authority.
/// Why it matters: paying the wrong person, a sanctioned person or someone with no authority
/// over an estate is money the insurer pays twice.
#[test]
fn a_payee_is_verified_before_anything_is_paid() {
    let mut w = World::new();
    admitted(&mut w, "c-1", 200_000);
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 1_000, None)), "payee_not_verified");
    let verify = |kind: PayeeKind, account: &str, doc: Option<&str>, who: &str| Command::VerifyPayee {
        claim: "c-1".into(),
        party: "party-estate".into(),
        kind,
        account_ref: account.into(),
        name_check_ref: "name-1".into(),
        screening_ref: "screen-1".into(),
        authority_doc: doc.map(str::to_string),
        actor: actor(who),
    };
    assert_eq!(w.refused(verify(PayeeKind::Beneficiary, "", None, "ana")), "payee_checks");
    assert_eq!(w.refused(verify(PayeeKind::Beneficiary, "acct-1", None, "ben")), "not_owner");
    assert_eq!(w.refused(verify(PayeeKind::Estate, "acct-1", None, "ana")), "authority_document_required");
    w.ok(verify(PayeeKind::Estate, "acct-1", Some("probate-1"), "ana"));
    let payee = w.claim("c-1").payee.clone().expect("recorded");
    assert_eq!(
        (payee.party.as_str(), payee.verified_by.as_str(), payee.verified_on),
        ("party-estate", "ana", d(2027, 3, 1))
    );
}

/// Purpose: a lump sum is released by its owner within their payment authority, in one payment
/// or several, never beyond what is admitted and unpaid, never twice under one reference; the
/// last payment closes the claim.
/// Why it matters: the released payment is the instruction the payments module sends to the
/// bank. A duplicate reference or an amount beyond the admission would be real money gone.
#[test]
fn a_lump_sum_is_released_within_what_was_admitted() {
    let mut w = World::new();
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    assert_eq!(w.refused(pay("c-1", "ben", "p-1", 50_000, None)), "not_owner");
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 0, None)), "exceeds_payable");
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 200_001, None)), "exceeds_payable");
    w.core.refuse_next = 1;
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 50_000, None)), "ledger");
    w.ok(pay("c-1", "ana", "p-1", 50_000, None));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Pay { amount_minor: 50_000, close: false, payee, .. }) if payee == "party-claimant"));
    let c = w.claim("c-1");
    assert_eq!((c.status, c.payable_minor, c.paid_minor(false)), (Status::Admitted, 150_000, 50_000));
    assert_eq!(c.timers[&TimerKind::Payment].met_on, Some(d(2027, 3, 1)));
    let notice = w.core.noticed("claim_paid")[0].clone();
    // The payments module settles on this policy's ledger and checks the account against its own payee.
    assert_eq!((notice["policy"].as_str(), notice["account_ref"].as_str()), (c.policy.as_str(), "acct-1"));
    assert_eq!(
        (notice["payment_ref"].as_str(), notice["amount_minor"].as_str(), notice["net_minor"].as_str()),
        ("p-1", "50000", "50000")
    );
    assert_eq!(
        (notice["payee"].as_str(), notice["period_end"].as_str(), notice["ex_gratia"].as_str()),
        ("party-claimant", "", "false")
    );
    assert_eq!(notice["run"], "", "released by hand, so in no run");
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 10_000, None)), "payment_exists");

    // A new authority version lowers the assessor's payment limit: it binds at once.
    let mut book = authority();
    book.version = 2;
    book.profiles.get_mut("assessor").expect("in the book").max_payment_minor = 100_000;
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    assert_eq!(w.refused(pay("c-1", "ana", "p-2", 150_000, None)), "authority");
    w.ok(pay("c-1", "ana", "p-2", 100_000, None));
    w.ok(pay("c-1", "ana", "p-3", 50_000, None));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Pay { close: true, .. })));
    let c = w.claim("c-1");
    assert_eq!((c.status, c.payable_minor, c.closed_on), (Status::Paid, 0, Some(d(2027, 3, 1))));
    assert_eq!(w.core.noticed("claim_closed")[0]["outcome"], "paid");
}

/// Purpose: a payment the bank returns goes back to what is payable on the claim, reopens a
/// paid claim, raises a task to pay it again, and can be paid again under a new reference.
/// Why it matters: returned claim money must never sit in suspense or be forgotten. The
/// claimant is still owed it, and the task is what makes someone find out why it bounced.
#[test]
fn a_returned_payment_is_owed_again() {
    let mut w = World::new();
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    w.ok(pay("c-1", "ana", "p-1", 200_000, None));
    let returned = |payment: &str| Command::PaymentReturned {
        claim: "c-1".into(),
        payment_ref: payment.into(),
    };
    assert_eq!(w.refused(returned("p-9")), "unknown_payment");
    w.core.refuse_next = 1;
    assert_eq!(w.refused(returned("p-1")), "ledger");
    w.ok(returned("p-1"));
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.payable_minor, c.closed_on, c.paid_minor(false)),
        (Status::Admitted, 200_000, None, 0)
    );
    assert!(c.tasks.iter().any(|t| t.id == "payment-failure/p-1" && t.kind == TaskKind::PaymentFailure));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::PaymentReturned { payment_ref, .. }) if payment_ref == "p-1"));
    assert_eq!(w.core.noticed("payment_returned")[0]["amount_minor"], "200000");
    assert_eq!(w.refused(returned("p-1")), "unknown_payment");
    w.ok(pay("c-1", "ana", "p-2", 200_000, Some("p-1")));
    assert_eq!(w.claim("c-1").status, Status::Paid);
    w.complete("c-1", "payment-failure/p-1", "repaid", "ana").expect("the owner closes the task");
}

/// Purpose: a benefit run is prepared by one person with a count and control total, approved by
/// a different person who is a lead, and then posts each period through the core; while a claim
/// waits in a run nothing else may change what it is paid.
/// Why it matters: the run is the largest regular movement of money out of the insurer. The
/// control total signed by a second person is what stops one person paying a book of claims,
/// and freezing the claims in the run is what makes the signed total the amount paid.
#[test]
fn a_benefit_run_is_prepared_by_one_person_and_approved_by_another() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    let c = w.claim("ip-1");
    assert_eq!((c.status, c.reserve_minor), (Status::InPayment, 48_000));
    assert_eq!(w.core.noticed("claim_admitted")[0]["monthly_minor"], "3000");
    assert_eq!(w.refused(approve_run("run-1", "sam")), "unknown_run");
    w.ok(prepare("run-1", d(2027, 3, 1)));
    assert_eq!(w.refused(prepare("run-1", d(2027, 3, 1))), "run_exists");
    assert_eq!(w.refused(prepare("run-2", d(2027, 3, 1))), "nothing_to_pay");
    let run = w.state.runs["run-1"].clone();
    assert_eq!((run.count, run.control_total_minor, run.status), (1, 3_000, RunStatus::Prepared));
    assert_eq!(w.core.noticed("run_prepared")[0]["control_total_minor"], "3000");

    // Frozen while it waits.
    assert_eq!(w.refused(income("ip-1", d(2027, 3, 1), 500)), "not_in_payment");
    let end = Request::EndBenefit {
        end: d(2027, 3, 1),
        reason: EndReason::Recovered,
    };
    assert_eq!(w.undecided("ip-1", "ana", end, "R-OK"), "in_run");

    assert_eq!(w.refused(approve_run("run-1", "ana")), "run_approval");
    assert_eq!(w.refused(approve_run("run-1", "ben")), "run_approval");
    w.ok(approve_run("run-1", "sam"));
    assert_eq!(w.refused(approve_run("run-1", "sue")), "run_approval");
    let Some(Instruction::PayBenefit {
        payment_ref,
        period_start,
        period_end,
        gross_minor,
        offset,
        close,
        ..
    }) = w.core.instructions.last()
    else {
        panic!("the run did not pay the benefit");
    };
    assert_eq!(
        (payment_ref.as_str(), *period_start, *period_end),
        ("run-1/ip-1/2027-03-01", d(2027, 3, 1), d(2027, 4, 1))
    );
    assert_eq!((*gross_minor, offset.is_none(), *close), (3_000, true, false));
    let c = w.claim("ip-1");
    let s = c.schedule.as_ref().expect("in payment");
    assert_eq!(
        (s.next_period, c.reserve_minor, c.in_run.clone(), c.paid_minor(false)),
        (d(2027, 4, 1), 45_000, None, 3_000)
    );
    let run = w.state.runs["run-1"].clone();
    assert_eq!(
        (run.status, run.submitted_minor, run.approved_by.as_deref()),
        (RunStatus::Submitted, 3_000, Some("sam"))
    );
    assert_eq!(w.core.noticed("claim_paid")[0]["period_end"], "2027-04-01");
    assert_eq!(w.core.noticed("run_submitted")[0]["exceptions"], "0");
    assert_eq!(w.state.remittance("run-1")["party-claimant"], (1, 3_000));
    assert!(w.state.remittance("run-9").is_empty());

    // The bank's total is matched to the run.
    let confirm = |run: &str, settled: i64| Command::ConfirmRun {
        run: run.into(),
        settled_minor: settled,
    };
    assert_eq!(w.refused(confirm("run-9", 3_000)), "unknown_run");
    w.ok(confirm("run-1", 2_900));
    assert_eq!(w.state.runs["run-1"].status, RunStatus::Unreconciled);
    assert_eq!(w.core.noticed("run_unreconciled")[0]["expected_minor"], "3000");
    assert_eq!(w.state.report().runs_unreconciled, 1);
    w.ok(confirm("run-1", 3_000));
    assert_eq!(w.state.runs["run-1"].status, RunStatus::Settled);
    assert_eq!(w.refused(confirm("run-1", 3_000)), "unknown_run");
}

/// Purpose: a prepared run can be cancelled, which releases its claims; a submitted run cannot.
/// Why it matters: a run prepared in error must be discardable without paying it, and the
/// claims in it must not stay frozen.
#[test]
fn a_prepared_run_can_be_cancelled() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.ok(prepare("run-1", d(2027, 3, 1)));
    let cancel = |run: &str| Command::CancelRun {
        run: run.into(),
        actor: actor("ben"),
    };
    assert_eq!(w.refused(cancel("run-9")), "unknown_run");
    w.ok(cancel("run-1"));
    assert_eq!(w.claim("ip-1").in_run, None);
    assert_eq!(w.state.runs["run-1"].status, RunStatus::Cancelled);
    assert_eq!(w.refused(cancel("run-1")), "unknown_run");
    w.pay_run("run-2", d(2027, 3, 1));
    assert_eq!(w.refused(cancel("run-2")), "unknown_run");
}

/// Purpose: a payment the core refuses inside a run becomes an exception with a task, later
/// periods of the same claim are held back, the rest of the run goes on, and the next run picks
/// the periods up again.
/// Why it matters: one bad claim must not stop a whole book being paid, and it must not be
/// silently skipped either: a benefit is someone's income.
#[test]
fn a_refused_payment_in_a_run_is_an_exception_not_a_stop() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.benefit_in_payment("ip-2");
    w.on(d(2027, 4, 1));
    w.ok(prepare("run-1", d(2027, 4, 1)));
    assert_eq!((w.state.runs["run-1"].count, w.state.runs["run-1"].control_total_minor), (4, 12_000));
    w.core.refuse_next = 1;
    w.ok(approve_run("run-1", "sam"));
    let run = w.state.runs["run-1"].clone();
    assert_eq!(run.submitted_minor, 6_000);
    assert_eq!(
        run.exceptions,
        vec![
            ("run-1/ip-1/2027-03-01".to_string(), "ledger".to_string()),
            ("run-1/ip-1/2027-04-01".to_string(), "prior_period_failed".to_string()),
        ]
    );
    assert_eq!(w.core.noticed("run_submitted")[0]["exceptions"], "2");
    let c = w.claim("ip-1");
    assert_eq!(c.schedule.as_ref().expect("in payment").next_period, d(2027, 3, 1));
    assert!(c.tasks.iter().any(|t| t.id == "payment-failure/run-1/ip-1/2027-03-01"));
    assert_eq!(w.claim("ip-2").schedule.as_ref().expect("in payment").next_period, d(2027, 5, 1));
    assert_eq!(w.state.remittance("run-1")["party-claimant"], (2, 6_000));

    w.pay_run("run-2", d(2027, 4, 1));
    assert_eq!(w.state.runs["run-2"].submitted_minor, 6_000);
    assert_eq!(w.claim("ip-1").schedule.as_ref().expect("in payment").next_period, d(2027, 5, 1));
}

/// Purpose: a benefit period that comes back from the bank is owed again as that period, is
/// taken off the run's expected settlement, and is reissued for exactly what came back.
/// Why it matters: the period record is what later replays (income, death, reversal) work
/// from. A returned period counted as paid would turn into a false overpayment.
#[test]
fn a_returned_benefit_period_is_reissued_as_that_period() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    let payment = "run-1/ip-1/2027-03-01";
    // The payments module confirms a run from the run named on each of its payments.
    assert_eq!(w.core.noticed("claim_paid")[0]["run"], "run-1");
    w.ok(Command::PaymentReturned {
        claim: "ip-1".into(),
        payment_ref: payment.into(),
    });
    let c = w.claim("ip-1");
    assert_eq!((c.status, c.payable_minor, c.paid_minor(false)), (Status::InPayment, 3_000, 0));
    assert_eq!(w.state.runs["run-1"].returned_minor, 3_000);
    w.ok(Command::ConfirmRun {
        run: "run-1".into(),
        settled_minor: 0,
    });
    assert_eq!(w.state.runs["run-1"].status, RunStatus::Settled);

    assert_eq!(w.refused(pay("ip-1", "ana", "again-1", 2_999, Some(payment))), "reissue_amount");
    assert_eq!(w.refused(pay("ip-1", "ana", payment, 3_000, Some(payment))), "payment_exists");
    w.ok(pay("ip-1", "ana", "again-1", 3_000, Some(payment)));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Pay { close: false, .. })));
    let c = w.claim("ip-1");
    let period = &c.schedule.as_ref().expect("in payment").periods[&d(2027, 3, 1)];
    assert_eq!((period.returned, period.payment_ref.as_str()), (false, "again-1"));
    assert_eq!(
        (c.status, c.payable_minor, c.paid_minor(false), c.payments.len()),
        (Status::InPayment, 0, 3_000, 0)
    );
    assert_eq!(w.core.noticed("claim_paid").last().expect("published")["period_end"], "2027-04-01");
}

/// Purpose: other income declared for a benefit reduces later periods, and periods already paid
/// are replayed: too much paid becomes an overpayment held as a fact, too little is owed and
/// goes out with the next benefit run, with a senior told when the person also owes an
/// overpayment; arrears paid with a period are never counted as overpaid for that period.
/// Why it matters: income offsets are the commonest reason a benefit in payment is wrong. The
/// replay must find the exact amount, and neither take money back by itself nor leave a
/// claimant short without anyone knowing.
#[test]
fn declared_income_replays_the_periods_already_paid() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    assert_eq!(w.refused(income("ip-1", d(2027, 3, 1), -1)), "not_in_payment");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    w.core.refuse_next = 1;
    assert_eq!(w.refused(income("ip-1", d(2027, 4, 1), 1_000)), "ledger");
    w.ok(income("ip-1", d(2027, 4, 1), 1_000));
    let c = w.claim("ip-1");
    let o = &c.overpayments[0];
    assert_eq!(
        (o.amount_minor, o.cause, o.status, o.protected()),
        (1_000, OverpaymentCause::LateInformation, RecoveryStatus::Held, false)
    );
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::RaiseOverpayment { amount_minor: 1_000, .. })
    ));

    // Held: the next benefit is reduced by the income, and nothing is offset.
    w.on(d(2027, 5, 1));
    w.pay_run("run-3", d(2027, 5, 1));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::PayBenefit {
            gross_minor: 2_000,
            offset: None,
            ..
        })
    ));

    // The income turns out lower from April: May and April were underpaid.
    w.ok(income("ip-1", d(2027, 4, 1), 400));
    let c = w.claim("ip-1").clone();
    assert_eq!(c.overpayments.len(), 1);
    assert!(c
        .tasks
        .iter()
        .any(|t| t.id == "senior-review/underpaid/2027-04-01" && t.kind == TaskKind::SeniorReview));
    assert_eq!(w.refused(income("c-none", d(2027, 4, 1), 0)), "unknown_claim");

    // The 600 short on each of April and May is owed, and goes out with June's benefit in the
    // ordinary run: 2,600 for June and 1,200 of arrears.
    assert_eq!(c.schedule.as_ref().expect("in payment").arrears_minor, 1_200);
    assert_eq!(w.core.noticed("underpayment_owed")[0]["amount_minor"], "1200");
    w.on(d(2027, 6, 1));
    w.pay_run("run-4", d(2027, 6, 1));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::PayBenefit {
            gross_minor: 3_800,
            offset: None,
            ..
        })
    ));
    let s = w.claim("ip-1").schedule.clone().expect("in payment");
    assert_eq!((s.arrears_minor, s.periods[&d(2027, 6, 1)].arrears_minor), (0, 1_200));
    // July is the plain benefit again.
    w.on(d(2027, 7, 1));
    w.pay_run("run-5", d(2027, 7, 1));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::PayBenefit { gross_minor: 2_600, .. })));

    // A death on 15 June ends the benefit from the 16th: half of June's benefit and all of July's were paid past it. The
    // arrears paid with June were owed for April and May and are not part of the overpayment.
    w.on(d(2027, 7, 20));
    w.ok(Command::RecordDeath {
        party: "party-life".into(),
        date_of_death: d(2027, 6, 15),
    });
    let late = w.claim("ip-1").overpayments.last().expect("raised").clone();
    assert_eq!(
        (late.cause, late.amount_minor),
        (OverpaymentCause::LateDeathNotification, 2_600 * 15 / 30 + 2_600)
    );
}

/// Purpose: an overpayment is taken from later benefits only once its status is set to offset,
/// at no more than the agreed share of each benefit, across the same person's claims, until it
/// is recovered.
/// Why it matters: an offset is the insurer paying itself out of someone's income. It happens
/// only by decision, is capped so the person can still live, and stops exactly when the debt
/// is cleared.
#[test]
fn an_offset_is_taken_only_by_decision_and_within_the_cap() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.benefit_in_payment("ip-2");
    w.pay_run("run-1", d(2027, 3, 1));
    w.ok(income("ip-1", d(2027, 3, 1), 1_000));
    let op = w.claim("ip-1").overpayments[0].id.clone();
    let offset = Request::RecoveryStatus {
        overpayment: op.clone(),
        status: RecoveryStatus::Offset,
        offset_pct: 25,
        instalment_minor: 0,
    };
    w.decided("ip-1", "sam", offset, "R-OK");
    assert_eq!(w.claim("ip-1").overpayments[0].offset_pct, 25);

    // April: 2,000 after income on one claim and 3,000 on the person's other claim. Each gives
    // up no more than a quarter, and together no more than is owed.
    w.on(d(2027, 4, 1));
    w.ok(prepare("run-2", d(2027, 4, 1)));
    let run = w.state.runs["run-2"].clone();
    assert_eq!((run.count, run.control_total_minor), (2, 4_000));
    assert!(run
        .items
        .iter()
        .all(|i| i.offset.as_ref().is_some_and(|o| o.amount_minor == 500 && o.amount_minor * 4 <= i.gross_minor)));
    w.ok(approve_run("run-2", "sam"));
    let c = w.claim("ip-1");
    assert_eq!(
        (c.overpayments[0].recovered_minor, c.overpayments[0].status),
        (1_000, RecoveryStatus::Recovered)
    );
    assert_eq!(c.schedule.as_ref().expect("in payment").periods[&d(2027, 4, 1)].offset_minor, 500);
    let paid: Vec<_> = w
        .core
        .noticed("claim_paid")
        .into_iter()
        .filter(|n| n["payment_ref"] == "run-2/ip-1/2027-04-01")
        .collect();
    assert_eq!((paid[0]["amount_minor"].as_str(), paid[0]["net_minor"].as_str()), ("2000", "1500"));

    // May: nothing is left to take, and both benefits are paid in full.
    w.on(d(2027, 5, 1));
    w.ok(prepare("run-3", d(2027, 5, 1)));
    assert_eq!(w.state.runs["run-3"].control_total_minor, 5_000);
}

/// Purpose: an overpayment on a closed claim is offset against the same person's benefit on
/// another claim, and the instruction names the claim and policy the receivable sits on.
/// Why it matters: the receivable is on the old claim's policy. The core has to credit that
/// policy's receivable, not the paying claim's, or the daily ledger control breaks; and the
/// recovery is published on the old claim so reinsurers share in it.
#[test]
fn an_offset_can_recover_another_claims_overpayment() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    let end = Request::EndBenefit {
        end: d(2027, 3, 17),
        reason: EndReason::Recovered,
    };
    w.decided("ip-1", "ana", end, "R-OK");
    let c = w.claim("ip-1");
    // 15 of March's 31 days were paid past the end.
    assert_eq!((c.status, c.overpayments[0].amount_minor), (Status::Ended, 1_451));
    let op = c.overpayments[0].id.clone();
    let offset = Request::RecoveryStatus {
        overpayment: op.clone(),
        status: RecoveryStatus::Offset,
        offset_pct: 10,
        instalment_minor: 0,
    };
    w.decided("ip-1", "sam", offset, "R-OK");

    w.benefit_in_payment("ip-2");
    w.pay_run("run-2", d(2027, 3, 1));
    let Some(Instruction::PayBenefit { claim, offset: Some(o), .. }) = w.core.instructions.last() else {
        panic!("the benefit was paid without the offset");
    };
    assert_eq!(
        (claim.as_str(), o.claim.as_str(), o.policy.as_str(), o.overpayment.as_str(), o.amount_minor),
        ("ip-2", "ip-1", "pol-2", op.as_str(), 300)
    );
    assert_eq!(w.claim("ip-1").overpayments[0].recovered_minor, 300);
    // Published for the overpayment's own claim, so reinsurers share in what came back.
    let recovered = w.core.noticed("overpayment_recovered");
    let last = recovered.last().expect("published");
    assert_eq!(
        (
            last["claim"].as_str(),
            last["amount_minor"].as_str(),
            last["outstanding_minor"].as_str(),
            last["source"].as_str()
        ),
        ("ip-1", "300", "1151", "offset")
    );
}

/// Purpose: ending a benefit in the past closes the claim and raises what was paid beyond the
/// end as an overpayment; ending it in the future lets the last, part period be paid and then
/// closes it; the same condition returning inside the window reopens the same claim.
/// Why it matters: a benefit that does not stop keeps paying, and one that stops without
/// replaying the paid periods hides an overpayment. Recurrence without a new waiting period is
/// a contractual promise.
#[test]
fn a_benefit_ends_closes_and_can_recur() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    let end = |on: chrono::NaiveDate| Request::EndBenefit {
        end: on,
        reason: EndReason::Recovered,
    };
    assert_eq!(w.undecided("ip-1", "ana", end(d(2029, 3, 2)), "R-OK"), "invalid_end");
    w.pay_run("run-1", d(2027, 3, 1));
    w.decided("ip-1", "ana", end(d(2027, 4, 16)), "R-OK");
    let c = w.claim("ip-1");
    let s = c.schedule.as_ref().expect("in payment");
    assert_eq!((c.status, s.end, s.ended), (Status::InPayment, d(2027, 4, 16), Some(EndReason::Recovered)));

    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::PayBenefit {
            gross_minor: 1_500,
            close: true,
            ..
        })
    ));
    let c = w.claim("ip-1");
    assert_eq!((c.status, c.reserve_minor, c.closed_on), (Status::Ended, 0, Some(d(2027, 4, 1))));
    assert_eq!(w.core.noticed("claim_closed")[0]["outcome"], "benefit_ended");
    assert_eq!(w.undecided("ip-1", "ana", end(d(2027, 4, 10)), "R-OK"), "invalid_end");

    let recur = |who: &str| Command::Reopen {
        claim: "ip-1".into(),
        basis: ReopenBasis::Recurrence,
        reserve_minor: 20_000,
        actor: actor(who),
    };
    // A recurrence inside the window serves no new waiting period, whatever the cover's is.
    w.core.cover(
        "pol-2",
        "ip",
        ic_claims::port::CoverFacts {
            sum_insured_minor: 4_000,
            waiting_days: 91,
            ..facts("income_protection")
        },
    );
    w.on(d(2027, 6, 1));
    w.ok(recur("ben"));
    let c = w.claim("ip-1");
    let s = c.schedule.as_ref().expect("in payment");
    assert_eq!((c.status, c.reserve_minor, c.overturns), (Status::InPayment, 20_000, 0));
    assert_eq!(
        (s.end, s.next_period, s.ended, s.case_manager.as_deref()),
        (d(2029, 3, 1), d(2027, 6, 1), None, Some("ben"))
    );
    w.verify_payee("ip-1", "ben");
    w.pay_run("run-3", d(2027, 6, 1));
    assert_eq!(w.claim("ip-1").paid_minor(false), 7_500);

    // Ended again, and this time the window passes.
    w.decided("ip-1", "ben", end(d(2027, 7, 1)), "R-OK");
    assert_eq!(w.claim("ip-1").status, Status::Ended);
    w.on(d(2027, 12, 29));
    assert_eq!(w.refused(recur("ben")), "recurrence_window");
}

/// Purpose: a death notified late ends every benefit in payment to that person from the day
/// after the death, records what was paid past it as an overpayment to recover from the estate,
/// and prompts a check of the death register feed (design addendum, entry 1).
/// Why it matters: benefits paid to someone who has died are the addendum's own example. The
/// amount must be exact to the day, recovery is from the estate rather than a held insurer
/// error, and a late notification is evidence that the register feed is too slow.
#[test]
fn a_death_notified_late_ends_the_benefit_and_raises_the_overpayment() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    w.on(d(2027, 4, 20));
    let died = |party: &str, on: chrono::NaiveDate| Command::RecordDeath {
        party: party.into(),
        date_of_death: on,
    };
    assert_eq!(w.refused(died("party-life", d(2027, 4, 21))), "event_in_future");
    w.ok(died("party-nobody", d(2027, 3, 10)));
    w.ok(died("party-life", d(2027, 3, 10)));
    let c = w.claim("ip-1").clone();
    let s = c.schedule.as_ref().expect("kept");
    assert_eq!((c.status, s.end, s.ended), (Status::Ended, d(2027, 3, 11), Some(EndReason::Death)));
    // 21 of March's 31 days and all of April were paid past the death.
    let o = &c.overpayments[0];
    assert_eq!(
        (o.amount_minor, o.cause, o.status, o.protected()),
        (2_032 + 3_000, OverpaymentCause::LateDeathNotification, RecoveryStatus::Held, false)
    );
    let decision = c.decisions.last().expect("recorded");
    assert!(decision.straight_through && decision.signers.is_empty());
    assert_eq!(decision.reasons, vec!["death_notified".to_string()]);
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Close { outcome, overpayment: Some((_, 5_032)), .. }) if outcome == "benefit_ended"));
    assert!(c.has_open_task(TaskKind::DeathRegisterCadence));
    assert_eq!(w.core.noticed("death_register_cadence_check").len(), 1);

    // The cadence check is the lead's to close, and recovery from the estate is a senior's call.
    assert_eq!(
        w.complete("ip-1", "death-register-cadence", "checked", "ana").expect_err("not a lead").code,
        "not_authorised"
    );
    w.complete("ip-1", "death-register-cadence", "checked", "sam").expect("a lead closes it");
    let pursue = Request::RecoveryStatus {
        overpayment: o.id.clone(),
        status: RecoveryStatus::Pursued,
        offset_pct: 0,
        instalment_minor: 0,
    };
    w.decided("ip-1", "sam", pursue, "R-OK");
    assert_eq!(w.claim("ip-1").overpayments[0].status, RecoveryStatus::Pursued);

    // The customer master may send the death again. The benefit has ended, so nothing more is raised.
    w.ok(died("party-life", d(2027, 3, 10)));
    assert_eq!(w.claim("ip-1").overpayments.len(), 1);
}

/// Purpose: a death notified promptly ends the benefit at the day after death without a
/// register check, the part period still owed is paid, and a claim that cannot be ended at
/// once (it is in a run, or the ledger refuses) goes to a senior instead of being dropped.
/// Why it matters: the death command comes from the customer master and cannot be bounced
/// back. Whatever state the claim is in, the death has to end up acted on by someone.
#[test]
fn a_death_is_never_dropped() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.on(d(2027, 3, 12));
    w.ok(Command::RecordDeath {
        party: "party-life".into(),
        date_of_death: d(2027, 3, 10),
    });
    let c = w.claim("ip-1");
    assert_eq!((c.status, c.schedule.as_ref().expect("in payment").end), (Status::InPayment, d(2027, 3, 11)));
    assert!(!c.has_open_task(TaskKind::DeathRegisterCadence) && c.overpayments.is_empty());
    w.pay_run("run-1", d(2027, 3, 12));
    // Ten of March's 31 days.
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::PayBenefit {
            gross_minor: 967,
            close: true,
            ..
        })
    ));
    assert_eq!(w.claim("ip-1").status, Status::Ended);

    // In a run: left for a senior.
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 1));
    w.ok(prepare("run-2", d(2027, 4, 1)));
    w.ok(Command::RecordDeath {
        party: "party-life".into(),
        date_of_death: d(2027, 3, 20),
    });
    let c = w.claim("ip-1");
    assert_eq!(c.status, Status::InPayment);
    assert!(c.tasks.iter().any(|t| t.id == "senior-review/death"));

    // The ledger refuses the close: also left for a senior.
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 3, 25));
    w.core.refuse_next = 1;
    w.ok(Command::RecordDeath {
        party: "party-life".into(),
        date_of_death: d(2027, 3, 20),
    });
    let c = w.claim("ip-1");
    assert_eq!((c.status, c.overpayments.len()), (Status::InPayment, 0));
    assert!(c.tasks.iter().any(|t| t.id == "senior-review/death"));
}

/// Purpose: a benefit that is due but has no verified payee is not skipped in silence: a task
/// is raised on the claim, and the other claims in the run are still paid.
/// Why it matters: without the task, a claimant whose bank details were never verified would
/// simply not be paid, month after month, and nobody would be told.
#[test]
fn a_benefit_with_no_payee_raises_a_task() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    let mut c = w.claim("ip-1").clone();
    c.id = "ip-0".into();
    c.payee = None;
    w.state.claims.insert("ip-0".into(), c);
    w.state.in_payment.insert("ip-0".into());
    w.ok(prepare("run-1", d(2027, 3, 1)));
    assert_eq!(w.state.runs["run-1"].count, 1);
    assert!(w.claim("ip-0").tasks.iter().any(|t| t.id == "payment-failure/no-payee/2027-03-01"));
    w.ok(approve_run("run-1", "sam"));

    // Alone, it still raises its task, and no empty run is made.
    w.on(d(2027, 4, 1));
    w.state.in_payment.remove("ip-1");
    w.ok(prepare("run-2", d(2027, 4, 1)));
    assert!(!w.state.runs.contains_key("run-2"));
    assert!(w.claim("ip-0").tasks.iter().any(|t| t.id == "payment-failure/no-payee/2027-04-01"));
}

/// Purpose: the reviews on a benefit's plan fall due by date, become tasks, are flagged overdue
/// after the grace period with a senior review, and never stop the benefit; a claim that runs
/// past its expected end is put in front of a senior.
/// Why it matters: claims in payment are a long-lived book. Reviews that depend on someone's
/// diary are missed; reviews that stop payment when missed punish the claimant for the
/// insurer's lateness.
#[test]
fn reviews_fall_due_by_date_and_never_stop_the_benefit() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    let s = w.claim("ip-1").schedule.clone().expect("in payment");
    assert_eq!(s.reviews[&TaskKind::IncomeDeclaration], d(2027, 6, 1));
    assert_eq!(s.reviews[&TaskKind::MedicalReview], d(2027, 9, 1));
    w.on(d(2027, 6, 1));
    assert!(w.state.due_backlog(d(2027, 6, 1)) >= 1);
    assert_eq!(w.state.due_backlog(d(2027, 3, 1)), 0);
    w.ok(Command::DrainDue);
    let c = w.claim("ip-1");
    assert!(c
        .tasks
        .iter()
        .any(|t| t.id == "review/IncomeDeclaration/2027-06-01" && t.due == Some(d(2027, 6, 1))));
    assert_eq!(c.schedule.as_ref().expect("in payment").reviews[&TaskKind::IncomeDeclaration], d(2027, 9, 1));
    assert_eq!(w.core.noticed("review_due")[0]["review"], "IncomeDeclaration");
    assert_eq!(w.state.book(d(2027, 6, 1)).reviews_open, 1);

    w.on(d(2027, 6, 15));
    w.ok(Command::DrainDue);
    let c = w.claim("ip-1");
    assert_eq!(c.schedule.as_ref().expect("in payment").reviews_overdue, 1);
    assert!(c.tasks.iter().any(|t| t.id == "senior-review/overdue/IncomeDeclaration/2027-06-15"));
    assert_eq!(w.core.noticed("review_overdue").len(), 1);
    assert_eq!(c.status, Status::InPayment);
    w.pay_run("run-1", d(2027, 6, 15));
    assert_eq!(w.state.runs["run-1"].count, 4);

    w.complete("ip-1", "review/IncomeDeclaration/2027-06-01", "declared", "ana")
        .expect("the case manager completes it");
    assert_eq!(w.claim("ip-1").schedule.as_ref().expect("in payment").reviews_overdue, 0);

    // Past the expected end, still in payment.
    w.on(d(2028, 3, 1));
    w.ok(Command::DrainDue);
    let c = w.claim("ip-1");
    assert!(c.tasks.iter().any(|t| t.id == "senior-review/duration"));
    assert!(c.tasks.iter().any(|t| t.id == "review/MedicalReview/2027-09-01"));
}

fn pay_cost(id: &str, who: &str, payment: &str, item: &str, provider: &str) -> Command {
    Command::PayCost {
        claim: id.into(),
        payment_ref: payment.into(),
        item: item.into(),
        provider: provider.into(),
        actor: actor(who),
    }
}

/// Evidence `item` asked for on a claim and, when `cost` is given, received at that cost.
fn evidence(w: &mut World, id: &str, item: &str, cost: Option<i64>) {
    w.ok(Command::RequestEvidence {
        claim: id.into(),
        evidence: item.into(),
        kind: "medical_report".into(),
        from: "treating-doctor".into(),
        actor: actor("ana"),
    });
    if let Some(cost_minor) = cost {
        w.ok(Command::ReceiveEvidence {
            claim: id.into(),
            evidence: item.into(),
            vault_ref: format!("vault-{item}"),
            cost_minor,
        });
    }
}

/// Purpose: the cost of received evidence or a completed rehabilitation task is paid to its
/// provider once, for exactly the amount recorded on the item, by the claim's owner within
/// their payment authority, as a handling cost and never as a benefit.
/// Why it matters: a doctor's report fee or a rehabilitation invoice is real money leaving the
/// insurer. If it could be paid twice, for an amount nobody recorded, or be counted as benefit
/// paid to the claimant, claims cost and the claimant's own entitlement would both be wrong.
#[test]
fn a_handling_cost_is_paid_once_to_its_provider() {
    let mut w = World::new();
    w.open_claim("c-1");
    evidence(&mut w, "c-1", "e-1", Some(150));
    evidence(&mut w, "c-1", "e-2", None);
    w.task("c-1", "rehab", TaskKind::Rehabilitation, "ana");

    assert_eq!(w.refused(pay_cost("c-1", "ben", "k-1", "e-1", "lab")), "not_owner");
    // Not received, not completed, unknown, or with nobody to pay: there is no cost to pay.
    for (item, provider) in [("e-2", "lab"), ("rehab", "gym"), ("e-9", "lab"), ("e-1", "")] {
        assert_eq!(w.refused(pay_cost("c-1", "ana", "k-1", item, provider)), "no_cost");
    }
    w.core.refuse_next = 1;
    assert_eq!(w.refused(pay_cost("c-1", "ana", "k-1", "e-1", "lab")), "ledger");
    w.ok(pay_cost("c-1", "ana", "k-1", "e-1", "lab"));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::HandlingCost { payment_ref, amount_minor: 150, payee, kind, .. })
            if payment_ref == "k-1" && payee == "lab" && kind == "evidence"
    ));
    let notice = w.core.noticed("claim_cost_paid")[0].clone();
    assert_eq!(
        (notice["policy"].as_str(), notice["payment_ref"].as_str(), notice["net_minor"].as_str()),
        (w.claim("c-1").policy.as_str(), "k-1", "150")
    );
    assert_eq!(
        (notice["payee"].as_str(), notice["kind"].as_str(), notice["item"].as_str()),
        ("lab", "evidence", "e-1")
    );
    // No benefit was paid and nothing is owed to the claimant because of it.
    assert!(w.core.noticed("claim_paid").is_empty());
    let c = w.claim("c-1");
    assert_eq!((c.paid_minor(false), c.payable_minor, c.cost_payable_minor), (0, 0, 0));
    assert_eq!(w.refused(pay_cost("c-1", "ana", "k-2", "e-1", "lab")), "cost_paid");

    w.ok(Command::CompleteTask {
        claim: "c-1".into(),
        task: "rehab".into(),
        outcome: "programme finished".into(),
        attendees: vec![],
        vault_ref: None,
        spend_minor: 2_000,
        actor: actor("ana"),
    });
    assert_eq!(w.refused(pay_cost("c-1", "ana", "k-1", "rehab", "gym")), "payment_exists");
    // A new authority version puts the spend above the assessor's payment limit.
    let mut book = authority();
    book.version = 2;
    book.profiles.get_mut("assessor").expect("in the book").max_payment_minor = 1_999;
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    assert_eq!(w.refused(pay_cost("c-1", "ana", "k-2", "rehab", "gym")), "authority");
    let mut book = authority();
    book.version = 3;
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    w.ok(pay_cost("c-1", "ana", "k-2", "rehab", "gym"));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::HandlingCost { amount_minor: 2_000, kind, .. }) if kind == "rehabilitation"
    ));
}

/// Purpose: a handling cost payment the bank returns is owed to the provider again and to
/// nobody else: the claimant cannot be paid it, a paid claim stays paid, and paying the provider
/// again releases the same money without expensing the cost a second time.
/// Why it matters: returned money sits in the same payable as the claimant's benefit. Without
/// keeping the two apart, a bounced lab fee could be paid out to the claimant, or expensed twice
/// when it is sent again, and a settled claim would reopen over an invoice.
#[test]
fn a_returned_handling_cost_is_owed_to_the_provider_again() {
    let mut w = World::new();
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    evidence(&mut w, "c-1", "e-1", Some(150));
    w.ok(pay_cost("c-1", "ana", "k-1", "e-1", "lab"));
    w.ok(pay("c-1", "ana", "p-1", 200_000, None));
    let returned = |claim: &str, payment: &str| Command::PaymentReturned {
        claim: claim.into(),
        payment_ref: payment.into(),
    };
    w.ok(returned("c-1", "k-1"));
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.payable_minor, c.cost_payable_minor, c.benefit_payable_minor()),
        (Status::Paid, 150, 150, 0)
    );
    assert!(c.tasks.iter().any(|t| t.id == "payment-failure/k-1"));
    assert_eq!(w.refused(pay("c-1", "ana", "p-2", 150, None)), "exceeds_payable");
    w.ok(pay_cost("c-1", "ana", "k-2", "e-1", "lab"));
    // Released from what came back, and the claim's ledger is closed again.
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay { payment_ref, amount_minor: 150, payee, close: true, .. }) if payment_ref == "k-2" && payee == "lab"
    ));
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.payable_minor, c.cost_payable_minor, c.paid_minor(false)),
        (Status::Paid, 0, 0, 200_000)
    );
    assert_eq!(w.core.noticed("claim_cost_paid").len(), 2);

    // The cost comes back before the benefit is paid: the benefit leaves the claim open, and
    // paying the provider again is what closes it.
    admitted(&mut w, "c-2", 200_000);
    w.verify_payee("c-2", "ana");
    evidence(&mut w, "c-2", "e-1", Some(150));
    w.ok(pay_cost("c-2", "ana", "k-1", "e-1", "lab"));
    w.ok(returned("c-2", "k-1"));
    assert_eq!(w.refused(pay("c-2", "ana", "p-1", 200_150, None)), "exceeds_payable");
    w.ok(pay("c-2", "ana", "p-1", 200_000, None));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Pay { close: false, .. })));
    assert_eq!(w.claim("c-2").status, Status::Admitted);
    w.ok(pay_cost("c-2", "ana", "k-2", "e-1", "lab"));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Pay { close: true, .. })));
    let c = w.claim("c-2");
    assert_eq!((c.status, c.payable_minor, c.closed_on.is_some()), (Status::Paid, 0, true));
}

/// Purpose: where the rules set late payment interest, a lump sum released after the free days
/// carries simple interest from the end of those days to the day it is released, shown apart
/// from the benefit; a payment inside the free days carries none; and a payment the bank
/// returns is sent again with the interest it had, not with more.
/// Why it matters: interest for delay is owed by law or by the product and is a cost of the
/// insurer's own slowness. It must be worked out the same way every time, never be counted as
/// benefit paid against the sum insured, and never grow because a bank sent the money back.
#[test]
fn a_lump_sum_paid_late_carries_interest() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    // Two basis points a day after ten free days.
    r.late_interest = Some(ic_claims::config::LateInterest {
        annual_rate_bps: 730,
        free_days: 10,
    });
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    w.advance(10);
    w.ok(pay("c-1", "ana", "p-1", 100_000, None));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Pay { interest_minor: 0, .. })));
    assert_eq!(w.core.noticed("claim_paid")[0]["net_minor"], "100000");

    w.advance(10);
    w.ok(pay("c-1", "ana", "p-2", 100_000, None));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            amount_minor: 100_000,
            interest_minor: 200,
            close: true,
            ..
        })
    ));
    let notice = w.core.noticed("claim_paid")[1].clone();
    assert_eq!(
        (notice["amount_minor"].as_str(), notice["net_minor"].as_str(), notice["interest_minor"].as_str()),
        ("100000", "100200", "200")
    );
    let c = w.claim("c-1");
    // Interest is not benefit: the claim has paid what was admitted and no more.
    assert_eq!((c.status, c.paid_minor(false), c.payable_minor), (Status::Paid, 200_000, 0));

    w.ok(Command::PaymentReturned {
        claim: "c-1".into(),
        payment_ref: "p-2".into(),
    });
    let back = w.core.noticed("payment_returned")[0].clone();
    assert_eq!((back["amount_minor"].as_str(), back["interest_minor"].as_str()), ("100200", "200"));
    assert_eq!((back["ex_gratia"].as_str(), back["cost"].as_str()), ("false", "false"));
    assert_eq!(w.claim("c-1").payable_minor, 100_200);
    w.advance(30);
    w.ok(pay("c-1", "ana", "p-3", 100_200, Some("p-2")));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            amount_minor: 100_200,
            interest_minor: 0,
            close: true,
            ..
        })
    ));
    let notice = w.core.noticed("claim_paid")[2].clone();
    assert_eq!(
        (notice["amount_minor"].as_str(), notice["net_minor"].as_str(), notice["interest_minor"].as_str()),
        ("100000", "100200", "200")
    );
    let c = w.claim("c-1");
    let last = c.payments.last().expect("the reissue");
    assert_eq!((last.amount_minor, last.interest_minor), (100_000, 200));
    assert_eq!((c.status, c.paid_minor(false), c.payable_minor), (Status::Paid, 200_000, 0));
}

fn waivers(w: &World) -> Vec<(chrono::NaiveDate, chrono::NaiveDate)> {
    let asked = w.core.instructions.iter().filter_map(|i| match i {
        Instruction::WaivePremium { from, until, .. } => Some((*from, *until)),
        _ => None,
    });
    asked.collect()
}

/// Purpose: the owner of a benefit in payment starts a premium waiver from a date inside it;
/// the core is asked at once and again every day to pay the billed premium from claims
/// expense, a day the core refuses is tried again the next, and the waiver's window closes
/// where the benefit ends.
/// Why it matters: a disabled customer whose cover includes waiver must not be billed into
/// arrears and lapse while on claim. Billing dates belong to the core, so the only way claims
/// can be sure no premium is missed is to ask every day for exactly the benefit's own window.
#[test]
fn premiums_are_waived_beside_a_benefit_in_payment() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    admitted(&mut w, "c-1", 1_000);
    let start = |claim: &str, who: &str, from: chrono::NaiveDate| Command::StartWaiver {
        claim: claim.into(),
        from,
        actor: actor(who),
    };
    assert_eq!(w.refused(start("ip-1", "ben", d(2027, 3, 1))), "not_owner");
    // A lump sum has no benefit to run beside; the date must fall inside the benefit.
    assert_eq!(w.refused(start("c-1", "ana", d(2027, 3, 1))), "not_in_payment");
    assert_eq!(w.refused(start("ip-1", "ana", d(2029, 3, 1))), "not_in_payment");
    assert_eq!(w.refused(start("ip-1", "ana", d(2027, 2, 1))), "not_in_payment");
    w.core.refuse_next = 1;
    assert_eq!(w.refused(start("ip-1", "ana", d(2027, 3, 1))), "ledger");
    assert_eq!(w.claim("ip-1").waiver_from, None);
    w.ok(start("ip-1", "ana", d(2027, 3, 1)));
    assert_eq!(waivers(&w), vec![(d(2027, 3, 1), d(2029, 3, 1))]);
    assert_eq!(w.core.noticed("premium_waiver_started")[0]["from"], "2027-03-01");
    assert_eq!(w.refused(start("ip-1", "ana", d(2027, 3, 1))), "waiver_exists");

    w.advance(1);
    w.ok(Command::DrainDue);
    assert_eq!(waivers(&w).len(), 2);
    // The core refuses for a day: it is noted and asked again the next.
    w.advance(1);
    w.core.refuse_next = 1;
    w.ok(Command::DrainDue);
    assert_eq!(waivers(&w).len(), 2);
    assert!(w.claim("ip-1").history.iter().any(|h| h.action == "waiver_refused"));
    w.advance(1);
    w.ok(Command::DrainDue);
    assert_eq!(waivers(&w).len(), 3);

    // The benefit ends: the window the core is given ends with it.
    let ends = w.core.today;
    let end = Request::EndBenefit {
        end: ends,
        reason: EndReason::Recovered,
    };
    w.decided("ip-1", "ana", end, "R-OK");
    w.advance(1);
    w.ok(Command::DrainDue);
    assert_eq!(waivers(&w).last(), Some(&(d(2027, 3, 1), ends)));
}

/// Purpose: a claim that is only a premium waiver is admitted with no money benefit, starts
/// waiving from its first day without anyone asking, is never put in a payment run, and ends
/// itself when its last day has passed.
/// Why it matters: a waiver claim pays nobody, so none of the machinery that moves a benefit
/// claim along (the payment timer, the run, the last payment closing it) applies. Without its
/// own path it would either pay the premium out as cash or stay open for ever.
#[test]
fn a_waiver_only_claim_waives_from_admission_and_ends_itself() {
    let mut w = World::new();
    w.ok(Command::Notify(Box::new(World::notification(
        "w-1",
        "pol-2",
        "ip",
        ic_claims::ClaimType::PremiumWaiver,
    ))));
    w.ok(Command::Acknowledge {
        claim: "w-1".into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign("w-1", "ana");
    w.assess("w-1", "CC-0002");
    let admit = |monthly: i64| Request::Admit {
        amount_minor: None,
        periodic: Some(ic_claims::command::Periodic {
            monthly_minor: monthly,
            first_period: d(2027, 3, 1),
            end: d(2029, 3, 1),
            expected_end: d(2028, 3, 1),
        }),
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    assert_eq!(w.undecided("w-1", "ana", admit(3_000), "R-OK"), "invalid_schedule");
    w.decided("w-1", "ana", admit(0), "R-OK");
    let c = w.claim("w-1");
    assert_eq!((c.status, c.waiver_from), (Status::InPayment, Some(d(2027, 3, 1))));
    assert!(!c.timers.contains_key(&TimerKind::Payment), "nothing is paid, so no payment is timed");
    w.ok(Command::DrainDue);
    assert_eq!(waivers(&w), vec![(d(2027, 3, 1), d(2029, 3, 1))]);
    // No payee, and still nothing for a run to pay or to complain about.
    let today = w.core.today;
    assert_eq!(w.refused(prepare("run-1", today)), "nothing_to_pay");
    assert!(w.claim("w-1").tasks.iter().all(|t| t.kind != TaskKind::PaymentFailure));

    let ends = w.core.today + chrono::Days::new(2);
    let end = Request::EndBenefit {
        end: ends,
        reason: EndReason::Recovered,
    };
    w.decided("w-1", "ana", end, "R-OK");
    w.advance(1);
    w.ok(Command::DrainDue);
    assert_eq!(w.claim("w-1").status, Status::InPayment);
    w.advance(1);
    w.ok(Command::DrainDue);
    let c = w.claim("w-1");
    assert_eq!((c.status, c.closed_on), (Status::Ended, Some(ends)));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Close { .. })));
    let asked = waivers(&w).len();
    w.advance(3);
    w.ok(Command::DrainDue);
    assert_eq!(waivers(&w).len(), asked, "an ended waiver asks for nothing more");
}

/// Purpose: where interests are recorded on the policy, a lump sum goes to those parties first,
/// in priority order and each up to its extent, before the claimant can be paid anything of
/// theirs; a payment to an interested party that the bank returns is owed to that party again.
/// Why it matters: a lender who took an assignment of the policy has a legal right to the
/// proceeds ahead of the owner. If the claims workflow could pay the owner first, the insurer
/// would have to pay the lender a second time out of its own pocket.
#[test]
fn recorded_interests_are_paid_before_the_claimant() {
    use ic_claims::port::{CoverFacts, Interest};
    let mut w = World::new();
    let interest = |party: &str, extent: Option<i64>, priority: u32| Interest {
        insolvency: false,
        party: party.into(),
        extent_minor: extent,
        priority,
    };
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            // Recorded out of order: the priority decides, not the order on file.
            interests: vec![interest("second-lender", Some(30_000), 2), interest("bank", Some(150_000), 1)],
            ..facts("trauma")
        },
    );
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    let due: Vec<_> = w.claim("c-1").interests.iter().map(|i| (i.party.as_str(), i.owed_minor)).collect();
    assert_eq!(due, vec![("bank", 150_000), ("second-lender", 30_000)]);
    assert_eq!(w.core.noticed("interest_to_be_paid").len(), 2);
    let to = |who: &str, payment: &str, party: &str| Command::PayInterest {
        claim: "c-1".into(),
        payment_ref: payment.into(),
        party: party.into(),
        actor: actor(who),
    };

    // The claimant's share is 20,000 and not a cent of the lenders' money.
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 20_001, None)), "interest_first");
    assert_eq!(w.refused(to("ben", "i-1", "bank")), "not_owner");
    assert_eq!(w.refused(to("ana", "i-1", "nobody")), "no_interest");
    assert_eq!(w.refused(to("ana", "i-1", "second-lender")), "interest_priority");
    w.ok(to("ana", "i-1", "bank"));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay { amount_minor: 150_000, payee, close: false, .. }) if payee == "bank"
    ));
    let notice = w.core.noticed("claim_paid")[0].clone();
    // The lender's account is the payments module's to hold, not the claimant's.
    assert_eq!(
        (notice["payee"].as_str(), notice["account_ref"].as_str(), notice["net_minor"].as_str()),
        ("bank", "", "150000")
    );
    assert_eq!(w.refused(to("ana", "i-2", "bank")), "no_interest");
    assert_eq!(w.refused(to("ana", "i-1", "second-lender")), "payment_exists");

    // The bank sends the lender's money back: it is the lender's again, not the claimant's.
    w.ok(Command::PaymentReturned {
        claim: "c-1".into(),
        payment_ref: "i-1".into(),
    });
    assert_eq!(w.claim("c-1").interests_unpaid_minor(), 180_000);
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 170_000, None)), "interest_first");
    w.ok(to("ana", "i-2", "bank"));
    w.ok(pay("c-1", "ana", "p-1", 20_000, None));
    assert_eq!(w.claim("c-1").status, Status::Admitted, "the second lender is still to be paid");
    w.ok(to("ana", "i-3", "second-lender"));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            amount_minor: 30_000,
            close: true,
            ..
        })
    ));
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.payable_minor, c.paid_minor(false), c.interests_unpaid_minor()),
        (Status::Paid, 0, 200_000, 0)
    );
}

/// Purpose: an interest recorded for everything takes the whole of the proceeds, leaving later
/// interests and the claimant nothing, and a payment above the assessor's authority is refused.
/// Why it matters: a full assignment is the common case for a mortgage. The claimant being
/// paid nothing has to be the outcome the system reaches by itself, and a large payment to a
/// lender needs the same authority as the same payment to anyone else.
#[test]
fn a_whole_interest_takes_all_of_the_proceeds() {
    use ic_claims::port::{CoverFacts, Interest};
    let mut w = World::new();
    let whole = |party: &str, priority: u32| Interest {
        insolvency: false,
        party: party.into(),
        extent_minor: None,
        priority,
    };
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            interests: vec![whole("bank", 1), whole("later", 2)],
            ..facts("trauma")
        },
    );
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    let c = w.claim("c-1");
    assert_eq!((c.interests.len(), c.interests[0].owed_minor), (1, 200_000));
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 1, None)), "interest_first");
    let mut book = authority();
    book.version = 2;
    book.profiles.get_mut("assessor").expect("in the book").max_payment_minor = 100_000;
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    let to_bank = Command::PayInterest {
        claim: "c-1".into(),
        payment_ref: "i-1".into(),
        party: "bank".into(),
        actor: actor("ana"),
    };
    assert_eq!(w.refused(to_bank.clone()), "authority");
    let mut book = authority();
    book.version = 3;
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    w.core.refuse_next = 1;
    assert_eq!(w.refused(to_bank.clone()), "ledger");
    w.ok(to_bank);
    assert_eq!(w.claim("c-1").status, Status::Paid);
}

/// Purpose: where the rules ask for proof of life, the check falls due on its interval; once
/// its grace has passed with no proof on file the benefit is held, not cancelled or ended; and
/// when the proof is filed the next run pays every held period in full.
/// Why it matters: a benefit paid for life is paid after death unless someone checks, and the
/// money is then hard to get back from an estate. Holding instead of stopping means a living
/// person who was slow to answer loses nothing, which is what the design promises them.
#[test]
fn an_overdue_proof_of_life_holds_the_benefit_until_it_is_filed() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.proof_of_life_months = 3;
    r.proof_of_life_grace_days = 10;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));

    w.on(d(2027, 6, 1));
    w.ok(Command::DrainDue);
    let task = "review/ProofOfLife/2027-06-01";
    assert!(w.claim("ip-1").tasks.iter().any(|t| t.id == task && t.kind == TaskKind::ProofOfLife));
    // Inside the grace the benefit is paid as usual.
    w.pay_run("run-2", d(2027, 6, 1));
    assert_eq!(w.state.runs["run-2"].count, 3);

    w.on(d(2027, 6, 11));
    w.ok(Command::DrainDue);
    let c = w.claim("ip-1");
    assert!(c.held_for_proof_of_life());
    assert_eq!(c.status, Status::InPayment, "held, not ended");
    assert!(w.core.noticed("review_overdue").iter().any(|n| n["review"] == "ProofOfLife"));
    w.on(d(2027, 8, 1));
    assert_eq!(w.refused(prepare("run-3", d(2027, 8, 1))), "nothing_to_pay");

    let file = |vault: Option<&str>| Command::CompleteTask {
        claim: "ip-1".into(),
        task: task.into(),
        outcome: "signed_declaration".into(),
        attendees: vec![],
        vault_ref: vault.map(str::to_string),
        spend_minor: 0,
        actor: actor("ana"),
    };
    // Saying so is not proof: the certificate, declaration or register match is on file.
    assert_eq!(w.refused(file(None)), "not_authorised");
    w.ok(file(Some("vault-declaration-1")));
    assert!(!w.claim("ip-1").held_for_proof_of_life());
    w.pay_run("run-3", d(2027, 8, 1));
    assert_eq!(w.state.runs["run-3"].count, 2, "July and August, both in full");
    // The next check is already on the plan.
    assert_eq!(
        w.claim("ip-1").schedule.as_ref().expect("in payment").reviews[&TaskKind::ProofOfLife],
        d(2027, 9, 1)
    );
}

fn valuation(version: u32, part: u32, reserves: &[(&str, i64)]) -> ic_claims::config::Valuation {
    ic_claims::config::Valuation {
        version,
        part,
        as_at: d(2027, 3, 31),
        basis: "dlr-2027".into(),
        reserves: reserves.iter().map(|(c, r)| (c.to_string(), *r)).collect(),
        prepared_by: "actuary-1".into(),
        approved_by: vec!["actuary-2".into(), "cfo".into()],
    }
}

/// Purpose: the appointed actuary's reserves for claims in payment are applied claim by claim
/// in parts, each approved by two people other than its preparer; a part sent again is a no-op,
/// the same part with other figures or an older valuation is refused, a claim held elsewhere is
/// left for its own partition, and a claim not in payment keeps its case reserve and is named.
/// Why it matters: the design makes the reserve for a claim in payment an actuarial figure, not
/// the first estimate made at notification. It drives the balance sheet and the reinsurance
/// recoveries, so it must come only from an approved valuation and never be applied twice.
#[test]
fn the_actuarys_valuation_sets_the_reserve_for_claims_in_payment() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.open_claim("c-1");
    let apply = |v: ic_claims::config::Valuation| Command::ApplyValuation { valuation: Box::new(v) };
    let mut unapproved = valuation(1, 1, &[("ip-1", 90_000)]);
    unapproved.approved_by = vec!["actuary-1".into(), "cfo".into()];
    assert_eq!(w.refused(apply(unapproved)), "dual_approval");
    assert_eq!(w.refused(apply(valuation(1, 1, &[("ip-1", -1)]))), "invalid_valuation");
    assert_eq!(w.refused(apply(valuation(1, 1, &[]))), "invalid_valuation");
    let too_many: Vec<(String, i64)> = (0..=ic_claims::config::MAX_VALUATION_PART).map(|i| (format!("c-{i}"), 1)).collect();
    let refs: Vec<(&str, i64)> = too_many.iter().map(|(c, r)| (c.as_str(), *r)).collect();
    assert_eq!(w.refused(apply(valuation(1, 1, &refs))), "invalid_valuation");
    w.core.refuse_next = 1;
    assert_eq!(w.refused(apply(valuation(1, 1, &[("ip-1", 90_000)]))), "ledger");

    w.ok(apply(valuation(1, 1, &[("ip-1", 90_000), ("c-1", 5), ("elsewhere", 7)])));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::SetReserve { amount_minor: 90_000, reason, .. }) if reason == "valuation:1:dlr-2027"
    ));
    assert_eq!(w.claim("ip-1").reserve_minor, 90_000);
    assert!(w.claim("c-1").reserve_minor != 5, "a claim not in payment keeps its case reserve");
    let applied = w.core.noticed("valuation_applied")[0].clone();
    assert_eq!((applied["applied"].as_str(), applied["not_in_payment"].as_str()), ("1", "c-1"));
    assert_eq!(w.core.noticed("claim_reserve_changed").last().expect("published")["valuation"], "1");
    // No senior review for the actuary's own figure, however far it moved.
    assert!(!w.claim("ip-1").tasks.iter().any(|t| t.id.starts_with("senior-review/reserve")));

    let sent = w.core.instructions.len();
    w.ok(apply(valuation(1, 1, &[("ip-1", 90_000), ("c-1", 5), ("elsewhere", 7)])));
    assert_eq!(w.core.instructions.len(), sent, "a part sent again changes nothing");
    assert_eq!(w.refused(apply(valuation(1, 1, &[("ip-1", 80_000)]))), "config_conflict");
    w.ok(apply(valuation(2, 1, &[("ip-1", 85_000)])));
    assert_eq!(w.claim("ip-1").reserve_minor, 85_000);
    assert_eq!(w.refused(apply(valuation(1, 2, &[("ip-1", 1)]))), "config_conflict");
}

/// Purpose: a death recorded in error and then withdrawn restores a benefit it had not yet
/// finished ending, sends a claim it closed to a senior, and only then lets that claim be
/// reopened on the basis that the death was retracted, carrying on from the last period paid;
/// a death that never ended a claim, or a different date, changes nothing.
/// Why it matters: death register feeds and people make mistakes. A living claimant whose
/// income stopped because of a wrong record must get it back with the months they missed, and
/// that must not be a way to reopen any closed claim without a reason on file.
#[test]
fn a_retracted_death_gives_the_benefit_back() {
    let died = |on: chrono::NaiveDate| Command::RecordDeath {
        party: "party-life".into(),
        date_of_death: on,
    };
    let undo = |on: chrono::NaiveDate| Command::RetractDeath {
        party: "party-life".into(),
        date_of_death: on,
    };
    // Not yet past the death date in payment: the benefit simply runs on.
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 15));
    w.ok(died(d(2027, 4, 10)));
    let s = w.claim("ip-1").schedule.clone().expect("in payment");
    assert_eq!((s.end, s.ended), (d(2027, 4, 11), Some(EndReason::Death)));
    w.ok(undo(d(2027, 3, 1)));
    assert_eq!(
        w.claim("ip-1").schedule.as_ref().expect("in payment").ended,
        Some(EndReason::Death),
        "another date is another death"
    );
    w.ok(undo(d(2027, 4, 10)));
    let c = w.claim("ip-1");
    let s = c.schedule.as_ref().expect("in payment");
    assert_eq!((c.status, s.end, s.ended), (Status::InPayment, d(2029, 3, 1), None));
    assert_eq!(w.core.noticed("death_retracted")[0]["benefit"], "restored");

    // A death that closed the claim: a senior reopens it, and the missed months are paid.
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 20));
    let reopen = |who: &str| Command::Reopen {
        claim: "ip-1".into(),
        basis: ReopenBasis::DeathRetracted,
        reserve_minor: 20_000,
        actor: actor(who),
    };
    w.ok(died(d(2027, 3, 31)));
    assert_eq!(w.claim("ip-1").status, Status::Ended);
    assert_eq!(w.refused(reopen("sam")), "death_not_retracted");
    w.ok(undo(d(2027, 3, 31)));
    let c = w.claim("ip-1");
    assert!(c.tasks.iter().any(|t| t.id == "senior-review/death-retracted"));
    assert_eq!(w.core.noticed("death_retracted")[0]["benefit"], "to_review");
    w.ok(reopen("sam"));
    let c = w.claim("ip-1");
    let s = c.schedule.as_ref().expect("in payment");
    assert_eq!(
        (c.status, s.ended, s.end, s.next_period),
        (Status::InPayment, None, d(2029, 3, 1), d(2027, 4, 1))
    );
    w.pay_run("run-2", d(2027, 4, 1));
    assert_eq!(w.state.runs["run-2"].count, 1, "April, missed while the claim was closed");
}

/// Purpose: when an income declaration is late, the benefit is still paid on the last confirmed
/// inputs, a task goes to a senior once the grace passes, and the declaration when it comes is
/// applied to the periods already paid: what was overpaid is held as an overpayment and the next
/// benefit is paid on the declared income.
/// Why it matters: persona test CLM-018. Stopping an income benefit because a form is late leaves
/// someone off work with nothing to live on; carrying on without adjusting would pay the wrong
/// benefit for as long as the declaration stays missing.
#[test]
fn a_late_income_declaration_does_not_stop_the_benefit() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    for (run, on) in [("run-3", d(2027, 3, 1)), ("run-4", d(2027, 4, 1)), ("run-5", d(2027, 5, 1))] {
        w.on(on);
        w.pay_run(run, on);
    }
    // The declaration due on 1 June does not arrive.
    w.on(d(2027, 6, 1));
    w.ok(Command::DrainDue);
    w.on(d(2027, 6, 15));
    w.ok(Command::DrainDue);
    let c = w.claim("ip-1");
    assert!(c.tasks.iter().any(|t| t.id == "senior-review/overdue/IncomeDeclaration/2027-06-15"));
    assert_eq!(c.status, Status::InPayment);
    w.pay_run("run-6", d(2027, 6, 15));
    assert!(
        matches!(w.core.instructions.last(), Some(Instruction::PayBenefit { gross_minor: 3_000, .. })),
        "June is paid in full on the last confirmed inputs"
    );

    // It arrives late: 500 a month of other income since May.
    w.on(d(2027, 6, 20));
    w.ok(income("ip-1", d(2027, 5, 1), 500));
    let c = w.claim("ip-1");
    let raised: i64 = c.overpayments.iter().map(|o| o.amount_minor).sum();
    assert_eq!(raised, 1_000, "May and June were each overpaid by the income");
    assert!(c.overpayments.iter().all(|o| o.status == RecoveryStatus::Held));
    w.on(d(2027, 7, 1));
    w.pay_run("run-7", d(2027, 7, 1));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::PayBenefit {
            gross_minor: 2_500,
            offset: None,
            ..
        })
    ));
}

/// Purpose: a death benefit goes to the nomination the record gives as in force on the date of
/// death, in its shares and after any lender, with anything not nominated left for the estate;
/// someone nominated before (an ex-partner replaced three months earlier) has nothing to be paid,
/// and a nomination whose shares are wrong stops the admission.
/// Why it matters: persona test CLM-029. Paying the nomination that stood when the claim was made,
/// or one replaced before the death, pays the wrong people with money the insurer then owes the
/// right ones; reading it from the record by the date of death is what makes the answer certain.
#[test]
fn a_death_benefit_goes_to_the_nomination_in_force_on_the_date_of_death() {
    use ic_claims::port::{CoverFacts, Interest, Nominee};
    let nominee = |party: &str, share_pct: u32| Nominee {
        party: party.into(),
        share_pct,
    };
    let death = |w: &mut World, id: &str, nominations: Vec<Nominee>| {
        w.core.cover(
            "pol-1",
            "life",
            CoverFacts {
                interests: vec![Interest {
                    insolvency: false,
                    party: "bank".into(),
                    extent_minor: Some(100_000),
                    priority: 1,
                }],
                nominations,
                ..facts("life")
            },
        );
        w.ok(Command::Notify(Box::new(World::notification(id, "pol-1", "life", ic_claims::ClaimType::Death))));
        w.assign(id, "ana");
        w.assess(id, "CC-0001");
    };
    let mut w = World::new();
    // As the record knows it now, the children replaced the ex-partner before the death.
    death(&mut w, "c-1", vec![nominee("child-1", 50), nominee("child-2", 30)]);
    w.decided("c-1", "ana", World::admit_lump(Some(300_001)), "R-OK");
    w.approved("c-1", "sam");
    let due: Vec<_> = w.claim("c-1").interests.iter().map(|i| (i.party.as_str(), i.owed_minor, i.nominated)).collect();
    assert_eq!(due, vec![("bank", 100_000, false), ("child-1", 100_000, true), ("child-2", 60_000, true)]);
    let bases: Vec<_> = w.core.noticed("interest_to_be_paid").iter().map(|n| n["basis"].clone()).collect();
    assert_eq!(bases, ["interest", "nomination", "nomination"]);
    let to = |payment: &str, party: &str| Command::PayInterest {
        claim: "c-1".into(),
        payment_ref: payment.into(),
        party: party.into(),
        actor: actor("ana"),
    };
    assert_eq!(w.refused(to("n-1", "ex-partner")), "no_interest", "a replaced nomination pays nothing");
    assert_eq!(w.refused(to("n-1", "child-1")), "interest_priority", "the lender first");
    w.ok(to("i-1", "bank"));
    w.ok(to("n-1", "child-1"));
    w.ok(to("n-2", "child-2"));
    // The 20% nobody was nominated for, 40,001, is the estate's.
    assert_eq!(w.claim("c-1").interests_unpaid_minor(), 0);
    assert_eq!(w.claim("c-1").benefit_payable_minor(), 40_001);

    // Whole shares: the cent the split leaves goes to the last nominee.
    let mut w = World::new();
    death(&mut w, "c-2", vec![nominee("child-1", 50), nominee("child-2", 50)]);
    w.decided("c-2", "ana", World::admit_lump(Some(300_001)), "R-OK");
    w.approved("c-2", "sam");
    let owed: Vec<i64> = w.claim("c-2").interests.iter().map(|i| i.owed_minor).collect();
    assert_eq!(owed, vec![100_000, 100_000, 100_001]);

    // Shares over 100% are a record to correct, not a payment to guess at.
    let mut w = World::new();
    death(&mut w, "c-3", vec![nominee("child-1", 60), nominee("child-2", 60)]);
    let refused = w.decide("c-3", "ana", World::admit_lump(Some(300_000)), "R-OK").expect_err("refused");
    assert_eq!(refused.code, "invalid_nomination");
}

/// Purpose: a benefit reduced by other income, such as accident compensation, shows on the
/// claimant's statement line by line: the benefit, the other income taken off it, any arrears and
/// any overpayment offset, and what was paid, each as its own figure.
/// Why it matters: persona test CLM-017. Someone receiving compensation and a reduced benefit
/// needs to see why the amount is lower; a single net figure looks like an underpayment and turns
/// into a complaint.
#[test]
fn other_income_shows_as_its_own_figure_on_each_benefit_line() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.ok(income("ip-1", d(2027, 3, 1), 1_200));
    w.pay_run("run-3", d(2027, 3, 1));
    w.on(d(2027, 4, 1));
    w.pay_run("run-4", d(2027, 4, 1));
    let lines = w.state.claimant_view("ip-1").expect("a view").benefit_lines;
    let figures: Vec<_> = lines.iter().map(|l| (l.start, l.benefit_minor, l.other_income_minor, l.paid_minor)).collect();
    assert_eq!(figures, vec![(d(2027, 3, 1), 3_000, 1_200, 1_800), (d(2027, 4, 1), 3_000, 1_200, 1_800)]);
    assert!(lines.iter().all(|l| l.arrears_minor == 0 && l.overpayment_offset_minor == 0 && !l.returned));
    assert!(w.state.claimant_view("c-none").is_none());
}

/// Purpose: late payment interest is worked out and offered to the payee with its amount; the
/// payee can decline it only after being told and with their own decision on file; a declined
/// amount is posted and reversed in the same payment with reason `declined_by_payee`, counted in
/// the report; and a payee who changes their mind before the claim closes is paid what they
/// declined with the next payment.
/// Why it matters: persona test CLM-037 and decision 2 of 13 November. The interest is the
/// insurer's debt for its own delay; it may be refused only by the payee's informed choice, and
/// the refusal must stay visible so the insurer can never quietly benefit from it.
#[test]
fn late_interest_is_offered_and_declined_only_by_an_informed_payee() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.late_interest = Some(ic_claims::config::LateInterest {
        annual_rate_bps: 730,
        free_days: 10,
    });
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    w.advance(20);
    let decline = |vault: &str| Command::DeclineLateInterest {
        claim: "c-1".into(),
        vault_ref: vault.into(),
        actor: actor("ana"),
    };
    let offer = Command::OfferLateInterest {
        claim: "c-1".into(),
        actor: actor("ana"),
    };
    // Never declined before the payee has been told the amount.
    assert_eq!(w.refused(decline("vault-1")), "interest_not_offered");
    w.ok(offer.clone());
    assert_eq!(w.core.noticed("late_interest_offered")[0]["amount_minor"], "400");
    assert_eq!(w.refused(decline(" ")), "decision_required");
    w.ok(decline("vault-1"));
    assert_eq!(w.core.noticed("late_interest_declined")[0]["reason"], "declined_by_payee");

    w.ok(pay("c-1", "ana", "p-1", 100_000, None));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            interest_minor: 200,
            interest_declined_minor: 200,
            ..
        })
    ));
    let paid = w.core.noticed("claim_paid")[0].clone();
    assert_eq!((paid["net_minor"].as_str(), paid["interest_minor"].as_str()), ("100000", "0"));
    assert_eq!(w.state.report().interest_declined_minor, 200);

    // The payee changes their mind: the 200 goes with the next payment, with its own interest.
    w.ok(Command::WithdrawInterestDecline {
        claim: "c-1".into(),
        actor: actor("ana"),
    });
    assert_eq!(w.core.noticed("late_interest_decline_withdrawn")[0]["owed_again_minor"], "200");
    w.ok(pay("c-1", "ana", "p-2", 100_000, None));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            interest_minor: 400,
            interest_declined_minor: 0,
            close: true,
            ..
        })
    ));
    assert_eq!(w.state.report().interest_declined_minor, 0);
    // Closed: there is nothing left to change a mind about.
    assert_eq!(w.refused(offer), "no_interest_owed");
    assert_eq!(
        w.refused(Command::WithdrawInterestDecline {
            claim: "c-1".into(),
            actor: actor("ana"),
        }),
        "no_decline"
    );
}

/// Purpose: an income benefit is suspended for the days the life is in custody after sentence,
/// by a decision with its own reason code that needs the evidence and a recorded conversation;
/// days already paid become an overpayment held as a fact; the claim stays in payment with
/// nothing paid for those days; the review plan waits; and on release the benefit resumes the
/// next day with no new waiting period. A product whose wording pays in custody refuses it.
/// Why it matters: persona test CLM-040 and decision 5 of 13 November. The benefit replaces
/// income lost to disability, not to imprisonment, but the person must leave custody still
/// insured and still on claim, never declined.
#[test]
fn a_benefit_is_suspended_in_custody_and_resumes_on_release() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    let review = w
        .claim("ip-1")
        .schedule
        .as_ref()
        .expect("in payment")
        .reviews
        .values()
        .min()
        .copied()
        .expect("a review");
    w.on(d(2027, 5, 3));
    let suspend = |evidence: &str| Request::SuspendBenefit {
        from: d(2027, 4, 16),
        evidence: evidence.into(),
    };
    assert_eq!(w.undecided("ip-1", "ana", suspend("vault-sentence"), "R-OK"), "unknown_reason");
    assert_eq!(w.undecided("ip-1", "ana", suspend("vault-sentence"), "R-CUSTODY"), "conversation_required");
    w.conversation("ip-1", "talk-custody");
    assert_eq!(w.undecided("ip-1", "ana", suspend(""), "R-CUSTODY"), "evidence_required");
    w.decided("ip-1", "ana", suspend("vault-sentence"), "R-CUSTODY");
    let c = w.claim("ip-1");
    assert_eq!(c.status, Status::InPayment, "never a decline");
    // April 16 to 30 was paid and is owed back: 15 of April's 30 days.
    assert_eq!((c.overpayments[0].amount_minor, c.overpayments[0].status), (1_500, RecoveryStatus::Held));
    assert!(!c.due.iter().any(|(_, d)| matches!(d, ic_claims::model::Due::Review(_))), "the reviews wait");
    assert_eq!(w.core.noticed("benefit_suspended")[0]["from"], "2027-04-16");
    assert_eq!(
        w.undecided("ip-1", "ana", suspend("vault-sentence"), "R-CUSTODY"),
        "invalid_suspension",
        "already suspended"
    );
    w.open_claim("c-9");
    assert_eq!(w.undecided("c-9", "ana", suspend("vault-sentence"), "R-CUSTODY"), "not_in_payment");

    // May: the claim is in the run and nothing is paid for it.
    w.pay_run("run-3", d(2027, 5, 1));
    let s = w.claim("ip-1").schedule.clone().expect("in payment");
    assert_eq!((s.periods[&d(2027, 5, 1)].gross_minor, s.next_period), (0, d(2027, 6, 1)));
    let resume = |from| Request::ResumeBenefit { from };
    assert_eq!(w.undecided("ip-1", "ana", resume(d(2027, 4, 10)), "R-CUSTODY"), "not_suspended");

    // Released on 10 June: payable again from the 11th, the reviews 56 days later than planned.
    w.on(d(2027, 6, 20));
    w.decided("ip-1", "ana", resume(d(2027, 6, 11)), "R-CUSTODY");
    let s = w.claim("ip-1").schedule.clone().expect("in payment");
    assert_eq!(s.reviews.values().min().copied(), review.checked_add_days(chrono::Days::new(56)));
    w.on(d(2027, 7, 1));
    w.pay_run("run-4", d(2027, 6, 1));
    // June: 20 of 30 days, 2,000.
    assert_eq!(
        w.claim("ip-1").schedule.as_ref().expect("in payment").periods[&d(2027, 6, 1)].gross_minor,
        2_000
    );

    // A product that pays in custody: no suspension.
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        ic_claims::port::CoverFacts {
            sum_insured_minor: 4_000,
            benefit_while_imprisoned: ic_claims::port::Imprisonment::Pay,
            ..facts("income_protection")
        },
    );
    w.benefit_in_payment("ip-1");
    w.conversation("ip-1", "talk-custody");
    assert_eq!(w.undecided("ip-1", "ana", suspend("vault-sentence"), "R-CUSTODY"), "paid_in_custody");
}

/// Purpose: a claim admitted while the Official Assignee's insolvency interest stands on the
/// policy pays any lender first as usual, but holds what the owner or the estate would get:
/// neither a payment nor a benefit run releases it until legal records a direction naming who
/// to pay, and then only to that party.
/// Why it matters: decision 4 of 13 November (JNT-029). Whether a bankrupt's policy money belongs
/// to the Official Assignee is a legal question decided case by case; paying the owner first
/// could leave the insurer paying twice.
#[test]
fn proceeds_are_held_under_an_insolvency_interest_until_legal_directs() {
    use ic_claims::port::{CoverFacts, Interest};
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            interests: vec![
                Interest {
                    party: "official-assignee".into(),
                    extent_minor: None,
                    priority: 2,
                    insolvency: true,
                },
                Interest {
                    party: "bank".into(),
                    extent_minor: Some(50_000),
                    priority: 1,
                    insolvency: false,
                },
            ],
            ..facts("trauma")
        },
    );
    admitted(&mut w, "c-1", 200_000);
    w.verify_payee("c-1", "ana");
    let c = w.claim("c-1");
    let due: Vec<_> = c.interests.iter().map(|i| (i.party.as_str(), i.owed_minor)).collect();
    assert_eq!(due, vec![("bank", 50_000)], "the Official Assignee is not paid out as a lender");
    assert!(c.insolvency_hold);
    assert_eq!(w.core.noticed("proceeds_held").len(), 1);
    w.ok(Command::PayInterest {
        claim: "c-1".into(),
        payment_ref: "i-1".into(),
        party: "bank".into(),
        actor: actor("ana"),
    });
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 150_000, None)), "held_for_direction");

    let direct = |who: &str, party: &str| Command::DirectProceeds {
        claim: "c-1".into(),
        party: party.into(),
        direction: "vault-direction-1".into(),
        actor: actor(who),
    };
    assert_eq!(w.refused(direct("ana", "party-claimant")), "legal_only");
    // Directed to someone other than the verified payee: still held.
    w.ok(direct("lex", "official-assignee"));
    assert_eq!(w.refused(pay("c-1", "ana", "p-1", 150_000, None)), "held_for_direction");
    w.ok(direct("lex", "party-claimant"));
    w.ok(pay("c-1", "ana", "p-1", 150_000, None));
    assert_eq!(w.claim("c-1").status, Status::Paid);

    // A benefit in payment is left out of runs until the direction, then paid as usual.
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        CoverFacts {
            sum_insured_minor: 4_000,
            interests: vec![Interest {
                party: "official-assignee".into(),
                extent_minor: None,
                priority: 1,
                insolvency: true,
            }],
            ..facts("income_protection")
        },
    );
    w.benefit_in_payment("ip-1");
    let run = |id: &str| Command::PrepareRun {
        run: id.into(),
        kind: RunKind::Benefit,
        benefit_date: d(2027, 3, 1),
        actor: actor("ana"),
    };
    assert_eq!(w.refused(run("run-1")), "nothing_to_pay", "nothing is paid while held");
    w.ok(Command::DirectProceeds {
        claim: "ip-1".into(),
        party: "party-claimant".into(),
        direction: "vault-direction-2".into(),
        actor: actor("lex"),
    });
    w.pay_run("run-2", d(2027, 3, 1));
    assert_eq!(w.state.runs["run-2"].count, 1);
}

/// Purpose: the reserve, admission and closing notices carry the cause category, the top
/// segment of the first tag of the recorded cause, and leave it out while no cause is recorded.
/// Why it matters: finance reports claims experience by cause from these notices; a guessed or
/// blank category would put the cost in the wrong line of the report.
#[test]
fn reserve_admission_and_closing_name_the_cause_category() {
    let mut w = World::new();
    admitted(&mut w, "c-1", 200_000);
    let reserved = w.core.noticed("claim_reserved");
    assert!(!reserved.last().expect("reserved").contains_key("cause_category"));
    assert_eq!(w.core.noticed("claim_admitted").last().expect("admitted")["cause_category"], "cancer");
    w.open_claim("c-2");
    w.assess("c-2", "CC-0002");
    w.conversation("c-2", "talk-2");
    w.decided("c-2", "ana", Request::Decline, "R-NO");
    w.approved("c-2", "ben");
    let closed = w.core.noticed("claim_closed");
    let closed = closed.last().expect("closed");
    assert_eq!((closed["outcome"].as_str(), closed["cause_category"].as_str()), ("declined", "mental"));
}

/// Purpose: part of a death benefit can be advanced before the claim is decided, up to the
/// rules' limit, and paid at once to the verified payee without closing the claim; the
/// admission that follows counts the advance as part of the benefit, admits only the rest and
/// cannot admit less than was advanced.
/// Why it matters: persona test CLM-004. A family arranging a funeral in the first week cannot
/// wait six weeks for the decision; an advance not counted against the final amount would pay
/// the estate twice, and one that closed the claim would leave the rest of the benefit unpaid.
#[test]
fn a_funeral_advance_is_paid_early_and_counted_against_the_benefit() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.funeral_advance_max_minor = 15_000;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w.ok(Command::Notify(Box::new(World::notification("c-1", "pol-1", "life", ClaimType::Death))));
    w.ok(Command::Acknowledge {
        claim: "c-1".into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign("c-1", "ana");
    let advance = |amount_minor| Request::FuneralAdvance { amount_minor };
    assert_eq!(w.undecided("c-1", "ana", advance(20_000), "R-OK"), "advance_limit");
    w.open_claim("t-1");
    assert_eq!(w.undecided("t-1", "ana", advance(1_000), "R-OK"), "not_open");

    w.decided("c-1", "ana", advance(10_000), "R-OK");
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.advanced_minor, c.admitted_minor, c.payable_minor),
        (Status::Open, 10_000, 10_000, 10_000)
    );
    assert!(w.core.instructions.iter().any(|i| matches!(
        i,
        Instruction::Admit {
            amount_minor: 10_000,
            release_reserve: false,
            terminate: None,
            life: None,
            ..
        }
    )));
    assert_eq!(w.core.noticed("funeral_advance_admitted")[0]["amount_minor"], "10000");
    w.verify_payee("c-1", "ana");
    w.ok(pay("c-1", "ana", "adv-1", 10_000, None));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Pay { close: false, .. })));
    assert_eq!(w.claim("c-1").status, Status::Open, "the claim is still to be decided");
    assert_eq!(w.undecided("c-1", "ana", advance(6_000), "R-OK"), "advance_limit");

    w.assess("c-1", "CC-0001");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(10_000)), "R-OK"), "below_advance");
    w.decided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK");
    let c = w.claim("c-1");
    assert_eq!((c.status, c.admitted_minor, c.payable_minor), (Status::Admitted, 200_000, 190_000));
    assert!(matches!(
        w.core.instructions.iter().rev().find(|i| matches!(i, Instruction::Admit { .. })),
        Some(Instruction::Admit {
            amount_minor: 190_000,
            release_reserve: true,
            ..
        })
    ));
    let admitted = w.core.noticed("claim_admitted");
    let admitted = admitted.last().expect("admitted");
    assert_eq!((admitted["admitted_minor"].as_str(), admitted["advanced_minor"].as_str()), ("200000", "10000"));
}

/// Purpose: a liveness check in the app or an identity check in the customer master meets every
/// open proof of life on the person's benefits with its evidence, releasing a held benefit
/// without a claims person; nothing open means nothing changes, and a confirmation must name
/// its source and evidence.
/// Why it matters: proof of life is asked of every person paid a benefit, most of whom can show
/// it in the app in seconds. A person filing each one by hand delays the payment of someone who
/// did what was asked.
#[test]
fn a_proof_of_life_is_met_by_the_app_or_the_customer_master() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.proof_of_life_months = 3;
    r.proof_of_life_grace_days = 10;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w.benefit_in_payment("ip-1");
    let alive = |evidence: &str| Command::ConfirmAlive {
        party: "party-life".into(),
        source: "app_liveness".into(),
        evidence_ref: evidence.into(),
    };
    // Nothing is open yet.
    w.ok(alive("vault-live-0"));
    assert!(w.core.noticed("proof_of_life_confirmed").is_empty());
    w.on(d(2027, 6, 1));
    w.ok(Command::DrainDue);
    w.on(d(2027, 6, 11));
    w.ok(Command::DrainDue);
    assert!(w.claim("ip-1").held_for_proof_of_life());
    assert_eq!(w.refused(alive("")), "invalid_confirmation");
    w.ok(alive("vault-live-1"));
    let c = w.claim("ip-1");
    assert!(!c.held_for_proof_of_life());
    let t = c.tasks.iter().find(|t| t.kind == TaskKind::ProofOfLife).expect("opened");
    assert_eq!(
        (t.outcome.as_deref(), t.vault_ref.as_deref(), t.completed_by.as_deref()),
        (Some("confirmed:app_liveness"), Some("vault-live-1"), None)
    );
    assert_eq!(c.schedule.as_ref().expect("in payment").reviews_overdue, 0);
    assert_eq!(w.core.noticed("proof_of_life_confirmed")[0]["source"], "app_liveness");
}

/// Purpose: where the product says the cover has no premium waiver, a waiver is refused beside a
/// benefit and a waiver-only claim cannot be admitted; where the product says nothing, the
/// owner's reading of the wording stands.
/// Why it matters: a waiver paid on cover without it is claims money spent on premium the
/// customer owed, found only at audit. The product record is the authority when it speaks.
#[test]
fn a_waiver_is_refused_where_the_product_has_none() {
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        CoverFacts {
            sum_insured_minor: 4_000,
            premium_waiver: Some(false),
            ..facts("income_protection")
        },
    );
    w.benefit_in_payment("ip-1");
    let start = Command::StartWaiver {
        claim: "ip-1".into(),
        from: d(2027, 3, 1),
        actor: actor("ana"),
    };
    assert_eq!(w.refused(start), "no_waiver_cover");

    w.ok(Command::Notify(Box::new(World::notification("w-1", "pol-2", "ip", ClaimType::PremiumWaiver))));
    w.assign("w-1", "ana");
    w.assess("w-1", "CC-0002");
    let admit = Request::Admit {
        amount_minor: None,
        periodic: Some(ic_claims::command::Periodic {
            monthly_minor: 0,
            first_period: d(2027, 3, 1),
            end: d(2029, 3, 1),
            expected_end: d(2028, 3, 1),
        }),
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    assert_eq!(w.undecided("w-1", "ana", admit, "R-OK"), "no_waiver_cover");
}

/// Purpose: where the rules keep back resident withholding tax on late payment interest, each
/// payment withholds it at the payee's rate (or the no-declaration rate when none was given),
/// the bank is told the net, and a payment that comes back is sent again for exactly what came
/// back with nothing withheld twice.
/// Why it matters: interest paid to a person is taxable income that the payer must withhold and
/// pay to the revenue authority. Withholding twice on a reissue, or at the wrong rate, takes the
/// claimant's money or leaves the insurer liable for the tax.
#[test]
fn tax_is_withheld_from_late_interest_at_the_payees_rate() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.late_interest = Some(ic_claims::config::LateInterest {
        annual_rate_bps: 730,
        free_days: 10,
    });
    r.interest_withholding = true;
    r.no_declaration_rate_bp = 4_500;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    admitted(&mut w, "c-1", 200_000);
    let rate = |rate_bp| Command::SetWithholdingRate {
        claim: "c-1".into(),
        rate_bp,
        actor: actor("ana"),
    };
    assert_eq!(w.refused(rate(Some(1_050))), "payee_not_verified");
    w.verify_payee("c-1", "ana");
    assert_eq!(w.refused(rate(Some(10_001))), "invalid_rate");
    w.advance(20);

    // No rate given: the no-declaration rate.
    w.ok(pay("c-1", "ana", "p-1", 100_000, None));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            interest_minor: 200,
            interest_withheld_minor: 90,
            ..
        })
    ));
    let paid = w.core.noticed("claim_paid")[0].clone();
    assert_eq!((paid["net_minor"].as_str(), paid["interest_minor"].as_str()), ("100110", "110"));

    w.ok(rate(Some(1_050)));
    w.ok(pay("c-1", "ana", "p-2", 100_000, None));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            interest_withheld_minor: 21,
            ..
        })
    ));
    // The bank sends it back: sent again for what came back, with nothing more withheld.
    w.ok(Command::PaymentReturned {
        claim: "c-1".into(),
        payment_ref: "p-2".into(),
    });
    w.ok(pay("c-1", "ana", "p-3", 100_179, Some("p-2")));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay {
            interest_minor: 0,
            interest_withheld_minor: 0,
            ..
        })
    ));
    assert_eq!(w.state.report().interest_withheld_minor, 111);
}

/// Purpose: where the rules keep back tax from income benefits, each period in a run carries the
/// tax, the core is told it with the period, the bank is sent the benefit less the tax, a
/// returned period is reissued for exactly what came back, and the report totals the tax;
/// with no rate set the benefit is paid gross.
/// Why it matters: persona test FIN-014. Tax kept back has to reach its own account and the tax
/// reconciliation; a reissue that added the tax back would pay the payee money owed to Inland
/// Revenue.
#[test]
fn tax_is_kept_back_from_an_income_benefit_where_the_rules_set_a_rate() {
    let mut w = World::new();
    w.benefit_in_payment("ip-0");
    w.pay_run("run-0", d(2027, 3, 1));
    assert!(matches!(
        w.core.instructions.iter().rev().find(|i| matches!(i, Instruction::PayBenefit { .. })),
        Some(Instruction::PayBenefit { tax_withheld_minor: 0, .. })
    ));

    let mut r = rules();
    r.version = 2;
    r.benefit_withholding_bp = 1_750;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w.on(d(2027, 4, 1));
    w.pay_run("run-1", d(2027, 4, 1));
    assert!(matches!(
        w.core.instructions.iter().rev().find(|i| matches!(i, Instruction::PayBenefit { .. })),
        Some(Instruction::PayBenefit {
            gross_minor: 3_000,
            tax_withheld_minor: 525,
            ..
        })
    ));
    let paid = w.core.noticed("claim_paid");
    assert_eq!(paid.last().expect("paid")["net_minor"], "2475");
    assert_eq!(w.state.runs["run-1"].submitted_minor, 2_475);
    assert_eq!(w.state.report().benefit_tax_withheld_minor, 525);
    let payment = "run-1/ip-0/2027-04-01";
    w.ok(Command::PaymentReturned {
        claim: "ip-0".into(),
        payment_ref: payment.into(),
    });
    assert_eq!(w.state.report().benefit_tax_withheld_minor, 0, "a returned period kept nothing back");
    assert_eq!(w.refused(pay("ip-0", "ana", "again-1", 3_000, Some(payment))), "reissue_amount");
    w.ok(pay("ip-0", "ana", "again-1", 2_475, Some(payment)));
}

/// Purpose: a living benefit for a policyholder who has a guardian is paid to the payee the
/// owner verified, the policyholder on their own account, and the payment names that party and
/// account and no other.
/// Why it matters: persona test VUL-012. Michael's money is his; payments pays whoever claims
/// releases, so the claim must name him and his account, and a guardian can be paid only as a
/// payee the owner checked and recorded in their own right.
#[test]
fn a_benefit_is_paid_to_the_payee_verified_and_their_own_account() {
    let mut w = World::new();
    admitted(&mut w, "t-1", 10_000);
    w.ok(Command::VerifyPayee {
        claim: "t-1".into(),
        party: "party-holder".into(),
        kind: PayeeKind::Policyholder,
        account_ref: "acct-michael".into(),
        name_check_ref: "name-michael".into(),
        screening_ref: "screen-michael".into(),
        authority_doc: None,
        actor: actor("ana"),
    });
    w.ok(pay("t-1", "ana", "p-1", 10_000, None));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Pay { payee, .. }) if payee == "party-holder"
    ));
    let paid = w.core.noticed("claim_paid");
    let paid = paid.last().expect("published");
    assert_eq!((paid["payee"].as_str(), paid["account_ref"].as_str()), ("party-holder", "acct-michael"));
    let payee = w.claim("t-1").payee.clone().expect("verified");
    assert_eq!((payee.name_check_ref.as_str(), payee.verified_by.as_str()), ("name-michael", "ana"));
}
