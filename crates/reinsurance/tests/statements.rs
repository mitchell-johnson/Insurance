//! Reinsurer accounts: the statement and listing for a treaty period, the
//! supplementary listing when a cession changes afterwards, and settlement.

mod common;

use common::*;
use ic_core::PolicyId;
use ic_reinsurance::account::{LineKind, StatementStatus};
use ic_reinsurance::cession::Cession;
use ic_reinsurance::loader::load_treaty;
use ic_reinsurance::{Command, InstructionBody, Notice, PostingKind, TreatyRef};

/// Anna's policy with February billed: north is owed 8.98 less 1.80 commission.
fn billed() -> Bench {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.factors("P1", "L1", d(1990, 3, 10), false);
    b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    b.today = d(2027, 4, 2);
    b
}

fn cut(period_end: chrono::NaiveDate) -> Command {
    Command::CutStatement {
        treaty: retail(),
        reinsurer: north(),
        period_end,
    }
}

fn instruct(number: u32) -> Command {
    Command::InstructSettlement {
        number,
        prepared_by: "kim".into(),
        approved_by: "lee".into(),
    }
}

fn document(applied: &ic_reinsurance::Applied) -> (String, serde_json::Value) {
    let (name, sha256, body) = applied
        .notices
        .iter()
        .find_map(|n| match n {
            Notice::Document { name, sha256, body } => Some((name, sha256, body)),
            _ => None,
        })
        .expect("a document");
    use sha2::Digest;
    assert_eq!(*sha256, hex::encode(sha2::Sha256::digest(body.as_bytes())));
    (name.clone(), serde_json::from_str(body).unwrap())
}

/// Purpose: a statement takes everything on the reinsurer's account valued up to the period
/// end, nets it, and produces one document with the listing; the listing carries the policy
/// reference, dates, sums and only the facts the treaty allows, and nothing that identifies
/// the person.
/// Why it matters: the statement is what the reinsurer is paid on and the listing is personal
/// data leaving the insurer. Both the arithmetic and the privacy boundary have to hold, and
/// the release has to be logged.
#[test]
fn a_statement_nets_the_account_and_lists_only_what_the_treaty_allows() {
    let mut b = billed();
    let applied = b.ok("month-end", cut(d(2027, 3, 31)));
    let s = b.ri.statement(1).unwrap().clone();
    assert_eq!((s.period_start, s.period_end, s.cut_on), (d(2027, 1, 1), d(2027, 3, 31), d(2027, 4, 2)));
    assert_eq!(s.net_minor, 8_98 - 1_80);
    assert_eq!(s.totals.get(&LineKind::CededPremium), Some(&8_98));
    assert_eq!(s.by_partition.get(&1), Some(&7_18));
    assert_eq!((s.prior_period_minor, s.listing_rows, s.status.clone()), (0, 1, StatementStatus::Issued));
    assert_eq!(s.cut.get(&1), Some(&2));
    let (name, body) = document(&applied);
    assert_eq!(name, "reinsurance/retail-life-surplus/reinsurer-north/statement-1.json");
    assert_eq!(name, s.document);
    assert_eq!(body["payment_reference"], "ri-stmt-1", "the reference the reinsurer quotes when it pays");
    assert_eq!(body["net_owed_to_reinsurer_minor"], 7_18);
    let row = &body["listing"][0];
    assert_eq!(
        (row["policy"].as_str(), row["ceded_minor"].as_i64(), row["sum_at_risk_minor"].as_i64()),
        (Some("N-P1"), Some(180 * K), Some(800 * K))
    );
    assert_eq!(
        (row["age"].as_i64(), row["sex"].as_str(), row["smoker"].as_bool(), row["premium_minor"].as_i64()),
        (Some(37), Some("female"), Some(false), Some(8_98))
    );
    let text = body.to_string();
    assert!(
        !text.contains("anna") && !text.contains("L1") && !text.contains("1990"),
        "the listing identifies the life: {text}"
    );
    assert!(applied
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ReleaseLogged { purpose, documents, .. } if purpose == "treaty statement and listing" && documents == std::slice::from_ref(&name))));
    // The account is now the statement; nothing is left pending.
    assert_eq!(b.ri.balance(&retail(), &north()), 7_18);
    assert!(b.ri.statement(0).is_none() && b.ri.statement(2).is_none());
    // South's account was not touched.
    assert_eq!(b.ri.balance(&retail(), &south()), 5_98 - 1_20);
    let exposure = b.ri.exposure(d(2027, 4, 2));
    assert_eq!((exposure[&north()].balance_minor, exposure[&north()].ceded_sum_at_risk_minor), (7_18, 180 * K));
    assert_eq!(exposure[&south()].balance_minor, 4_78);
    b.assert_reconciles();
}

