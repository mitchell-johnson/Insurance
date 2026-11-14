//! Commission on real premiums: the core in a box issues and collects, the
//! module allocates, and the core's own ledgers are what the tests read.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use ic_core::counterparty::InternalLedger;
use ic_core::{Command as CoreCommand, PolicyStatus};
use ic_distribution::config::{AdviserChangeMode, AdviserChangeSetting, BookOutcome, Change, MoveRequest};
use ic_distribution::model::{Dest, PostingState};
use ic_distribution::rules::{portion, CommissionType, Component, Fragment, HeldReason, Line, Share, WHOLE};
use ic_distribution::structure::{PartyKind, Relation, RelationKind};
use ic_distribution::{BookId, Command, EngagementId, Event, FragmentId};

const ANNUAL: i64 = 1_200_00;

/// An advised policy with its first premium paid. Returns the world, the
/// policy and the renewal commission that first premium earned.
fn advised() -> (World, ic_core::PolicyId, i64) {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", ANNUAL).unwrap();
    w.pay(&policy, "r1");
    let renewal = w.payable("a-co") + w.payable("fap-one") - ANNUAL;
    (w, policy, renewal)
}

fn start() -> chrono::NaiveDate {
    d(START.0, START.1, START.2)
}

/// Purpose: the first premium on an advised policy pays initial commission on the annual
/// premium and renewal commission on the premium received, split 75/25 by the book's rule,
/// and the amounts land in each payee's payable account on the core's ledger.
/// Why it matters: this is the whole forward path in one test: event in, allocation, posting
/// to the core, confirmation back. If any link breaks, nobody is paid or the ledger and the
/// module disagree, and every reconciliation in the design depends on them agreeing.
#[test]
fn a_first_premium_pays_initial_and_renewal_commission_by_the_rule() {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    let paid = w.pay(&policy, "r1");
    assert!(paid > 0);
    let net = w.dist.movements.iter().map(|m| m.amount_minor).sum::<i64>();
    // Initial: 100% of 1,200.00. Renewal: 10% of the cover premium received.
    let renewal = net - 1_200_00;
    assert!(renewal > 0 && renewal < paid / 5, "renewal {renewal} on {paid}");
    assert_eq!(w.payable("a-co") + w.payable("fap-one"), net);
    assert_eq!(w.payable("a-co"), net - w.payable("fap-one"));
    // 25% of each component, rounded half-even; A's company absorbs the odd cent.
    assert_eq!(w.payable("fap-one"), 300_00 + ic_distribution::rules::portion(renewal, pct(25.0)));
    w.assert_reconciled();
}

/// Purpose: the second premium earns renewal commission only; initial commission is paid once
/// per layer however many premiums arrive.
/// Why it matters: paying initial commission twice is the most expensive single mistake this
/// module could make, and every monthly policy would trigger it eleven times a year.
#[test]
fn initial_commission_is_paid_once_and_renewal_on_every_premium() {
    let (mut w, policy, first_renewal) = advised();
    let before = w.payable("a-co") + w.payable("fap-one");
    w.days(31);
    let paid = w.pay(&policy, "r2");
    assert!(paid > 0);
    let earned = w.payable("a-co") + w.payable("fap-one") - before;
    assert_eq!(earned, first_renewal, "the same premium earns the same renewal and no more initial");
    assert_eq!(w.dist.exposures.len(), 2, "one exposure item per payee of the one initial payment");
    w.assert_reconciled();
}

/// Purpose: commission expense is recorded against the product and the cover type that earned
/// it, and the expense accounts equal what payees were credited.
/// Why it matters: finance reports commission cost by product line. If expense were lumped
/// together, or drifted from payables, the margin by cover type would be wrong and unprovable.
#[test]
fn expense_is_recorded_by_cover_type_and_equals_what_payees_were_credited() {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, Some(100_000_00));
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", ANNUAL).unwrap();
    w.write(&policy, "trauma", 1, "eng-a-one", "a-at-fap-one", 600_00).unwrap();
    w.pay(&policy, "r1");
    let expense = |cover: &str| {
        w.internal(InternalLedger::CommissionExpense {
            product: "term-life".into(),
            cover_type: cover.into(),
        })
    };
    let types: BTreeSet<String> = w.dist.allocations.values().flat_map(|a| a.lines.iter().map(|l| l.cover_type.clone())).collect();
    assert_eq!(types.len(), 2, "life and trauma are different cover types: {types:?}");
    let total: i64 = types.iter().map(|t| expense(t)).sum();
    assert_eq!(total, w.payable("a-co") + w.payable("fap-one"));
    assert!(types.iter().all(|t| expense(t) > 0));
    assert!(total > ANNUAL + 600_00);
    w.assert_reconciled();
}

