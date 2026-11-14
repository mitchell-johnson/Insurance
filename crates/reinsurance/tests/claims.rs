//! Recoveries: the reinsurers' share of a claim, large-claim notification and
//! consent, disputed items after a reversed admission, and aggregate covers.

mod common;

use common::*;
use ic_core::{LifeId, PolicyId};
use ic_reinsurance::account::LineKind;
use ic_reinsurance::claims::{dispute_reference, ClaimFact, ClaimStatus, ConsentStatus};
use ic_reinsurance::loader::{load_aggregate_treaty, load_treaty};
use ic_reinsurance::{ClaimId, Command, Notice, PostingKind, TreatyId};

fn reserved(claim: &str, policy: &str, cover: &str, amount: i64) -> ClaimFact {
    ClaimFact::Reserved {
        claim: ClaimId::from(claim),
        policy: PolicyId::from(policy),
        cover: cover.into(),
        life: LifeId::from("L1"),
        event_date: d(2027, 6, 1),
        gross_reserve_minor: amount,
        periodic: false,
        catastrophe_event: None,
    }
}

fn paid(claim: &str, reference: &str, amount: i64, ex_gratia: bool) -> ClaimFact {
    ClaimFact::Paid {
        claim: ClaimId::from(claim),
        payment_ref: reference.into(),
        amount_minor: amount,
        period_end: None,
        ex_gratia,
    }
}

fn fact(b: &mut Bench, f: ClaimFact) -> ic_reinsurance::Applied {
    b.ok("claims", Command::Claim(Box::new(f)))
}

/// Anna with 800,000 of life cover: 500,000 kept, 180,000 to north, 120,000 to south.
fn anna() -> Bench {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.today = d(2027, 6, 10);
    b
}

fn claim(b: &Bench, id: &str) -> ic_reinsurance::claims::ClaimBook {
    b.ri.claim(&ClaimId::from(id)).unwrap().clone()
}

/// Purpose: the life of an ordinary claim: the reserve posts the expected recovery as
/// recoverable, a change of reserve moves it, payment makes the recovery due from each
/// reinsurer in its share, and a payment delivered twice counts once.
/// Why it matters: recoveries are the money reinsurance exists to bring in. They must follow
/// the cession at the event date exactly, and at-least-once delivery from claims must never
/// bill a reinsurer twice.
#[test]
fn a_claim_recovers_each_reinsurers_share_as_it_is_paid() {
    let mut b = anna();
    fact(&mut b, reserved("C1", "P1", "life", 400 * K));
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 90 * K);
    fact(
        &mut b,
        ClaimFact::ReserveChanged {
            claim: ClaimId::from("C1"),
            gross_reserve_minor: 800 * K,
        },
    );
    assert_eq!(
        (b.posted(PostingKind::Recoverable, &north()), b.posted(PostingKind::Recoverable, &south())),
        (180 * K, 120 * K)
    );
    assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 0);
    assert_eq!(b.ri.open_recoverables().get(&north()), Some(&(180 * K)));
    fact(
        &mut b,
        ClaimFact::Admitted {
            claim: ClaimId::from("C1"),
            admitted_minor: 800 * K,
        },
    );
    let applied = fact(&mut b, paid("C1", "PAY-1", 800 * K, false));
    assert_eq!(
        (b.posted(PostingKind::RecoveryDue, &north()), b.posted(PostingKind::RecoveryDue, &south())),
        (180 * K, 120 * K)
    );
    assert!(applied.instructions.iter().all(|i| i.partition == 1));
    assert_eq!(b.ri.balance(&retail(), &north()), -180 * K);
    assert!(b.ri.open_recoverables().is_empty());
    assert!(fact(&mut b, paid("C1", "PAY-1", 800 * K, false)).instructions.is_empty());
    assert!(fact(
        &mut b,
        ClaimFact::Closed {
            claim: ClaimId::from("C1"),
            outcome: Some("paid".into())
        }
    )
    .instructions
    .is_empty());
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Closed);
    // Reserving the same claim again with the same details only restates the reserve.
    assert!(fact(&mut b, reserved("C1", "P1", "life", 800 * K)).instructions.is_empty());
    assert_eq!(b.ri.consent_status(&ClaimId::from("C1")), Some(ConsentStatus::NotRequired));
    assert_eq!(b.ri.consent_status(&ClaimId::from("zz")), None);
    b.assert_reconciles();
}

/// Purpose: a claim closed for less than its reserve, or declined, takes the expected recovery
/// back down to what was actually paid.
/// Why it matters: recoverable is an asset on the balance sheet. A declined claim that left
/// its recoverable standing would overstate what reinsurers owe.
#[test]
fn closing_a_claim_releases_the_recoverable_that_was_not_paid() {
    let mut b = anna();
    fact(&mut b, reserved("C1", "P1", "life", 800 * K));
    fact(&mut b, paid("C1", "PAY-1", 200 * K, false));
    fact(
        &mut b,
        ClaimFact::Closed {
            claim: ClaimId::from("C1"),
            outcome: None,
        },
    );
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 45 * K);
    assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 45 * K);
    fact(&mut b, reserved("C2", "P1", "life", 800 * K));
    fact(
        &mut b,
        ClaimFact::Closed {
            claim: ClaimId::from("C2"),
            outcome: Some("declined".into()),
        },
    );
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 45 * K);
    b.assert_reconciles();
}

