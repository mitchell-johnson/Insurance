//! The mix-and-match health product: a menu of covers and the rules for combining them.

use chrono::NaiveDate;
use ic_core::command::ReduceCover;
use ic_core::event::Event;
use ic_core::{Command, CoverStatus, LifeId, PolicyId, Reject};
use ic_product::Frequency;
use ic_sim::config::shipped_activations;
use ic_sim::persona::{health_flex, issue_command};
use ic_sim::Cluster;

const MENU: [&str; 4] = ["hospital", "specialist", "dental", "optical"];

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

fn adult() -> NaiveDate {
    d(1985, 6, 1)
}

/// Issue a policy and return what its first period was billed, where it was accepted.
fn issue(c: &mut Cluster, n: u64, people: &[(NaiveDate, &[&str])]) -> Result<(PolicyId, i64), Reject> {
    let policy = health_flex(n, start(), people, Frequency::Yearly);
    let id = policy.policy.clone();
    c.take_events();
    c.submit(0, "new-business", issue_command(policy))?;
    let billed = c
        .take_events()
        .into_iter()
        .find_map(|e| match e.event {
            Event::PremiumDue { total_minor, .. } => Some(total_minor),
            _ => None,
        })
        .expect("the first period is billed at issue");
    Ok((id, billed))
}

fn give_up(c: &mut Cluster, policy: &PolicyId, cover: &str, life: &str) -> Result<(), Reject> {
    let command = Command::ReduceCover(Box::new(ReduceCover {
        policy: policy.clone(),
        cover: cover.to_string(),
        life: LifeId(life.to_string()),
        sum_insured_minor: 0,
    }));
    c.submit(0, "policy-servicing", command)
}

fn holds(c: &Cluster, policy: &PolicyId, cover: &str, life: &str) -> bool {
    let today = c.state(0).business_date.unwrap();
    c.state(0).policies[policy]
        .covers
        .get(cover)
        .is_some_and(|k| k.status == CoverStatus::InForce && k.layers.iter().any(|l| l.life.0 == life && l.in_force_on(today)))
}

/// Purpose: on one family policy each person holds their own set of covers; a person
/// cannot be given specialist cover without hospital cover; and hospital cover cannot be
/// given up while that person's specialist cover remains, though it can once the
/// specialist cover has gone. Another person's covers neither satisfy nor break the rule.
/// Why it matters: specialist cover is priced and worded as an addition to hospital
/// cover. A policy holding it alone is a product nobody designed, priced or filed, and a
/// rule checked across the family instead of per person would let one adult's hospital
/// cover stand in for another's.
#[test]
fn each_life_holds_its_own_covers_and_the_combination_rules_hold_per_life() {
    let mut c = cluster();
    let child = d(2018, 4, 9);
    // The second adult's hospital cover does not let the first hold specialist alone.
    let alone = issue(&mut c, 1, &[(adult(), &["specialist"]), (adult(), &["hospital"])]);
    assert_eq!(alone.unwrap_err().code, "composition");

    let (family, _) = issue(
        &mut c,
        2,
        &[(adult(), &["hospital", "specialist"]), (adult(), &["hospital"]), (child, &["dental"])],
    )
    .unwrap();
    assert!(holds(&c, &family, "specialist", "life-1"));
    assert!(!holds(&c, &family, "specialist", "life-2"));
    assert!(holds(&c, &family, "dental", "life-3"));
    assert!(!holds(&c, &family, "hospital", "life-3"));

    assert_eq!(code(give_up(&mut c, &family, "hospital", "life-1")), "composition");
    assert!(holds(&c, &family, "hospital", "life-1"));
    assert_eq!(code(give_up(&mut c, &family, "dental", "life-1")), "unknown_life");
    give_up(&mut c, &family, "specialist", "life-1").unwrap();
    give_up(&mut c, &family, "hospital", "life-1").unwrap();
    assert!(!holds(&c, &family, "hospital", "life-1"));
    // The others are as they were.
    assert!(holds(&c, &family, "hospital", "life-2"));
    assert!(holds(&c, &family, "dental", "life-3"));
    assert!(c.end_of_day().unwrap().breaks.is_empty());
}

/// Purpose: for every subset of the menu, one person's policy is either refused by the
/// combination rules or billed the sum of what each chosen cover costs on its own, less
/// the bundle discount once three covers are held.
/// Why it matters: the product promises that a customer's premium is the sum of the
/// covers they chose under the discount chain, for any valid choice. A combination that
/// priced differently from its parts would be a premium nobody can explain to the
/// customer or the actuary.
#[test]
fn any_valid_subset_is_billed_the_sum_of_its_covers_under_the_bundle_discount() {
    let mut c = cluster();
    let product = c.state(0).catalog.product_on("health-flex", start()).unwrap().clone();
    let mut n = 0;
    let mut bill = |c: &mut Cluster, chosen: &[&str]| {
        n += 1;
        issue(c, n, &[(adult(), chosen)]).map(|(_, billed)| billed)
    };
    // What each cover costs alone. Specialist cannot be held alone, so its price is what it adds to hospital.
    let hospital = bill(&mut c, &["hospital"]).unwrap();
    let alone: Vec<i64> = MENU
        .iter()
        .map(|cover| match *cover {
            "specialist" => bill(&mut c, &["hospital", "specialist"]).unwrap() - hospital,
            other => bill(&mut c, &[other]).unwrap(),
        })
        .collect();
    assert!(alone.iter().all(|p| *p > 0));

    let (mut valid, mut refused) = (0, 0);
    for mask in 1u32..16 {
        let chosen: Vec<&str> = MENU.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, c)| *c).collect();
        let sum: i64 = MENU.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(i, _)| alone[i]).sum();
        let allowed = product.check_composition(&chosen.iter().copied().collect()).is_ok();
        match bill(&mut c, &chosen) {
            Err(e) => {
                assert!(!allowed, "{chosen:?} was refused: {e:?}");
                assert_eq!(e.code, "composition");
                refused += 1;
            }
            Ok(billed) => {
                assert!(allowed, "{chosen:?} should have been refused");
                let expected = if chosen.len() >= 3 { sum * 95 / 100 } else { sum };
                // Each cover's premium and tax is rounded on its own line.
                assert!(
                    (billed - expected).abs() <= 2 * chosen.len() as i64,
                    "{chosen:?}: billed {billed}, expected {expected}"
                );
                valid += 1;
            }
        }
    }
    // Specialist without hospital is the only rule a single life can break: four of the fifteen subsets.
    assert_eq!((valid, refused), (11, 4));
}