/// Purpose: the settlement path: agreement where the treaty requires it, maker-checker, a hold
/// that blocks payment, one sweep per partition and one net payment, and a bank confirmation
/// for the wrong amount kept as an open item.
/// Why it matters: this is money leaving the insurer. Each control here is one the design
/// names: agreed before payment, prepared by one person and approved by another, held while a
/// listing is in question, and never forced to match.
#[test]
fn settlement_is_agreed_checked_and_paid_once() {
    let mut b = billed();
    b.ok("month-end", cut(d(2027, 3, 31)));
    assert_eq!(b.refused("finance", instruct(1)), "not_agreed");
    assert_eq!(
        b.refused(
            "finance",
            Command::AgreeStatement {
                number: 1,
                reference: String::new()
            }
        ),
        "reference_required"
    );
    b.ok(
        "reinsurance-ops",
        Command::AgreeStatement {
            number: 1,
            reference: "RE-1".into(),
        },
    );
    b.ok(
        "reinsurance-ops",
        Command::AgreeStatement {
            number: 1,
            reference: "RE-1".into(),
        },
    );
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::AgreeStatement {
                number: 1,
                reference: "RE-2".into()
            }
        ),
        "statement_not_open"
    );
    assert_eq!(
        b.refused(
            "finance",
            Command::InstructSettlement {
                number: 1,
                prepared_by: "kim".into(),
                approved_by: "kim".into()
            }
        ),
        "maker_checker"
    );
    assert_eq!(
        b.refused(
            "finance",
            Command::InstructSettlement {
                number: 1,
                prepared_by: String::new(),
                approved_by: "kim".into()
            }
        ),
        "maker_checker"
    );
    assert_eq!(
        b.refused(
            "finance",
            Command::HoldSettlement {
                number: 1,
                reason: String::new()
            }
        ),
        "reason_required"
    );
    b.ok(
        "reinsurance-ops",
        Command::HoldSettlement {
            number: 1,
            reason: "listing under review".into(),
        },
    );
    assert_eq!(b.refused("finance", instruct(1)), "settlement_held");
    b.ok("reinsurance-ops", Command::ReleaseHold { number: 1 });
    assert_eq!(
        b.refused(
            "finance",
            Command::RecordSettlement {
                number: 1,
                payment_ref: "BANK-1".into(),
                amount_minor: 7_18,
                bank_account: None
            }
        ),
        "not_instructed"
    );
    let applied = b.ok("finance", instruct(1));
    assert!(
        matches!(&applied.instructions[..], [i] if i.partition == 1 && matches!(&i.body, InstructionBody::Sweep { statement: 1, cut_seq: 2, amount_minor: 7_18, .. }))
    );
    assert!(matches!(
        &applied.notices[..],
        [Notice::SettlementDue {
            statement: 1,
            net_minor: 7_18,
            attempt: 0,
            currency: ic_money::Currency::NZD,
            ..
        }]
    ));
    assert_eq!(b.refused("finance", instruct(1)), "statement_not_open");
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::HoldSettlement {
                number: 1,
                reason: "late".into()
            }
        ),
        "statement_not_open"
    );
    // The bank paid a different amount: recorded, not forced.
    b.ok(
        "finance",
        Command::RecordSettlement {
            number: 1,
            payment_ref: "BANK-1".into(),
            amount_minor: 7_00,
            bank_account: None,
        },
    );
    assert!(b.ri.exceptions().contains_key("settlement_mismatch:1"));
    assert_eq!(b.ri.statement(1).unwrap().status, StatementStatus::Instructed);
    let paid = b.ok(
        "finance",
        Command::RecordSettlement {
            number: 1,
            payment_ref: "BANK-2".into(),
            amount_minor: 7_18,
            bank_account: Some("nz-main".into()),
        },
    );
    // Settlement clearing is cleared against the bank account the money left.
    let cleared: i64 = paid
        .instructions
        .iter()
        .map(|i| match &i.body {
            InstructionBody::ClearSettlement {
                bank_account, amount_minor, ..
            } if bank_account == "nz-main" => *amount_minor,
            other => panic!("{other:?}"),
        })
        .sum();
    assert_eq!(cleared, 7_18);
    assert_eq!(b.ri.statement(1).unwrap().cleared.as_deref(), Some("nz-main"));
    assert_eq!(b.ri.statement(1).unwrap().status, StatementStatus::Settled { payment_ref: "BANK-2".into() });
    assert!(b.ri.exceptions().is_empty());
    assert_eq!(b.ri.balance(&retail(), &north()), 0);
    assert_eq!(
        b.refused(
            "finance",
            Command::RecordSettlement {
                number: 1,
                payment_ref: "BANK-3".into(),
                amount_minor: 7_18,
                bank_account: None
            }
        ),
        "not_instructed"
    );
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::AgreeStatement {
                number: 1,
                reference: "RE-1".into()
            }
        ),
        "statement_not_open"
    );
    b.assert_reconciles();
}

/// Purpose: the payments module records that the bank settled a statement, and when the bank
/// later sends that payment back the statement is owed again with an exception for a person;
/// a second settlement closes the exception. A return of a payment that did not settle the
/// statement, or of a statement not yet sent for payment, is refused.
/// Why it matters: a returned payment that left the statement "settled" would show the
/// reinsurer as paid when it holds nothing, and the balance and the month-end close would both
/// be wrong by the whole settlement until the reinsurer chased it.
#[test]
fn a_settlement_the_bank_returns_is_owed_again() {
    let mut g = Bench::new();
    g.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    g.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 100_00)]);
    g.today = d(2027, 3, 2);
    g.ok(
        "month-end",
        Command::CutStatement {
            treaty: group(),
            reinsurer: north(),
            period_end: d(2027, 2, 28),
        },
    );
    let returned = |payment_ref: &str| Command::SettlementReturned {
        number: 1,
        payment_ref: payment_ref.into(),
        reason: "account closed".into(),
    };
    let settled = |payment_ref: &str| Command::RecordSettlement {
        number: 1,
        payment_ref: payment_ref.into(),
        amount_minor: 25_50,
        bank_account: Some("nz-main".into()),
    };
    assert_eq!(g.refused("payments", returned("ri-stmt-1")), "not_settled");
    g.ok("finance", instruct(1));
    assert_eq!(g.refused("payments", returned("")), "not_settled");
    assert_eq!(g.refused("claims", settled("ri-stmt-1")), "caller_not_allowed");
    assert_eq!(g.refused("claims", returned("ri-stmt-1")), "caller_not_allowed");
    g.ok("payments", settled("ri-stmt-1"));
    assert_eq!(g.ri.balance(&group(), &north()), 0);
    assert_eq!(g.refused("payments", returned("some-other-payment")), "not_settled");

    let applied = g.ok("payments", returned("ri-stmt-1"));
    assert!(matches!(&applied.notices[..], [Notice::ExceptionOpened { id, code, detail }]
        if id == "settlement_returned:1" && code == "settlement_returned" && detail.contains("account closed")));
    // The money is back in clearing until it is paid again.
    assert!(matches!(&applied.instructions[..], [i] if matches!(&i.body, InstructionBody::ClearSettlement { amount_minor: -25_50, .. })));
    assert_eq!(g.ri.statement(1).unwrap().cleared, None);
    assert_eq!(g.ri.statement(1).unwrap().status, StatementStatus::Instructed);
    assert_eq!(g.ri.balance(&group(), &north()), 25_50, "the reinsurer is owed again");
    // Told again while still owed (the bank file read twice): one exception.
    let again = g.ok("payments", returned("ri-stmt-1"));
    assert!(again.notices.is_empty());

    let applied = g.ok("payments", settled("ri-stmt-1b"));
    assert!(matches!(&applied.notices[..], [Notice::ExceptionClosed { id }] if id == "settlement_returned:1"));
    assert_eq!(
        g.ri.statement(1).unwrap().status,
        StatementStatus::Settled {
            payment_ref: "ri-stmt-1b".into()
        }
    );
    assert!(g.ri.exceptions().is_empty());
}