/// Purpose: a large claim is notified to the panel once, and where the treaty gives the
/// reinsurers a say they are asked with a deadline; admitting without their agreement is an
/// open exception until every reinsurer on the panel has agreed.
/// Why it matters: the claims-cooperation clause. A recovery on a large claim admitted without
/// the reinsurer's agreement can be refused, so the gap has to be visible while it can still
/// be closed.
#[test]
fn large_claims_are_notified_and_consent_is_tracked() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 3000 * K, d(2027, 2, 1))]);
    b.today = d(2027, 6, 10);
    let id = ClaimId::from("C1");
    let applied = fact(&mut b, reserved("C1", "P1", "life", 3000 * K));
    assert!(applied.notices.contains(&Notice::LargeClaimNotification {
        claim: id.clone(),
        treaty: retail(),
        reinsurers: vec![north(), south()]
    }));
    assert!(applied.notices.contains(&Notice::ReinsurerConsentRequired {
        claim: id.clone(),
        treaty: retail(),
        due_by: d(2027, 6, 20)
    }));
    assert_eq!(b.ri.consent_status(&id), Some(ConsentStatus::Waiting { due_by: d(2027, 6, 20) }));
    // Neither notice is sent twice.
    let again = fact(
        &mut b,
        ClaimFact::ReserveChanged {
            claim: id.clone(),
            gross_reserve_minor: 2900 * K,
        },
    );
    assert!(!again
        .notices
        .iter()
        .any(|n| matches!(n, Notice::LargeClaimNotification { .. } | Notice::ReinsurerConsentRequired { .. })));
    let consent = |reinsurer, agreed| Command::ReinsurerConsent {
        claim: ClaimId::from("C1"),
        reinsurer,
        agreed,
        reference: "email-77".into(),
    };
    let applied = b.ok("reinsurance-ops", consent(north(), true));
    assert!(matches!(&applied.notices[..], [Notice::ReinsurerConsent { agreed: true, .. }]));
    fact(
        &mut b,
        ClaimFact::Admitted {
            claim: id.clone(),
            admitted_minor: 3000 * K,
        },
    );
    assert!(b.ri.exceptions()["admitted_without_consent:C1"].detail.contains("reinsurer-south"));
    b.ok("reinsurance-ops", consent(south(), false));
    assert_eq!(b.ri.consent_status(&id), Some(ConsentStatus::Refused));
    b.ok("reinsurance-ops", consent(south(), true));
    assert_eq!(b.ri.consent_status(&id), Some(ConsentStatus::Agreed));
    assert!(b.ri.exceptions().is_empty());
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::ReinsurerConsent {
                claim: ClaimId::from("zz"),
                reinsurer: north(),
                agreed: true,
                reference: "r".into()
            }
        ),
        "unknown_claim"
    );
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::ReinsurerConsent {
                claim: id,
                reinsurer: north(),
                agreed: true,
                reference: String::new()
            }
        ),
        "reference_required"
    );
}

/// Purpose: a claim paid by period recovers the ceded proportion of every payment, and an ex
/// gratia payment recovers only under a treaty that says it does.
/// Why it matters: income-style claims run for years and ex gratia payments are the insurer's
/// own choice. Billing a reinsurer for a payment its treaty does not cover starts a dispute.
#[test]
fn periodic_and_ex_gratia_payments_follow_the_treaty() {
    let run = |ex_gratia_recoverable: bool| {
        let mut b = Bench::empty();
        let source = if ex_gratia_recoverable {
            RETAIL.replace("consent_days = 10)", "consent_days = 10, ex_gratia_recoverable = True)")
        } else {
            RETAIL.to_string()
        };
        b.ok(
            "config",
            Command::ActivateTreaty {
                treaty: Box::new(load_treaty("t.star", &source).unwrap()),
            },
        );
        b.issue(1, "P1", "anna", vec![layer("trauma", "trauma", 1, "L1", 600 * K, d(2027, 2, 1))]);
        b.today = d(2027, 6, 10);
        let mut f = reserved("C1", "P1", "trauma", 600 * K);
        if let ClaimFact::Reserved { periodic, .. } = &mut f {
            *periodic = true;
        }
        fact(&mut b, f);
        for month in 1..=3 {
            fact(&mut b, paid("C1", &format!("PAY-{month}"), 600 * K, false));
        }
        // 100,000 of 600,000 is ceded, 60/40: a sixth of every payment.
        assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 180 * K);
        fact(&mut b, paid("C1", "XG-1", 60 * K, true));
        b.assert_reconciles();
        b.posted(PostingKind::RecoveryDue, &north()) - 180 * K
    };
    assert_eq!(run(false), 0);
    assert_eq!(run(true), 6 * K);
}

/// Purpose: money got back against an overpaid claim, repaid by a provider that billed twice or
/// offset from another partition, comes off the loss the reinsurers share; each receipt counts
/// once however often it is delivered, and a treaty that keeps recoveries for the insurer bills
/// the reinsurers on what was paid.
/// Why it matters: a treaty shares the net loss. Billing reinsurers for money the insurer has
/// since got back overstates the recovery due, and the setting lets a reinsurance manager follow
/// a treaty that says otherwise.
#[test]
fn money_got_back_on_an_overpaid_claim_reduces_what_the_reinsurers_pay() {
    let run = |share: bool| {
        let mut b = Bench::empty();
        let source = if share {
            RETAIL.to_string()
        } else {
            RETAIL.replace("consent_days = 10)", "consent_days = 10, share_recoveries = False)")
        };
        b.ok(
            "config",
            Command::ActivateTreaty {
                treaty: Box::new(load_treaty("t.star", &source).unwrap()),
            },
        );
        b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
        b.today = d(2027, 6, 10);
        fact(&mut b, reserved("C1", "P1", "life", 800 * K));
        fact(&mut b, paid("C1", "PAY-1", 800 * K, false));
        // North holds 180,000 of 800,000: 22.5% of the claim.
        assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 180 * K);
        let repaid = [
            ("overpayment", "OP-1"),
            ("amount_minor", "6000000"),
            ("outstanding_minor", "2000000"),
            ("cause", "ProviderBilling"),
        ];
        b.feed(
            1,
            vec![published("overpayment_raised", "C1", &[("overpayment", "OP-1"), ("amount_minor", "8000000")])],
        );
        b.feed(1, vec![published("overpayment_recovered", "C1", &repaid)]);
        b.feed(1, vec![published("overpayment_recovered", "C1", &repaid)]);
        let offset = [
            ("overpayment", "OP-1"),
            ("amount_minor", "2000000"),
            ("outstanding_minor", "0"),
            ("partition", "2"),
        ];
        b.feed(1, vec![published("overpayment_offset_received", "C1", &offset)]);
        b.assert_reconciles();
        (claim(&b, "C1").recovered_minor, b.posted(PostingKind::RecoveryDue, &north()))
    };
    // 80,000 got back in two receipts: 22.5% of 720,000 is 162,000.
    assert_eq!(run(true), (80 * K, 162 * K));
    assert_eq!(run(false), (80 * K, 180 * K));

    // A receipt that cannot be read, or is for nothing, is refused rather than counted.
    let mut b = anna();
    fact(&mut b, reserved("C1", "P1", "life", 800 * K));
    assert_eq!(
        b.refused(
            "claims",
            Command::Claim(Box::new(ClaimFact::Recovered {
                claim: ClaimId::from("C1"),
                reference: "recovered:OP-1:0".into(),
                amount_minor: 0,
            }))
        ),
        "invalid_amount"
    );
}

