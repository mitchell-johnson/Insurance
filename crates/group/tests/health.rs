//! A scheme's health menu in force: members on the default plan from the
//! day they join, their choices and dependants put into the core, who pays
//! for each cover, and what follows a change of menu.

mod common;

use chrono::{Days, NaiveDate};
use common::*;
use ic_core::{LayerOrigin, Reject};
use ic_group::command::{caller, DependantSpec, GroupCommand};
use ic_group::event::{ExceptionKind, GroupEvent};
use ic_group::mapping::FileKind;
use ic_group::terms::{HealthMenu, HealthPlan, HealthTier, MenuCover, SchemeTerms};
use serde_json::json;

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn cover(code: &str, excess: Option<&str>, member_pays: bool) -> MenuCover {
    MenuCover {
        cover: code.into(),
        excess_option: excess.map(str::to_string),
        member_pays,
    }
}

/// Core: hospital at the standard excess, paid by the employer. Plus: the
/// same, with the specialist upgrade the member pays for. Premium: hospital
/// with no excess, specialist and dental, all paid by the employer.
fn menu() -> HealthMenu {
    let plan = |code: &str, covers: Vec<MenuCover>| HealthPlan {
        code: code.into(),
        name: code.into(),
        covers,
    };
    HealthMenu {
        plans: vec![
            plan("core", vec![cover("hospital", Some("standard"), false)]),
            plan("plus", vec![cover("hospital", Some("standard"), false), cover("specialist", None, true)]),
            plan(
                "premium",
                vec![
                    cover("hospital", Some("nil"), false),
                    cover("specialist", None, false),
                    cover("dental", None, false),
                ],
            ),
        ],
        default_plan: "core".into(),
        tiers: vec![HealthTier::Single, HealthTier::Couple, HealthTier::Family],
        choice_backdating_days: 30,
        employer_pays_dependants: false,
    }
}

fn with_menu() -> SchemeTerms {
    let mut t = terms();
    t.health_menu = Some(menu());
    t
}

/// The founder E001 and E002, on a Workplace Benefits scheme with the menu.
fn scheme() -> Harness {
    scheme_on(with_menu())
}

fn scheme_on(t: SchemeTerms) -> Harness {
    scheme_on_product(t, benefits_product())
}

fn scheme_on_product(t: SchemeTerms, product: ic_product::ProductDefinition) -> Harness {
    let mut h = Harness::on(t, product);
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        ],
    );
    h
}

fn dependant(key: &str, born: NaiveDate) -> DependantSpec {
    DependantSpec {
        key: key.into(),
        date_of_birth: born,
    }
}

fn choose(employee_id: &str, plan: &str, tier: HealthTier, effective: NaiveDate, dependants: Vec<DependantSpec>) -> GroupCommand {
    GroupCommand::ChooseHealthPlan {
        scheme: SCHEME.into(),
        employee_id: employee_id.into(),
        plan: plan.into(),
        tier,
        effective,
        dependants,
    }
}

fn send(h: &mut Harness, caller: &str, command: GroupCommand) -> Result<(), Reject> {
    let r = h.send_as(caller, "someone", command);
    h.settle();
    r
}

/// The chosen covers a life holds in the core today: code, excess level and
/// whether the member pays.
fn held(h: &Harness, life_id: &str) -> Vec<(String, Option<String>, bool)> {
    let policy = &h.c.state(h.p).policies[&h.policy];
    let mut out: Vec<_> = policy
        .covers
        .values()
        .flat_map(|c| c.layers.iter().map(move |l| (c.code.clone(), l)))
        .filter(|(_, l)| l.life == life(life_id) && l.origin == LayerOrigin::MemberChoice && l.end.is_none())
        .map(|(code, l)| (code, l.excess_option.clone(), l.member_pays == ic_core::policy::MemberPays::All))
        .collect();
    out.sort();
    out
}

fn in_core(h: &Harness, life_id: &str) -> bool {
    h.core_lives().iter().any(|l| l == life_id)
}

