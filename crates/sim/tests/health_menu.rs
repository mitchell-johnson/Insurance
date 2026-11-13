//! A health menu on a group scheme: covers each member chooses for
//! themselves and for their dependants, each paid for by the employer or by
//! the member, on the shipped Workplace Benefits product.

use chrono::NaiveDate;
use ic_core::command::{LifeSpec, MemberChange};
use ic_core::event::Event;
use ic_core::voluntary::credit_minor;
use ic_core::{ChosenCover, Command, LayerOrigin, LifeFactors, LifeId, LifeKind, PolicyId, Reject, SetMemberCovers, SplitScheme};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn life(id: &str) -> LifeId {
    LifeId(id.to_string())
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn events(c: &mut Cluster) -> Vec<Event> {
    c.take_events().into_iter().map(|r| r.event).collect()
}

fn dependant(id: &str, of: &str, dob: NaiveDate) -> LifeSpec {
    LifeSpec {
        id: life(id),
        kind: LifeKind::Dependant { of: life(of), party: None },
        factors: LifeFactors {
            date_of_birth: Some(dob),
            ..LifeFactors::default()
        },
    }
}

fn membership(c: &mut Cluster, p: PartitionId, id: &PolicyId, changes: Vec<MemberChange>) -> Result<(), Reject> {
    c.submit(p, "group-admin", Command::ApplyMembership { policy: id.clone(), changes })
}

fn chosen(code: &str, excess: Option<&str>, member_pays: bool) -> ChosenCover {
    ChosenCover {
        code: code.into(),
        excess_option: excess.map(str::to_string),
        member_pays,
    }
}

fn choose(c: &mut Cluster, p: PartitionId, id: &PolicyId, who: &str, effective: NaiveDate, covers: Vec<ChosenCover>) -> Result<(), Reject> {
    let cmd = SetMemberCovers {
        policy: id.clone(),
        life: life(who),
        covers,
        effective,
        reference: "enrol-1".into(),
    };
    c.submit(p, "group-admin", Command::SetMemberCovers(Box::new(cmd)))
}

fn employer_owes(c: &Cluster, p: PartitionId, id: &PolicyId) -> i64 {
    c.state(p).policies[id].policyholder_outstanding_minor()
}

fn member_owes(c: &Cluster, p: PartitionId, id: &PolicyId, who: &str) -> i64 {
    c.state(p).policies[id].member_outstanding_minor(&life(who))
}

/// A Workplace Benefits scheme from 1 November 2027. A staff member aged 42
/// on 60,000 (life cover 11.00 a month), an executive (90.62), and the staff
/// member's child aged 12, who has no cover until one is chosen. For the
/// staff member: the hospital plan at the standard excess, paid by the
/// employer (1,200 a year: 100.00 a month and 15.00 GST), and the specialist
/// upgrade paid by the member (300 a year: 25.00 and 3.75). For the child:
/// the hospital plan at the high excess, paid by the member (360 a year less
/// a fifth: 24.00 and 3.60).
fn scheme(c: &mut Cluster) -> (PartitionId, PolicyId) {
    let mut issue = group_scheme(
        31,
        d(2027, 11, 1),
        vec![
            member("staff-1", d(1985, 1, 1), "staff", 60_000_00),
            member("exec-1", d(1975, 1, 1), "executive", 240_000_00),
        ],
    );
    issue.product_code = "group-benefits".into();
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    assert_eq!(employer_owes(c, p, &id), 11_00 + 90_62);
    let joins = MemberChange::Joined {
        life: dependant("staff-1-child", "staff-1", d(2015, 6, 1)),
        effective: d(2027, 11, 1),
    };
    membership(c, p, &id, vec![joins]).unwrap();
    assert_eq!(employer_owes(c, p, &id), 11_00 + 90_62, "a dependant has no cover the scheme gives by formula");
    choose(
        c,
        p,
        &id,
        "staff-1",
        d(2027, 11, 1),
        vec![chosen("hospital", Some("standard"), false), chosen("specialist", None, true)],
    )
    .unwrap();
    choose(c, p, &id, "staff-1-child", d(2027, 11, 1), vec![chosen("hospital", Some("high"), true)]).unwrap();
    (p, id)
}

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 7);
    c.open(d(2027, 11, 1)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

/// Purpose: a member's chosen covers are priced on that life's own age and excess level,
/// the employer's invoice carries only the covers the employer pays for, the member is
/// billed for their own upgrade and for their child's cover in the member's name, a
/// dependant adds nothing by formula and is not counted as a member, and a change of
/// excess part way through a billed month ends one layer, starts another and bills the
/// difference for the days left while the unchanged cover is not touched.
/// Why it matters: an employer health scheme splits one family's cover between two
/// payers. If the split were wrong the employer would be invoiced for upgrades staff
/// chose for themselves, or staff would be billed for the plan their employer provides;
/// and a dependant priced as a member would be given life cover nobody asked for.
#[test]
fn chosen_covers_are_billed_to_whoever_pays_each_and_a_change_touches_only_what_changed() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    // November was billed at issue: the choices are adjustments to it, from the first.
    assert_eq!(employer_owes(&c, p, &id), 11_00 + 90_62 + 115_00);
    assert_eq!(member_owes(&c, p, &id, "staff-1"), 28_75 + 27_60);
    assert_eq!(member_owes(&c, p, &id, "staff-1-child"), 0, "a dependant is never billed");
    let seen = events(&mut c);
    let added: Vec<(String, LifeId)> = seen
        .iter()
        .filter_map(|e| match e {
            Event::LayerAdded { layer, origin, .. } if *origin == LayerOrigin::MemberChoice => Some((layer.cover.clone(), layer.life.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        added,
        vec![
            ("hospital".to_string(), life("staff-1")),
            ("specialist".to_string(), life("staff-1")),
            ("hospital".to_string(), life("staff-1-child")),
        ]
    );
    assert!(seen.iter().any(
        |e| matches!(e, Event::MemberCoversSet { life: l, payer, covers, .. } if *l == life("staff-1-child") && *payer == life("staff-1") && covers.len() == 1)
    ));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::MemberJoined { life: l, covers, .. } if *l == life("staff-1-child") && covers.is_empty())));

    // The same set sent again changes nothing and says nothing.
    choose(&mut c, p, &id, "staff-1-child", d(2027, 11, 1), vec![chosen("hospital", Some("high"), true)]).unwrap();
    let again = events(&mut c);
    assert!(
        !again.iter().any(|e| matches!(
            e,
            Event::LayerAdded { .. } | Event::LayerEnded { .. } | Event::MemberCoversSet { .. } | Event::MemberPremiumBilled { .. }
        )),
        "{again:?}"
    );

    // December's invoice: the employer's lines, and two bills in the member's name.
    days(&mut c, 30);
    assert_eq!(c.business_date(), Some(d(2027, 12, 1)));
    let seen = events(&mut c);
    let (lines, members) = seen
        .iter()
        .find_map(|e| match e {
            Event::SchemeInvoiceChunk { lines, complete: true, .. } => Some(lines.clone()),
            _ => None,
        })
        .zip(seen.iter().find_map(|e| match e {
            Event::PremiumDue { members, .. } => *members,
            _ => None,
        }))
        .expect("December was invoiced");
    assert_eq!(members, 2, "the child is covered but is not a member");
    let line = |who: &str| lines.iter().find(|(l, _)| *l == life(who)).map(|(_, m)| *m);
    assert_eq!(line("staff-1"), Some(11_00 + 115_00));
    assert_eq!(line("staff-1-child"), Some(0));
    let billed: Vec<(LifeId, i64)> = seen
        .iter()
        .filter_map(|e| match e {
            Event::MemberPremiumBilled {
                life,
                total_minor,
                adjustment: false,
                ..
            } => Some((life.clone(), *total_minor)),
            _ => None,
        })
        .collect();
    assert_eq!(billed, vec![(life("staff-1"), 28_75), (life("staff-1"), 27_60)]);

    // On 15 December the member moves their own hospital plan to no excess: a fifth more,
    // 23.00 a month with GST, for 17 of December's 31 days (10.97 and 1.65 GST).
    days(&mut c, 14);
    let before = (employer_owes(&c, p, &id), member_owes(&c, p, &id, "staff-1"));
    choose(
        &mut c,
        p,
        &id,
        "staff-1",
        d(2027, 12, 15),
        vec![chosen("hospital", Some("nil"), false), chosen("specialist", None, true)],
    )
    .unwrap();
    assert_eq!(employer_owes(&c, p, &id) - before.0, 12_62);
    assert_eq!(member_owes(&c, p, &id, "staff-1"), before.1, "the upgrade the member pays for did not change");
    let policy = c.state(p).policies[&id].clone();
    let mine = |code: &str| -> Vec<(u32, Option<NaiveDate>, Option<String>)> {
        policy.covers[code]
            .layers
            .iter()
            .filter(|l| l.life == life("staff-1"))
            .map(|l| (l.id, l.end, l.excess_option.clone()))
            .collect()
    };
    assert_eq!(
        mine("hospital"),
        vec![(1, Some(d(2027, 12, 15)), Some("standard".into())), (3, None, Some("nil".into()))]
    );
    assert_eq!(mine("specialist"), vec![(1, None, None)]);
    let seen = events(&mut c);
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::LayerEnded { cover, layer: 1, effective, .. } if cover == "hospital" && *effective == d(2027, 12, 15))));
    assert!(!seen.iter().any(|e| matches!(e, Event::LayerEnded { cover, .. } if cover == "specialist")));
}