/// Purpose: under an excess of loss treaty a claim recovers what it costs above the retention,
/// not a proportion of it.
/// Why it matters: the two bases give very different recoveries for a partial claim. Using the
/// proportional rule on an excess of loss treaty would over-claim on every small loss.
#[test]
fn excess_of_loss_recovers_the_cost_above_the_retention() {
    let mut b = Bench::empty();
    let cut = RETAIL.find("SCENARIOS").unwrap();
    let source = format!(
        "{}SCENARIOS = [scenario(\"x\", covers = [covered(dollars(800000), \"2027-02-01\")], expect = [expect(retained = dollars(500000), ceded = dollars(300000))])]\n",
        RETAIL[..cut].replace("surplus(retention = dollars(500000), automatic_limit = dollars(4000000))", "excess_of_loss(dollars(500000), dollars(4000000))")
    );
    b.ok(
        "config",
        Command::ActivateTreaty {
            treaty: Box::new(load_treaty("t.star", &source).unwrap()),
        },
    );
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.today = d(2027, 6, 10);
    fact(&mut b, reserved("C1", "P1", "life", 400 * K));
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 0);
    fact(
        &mut b,
        ClaimFact::ReserveChanged {
            claim: ClaimId::from("C1"),
            gross_reserve_minor: 600 * K,
        },
    );
    assert_eq!(
        (b.posted(PostingKind::Recoverable, &north()), b.posted(PostingKind::Recoverable, &south())),
        (60 * K, 40 * K)
    );
    fact(
        &mut b,
        ClaimFact::ReserveChanged {
            claim: ClaimId::from("C1"),
            gross_reserve_minor: 800 * K,
        },
    );
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 180 * K);
}

/// Purpose: when the cession behind a claim is corrected, including a claim that arrived
/// before its policy's events did, the expected recovery follows the corrected cession.
/// Why it matters: runbook 5 and addendum decision 1 both end with "recoveries follow the
/// corrected cessions". A recovery frozen at the first answer would be claimed on a cession
/// the reinsurer's listing no longer shows.
#[test]
fn a_corrected_cession_corrects_the_recovery() {
    let mut b = Bench::new();
    b.today = d(2027, 6, 10);
    // The claim arrives before the feed has caught up with the policy.
    assert!(fact(&mut b, reserved("C1", "P1", "life", 800 * K)).instructions.is_empty());
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 180 * K);
    // An earlier policy on the same life turns up on another partition: it takes 200,000 of
    // the retention, so P1 cedes 500,000.
    b.issue(6, "P0", "anna", vec![layer("life", "life", 1, "L1", 200 * K, d(2027, 1, 15))]);
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 300 * K);
    assert_eq!(b.posted(PostingKind::Recoverable, &south()), 200 * K);
    b.assert_reconciles();
}

/// Purpose: when an admission is reversed, recoveries already taken stay on the reinsurer's
/// account and become disputed items citing the treaty's clauses; nothing more is recovered;
/// people settle each item under maker-checker for nothing, part or all.
/// Why it matters: addendum decision 5: "settled by people, never reversed automatically".
/// An automatic reversal would repay a reinsurer money the treaty may let the insurer keep.
#[test]
fn a_reversed_admission_raises_disputed_items_for_people_to_settle() {
    let mut b = anna();
    let id = ClaimId::from("C1");
    fact(&mut b, reserved("C1", "P1", "life", 800 * K));
    fact(&mut b, paid("C1", "PAY-1", 400 * K, false));
    let balance = b.ri.balance(&retail(), &north());
    assert_eq!(balance, -90 * K);
    let reversal = ClaimFact::AdmissionReversed {
        claim: id.clone(),
        decided_on: d(2027, 6, 9),
        reason_code: "exclusion_missed".into(),
        cause: Some("assessor_override".into()),
    };
    let applied = fact(&mut b, reversal.clone());
    let reference = dispute_reference(&id, &retail(), &north());
    assert_eq!(reference, "DSP/C1/retail-life-surplus/reinsurer-north");
    assert_eq!(applied.notices.iter().filter(|n| matches!(n, Notice::DisputedItemRaised { .. })).count(), 2);
    let item = b.ri.dispute(&reference).unwrap().clone();
    assert_eq!((item.amount_minor, item.decided_on, item.partition), (90 * K, d(2027, 6, 9), 1));
    assert_eq!(
        item.clauses,
        vec!["errors and omissions: article 14".to_string(), "claims cooperation: article 11".to_string()]
    );
    assert_eq!(item.cause.as_deref(), Some("assessor_override"));
    // The recovery taken stands; the expectation of more is withdrawn.
    assert_eq!(b.ri.balance(&retail(), &north()), balance);
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 90 * K);
    assert_eq!(b.ri.exposure(d(2027, 6, 10))[&north()].disputed_minor, 90 * K);
    assert!(b.ri.exceptions().contains_key(&format!("disputed_item:{reference}")));
    // Nothing more is recovered, whatever happens to the claim afterwards.
    assert!(fact(&mut b, paid("C1", "PAY-2", 100 * K, false)).instructions.is_empty());
    assert!(fact(&mut b, reversal).notices.is_empty());
    assert!(fact(
        &mut b,
        ClaimFact::Closed {
            claim: id.clone(),
            outcome: Some("admission_reversed".into())
        }
    )
    .instructions
    .is_empty());
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Reversed);
    // The insurer writes off the overpayment: recorded on each item for the negotiation.
    fact(
        &mut b,
        ClaimFact::OverpaymentWrittenOff {
            claim: id.clone(),
            amount_minor: 500 * K,
            insurer_error: true,
        },
    );
    let item = b.ri.dispute(&reference).unwrap();
    assert!(item.insurer_error && item.written_off_minor == 500 * K);

    let resolve = |repay, prepared: &str, approved: &str, agreement: &str| Command::ResolveDispute {
        reference: "DSP/C1/retail-life-surplus/reinsurer-north".into(),
        repay_minor: repay,
        agreement_ref: agreement.into(),
        prepared_by: prepared.into(),
        approved_by: approved.into(),
    };
    assert_eq!(b.refused("finance", resolve(45 * K, "kim", "kim", "AG-1")), "maker_checker");
    assert_eq!(b.refused("finance", resolve(91 * K, "kim", "lee", "AG-1")), "invalid_resolution");
    assert_eq!(b.refused("finance", resolve(-1, "kim", "lee", "AG-1")), "invalid_resolution");
    assert_eq!(b.refused("finance", resolve(45 * K, "kim", "lee", "")), "invalid_resolution");
    let applied = b.ok("finance", resolve(45 * K, "kim", "lee", "AG-1"));
    assert!(applied.notices.contains(&Notice::DisputeResolved {
        claim: ClaimId::from("C1"),
        reference: reference.clone(),
        repay_minor: 45 * K
    }));
    assert_eq!(b.posted(PostingKind::DisputeSettlement, &north()), 45 * K);
    assert_eq!(b.ri.balance(&retail(), &north()), balance + 45 * K);
    assert!(!b.ri.exceptions().contains_key(&format!("disputed_item:{reference}")));
    assert_eq!(b.ri.exposure(d(2027, 6, 10))[&north()].disputed_minor, 0);
    // The same settlement again is harmless; a different one is refused.
    assert!(b.ok("finance", resolve(45 * K, "kim", "lee", "AG-1")).instructions.is_empty());
    assert_eq!(b.refused("finance", resolve(0, "kim", "lee", "AG-2")), "already_resolved");
    let unknown = Command::ResolveDispute {
        reference: "DSP/none".into(),
        repay_minor: 0,
        agreement_ref: "a".into(),
        prepared_by: "a".into(),
        approved_by: "b".into(),
    };
    assert_eq!(b.refused("finance", unknown), "unknown_dispute");
    // The south item is settled with nothing repaid: no entry, item closed.
    let south_ref = dispute_reference(&id, &retail(), &south());
    let applied = b.ok(
        "finance",
        Command::ResolveDispute {
            reference: south_ref,
            repay_minor: 0,
            agreement_ref: "AG-3".into(),
            prepared_by: "kim".into(),
            approved_by: "lee".into(),
        },
    );
    assert!(applied.instructions.is_empty());
    assert!(b.ri.exceptions().is_empty());
    b.assert_reconciles();
}

