//! Configuration changes: what a change set may contain, who must approve
//! it, and what is refused before anything takes effect.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_distribution::authority::{Grant, GrantKind, Role};
use ic_distribution::config::{AdviserChangeMode, AdviserChangeSetting, Change, ChangeState};
use ic_distribution::model::ScheduleOption;
use ic_distribution::rules::{CommissionType, Component, Fragment, Line, Rule, Share, WHOLE};
use ic_distribution::state::{Settings, WriteLayer};
use ic_distribution::structure::{Engagement, PartyKind, Relation, RelationKind};
use ic_distribution::{Actor, BookId, Command, EngagementId, Event, FragmentId};

fn start() -> chrono::NaiveDate {
    d(START.0, START.1, START.2)
}

fn split(a: f64, b: f64) -> Vec<Line> {
    vec![
        base("a-co", Share::OfRemainder(pct(a)), true),
        base("fap-one", Share::OfRemainder(pct(b)), false),
    ]
}

fn next_rule(lines: Vec<Line>) -> Rule {
    rule("a-at-fap-one", 2, start(), lines, vec![liable("fap-one")])
}

fn book(id: &str) -> Change {
    Change::CreateBook {
        id: BookId::from(id),
        name: id.into(),
        engagement: None,
        owner: None,
        house: false,
    }
}

/// Purpose: a change takes effect only after a commission approver and a compliance approver,
/// two different people neither of whom prepared it, have both signed; until then nothing in
/// the configuration moves, and a rejected or activated change takes no more approvals.
/// Why it matters: a distribution rule decides who is paid. One person being able to write and
/// activate one is the fraud the design's two approvals exist to prevent.
#[test]
fn a_change_needs_two_approvals_from_two_people_who_did_not_prepare_it() {
    let mut w = worked_example();
    let events = w.propose("c1", vec![Change::ActivateRule(next_rule(split(60.0, 40.0)))]).unwrap();
    assert!(matches!(&events[0], Event::ChangeProposed { hash, .. } if hash.len() == 64));
    assert_eq!(w.dist.config.books[&BookId::from("a-at-fap-one")].rules.len(), 1, "proposed is not active");
    let approve = |compliance| Command::ApproveChange { id: "c1".into(), compliance };
    // The wrong role, the preparer, and a module are all refused.
    assert_eq!(code(w.send(ops(), approve(false))), "not_authorised");
    assert_eq!(code(w.send(approver(), approve(true))), "not_authorised");
    assert_eq!(code(w.send(admin(), approve(false))), "not_authorised");
    assert_eq!(code(w.send(module("migration"), approve(false))), "caller_not_allowed");
    let maker_with_everything = staff("staff-ops", &[Role::CommissionOps, Role::CommissionApprover, Role::Compliance]);
    let denied = w.send(maker_with_everything, approve(false)).unwrap_err();
    assert!(denied.message.contains("you prepared this"), "{}", denied.message);
    // A structure manager who also holds the approver role is a declared conflict.
    let conflicted = staff("staff-conflicted", &[Role::DistributionAdmin, Role::CommissionApprover]);
    assert_eq!(code(w.send(conflicted, approve(false))), "not_authorised");

    let both = || staff("staff-both", &[Role::CommissionApprover, Role::Compliance]);
    w.send(both(), approve(false)).unwrap();
    assert_eq!(
        code(w.send(both(), approve(true))),
        "not_authorised",
        "the same person cannot give both approvals"
    );
    assert_eq!(code(w.send(approver(), approve(false))), "already_approved");
    assert_eq!(w.dist.config.books[&BookId::from("a-at-fap-one")].rules.len(), 1);
    let events = w.send(compliance(), approve(true)).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::RuleActivated { version: 2, .. })));
    assert!(events.iter().any(|e| matches!(e, Event::ChangeActivated { .. })));
    assert_eq!(w.dist.change_sets["c1"].state, ChangeState::Activated);
    assert_eq!(code(w.send(compliance(), approve(true))), "change_settled");
    assert_eq!(
        code(w.send(
            compliance(),
            Command::ApproveChange {
                id: "ghost".into(),
                compliance: true
            }
        )),
        "unknown_change"
    );

    w.propose("c2", vec![book("b2")]).unwrap();
    assert_eq!(
        code(w.send(
            ops(),
            Command::RejectChange {
                id: "ghost".into(),
                reason: "x".into()
            }
        )),
        "unknown_change"
    );
    w.send(
        ops(),
        Command::RejectChange {
            id: "c2".into(),
            reason: "wrong book".into(),
        },
    )
    .unwrap();
    assert_eq!(w.dist.change_sets["c2"].rejected_because.as_deref(), Some("wrong book"));
    assert_eq!(
        code(w.send(
            ops(),
            Command::RejectChange {
                id: "c2".into(),
                reason: "x".into()
            }
        )),
        "change_settled"
    );
    assert_eq!(
        code(w.send(
            approver(),
            Command::ApproveChange {
                id: "c2".into(),
                compliance: false
            }
        )),
        "change_settled"
    );
    assert!(!w.dist.config.books.contains_key(&BookId::from("b2")));
}