/// Purpose: each choice the menu does not allow is refused with its own code and changes
/// nothing: an upgrade without the plan it needs, a cover that is not on the menu, a plan
/// without one of its excess levels, a cover named twice, a life that is not on the
/// scheme, a date before a later change or after today, a dependant joining through
/// someone who is not a member, a member moved to another scheme without their
/// dependants, a reference that is not a plain identifier, and a policy that is not a
/// scheme.
/// Why it matters: these are the mistakes an enrolment file makes. Each one accepted
/// would be cover on risk that the product does not offer or that nobody can price, and
/// a refusal that did not say which rule was broken could not be fixed by the employer.
#[test]
fn a_choice_the_menu_does_not_allow_is_refused_and_changes_nothing() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    let before = c.state(p).policies[&id].clone();
    let day = d(2027, 11, 1);
    let mut refuse = |who: &str, on: NaiveDate, covers: Vec<ChosenCover>| choose(&mut c, p, &id, who, on, covers).unwrap_err().code;
    assert_eq!(refuse("staff-1", day, vec![chosen("specialist", None, false)]), "composition");
    assert_eq!(refuse("staff-1", day, vec![chosen("group-life", None, false)]), "not_on_menu");
    assert_eq!(refuse("staff-1", day, vec![chosen("hospital", None, false)]), "excess_option");
    assert_eq!(
        refuse("staff-1", day, vec![chosen("dental", None, false), chosen("dental", None, true)]),
        "invalid_choice"
    );
    assert_eq!(refuse("nobody", day, vec![chosen("dental", None, false)]), "unknown_member");
    assert_eq!(refuse("staff-1", d(2027, 11, 2), vec![]), "future_dated");
    assert_eq!(refuse("staff-1", d(2026, 1, 1), vec![]), "backdating_limit");
    days(&mut c, 9);
    choose(&mut c, p, &id, "exec-1", d(2027, 11, 8), vec![chosen("dental", None, false)]).unwrap();
    assert_eq!(
        choose(&mut c, p, &id, "exec-1", d(2027, 11, 5), vec![]).unwrap_err().code,
        "later_change",
        "the past before a later change is not rewritten"
    );
    choose(&mut c, p, &id, "exec-1", d(2027, 11, 8), vec![]).unwrap();
    let after = c.state(p).policies[&id].clone();
    assert_eq!(after.covers["hospital"], before.covers["hospital"]);
    assert_eq!(after.covers["specialist"], before.covers["specialist"]);
    assert!(
        after.covers["dental"].layers.iter().all(|l| l.end == Some(l.start)),
        "chosen and given up the same day"
    );

    let through = |of: &str| MemberChange::Joined {
        life: dependant("another-child", of, d(2016, 1, 1)),
        effective: d(2027, 11, 1),
    };
    assert_eq!(membership(&mut c, p, &id, vec![through("staff-1-child")]).unwrap_err().code, "unknown_member");
    assert_eq!(membership(&mut c, p, &id, vec![through("nobody")]).unwrap_err().code, "unknown_member");

    let split = |lives: &[&str]| {
        Command::SplitScheme(Box::new(SplitScheme {
            policy: id.clone(),
            new_policy: PolicyId("pol-00000032".into()),
            new_number: "WPL00000032".into(),
            lives: lives.iter().map(|l| life(l)).collect(),
            effective: d(2027, 12, 1),
        }))
    };
    assert_eq!(c.submit(p, "group-admin", split(&["staff-1"])).unwrap_err().code, "dependants");
    assert_eq!(c.submit(p, "group-admin", split(&["staff-1-child"])).unwrap_err().code, "dependants");

    // A reference that is not a plain identifier, and a policy that is not a scheme.
    let mut cmd = SetMemberCovers {
        policy: id.clone(),
        life: life("staff-1"),
        covers: vec![],
        effective: d(2027, 11, 10),
        reference: "see the email".into(),
    };
    let send = |c: &mut Cluster, cmd: &SetMemberCovers| c.submit(p, "group-admin", Command::SetMemberCovers(Box::new(cmd.clone())));
    assert_eq!(send(&mut c, &cmd).unwrap_err().code, "invalid_choice");
    let retail = term_life(
        33,
        d(2027, 11, 10),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    cmd.reference = "enrol-2".into();
    cmd.policy = retail.policy.clone();
    c.submit(p, "new-business", issue_command(retail)).unwrap();
    assert_eq!(send(&mut c, &cmd).unwrap_err().code, "not_a_scheme");
}