/// Purpose: claim facts that cannot be right are refused with their own codes and change
/// nothing.
/// Why it matters: the claims module delivers facts at least once and from another codebase.
/// A negative reserve or a fact for an unknown claim must stop at the door, not post money.
#[test]
fn impossible_claim_facts_are_refused() {
    let mut b = anna();
    assert_eq!(
        b.refused("claims", Command::Claim(Box::new(reserved("C1", "P1", "life", -1)))),
        "invalid_amount"
    );
    for f in [
        ClaimFact::ReserveChanged {
            claim: ClaimId::from("zz"),
            gross_reserve_minor: 1,
        },
        ClaimFact::Admitted {
            claim: ClaimId::from("zz"),
            admitted_minor: 1,
        },
        paid("zz", "p", 1, false),
        ClaimFact::Closed {
            claim: ClaimId::from("zz"),
            outcome: None,
        },
        ClaimFact::AdmissionReversed {
            claim: ClaimId::from("zz"),
            decided_on: d(2027, 6, 1),
            reason_code: "x".into(),
            cause: None,
        },
        ClaimFact::OverpaymentWrittenOff {
            claim: ClaimId::from("zz"),
            amount_minor: 1,
            insurer_error: false,
        },
    ] {
        assert_eq!(b.refused("claims", Command::Claim(Box::new(f))), "unknown_claim");
    }
    fact(&mut b, reserved("C1", "P1", "life", 800 * K));
    assert_eq!(
        b.refused("claims", Command::Claim(Box::new(reserved("C1", "P1", "trauma", 800 * K)))),
        "claim_exists"
    );
    assert_eq!(
        b.refused(
            "claims",
            Command::Claim(Box::new(ClaimFact::ReserveChanged {
                claim: ClaimId::from("C1"),
                gross_reserve_minor: -1
            }))
        ),
        "invalid_amount"
    );
    assert_eq!(
        b.refused(
            "claims",
            Command::Claim(Box::new(ClaimFact::Admitted {
                claim: ClaimId::from("C1"),
                admitted_minor: -1
            }))
        ),
        "invalid_amount"
    );
    assert_eq!(b.refused("claims", Command::Claim(Box::new(paid("C1", "", 1, false)))), "invalid_amount");
    assert_eq!(b.refused("claims", Command::Claim(Box::new(paid("C1", "p", -1, false)))), "invalid_amount");
    // A clawed-back payment is a negative payment with its own reference.
    fact(&mut b, paid("C1", "PAY-1", 800 * K, false));
    fact(&mut b, paid("C1", "PAY-1-R", -800 * K, false));
    assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 0);
    b.assert_reconciles();
}

/// Purpose: catastrophe cover responds once enough lives are lost in one event, on what the
/// insurer paid and kept above the retention; stop loss responds to what is kept across the
/// year after the catastrophe cover; both split across their panels.
/// Why it matters: these covers protect the balance sheet in the worst year. They respond to
/// the portfolio, not a policy, so no partition can work them out on its own.
#[test]
fn catastrophe_and_stop_loss_respond_to_what_is_kept_across_claims() {
    let mut b = Bench::with_aggregates();
    let stop = STOP_LOSS
        .replace("stop-loss-2027", "stop-loss-low")
        .replace("attachment = dollars(30000000)", "attachment = dollars(600000)");
    b.ok(
        "config",
        Command::ActivateAggregateTreaty {
            treaty: Box::new(load_aggregate_treaty("s.star", &stop).unwrap()),
        },
    );
    let (cat, low) = (TreatyId::from("catastrophe-2027"), TreatyId::from("stop-loss-low"));
    for n in 1..=4 {
        b.issue(
            n,
            &format!("P{n}"),
            &format!("life-{n}"),
            vec![layer("life", "life", 1, "L1", 1500 * K, d(2027, 2, 1))],
        );
    }
    b.today = d(2027, 6, 10);
    let lose = |b: &mut Bench, n: u16, event: Option<&str>| {
        let mut f = reserved(&format!("C{n}"), &format!("P{n}"), "life", 1500 * K);
        if let ClaimFact::Reserved { catastrophe_event, .. } = &mut f {
            *catastrophe_event = event.map(str::to_string);
        }
        fact(b, f);
        fact(b, paid(&format!("C{n}"), &format!("PAY-{n}"), 1500 * K, false));
    };
    // Each claim costs 1.5m, recovers 1m under the surplus treaty and leaves 500,000 kept.
    lose(&mut b, 1, Some("quake-1"));
    lose(&mut b, 2, Some("quake-1"));
    assert_eq!(b.ri.booked(&cat, &north(), LineKind::AggregateRecovery), 0);
    assert_eq!(b.ri.booked(&low, &south(), LineKind::AggregateRecovery), 400 * K);
    lose(&mut b, 3, Some("quake-1"));
    // Three lives, 1.5m kept, 1m retention: 500,000 from the catastrophe cover, 50/50.
    assert_eq!(b.ri.booked(&cat, &north(), LineKind::AggregateRecovery), 250 * K);
    assert_eq!(b.ri.booked(&cat, &south(), LineKind::AggregateRecovery), 250 * K);
    // Stop loss counts what the catastrophe cover left: 1.5m - 0.5m - 0.6m attachment.
    assert_eq!(b.ri.booked(&low, &south(), LineKind::AggregateRecovery), 400 * K);
    // A claim outside any event counts for stop loss only.
    lose(&mut b, 4, None);
    assert_eq!(b.ri.booked(&cat, &north(), LineKind::AggregateRecovery), 250 * K);
    assert_eq!(b.ri.booked(&low, &south(), LineKind::AggregateRecovery), 900 * K);
    assert_eq!(b.ri.booked(&TreatyId::from("stop-loss-2027"), &south(), LineKind::AggregateRecovery), 0);
    assert_eq!(b.posted(PostingKind::AggregateRecovery, &north()), 250 * K);
    b.assert_reconciles();
}

