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