fn pay(id: &PolicyId, who: &str, receipt: &str, minor: i64) -> Command {
    Command::ApplyMemberReceipt {
        policy: id.clone(),
        life: life(who),
        receipt: receipt.into(),
        amount: Money::new(minor, NZD),
    }
}

/// Purpose: when a member leaves, their dependants leave the same day and every chosen
/// cover of the family ends with credit to whoever paid for it; reinstating the member
/// brings the dependants and the covers back as they were; and a split moves a member,
/// their dependant and their chosen covers to the new scheme together.
/// Why it matters: a dependant is covered only through the member. One left behind when
/// the member goes would be a child on risk on an employer's scheme with nobody employed
/// there and nobody billed, and a family split across two schemes by a demerger would
/// have its claims and its bills in different places.
#[test]
fn dependants_and_chosen_covers_follow_their_member_out_back_and_across_a_split() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    c.submit(p, "cash-matching", pay(&id, "staff-1", "m-1", 28_75 + 27_60)).unwrap();
    days(&mut c, 10);
    c.take_events();
    let leave = MemberChange::Left {
        life: life("staff-1"),
        effective: d(2027, 11, 11),
    };
    membership(&mut c, p, &id, vec![leave]).unwrap();
    let policy = c.state(p).policies[&id].clone();
    assert_eq!(policy.lives[&life("staff-1-child")].left, Some(d(2027, 11, 11)));
    let open = |policy: &ic_core::Policy| {
        policy
            .covers
            .values()
            .flat_map(|c| &c.layers)
            .filter(|l| l.origin == LayerOrigin::MemberChoice && l.end.is_none())
            .count()
    };
    assert_eq!(open(&policy), 0);
    // 20 of November's 30 days come back: the employer's life and hospital premium, and
    // what the member paid for the upgrade and the child.
    assert_eq!(employer_owes(&c, p, &id), 90_62 + (11_00 + 115_00) - (7_33 + 76_67));
    // The member had paid November: what comes back is theirs, held as credit.
    assert_eq!(member_owes(&c, p, &id, "staff-1"), 0);
    assert_eq!(credit_minor(&c.state(p).ledger, &c.state(p).policies[&id], &life("staff-1")), 1_916 + 1_840);
    let seen = events(&mut c);
    let left: Vec<&LifeId> = seen
        .iter()
        .filter_map(|e| match e {
            Event::MemberLeft { life, .. } => Some(life),
            _ => None,
        })
        .collect();
    assert_eq!(left, vec![&life("staff-1"), &life("staff-1-child")]);
    assert_eq!(seen.iter().filter(|e| matches!(e, Event::LayerEnded { .. })).count(), 3);

    membership(&mut c, p, &id, vec![MemberChange::Reinstated { life: life("staff-1") }]).unwrap();
    let policy = c.state(p).policies[&id].clone();
    assert_eq!(policy.lives[&life("staff-1-child")].left, None);
    assert_eq!(open(&policy), 3);
    assert_eq!(employer_owes(&c, p, &id), 11_00 + 90_62 + 115_00);
    assert_eq!(
        credit_minor(&c.state(p).ledger, &c.state(p).policies[&id], &life("staff-1")),
        0,
        "the credit met the bill"
    );
    assert_eq!(member_owes(&c, p, &id, "staff-1"), 0);
    let restored = events(&mut c)
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::LayerAdded {
                    origin: LayerOrigin::MemberChoice,
                    ..
                }
            )
        })
        .count();
    assert_eq!(restored, 3);

    // The family moves to a new scheme from 1 December.
    days(&mut c, 20);
    c.submit(p, "cash-matching", pay(&id, "staff-1", "m-2", 28_75 + 27_60)).unwrap();
    let split = Command::SplitScheme(Box::new(SplitScheme {
        policy: id.clone(),
        new_policy: PolicyId("pol-00000032".into()),
        new_number: "WPL00000032".into(),
        lives: vec![life("staff-1"), life("staff-1-child")],
        effective: d(2027, 12, 1),
    }));
    c.submit(p, "group-admin", split).unwrap();
    let new = c.state(p).policies[&PolicyId("pol-00000032".into())].clone();
    assert_eq!(new.lives.len(), 2);
    assert_eq!(open(&new), 3);
    assert_eq!(open(&c.state(p).policies[&id]), 0);
    // December, paid on the old scheme, came back as the member's credit there and went
    // with them: it meets December's bill on the new scheme.
    let new_id = PolicyId("pol-00000032".into());
    assert!(events(&mut c)
        .iter()
        .any(|e| matches!(e, Event::MemberCreditMoved { life: l, amount_minor, .. } if *l == life("staff-1") && *amount_minor == 28_75 + 27_60)));
    assert_eq!(credit_minor(&c.state(p).ledger, &c.state(p).policies[&id], &life("staff-1")), 0);
    days(&mut c, 1);
    assert_eq!(employer_owes(&c, p, &new_id), 11_00 + 115_00);
    assert_eq!(member_owes(&c, p, &new_id, "staff-1"), 0);
    assert_eq!(credit_minor(&c.state(p).ledger, &c.state(p).policies[&new_id], &life("staff-1")), 0);
}

