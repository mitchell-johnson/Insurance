//! The adviser and commission rows of the persona test matrix
//! (docs/testing/personas, area ADV) that no other test carries out.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use ic_core::{Command as CoreCommand, LifeId, PolicyId};
use ic_distribution::config::{BookOutcome, Change, MoveRequest};
use ic_distribution::rules::{portion, CommissionType, Component, Line, Share};
use ic_distribution::structure::PartyKind;
use ic_distribution::{BookId, EngagementId, Event};

/// What each payee was allocated on one layer since event `from`, by commission type.
fn on_layer(w: &World, from: usize, layer: &str) -> BTreeMap<(String, CommissionType), i64> {
    let mut out = BTreeMap::new();
    for e in &w.events[from..] {
        if let Event::CommissionEarned { reference, .. } = e {
            for l in &w.dist.allocations[reference].lines {
                if l.layer.as_deref() == Some(layer) {
                    if let Some(t) = l.commission_type {
                        *out.entry((l.payee.0.clone(), t)).or_insert(0) += l.amount_minor;
                    }
                }
            }
        }
    }
    out
}

fn layer(policy: &PolicyId, n: u32) -> String {
    format!("{policy}/life/{n}")
}

/// Adviser A moves from FAP One to FAP Two taking the book, with FAP One
/// keeping a quarter of renewal commission as a tail. FAP Two also gives A a
/// book of A's own for new business there, with no tail.
fn moved() -> (World, PolicyId) {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.pay(&policy, "r1");
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
            new_book_name: "A's book from FAP One".into(),
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
    let mut changes = w.dist.plan_move(&request).unwrap();
    changes.extend([
        Change::CreateBook {
            id: BookId::from("a-own-at-fap-two"),
            name: "A at FAP Two".into(),
            engagement: Some(EngagementId::from("eng-a-two")),
            owner: Some(party("fap-two")),
            house: false,
        },
        Change::ActivateRule(rule(
            "a-own-at-fap-two",
            1,
            on,
            vec![
                base("a-co", Share::OfRemainder(pct(75.0)), true),
                base("fap-two", Share::OfRemainder(pct(25.0)), false),
            ],
            vec![liable("fap-two")],
        )),
    ]);
    w.activate("move-a", changes).unwrap();
    (w, policy)
}

/// Purpose (ADV-005): after A has moved, the customer increases cover. The increase is a new
/// layer written under A's new engagement into A's own book at FAP Two: its initial and renewal
/// commission and its clawback belong to the new arrangement, and FAP One's tail is paid only
/// on the original layer.
/// Why it matters: a tail is what the previous provider negotiated for the book it built. It
/// must not reach business the adviser writes afterwards, and the new provider must bear the
/// clawback on commission it was paid.
#[test]
fn an_increase_after_a_move_belongs_to_the_new_arrangement_and_pays_no_tail() {
    let (mut w, policy) = moved();
    w.core(
        &policy,
        "underwriting",
        CoreCommand::AddUnderwrittenLayer(Box::new(ic_core::command::UnderwrittenLayer {
            policy: policy.clone(),
            cover: "life".into(),
            life: LifeId("life-1".into()),
            sum_insured_minor: 100_000_00,
            loadings: vec![],
            exclusions: vec![],
            reference: "uw-increase".into(),
        })),
    )
    .unwrap();
    assert!(w.dist.layers[&layer(&policy, 2)].terms.is_none(), "an increase earns nothing until written");
    w.write(&policy, "life", 2, "eng-a-two", "a-own-at-fap-two", 240_00).unwrap();
    w.days(31);
    let before = w.events.len();
    w.pay(&policy, "r2");

    let increase = on_layer(&w, before, &layer(&policy, 2));
    assert!(increase.contains_key(&("a-co".into(), CommissionType::Initial)), "{increase:?}");
    assert!(increase.contains_key(&("fap-two".into(), CommissionType::Initial)));
    assert!(increase.keys().all(|(who, _)| who != "fap-one"), "no tail on the increase: {increase:?}");
    let original = on_layer(&w, before, &layer(&policy, 1));
    let renewal: i64 = original.values().sum();
    assert_eq!(
        original[&("fap-one".into(), CommissionType::Renewal)],
        portion(renewal, pct(25.0)),
        "the tail stays on the original"
    );
    let exposures: Vec<_> = w.dist.exposures.values().filter(|e| e.layer == layer(&policy, 2)).collect();
    assert!(!exposures.is_empty() && exposures.iter().all(|e| e.liable.iter().all(|(p, ..)| p == &party("fap-two"))));
    w.assert_reconciled();
}