/// Purpose: a dishonoured premium takes commission back from exactly the payees the original
/// entry paid, even though the book's rule has changed since.
/// Why it matters: the design is explicit that a reversal follows the entry it reverses. If it
/// followed today's rule, a rule change would move one party's debt onto another.
#[test]
fn a_dishonour_reverses_the_original_lines_whatever_the_rule_says_today() {
    let (mut w, policy, _) = advised();
    w.days(1);
    let today = w.today();
    w.activate(
        "all-to-fap",
        vec![Change::ActivateRule(rule(
            "a-at-fap-one",
            2,
            today,
            vec![base("fap-one", Share::OfRemainder(WHOLE), true)],
            vec![liable("fap-one")],
        ))],
    )
    .unwrap();
    w.core(
        &policy,
        "cash-matching",
        CoreCommand::ReverseReceipt {
            policy: policy.clone(),
            receipt: "r1".into(),
        },
    )
    .unwrap();
    assert_eq!(w.payable("a-co"), 0);
    assert_eq!(w.payable("fap-one"), 0);
    assert!(w.dist.exposures.values().all(|e| !e.open), "a reversed payment leaves nothing to claw back");
    assert!(w.events.iter().any(|e| matches!(e, Event::CommissionReversed { .. })));
    // The premium paid again earns initial commission again, now under the new rule.
    w.pay(&policy, "r1-again");
    assert_eq!(w.payable("a-co"), 0);
    assert!(w.payable("fap-one") > ANNUAL);
    w.assert_reconciled();
}

/// Purpose: a lapse inside the clawback period debits the party the rule names as liable for
/// the whole of the initial commission, not the parties who were paid it; reinstatement
/// reverses the clawback and the exposure runs again.
/// Why it matters: this is the worked example's central point. The provider bears clawback for
/// its advisers, and a reinstated policy must leave everyone exactly where they were.
#[test]
fn a_lapse_claws_back_from_the_liable_party_and_reinstatement_restores_it() {
    let (mut w, policy, renewal) = advised();
    let a_co = w.payable("a-co");
    let fap = w.payable("fap-one");
    assert_eq!(w.dist.clawback_exposure(w.today()), BTreeMap::from([(party("fap-one"), ANNUAL)]));
    w.until_status(&policy, PolicyStatus::Lapsed);
    assert_eq!(w.payable("a-co"), a_co, "the adviser's company is not liable under this rule");
    assert_eq!(w.payable("fap-one"), fap - ANNUAL);
    assert!(w
        .events
        .iter()
        .any(|e| matches!(e, Event::ClawbackRaised { total_minor, .. } if *total_minor == ANNUAL)));
    assert!(w.dist.clawback_exposure(w.today()).is_empty());
    w.assert_reconciled();

    // The instalment that fell due on the day of the lapse was not billed that day; the
    // core bills it at reinstatement, so it is paid with the arrears.
    let lapse = w.cluster.state(0).policies[&policy].lapse.clone().unwrap();
    let missed = lapse.receivables_before.last().map_or(0, |r| r.total_minor);
    w.pay_amount(&policy, "catch-up", lapse.arrears_minor + missed);
    w.core(&policy, "collections", CoreCommand::Reinstate { policy: policy.clone() }).unwrap();
    // The clawback is reversed, and the arrears now applied earn renewal commission.
    let total = w.payable("a-co") + w.payable("fap-one");
    assert!(total > a_co + fap, "renewal on the arrears: {total}");
    assert!(total - a_co - fap < 10 * renewal);
    assert_eq!(w.dist.clawback_exposure(w.today()), BTreeMap::from([(party("fap-one"), ANNUAL)]));
    w.assert_reconciled();
}

/// Purpose: a cancellation inside the cooling-off period reverses every cent of commission
/// from whoever received it, not from the liable party.
/// Why it matters: cooling-off is a full unwind. Treating it as clawback would charge the
/// provider for money the adviser's company keeps.
#[test]
fn cancelling_in_cooling_off_reverses_all_commission_from_whoever_received_it() {
    let (mut w, policy, _) = advised();
    w.days(3);
    w.core(&policy, "policy-servicing", CoreCommand::CancelPolicy { policy: policy.clone() })
        .unwrap();
    assert_eq!(w.payable("a-co"), 0);
    assert_eq!(w.payable("fap-one"), 0);
    assert!(!w.events.iter().any(|e| matches!(e, Event::ClawbackRaised { .. })));
    w.assert_reconciled();
}

/// Purpose: a cancellation after cooling-off but inside the clawback period is a clawback on
/// the scale, charged to the liable party.
/// Why it matters: together with the cooling-off test this pins the boundary between a full
/// reversal and a scaled clawback, which decides who loses money.
#[test]
fn cancelling_after_cooling_off_is_a_clawback_on_the_scale() {
    let (mut w, policy, _) = advised();
    let fap = w.payable("fap-one");
    w.days(20);
    w.core(&policy, "policy-servicing", CoreCommand::CancelPolicy { policy: policy.clone() })
        .unwrap();
    assert_eq!(w.payable("fap-one"), fap - ANNUAL, "under twelve months the scale takes all of it");
    w.assert_reconciled();
}