/// Purpose: a member who owes for cover they pay for can still be moved in a split. What
/// they owe for the time on the old scheme stays owed there, the bill for a period the new
/// scheme now covers is taken back, and the new scheme bills them from its first period.
/// Why it matters: on a scheme where staff pay for upgrades, somebody always owes
/// something. A demerger that had to wait until every member's balance was nil would
/// never happen, and a debt carried to the new scheme would be chased by an employer it
/// was never owed to.
#[test]
fn a_member_in_debt_moves_in_a_split_and_the_debt_stays_where_it_arose() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    days(&mut c, 30);
    assert_eq!(member_owes(&c, p, &id, "staff-1"), 2 * (28_75 + 27_60), "November and December, unpaid");
    let new_id = PolicyId("pol-00000033".into());
    let split = Command::SplitScheme(Box::new(SplitScheme {
        policy: id.clone(),
        new_policy: new_id.clone(),
        new_number: "WPL00000033".into(),
        lives: vec![life("staff-1"), life("staff-1-child")],
        effective: d(2027, 12, 1),
    }));
    c.submit(p, "group-admin", split).unwrap();
    assert_eq!(member_owes(&c, p, &id, "staff-1"), 28_75 + 27_60, "November is still owed to the old scheme");
    assert!(!events(&mut c).iter().any(|e| matches!(e, Event::MemberCreditMoved { .. })));
    days(&mut c, 1);
    assert_eq!(member_owes(&c, p, &new_id, "staff-1"), 28_75 + 27_60, "December is the new scheme's");
    // The old debt can still be paid where it is owed.
    c.submit(p, "cash-matching", pay(&id, "staff-1", "m-9", 28_75 + 27_60)).unwrap();
    assert_eq!(member_owes(&c, p, &id, "staff-1"), 0);
}