/// Purpose: a person offers a returned settlement to payments again, as a new offer numbered
/// after the first, which closes the return's exception; it is refused when no return is
/// waiting, when the reinsurer owes the money, without a filed reference, or from anyone but
/// operations, and a second return can be offered a third time.
/// Why it matters: a returned payment is still owed to the reinsurer. Payments never reuses a
/// payment reference, so offering the same reference again would be refused and the reinsurer
/// would go unpaid; and paying again without a person's decision could pay twice to the bank
/// details that bounced.
#[test]
fn a_returned_settlement_is_offered_again_as_a_new_payment() {
    let mut g = Bench::new();
    g.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    g.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 100_00)]);
    g.today = d(2027, 3, 2);
    g.ok(
        "month-end",
        Command::CutStatement {
            treaty: group(),
            reinsurer: north(),
            period_end: d(2027, 2, 28),
        },
    );
    let reoffer = |reference: &str| Command::ReofferSettlement {
        number: 1,
        reference: reference.into(),
    };
    let returned = |payment_ref: &str| Command::SettlementReturned {
        number: 1,
        payment_ref: payment_ref.into(),
        reason: "account closed".into(),
    };
    assert_eq!(g.refused("reinsurance-ops", reoffer("OPS-1")), "not_returned");
    g.ok("finance", instruct(1));
    assert_eq!(g.refused("reinsurance-ops", reoffer("OPS-1")), "not_returned");
    g.ok("payments", returned("ri-stmt-1"));
    assert_eq!(g.refused("reinsurance-ops", reoffer(" ")), "reference_required");
    assert_eq!(g.refused("payments", reoffer("OPS-1")), "caller_not_allowed");
    let applied = g.ok("reinsurance-ops", reoffer("OPS-1"));
    assert!(
        matches!(&applied.notices[..], [Notice::SettlementDue { attempt: 1, net_minor: 25_50, .. }, Notice::ExceptionClosed { id }] if id == "settlement_returned:1")
    );
    let command = ic_reinsurance::driver::payments_command(&applied.notices[0]).unwrap();
    assert_eq!(command["command"]["waiting"]["payment"], "ri-stmt-1-2");
    assert_eq!(command["command"]["waiting"]["source"], "reinsurance:statement:1");
    assert_eq!(command["key"], "reinsurance:settlement:1:2");
    assert_eq!(g.refused("reinsurance-ops", reoffer("OPS-1")), "not_returned", "one decision, one offer");

    g.ok("payments", returned("ri-stmt-1-2"));
    let applied = g.ok("reinsurance-ops", reoffer("OPS-2"));
    assert!(matches!(&applied.notices[0], Notice::SettlementDue { attempt: 2, .. }));
    assert_eq!(
        g.ri.statement(1).unwrap().reoffers,
        vec![(d(2027, 3, 2), "OPS-1".to_string()), (d(2027, 3, 2), "OPS-2".to_string())]
    );
    g.ok(
        "payments",
        Command::RecordSettlement {
            number: 1,
            payment_ref: "ri-stmt-1-3".into(),
            amount_minor: 25_50,
            bank_account: Some("nz-main".into()),
        },
    );
    assert!(g.ri.exceptions().is_empty());
    assert_eq!(without_keys(&g.replayed()), without_keys(&g.ri));

    // Money owed by the reinsurer is collected, never paid.
    let mut owed = Bench::new();
    owed.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    owed.today = d(2027, 6, 10);
    owed.ok(
        "claims",
        Command::Claim(Box::new(ic_reinsurance::claims::ClaimFact::Reserved {
            claim: ic_reinsurance::ClaimId::from("C1"),
            policy: ic_core::PolicyId::from("P1"),
            cover: "life".into(),
            life: ic_core::LifeId::from("L1"),
            event_date: d(2027, 6, 1),
            gross_reserve_minor: 800 * K,
            periodic: false,
            catastrophe_event: None,
        })),
    );
    owed.ok(
        "claims",
        Command::Claim(Box::new(ic_reinsurance::claims::ClaimFact::Paid {
            claim: ic_reinsurance::ClaimId::from("C1"),
            payment_ref: "PAY-1".into(),
            amount_minor: 800 * K,
            period_end: None,
            ex_gratia: false,
        })),
    );
    owed.today = d(2027, 7, 2);
    owed.ok(
        "month-end",
        Command::CutStatement {
            treaty: retail(),
            reinsurer: north(),
            period_end: d(2027, 6, 30),
        },
    );
    assert!(owed.ri.statement(1).unwrap().net_minor < 0);
    owed.ok(
        "reinsurance-ops",
        Command::AgreeStatement {
            number: 1,
            reference: "RE-1".into(),
        },
    );
    owed.ok("finance", instruct(1));
    owed.ok("payments", returned("ri-stmt-1"));
    assert_eq!(owed.refused("reinsurance-ops", reoffer("OPS-1")), "nothing_to_pay");
}

/// Purpose: statements that cannot be cut, and operations on statements that do not exist, are
/// refused with their own codes.
/// Why it matters: month-end runs unattended. A second statement for a period already cut, or
/// one for a period still open, would put the same money in front of a reinsurer twice.
#[test]
fn statements_that_cannot_be_cut_are_refused() {
    let mut b = billed();
    assert_eq!(b.refused("month-end", cut(d(2027, 4, 2))), "period_open");
    assert_eq!(
        b.refused(
            "month-end",
            Command::CutStatement {
                treaty: "nope".into(),
                reinsurer: north(),
                period_end: d(2027, 3, 31)
            }
        ),
        "unknown_treaty"
    );
    b.ok("month-end", cut(d(2027, 3, 31)));
    assert_eq!(b.refused("month-end", cut(d(2027, 3, 31))), "statement_exists");
    assert_eq!(b.refused("month-end", cut(d(2026, 12, 31))), "statement_exists");
    for command in [
        Command::AgreeStatement {
            number: 9,
            reference: "r".into(),
        },
        Command::HoldSettlement { number: 9, reason: "r".into() },
        Command::ReleaseHold { number: 0 },
        instruct(9),
        Command::RecordSettlement {
            number: 9,
            payment_ref: "p".into(),
            amount_minor: 1,
            bank_account: None,
        },
        Command::IssueSupplementaryListing { number: 9, reason: "r".into() },
    ] {
        assert_eq!(b.refused("finance", command), "unknown_statement");
    }
    assert_eq!(
        b.refused("finance", Command::IssueSupplementaryListing { number: 1, reason: "r".into() }),
        "nothing_to_issue"
    );
    // A treaty that does not need agreement pays on the issued statement.
    let mut g = Bench::new();
    g.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    g.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 100_00)]);
    g.today = d(2027, 3, 2);
    g.ok(
        "month-end",
        Command::CutStatement {
            treaty: group(),
            reinsurer: north(),
            period_end: d(2027, 2, 28),
        },
    );
    assert_eq!(g.ri.statement(1).unwrap().period_start, d(2027, 2, 1));
    let applied = g.ok("finance", instruct(1));
    assert!(matches!(&applied.notices[..], [Notice::SettlementDue { net_minor: 25_50, .. }]));
}

