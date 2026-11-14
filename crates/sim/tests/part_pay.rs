//! A part-pay (contributory) scheme: each member of a category pays a share of
//! their own automatic cover, billed to them; the employer pays the rest.

use chrono::NaiveDate;
use ic_core::command::MemberChange;
use ic_core::event::Event;
use ic_core::{Command, LifeId, PolicyId, Reject, Standing};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money};
use ic_product::load_product;
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn staff() -> LifeId {
    LifeId("staff-1".into())
}

/// The shipped group life product with staff paying 40% of their own automatic cover.
fn part_pay_source(percent: &str) -> String {
    include_str!("../../../products/group_life.star")
        .replace("code = \"group-life\",\n    name", "code = \"group-life-part-pay\",\n    name")
        .replace(
            "benefit_category(\"staff\", salary_multiple = \"2\", max_sum_insured = dollars(1000000))",
            &format!("benefit_category(\"staff\", salary_multiple = \"2\", max_sum_insured = dollars(1000000), member_pays_percent = \"{percent}\")"),
        )
}

fn cluster() -> (Cluster, PartitionId, PolicyId) {
    let mut c = Cluster::in_memory(1, 7);
    c.open(d(2027, 11, 1)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let product = load_product("group_life_part_pay.star", &part_pay_source("40")).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let mut issue = group_scheme(
        20,
        d(2027, 11, 1),
        vec![
            member("staff-1", d(1985, 1, 1), "staff", 60_000_00),
            member("exec-1", d(1975, 1, 1), "executive", 240_000_00),
        ],
    );
    issue.product_code = "group-life-part-pay".into();
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    (c, p, id)
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

fn employer_owes(c: &Cluster, p: PartitionId, id: &PolicyId) -> i64 {
    c.state(p).policies[id].policyholder_outstanding_minor()
}

fn member_owes(c: &Cluster, p: PartitionId, id: &PolicyId) -> i64 {
    c.state(p).policies[id].member_outstanding_minor(&staff())
}

fn last_lines(seen: &[Event]) -> Vec<(LifeId, i64)> {
    seen.iter()
        .rev()
        .find_map(|e| match e {
            Event::SchemeInvoiceChunk { lines, .. } => Some(lines.clone()),
            _ => None,
        })
        .unwrap()
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

/// Purpose: on a part-pay scheme the staff category pays 40% of its own automatic cover.
/// The month's invoice carries the employer's 60% on the member's line (6.60 of 11.00) and
/// the executive's whole premium; the member is billed the other 4.40 as their own bill,
/// with its own standing, and paying it touches nothing of the employer's. A member who
/// joins in the month is charged their share of the days left, and the employer the rest.
/// Why it matters: many schemes are contributory. If the member's share stayed on the
/// employer's invoice the employer would be charged for what its staff agreed to pay; if it
/// were billed to the member as well, the cover would be paid for twice.
#[test]
fn a_member_pays_their_share_of_their_own_cover_and_the_employer_the_rest() {
    let (mut c, p, id) = cluster();
    let seen = events(&mut c);
    assert_eq!(last_lines(&seen), vec![(LifeId("exec-1".into()), 90_62), (staff(), 6_60)]);
    assert_eq!(employer_owes(&c, p, &id), 90_62 + 6_60);
    assert_eq!(member_owes(&c, p, &id), 4_40);
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::MemberPremiumBilled { life, total_minor: 4_40, .. } if *life == staff())));

    let pays = Command::ApplyMemberReceipt {
        policy: id.clone(),
        life: staff(),
        receipt: "m-1".into(),
        amount: Money::new(4_40, NZD),
    };
    c.submit(p, "cash-matching", pays).unwrap();
    assert_eq!((member_owes(&c, p, &id), employer_owes(&c, p, &id)), (0, 90_62 + 6_60));
    days(&mut c, 2);
    assert_eq!(c.state(p).policies[&id].member_standing.get(&staff()).copied(), Some(Standing::Paid));

    // A staff member joining on 16 November: half the month, split the same way.
    c.take_events();
    let joiner = Command::ApplyMembership {
        policy: id.clone(),
        changes: vec![MemberChange::Joined {
            life: member("staff-2", d(1985, 1, 1), "staff", 60_000_00),
            effective: d(2027, 11, 16),
        }],
    };
    c.submit(p, "group-admin", joiner).unwrap();
    let seen = events(&mut c);
    let theirs: i64 = seen
        .iter()
        .filter_map(|e| match e {
            Event::MemberPremiumBilled { life, total_minor, .. } if life.0 == "staff-2" => Some(*total_minor),
            _ => None,
        })
        .sum();
    let employers: i64 = seen
        .iter()
        .filter_map(|e| match e {
            Event::PremiumAdjusted { life, total_minor, .. } if life.0 == "staff-2" => Some(*total_minor),
            _ => None,
        })
        .sum();
    // 15 of 30 days of 11.00: 5.50, of which 2.20 is the member's.
    assert_eq!((theirs, employers), (2_20, 3_30));
}