/// Purpose: a member who stops paying loses, after the product's lapse period, the cover
/// they pay for: their own upgrade and their dependant's plan. The plan the employer pays
/// for carries on, and the member and the dependant stay on the scheme. Until then a
/// claim's cover check counts the member's debt against the member's family only.
/// Why it matters: the member's debt is the member's. Counted against a colleague it
/// would hold up a claim on cover that is fully paid. Cover left running unpaid is risk
/// carried for nothing, and ending the employer-paid plan with it would punish the
/// employer's staff for a bill that was never the employer's.
#[test]
fn a_member_who_stops_paying_loses_their_upgrade_and_their_dependants_cover_only() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    let employer_pays = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "emp-1".into(),
        amount: Money::new(12 * (11_00 + 90_62 + 115_00), NZD),
    };
    c.submit(p, "cash-matching", employer_pays).unwrap();
    days(&mut c, 40);
    // A claim sees the standing of whoever pays for the life's cover: the member's unpaid
    // bills count against the member and their child, and against nobody else.
    let on = c.business_date().unwrap();
    let facts = |cover: &str, who: &str| ic_core::cover_facts(c.state(p), &id.0, cover, Some(who), on).unwrap();
    let child = facts("hospital", "staff-1-child");
    assert_eq!(
        (child.standing.as_deref(), child.outstanding_premium_minor),
        (Some("in_grace"), 2 * (28_75 + 27_60))
    );
    assert_eq!(child.chosen_excess_minor, Some(2_000_00));
    let other = facts("group-life", "exec-1");
    assert_eq!((other.standing.as_deref(), other.outstanding_premium_minor), (Some("paid"), 0));
    days(&mut c, 82);
    let policy = c.state(p).policies[&id].clone();
    let today = c.business_date().unwrap();
    let in_force = |code: &str, who: &str| policy.covers[code].layers.iter().any(|l| l.life == life(who) && l.in_force_on(today));
    assert!(in_force("hospital", "staff-1"), "the employer's plan carries on");
    assert!(!in_force("specialist", "staff-1"));
    assert!(!in_force("hospital", "staff-1-child"));
    assert!(policy.lives[&life("staff-1")].in_force_on(today) && policy.lives[&life("staff-1-child")].in_force_on(today));
    assert_eq!(member_owes(&c, p, &id, "staff-1"), 0, "what was owed is written off with the cover");
    let lapsed = events(&mut c).into_iter().find_map(|e| match e {
        Event::MemberCoverLapsed { life, layers, .. } => Some((life, layers)),
        _ => None,
    });
    assert_eq!(
        lapsed,
        Some((life("staff-1"), vec![("specialist".to_string(), 1), ("hospital".to_string(), 2)]))
    );
}