/// Purpose: a cession that changes for a period whose listing has already been sent flags the
/// statement, blocks its settlement until a supplementary listing is issued, and the
/// supplementary listing shows each changed row as it was sent and as it is now; the money
/// correction arrives on the next statement as a prior-period amount.
/// Why it matters: addendum decision 1 and runbook 5: "a supplementary reinsurer listing
/// corrects the period". Paying on a listing known to be wrong, or correcting it silently,
/// are the two failures this prevents.
#[test]
fn a_change_after_the_listing_was_sent_needs_a_supplementary_listing() {
    let mut b = billed();
    b.ok("month-end", cut(d(2027, 3, 31)));
    b.ok(
        "reinsurance-ops",
        Command::AgreeStatement {
            number: 1,
            reference: "RE-1".into(),
        },
    );
    // An increase backdated to 15 February arrives in April.
    let applied = b.add_layer(1, "P1", layer("life", "life", 2, "L1", 100 * K, d(2027, 2, 15)));
    assert_eq!(
        applied
            .notices
            .iter()
            .filter(|n| matches!(n, Notice::SupplementaryListingDue { statement: 1 }))
            .count(),
        1
    );
    assert!(b.ri.statement(1).unwrap().supplementary_due);
    assert_eq!(b.refused("finance", instruct(1)), "supplementary_due");
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::IssueSupplementaryListing {
                number: 1,
                reason: String::new()
            }
        ),
        "nothing_to_issue"
    );
    let applied = b.ok(
        "reinsurance-ops",
        Command::IssueSupplementaryListing {
            number: 1,
            reason: "backdated increase".into(),
        },
    );
    let (name, body) = document(&applied);
    assert_eq!(name, "reinsurance/retail-life-surplus/reinsurer-north/statement-1-supplementary-1.json");
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0]["layer"].as_i64(), rows[0]["was"].is_null(), rows[0]["now"]["ceded_minor"].as_i64()),
        (Some(2), true, Some(60 * K))
    );
    assert!(applied
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ReleaseLogged { purpose, .. } if purpose == "supplementary listing")));
    let s = b.ri.statement(1).unwrap();
    assert!(!s.supplementary_due && s.supplementary_issued == 1);
    b.ok("finance", instruct(1));
    // The money for the backdated increase is on the next statement, as a correction to
    // the earlier period: 1.50 of premium less 0.30 of commission.
    b.today = d(2027, 7, 2);
    b.ok("month-end", cut(d(2027, 6, 30)));
    let s2 = b.ri.statement(2).unwrap().clone();
    assert_eq!(s2.prior_period_minor, 1_20);
    assert_eq!(s2.totals.get(&LineKind::CededPremium), Some(&1_50));
    // A change reaching both statements flags the one whose rows changed.
    let applied = b.feed(
        1,
        vec![ic_core::event::Event::CoverExpired {
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            on: d(2027, 6, 1),
        }],
    );
    assert_eq!(
        applied
            .notices
            .iter()
            .filter(|n| matches!(n, Notice::SupplementaryListingDue { .. }))
            .collect::<Vec<_>>(),
        vec![&Notice::SupplementaryListingDue { statement: 2 }]
    );
    let applied = b.ok(
        "reinsurance-ops",
        Command::IssueSupplementaryListing {
            number: 2,
            reason: "expiry notified late".into(),
        },
    );
    let (_, body) = document(&applied);
    assert_eq!(body["rows"].as_array().unwrap().len(), 2);
    assert!(body["rows"][0]["now"].is_null() && !body["rows"][0]["was"].is_null());
    b.assert_reconciles();
}

/// Purpose: a correction valued in an earlier treaty period is carried on the next statement
/// and shown as a prior-period amount.
/// Why it matters: reinsurers reconcile statements period by period. A correction that
/// appeared as this period's business would not match their own records of either period.
#[test]
fn a_correction_to_an_earlier_period_is_shown_as_prior_period() {
    let mut b = billed();
    b.ok("month-end", cut(d(2027, 3, 31)));
    b.add_layer(1, "P1", layer("life", "life", 2, "L1", 100 * K, d(2027, 2, 15)));
    b.today = d(2027, 7, 2);
    b.premium_due(1, "P1", 2, d(2027, 5, 1), d(2027, 6, 1), &[]);
    b.ok("month-end", cut(d(2027, 6, 30)));
    let s = b.ri.statement(2).unwrap();
    assert_eq!(s.prior_period_minor, 1_50 - 30);
    assert!(s.net_minor > s.prior_period_minor);
    b.assert_reconciles();
}

/// Purpose: profit commission is worked out from the start of the treaty and only the change
/// since the last review is booked; a treaty without the terms refuses the review.
/// Why it matters: profit commission runs over years with losses carried forward. Booking the
/// cumulative figure each time, not the change, would pay it again at every review.
#[test]
fn profit_commission_books_only_the_change_since_the_last_review() {
    let mut b = billed();
    let review = || Command::ReviewProfitCommission {
        treaty: retail(),
        reinsurer: north(),
    };
    // Premium 8.98, commission 1.80, expenses 0.90: profit 6.28, half of it to the insurer.
    let applied = b.ok("finance", review());
    assert!(
        matches!(&applied.instructions[..], [i] if i.partition == 1 && matches!(&i.body, InstructionBody::Post { kind: PostingKind::ProfitCommission, amount_minor: 3_14, .. }))
    );
    assert!(b.ok("finance", review()).instructions.is_empty());
    assert_eq!(b.ri.balance(&retail(), &north()), 8_98 - 1_80 - 3_14);
    // A claim turns the treaty to a loss: the commission is taken back.
    b.ok(
        "claims",
        Command::Claim(Box::new(ic_reinsurance::claims::ClaimFact::Reserved {
            claim: "C1".into(),
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            life: "L1".into(),
            event_date: d(2027, 3, 1),
            gross_reserve_minor: 800 * K,
            periodic: false,
            catastrophe_event: None,
        })),
    );
    b.ok(
        "claims",
        Command::Claim(Box::new(ic_reinsurance::claims::ClaimFact::Paid {
            claim: "C1".into(),
            payment_ref: "PAY".into(),
            amount_minor: 800 * K,
            period_end: None,
            ex_gratia: false,
        })),
    );
    b.ok("finance", review());
    assert_eq!(b.ri.booked(&retail(), &north(), LineKind::ProfitCommission), 0);
    assert_eq!(
        b.refused(
            "finance",
            Command::ReviewProfitCommission {
                treaty: group(),
                reinsurer: north()
            }
        ),
        "no_profit_commission"
    );
    assert_eq!(
        b.refused(
            "finance",
            Command::ReviewProfitCommission {
                treaty: "nope".into(),
                reinsurer: north()
            }
        ),
        "no_profit_commission"
    );
}