/// Purpose: a claim on cover ceded facultatively recovers the facultative reinsurer's share
/// as it does a treaty share, and the exposure to that reinsurer shows the sum ceded to it
/// and the recovery expected while the claim is open.
/// Why it matters: facultative cover is placed exactly where the amounts are largest. A
/// recovery that counted treaty shares only would leave the insurer carrying the part of a
/// claim it paid a reinsurer to take, and the exposure report would understate what one
/// reinsurer owes if it failed.
#[test]
fn a_claim_on_facultative_cover_recovers_the_facultative_share() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 450 * K, d(2027, 2, 1))]);
    b.today = d(2027, 3, 1);
    b.issue_scheme(2, "S1", d(2027, 3, 1), vec![member_cover("M1", 700 * K)]);
    b.feed(
        2,
        vec![ic_core::event::Event::MemberIdentified {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            party: ic_core::PartyId::from("anna"),
        }],
    );
    // The scheme cover keeps 50,000 and 650,000 is with north on the standing terms.
    b.today = d(2027, 6, 10);
    let on = d(2027, 6, 1);
    assert_eq!(b.ri.exposure(on)[&north()].ceded_sum_at_risk_minor, 650 * K);
    fact(
        &mut b,
        ClaimFact::Reserved {
            claim: ClaimId::from("C1"),
            policy: PolicyId::from("S1"),
            cover: "group-life".into(),
            life: LifeId::from("M1"),
            event_date: on,
            gross_reserve_minor: 700 * K,
            periodic: false,
            catastrophe_event: None,
        },
    );
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 650 * K);
    let exposure = b.ri.exposure(on);
    assert_eq!(exposure[&north()].open_recoverables_minor, 650 * K);
    assert!(!exposure.contains_key(&south()), "south has no share of either cover");
    fact(
        &mut b,
        ClaimFact::Admitted {
            claim: ClaimId::from("C1"),
            admitted_minor: 700 * K,
        },
    );
    fact(&mut b, paid("C1", "PAY-1", 700 * K, false));
    assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 650 * K);
    assert_eq!(b.ri.exposure(on)[&north()].open_recoverables_minor, 0);
    b.assert_reconciles();
}

/// One of the claims module's published facts, as the core's event feed shows it.
fn published(kind: &str, claim: &str, detail: &[(&str, &str)]) -> ic_core::event::Event {
    ic_core::event::Event::Module {
        module: "claims".into(),
        kind: kind.into(),
        subject: claim.into(),
        detail: detail.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

const RESERVED: [(&str, &str); 8] = [
    ("claim", "C1"),
    ("policy", "P1"),
    ("cover", "life"),
    ("life", "L1"),
    ("event_date", "2027-06-01"),
    ("gross_reserve_minor", "80000000"),
    ("periodic", "false"),
    ("catastrophe_event", ""),
];

/// Purpose: the facts the claims module publishes on a partition's event feed drive a
/// claim's recoveries from reserve to payment with nobody sending the module a command: the
/// reserve, the admission, the payment, a payment the bank returns, the close, a reopening
/// and a reversed admission each do what the same fact sent as a command does, and kinds
/// reinsurance has no use for, and other modules' facts, change nothing.
/// Why it matters: this is how recoveries happen in production. The claims module lives in
/// the core and tells nobody directly, so a fact misread here is a recovery not billed, or
/// billed for a payment the bank sent back.
#[test]
fn claims_facts_published_by_the_core_drive_the_recoveries() {
    let mut b = anna();
    b.feed(1, vec![published("claim_reserved", "C1", &RESERVED)]);
    assert_eq!(
        (b.posted(PostingKind::Recoverable, &north()), b.posted(PostingKind::Recoverable, &south())),
        (180 * K, 120 * K)
    );
    assert_eq!(claim(&b, "C1").event_date, d(2027, 6, 1));
    // Work inside claims, and another module's fact with the same name, are not ours.
    let quiet = b.feed(
        1,
        vec![
            published("claim_assigned", "C1", &[("handover", "false")]),
            published("evidence_requested", "C1", &[("evidence", "e1"), ("due", "2027-06-20")]),
            ic_core::event::Event::Module {
                module: "group".into(),
                kind: "claim_closed".into(),
                subject: "C1".into(),
                detail: Default::default(),
            },
        ],
    );
    assert!(quiet.instructions.is_empty() && quiet.notices.is_empty());
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Open);

    b.feed(1, vec![published("claim_reserve_changed", "C1", &[("gross_reserve_minor", "40000000")])]);
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), 90 * K);
    b.feed(1, vec![published("claim_admitted", "C1", &[("admitted_minor", "80000000")])]);
    assert_eq!(claim(&b, "C1").admitted_minor, Some(800 * K));
    let pay = [
        ("policy", "P1"),
        ("account_ref", "acct-1"),
        ("payment_ref", "PAY-1"),
        ("amount_minor", "80000000"),
        ("net_minor", "80000000"),
        ("payee", "party-claimant"),
        ("period_end", ""),
        ("ex_gratia", "false"),
    ];
    b.feed(1, vec![published("claim_paid", "C1", &pay)]);
    assert_eq!(
        (b.posted(PostingKind::RecoveryDue, &north()), b.posted(PostingKind::RecoveryDue, &south())),
        (180 * K, 120 * K)
    );
    // The bank sends the payment back: nothing is due from the reinsurers until it is paid again.
    // A provider's fee that comes back was never a benefit, so it changes nothing. The
    // benefit comes back with the interest paid for lateness, which was never counted.
    let fee = [("payment_ref", "FEE-1"), ("amount_minor", "25000"), ("cost", "true"), ("ex_gratia", "false")];
    assert!(b.feed(1, vec![published("payment_returned", "C1", &fee)]).instructions.is_empty());
    let back = [
        ("payment_ref", "PAY-1"),
        ("amount_minor", "80012345"),
        ("interest_minor", "12345"),
        ("cost", "false"),
    ];
    b.feed(1, vec![published("payment_returned", "C1", &back)]);
    assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 0);
    assert_eq!(claim(&b, "C1").paid_minor, 0);
    let mut again = pay;
    again[2] = ("payment_ref", "PAY-2");
    b.feed(1, vec![published("claim_paid", "C1", &again)]);
    assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 180 * K);

    b.feed(1, vec![published("claim_closed", "C1", &[("outcome", "paid")])]);
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Closed);
    b.feed(
        1,
        vec![
            published("claim_reopened", "C1", &[("basis", "NewEvidence")]),
            published("claim_reserve_changed", "C1", &[("gross_reserve_minor", "80000000")]),
        ],
    );
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Open);

    let reversed = b.feed(
        1,
        vec![
            published("claim_closed", "C1", &[("outcome", "reversed")]),
            published(
                "admission_reversed",
                "C1",
                &[("decided_on", "2027-06-10"), ("reason_code", "R-1"), ("cause", "override")],
            ),
        ],
    );
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Reversed);
    assert_eq!(
        reversed.notices.iter().filter(|n| matches!(n, Notice::DisputedItemRaised { .. })).count(),
        2,
        "one disputed item for each reinsurer that paid"
    );
    // A reopening does not take a reversed admission away from the people settling it.
    b.feed(1, vec![published("claim_reopened", "C1", &[("basis", "Complaint")])]);
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Reversed);
    assert!(b.ri.exceptions().contains_key("reopened_after_reversal:C1"));
    let written_off = [("amount_minor", "80000000"), ("insurer_error", "true"), ("status", "written_off")];
    b.feed(1, vec![published("overpayment_written_off", "C1", &written_off)]);
    let item = b.ri.dispute(&dispute_reference(&ClaimId::from("C1"), &retail(), &north())).unwrap();
    assert!(item.insurer_error && item.written_off_minor == 800 * K);
    assert!(b.ri.exceptions().keys().all(|k| !k.starts_with("claim_fact:")));
    b.assert_reconciles();
    assert_eq!(without_keys(&b.replayed()), without_keys(&b.ri));
}