/// Purpose: a policy that ends because a claim was paid never triggers clawback.
/// Why it matters: clawing back commission because a customer died would be wrong under every
/// agreement the design describes, and would be a conduct problem as well as a money one.
#[test]
fn a_claim_never_triggers_clawback() {
    let (mut w, policy, _) = advised();
    let before = (w.payable("a-co"), w.payable("fap-one"));
    w.days(10);
    let event_date = w.today();
    w.core(
        &policy,
        "claims",
        CoreCommand::TerminateOnClaim {
            policy: policy.clone(),
            event_date,
            claim: "claim-1".into(),
            payer_deceased: false,
        },
    )
    .unwrap();
    assert_eq!((w.payable("a-co"), w.payable("fap-one")), before);
    assert!(w.dist.exposures.values().all(|e| !e.open));
    assert!(w.dist.clawback_exposure(w.today()).is_empty());
}

/// Purpose: the worked example in the design notes. Adviser A moves from FAP One to FAP Two
/// with the book; FAP One keeps a 25% tail of renewal commission, A's company gets 56.25% and
/// FAP Two 18.75%; FAP One stays liable for clawback on what it was paid before the move.
/// Why it matters: an adviser move is the hardest ordinary event in commission. The tail as a
/// share of the whole with the rest shared out is what makes the split come out right without
/// rewriting the rule when the tail ends.
#[test]
fn an_adviser_moves_with_the_book_and_the_previous_provider_keeps_a_tail() {
    let (mut w, policy, renewal) = advised();
    w.days(31);
    let on = w.today();
    let request = MoveRequest {
        from_engagement: EngagementId::from("eng-a-one"),
        new_engagement: EngagementId::from("eng-a-two"),
        to: party("fap-two"),
        on,
        accreditations: BTreeSet::from(["term-life".to_string()]),
        books: vec![BookOutcome::Moves {
            book: BookId::from("a-at-fap-one"),
            policies: None,
            new_book: BookId::from("a-at-fap-two"),
            new_book_name: "A at FAP Two".into(),
            release_ref: "release-signed-by-fap-one".into(),
            lines: vec![
                base("a-co", Share::OfRemainder(pct(75.0)), true),
                base("fap-two", Share::OfRemainder(pct(25.0)), false),
            ],
            liability: vec![liable("fap-two")],
            fragments: vec![],
            tail: Some((party("fap-one"), pct(25.0), None)),
            examples: vec![],
        }],
    };
    let changes = w.dist.plan_move(&request).unwrap();
    w.activate("move-a", changes).unwrap();
    assert_eq!(w.dist.policies[&policy].servicing, Some(EngagementId::from("eng-a-two")));
    assert!(w.events.iter().any(|e| matches!(e, Event::ServicingChanged { .. })), "the customer is told");
    assert!(
        w.dist.config.books[&BookId::from("a-at-fap-two")].watch_until.is_some(),
        "replacement monitoring"
    );
    assert_eq!(w.dist.config.structure.engagements[&EngagementId::from("eng-a-one")].to, Some(on));

    let before = (w.payable("a-co"), w.payable("fap-one"), w.payable("fap-two"));
    w.pay(&policy, "r2");
    let tail = portion(renewal, pct(25.0));
    let fap_two = portion(renewal - tail, pct(25.0));
    assert_eq!(w.payable("fap-one") - before.1, tail, "25% of the whole");
    assert_eq!(w.payable("fap-two") - before.2, fap_two, "18.75%: a quarter of what the tail leaves");
    assert_eq!(w.payable("a-co") - before.0, renewal - tail - fap_two, "56.25%, and the odd cent");
    // The exposure on initial commission paid before the move stays with FAP One.
    assert_eq!(w.dist.clawback_exposure(w.today()), BTreeMap::from([(party("fap-one"), ANNUAL)]));
    w.assert_reconciled();
}

