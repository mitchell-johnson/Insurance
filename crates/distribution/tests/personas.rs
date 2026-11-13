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
