//! The adviser and commission rows of the persona test matrix
//! (docs/testing/personas, area ADV) that no other test carries out.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use ic_core::{Command as CoreCommand, LifeId, PolicyId};
use ic_distribution::config::{BookOutcome, Change, MoveRequest};
use ic_distribution::rules::{portion, CommissionType, Component, Line, Share, WHOLE};
use ic_distribution::state::WriteLayer;
use ic_distribution::structure::PartyKind;
use ic_distribution::{BookId, Command, EngagementId, Event};

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

/// Purpose: (ADV-005) after A has moved, the customer increases cover. The increase is a new
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

/// Purpose: (ADV-008) A moves a second time, from FAP Two to FAP Three, while FAP One's tail is
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

/// Purpose: (ADV-010) adviser B adds trauma cover to a policy adviser A wrote. The added cover
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
            excess_option: None,
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

/// Purpose: (ADV-011) one adviser holds two engagements at once, with FAP One and with FAP Two.
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

/// Purpose: (ADV-043) a policy bought direct is written to no book and earns nothing however
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

/// Purpose: (ADV-009) A and B wrote a policy together, 60 and 40, each into their own book. When
/// A moves to FAP Two with the book, only A's share of the layer moves to A's new book under the
/// new rule; B's book, its rule and B's commission are untouched.
/// Why it matters: a joint policy is two advisers' business. Moving it whole with one of them
/// would hand B's trail to A's new provider.
#[test]
fn a_joint_policy_moves_only_the_share_of_the_adviser_who_leaves() {
    let mut w = worked_example();
    with_adviser_b(&mut w);
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.send(
        module("new-business"),
        Command::WriteLayer(Box::new(WriteLayer {
            policy: policy.clone(),
            cover: "life".into(),
            layer: 1,
            writing: vec![(EngagementId::from("eng-a-one"), pct(60.0)), (EngagementId::from("eng-b-one"), pct(40.0))],
            books: vec![(BookId::from("a-at-fap-one"), pct(60.0)), (BookId::from("b-at-fap-one"), pct(40.0))],
            schedule: "standard".into(),
            option: "upfront".into(),
            annual_premium_minor: 1_200_00,
            replacement: false,
        })),
    )
    .unwrap();
    w.pay(&policy, "r1");
    w.days(31);
    let on = w.today();
    let b_rule = w.dist.config.books[&BookId::from("b-at-fap-one")].clone();
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
            tail: None,
            examples: vec![],
        }],
    };
    let changes = w.dist.plan_move(&request).unwrap();
    w.activate("move-a", changes).unwrap();
    let key = layer(&policy, 1);
    assert_eq!(
        w.dist.layers[&key].books_on(on).to_vec(),
        vec![(BookId::from("a-at-fap-two"), pct(60.0)), (BookId::from("b-at-fap-one"), pct(40.0))]
    );
    assert_eq!(w.dist.config.books[&BookId::from("b-at-fap-one")], b_rule, "B's book is untouched");

    let before = w.events.len();
    w.pay(&policy, "r2");
    let paid = on_layer(&w, before, &key);
    let total: i64 = paid.values().sum();
    let b_side = paid[&("b-co".into(), CommissionType::Renewal)] + portion(portion(total, pct(40.0)), pct(20.0));
    assert!((b_side * 10 - total * 4).abs() <= 10, "B's book still takes 40%: {paid:?}");
    assert!(paid.contains_key(&("fap-two".into(), CommissionType::Renewal)));
    w.assert_reconciled();
}