/// Purpose: a part-pay member who does not pay their share within the lapse period leaves the
/// scheme's cover on the day it lapses (the employer's part ends with it), what they owed is
/// written off and their own collection ends; inside the reinstatement window, once their
/// credit holds what was written off and their share since, they come back from the day they
/// left with no gap, and the employer is billed its part for those periods again.
/// Why it matters: the member's share is what keeps their cover; leaving them covered on the
/// employer's part alone would give cover the scheme's terms do not, and a member who pays
/// late must be able to return without a new application, as a retail policyholder can.
#[test]
fn a_member_who_does_not_pay_their_share_leaves_and_can_come_back() {
    let (mut c, p, id) = cluster();
    let employer_pays = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "emp-1".into(),
        amount: Money::new(12 * (90_62 + 6_60), NZD),
    };
    c.submit(p, "cash-matching", employer_pays).unwrap();
    days(&mut c, 152);
    let policy = c.state(p).policies[&id].clone();
    let today = c.business_date().unwrap();
    assert!(!policy.lives[&staff()].in_force_on(today), "the member has left the scheme's cover");
    let lapse = policy.member_lapses[&staff()].clone();
    assert!(lapse.left && lapse.arrears_minor > 0);
    assert_eq!(member_owes(&c, p, &id), 0, "what they owed is written off");
    let seen = events(&mut c);
    assert!(seen.iter().any(|e| matches!(e, Event::MemberLeft { life, .. } if *life == staff())));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::PaymentAuthorityCancelled { life: Some(l), .. } if *l == staff())));

    // Back, paying what was written off and their share of the month since.
    days(&mut c, 10);
    let reinstate = Command::ReinstateMemberCover {
        policy: id.clone(),
        life: staff(),
        reference: "rein-1".into(),
    };
    assert_eq!(code(c.submit(p, "group-admin", reinstate.clone())), "arrears_unpaid");
    let pays = Command::ApplyMemberReceipt {
        policy: id.clone(),
        life: staff(),
        receipt: "m-1".into(),
        amount: Money::new(lapse.arrears_minor + 2 * 4_40, NZD),
    };
    c.submit(p, "cash-matching", pays).unwrap();
    c.take_events();
    c.submit(p, "group-admin", reinstate).unwrap();
    let policy = c.state(p).policies[&id].clone();
    let today = c.business_date().unwrap();
    assert!(policy.lives[&staff()].in_force_on(today) && policy.lives[&staff()].in_force_on(lapse.lapsed_on));
    assert!(policy.member_lapses.is_empty());
    assert_eq!(member_owes(&c, p, &id), 0, "the credit paid it");
    let seen = events(&mut c);
    let billed = seen
        .iter()
        .find_map(|e| match e {
            Event::MemberCoverReinstated { billed_minor, .. } => Some(*billed_minor),
            _ => None,
        })
        .unwrap();
    assert!(billed > 0);
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::PremiumAdjusted { life, total_minor, .. } if *life == staff() && *total_minor > 0)),
        "the employer is billed its part again"
    );
    days(&mut c, 31);
}

/// Purpose: a member's share is a percentage above 0 and at most 100; anything else is
/// refused when the product loads.
/// Why it matters: a share of 0 would bill members nothing while marking them part-payers,
/// and one above 100 would bill them more than the cover costs and the employer a credit.
#[test]
fn a_share_outside_0_to_100_does_not_load() {
    for bad in ["0", "100.5", "-10"] {
        let error = load_product("bad.star", &part_pay_source(bad)).unwrap_err().to_string();
        assert!(error.contains("member_pays_percent"), "{bad}: {error}");
    }
    assert!(load_product("full.star", &part_pay_source("100")).is_ok());
}