/// Purpose: planning a move refuses a book that does not exist, an unknown engagement, and a
/// move without the previous provider's release; a book that stays gets a new servicing
/// adviser and is watched.
/// Why it matters: a book only moves with a signed release. Without that check an adviser
/// could take a provider's clients and their trail by filling in a form.
#[test]
fn a_move_needs_a_release_and_a_staying_book_gets_a_new_servicing_adviser() {
    let (mut w, policy, _) = advised();
    w.add_party("adviser-b", PartyKind::Adviser);
    w.engage("eng-b-one", "adviser-b", "fap-one", d(2020, 1, 1));
    let on = w.today();
    let mut request = MoveRequest {
        from_engagement: EngagementId::from("eng-a-one"),
        new_engagement: EngagementId::from("eng-a-two"),
        to: party("fap-two"),
        on: on.succ_opt().unwrap(),
        accreditations: BTreeSet::new(),
        books: vec![BookOutcome::Moves {
            book: BookId::from("a-at-fap-one"),
            policies: Some(BTreeSet::from([policy.clone()])),
            new_book: BookId::from("new"),
            new_book_name: "New".into(),
            release_ref: " ".into(),
            lines: vec![],
            liability: vec![],
            fragments: vec![],
            tail: None,
            examples: vec![],
        }],
    };
    assert_eq!(code(w.dist.plan_move(&request)), "release_required");
    request.books = vec![BookOutcome::Stays {
        book: BookId::from("nowhere"),
        new_servicing: EngagementId::from("eng-b-one"),
    }];
    assert_eq!(code(w.dist.plan_move(&request)), "unknown_book");
    request.books = vec![BookOutcome::Moves {
        book: BookId::from("nowhere"),
        policies: None,
        new_book: BookId::from("new"),
        new_book_name: "New".into(),
        release_ref: "r".into(),
        lines: vec![],
        liability: vec![],
        fragments: vec![],
        tail: None,
        examples: vec![],
    }];
    assert_eq!(code(w.dist.plan_move(&request)), "unknown_book");
    request.from_engagement = EngagementId::from("ghost");
    assert_eq!(code(w.dist.plan_move(&request)), "unknown_engagement");

    request.from_engagement = EngagementId::from("eng-a-one");
    request.books = vec![BookOutcome::Stays {
        book: BookId::from("a-at-fap-one"),
        new_servicing: EngagementId::from("eng-b-one"),
    }];
    let changes = w.dist.plan_move(&request).unwrap();
    w.days(1);
    w.activate("a-leaves", changes).unwrap();
    assert_eq!(w.dist.policies[&policy].servicing, Some(EngagementId::from("eng-b-one")));
    assert!(w.dist.config.books[&BookId::from("a-at-fap-one")].watch_until.is_some());
    // The book stayed: commission still follows its rule.
    w.days(31);
    let before = w.payable("a-co");
    w.pay(&policy, "r2");
    assert!(w.payable("a-co") > before);
}

/// Purpose: a rule activated with a past effective date re-allocates base commission earned
/// since that date under the corrected rule, posting only the differences between payees, with
/// commission expense unchanged.
/// Why it matters: instructions arrive late. The correction must move money between the right
/// parties without touching what the insurer's books say commission cost.
#[test]
fn a_backdated_rule_moves_the_difference_between_payees_and_leaves_expense_alone() {
    let (mut w, _policy, renewal) = advised();
    let total = ANNUAL + renewal;
    let expense = w.internal(InternalLedger::CommissionExpense {
        product: "term-life".into(),
        cover_type: "life".into(),
    });
    w.days(10);
    // From day one the split should have been 60/40.
    w.activate(
        "correction",
        vec![Change::ActivateRule(rule(
            "a-at-fap-one",
            2,
            start(),
            vec![
                base("a-co", Share::OfRemainder(pct(60.0)), true),
                base("fap-one", Share::OfRemainder(pct(40.0)), false),
            ],
            vec![liable("fap-one")],
        ))],
    )
    .unwrap();
    let fap = portion(ANNUAL, pct(40.0)) + portion(renewal, pct(40.0));
    assert_eq!(w.payable("fap-one"), fap);
    assert_eq!(w.payable("a-co"), total - fap);
    assert_eq!(
        w.internal(InternalLedger::CommissionExpense {
            product: "term-life".into(),
            cover_type: "life".into(),
        }),
        expense
    );
    assert!(w.dist.allocations.values().any(|a| a.kind == "commission_reallocated"));
    // Activating the same split again changes nothing: there is no difference to post.
    let count = w.dist.allocations.len();
    w.activate(
        "same-again",
        vec![Change::ActivateRule(rule(
            "a-at-fap-one",
            3,
            start(),
            vec![
                base("a-co", Share::OfRemainder(pct(60.0)), true),
                base("fap-one", Share::OfRemainder(pct(40.0)), false),
            ],
            vec![liable("fap-one")],
        ))],
    )
    .unwrap();
    assert_eq!(w.dist.allocations.len(), count);
    w.assert_reconciled();
}