/// Purpose: (ADV-012) a retiring adviser's book is split between three successors by file. Each
/// successor's share of the policies moves into a new book of the successor's carrying the
/// retiring adviser's tail; the successors' existing books, and new business they write there,
/// pay no tail.
/// Why it matters: a retirement sale is priced on the tail. Paid on the wrong policies, the
/// retiring adviser is overpaid out of the successors' own business; missed, they lose what
/// they sold the book for.
#[test]
fn a_retiring_advisers_book_split_between_three_successors_carries_the_tail() {
    let mut w = worked_example();
    let today = w.today();
    let mut policies = vec![];
    for n in 1..=3 {
        let (p, _) = w.issue(n, 500_000_00, None);
        w.write(&p, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
        w.pay(&p, &format!("r{n}"));
        policies.push(p);
    }
    let mut changes = vec![];
    for (n, successor) in ["s1", "s2", "s3"].into_iter().enumerate() {
        let (adviser, co, engagement) = (format!("adviser-{successor}"), format!("{successor}-co"), format!("eng-{successor}"));
        w.add_party(&adviser, PartyKind::Adviser);
        w.add_party(&co, PartyKind::Company);
        w.payee(&co);
        w.account(&co, &format!("acct-{co}"));
        w.engage(&engagement, &adviser, "fap-one", d(2020, 1, 1));
        let lines = |tail: bool| {
            let mut l = vec![
                base(&co, Share::OfRemainder(pct(75.0)), true),
                base("fap-one", Share::OfRemainder(pct(25.0)), false),
            ];
            if tail {
                l.push(Line {
                    commission_type: Some(CommissionType::Renewal),
                    share: Share::OfComponent(pct(20.0)),
                    ..base("a-co", Share::OfRemainder(1), false)
                });
            }
            l
        };
        for (book, tail) in [(format!("{successor}-own"), false), (format!("{successor}-from-a"), true)] {
            changes.push(Change::CreateBook {
                id: BookId::from(book.as_str()),
                name: book.clone(),
                engagement: Some(EngagementId::from(engagement.as_str())),
                owner: Some(party("fap-one")),
                house: false,
            });
            changes.push(Change::ActivateRule(rule(&book, 1, today, lines(tail), vec![liable("fap-one")])));
        }
        changes.push(Change::MoveLayers {
            layers: vec![layer(&policies[n], 1)],
            to: vec![(BookId::from(format!("{successor}-from-a").as_str()), WHOLE)],
            from: today,
        });
    }
    w.activate("retirement", changes).unwrap();
    // Each successor also writes new business of their own.
    let (own, _) = w.issue(9, 500_000_00, None);
    w.write(&own, "life", 1, "eng-s1", "s1-own", 1_200_00).unwrap();

    w.days(31);
    let before = w.events.len();
    for (n, p) in policies.iter().enumerate() {
        w.pay(p, &format!("m{n}"));
    }
    w.pay(&own, "own1");
    for (n, p) in policies.iter().enumerate() {
        let paid = on_layer(&w, before, &layer(p, 1));
        let whole: i64 = paid.values().sum();
        assert_eq!(paid[&("a-co".into(), CommissionType::Renewal)], portion(whole, pct(20.0)), "the tail on {p}");
        assert!(paid.contains_key(&(format!("s{}-co", n + 1), CommissionType::Renewal)));
    }
    let paid = on_layer(&w, before, &layer(&own, 1));
    assert!(paid.keys().all(|(who, _)| who != "a-co"), "no tail on the successor's own business: {paid:?}");
    w.assert_reconciled();
}

/// Purpose: (ADV-015) a cancellation backdated to before A's move hands back premium for months
/// paid both before and after the move. Each month handed back takes back exactly the lines
/// its own premium recorded: the month earned under FAP One's rule comes back from A's company
/// and FAP One, and the months earned after the move come back from A's company, FAP Two and
/// FAP One's tail, whatever the rule in force today says.
/// Why it matters: commission is reversed against what was paid, not recalculated. Recomputing
/// a pre-move month under the new rule would take money from FAP Two that FAP One received.
#[test]
fn premium_handed_back_across_a_move_reverses_the_lines_each_month_recorded() {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.pay(&policy, "r1");
    let unwind_after = w.today() + chrono::Duration::days(20);
    let who = ["a-co", "fap-one", "fap-two"];
    let balances = |w: &World| who.map(|p| w.payable(p) + w.held(p));
    let after_r1 = balances(&w);
    w.days(31);
    w.pay(&policy, "r2");
    let before_move = balances(&w);
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
    let after_move = balances(&w);
    w.days(31);
    w.pay(&policy, "r3");
    let after_r3 = balances(&w);
    let earned_before: Vec<i64> = (0..3).map(|i| before_move[i] - after_r1[i]).collect();
    let earned_after: Vec<i64> = (0..3).map(|i| after_r3[i] - after_move[i]).collect();
    assert_eq!(earned_before[2], 0, "FAP Two earned nothing before the move");
    assert!(earned_after.iter().all(|e| *e > 0), "all three earned after the move: {earned_after:?}");

    w.core(
        &policy,
        "remediation",
        CoreCommand::UnwindAfter {
            policy: policy.clone(),
            after: unwind_after,
            cause: ic_core::UnwindCause::Remediation,
            reference: "backdated-cancellation".into(),
        },
    )
    .unwrap();
    let now = balances(&w);
    for i in 0..3 {
        assert_eq!(
            after_r3[i] - now[i],
            earned_before[i] + earned_after[i],
            "{} gives back exactly what the two months paid it",
            who[i]
        );
    }
    w.assert_reconciled();
}

/// Purpose: (ADV-018) a layer was written on the upfront option when the adviser chose level,
/// and it is found a year later. An approved change corrects the layer's terms from its start:
/// the initial commission paid comes back, every month's renewal is worked out again at the
/// level rate, the differences post today and commission expense changes. Afterwards every
/// payee stands exactly where they would if the layer had been written on level from the
/// start, the clawback exposure on the initial commission is gone, and the next premium earns
/// at the level rate.
/// Why it matters: a keying error on commission terms is found at the adviser's annual review.
/// Fixing it by hand means a year of lines recalculated in a spreadsheet; fixing it here keeps
/// every line traceable to the premium that earned it.
#[test]
fn a_wrong_commission_option_found_a_year_later_is_corrected_from_the_start() {
    let year = |option: &str| {
        let mut w = worked_example();
        let (policy, _) = w.issue(1, 500_000_00, None);
        w.send(
            module("new-business"),
            Command::WriteLayer(Box::new(WriteLayer {
                policy: policy.clone(),
                cover: "life".into(),
                layer: 1,
                writing: vec![(EngagementId::from("eng-a-one"), WHOLE)],
                books: vec![(BookId::from("a-at-fap-one"), WHOLE)],
                schedule: "standard".into(),
                option: option.into(),
                annual_premium_minor: 1_200_00,
                replacement: false,
            })),
        )
        .unwrap();
        w.pay(&policy, "r0");
        for n in 1..12 {
            w.days(31);
            w.pay(&policy, &format!("r{n}"));
        }
        (w, policy)
    };
    let (right, _) = year("level");
    let (mut w, policy) = year("upfront");
    let key = layer(&policy, 1);
    let expense = |w: &World| w.payable("a-co") + w.payable("fap-one");
    let wrong = expense(&w);
    assert!(wrong != expense(&right));
    assert!(w.dist.exposures.values().any(|e| e.layer == key && e.amount_minor > 0));

    let correct = |option: &str| {
        vec![Change::CorrectTerms {
            layer: key.clone(),
            option: option.into(),
        }]
    };
    assert_eq!(code(w.propose("same", correct("upfront"))), "nothing_to_correct");
    assert_eq!(code(w.propose("bad", correct("monthly"))), "unknown_option");
    w.activate("fix-option", correct("level")).unwrap();
    for who in ["a-co", "fap-one"] {
        assert!(
            (w.payable(who) - right.payable(who)).abs() <= 12,
            "{who}: {} against {}",
            w.payable(who),
            right.payable(who)
        );
    }
    assert!(w.dist.exposures.values().filter(|e| e.layer == key).all(|e| e.amount_minor == 0 && !e.open));
    assert_eq!(w.dist.layers[&key].terms.as_ref().unwrap().option, "level");
    assert!(w.dist.allocations.values().any(|a| a.kind == "commission_corrected" && a.date == w.today()));

    let before = expense(&w);
    w.days(31);
    w.pay(&policy, "r12");
    let mut level = right;
    let level_before = expense(&level);
    level.days(31);
    level.pay(&policy, "r12");
    assert_eq!(expense(&w) - before, expense(&level) - level_before, "the next premium earns at the level rate");
    w.assert_reconciled();
}

/// Purpose: (ADV-007, ADV-045) FAP One leaves dealer group Old for dealer group New. The change
/// is one approved change set: the relation moves on a date and New's override fragment is
/// activated once. Books that existed at the change keep following Old's fragment, so Old keeps
/// its override on them; a book created afterwards follows New's fragment, and Old receives
/// nothing on it.
/// Why it matters: an aggregator's override on existing business is what it negotiated when the
/// provider joined. Moving it automatically would take Old's income; leaving new books on Old's
/// fragment would pay Old for business written after the provider left.
#[test]
fn a_provider_changing_aggregator_keeps_the_old_override_on_existing_books_only() {
    let mut w = worked_example();
    let today = w.today();
    w.add_party("group-new", PartyKind::Group);
    let under_old = ic_distribution::structure::Relation {
        child: party("fap-one"),
        parent: party("group-old"),
        kind: ic_distribution::structure::RelationKind::Under,
        from: d(2020, 1, 1),
        to: None,
    };
    w.send(admin(), Command::Relate(under_old)).unwrap();
    w.payee("group-new");
    w.account("group-new", "acct-group-new");
    let override_for = |group: &str, fragment: &str, from| ic_distribution::rules::Fragment {
        id: ic_distribution::FragmentId::from(fragment),
        version: 1,
        effective_from: from,
        lines: vec![Line {
            component: Component::Override("group".into()),
            share: Share::RateOnPremium(pct(2.0)),
            ..base(group, Share::OfRemainder(1), false)
        }],
        liability: vec![],
    };
    let rule_with = |book: &str, version, fragment: &str, from| ic_distribution::rules::Rule {
        fragments: vec![ic_distribution::FragmentId::from(fragment)],
        ..rule(
            book,
            version,
            from,
            vec![
                base("a-co", Share::OfRemainder(pct(75.0)), true),
                base("fap-one", Share::OfRemainder(pct(25.0)), false),
            ],
            vec![liable("fap-one")],
        )
    };
    w.activate(
        "old-group",
        vec![
            Change::ActivateFragment(override_for("group-old", "old-override", today)),
            Change::ActivateRule(rule_with("a-at-fap-one", 2, "old-override", today)),
        ],
    )
    .unwrap();
    let (existing, _) = w.issue(1, 500_000_00, None);
    w.write(&existing, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.pay(&existing, "e1");

    w.days(10);
    let change = w.today();
    w.activate(
        "fap-one-joins-new",
        vec![
            Change::EndRelation {
                child: party("fap-one"),
                parent: party("group-old"),
                kind: ic_distribution::structure::RelationKind::Under,
                on: change,
            },
            Change::Relate(ic_distribution::structure::Relation {
                child: party("fap-one"),
                parent: party("group-new"),
                kind: ic_distribution::structure::RelationKind::Under,
                from: change,
                to: None,
            }),
            Change::ActivateFragment(override_for("group-new", "new-override", change)),
            Change::CreateBook {
                id: BookId::from("a-at-fap-one-2"),
                name: "A at FAP One, written after the change".into(),
                engagement: Some(EngagementId::from("eng-a-one")),
                owner: Some(party("fap-one")),
                house: false,
            },
            Change::ActivateRule(rule_with("a-at-fap-one-2", 1, "new-override", change)),
        ],
    )
    .unwrap();
    let (later, _) = w.issue(2, 500_000_00, None);
    w.write(&later, "life", 1, "eng-a-one", "a-at-fap-one-2", 1_200_00).unwrap();
    w.days(21);
    let before = w.events.len();
    w.pay(&existing, "e2");
    w.pay(&later, "l1");
    let override_on = |p: &PolicyId| -> BTreeMap<String, i64> {
        let mut out = BTreeMap::new();
        for e in &w.events[before..] {
            if let Event::CommissionEarned { reference, .. } = e {
                for l in w.dist.allocations[reference]
                    .lines
                    .iter()
                    .filter(|l| l.layer.as_deref() == Some(layer(p, 1).as_str()))
                {
                    if matches!(l.component, Some(Component::Override(_))) {
                        *out.entry(l.payee.0.clone()).or_insert(0) += l.amount_minor;
                    }
                }
            }
        }
        out
    };
    let on_existing = override_on(&existing);
    let on_later = override_on(&later);
    assert!(
        on_existing.get("group-old").is_some_and(|v| *v > 0) && !on_existing.contains_key("group-new"),
        "{on_existing:?}"
    );
    assert!(
        on_later.get("group-new").is_some_and(|v| *v > 0) && !on_later.contains_key("group-old"),
        "{on_later:?}"
    );
    w.assert_reconciled();
}

/// Purpose: (ADV-040) a retired adviser receives a tail through a line in their own name. The
/// adviser dies; the engagement had already ended, and the agreement says the entitlement
/// passes to the estate. A new version of the rule from the date of death names the estate:
/// renewal before the death stays with the adviser, renewal after it goes to the estate, and
/// nothing else in the book changes.
/// Why it matters: a death must not stop money the agreement says is owed, nor keep paying a
/// person who has died into an account nobody may operate.
#[test]
fn a_retired_advisers_tail_passes_to_the_estate_from_the_date_of_death() {
    let mut w = worked_example();
    w.add_party("estate-a", PartyKind::Counterparty);
    w.payee("estate-a");
    w.account("estate-a", "acct-estate-a");
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.pay(&policy, "r1");
    w.days(31);
    let death = w.today();
    w.activate(
        "estate",
        vec![Change::ActivateRule(rule(
            "a-at-fap-one",
            2,
            death,
            vec![
                base("estate-a", Share::OfRemainder(pct(75.0)), true),
                base("fap-one", Share::OfRemainder(pct(25.0)), false),
            ],
            vec![liable("fap-one")],
        ))],
    )
    .unwrap();
    let a_before = w.payable("a-co");
    let before = w.events.len();
    w.pay(&policy, "r2");
    let paid = on_layer(&w, before, &layer(&policy, 1));
    assert!(paid.contains_key(&("estate-a".into(), CommissionType::Renewal)), "{paid:?}");
    assert!(!paid.contains_key(&("a-co".into(), CommissionType::Renewal)));
    assert_eq!(w.payable("a-co"), a_before, "what the adviser earned before stays theirs");
    w.assert_reconciled();
}

/// Purpose: (ADV-041) an authorised body licensed through FAP One gains its own licence. From
/// the date, one change set ends the licensing relation, and the body's own licence is
/// recorded; the adviser's engagement with the body, the body's book and its payee record are
/// unchanged, and the book's rule drops FAP One's override in one new version. Afterwards a
/// licence problem at FAP One no longer stops the body's adviser writing business or being paid.
/// Why it matters: the licence cascade walks the dated structure. If the body stayed under FAP
/// One after licensing itself, FAP One losing its licence would wrongly stop advisers who no
/// longer depend on it; if FAP One's override stayed, it would be paid on business it no longer
/// supervises.
#[test]
fn an_authorised_body_that_gains_its_own_licence_leaves_its_providers_cascade() {
    use ic_distribution::structure::{LicenceState, Relation, RelationKind};
    let mut w = worked_example();
    let start = d(START.0, START.1, START.2);
    for (id, kind) in [("adviser-x", PartyKind::Adviser), ("ab-one", PartyKind::AuthorisedBody)] {
        w.add_party(id, kind);
    }
    w.payee("ab-one");
    w.account("ab-one", "acct-ab-one");
    let licensed_by = Relation {
        child: party("ab-one"),
        parent: party("fap-one"),
        kind: RelationKind::LicensedBy,
        from: d(2020, 1, 1),
        to: None,
    };
    w.send(admin(), Command::Relate(licensed_by)).unwrap();
    w.engage("eng-x-ab", "adviser-x", "ab-one", d(2020, 1, 1));
    let lines = || {
        vec![
            base("ab-one", Share::OfRemainder(pct(100.0)), true),
            Line {
                component: Component::Override("provider".into()),
                share: Share::RateOnPremium(pct(1.0)),
                ..base("fap-one", Share::OfRemainder(1), false)
            },
        ]
    };
    w.activate(
        "ab-book",
        vec![
            Change::CreateBook {
                id: BookId::from("x-at-ab"),
                name: "X at the authorised body".into(),
                engagement: Some(EngagementId::from("eng-x-ab")),
                owner: Some(party("ab-one")),
                house: false,
            },
            Change::ActivateRule(rule("x-at-ab", 1, start, lines(), vec![liable("ab-one")])),
        ],
    )
    .unwrap();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-x-ab", "x-at-ab", 1_200_00).unwrap();
    w.pay(&policy, "r1");
    let book_before = w.dist.config.books[&BookId::from("x-at-ab")].engagement.clone();

    w.days(10);
    let on = w.today();
    let mut own = lines();
    own.pop();
    w.activate(
        "ab-licensed",
        vec![
            Change::EndRelation {
                child: party("ab-one"),
                parent: party("fap-one"),
                kind: RelationKind::LicensedBy,
                on,
            },
            Change::ActivateRule(rule("x-at-ab", 2, on, own, vec![liable("ab-one")])),
        ],
    )
    .unwrap();
    w.licence("ab-one", LicenceState::Active, on).unwrap();
    assert_eq!(w.dist.config.books[&BookId::from("x-at-ab")].engagement, book_before, "the book is unchanged");
    assert!(w.dist.payees.contains_key(&party("ab-one")), "and so is the payee");

    // FAP One then loses its licence: the body's adviser is not caught by it.
    w.days(1);
    let today = w.today();
    w.licence("fap-one", LicenceState::Suspended, today).unwrap();
    let structure = &w.dist.config.structure;
    assert!(structure.may_advise(&EngagementId::from("eng-x-ab"), "term-life", today).is_ok());
    assert!(structure.payee_impairment(&party("ab-one"), today).is_none());
    let (later, _) = w.issue(2, 500_000_00, None);
    w.write(&later, "life", 1, "eng-x-ab", "x-at-ab", 1_200_00).unwrap();
    w.days(20);
    let before = w.events.len();
    w.pay(&policy, "r2");
    let paid = on_layer(&w, before, &layer(&policy, 1));
    assert!(paid.keys().all(|(who, _)| who == "ab-one"), "no override to FAP One after the change: {paid:?}");
    assert_eq!(w.held("ab-one"), 0, "nothing held for a licence problem that is not the body's");
    w.assert_reconciled();
}

/// Purpose: (ADV-030) a group scheme's cover is formula-driven per member, so the module keeps
/// one layer for each scheme cover that the scheme's broker writes. Mid-year the employer
/// appoints a new broker: from the appointment date the scheme's layer moves to the new
/// broker's book, whose rule carries the outgoing broker's agreed share as a dated line. Before
/// the date the old broker is paid alone; between the date and the end of the handover share
/// the two split it; after that the new broker is paid alone.
/// Why it matters: scheme commission is paid on the whole scheme premium, so a broker change
/// moves a large income stream on one date. Paying the wrong broker for even a month is a
/// dispute between two firms the insurer has to settle.
#[test]
fn a_scheme_changing_broker_mid_year_moves_its_book_with_a_dated_handover_split() {
    let mut w = worked_example();
    let start = d(START.0, START.1, START.2);
    for (broker, person) in [("broker-x", "person-x"), ("broker-y", "person-y")] {
        w.add_party(broker, PartyKind::Broker);
        w.add_party(person, PartyKind::Adviser);
        w.payee(broker);
        w.account(broker, &format!("acct-{broker}"));
        w.send(
            admin(),
            Command::StartEngagement {
                id: EngagementId::from(format!("eng-{broker}").as_str()),
                engagement: ic_distribution::structure::Engagement {
                    adviser: party(person),
                    with: party(broker),
                    from: d(2020, 1, 1),
                    to: None,
                    accreditations: BTreeSet::from(["group-life".to_string()]),
                },
            },
        )
        .unwrap();
    }
    let today = w.today();
    let handover_until = today + chrono::Duration::days(75);
    let book = |id: &str, broker: &str, lines: Vec<Line>| {
        vec![
            Change::CreateBook {
                id: BookId::from(id),
                name: id.into(),
                engagement: Some(EngagementId::from(format!("eng-{broker}").as_str())),
                owner: Some(party(broker)),
                house: false,
            },
            Change::ActivateRule(rule(id, 1, start, lines, vec![liable(broker)])),
        ]
    };
    let mut changes = book("x-schemes", "broker-x", vec![base("broker-x", Share::OfRemainder(WHOLE), true)]);
    changes.extend(book(
        "y-schemes",
        "broker-y",
        vec![
            Line {
                commission_type: Some(CommissionType::Renewal),
                share: Share::OfComponent(pct(50.0)),
                until: Some(handover_until),
                ..base("broker-x", Share::OfRemainder(1), false)
            },
            base("broker-y", Share::OfRemainder(WHOLE), true),
        ],
    ));
    w.activate("brokers", changes).unwrap();

    let issue = ic_sim::persona::group_scheme(
        5,
        today,
        vec![
            ic_sim::persona::member("m1", d(1980, 1, 1), "staff", 80_000_00),
            ic_sim::persona::member("m2", d(1985, 6, 1), "staff", 95_000_00),
        ],
    );
    let scheme = issue.policy.clone();
    let partition = w.cluster.partition_of(&issue.group);
    w.cluster.submit(partition, "group-admin", ic_sim::persona::issue_command(issue)).unwrap();
    w.sync();
    let key = format!("{scheme}/group-life/0");
    assert!(w.dist.layers.contains_key(&key), "the scheme cover has a layer for commission");
    let monthly = w.owing(&scheme);
    w.send(
        module("group-admin"),
        Command::WriteLayer(Box::new(WriteLayer {
            policy: scheme.clone(),
            cover: "group-life".into(),
            layer: 0,
            writing: vec![(EngagementId::from("eng-broker-x"), WHOLE)],
            books: vec![(BookId::from("x-schemes"), WHOLE)],
            schedule: "standard".into(),
            option: "level".into(),
            annual_premium_minor: monthly * 12,
            replacement: false,
        })),
    )
    .unwrap();
    let month = |w: &mut World, n: u32| {
        let before = w.events.len();
        w.pay(&scheme, &format!("s{n}"));
        on_layer(w, before, &key)
    };
    let first = month(&mut w, 0);
    assert!(first.keys().all(|(who, _)| who == "broker-x") && !first.is_empty(), "{first:?}");

    w.days(31);
    let appointed = w.today();
    w.activate(
        "broker-change",
        vec![
            Change::MoveLayers {
                layers: vec![key.clone()],
                to: vec![(BookId::from("y-schemes"), WHOLE)],
                from: appointed,
            },
            Change::SetServicing {
                policy: scheme.clone(),
                engagement: Some(EngagementId::from("eng-broker-y")),
            },
        ],
    )
    .unwrap();
    let handover = month(&mut w, 1);
    let whole: i64 = handover.values().sum();
    assert_eq!(
        handover[&("broker-x".into(), CommissionType::Renewal)],
        portion(whole, pct(50.0)),
        "{handover:?}"
    );
    assert_eq!(handover[&("broker-y".into(), CommissionType::Renewal)], whole - portion(whole, pct(50.0)));

    w.days(31);
    let _ = month(&mut w, 2);
    w.days(31);
    assert!(w.today() > handover_until);
    let after = month(&mut w, 3);
    assert!(after.keys().all(|(who, _)| who == "broker-y") && !after.is_empty(), "{after:?}");
    w.assert_reconciled();
}

/// Purpose: (ADV-019) FAP One loses its licence. From the date, new business under any
/// engagement with it is refused; commission still earned on its advisers' existing business,
/// for FAP One and for the adviser's company, goes to held with the licence as the reason; each
/// policy it serviced gets a task for a new servicing adviser; and appointing one tells the
/// customer.
/// Why it matters: this is the scenario the held balance design exists for. Paying a provider
/// that may not give advice, or losing the commission it would otherwise have earned, are both
/// failures a regulator would see.
#[test]
fn a_provider_losing_its_licence_stops_new_business_holds_commission_and_asks_for_servicing() {
    use ic_distribution::structure::LicenceState;
    let mut w = worked_example();
    with_adviser_b(&mut w);
    w.add_party("fap-two-adviser", PartyKind::Adviser);
    w.engage("eng-c-two", "fap-two-adviser", "fap-two", d(2020, 1, 1));
    // A's company is paid on A's behalf, so it follows A's licence.
    let payee_for = ic_distribution::structure::Relation {
        child: party("a-co"),
        parent: party("adviser-a"),
        kind: ic_distribution::structure::RelationKind::PayeeFor,
        from: d(2020, 1, 1),
        to: None,
    };
    w.send(admin(), Command::Relate(payee_for)).unwrap();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.activate(
        "servicing",
        vec![Change::SetServicing {
            policy: policy.clone(),
            engagement: Some(EngagementId::from("eng-a-one")),
        }],
    )
    .unwrap();
    w.pay(&policy, "r1");
    w.days(31);
    let today = w.today();
    let events = w.licence("fap-one", LicenceState::Revoked, today).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::CaseOpened { .. })), "a conduct case opens: {events:?}");

    let (new, _) = w.issue(2, 500_000_00, None);
    assert_eq!(
        code(w.write(&new, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00)),
        "not_authorised_to_advise"
    );
    assert_eq!(
        code(w.write(&new, "life", 1, "eng-b-one", "b-at-fap-one", 1_200_00)),
        "not_authorised_to_advise"
    );

    let held_before = (w.held("fap-one"), w.held("a-co"));
    let payable_before = (w.payable("fap-one"), w.payable("a-co"));
    w.pay(&policy, "r2");
    assert!(w.held("fap-one") > held_before.0 && w.held("a-co") > held_before.1, "earned into held");
    assert_eq!((w.payable("fap-one"), w.payable("a-co")), payable_before, "and nothing to payable");
    assert!(w
        .dist
        .held
        .iter()
        .any(|h| h.party == party("fap-one") && h.reason == Some(ic_distribution::rules::HeldReason::SuspendedLicence)));

    let tasks: Vec<_> = w.dist.tasks.values().filter(|t| t.subject.contains(&policy.0) && t.done.is_none()).collect();
    assert!(!tasks.is_empty(), "the policy needs a new servicing adviser: {:?}", w.dist.tasks);
    w.activate(
        "new-servicing",
        vec![Change::SetServicing {
            policy: policy.clone(),
            engagement: Some(EngagementId::from("eng-c-two")),
        }],
    )
    .unwrap();
    assert!(w.events.iter().any(|e| matches!(e, Event::ServicingChanged { .. })), "the customer is told");
    assert!(
        w.dist.tasks.values().filter(|t| t.subject == policy.0).all(|t| t.done.is_some()),
        "appointing settles the task"
    );
    w.assert_reconciled();
}