/// Purpose: when claims reopens a claim whose admission was reversed, reinsurance operations
/// are told once and nothing changes until they act; they can resume recoveries only after
/// every disputed item on the claim is settled, and then what was repaid to a reinsurer
/// under a settlement is billed to it again while what was left with the insurer is not; a
/// second reversal raises new disputed items beside the settled ones.
/// Why it matters: a reversal overturned on review puts the claim back in payment for
/// years. Left reversed here, every later benefit would be paid with no recovery and nobody
/// told. Resumed by the machine, the insurer could bill a reinsurer for money still in
/// dispute, or lose the record of what was agreed the first time.
#[test]
fn recoveries_resume_on_a_reopened_claim_only_when_its_disputes_are_settled() {
    let mut b = anna();
    let pay = [("payment_ref", "PAY-1"), ("amount_minor", "80000000"), ("ex_gratia", "false")];
    let reversal = |on: &'static str| published("admission_reversed", "C1", &[("decided_on", on), ("reason_code", "R-1")]);
    b.feed(
        1,
        vec![
            published("claim_reserved", "C1", &RESERVED),
            published("claim_admitted", "C1", &[("admitted_minor", "80000000")]),
            published("claim_paid", "C1", &pay),
            reversal("2027-06-10"),
        ],
    );
    let due = |b: &Bench| (b.posted(PostingKind::RecoveryDue, &north()), b.posted(PostingKind::RecoveryDue, &south()));
    assert_eq!(due(&b), (180 * K, 120 * K));
    let resume = |reference: &str| Command::ResumeRecoveries {
        claim: ClaimId::from("C1"),
        reference: reference.into(),
    };
    assert_eq!(b.refused("reinsurance-ops", resume("vault/d-1")), "not_reopened");

    let reopened = b.feed(1, vec![published("claim_reopened", "C1", &[("basis", "InternalReview")])]);
    assert!(reopened
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ExceptionOpened { id, code, .. } if id == "reopened_after_reversal:C1" && code == "reopened_after_reversal")));
    assert!(reopened.instructions.is_empty());
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Reversed);
    assert!(b
        .feed(1, vec![published("claim_reopened", "C1", &[("basis", "InternalReview")])])
        .notices
        .is_empty());
    assert_eq!(b.refused("reinsurance-ops", resume(" ")), "reference_required");
    assert_eq!(b.refused("claims", resume("vault/d-1")), "caller_not_allowed");
    assert_eq!(b.refused("reinsurance-ops", resume("vault/d-1")), "disputes_open");

    // North was repaid in full when the admission was reversed; south kept what it had paid.
    let id = ClaimId::from("C1");
    for (reinsurer, repay) in [(north(), 180 * K), (south(), 0)] {
        b.ok(
            "finance",
            Command::ResolveDispute {
                reference: dispute_reference(&id, &retail(), &reinsurer),
                repay_minor: repay,
                agreement_ref: "AG-1".into(),
                prepared_by: "kim".into(),
                approved_by: "lee".into(),
            },
        );
    }
    let resumed = b.ok("reinsurance-ops", resume("vault/d-1"));
    assert!(resumed
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ExceptionClosed { id } if id == "reopened_after_reversal:C1")));
    assert_eq!(claim(&b, "C1").status, ClaimStatus::Open);
    assert_eq!(due(&b), (360 * K, 120 * K), "north is billed again for what it was repaid, south for nothing");
    assert_eq!(b.posted(PostingKind::DisputeSettlement, &north()), 180 * K);
    assert!(b.ri.exceptions().is_empty());
    assert_eq!(b.refused("reinsurance-ops", resume("vault/d-1")), "not_reopened");
    b.assert_reconciles();

    // Reversed a second time: new items, and the first settlements are still on record.
    b.today = d(2027, 9, 1);
    let again = b.feed(1, vec![reversal("2027-09-01")]);
    assert_eq!(again.notices.iter().filter(|n| matches!(n, Notice::DisputedItemRaised { .. })).count(), 2);
    let first = dispute_reference(&id, &retail(), &north());
    assert_eq!(b.ri.dispute(&first).unwrap().resolution.as_ref().unwrap().repay_minor, 180 * K);
    let second = b.ri.dispute(&format!("{first}/2")).unwrap();
    assert!(second.resolution.is_none() && second.amount_minor == 180 * K && second.decided_on == d(2027, 9, 1));
    // And a third time, after the same path, beside both.
    b.feed(1, vec![published("claim_reopened", "C1", &[("basis", "DisputeScheme")])]);
    for reinsurer in [north(), south()] {
        b.ok(
            "finance",
            Command::ResolveDispute {
                reference: format!("{}/2", dispute_reference(&id, &retail(), &reinsurer)),
                repay_minor: 0,
                agreement_ref: "AG-2".into(),
                prepared_by: "kim".into(),
                approved_by: "lee".into(),
            },
        );
    }
    b.ok("reinsurance-ops", resume("vault/d-2"));
    assert_eq!(due(&b), (360 * K, 120 * K), "nothing was repaid the second time, so nothing is billed again");
    b.feed(1, vec![reversal("2027-10-01")]);
    assert!(b.ri.dispute(&format!("{first}/3")).is_some());
    b.assert_reconciles();
    assert_eq!(without_keys(&b.replayed()), without_keys(&b.ri));
}