/// Purpose: every member is on the default plan from the day they join, founders included;
/// a member's choice of plan and tier puts each cover on their life with the plan's payer
/// (the employer for the plan, the member for the upgrade), joins their dependants as lives
/// holding the same covers at the member's cost, and is recorded and published only once
/// the core has it all; a smaller tier takes the dependant left out off the plan; the
/// invoice counts members, not dependants; and a member who leaves takes their dependants.
/// Why it matters: Mitchell's decision of 14 Nov 2026. A member not on the default plan
/// would be uncovered for hospital from the day they joined; a split of payers the wrong way
/// round would bill the employer for upgrades staff chose for themselves; and a dependant
/// left on the policy after their member went would be cover nobody is paying for.
#[test]
fn a_members_choice_puts_their_plan_and_dependants_into_force() {
    let mut h = scheme();
    let today = h.today();
    let standard = || ("hospital".to_string(), Some("standard".to_string()), false);
    assert_eq!(held(&h, "E001"), vec![standard()], "the founder is on the default plan");
    assert_eq!(held(&h, "E002"), vec![standard()], "a joiner is on the default plan");
    assert!(h.scheme().membership.members["E002"].health.is_none());

    let family = vec![dependant("partner", d(1988, 3, 1)), dependant("child-1", d(2016, 7, 1))];
    send(&mut h, caller::MEMBER, choose("E002", "plus", HealthTier::Family, today, family)).unwrap();
    let specialist = |member_pays| ("specialist".to_string(), None, member_pays);
    assert_eq!(held(&h, "E002"), vec![standard(), specialist(true)]);
    let theirs = vec![("hospital".to_string(), Some("standard".to_string()), true), specialist(true)];
    assert_eq!(held(&h, "E002+partner"), theirs, "the member pays for their dependants");
    assert_eq!(held(&h, "E002+child-1"), theirs);
    let m = &h.scheme().membership.members["E002"];
    assert_eq!(m.dependants.len(), 2);
    assert_eq!(m.health.as_ref().map(|c| (c.plan.as_str(), c.tier)), Some(("plus", HealthTier::Family)));
    assert!(h.saw(|e| matches!(e, GroupEvent::HealthPlanChosen { tier: HealthTier::Family, .. })));

    // The employer sees the plan and tier and nothing of who the dependants are.
    let view = ic_group::answer(&h.g, json!({"ask": "member", "scheme": SCHEME, "employee": "E002"})).unwrap();
    assert_eq!(view["health"], json!({"plan": "plus", "tier": "family", "since": today}));
    assert!(!view.to_string().contains("partner") && !view.to_string().contains("1988"), "{view}");

    // Couple: the child comes off the plan from the date; choosing the same again changes nothing.
    send(
        &mut h,
        caller::MEMBER,
        choose("E002", "plus", HealthTier::Couple, today, vec![dependant("partner", d(1988, 3, 1))]),
    )
    .unwrap();
    assert!(!in_core(&h, "E002+child-1"));
    assert!(in_core(&h, "E002+partner"));
    assert_eq!(h.scheme().membership.members["E002"].dependants.len(), 1);
    send(
        &mut h,
        caller::MEMBER,
        choose("E002", "plus", HealthTier::Couple, today, vec![dependant("partner", d(1988, 3, 1))]),
    )
    .unwrap();
    assert_eq!(held(&h, "E002+partner").len(), 2);

    // December's invoice counts two members; the partner's cover is billed to E002, not the employer.
    h.days(31);
    let invoice = h.scheme().invoices.back().cloned().expect("invoiced");
    assert_eq!(invoice.members(), 2);

    // E002 leaves: the partner goes with them.
    let today = h.today();
    h.apply(
        "f2",
        FileKind::Changes,
        vec![change(1, "E002", ic_group::record::RowChange::Leave, today, attrs("staff", 60_000_00, "wlg"))],
    );
    assert!(!in_core(&h, "E002+partner"));
    assert!(!in_core(&h, "E002"));
}