/// Purpose: overrides and fixed fees in a shared fragment are paid on top of base commission,
/// to the fragment's payees, on every book that follows it; a referral fee is paid once, with
/// initial commission.
/// Why it matters: a group's override is one definition shared by thousands of books. It must
/// add to the base split, never take from it, and a per-policy fee must not repeat monthly.
#[test]
fn a_shared_fragment_pays_overrides_and_a_one_off_fee_on_top_of_base() {
    let mut w = worked_example();
    let today = w.today();
    let over = Line {
        component: Component::Override("group".into()),
        share: Share::RateOnPremium(pct(2.0)),
        ..base("group-old", Share::OfRemainder(1), false)
    };
    let fee = Line {
        component: Component::FixedFee("referral".into()),
        commission_type: Some(CommissionType::Initial),
        share: Share::Fixed(50_00),
        ..base("group-old", Share::OfRemainder(1), false)
    };
    w.activate(
        "group-lines",
        vec![
            Change::ActivateFragment(Fragment {
                id: FragmentId::from("group-standard"),
                version: 1,
                effective_from: today,
                lines: vec![over, fee],
                liability: vec![],
            }),
            Change::ActivateRule(ic_distribution::rules::Rule {
                fragments: vec![FragmentId::from("group-standard")],
                ..rule(
                    "a-at-fap-one",
                    2,
                    today,
                    vec![
                        base("a-co", Share::OfRemainder(pct(75.0)), true),
                        base("fap-one", Share::OfRemainder(pct(25.0)), false),
                    ],
                    vec![liable("fap-one")],
                )
            }),
        ],
    )
    .unwrap();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", ANNUAL).unwrap();
    let premium = w.pay(&policy, "r1");
    let base_total = w.payable("a-co") + w.payable("fap-one");
    let first = w.payable("group-old");
    // 2% of the annual premium with initial, 2% of the premium received with renewal, and the fee.
    let net: i64 = w
        .dist
        .allocations
        .values()
        .next()
        .unwrap()
        .lines
        .iter()
        .filter(|l| l.commission_type == Some(CommissionType::Renewal) && l.component == Some(Component::Base))
        .map(|l| l.amount_minor)
        .sum::<i64>()
        * 10;
    assert!(net <= premium);
    assert_eq!(first, portion(ANNUAL, pct(2.0)) + portion(net, pct(2.0)) + 50_00);
    assert_eq!(base_total, ANNUAL + portion(net, pct(10.0)), "base is not reduced by the override");
    w.days(31);
    w.pay(&policy, "r2");
    assert_eq!(w.payable("group-old") - first, portion(net, pct(2.0)), "no fee on a renewal premium");
    w.assert_reconciled();
}

/// Purpose: a line marked disputed, and a payee with an accrual hold, earn into the held
/// account with the reason recorded, while the other payees are paid as usual.
/// Why it matters: held money must be earned and visible, not lost or paid. The reason on each
/// held item is what the workbench and the statement show.
#[test]
fn disputed_lines_and_accrual_holds_earn_into_held_with_their_reason() {
    let mut w = worked_example();
    let today = w.today();
    let mut disputed = base("fap-one", Share::OfRemainder(pct(25.0)), false);
    disputed.disputed = true;
    w.activate(
        "dispute",
        vec![Change::ActivateRule(rule(
            "a-at-fap-one",
            2,
            today,
            vec![base("a-co", Share::OfRemainder(pct(75.0)), true), disputed],
            vec![liable("fap-one")],
        ))],
    )
    .unwrap();
    assert_eq!(
        code(w.send(
            admin(),
            Command::SetAccrualHold {
                party: party("a-co"),
                hold: Some((today, HeldReason::Dispute, "x".into()))
            }
        )),
        "invalid_hold"
    );
    assert_eq!(
        code(w.send(
            admin(),
            Command::SetAccrualHold {
                party: party("ghost"),
                hold: None
            }
        )),
        "unknown_payee"
    );
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", ANNUAL).unwrap();
    w.pay(&policy, "r1");
    assert_eq!(w.payable("fap-one"), 0);
    assert!(w.held("fap-one") >= 300_00);
    assert!(w.payable("a-co") > 0);
    assert!(w
        .dist
        .held
        .iter()
        .all(|h| h.reason == Some(HeldReason::Dispute) && h.book == Some(BookId::from("a-at-fap-one"))));
    assert!(w.events.iter().any(|e| matches!(
        e,
        Event::AmountHeld {
            reason: Some(HeldReason::Dispute),
            ..
        }
    )));

    w.send(
        admin(),
        Command::SetAccrualHold {
            party: party("a-co"),
            hold: Some((w.today(), HeldReason::MissingPaymentDetails, "bank-letter".into())),
        },
    )
    .unwrap();
    let paid = w.payable("a-co");
    w.days(31);
    w.pay(&policy, "r2");
    assert_eq!(w.payable("a-co"), paid);
    assert!(w.held("a-co") > 0);
    assert!(w.dist.held.iter().any(|h| h.reason == Some(HeldReason::MissingPaymentDetails)));
    let position = w.dist.position(&party("fap-one"), w.today());
    assert_eq!(position.held.iter().map(|h| h.amount_minor).sum::<i64>(), w.held("fap-one"));
    w.assert_reconciled();
}