/// Purpose (ADV-008): A moves a second time, from FAP Two to FAP Three, while FAP One's tail is
/// still running. FAP Two keeps a tail of its own until a date. Both tails are lines on the
/// whole renewal commission with their own end dates; A and FAP Three share what is left; and
/// FAP Two's tail drops out on its date while FAP One's carries on.
/// Why it matters: a book that moves twice carries two providers' tails. If the second move
/// replaced the first tail, FAP One would lose what it was promised; if the tails were taken
/// from the remainder in turn, the adviser would be paid the wrong amount.
#[test]
fn a_second_move_carries_both_tails_each_ending_on_its_own_date() {
    let (mut w, policy) = moved();
    w.add_party("fap-three", PartyKind::Fap);
    w.payee("fap-three");
    w.account("fap-three", "acct-fap-three");
    w.days(31);
    w.pay(&policy, "r2");
    let on = w.today();
    let fap_two_until = on.checked_add_days(chrono::Days::new(45)).unwrap();
    let first_tail = Line {
        component: Component::Base,
        commission_type: Some(CommissionType::Renewal),
        share: Share::OfComponent(pct(25.0)),
        ..base("fap-one", Share::OfRemainder(1), false)
    };
    let request = MoveRequest {
        from_engagement: EngagementId::from("eng-a-two"),
        new_engagement: EngagementId::from("eng-a-three"),
        to: party("fap-three"),
        on,
        accreditations: BTreeSet::from(["term-life".to_string()]),
        books: vec![BookOutcome::Moves {
            book: BookId::from("a-at-fap-two"),
            policies: None,
            new_book: BookId::from("a-at-fap-three"),
            new_book_name: "A's book at FAP Three".into(),
            release_ref: "release-signed-by-fap-two".into(),
            lines: vec![
                first_tail,
                base("a-co", Share::OfRemainder(pct(75.0)), true),
                base("fap-three", Share::OfRemainder(pct(25.0)), false),
            ],
            liability: vec![liable("fap-three")],
            fragments: vec![],
            tail: Some((party("fap-two"), pct(10.0), Some(fap_two_until))),
            examples: vec![],
        }],
    };
    let changes = w.dist.plan_move(&request).unwrap();
    w.activate("move-a-again", changes).unwrap();

    w.days(31);
    let before = w.events.len();
    w.pay(&policy, "r3");
    let paid = on_layer(&w, before, &layer(&policy, 1));
    let whole: i64 = paid.values().sum();
    let renewal = |who: &str| paid.get(&(who.to_string(), CommissionType::Renewal)).copied().unwrap_or(0);
    assert_eq!(renewal("fap-one"), portion(whole, pct(25.0)), "{paid:?}");
    assert_eq!(renewal("fap-two"), portion(whole, pct(10.0)));
    let rest = whole - renewal("fap-one") - renewal("fap-two");
    assert_eq!(renewal("fap-three"), portion(rest, pct(25.0)));
    assert_eq!(renewal("a-co"), rest - renewal("fap-three"));

    // The next month's premium falls due after FAP Two's tail has ended.
    w.days(31);
    assert!(w.today() > fap_two_until);
    let before = w.events.len();
    w.pay(&policy, "r4");
    let paid = on_layer(&w, before, &layer(&policy, 1));
    let whole: i64 = paid.values().sum();
    assert!(
        !paid.contains_key(&("fap-two".into(), CommissionType::Renewal)),
        "FAP Two's tail ended: {paid:?}"
    );
    assert_eq!(
        paid[&("fap-one".into(), CommissionType::Renewal)],
        portion(whole, pct(25.0)),
        "FAP One's carries on"
    );
    w.assert_reconciled();
}

/// Adviser B at FAP One, with a book of B's own.
fn with_adviser_b(w: &mut World) {
    w.add_party("adviser-b", PartyKind::Adviser);
    w.add_party("b-co", PartyKind::Company);
    w.payee("b-co");
    w.account("b-co", "acct-b-co");
    w.engage("eng-b-one", "adviser-b", "fap-one", d(2020, 1, 1));
    let start = d(START.0, START.1, START.2);
    w.activate(
        "b-book",
        vec![
            Change::CreateBook {
                id: BookId::from("b-at-fap-one"),
                name: "B at FAP One".into(),
                engagement: Some(EngagementId::from("eng-b-one")),
                owner: Some(party("fap-one")),
                house: false,
            },
            Change::ActivateRule(rule(
                "b-at-fap-one",
                1,
                start,
                vec![
                    base("b-co", Share::OfRemainder(pct(80.0)), true),
                    base("fap-one", Share::OfRemainder(pct(20.0)), false),
                ],
                vec![liable("fap-one")],
            )),
        ],
    )
    .unwrap();
}