/// Purpose: a choice that does not fit is refused before anything is sent (no menu, a plan
/// or tier not offered, dependants that do not match the tier or are named badly or born
/// after the date, too far back, not a member, the wrong caller, a choice already being put
/// in place); a menu naming the scheme's own formula cover or a cover the product lacks is
/// refused; a choice dated ahead waits for its day; and a step the core refuses stops the
/// rest of the choice, leaves the member as they were, and is raised for staff.
/// Why it matters: a member is told at once what was wrong with what they asked; a choice
/// half sent would leave dependants on cover nobody chose, and a refusal nobody sees leaves
/// a family believing it is covered.
#[test]
fn a_choice_that_does_not_fit_is_refused_and_a_refused_step_stops_it() {
    let mut plain = Harness::new(terms());
    let today = plain.today();
    assert_eq!(
        code(send(&mut plain, caller::MEMBER, choose("E001", "core", HealthTier::Single, today, vec![]))),
        "no_menu"
    );

    let mut h = scheme();
    let one = || vec![dependant("partner", d(1988, 3, 1))];
    let refused = |h: &mut Harness, c: GroupCommand| code(send(h, caller::MEMBER, c));
    assert_eq!(refused(&mut h, choose("E002", "gold", HealthTier::Single, today, vec![])), "unknown_plan");
    assert_eq!(refused(&mut h, choose("E002", "core", HealthTier::Single, today, one())), "dependants_for_tier");
    assert_eq!(
        refused(&mut h, choose("E002", "core", HealthTier::Family, today, vec![])),
        "dependants_for_tier"
    );
    let twice = vec![dependant("kid", d(2016, 1, 1)), dependant("kid", d(2017, 1, 1))];
    assert_eq!(refused(&mut h, choose("E002", "core", HealthTier::Family, today, twice)), "invalid_dependant");
    assert_eq!(
        refused(&mut h, choose("E002", "core", HealthTier::Couple, today, vec![dependant("a b", d(1988, 1, 1))])),
        "invalid_dependant"
    );
    let unborn = vec![dependant("baby", today.checked_add_days(Days::new(1)).unwrap())];
    assert_eq!(refused(&mut h, choose("E002", "core", HealthTier::Couple, today, unborn)), "invalid_dependant");
    let long_ago = today.checked_sub_days(Days::new(31)).unwrap();
    assert_eq!(
        refused(&mut h, choose("E002", "core", HealthTier::Single, long_ago, vec![])),
        "backdating_limit"
    );
    assert_eq!(refused(&mut h, choose("E009", "core", HealthTier::Single, today, vec![])), "unknown_member");
    assert_eq!(
        code(send(&mut h, caller::REINSURANCE, choose("E002", "core", HealthTier::Single, today, vec![]))),
        "caller_not_allowed"
    );
    // One choice at a time.
    h.send_as(caller::MEMBER, "someone", choose("E002", "premium", HealthTier::Single, today, vec![]))
        .unwrap();
    assert_eq!(
        code(h.send_as(caller::MEMBER, "someone", choose("E002", "core", HealthTier::Single, today, vec![]))),
        "choice_in_progress"
    );
    h.settle();

    // Menus the product cannot carry.
    let amend = |h: &mut Harness, f: &dyn Fn(&mut HealthMenu)| {
        let mut t = h.scheme().terms.clone();
        let mut m = menu();
        f(&mut m);
        t.health_menu = Some(m);
        h.staff(GroupCommand::AmendTerms {
            scheme: SCHEME.into(),
            terms: Box::new(t),
        })
    };
    assert_eq!(code(amend(&mut h, &|m| m.plans[0].covers[0].cover = "group-life".into())), "invalid_terms");
    assert_eq!(code(amend(&mut h, &|m| m.plans[0].covers[0].cover = "optical".into())), "invalid_terms");

    // Dated ahead: nothing reaches the core until the day.
    let ahead = today.checked_add_days(Days::new(5)).unwrap();
    send(&mut h, caller::EMPLOYER, choose("E001", "plus", HealthTier::Couple, ahead, one())).unwrap();
    assert!(!in_core(&h, "E001+partner"));
    h.days(5);
    assert!(in_core(&h, "E001+partner"));
    assert_eq!(h.scheme().membership.members["E001"].health.as_ref().map(|c| c.effective), Some(ahead));

    // The core refuses the first step: nothing more is sent and the member is as they were.
    let today = h.today();
    let before = h.scheme().membership.members["E002"].clone();
    h.core_faults.push_back(Some("composition".into()));
    send(&mut h, caller::STAFF, choose("E002", "plus", HealthTier::Couple, today, one())).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::HealthPlanRefused { code, .. } if code == "composition")));
    assert_eq!(h.scheme().membership.members["E002"], before);
    assert!(!in_core(&h, "E002+partner"));
    assert!(h.g.exceptions.values().any(|x| x.kind == ExceptionKind::RowRefused));
    assert!(h.scheme().queue.is_empty());
}