/// Purpose: (ADV-025) a referral partner is paid a fixed fee when a policy is issued from its
/// referral. The partner is a party with a payee record and an agreement, but no engagement, so
/// it can write no business; the fee is a line of its own on the book's rule, separate from base
/// commission; and the fee is clawed back with the initial commission when the policy is
/// cancelled inside the clawback period.
/// Why it matters: referral fees are commission in law. Paid without an agreement, or kept when
/// the business does not stay, they become an inducement the insurer cannot justify.
#[test]
fn a_referral_partners_fixed_fee_is_its_own_line_and_is_clawed_back() {
    let mut w = worked_example();
    w.add_party("partner", PartyKind::ReferralPartner);
    w.payee("partner");
    w.account("partner", "acct-partner");
    let today = w.today();
    let fee = Line {
        component: Component::FixedFee("referral".into()),
        commission_type: Some(CommissionType::Initial),
        share: Share::Fixed(150_00),
        ..base("partner", Share::OfRemainder(1), false)
    };
    let mut lines = vec![
        base("a-co", Share::OfRemainder(pct(75.0)), true),
        base("fap-one", Share::OfRemainder(pct(25.0)), false),
    ];
    lines.push(fee);
    w.activate(
        "referral",
        vec![Change::ActivateRule(rule("a-at-fap-one", 2, today, lines, vec![liable("fap-one")]))],
    )
    .unwrap();
    assert!(!w
        .dist
        .config
        .structure
        .engagements
        .values()
        .any(|e| e.adviser == party("partner") || e.with == party("partner")));
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    let before = w.events.len();
    w.pay(&policy, "r1");
    let mut fee_lines = vec![];
    for e in &w.events[before..] {
        if let Event::CommissionEarned { reference, .. } = e {
            fee_lines.extend(w.dist.allocations[reference].lines.iter().filter(|l| l.payee == party("partner")).cloned());
        }
    }
    assert_eq!(fee_lines.len(), 1);
    assert_eq!(fee_lines[0].component, Some(Component::FixedFee("referral".into())));
    assert_eq!(fee_lines[0].amount_minor, 150_00);
    assert_eq!(w.payable("partner"), 150_00);
    assert!(w.dist.exposures.values().any(|e| e.paid_to == party("partner") && e.amount_minor == 150_00));

    w.days(40);
    w.core(&policy, "policy-servicing", CoreCommand::CancelPolicy { policy: policy.clone() })
        .unwrap();
    let clawed: i64 = w
        .dist
        .exposures
        .values()
        .filter(|e| e.paid_to == party("partner"))
        .map(|e| e.clawed_minor)
        .sum();
    assert_eq!(clawed, 150_00, "inside the first year the whole fee comes back");
    w.assert_reconciled();
}