/// Purpose: every way a change set can be malformed or conflict with what is active is
/// refused when it is proposed, with a code that says which rule it broke.
/// Why it matters: a rule that does not allocate the whole of base commission, or names a
/// payee with no agreement, would fail on the first premium for every policy in the book.
/// Refusing at proposal means it never reaches an approver, let alone a premium.
#[test]
fn a_malformed_or_conflicting_change_is_refused_when_proposed() {
    let mut w = worked_example();
    let today = w.today();
    let rule_change = |r: Rule| vec![Change::ActivateRule(r)];
    let cases: Vec<(&str, Vec<Change>)> = vec![
        ("empty_change", vec![]),
        ("config_conflict", vec![book("a-at-fap-one")]),
        (
            "unknown_engagement",
            vec![Change::CreateBook {
                id: BookId::from("x"),
                name: "x".into(),
                engagement: Some(EngagementId::from("ghost")),
                owner: None,
                house: false,
            }],
        ),
        (
            "unknown_party",
            vec![Change::CreateBook {
                id: BookId::from("x"),
                name: "x".into(),
                engagement: None,
                owner: Some(party("ghost")),
                house: false,
            }],
        ),
        ("unknown_book", rule_change(rule("ghost", 1, today, split(75.0, 25.0), vec![]))),
        ("config_conflict", rule_change(rule("a-at-fap-one", 3, today, split(75.0, 25.0), vec![]))),
        (
            "config_conflict",
            rule_change(rule("a-at-fap-one", 2, d(2027, 2, 1), split(75.0, 25.0), vec![])),
        ),
        (
            "invalid_rule",
            rule_change(Rule {
                instructions_ref: " ".into(),
                ..next_rule(split(75.0, 25.0))
            }),
        ),
        ("invalid_rule", rule_change(next_rule(split(75.0, 20.0)))),
        ("invalid_rule", rule_change(next_rule(vec![base("a-co", Share::OfRemainder(WHOLE), false)]))),
        (
            "payee_without_agreement",
            rule_change(next_rule(vec![base("adviser-a", Share::OfRemainder(WHOLE), true)])),
        ),
        (
            "unknown_fragment",
            rule_change(Rule {
                fragments: vec![FragmentId::from("ghost")],
                ..next_rule(split(75.0, 25.0))
            }),
        ),
        (
            "house_book",
            vec![
                Change::CreateBook {
                    id: BookId::from("house"),
                    name: "House".into(),
                    engagement: None,
                    owner: None,
                    house: true,
                },
                Change::ActivateRule(rule("house", 1, today, split(75.0, 25.0), vec![])),
            ],
        ),
        (
            "unknown_book",
            vec![Change::CloseBook {
                book: BookId::from("ghost"),
                from: today,
            }],
        ),
        (
            "unknown_book",
            vec![Change::WatchBook {
                book: BookId::from("ghost"),
                until: today,
            }],
        ),
        ("config_conflict", vec![Change::ActivateSchedule(schedule())]),
        (
            "config_conflict",
            vec![Change::ActivateSchedule(ic_distribution::model::Schedule {
                id: "new".into(),
                version: 2,
                ..schedule()
            })],
        ),
        (
            "invalid_schedule",
            vec![Change::ActivateSchedule(ic_distribution::model::Schedule {
                id: "new".into(),
                options: Default::default(),
                ..schedule()
            })],
        ),
        (
            "config_conflict",
            vec![Change::AdviserChangeSetting(AdviserChangeSetting {
                version: 5,
                mode: AdviserChangeMode::MovesAtOnce,
            })],
        ),
        (
            "structure",
            vec![Change::EndEngagement {
                id: EngagementId::from("ghost"),
                on: today,
            }],
        ),
        (
            "invalid_shares",
            vec![Change::MoveLayers {
                layers: vec![],
                to: vec![(BookId::from("a-at-fap-one"), 5)],
                from: today,
            }],
        ),
        (
            "unknown_book",
            vec![Change::MoveLayers {
                layers: vec![],
                to: vec![(BookId::from("ghost"), WHOLE)],
                from: today,
            }],
        ),
        (
            "unknown_layer",
            vec![Change::MoveLayers {
                layers: vec!["ghost/life/1".into()],
                to: vec![(BookId::from("a-at-fap-one"), WHOLE)],
                from: today,
            }],
        ),
        (
            "unknown_policy",
            vec![Change::SetServicing {
                policy: ic_core::PolicyId("ghost".into()),
                engagement: None,
            }],
        ),
    ];
    for (n, (expected, changes)) in cases.into_iter().enumerate() {
        assert_eq!(code(w.propose(&format!("bad-{n}"), changes)), expected, "case {n}");
    }
    assert!(w.dist.change_sets.len() == 1, "only the setup change exists");
    assert_eq!(code(w.propose("setup", vec![book("z")])), "duplicate_change");
    let no_instructions = Command::ProposeChange {
        id: "n".into(),
        changes: vec![book("z")],
        instructions_ref: " ".into(),
    };
    assert_eq!(code(w.send(ops(), no_instructions.clone())), "instructions_required");
    assert_eq!(code(w.send(admin(), no_instructions.clone())), "not_authorised");
    assert_eq!(code(w.send(module("migration"), no_instructions)), "caller_not_allowed");
}