/// Purpose: migration loads opening balances and cessions as the legacy system held them; a
/// loaded cession stands unrecalculated until the cover itself changes, and the rebuilt
/// accumulation can be compared with the legacy retention records.
/// Why it matters: at cutover the reinsurer's records are the legacy system's. Recalculating
/// in-force cessions on day one would open a dispute on every cover that differs by a cent.
#[test]
fn migrated_cessions_and_balances_are_taken_as_data() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    let k = key("P1", "life", 1, "L1");
    let legacy = Cession {
        treaty: Some(TreatyRef { id: retail(), version: 1 }),
        sum_at_risk_minor: 800 * K,
        retained_minor: 450 * K,
        ceded: vec![(north(), 350 * K)],
        facultative: vec![],
        unplaced_minor: 0,
        unplaced_reason: None,
    };
    let unbalanced = Cession {
        retained_minor: 1,
        ..legacy.clone()
    };
    assert_eq!(
        b.refused(
            "migration",
            Command::LoadMigratedCession {
                key: k.clone(),
                cession: unbalanced
            }
        ),
        "unbalanced_cession"
    );
    assert_eq!(
        b.refused(
            "migration",
            Command::LoadMigratedCession {
                key: key("P9", "life", 1, "L1"),
                cession: legacy.clone()
            }
        ),
        "unknown_risk"
    );
    b.ok(
        "migration",
        Command::LoadMigratedCession {
            key: k.clone(),
            cession: legacy.clone(),
        },
    );
    assert_eq!(b.ri.cession(&k, d(2027, 2, 1)), Some(&legacy));
    let anna = ic_core::PartyId::from("anna");
    assert_eq!(b.ri.compare_retention(&[(anna.clone(), 450 * K)], d(2027, 2, 1)), vec![]);
    assert_eq!(
        b.ri.compare_retention(&[(anna.clone(), 500 * K)], d(2027, 2, 1)),
        vec![(anna, 500 * K, 450 * K)]
    );
    // Once the cover changes it is calculated like any other.
    b.today = d(2027, 5, 1);
    b.feed(
        1,
        vec![ic_core::event::Event::LayersMerged {
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            removed: vec![],
            into: 1,
            sum_insured_minor: 900 * K,
        }],
    );
    assert_eq!(b.ri.cession(&k, d(2027, 4, 30)), Some(&legacy));
    assert_eq!(b.ri.cession(&k, d(2027, 5, 1)).unwrap().retained_minor, 500 * K);
    // Opening balances go on the account and to the core.
    let applied = b.ok(
        "migration",
        Command::LoadOpeningBalance {
            treaty: retail(),
            reinsurer: north(),
            partition: 4,
            amount_minor: -12_000_00,
            reference: "legacy Q4".into(),
        },
    );
    assert!(
        matches!(&applied.instructions[..], [i] if i.partition == 4 && matches!(&i.body, InstructionBody::Post { kind: PostingKind::OpeningBalance, amount_minor: -12_000_00, policy: None, .. }))
    );
    assert_eq!(b.ri.balance(&retail(), &north()), -12_000_00);
    assert_eq!(
        b.refused(
            "migration",
            Command::LoadOpeningBalance {
                treaty: "nope".into(),
                reinsurer: north(),
                partition: 4,
                amount_minor: 1,
                reference: "x".into()
            }
        ),
        "unknown_treaty"
    );
    b.assert_reconciles();
}

/// Purpose: cover is taken back from a treaty only when the treaty allows it, and moved to a
/// new treaty version only by command; either changes the cession from its effective date and
/// nothing before it.
/// Why it matters: in-force cover stays on the version it was written under. These two
/// commands are the only doors out, and each must leave the past record alone.
#[test]
fn recapture_and_a_negotiated_move_change_cover_from_a_date() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    let k = key("P1", "life", 1, "L1");
    let recapture = |effective, keys| Command::Recapture {
        treaty: retail(),
        effective,
        keys,
    };
    assert_eq!(b.refused("reinsurance-ops", recapture(d(2030, 2, 1), vec![k.clone()])), "recapture_not_allowed");
    assert_eq!(
        b.refused("reinsurance-ops", recapture(d(2037, 2, 1), vec![key("P9", "life", 1, "L1")])),
        "unknown_risk"
    );
    assert_eq!(b.refused("reinsurance-ops", recapture(d(2027, 1, 1), vec![k.clone()])), "not_ceded");
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::Recapture {
                treaty: group(),
                effective: d(2037, 2, 1),
                keys: vec![k.clone()]
            }
        ),
        "not_ceded"
    );
    // Version 2 raises the commission; cover moves to it only when told to.
    let v2 = load_treaty(
        "t.star",
        &RETAIL
            .replace("version = 1", "version = 2")
            .replace("effective_from = \"2027-01-01\"", "effective_from = \"2028-01-01\"")
            .replace("commission = \"0.20\"", "commission = \"0.25\""),
    )
    .unwrap();
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::MoveTreatyVersion {
                to: v2.reference(),
                effective: d(2028, 1, 1),
                keys: vec![k.clone()]
            }
        ),
        "unknown_treaty"
    );
    // What would the new version change? Nothing in the split, so the comparison is empty.
    assert!(b.ri.compare_treaty(&v2, d(2028, 1, 1)).is_empty());
    let narrower = load_treaty("t.star", &RETAIL.replace("version = 1", "version = 3").replace("automatic_limit = dollars(4000000)", "automatic_limit = dollars(200000)").replace(&RETAIL[RETAIL.find("SCENARIOS").unwrap()..], "SCENARIOS = [scenario(\"x\", covers = [covered(dollars(800000), \"2027-02-01\")], expect = [expect(retained = dollars(500000), ceded = dollars(200000), unplaced = dollars(100000))])]\n")).unwrap();
    let changes = b.ri.compare_treaty(&narrower, d(2028, 1, 1));
    assert!(matches!(&changes[..], [(key, Some(now), Some(then))] if *key == k && now.unplaced_minor == 0 && then.unplaced_minor == 100 * K));
    b.ok("config", Command::ActivateTreaty { treaty: Box::new(v2.clone()) });
    assert_eq!(b.ri.cession(&k, d(2028, 6, 1)).unwrap().treaty.as_ref().unwrap().version, 1);
    b.ok(
        "reinsurance-ops",
        Command::MoveTreatyVersion {
            to: v2.reference(),
            effective: d(2028, 1, 1),
            keys: vec![k.clone()],
        },
    );
    assert_eq!(b.ri.cession(&k, d(2027, 12, 31)).unwrap().treaty.as_ref().unwrap().version, 1);
    assert_eq!(b.ri.cession(&k, d(2028, 1, 1)).unwrap().treaty.as_ref().unwrap().version, 2);
    // New business from 2028 is written on version 2 without being told.
    b.today = d(2028, 3, 1);
    b.issue(1, "P2", "bella", vec![layer("life", "life", 1, "L1", 800 * K, d(2028, 3, 1))]);
    assert_eq!(
        b.ri.cession(&key("P2", "life", 1, "L1"), d(2028, 3, 1))
            .unwrap()
            .treaty
            .as_ref()
            .unwrap()
            .version,
        2
    );
    // After ten years the insurer may take the cover back.
    b.ok("reinsurance-ops", recapture(d(2037, 2, 1), vec![k.clone()]));
    let after = b.ri.cession(&k, d(2037, 2, 1)).unwrap();
    assert_eq!((after.retained_minor, after.treaty_ceded(), after.treaty.clone()), (800 * K, 0, None));
    assert_eq!(b.ri.cession(&k, d(2037, 1, 31)).unwrap().treaty_ceded(), 300 * K);
    b.assert_reconciles();
}