/// Purpose: an employer health scheme with no life cover at all is issued with its members
/// and nothing to bill, is billed as a scheme once members choose from the menu (one
/// invoice for the employer, the member's own bill beside it), takes joiners and leavers
/// through the membership file like any scheme, and refuses both a cover written at issue
/// and the commands that belong to a retail policy.
/// Why it matters: a health-only employer plan is the common case. If the core knew a
/// scheme only by its formula cover, this one would be billed as a retail policy with no
/// layers, which is nothing at all, and every member would be on risk unbilled.
#[test]
fn a_health_only_scheme_is_a_scheme_from_issue_and_is_billed_on_what_members_choose() {
    let mut c = cluster();
    let mut issue = group_scheme(
        41,
        d(2027, 11, 1),
        vec![
            member("staff-1", d(1985, 1, 1), "staff", 60_000_00),
            member("staff-2", d(1995, 1, 1), "staff", 50_000_00),
        ],
    );
    issue.product_code = "group-health".into();
    let mut with_cover = issue.clone();
    with_cover.covers[0].code = "dental".into();
    with_cover.covers[0].life = Some(life("staff-1"));
    issue.covers.clear();
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    assert_eq!(c.submit(p, "new-business", issue_command(with_cover)).unwrap_err().code, "invalid_policy");
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    assert!(c.state(p).policies[&id].is_scheme());
    assert_eq!(employer_owes(&c, p, &id), 0, "nobody has chosen anything yet");

    // Staff-1 (42): hospital at the standard excess from the employer, 115.00 a month with
    // GST. Staff-2 (32): dental paid for themselves.
    choose(&mut c, p, &id, "staff-1", d(2027, 11, 1), vec![chosen("hospital", Some("standard"), false)]).unwrap();
    choose(&mut c, p, &id, "staff-2", d(2027, 11, 1), vec![chosen("dental", None, true)]).unwrap();
    assert_eq!(employer_owes(&c, p, &id), 115_00);
    let dental = member_owes(&c, p, &id, "staff-2");
    assert!(dental > 0);

    let joins = MemberChange::Joined {
        life: dependant("staff-1-partner", "staff-1", d(1986, 2, 2)),
        effective: d(2027, 11, 1),
    };
    membership(&mut c, p, &id, vec![joins]).unwrap();
    choose(
        &mut c,
        p,
        &id,
        "staff-1-partner",
        d(2027, 11, 1),
        vec![chosen("hospital", Some("standard"), false)],
    )
    .unwrap();
    assert_eq!(employer_owes(&c, p, &id), 2 * 115_00);

    // December is one scheme invoice and one member bill.
    days(&mut c, 30);
    let seen = events(&mut c);
    let invoiced = seen.iter().find_map(|e| match e {
        Event::PremiumDue { total_minor, members, .. } => Some((*total_minor, *members)),
        _ => None,
    });
    assert_eq!(invoiced, Some((2 * 115_00, Some(2))));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::MemberPremiumBilled { life: l, total_minor, .. } if *l == life("staff-2") && *total_minor == dental)));

    // A leaver takes their dependant and their covers with them.
    let leave = MemberChange::Left {
        life: life("staff-1"),
        effective: d(2027, 12, 1),
    };
    membership(&mut c, p, &id, vec![leave]).unwrap();
    assert_eq!(employer_owes(&c, p, &id), 2 * 115_00, "November stands and December is credited in full");

    // What belongs to a retail policy is refused here.
    let unwind = Command::UnwindAfter {
        policy: id.clone(),
        after: d(2027, 11, 15),
        cause: ic_core::UnwindCause::Remediation,
        reference: "rem-1".into(),
    };
    assert_eq!(c.submit(p, "remediation", unwind).unwrap_err().code, "scheme_policy");
}