/// Purpose: a rule can be backdated only so far, a backdated rule may change base lines only,
/// and a shared fragment changes from today, never in the past; a new fragment version is
/// refused if any book that follows it would no longer be whole.
/// Why it matters: backdating re-pays history. Limiting it to base lines inside a window keeps
/// the replay bounded and explainable; a fragment is shared by thousands of books, so a bad
/// version must fail as one change, not book by book.
#[test]
fn backdating_is_limited_and_a_fragment_change_must_keep_every_follower_whole() {
    let mut w = worked_example();
    let today = w.today();
    let over = |rate: f64| Line {
        component: Component::Override("group".into()),
        share: Share::RateOnPremium(pct(rate)),
        ..base("group-old", Share::OfRemainder(1), false)
    };
    let fragment = |version, from, lines| Fragment {
        id: FragmentId::from("std"),
        version,
        effective_from: from,
        lines,
        liability: vec![],
    };
    assert_eq!(
        code(w.propose("f0", vec![Change::ActivateFragment(fragment(1, today.pred_opt().unwrap(), vec![over(2.0)]))])),
        "backdated_fragment"
    );
    assert_eq!(
        code(w.propose("f0", vec![Change::ActivateFragment(fragment(2, today, vec![over(2.0)]))])),
        "config_conflict"
    );
    let following = Rule {
        fragments: vec![FragmentId::from("std")],
        ..rule("a-at-fap-one", 2, today, split(75.0, 25.0), vec![liable("fap-one")])
    };
    w.activate(
        "f1",
        vec![
            Change::ActivateFragment(fragment(1, today, vec![over(2.0)])),
            Change::ActivateRule(following.clone()),
        ],
    )
    .unwrap();
    // A new version whose line does not fit its component breaks the follower: refused whole.
    let broken = Line {
        share: Share::Fixed(5),
        ..over(2.0)
    };
    let refused = w.propose("f2", vec![Change::ActivateFragment(fragment(2, today, vec![broken]))]).unwrap_err();
    assert_eq!(refused.code, "invalid_rule");
    assert!(refused.message.contains("book a-at-fap-one"), "{}", refused.message);
    w.activate("f2", vec![Change::ActivateFragment(fragment(2, today, vec![over(3.0)]))]).unwrap();

    w.days(400);
    let today = w.today();
    let long_ago = today.checked_sub_days(chrono::Days::new(366)).unwrap();
    let backdated = |from, lines: Vec<Line>, fragments: Vec<FragmentId>| {
        vec![Change::ActivateRule(Rule {
            fragments,
            ..rule("a-at-fap-one", 3, from, lines, vec![liable("fap-one")])
        })]
    };
    let std = vec![FragmentId::from("std")];
    assert_eq!(code(w.propose("b", backdated(long_ago, split(60.0, 40.0), std.clone()))), "backdating_limit");
    let recent = today.checked_sub_days(chrono::Days::new(30)).unwrap();
    assert_eq!(code(w.propose("b", backdated(recent, split(60.0, 40.0), vec![]))), "backdated_override");
    let mut with_override = split(60.0, 40.0);
    with_override.push(Line {
        component: Component::Override("extra".into()),
        commission_type: Some(CommissionType::Renewal),
        share: Share::RateOnPremium(pct(1.0)),
        ..base("fap-one", Share::OfRemainder(1), false)
    });
    assert_eq!(
        code(w.propose("b", backdated(recent, with_override.clone(), std.clone()))),
        "backdated_override"
    );
    w.activate("b", backdated(recent, split(60.0, 40.0), std.clone())).unwrap();
    // From today, the override may change.
    let mut from_today = backdated(today, with_override, std);
    if let Change::ActivateRule(r) = &mut from_today[0] {
        r.version = 4;
    }
    w.activate("b2", from_today).unwrap();
}