/// Purpose: after a statement is cut, a change on another treaty does not flag it, and a
/// change to one of its own rows that is put back before anything is issued leaves a
/// supplementary listing with no rows, which still clears the flag.
/// Why it matters: the flag blocks settlement. Raising it for cover the statement never
/// listed would hold a reinsurer's money for nothing, and listing a row as changed when it
/// is back as it was sent would tell the reinsurer something untrue.
#[test]
fn a_supplementary_listing_shows_only_rows_that_still_differ() {
    let mut b = billed();
    b.ok("month-end", cut(d(2027, 3, 31)));
    // A scheme written from 1 March on the group treaty reaches the period, not the statement.
    b.issue_scheme(2, "S1", d(2027, 3, 1), vec![member_cover("M1", 200 * K)]);
    assert!(!b.ri.statement(1).unwrap().supplementary_due);
    // An expiry from 15 March is reported late, then unwound.
    let expired = b.feed(
        1,
        vec![ic_core::event::Event::CoverExpired {
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            on: d(2027, 3, 15),
        }],
    );
    assert!(expired.notices.contains(&Notice::SupplementaryListingDue { statement: 1 }));
    let unwound = b.feed(
        1,
        vec![ic_core::event::Event::CoverExpiryUnwound {
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            layers: vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))],
            reference: "REM-7".into(),
        }],
    );
    assert!(!unwound.notices.contains(&Notice::SupplementaryListingDue { statement: 1 }), "said once");
    assert!(b.ri.statement(1).unwrap().supplementary_due);
    let applied = b.ok(
        "reinsurance-ops",
        Command::IssueSupplementaryListing {
            number: 1,
            reason: "expiry reported in error".into(),
        },
    );
    let (_, body) = document(&applied);
    assert_eq!(body["rows"], serde_json::json!([]));
    let s = b.ri.statement(1).unwrap();
    assert!(!s.supplementary_due && s.supplementary_issued == 1);
    b.assert_reconciles();
}

/// Purpose: comparing a proposed treaty version looks only at lives with cover on that
/// treaty; a scheme member on another treaty is not in the answer.
/// Why it matters: the comparison is what people read before moving cover to new terms. Rows
/// for cover the treaty does not touch would be noise at best and, recalculated against the
/// wrong terms, a false alarm.
#[test]
fn a_treaty_comparison_looks_only_at_cover_on_that_treaty() {
    let mut b = billed();
    b.issue_scheme(2, "S1", d(2027, 3, 1), vec![member_cover("M1", 200 * K)]);
    let same = load_treaty("t.star", RETAIL).unwrap();
    assert!(b.ri.compare_treaty(&same, d(2027, 4, 2)).is_empty());
    let narrower = load_treaty(
        "t.star",
        &RETAIL
            .replace("version = 1", "version = 3")
            .replace("automatic_limit = dollars(4000000)", "automatic_limit = dollars(200000)")
            .replace(
                &RETAIL[RETAIL.find("SCENARIOS").unwrap()..],
                "SCENARIOS = [scenario(\"x\", covers = [covered(dollars(800000), \"2027-02-01\")], expect = [expect(retained = dollars(500000), ceded = dollars(200000), unplaced = dollars(100000))])]\n",
            ),
    )
    .unwrap();
    let changes = b.ri.compare_treaty(&narrower, d(2027, 4, 2));
    assert!(matches!(&changes[..], [(k, Some(_), Some(then))] if *k == key("P1", "life", 1, "L1") && then.unplaced_minor == 100 * K));
}