/// Purpose: when the menu changes, every member's and dependant's covers are sent again as
/// the new menu has them (members on the default plan follow the new default; a member on a
/// plan taken off the menu goes to the default), and taking the menu away ends every chosen
/// cover; and a split moves a member with their dependants, onto the new scheme's own menu.
/// Why it matters: the core holds only what it was last told. A menu change that reached
/// the documents but not the core would leave members insured, and billed, on last year's
/// plan; a split that left dependants behind would be refused by the core.
#[test]
fn a_new_menu_reaches_every_member_and_a_split_takes_dependants_along() {
    // This employer pays for dependants too, so no member owes anything when the split comes.
    let mut first = menu();
    first.employer_pays_dependants = true;
    let mut t = terms();
    t.health_menu = Some(first.clone());
    let mut h = scheme_on(t);
    let today = h.today();
    send(
        &mut h,
        caller::MEMBER,
        choose("E002", "premium", HealthTier::Couple, today, vec![dependant("partner", d(1988, 3, 1))]),
    )
    .unwrap();
    assert_eq!(held(&h, "E002").len(), 3);

    // Premium is withdrawn and the default goes to the high excess.
    let mut t = h.scheme().terms.clone();
    let mut m = first;
    m.plans.retain(|p| p.code != "premium");
    m.plans[0].covers[0].excess_option = Some("high".into());
    t.health_menu = Some(m);
    h.staff(GroupCommand::AmendTerms {
        scheme: SCHEME.into(),
        terms: Box::new(t),
    })
    .unwrap();
    let high = vec![("hospital".to_string(), Some("high".to_string()), false)];
    assert_eq!(held(&h, "E001"), high);
    assert_eq!(held(&h, "E002"), high, "premium is gone: E002 is on the default");
    assert_eq!(held(&h, "E002+partner"), high, "the employer pays for this scheme's dependants");

    // A split: E002 goes with the partner, onto a scheme with no menu.
    let mut terms_new = terms();
    terms_new.health_menu = None;
    h.staff(GroupCommand::SplitScheme {
        scheme: SCHEME.into(),
        into: "harbour-north".into(),
        new_policy: ic_core::PolicyId("pol-north".into()),
        new_number: "WPLNORTH01".into(),
        employer: party("employer-north"),
        effective: d(2027, 12, 1),
        employee_ids: vec!["E002".into()],
        terms: Box::new(terms_new),
    })
    .unwrap();
    let refusal: Vec<_> = h.published.iter().filter(|e| matches!(e, GroupEvent::SplitRefused { .. })).collect();
    assert!(h.saw(|e| matches!(e, GroupEvent::SplitCompleted { .. })), "{refusal:?}");
    let new = &h.g.schemes["harbour-north"];
    assert_eq!(new.membership.members["E002"].dependants.len(), 1);
    let moved = &h.c.state(h.p).policies[&ic_core::PolicyId("pol-north".into())];
    assert!(moved.lives.contains_key(&life("E002+partner")));
    h.days(31);
    let chosen_after = moved_choices(&h, "E002");
    assert_eq!(chosen_after, 0, "no menu on the new scheme: the chosen covers end");

    // Taking the menu away on the old scheme ends the founder's chosen cover.
    let mut t = h.scheme().terms.clone();
    t.health_menu = None;
    h.staff(GroupCommand::AmendTerms {
        scheme: SCHEME.into(),
        terms: Box::new(t),
    })
    .unwrap();
    assert!(held(&h, "E001").is_empty());
}

fn moved_choices(h: &Harness, id: &str) -> usize {
    let policy = &h.c.state(h.p).policies[&ic_core::PolicyId("pol-north".into())];
    policy
        .covers
        .values()
        .flat_map(|c| c.layers.iter())
        .filter(|l| l.life == life(id) && l.origin == LayerOrigin::MemberChoice && l.end.is_none_or(|e| e > h.today()))
        .count()
}