/// Purpose: who wrote a layer is recorded once and checked: the engagement must be in force,
/// accredited for the product and licensed that day, the book open with a rule, the schedule
/// and option real, and the shares whole.
/// Why it matters: the writing engagement is the fact every later question hangs on: who gave
/// the advice, under whose licence. It cannot be allowed to be wrong or to change.
#[test]
fn writing_a_layer_is_checked_and_can_never_be_rewritten() {
    let mut w = worked_example();
    let today = w.today();
    w.add_party("adviser-b", PartyKind::Adviser);
    let unaccredited = Engagement {
        adviser: party("adviser-b"),
        with: party("fap-one"),
        from: d(2020, 1, 1),
        to: None,
        accreditations: BTreeSet::from(["health".to_string()]),
    };
    w.send(
        admin(),
        Command::StartEngagement {
            id: EngagementId::from("eng-b"),
            engagement: unaccredited,
        },
    )
    .unwrap();
    w.add_party("adviser-c", PartyKind::Adviser);
    w.engage("eng-c-ended", "adviser-c", "fap-one", d(2020, 1, 1));
    w.send(
        admin(),
        Command::EndEngagement {
            id: EngagementId::from("eng-c-ended"),
            on: d(2025, 1, 1),
        },
    )
    .unwrap();
    w.activate(
        "books",
        vec![
            book("closed"),
            Change::ActivateRule(rule("closed", 1, today, split(75.0, 25.0), vec![])),
            Change::CloseBook {
                book: BookId::from("closed"),
                from: today,
            },
            book("no-rule"),
        ],
    )
    .unwrap();
    let (policy, _) = w.issue(1, 500_000_00, None);
    let good = WriteLayer {
        policy: policy.clone(),
        cover: "life".into(),
        layer: 1,
        writing: vec![(EngagementId::from("eng-a-one"), WHOLE)],
        books: vec![(BookId::from("a-at-fap-one"), WHOLE)],
        schedule: "standard".into(),
        option: "upfront".into(),
        annual_premium_minor: 1_200_00,
        replacement: false,
    };
    let send = |w: &mut World, layer: WriteLayer| w.send(module("new-business"), Command::WriteLayer(Box::new(layer)));
    let cases: Vec<(&str, WriteLayer)> = vec![
        ("unknown_layer", WriteLayer { layer: 9, ..good.clone() }),
        (
            "invalid_shares",
            WriteLayer {
                writing: vec![],
                ..good.clone()
            },
        ),
        (
            "invalid_shares",
            WriteLayer {
                writing: vec![(EngagementId::from("eng-a-one"), 5)],
                ..good.clone()
            },
        ),
        ("invalid_shares", WriteLayer { books: vec![], ..good.clone() }),
        (
            "not_authorised_to_advise",
            WriteLayer {
                writing: vec![(EngagementId::from("ghost"), WHOLE)],
                ..good.clone()
            },
        ),
        (
            "not_authorised_to_advise",
            WriteLayer {
                writing: vec![(EngagementId::from("eng-b"), WHOLE)],
                ..good.clone()
            },
        ),
        (
            "not_authorised_to_advise",
            WriteLayer {
                writing: vec![(EngagementId::from("eng-c-ended"), WHOLE)],
                ..good.clone()
            },
        ),
        (
            "unknown_book",
            WriteLayer {
                books: vec![(BookId::from("ghost"), WHOLE)],
                ..good.clone()
            },
        ),
        (
            "book_closed",
            WriteLayer {
                books: vec![(BookId::from("closed"), WHOLE)],
                ..good.clone()
            },
        ),
        (
            "no_rule",
            WriteLayer {
                books: vec![(BookId::from("no-rule"), WHOLE)],
                ..good.clone()
            },
        ),
        (
            "unknown_schedule",
            WriteLayer {
                schedule: "ghost".into(),
                ..good.clone()
            },
        ),
        (
            "unknown_option",
            WriteLayer {
                option: "ghost".into(),
                ..good.clone()
            },
        ),
        (
            "invalid_amount",
            WriteLayer {
                annual_premium_minor: 0,
                ..good.clone()
            },
        ),
    ];
    for (n, (expected, layer)) in cases.into_iter().enumerate() {
        assert_eq!(code(send(&mut w, layer)), expected, "case {n}");
    }
    assert_eq!(code(w.send(admin(), Command::WriteLayer(Box::new(good.clone())))), "not_authorised");
    let events = w.send(ops(), Command::WriteLayer(Box::new(good.clone()))).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::ServicingChanged { .. })) && events.iter().any(|e| matches!(e, Event::LayerAssigned { .. })));
    assert_eq!(code(send(&mut w, good)), "layer_written");
    let record = &w.dist.policies[&policy];
    assert_eq!(record.default.as_ref().map(|x| x.book.clone()), Some(BookId::from("a-at-fap-one")));
    // A level option pays no initial commission, so there is nothing to claw back.
    let (second, _) = w.issue(2, 500_000_00, None);
    w.send(
        module("new-business"),
        Command::WriteLayer(Box::new(WriteLayer {
            policy: second.clone(),
            cover: "life".into(),
            layer: 1,
            writing: vec![(EngagementId::from("eng-a-one"), pct(50.0)), (EngagementId::from("eng-a-one"), pct(50.0))],
            books: vec![(BookId::from("a-at-fap-one"), pct(50.0)), (BookId::from("closed"), pct(50.0))],
            schedule: "standard".into(),
            option: "level".into(),
            annual_premium_minor: 1_200_00,
            replacement: false,
        })),
    )
    .unwrap_err();
    let _ = ScheduleOption {
        initial_ppm: 0,
        renewal_ppm: 0,
        renewal_from_month: 0,
    };
}