/// Purpose: replacement cover earns renewal commission only, and business on the house book
/// earns nothing at all.
/// Why it matters: paying initial commission on replacement business rewards churning, which
/// the design forbids; orphan business on the house book has no adviser to pay.
#[test]
fn replacement_cover_earns_trail_only_and_the_house_book_earns_nothing() {
    let mut w = worked_example();
    w.activate(
        "house",
        vec![Change::CreateBook {
            id: BookId::from("house"),
            name: "House".into(),
            engagement: None,
            owner: None,
            house: true,
        }],
    )
    .unwrap();
    let (replacement, _) = w.issue(1, 500_000_00, None);
    let (orphan, _) = w.issue(2, 500_000_00, None);
    let (original, _) = w.issue(3, 500_000_00, None);
    let mut layer = ic_distribution::state::WriteLayer {
        policy: replacement.clone(),
        cover: "life".into(),
        layer: 1,
        writing: vec![(EngagementId::from("eng-a-one"), WHOLE)],
        books: vec![(BookId::from("a-at-fap-one"), WHOLE)],
        schedule: "standard".into(),
        option: "upfront".into(),
        annual_premium_minor: ANNUAL,
        replacement: true,
    };
    w.send(module("new-business"), Command::WriteLayer(Box::new(layer.clone()))).unwrap();
    layer.policy = orphan.clone();
    layer.books = vec![(BookId::from("house"), WHOLE)];
    layer.replacement = false;
    w.send(module("new-business"), Command::WriteLayer(Box::new(layer))).unwrap();
    w.send(
        module("new-business"),
        Command::LinkReplacement {
            policy: replacement.clone(),
            replaces: original.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        code(w.send(
            module("new-business"),
            Command::LinkReplacement {
                policy: replacement.clone(),
                replaces: ic_core::PolicyId("ghost".into())
            }
        )),
        "unknown_policy"
    );
    assert_eq!(
        code(w.send(
            module("new-business"),
            Command::LinkReplacement {
                policy: ic_core::PolicyId("ghost".into()),
                replaces: original
            }
        )),
        "unknown_policy"
    );
    w.pay(&replacement, "r1");
    w.pay(&orphan, "r1");
    let total = w.payable("a-co") + w.payable("fap-one");
    assert!(total > 0 && total < ANNUAL / 10, "renewal only: {total}");
    assert!(w.dist.exposures.is_empty(), "no initial commission, so nothing to claw back");
    assert!(w.dist.allocations.values().all(|a| a.policy == replacement));
    w.assert_reconciled();
}

/// Purpose: when a customer appoints a new adviser, servicing always changes at once; whether
/// renewal commission follows is the insurer's setting: never, at once, or after a stand-down.
/// Why it matters: access follows the customer's choice by law of consent, but commission is
/// commercial. Mixing the two would either block the customer or move money without agreement.
#[test]
fn a_customers_change_of_adviser_moves_servicing_at_once_and_commission_by_the_setting() {
    let (mut w, policy, _) = advised();
    w.add_party("adviser-b", PartyKind::Adviser);
    w.add_party("b-co", PartyKind::Company);
    w.payee("b-co");
    w.engage("eng-b-two", "adviser-b", "fap-two", d(2020, 1, 1));
    let today = w.today();
    w.activate(
        "b-book",
        vec![
            Change::CreateBook {
                id: BookId::from("b-at-fap-two"),
                name: "B at FAP Two".into(),
                engagement: Some(EngagementId::from("eng-b-two")),
                owner: Some(party("fap-two")),
                house: false,
            },
            Change::ActivateRule(rule(
                "b-at-fap-two",
                1,
                today,
                vec![base("b-co", Share::OfRemainder(WHOLE), true)],
                vec![liable("fap-two")],
            )),
        ],
    )
    .unwrap();
    let appoint = |w: &mut World, engagement: Option<&str>, book: Option<&str>| {
        w.send(
            module("customer-app"),
            Command::AppointAdviser {
                policy: policy.clone(),
                engagement: engagement.map(EngagementId::from),
                book: book.map(BookId::from),
            },
        )
    };
    // Default setting: commission stays with the previous adviser.
    appoint(&mut w, Some("eng-b-two"), Some("b-at-fap-two")).unwrap();
    assert_eq!(w.dist.policies[&policy].servicing, Some(EngagementId::from("eng-b-two")));
    assert_eq!(w.dist.policies[&policy].adviser_change_setting, Some(1));
    w.days(31);
    w.pay(&policy, "r2");
    assert_eq!(w.payable("b-co"), 0);

    // Stand-down: a future-dated assignment, three months out.
    w.activate(
        "stand-down",
        vec![Change::AdviserChangeSetting(AdviserChangeSetting {
            version: 2,
            mode: AdviserChangeMode::StandDown { months: 3 },
        })],
    )
    .unwrap();
    assert_eq!(code(appoint(&mut w, Some("eng-b-two"), Some("a-at-fap-one"))), "wrong_book");
    assert_eq!(code(appoint(&mut w, Some("eng-b-two"), Some("ghost"))), "unknown_book");
    assert_eq!(code(appoint(&mut w, Some("ghost"), None)), "unknown_engagement");
    appoint(&mut w, Some("eng-b-two"), Some("b-at-fap-two")).unwrap();
    w.days(31);
    w.pay(&policy, "r3");
    assert_eq!(w.payable("b-co"), 0, "inside the stand-down the previous adviser is still paid");

    // At once.
    w.activate(
        "at-once",
        vec![Change::AdviserChangeSetting(AdviserChangeSetting {
            version: 3,
            mode: AdviserChangeMode::MovesAtOnce,
        })],
    )
    .unwrap();
    appoint(&mut w, Some("eng-b-two"), Some("b-at-fap-two")).unwrap();
    w.days(31);
    w.pay(&policy, "r4");
    assert!(w.payable("b-co") > 0);
    // The customer can also choose no adviser: the insurer services directly.
    appoint(&mut w, None, None).unwrap();
    assert_eq!(w.dist.policies[&policy].servicing, None);
    // The previous adviser's exposure stays theirs.
    assert_eq!(w.dist.clawback_exposure(w.today()), BTreeMap::from([(party("fap-one"), ANNUAL)]));
    let ghost = ic_core::PolicyId("ghost".into());
    assert_eq!(
        code(w.send(
            module("customer-app"),
            Command::AppointAdviser {
                policy: ghost,
                engagement: None,
                book: None
            }
        )),
        "unknown_policy"
    );
    w.assert_reconciled();
}

/// Purpose: the event relay may deliver an event twice or skip one. A duplicate is ignored, a
/// gap is refused with the sequence the module is waiting for, and a repeated command key
/// returns the first result without doing anything again.
/// Why it matters: delivery is at-least-once. Counting a premium twice pays commission twice;
/// silently skipping one loses it. Both must be impossible, not merely unlikely.
#[test]
fn duplicate_and_missing_events_cannot_double_or_lose_commission() {
    let mut w = worked_example();
    let (policy, partition) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", ANNUAL).unwrap();
    let p = w.cluster.locate(&policy).unwrap();
    w.cluster
        .submit(
            p,
            "cash-matching",
            CoreCommand::ApplyReceipt {
                policy: policy.clone(),
                receipt: "r1".into(),
                amount: ic_money::Money::new(w.owing(&policy), ic_money::Currency::NZD),
            },
        )
        .unwrap();
    let events = w.cluster.take_events();
    assert!(!events.is_empty());
    let ingest = |e: &ic_core::EventRecord| Command::Ingest(Box::new(e.clone()));
    // A later event first: refused, and nothing changes.
    if events.len() > 1 {
        assert_eq!(code(w.send(module("event-relay"), ingest(&events[1]))), "event_gap");
    } else {
        let mut skipped = events[0].clone();
        skipped.seq += 1;
        assert_eq!(code(w.send(module("event-relay"), ingest(&skipped))), "event_gap");
    }
    assert!(w.dist.allocations.is_empty());
    for e in &events {
        w.send(module("event-relay"), ingest(e)).unwrap();
    }
    for e in &events {
        assert!(w.send(module("event-relay"), ingest(e)).unwrap().is_empty(), "a second delivery does nothing");
    }
    assert_eq!(w.dist.allocations.len(), 1);
    assert_eq!(w.dist.checkpoints[&partition], events.last().unwrap().seq);
    // The driver lost its outbox: what is unconfirmed can be sent again, and posts once.
    assert_eq!(w.dist.unconfirmed().len(), 1);
    let again = w.dist.unconfirmed();
    w.deliver();
    w.outbox = again;
    w.deliver();
    assert_eq!(w.refused, vec![], "the same reference under the same key is the same command");
    w.sync();
    assert!(w.dist.requested.values().all(|r| matches!(r.state, PostingState::Posted { .. })));
    let total = w.payable("a-co") + w.payable("fap-one");
    // The same command key again returns the stored result and changes nothing.
    let first = w.send_keyed(
        "same-key",
        admin(),
        Command::AddParty {
            party: party("x"),
            kind: PartyKind::Team,
        },
    );
    assert!(first.is_ok());
    let out = w.dist.apply_envelope(
        999,
        ic_distribution::Envelope {
            key: "same-key".into(),
            actor: admin(),
            command: Command::AddParty {
                party: party("x"),
                kind: PartyKind::Team,
            },
        },
    );
    assert!(out.duplicate && out.result.is_ok() && out.events.is_empty());
    assert_eq!(w.payable("a-co") + w.payable("fap-one"), total);
    w.assert_reconciled();
}

/// Purpose: a replica that applies the same log reaches the same state hash, and the state
/// survives being written out and read back.
/// Why it matters: the module is replicated. Any dependence on a clock, on map ordering or on
/// what is not serialised would make replicas diverge, and a divergent replica pays differently.
#[test]
fn replaying_the_log_reaches_the_same_state_and_the_state_round_trips() {
    let (mut w, policy, _) = advised();
    w.licence("fap-one", ic_distribution::structure::LicenceState::Suspended, start()).unwrap();
    w.days(31);
    w.pay(&policy, "r2");
    let mut replica = ic_distribution::DistributionState::new(w.dist.settings.clone());
    for (index, envelope) in &w.log {
        use ic_log::StateMachine;
        replica.apply(*index, envelope.clone());
    }
    assert_eq!(replica.state_hash(), w.dist.state_hash());
    let restored: ic_distribution::DistributionState = serde_json::from_str(&serde_json::to_string(&w.dist).unwrap()).unwrap();
    assert_eq!(restored.state_hash(), w.dist.state_hash());
    assert_eq!(restored, w.dist);
}

/// Purpose: the dry run says what a premium would earn for each payee without changing
/// anything.
/// Why it matters: commission disclosure at the point of sale and the who-gains-who-loses view
/// of a proposed change both rest on it. A dry run that moved money would be a disaster.
#[test]
fn a_dry_run_shows_what_a_premium_would_earn_and_changes_nothing() {
    let (w, policy, _) = advised();
    let hash = w.dist.state_hash();
    let out = w.dist.dry_run(&policy, w.today(), &BTreeMap::from([("life".to_string(), 100_00)]));
    assert_eq!(out, BTreeMap::from([(party("a-co"), 7_50), (party("fap-one"), 2_50)]));
    assert!(w.dist.dry_run(&ic_core::PolicyId("ghost".into()), w.today(), &BTreeMap::new()).is_empty());
    assert_eq!(w.dist.state_hash(), hash);
}

/// Purpose: layers moved to a book whose rule is not yet in force raise an exception item and
/// a task instead of guessing who to pay, and the event stream keeps moving.
/// Why it matters: ingesting an event can never fail for a business reason, or one bad book
/// would stall commission for every policy on the partition.
#[test]
fn commission_that_cannot_be_worked_out_becomes_an_exception_and_does_not_stall_the_stream() {
    let (mut w, policy, _) = advised();
    let today = w.today();
    let later = today.checked_add_days(chrono::Days::new(60)).unwrap();
    w.activate(
        "early-move",
        vec![
            Change::CreateBook {
                id: BookId::from("late-book"),
                name: "Late".into(),
                engagement: None,
                owner: None,
                house: false,
            },
            Change::ActivateRule(rule("late-book", 1, later, vec![base("a-co", Share::OfRemainder(WHOLE), true)], vec![])),
            Change::MoveLayers {
                layers: vec![format!("{policy}/life/1")],
                to: vec![(BookId::from("late-book"), WHOLE)],
                from: today,
            },
        ],
    )
    .unwrap();
    let before = w.payable("a-co");
    w.days(31);
    w.pay(&policy, "r2");
    assert_eq!(w.payable("a-co"), before);
    assert!(w.events.iter().any(|e| matches!(e, Event::CommissionException { .. })));
    assert!(w.dist.tasks.values().any(|t| t.kind == "commission_exception" && t.done.is_none()));
    // Once the rule is in force the next premium pays, to whoever the rule names with no liability line.
    w.days(31);
    w.pay(&policy, "r3");
    assert!(w.payable("a-co") > before);
    w.assert_reconciled();
}

/// Purpose: a company paid on an adviser's behalf follows that adviser's standing, and the
/// structure refuses what would corrupt it.
/// Why it matters: suspension must reach the account the money actually goes to. An adviser
/// suspended in person but paid through a company would otherwise keep being paid.
#[test]
fn a_company_paid_for_an_adviser_is_held_when_the_adviser_is_suspended() {
    let (mut w, policy, _) = advised();
    w.send(
        admin(),
        Command::Relate(Relation {
            child: party("a-co"),
            parent: party("adviser-a"),
            kind: RelationKind::PayeeFor,
            from: d(2020, 1, 1),
            to: None,
        }),
    )
    .unwrap();
    w.days(1);
    let today = w.today();
    let events = w.licence("adviser-a", ic_distribution::structure::LicenceState::Suspended, today).unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::AuthorisationChanged { suspended_payees, .. } if suspended_payees.contains(&party("a-co")))));
    let (a_co, fap) = (w.payable("a-co"), w.payable("fap-one"));
    w.days(31);
    w.pay(&policy, "r2");
    assert_eq!(w.payable("a-co"), a_co);
    assert!(w.held("a-co") > 0);
    assert!(w.payable("fap-one") > fap, "the provider's own licence is fine");
    assert!(w
        .dist
        .allocations
        .values()
        .flat_map(|a| &a.lines)
        .any(|l| l.dest == Dest::Held(HeldReason::SuspendedLicence)));
    w.assert_reconciled();
}