/// Purpose: a benefit in payment, which claims admits with a monthly amount and no sum,
/// keeps its expected recovery on the reserve, and each period paid is recovered as it is
/// paid.
/// Why it matters: claims publishes that admission with an admitted sum of nothing. Taken
/// literally it would drop the expected recovery on every income protection claim to what
/// has been paid so far, and the ledger would understate what reinsurers owe.
#[test]
fn a_benefit_in_payment_keeps_its_expected_recovery_on_the_reserve() {
    let mut b = anna();
    let mut reserved = RESERVED;
    reserved[5] = ("gross_reserve_minor", "40000000");
    reserved[6] = ("periodic", "true");
    b.feed(1, vec![published("claim_reserved", "C1", &reserved)]);
    assert!(claim(&b, "C1").periodic);
    let expected = b.posted(PostingKind::Recoverable, &north());
    assert!(expected > 0);
    let admitted = b.feed(
        1,
        vec![published("claim_admitted", "C1", &[("admitted_minor", "0"), ("monthly_minor", "500000")])],
    );
    assert!(admitted.instructions.is_empty());
    assert_eq!(claim(&b, "C1").admitted_minor, None);
    let pay = [
        ("payment_ref", "RUN-1/C1"),
        ("amount_minor", "500000"),
        ("period_end", "2027-06-30"),
        ("ex_gratia", "false"),
    ];
    b.feed(1, vec![published("claim_paid", "C1", &pay)]);
    assert!(b.posted(PostingKind::RecoveryDue, &north()) > 0);
    assert_eq!(b.posted(PostingKind::Recoverable, &north()), expected);
    b.assert_reconciles();
}

/// Purpose: a claims fact that cannot be read, or that the module cannot take, opens one
/// exception on its claim and the feed carries on; the exception closes when the fact is
/// entered by hand, and a fact about a claim the module has never seen is treated the same
/// way.
/// Why it matters: the feed carries every partition's events in order, so refusing a batch
/// for one bad fact would stop cessions and premiums for every policy behind it. Dropping
/// the fact silently would leave a recovery wrong with nothing to show for it.
#[test]
fn a_claims_fact_that_cannot_be_taken_is_raised_and_the_feed_goes_on() {
    let mut b = anna();
    let mut unreadable = RESERVED;
    unreadable[5] = ("gross_reserve_minor", "lots");
    let applied = b.feed(
        1,
        vec![
            published("claim_reserved", "C1", &unreadable),
            published("claim_reserve_changed", "C7", &[("gross_reserve_minor", "100")]),
            published("claim_paid", "C8", &[("amount_minor", "100")]),
            published("admission_reversed", "C9", &[("decided_on", "tomorrow"), ("reason_code", "R-1")]),
            published(
                "payment_returned",
                "C6",
                &[("payment_ref", "P"), ("amount_minor", "5"), ("interest_minor", "some")],
            ),
            published("claim_reserved", "C2", &RESERVED),
        ],
    );
    let why = |id: &str| b.ri.exceptions()[&format!("claim_fact:{id}")].detail.clone();
    assert!(
        why("C1").contains("gross_reserve_minor is not an amount") && why("C1").contains("1-"),
        "{}",
        why("C1")
    );
    assert!(why("C7").contains("unknown_claim"), "{}", why("C7"));
    assert!(why("C8").contains("claim_paid has no payment_ref"), "{}", why("C8"));
    assert!(why("C9").contains("decided_on is not a date"), "{}", why("C9"));
    assert!(why("C6").contains("interest_minor is not an amount"), "{}", why("C6"));
    assert_eq!(b.ri.exceptions()["claim_fact:C1"].code, "claim_fact_not_taken");
    // The fact behind the bad ones was taken, and the feed has moved past all five.
    assert!(b.ri.claim(&ClaimId::from("C2")).is_some());
    assert_eq!(applied.notices.iter().filter(|n| matches!(n, Notice::ExceptionOpened { .. })).count(), 5);
    b.feed(1, vec![published("claim_reserve_changed", "C2", &[("gross_reserve_minor", "100")])]);
    // Operations enter the fact that could not be read, which settles the exception.
    assert_eq!(
        b.refused("underwriting", Command::Claim(Box::new(reserved("C1", "P1", "life", 800 * K)))),
        "caller_not_allowed"
    );
    b.ok("reinsurance-ops", Command::Claim(Box::new(reserved("C1", "P1", "life", 800 * K))));
    assert!(!b.ri.exceptions().contains_key("claim_fact:C1"));
    assert!(b.ri.exceptions().contains_key("claim_fact:C7"));
    b.assert_reconciles();
}