/// Purpose: a layer split between two books pays each book's rule its share, and a level
/// commission option pays renewal only.
/// Why it matters: joint cases are split between advisers at the layer, and the split has to
/// carry through to cents without losing or inventing any.
#[test]
fn a_layer_split_between_books_pays_each_book_its_share() {
    let mut w = worked_example();
    let today = w.today();
    w.add_party("b-co", PartyKind::Company);
    w.payee("b-co");
    w.activate(
        "second-book",
        vec![
            book("b-book"),
            Change::ActivateRule(rule("b-book", 1, today, vec![base("b-co", Share::OfRemainder(WHOLE), true)], vec![])),
        ],
    )
    .unwrap();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.send(
        module("new-business"),
        Command::WriteLayer(Box::new(WriteLayer {
            policy: policy.clone(),
            cover: "life".into(),
            layer: 1,
            writing: vec![(EngagementId::from("eng-a-one"), WHOLE)],
            books: vec![(BookId::from("a-at-fap-one"), pct(60.0)), (BookId::from("b-book"), pct(40.0))],
            schedule: "standard".into(),
            option: "level".into(),
            annual_premium_minor: 1_200_00,
            replacement: false,
        })),
    )
    .unwrap();
    w.pay(&policy, "r1");
    let first = w.payable("a-co") + w.payable("fap-one");
    let second = w.payable("b-co");
    assert!(first > 0 && second > 0);
    let total = first + second;
    assert!((first * 10 - total * 6).abs() <= 10, "60/40 to the cent: {first} {second}");
    assert!(w.dist.exposures.is_empty(), "level commission has no initial to claw back");
    // With no liability line the payee bears their own clawback.
    w.assert_reconciled();
}

/// Purpose: an engagement started with no commission book of its own gives the distribution
/// administrator an open task that says so, which a change set creating its book settles;
/// an engagement started in the same change set as its book raises none.
/// Why it matters: after an adviser moved provider, nothing he wrote earned commission, because
/// books belong to engagements and nobody was told the new one had none (found by the scenario
/// run, F2-20-8).
#[test]
fn an_engagement_without_a_book_is_a_task_until_it_has_one() {
    let mut w = worked_example();
    w.add_party("adviser-n", PartyKind::Adviser);
    w.engage("eng-n", "adviser-n", "fap-one", d(2027, 11, 15));
    let open = |w: &World| {
        w.dist
            .tasks
            .values()
            .filter(|t| t.kind == "needs_commission_book" && t.done.is_none())
            .map(|t| (t.subject.clone(), t.detail.clone()))
            .collect::<Vec<_>>()
    };
    let tasks = open(&w);
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0].0, "eng-n");
    assert!(tasks[0].1.contains("earns commission until a change set creates its book"), "{}", tasks[0].1);
    let own_book = |id: &str, engagement: &str| Change::CreateBook {
        id: BookId::from(id),
        name: id.into(),
        engagement: Some(EngagementId::from(engagement)),
        owner: Some(party("fap-one")),
        house: false,
    };
    w.activate("book-n", vec![own_book("book-n", "eng-n")]).unwrap();
    assert!(open(&w).is_empty(), "settled by the book");

    w.add_party("adviser-m", PartyKind::Adviser);
    let start = Change::StartEngagement {
        id: EngagementId::from("eng-m"),
        engagement: Engagement {
            adviser: party("adviser-m"),
            with: party("fap-one"),
            from: d(2027, 11, 15),
            to: None,
            accreditations: BTreeSet::from(["term-life".to_string()]),
        },
    };
    w.activate("with-book", vec![start, own_book("book-m", "eng-m")]).unwrap();
    assert!(open(&w).is_empty(), "started with its book");
}