/// Purpose: the close check finance asks for is green only when every treaty whose
/// statement period ends on the date has had its statement cut, no cut statement is waiting
/// on a supplementary listing, the core has taken every instruction, and the module
/// reconciles; each thing in the way is named, and a treaty whose period has not ended is
/// not asked for a statement.
/// Why it matters: this answer is the `reinsurance_sweep` step of the financial close. Green
/// with a statement uncut, or with entries the core has not posted, would close a period in
/// which the reinsurer accounts in the ledger do not net to the statements. Red for a yearly
/// treaty in March would block every close for nothing.
#[test]
fn the_close_check_names_what_stands_in_the_way() {
    use ic_reinsurance::host::{answer, Query};
    let check = |b: &Bench, period_end| answer(&b.ri, serde_json::to_value(Query::SweepCheck { period_end }).unwrap()).unwrap();
    let kinds = |v: &serde_json::Value| -> Vec<String> { v["items"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap().to_string()).collect() };
    let delivered = |b: &mut Bench| {
        let through = b.ri.outbox(usize::MAX).last().map_or(0, |(seq, _)| *seq);
        b.ok("driver", Command::AcknowledgeInstructions { through });
    };
    // Anna's February premium, under a treaty that states monthly, quarterly or yearly.
    let with_period = |period: &str| {
        let mut b = Bench::empty();
        let source = RETAIL.replace("statements(\"quarterly\"", &format!("statements(\"{period}\""));
        b.ok(
            "config",
            Command::ActivateTreaty {
                treaty: Box::new(load_treaty("t.star", &source).unwrap()),
            },
        );
        b.ok(
            "config",
            Command::ActivateRateTable {
                table: Box::new(ic_reinsurance::loader::load_rate_table("r.star", RATES).unwrap()),
            },
        );
        b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
        b.factors("P1", "L1", d(1990, 3, 10), false);
        b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
        b.today = d(2028, 1, 5);
        b
    };
    let (feb, mar, dec) = (d(2027, 2, 28), d(2027, 3, 31), d(2027, 12, 31));
    for (period, due) in [
        ("monthly", [true, true, true]),
        ("quarterly", [false, true, true]),
        ("yearly", [false, false, true]),
    ] {
        let mut b = with_period(period);
        assert_eq!(kinds(&check(&b, mar)).last().map(String::as_str), Some("instructions_not_delivered"));
        delivered(&mut b);
        for (end, due) in [feb, mar, dec].into_iter().zip(due) {
            let answer = check(&b, end);
            assert_eq!(answer["green"], !due, "{period} to {end}: {answer}");
            assert_eq!(kinds(&answer), vec!["statement_not_cut"; if due { 2 } else { 0 }], "{period} to {end}");
        }
        // Nothing valued by the end of January, so nothing is owed a statement then.
        assert_eq!(check(&b, d(2027, 1, 31)), serde_json::json!({"green": true, "items": []}));
    }

    let mut b = with_period("quarterly");
    delivered(&mut b);
    let uncut = check(&b, mar);
    assert_eq!(
        uncut["items"][0],
        serde_json::json!({"kind": "statement_not_cut", "treaty": "retail-life-surplus", "reinsurer": "reinsurer-north", "net_minor": 7_18})
    );
    for reinsurer in [north(), south()] {
        b.ok(
            "month-end",
            Command::CutStatement {
                treaty: retail(),
                reinsurer,
                period_end: mar,
            },
        );
    }
    delivered(&mut b);
    assert_eq!(check(&b, mar), serde_json::json!({"green": true, "items": []}));

    // A backdated increase: the listing sent is now wrong, and the core is owed entries.
    b.add_layer(1, "P1", layer("life", "life", 2, "L1", 100 * K, d(2027, 2, 15)));
    let first = b.ri.outbox(1)[0].0;
    b.ok(
        "driver",
        Command::InstructionRefused {
            seq: first,
            code: "period_closed".into(),
            message: "the value date is in a closed period".into(),
        },
    );
    let red = check(&b, mar);
    assert_eq!(red["green"], false);
    assert_eq!(
        kinds(&red),
        [
            "supplementary_listing_due",
            "supplementary_listing_due",
            "instruction_refused",
            "instructions_not_delivered"
        ]
    );
    assert_eq!(red["items"][0]["statement"], 1);
    assert!(red["items"][2]["detail"].as_str().unwrap().contains("period_closed"));
    // The statement was cut after February, so February's close does not see its flag.
    assert!(!kinds(&check(&b, feb)).contains(&"supplementary_listing_due".to_string()));
    delivered(&mut b);
    for number in [1, 2] {
        let reason = "backdated increase".to_string();
        b.ok("reinsurance-ops", Command::IssueSupplementaryListing { number, reason });
    }
    delivered(&mut b);
    assert_eq!(check(&b, mar)["green"], true, "the correction itself waits for the next statement");

    // Damage the module cannot explain is in the way too.
    let mut v = serde_json::to_value(&b.ri).unwrap();
    v["exceptions"]["over_retention:P1/life/1/L1"] = serde_json::json!({"code": "over_retention", "detail": "x", "opened_on": "2027-04-02"});
    let damaged: ic_reinsurance::Reinsurance = serde_json::from_value(v).unwrap();
    let broken = answer(&damaged, serde_json::json!({"sweep_check": {"period_end": "2027-03-31"}})).unwrap();
    assert_eq!(kinds(&broken), ["does_not_reconcile"]);
    assert!(broken["items"][0]["detail"].as_str().unwrap().contains("exception item disagree"));
}

/// Purpose: a settlement recorded by hand without its bank account opens an exception asking
/// for it, and naming it clears settlement clearing once; a malformed bank account is refused
/// before anything changes; and a return of a settlement never cleared has nothing to put back.
/// Why it matters: money that moved some other way is still money at the bank. Left in
/// clearing it would show reinsurance owing cash it has already paid, and finance's clearing
/// account would never reconcile to zero.
#[test]
fn a_settlement_recorded_without_its_bank_account_is_cleared_when_it_is_named() {
    let mut g = Bench::new();
    g.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    g.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 100_00)]);
    g.today = d(2027, 3, 2);
    g.ok(
        "month-end",
        Command::CutStatement {
            treaty: group(),
            reinsurer: north(),
            period_end: d(2027, 2, 28),
        },
    );
    g.ok("finance", instruct(1));
    let clear = |bank: &str| Command::ClearSettlement {
        number: 1,
        bank_account: bank.into(),
    };
    assert_eq!(g.refused("finance", clear("nz-main")), "not_uncleared");
    let by_hand = |bank: Option<&str>| Command::RecordSettlement {
        number: 1,
        payment_ref: "JNL-9".into(),
        amount_minor: 25_50,
        bank_account: bank.map(str::to_string),
    };
    assert_eq!(g.refused("finance", by_hand(Some("not valid"))), "invalid_bank_account");
    let applied = g.ok("finance", by_hand(None));
    assert!(applied.instructions.is_empty());
    assert_eq!(g.ri.exceptions()["settlement_not_cleared:1"].code, "settlement_not_cleared");
    assert_eq!(g.refused("finance", clear("")), "invalid_bank_account");
    assert_eq!(g.refused("claims", clear("nz-main")), "caller_not_allowed");
    let applied = g.ok("finance", clear("nz-main"));
    assert!(matches!(&applied.instructions[..], [i] if matches!(&i.body, InstructionBody::ClearSettlement { amount_minor: 25_50, .. })));
    assert!(g.ri.exceptions().is_empty());
    assert_eq!(g.refused("finance", clear("nz-main")), "not_uncleared", "cleared once");

    // Recorded without a bank account and returned before anyone named it: nothing to put back.
    let returned = Command::SettlementReturned {
        number: 1,
        payment_ref: "JNL-9".into(),
        reason: "entered in error".into(),
    };
    let mut h = Bench::new();
    h.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    h.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 100_00)]);
    h.today = d(2027, 3, 2);
    h.ok(
        "month-end",
        Command::CutStatement {
            treaty: group(),
            reinsurer: north(),
            period_end: d(2027, 2, 28),
        },
    );
    h.ok("finance", instruct(1));
    h.ok("finance", by_hand(None));
    let applied = h.ok("payments", returned);
    assert!(applied.instructions.is_empty());
    assert!(!h.ri.exceptions().contains_key("settlement_not_cleared:1"));
    assert!(h.ri.exceptions().contains_key("settlement_returned:1"));
    assert_eq!(without_keys(&h.replayed()), without_keys(&h.ri));
}