/// Purpose: an employer health plan with no group life is a scheme: it registers on a group
/// product with only chosen covers when it has a health menu (and is refused without one),
/// its members join from the employer's file with no cover of their own until the default
/// plan is set, the employer is invoiced only for what the plan says it pays for, and a
/// member's choice and dependants work as on any scheme.
/// Why it matters: Mitchell asked for a health plan menu, and the core thread confirmed a
/// health-only employer plan exists. Refusing it would leave such employers on paper; and
/// registering one without a menu would bill members for nothing and cover them for nothing.
#[test]
fn a_health_only_scheme_runs_on_its_menu_alone() {
    // Without a menu there is nothing to cover.
    let mut h = Harness::new(terms());
    let refused = GroupCommand::RegisterScheme {
        scheme: "clinic".into(),
        policy: ic_core::PolicyId("pol-clinic".into()),
        group: ic_core::GroupId("grp-clinic".into()),
        employer: party("employer-clinic"),
        inception: h.today(),
        product: Box::new(health_product()),
        terms: Box::new(terms()),
        founding: vec![],
        pools: vec![],
    };
    assert_eq!(code(h.staff(refused)), "not_a_scheme");

    let mut h = scheme_on_product(with_menu(), health_product());
    let standard = ("hospital".to_string(), Some("standard".to_string()), false);
    assert_eq!(held(&h, "E001"), vec![standard.clone()]);
    assert_eq!(held(&h, "E002"), vec![standard]);
    let policy = &h.c.state(h.p).policies[&h.policy];
    assert!(
        policy.covers.values().all(|c| c.layers.iter().all(|l| l.origin == LayerOrigin::MemberChoice)),
        "no cover but what members hold from the menu"
    );

    let today = h.today();
    send(
        &mut h,
        caller::MEMBER,
        choose("E002", "plus", HealthTier::Couple, today, vec![dependant("partner", d(1988, 3, 1))]),
    )
    .unwrap();
    assert!(in_core(&h, "E002+partner"));
    assert_eq!(held(&h, "E002").len(), 2);

    h.days(31);
    let invoice = h.scheme().invoices.back().cloned().expect("invoiced");
    assert_eq!(invoice.members(), 2);
    assert!(invoice.total_minor() > 0, "the employer pays for each member's hospital cover");
}

/// Purpose: a broker on a scheme with a health menu earns commission on the menu covers
/// their expected premium names, as well as on every formula cover; on a health-only scheme
/// that is the menu covers alone; and an expected premium for a cover neither the scheme
/// nor its menu has, or one missing a formula cover, is refused.
/// Why it matters: employer health plans are commonly placed by a broker. Without layers
/// for the menu covers the commission module would never pay the broker on a health-only
/// scheme, and a layer for a cover the scheme cannot hold would be refused there for good.
#[test]
fn a_broker_earns_on_the_menu_covers_their_premium_names() {
    let broker = |premium: &[(&str, i64)]| ic_group::terms::SchemeBroker {
        engagement: "eng-broker-x".into(),
        book: "x-schemes".into(),
        schedule: "standard".into(),
        option: "level".into(),
        expected_annual_premium_minor: premium.iter().map(|(c, m)| (c.to_string(), *m)).collect(),
    };
    let mut t = with_menu();
    t.broker = Some(broker(&[("hospital", 50_000_00), ("specialist", 5_000_00)]));
    let h = scheme_on_product(t, health_product());
    let mut written: Vec<String> = h.commission_layers().into_iter().map(|(_, _, l)| l.cover).collect();
    written.sort();
    assert_eq!(written, vec!["hospital".to_string(), "specialist".to_string()]);

    let mut h = scheme_on_product(with_menu(), benefits_product());
    for (wrong, why) in [
        (broker(&[("hospital", 1)]), "group life is the scheme's own cover"),
        (broker(&[("group-life", 1), ("optical", 1)]), "optical is on neither"),
    ] {
        let mut t = h.scheme().terms.clone();
        t.broker = Some(wrong);
        assert_eq!(
            code(h.staff(GroupCommand::AmendTerms {
                scheme: SCHEME.into(),
                terms: Box::new(t),
            })),
            "invalid_terms",
            "{why}"
        );
    }
    let mut t = h.scheme().terms.clone();
    t.broker = Some(broker(&[("group-life", 30_000_00), ("dental", 2_000_00)]));
    h.staff(GroupCommand::AmendTerms {
        scheme: SCHEME.into(),
        terms: Box::new(t),
    })
    .unwrap();
    let mut written: Vec<String> = h.commission_layers().into_iter().map(|(_, _, l)| l.cover).collect();
    written.sort();
    assert_eq!(written, vec!["dental".to_string(), "group-life".to_string()]);
}