/// Purpose: the structure commands are checked: parties and engagements are unique and of the
/// right kind, relations cannot loop or duplicate, and only a structure manager may change
/// them, with a migration feed allowed to load them.
/// Why it matters: licence cascades walk this structure. A loop or a provider engaged as an
/// adviser would make the cascade wrong for everyone below it.
#[test]
fn the_structure_refuses_duplicates_loops_and_the_wrong_kind_of_party() {
    let mut w = worked_example();
    let relate = |child: &str, parent: &str, kind| {
        Command::Relate(Relation {
            child: party(child),
            parent: party(parent),
            kind,
            from: d(2020, 1, 1),
            to: None,
        })
    };
    assert_eq!(
        code(w.send(
            admin(),
            Command::AddParty {
                party: party("fap-one"),
                kind: PartyKind::Fap
            }
        )),
        "structure"
    );
    assert_eq!(
        code(w.send(
            ops(),
            Command::AddParty {
                party: party("new"),
                kind: PartyKind::Fap
            }
        )),
        "not_authorised"
    );
    w.send(
        module("migration"),
        Command::AddParty {
            party: party("ab-1"),
            kind: PartyKind::AuthorisedBody,
        },
    )
    .unwrap();
    assert_eq!(
        code(w.send(
            module("payments"),
            Command::AddParty {
                party: party("p"),
                kind: PartyKind::Fap
            }
        )),
        "caller_not_allowed"
    );
    w.send(admin(), relate("fap-one", "group-old", RelationKind::Under)).unwrap();
    assert_eq!(code(w.send(admin(), relate("fap-one", "group-old", RelationKind::Under))), "structure");
    assert_eq!(code(w.send(admin(), relate("group-old", "fap-one", RelationKind::Under))), "structure");
    assert_eq!(code(w.send(admin(), relate("ghost", "fap-one", RelationKind::Under))), "structure");
    w.send(admin(), relate("ab-1", "fap-one", RelationKind::LicensedBy)).unwrap();
    let end = |on| Command::EndRelation {
        child: party("ab-1"),
        parent: party("fap-one"),
        kind: RelationKind::LicensedBy,
        on,
    };
    assert_eq!(code(w.send(admin(), end(d(2019, 1, 1)))), "structure");
    let events = w.send(admin(), end(d(2028, 1, 1))).unwrap();
    assert!(matches!(&events[0], Event::StructureChanged { .. }));
    assert_eq!(code(w.send(admin(), end(d(2028, 6, 1)))), "structure");

    let engage = |id: &str, adviser: &str, with: &str| Command::StartEngagement {
        id: EngagementId::from(id),
        engagement: Engagement {
            adviser: party(adviser),
            with: party(with),
            from: d(2020, 1, 1),
            to: None,
            accreditations: BTreeSet::new(),
        },
    };
    assert_eq!(code(w.send(admin(), engage("eng-a-one", "adviser-a", "fap-one"))), "structure");
    assert_eq!(code(w.send(admin(), engage("e", "a-co", "fap-one"))), "structure");
    assert_eq!(code(w.send(admin(), engage("e", "adviser-a", "group-old"))), "structure");
    assert_eq!(code(w.send(admin(), engage("e", "adviser-a", "ghost"))), "structure");
    w.send(admin(), engage("e", "adviser-a", "ab-1")).unwrap();
    let end_engagement = |on| Command::EndEngagement {
        id: EngagementId::from("e"),
        on,
    };
    assert_eq!(code(w.send(admin(), end_engagement(d(2019, 12, 31)))), "structure");
    w.send(admin(), end_engagement(d(2028, 1, 1))).unwrap();
    assert_eq!(code(w.send(admin(), end_engagement(d(2029, 1, 1)))), "structure");
    assert_eq!(
        code(w.send(
            admin(),
            Command::MarkCeased {
                party: party("ghost"),
                on: d(2028, 1, 1),
                reason: ic_distribution::structure::CeasedReason::Retired
            }
        )),
        "structure"
    );

    // Payees need a place in the structure, an agreement and a sane recovery share.
    let register = |who: &str, agreement: &str, ppm| Command::RegisterPayee {
        party: party(who),
        agreement_ref: agreement.into(),
        advance_recovery_ppm: ppm,
    };
    assert_eq!(code(w.send(admin(), register("ghost", "a", 0))), "unknown_party");
    assert_eq!(code(w.send(admin(), register("ab-1", " ", 0))), "invalid_payee");
    assert_eq!(code(w.send(admin(), register("ab-1", "a", WHOLE + 1))), "invalid_payee");
    let account = |who: &str, account: &str, check: &str| Command::AddPayeeAccount {
        party: party(who),
        account_ref: account.into(),
        name_check_ref: check.into(),
        step_up_ref: "s".into(),
        appointment_ref: None,
    };
    assert_eq!(code(w.send(admin(), account("ab-1", "x", "n"))), "unknown_payee");
    assert_eq!(code(w.send(admin(), account("a-co", "x", " "))), "invalid_account");
    assert_eq!(code(w.send(admin(), account("a-co", "acct-a-co", "n"))), "duplicate_account");
}

/// Purpose: someone covering for a colleague inherits that colleague's assigned records for
/// the period of the grant and no longer, and the decision records the grant it relied on.
/// Why it matters: leave cover is the commonest reason people share logins. A dated, recorded
/// grant gives the access without the sharing, and ends by itself.
#[test]
fn a_cover_grant_lends_a_colleagues_records_for_its_period_only() {
    let mut w = worked_example();
    let today = w.today();
    w.activate(
        "paths",
        vec![Change::SetStaffPaths {
            staff: party("staff-away"),
            paths: BTreeSet::from(["a-co".to_string()]),
        }],
    )
    .unwrap();
    let mut cover = person("staff-cover", &[Role::DistributionAdmin]);
    cover.paths = BTreeSet::new();
    let hold = || Command::SetPaymentHold {
        party: party("a-co"),
        reason: Some("review".into()),
    };
    assert_eq!(code(w.send(Actor::Staff(Box::new(cover.clone())), hold())), "not_authorised");
    cover.grants.push(Grant {
        id: "grant-1".into(),
        kind: GrantKind::Cover { of: party("staff-away") },
        reason: "annual leave".into(),
        approver: party("manager-1"),
        from: today,
        until: today.checked_add_days(chrono::Days::new(5)).unwrap(),
        external_ref: None,
    });
    w.send(Actor::Staff(Box::new(cover.clone())), hold()).unwrap();
    let other = Command::SetPaymentHold {
        party: party("fap-one"),
        reason: Some("review".into()),
    };
    assert_eq!(
        code(w.send(Actor::Staff(Box::new(cover.clone())), other)),
        "not_authorised",
        "only the colleague's records"
    );
    w.days(6);
    assert_eq!(code(w.send(Actor::Staff(Box::new(cover)), hold())), "not_authorised", "the grant has ended");
}