/// Purpose: a listing longer than the treaty's part size is filed as numbered parts, each its
/// own record with its hash, and the statement names every part, its hash and its row count
/// instead of carrying the rows; the release log names every record released; a treaty asking
/// for parts of no rows is refused.
/// Why it matters: each document is held in replicated state until it is filed. One document
/// the size of the whole ceded book would grow with the book until it slowed every node, and a
/// reinsurer must be able to check from the statement alone that no part went missing.
#[test]
fn a_long_listing_is_filed_in_parts_the_statement_names() {
    let mut b = Bench::empty();
    let parted = RETAIL.replace(
        "statements(\"quarterly\", agree_before_payment = True)",
        "statements(\"quarterly\", agree_before_payment = True, listing_part_rows = 1)",
    );
    assert_ne!(parted, RETAIL, "the shipped treaty's statement terms moved");
    for (file, source) in [("retail_life_surplus.star", parted.as_str()), ("group_life_surplus.star", GROUP)] {
        let treaty = Box::new(load_treaty(file, source).unwrap());
        b.ok("config", Command::ActivateTreaty { treaty });
    }
    let table = Box::new(ic_reinsurance::loader::load_rate_table("ri_life_base.star", RATES).unwrap());
    b.ok("config", Command::ActivateRateTable { table });
    let none = RETAIL.replace(
        "statements(\"quarterly\", agree_before_payment = True)",
        "statements(\"quarterly\", agree_before_payment = True, listing_part_rows = 0)",
    );
    assert!(load_treaty("t.star", &none).unwrap_err().to_string().contains("between 1 and 10,000 rows"));

    for (policy, party, dob) in [("P1", "anna", d(1990, 3, 10)), ("P2", "bob", d(1985, 7, 1))] {
        b.issue(1, policy, party, vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
        b.factors(policy, "L1", dob, false);
        b.premium_due(1, policy, 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    }
    b.today = d(2027, 4, 2);
    let applied = b.ok("month-end", cut(d(2027, 3, 31)));
    let documents: Vec<(&String, &String, serde_json::Value)> = applied
        .notices
        .iter()
        .filter_map(|n| match n {
            Notice::Document { name, sha256, body } => Some((name, sha256, serde_json::from_str(body).unwrap())),
            _ => None,
        })
        .collect();
    assert_eq!(documents.len(), 3, "two parts and the statement");
    let statement = &documents[2].2;
    assert!(statement.get("listing").is_none());
    let named = statement["listing_parts"].as_array().unwrap();
    assert_eq!(named.len(), 2);
    for (i, (name, sha256, part)) in documents[..2].iter().enumerate() {
        assert_eq!(
            **name,
            format!("reinsurance/retail-life-surplus/reinsurer-north/statement-1-listing-{}-of-2.json", i + 1)
        );
        assert_eq!(
            (named[i]["name"].as_str(), named[i]["sha256"].as_str(), named[i]["rows"].as_u64()),
            (Some(name.as_str()), Some(sha256.as_str()), Some(1))
        );
        assert_eq!(
            (part["part"].as_u64(), part["of"].as_u64(), part["listing"].as_array().map(Vec::len)),
            (Some(i as u64 + 1), Some(2), Some(1))
        );
    }
    assert_eq!(b.ri.statement(1).unwrap().listing_rows, 2);
    assert!(applied.notices.iter().any(|n| matches!(n, Notice::ReleaseLogged { .. })));
    assert_eq!(without_keys(&b.replayed()), without_keys(&b.ri));
}

/// Purpose: a statement cut for a reinsurer named on the panel with its organisation party
/// asks documents for a covering letter with the totals, the period, the payment reference and
/// the filed record's name and hash, and nothing from the listing; a reinsurer with no party
/// gets no letter.
/// Why it matters: the letter is how a reinsurer learns a statement is waiting and what to pay
/// or expect. It goes by email, so it must carry no policy-level data, and the record's hash
/// lets the reinsurer check the file it collects is the one the letter describes.
#[test]
fn a_statement_asks_documents_for_a_covering_letter_to_the_reinsurers_party() {
    let mut b = Bench::empty();
    let addressed = RETAIL.replace(
        "reinsurer(\"reinsurer-north\", \"0.60\")",
        "reinsurer(\"reinsurer-north\", \"0.60\", party = \"party-north-re\")",
    );
    assert_ne!(addressed, RETAIL, "the shipped panel moved");
    for (file, source) in [("retail_life_surplus.star", addressed.as_str()), ("group_life_surplus.star", GROUP)] {
        let treaty = Box::new(load_treaty(file, source).unwrap());
        b.ok("config", Command::ActivateTreaty { treaty });
    }
    let table = Box::new(ic_reinsurance::loader::load_rate_table("ri_life_base.star", RATES).unwrap());
    b.ok("config", Command::ActivateRateTable { table });
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.factors("P1", "L1", d(1990, 3, 10), false);
    b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    b.today = d(2027, 4, 2);
    let applied = b.ok("month-end", cut(d(2027, 3, 31)));
    let s = b.ri.statement(1).unwrap().clone();
    let letters: Vec<&Notice> = applied.notices.iter().filter(|n| matches!(n, Notice::StatementLetterDue { .. })).collect();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].audience(), ic_reinsurance::machine::Audience::Documents);
    let command = ic_reinsurance::driver::documents_command(letters[0], d(2027, 4, 2)).unwrap();
    let asked = &command["command"]["request_letter"];
    assert_eq!(command["key"], "ri-letter:1");
    assert_eq!(asked["request"], "ri:1");
    assert_eq!(asked["recipient"]["party"]["party"], "party-north-re");
    assert_eq!(asked["detail"]["net_owed_to_reinsurer_minor"], s.net_minor.to_string());
    assert_eq!(asked["detail"]["record"], s.document);
    assert_eq!(asked["detail"]["record_sha256"], s.sha256);
    assert_eq!(asked["detail"]["period"], "2027-01-01 to 2027-03-31");
    assert!(!command.to_string().contains("P1"), "nothing from the listing: {command}");
    assert!(ic_reinsurance::driver::documents_command(&Notice::ExceptionClosed { id: "x".into() }, d(2027, 4, 2)).is_none());

    let south = b.ok(
        "month-end",
        Command::CutStatement {
            treaty: retail(),
            reinsurer: ic_reinsurance::ReinsurerId::from("reinsurer-south"),
            period_end: d(2027, 3, 31),
        },
    );
    assert!(
        !south.notices.iter().any(|n| matches!(n, Notice::StatementLetterDue { .. })),
        "no party, no letter"
    );
}