/// Purpose: each notice for claims becomes the command the claims module publishes for
/// it, word for word, sent as `reinsurance` to the partition that holds the claim's policy:
/// the large-claim notification, the consent requirement, a consent, a disputed item, and
/// its settlement with the claim it belongs to. Notices for anyone else have no such command.
/// Why it matters: the claims module reads these by name. A field spelled differently is a
/// notice the core refuses in production, and a settlement sent without its claim cannot be
/// matched to the disputed item it closes.
#[test]
fn notices_for_claims_are_the_commands_claims_reads() {
    let mut b = Bench::new();
    b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 3000 * K, d(2027, 2, 1))]);
    b.today = d(2027, 6, 10);
    fact(&mut b, reserved("C1", "P1", "life", 3000 * K));
    b.ok(
        "reinsurance-ops",
        Command::ReinsurerConsent {
            claim: ClaimId::from("C1"),
            reinsurer: north(),
            agreed: true,
            reference: "email-77".into(),
        },
    );
    fact(&mut b, paid("C1", "PAY-1", 3000 * K, false));
    fact(
        &mut b,
        ClaimFact::AdmissionReversed {
            claim: ClaimId::from("C1"),
            decided_on: d(2027, 6, 10),
            reason_code: "exclusion_missed".into(),
            cause: None,
        },
    );
    let reference = dispute_reference(&ClaimId::from("C1"), &retail(), &north());
    b.ok(
        "finance",
        Command::ResolveDispute {
            reference: reference.clone(),
            repay_minor: 25_00,
            agreement_ref: "AG-1".into(),
            prepared_by: "kim".into(),
            approved_by: "lee".into(),
        },
    );
    let mut sent = Vec::new();
    let mut others = 0;
    for (seq, notice) in b.ri.notices(usize::MAX) {
        match b.ri.notice_to_core(seq) {
            Some(delivery) => {
                assert_eq!(notice.audience(), ic_reinsurance::machine::Audience::Claims);
                assert_eq!((delivery.partition, delivery.caller), (3, "reinsurance"));
                assert_eq!(delivery.key, format!("reinsurance:notice:{seq}:0"));
                sent.push(serde_json::to_value(&delivery.command).unwrap()["claims"]["reinsurance"].clone());
            }
            None => others += 1,
        }
    }
    assert!(others > 0, "exceptions and documents are not for claims");
    assert!(b.ri.notice_to_core(9_999).is_none());
    use serde_json::json;
    let expected = [
        json!({"large_claim_notification": {"claim": "C1", "treaty": "retail-life-surplus"}}),
        json!({"reinsurer_consent_required": {"claim": "C1", "treaty": "retail-life-surplus", "due_by": "2027-06-20"}}),
        json!({"reinsurer_consent": {"claim": "C1", "reinsurer": "reinsurer-north", "agreed": true, "reference": "email-77", "treaty": null}}),
        json!({"disputed_item_raised": {"claim": "C1", "reinsurer": "reinsurer-north", "reference": reference}}),
        json!({"disputed_item_raised": {"claim": "C1", "reinsurer": "reinsurer-south", "reference": "DSP/C1/retail-life-surplus/reinsurer-south"}}),
        json!({"dispute_resolved": {"claim": "C1", "reference": reference, "repay_minor": 25_00}}),
    ];
    assert_eq!(sent, expected);
}

/// Purpose: a policy avoided for non-disclosure is treated as never ceded: its covers leave
/// the cession record from the day they started, the retention they used on the life goes to
/// the person's other policy from that day, every premium ceded for it comes back, and the
/// recoveries billed on its claim are taken back from the reinsurers. The same event delivered
/// again changes nothing.
/// Why it matters: the core treats an avoided policy as never in force and refunds or keeps
/// the customer's premium. If reinsurance kept its cessions, reinsurers would hold premium for
/// a risk that never existed, would have paid a claim they never owed, and the person's other
/// policy would stay over-ceded for want of retention the avoided policy still held.
#[test]
fn an_avoided_policy_is_reversed_from_inception() {
    use ic_core::event::Event;
    use ic_core::policy::PremiumOnAvoidance;
    use ic_core::{PartyId, PolicyStatus};
    use ic_reinsurance::LifeKey;
    let mut b = anna();
    b.factors("P1", "L1", d(1990, 3, 10), false);
    b.issue(1, "P2", "anna", vec![layer("life", "life", 1, "L1", 300 * K, d(2027, 3, 1))]);
    let anna_life = LifeKey::Party(PartyId::from("anna"));
    assert_eq!(b.ri.retained_on(&anna_life, d(2027, 3, 1)), 500 * K);
    b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    assert!(b.posted(PostingKind::CededPremium, &north()) > 0);
    fact(&mut b, reserved("C1", "P1", "life", 800 * K));
    fact(&mut b, paid("C1", "PAY-1", 800 * K, false));
    assert_eq!(b.posted(PostingKind::RecoveryDue, &north()), 180 * K);

    let avoided = || {
        vec![
            Event::PolicyStatusChanged {
                policy: PolicyId::from("P1"),
                from: PolicyStatus::InForce,
                to: PolicyStatus::Avoided,
            },
            Event::PolicyAvoided {
                policy: PolicyId::from("P1"),
                case: "nd-1".into(),
                premium: PremiumOnAvoidance::Retain,
                refunded_minor: 0,
            },
        ]
    };
    b.feed(1, avoided());
    for on in [d(2027, 2, 1), d(2027, 3, 1), d(2027, 6, 10)] {
        assert!(b.ri.cession(&key("P1", "life", 1, "L1"), on).is_none());
    }
    // The other policy now has the whole retention from its own first day.
    assert_eq!(b.ri.retained_on(&anna_life, d(2027, 2, 1)), 0);
    assert_eq!(b.ri.retained_on(&anna_life, d(2027, 3, 1)), 300 * K);
    assert!(b.ri.charged(&PolicyId::from("P1"), d(2027, 2, 1)).unwrap().is_empty());
    for reinsurer in [north(), south()] {
        assert_eq!(b.posted(PostingKind::CededPremium, &reinsurer), 0);
        assert_eq!(b.posted(PostingKind::Commission, &reinsurer), 0);
        assert_eq!(b.posted(PostingKind::RecoveryDue, &reinsurer), 0);
        assert_eq!(b.posted(PostingKind::Recoverable, &reinsurer), 0);
        assert_eq!(b.ri.balance(&retail(), &reinsurer), 0);
    }
    assert!(claim(&b, "C1").due.values().all(|v| *v == 0));
    assert!(b.ri.exposure(d(2027, 6, 10)).values().all(|e| e.ceded_sum_at_risk_minor == 0));
    b.assert_reconciles();
    let again = b.feed(1, avoided());
    assert!(again.instructions.is_empty());
    assert_eq!(without_keys(&b.replayed()), without_keys(&b.ri));
}