/// Purpose: a command sent before the module has seen any business date is refused rather
/// than dated by a clock.
/// Why it matters: the module never reads a clock, so replicas agree. Until the core has said
/// what day it is, there is no day to act on.
#[test]
fn nothing_is_decided_before_the_first_business_date_is_known() {
    let mut state = ic_distribution::DistributionState::default();
    let out = state.apply_envelope(
        1,
        ic_distribution::Envelope {
            key: "k".into(),
            actor: admin(),
            command: Command::AddParty {
                party: party("x"),
                kind: PartyKind::Fap,
            },
        },
    );
    assert_eq!(out.result.unwrap_err().code, "business_date");
    assert!(state.unconfirmed().is_empty());
}

/// Purpose: with the production setting, a rule is refused unless it carries
/// worked examples; an example that the rule does not reproduce is refused
/// naming what the rule pays and what the agreement says; an example dated
/// before the rule takes effect is refused; and a rule whose examples all
/// come out right activates.
/// Why it matters: a rule is the agreement turned into data. The examples
/// are the agreement's own arithmetic, written by the person who read it, so
/// a share typed wrongly is caught before any adviser is paid by it.
#[test]
fn a_rule_activates_only_when_its_worked_examples_come_out_right() {
    use ic_distribution::rules::RuleExample;
    let mut w = worked_example();
    w.dist.settings.rule_examples_required = true;
    let on = w.today();
    // FAP One also takes an override of 2% of premium on top of base.
    let lines = vec![
        base("a-co", Share::OfRemainder(pct(80.0)), true),
        base("fap-one", Share::OfRemainder(pct(20.0)), false),
        Line {
            component: Component::Override("group".into()),
            share: Share::RateOnPremium(pct(2.0)),
            ..base("fap-one", Share::OfRemainder(1), false)
        },
    ];
    let example = |name: &str, on, a_co: i64, fap_one: i64| RuleExample {
        name: name.into(),
        on,
        commission_type: CommissionType::Initial,
        gross_minor: 1_000_00,
        premium_minor: 10_000_00,
        expect: vec![
            (party("a-co"), Component::Base, a_co),
            (party("fap-one"), Component::Base, fap_one),
            (party("fap-one"), Component::Override("group".into()), 200_00),
        ],
    };
    let with = |examples: Vec<RuleExample>| {
        let mut r = rule("a-at-fap-one", 2, on, lines.clone(), vec![liable("fap-one")]);
        r.examples = examples;
        vec![Change::ActivateRule(r)]
    };
    let refused = |w: &mut World, id: &str, changes| w.propose(id, changes).unwrap_err();
    assert_eq!(refused(&mut w, "none", with(vec![])).code, "rule_examples");
    let wrong = refused(&mut w, "wrong", with(vec![example("typed-75", on, 750_00, 250_00)]));
    assert_eq!(wrong.code, "rule_examples");
    assert!(
        wrong.message.contains("typed-75") && wrong.message.contains("800") && wrong.message.contains("750"),
        "the refusal says what differs: {}",
        wrong.message
    );
    let early = refused(&mut w, "early", with(vec![example("early", on.pred_opt().unwrap(), 800_00, 200_00)]));
    assert_eq!(early.code, "rule_examples");
    w.activate("right", with(vec![example("agreement-2", on, 800_00, 200_00)])).unwrap();
    assert!(w.events.iter().any(|e| matches!(e, Event::RuleActivated { .. })));
}

/// Purpose: settings written by an older build, before the replacement window, the amount
/// tolerance, worked examples and bounded history existed, are read back with the defaults
/// for those settings.
/// Why it matters: a replica restores its state from a snapshot taken by the build before.
/// If the newer settings were required, an upgrade would fail to start; if they defaulted to
/// zero, the replacement rule and history bounding would be silently off.
#[test]
fn settings_from_an_older_snapshot_take_the_new_defaults() {
    let mut old = serde_json::to_value(Settings::default()).unwrap();
    let fields = old.as_object_mut().unwrap();
    for added in [
        "replacement_window_months",
        "replacement_amount_tolerance_ppm",
        "rule_examples_required",
        "history_days",
    ] {
        assert!(fields.remove(added).is_some(), "{added} is a setting");
    }
    let read: Settings = serde_json::from_value(old).unwrap();
    assert_eq!(read, Settings::default());
}