/// Purpose: (ADV-039) premium paused under a hardship arrangement is not billed, so it earns no
/// commission while the pause runs; when billing resumes, renewal commission is earned again on
/// what is paid. (Premium waived during a claim, which does earn, is covered in lifecycle.rs.)
/// Why it matters: a pause is premium the insurer never receives. Commission on it would be paid
/// out of nothing and taken back later from an adviser who did nothing wrong.
#[test]
fn premium_paused_in_hardship_earns_nothing_until_billing_resumes() {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.pay(&policy, "r1");
    let from = w.today() + chrono::Duration::days(20);
    let to = from + chrono::Duration::days(60);
    w.core(
        &policy,
        "collections",
        CoreCommand::GrantHardship {
            policy: policy.clone(),
            hardship: "pause-1".into(),
            from,
            to,
            share_bps: 0,
        },
    )
    .unwrap();
    let total = |w: &World| w.payable("a-co") + w.payable("fap-one");
    let before = total(&w);
    let earned_events = w.events.len();
    w.days(31);
    let owed = w.owing(&policy);
    if owed > 0 {
        w.pay(&policy, "during");
    }
    assert_eq!(owed, 0, "nothing is billed while premium is paused");
    assert_eq!(total(&w), before, "and nothing is earned");
    assert!(!w.events[earned_events..].iter().any(|e| matches!(e, Event::CommissionEarned { .. })));
    w.run_until(to + chrono::Duration::days(1));
    w.days(31);
    assert!(w.pay(&policy, "after") > 0, "billing resumes");
    assert!(total(&w) > before, "and earns again");
    w.assert_reconciled();
}