/// Purpose (ADV-010): adviser B adds trauma cover to a policy adviser A wrote. The added cover
/// is a layer of its own, written under B's engagement into B's book; each premium is split
/// across the layers, A's book earning on the life cover and B's on the trauma cover only.
/// Why it matters: a second adviser servicing a customer must be paid for what they wrote
/// without touching the first adviser's commission on the cover that was already there.
#[test]
fn a_cover_another_adviser_adds_earns_in_that_advisers_book_only() {
    let mut w = worked_example();
    with_adviser_b(&mut w);
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.pay(&policy, "r1");
    w.core(
        &policy,
        "underwriting",
        CoreCommand::AddCover(Box::new(ic_core::command::AddedCover {
            policy: policy.clone(),
            cover: "trauma".into(),
            life: LifeId("life-1".into()),
            sum_insured_minor: 100_000_00,
            loadings: vec![],
            exclusions: vec![],
            reference: "uw-trauma".into(),
        })),
    )
    .unwrap();
    let trauma = format!("{policy}/trauma/1");
    assert!(w.dist.layers.contains_key(&trauma), "the added cover is a layer of its own");
    w.write(&policy, "trauma", 1, "eng-b-one", "b-at-fap-one", 360_00).unwrap();
    w.days(31);
    let before = w.events.len();
    w.pay(&policy, "r2");
    let on_trauma = on_layer(&w, before, &trauma);
    let on_life = on_layer(&w, before, &layer(&policy, 1));
    assert!(on_trauma.contains_key(&("b-co".into(), CommissionType::Initial)), "{on_trauma:?}");
    assert!(on_trauma.keys().all(|(who, _)| who == "b-co" || who == "fap-one"));
    assert!(
        on_life
            .keys()
            .all(|(who, t)| (who == "a-co" || who == "fap-one") && *t == CommissionType::Renewal),
        "{on_life:?}"
    );
    assert!(on_life.contains_key(&("a-co".into(), CommissionType::Renewal)));
    w.assert_reconciled();
}

/// Purpose (ADV-011): one adviser holds two engagements at once, with FAP One and with FAP Two.
/// Each policy lands in the book of the engagement it was written under, and nothing is shared
/// between the two.
/// Why it matters: advisers do work for two providers. Commission written under one provider's
/// licence and paid through the other's book would breach both agreements.
#[test]
fn an_adviser_with_two_engagements_earns_each_policy_in_the_book_it_was_written_under() {
    let mut w = worked_example();
    w.engage("eng-a-two", "adviser-a", "fap-two", d(2020, 1, 1));
    let start = d(START.0, START.1, START.2);
    w.activate(
        "a-two",
        vec![
            Change::CreateBook {
                id: BookId::from("a-at-fap-two"),
                name: "A at FAP Two".into(),
                engagement: Some(EngagementId::from("eng-a-two")),
                owner: Some(party("fap-two")),
                house: false,
            },
            Change::ActivateRule(rule(
                "a-at-fap-two",
                1,
                start,
                vec![
                    base("a-co", Share::OfRemainder(pct(60.0)), true),
                    base("fap-two", Share::OfRemainder(pct(40.0)), false),
                ],
                vec![liable("fap-two")],
            )),
        ],
    )
    .unwrap();
    let (one, _) = w.issue(1, 500_000_00, None);
    let (two, _) = w.issue(2, 500_000_00, None);
    w.write(&one, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.write(&two, "life", 1, "eng-a-two", "a-at-fap-two", 1_200_00).unwrap();
    // Writing under one engagement into the other's book is refused.
    let (three, _) = w.issue(3, 500_000_00, None);
    assert_eq!(code(w.write(&three, "life", 1, "eng-a-one", "a-at-fap-two", 1_200_00)), "book_not_writers");
    let before = w.events.len();
    w.pay(&one, "o1");
    w.pay(&two, "t1");
    let first = on_layer(&w, before, &layer(&one, 1));
    let second = on_layer(&w, before, &layer(&two, 1));
    assert!(first.keys().all(|(who, _)| who == "a-co" || who == "fap-one"), "{first:?}");
    assert!(second.keys().all(|(who, _)| who == "a-co" || who == "fap-two"), "{second:?}");
    let a_first: i64 = first.iter().filter(|((w, _), _)| w == "a-co").map(|(_, v)| *v).sum();
    let a_second: i64 = second.iter().filter(|((w, _), _)| w == "a-co").map(|(_, v)| *v).sum();
    assert!(a_first > a_second, "75% under FAP One against 60% under FAP Two");
    w.assert_reconciled();
}

/// Purpose (ADV-043): a policy bought direct is written to no book and earns nothing however
/// many premiums it pays; it shows in no adviser's commission and raises no clawback when
/// cancelled.
/// Why it matters: direct business has no adviser to pay. Commission accrued on it would be an
/// expense with no one entitled to it, and a clawback on it would chase money never paid.
#[test]
fn direct_business_sits_in_no_book_and_earns_nothing() {
    let mut w = worked_example();
    let (direct, _) = w.issue(1, 500_000_00, None);
    let before = w.events.len();
    w.pay(&direct, "d1");
    w.days(31);
    w.pay(&direct, "d2");
    assert!(w.dist.layers[&layer(&direct, 1)].books_on(w.today()).is_empty());
    assert!(!w.events[before..].iter().any(|e| matches!(e, Event::CommissionEarned { .. })));
    w.core(&direct, "policy-servicing", CoreCommand::CancelPolicy { policy: direct.clone() })
        .unwrap();
    assert!(!w.events[before..].iter().any(|e| matches!(e, Event::ClawbackRaised { .. })));
    assert_eq!(w.payable("a-co") + w.payable("fap-one"), 0);
    w.assert_reconciled();
}