/// Purpose: the starter commission schedules shipped for the AIA products load as one change
/// set the module activates, every AIA product the repository ships either has a schedule
/// named for its new business or is listed as earning no commission, and income cover on AIA
/// Living is written under the income schedule. The file says plainly that its rates are
/// placeholders, and naming a schedule that was never activated is refused.
/// Why it matters: a product with no schedule cannot have its layers written, so advisers
/// could sell it and never be paid; and a placeholder rate mistaken for an agreed one is paid
/// to every adviser until someone notices.
#[test]
fn the_shipped_aia_schedules_activate_and_cover_every_aia_product() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{root}/config/distribution/aia-schedules.json")).unwrap()).unwrap();
    assert_eq!(file["placeholder"], true);
    let changes: Vec<Change> = serde_json::from_value(file["changes"].clone()).unwrap();
    let mut w = World::new();
    w.activate("aia-starter-schedules", changes).unwrap();
    let named = &w.dist.config.product_schedules;
    assert_eq!(named["aia-living/income_protection"], "aia-income");
    let mut covered: BTreeSet<String> = named.keys().filter(|k| !k.contains('/')).cloned().collect();
    covered.extend(file["no_commission"].as_array().unwrap().iter().filter_map(|v| v.as_str()).map(String::from));
    let mut shipped = BTreeSet::new();
    for entry in std::fs::read_dir(format!("{root}/products")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_string_lossy().starts_with("aia_") {
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("code = \"aia-")
                    .map(|r| format!("aia-{}", r.trim_end_matches(['"', ','])))
            });
            shipped.insert(code.unwrap());
        }
    }
    assert_eq!(covered, shipped, "every AIA product has a schedule or is named as earning none");

    let unknown = Change::SetProductSchedule {
        product: "aia-living".into(),
        cover_type: None,
        schedule: Some("never-activated".into()),
    };
    assert_eq!(code(w.activate("bad", vec![unknown])), "unknown_schedule");
    let cleared = Change::SetProductSchedule {
        product: "aia-starter".into(),
        cover_type: None,
        schedule: None,
    };
    w.activate("clear", vec![cleared]).unwrap();
    assert!(!w.dist.config.product_schedules.contains_key("aia-starter"));
}

/// Purpose: compliance holds and releases a payee's payments without touching any licence; a hold on a party that is not
/// a payee says who its earnings are paid to, or that it must be registered first; a refusal for want of a role says in words
/// what the action is and who does it; and the parties question lists every party with its kind, licence and hold.
/// Why it matters: found by the scenario run, SCN-20 (F-20-3) and SCN-03 (F-SCN03-3, F-SCN03-5). Compliance could only stop an
/// investigated adviser being paid by recording his licence as suspended, which misstated it and locked his book; the refusals said
/// "none of your roles allows payee.manage" and "adv-d5 is not a registered payee", with no next step; and the console had no list
/// of parties to pick from.
#[test]
fn compliance_holds_payments_and_refusals_say_who_may() {
    let mut w = worked_example();
    let hold = |who: &str, reason: Option<&str>| Command::SetPaymentHold {
        party: party(who),
        reason: reason.map(str::to_string),
    };
    w.send(compliance(), hold("a-co", Some("adviser under investigation"))).unwrap();
    assert_eq!(
        w.dist.position(&party("a-co"), w.today()).payment_hold.as_deref(),
        Some("adviser under investigation")
    );
    let parties = ic_distribution::host::ask(&w.dist, ic_distribution::host::Question::Parties).unwrap();
    let a_co = parties.as_array().unwrap().iter().find(|p| p["party"] == "a-co").unwrap();
    assert_eq!(
        (a_co["kind"].as_str(), a_co["payee"].as_bool(), a_co["payment_hold"].as_str()),
        (Some("company"), Some(true), Some("adviser under investigation"))
    );
    let adviser = parties.as_array().unwrap().iter().find(|p| p["party"] == "adviser-a").unwrap();
    assert_eq!((adviser["payee"].as_bool(), adviser["licence"].is_null()), (Some(false), true));
    w.send(compliance(), hold("a-co", None)).unwrap();
    assert_eq!(w.dist.position(&party("a-co"), w.today()).payment_hold, None);

    // An adviser who is not a payee: say what to do instead.
    let refused = w.send(compliance(), hold("adviser-a", Some("x"))).unwrap_err();
    assert_eq!(
        refused.message,
        "adviser-a is not a registered payee: nothing is paid to adviser-a to hold; register adviser-a as a payee first, and its payments can then be held"
    );
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
    let refused = w.send(compliance(), hold("adviser-a", Some("x"))).unwrap_err();
    assert_eq!(
        refused.message,
        "adviser-a is not a registered payee: what adviser-a earns is paid to a-co; hold that payee's payments"
    );

    // Refusals in words, keeping the action's name.
    let refused = w.send(ops(), hold("a-co", Some("x"))).unwrap_err();
    assert_eq!(
        refused.message,
        "none of your roles allows payee.hold (holding or releasing a payee's payments): that is done by the distribution administrator or compliance"
    );
    let licence = Command::RecordLicence {
        party: party("fap-one"),
        state: ic_distribution::structure::LicenceState::Suspended,
        effective: w.today(),
        source_ref: "FSPR-1".into(),
    };
    let refused = w.send(ops(), licence).unwrap_err();
    assert!(
        refused
            .message
            .ends_with("(recording a licence state): that is done by compliance or the distribution administrator"),
        "{}",
        refused.message
    );
}
