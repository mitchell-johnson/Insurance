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

fn add_cover(c: &mut Cluster, caller: &str, policy: &PolicyId, cover: &str, life: &str) -> Result<(), Reject> {
    let command = Command::AddCover(Box::new(ic_core::command::AddedCover {
        excess_option: (cover == "hospital").then(|| "standard".to_string()),
        policy: policy.clone(),
        cover: cover.to_string(),
        life: LifeId(life.to_string()),
        sum_insured_minor: 0,
        loadings: vec![],
        exclusions: vec![],
        reference: "req-1".to_string(),
    }));
    c.submit(0, caller, command)
}

/// The adjustment billed by the last command.
fn adjusted(c: &mut Cluster) -> i64 {
    c.take_events()
        .into_iter()
        .find_map(|e| match e.event {
            Event::PremiumAdjusted { total_minor, .. } => Some(total_minor),
            _ => None,
        })
        .expect("the days left of the billed period are billed for the new cover")
}

/// Purpose: a cover from the menu can be added to a life after issue. It starts on the day
/// it is added, the combination rules are checked on what the life would then hold, the days
/// left of the year already billed are billed at what the cover adds to the whole policy's
/// price, including a bundle discount the new cover brings in, and a cover given up can be
/// taken again as a new layer. Adding a cover the life already holds, one the product does
/// not offer, one for a stranger, or from a module that may not, is refused and changes
/// nothing.
/// Why it matters: the product is sold as a menu the customer can change, and most changes
/// are additions made mid-year. If an addition skipped the combination rules a customer
/// could reach by two steps a set of covers refused in one, and if the part-year premium
/// ignored the discount the customer would be overcharged until the anniversary.
#[test]
fn a_cover_can_be_added_after_issue_and_is_billed_for_the_days_left() {
    let mut c = cluster();
    // Yearly prices at issue, for comparison.
    let (_, hospital) = issue(&mut c, 1, &[(adult(), &["hospital"])]).unwrap();
    let (_, with_specialist) = issue(&mut c, 2, &[(adult(), &["hospital", "specialist"])]).unwrap();
    let (_, all_three) = issue(&mut c, 3, &[(adult(), &["hospital", "specialist", "dental"])]).unwrap();
    let (id, _) = issue(&mut c, 4, &[(adult(), &["hospital"]), (adult(), &["dental"])]).unwrap();
    let (solo, _) = issue(&mut c, 5, &[(adult(), &["hospital"])]).unwrap();
    let (_, dental_alone) = issue(&mut c, 6, &[(adult(), &["dental"])]).unwrap();
    for (n, policy) in [&id, &solo].into_iter().enumerate() {
        let receipt = Command::ApplyReceipt {
            policy: policy.clone(),
            receipt: format!("r-{n}"),
            amount: ic_money::Money::new(20_000_00, ic_money::Currency::NZD),
        };
        c.submit(0, "cash-matching", receipt).unwrap();
    }
    for _ in 0..73 {
        assert!(c.end_of_day().unwrap().breaks.is_empty());
    }
    let today = c.state(0).business_date.unwrap();
    let year_end = d(2028, 11, 15);
    let part = |yearly: i64| yearly * (year_end - today).num_days() / (year_end - start()).num_days();

    let before = (c.state(0).ledger.head(), c.state(0).policies[&id].clone());
    assert_eq!(code(add_cover(&mut c, "claims", &id, "optical", "life-1")), "caller_not_allowed");
    assert_eq!(code(add_cover(&mut c, "policy-servicing", &id, "specialist", "life-2")), "composition");
    assert_eq!(code(add_cover(&mut c, "policy-servicing", &id, "hospital", "life-1")), "cover_held");
    assert_eq!(code(add_cover(&mut c, "policy-servicing", &id, "physio", "life-1")), "unknown_cover");
    assert_eq!(code(add_cover(&mut c, "policy-servicing", &id, "optical", "life-9")), "unknown_life");
    assert_eq!((c.state(0).ledger.head(), c.state(0).policies[&id].clone()), before);

    // A cover nobody on the policy had: it joins the policy today.
    c.take_events();
    add_cover(&mut c, "policy-servicing", &id, "optical", "life-2").unwrap();
    assert!(adjusted(&mut c) > 0);
    let optical = &c.state(0).policies[&id].covers["optical"];
    assert_eq!((optical.start, optical.layers[0].start, optical.layers[0].end), (today, today, None));
    assert!(holds(&c, &id, "optical", "life-2") && !holds(&c, &id, "optical", "life-1"));
    // A cover someone else already had: the other person's is untouched.
    add_cover(&mut c, "underwriting", &id, "dental", "life-1").unwrap();
    // The policy already had its third cover from the step before, so this one comes at the
    // discounted price and the discount on the others is not counted a second time.
    let fourth = adjusted(&mut c);
    let expected = part(dental_alone * 95 / 100);
    assert!(
        (fourth - expected).abs() <= 4,
        "dental for the first person: billed {fourth}, expected {expected}"
    );
    let dental = &c.state(0).policies[&id].covers["dental"];
    assert_eq!((dental.start, dental.layers.len(), dental.layers[1].start), (start(), 2, today));

    // On a policy of one person the part-year premium is what the cover adds to the price.
    c.take_events();
    add_cover(&mut c, "policy-servicing", &solo, "specialist", "life-1").unwrap();
    let second = adjusted(&mut c);
    let expected = part(with_specialist - hospital);
    assert!((second - expected).abs() <= 4, "specialist: billed {second}, expected {expected}");
    // The third cover brings the bundle discount in on all three.
    add_cover(&mut c, "policy-servicing", &solo, "dental", "life-1").unwrap();
    let third = adjusted(&mut c);
    let expected = part(all_three - with_specialist);
    assert!((third - expected).abs() <= 6, "dental: billed {third}, expected {expected}");
    assert!(c.reconcile().is_empty());

    // Given up and taken again: a new layer from today, the old one kept as it ended.
    give_up(&mut c, &solo, "dental", "life-1").unwrap();
    assert!(!holds(&c, &solo, "dental", "life-1"));
    add_cover(&mut c, "policy-servicing", &solo, "dental", "life-1").unwrap();
    assert!(holds(&c, &solo, "dental", "life-1"));
    assert_eq!(c.state(0).policies[&solo].covers["dental"].layers.len(), 2);
    assert!(c.end_of_day().unwrap().breaks.is_empty());
}

fn change_excess(c: &mut Cluster, caller: &str, policy: &PolicyId, cover: &str, option: &str) -> Result<(), Reject> {
    let command = Command::ChangeExcess(Box::new(ic_core::command::ChangedExcess {
        policy: policy.clone(),
        cover: cover.to_string(),
        life: LifeId("life-1".to_string()),
        excess_option: option.to_string(),
    }));
    c.submit(0, caller, command)
}

/// Purpose: a cover that offers excess levels must be issued at one of them and a cover that
/// offers none must not be given one; the level prices the cover by its factor; and a change
/// of level mid-year ends the layer and starts a new one from that day at the new level,
/// billing or crediting the days left of the year, while the old layer keeps the level it had.
/// A level the cover does not offer, the level already held, and a change sent by a module
/// that may not are refused and change nothing.
/// Why it matters: the excess is the customer's main lever on a health premium and decides
/// what they pay at every claim. If a change rewrote the old layer, a claim for treatment
/// before the change would be assessed at the wrong excess; if it were not priced from the
/// day it took effect, the customer would pay for a level they did not have.
#[test]
fn an_excess_level_prices_the_cover_and_a_change_starts_a_new_layer() {
    let mut c = cluster();
    let (_, standard) = issue(&mut c, 1, &[(adult(), &["hospital"])]).unwrap();
    let (_, nil) = issue(&mut c, 2, &[(adult(), &["hospital:nil"])]).unwrap();
    let (id, high) = issue(&mut c, 3, &[(adult(), &["hospital:high", "dental"])]).unwrap();
    let (_, dental) = issue(&mut c, 4, &[(adult(), &["dental"])]).unwrap();
    assert_eq!((nil, high - dental), (standard * 120 / 100, standard * 80 / 100));
    assert_eq!(code(issue(&mut c, 5, &[(adult(), &["hospital:platinum"])]).map(|_| ())), "excess_option");
    assert_eq!(code(issue(&mut c, 6, &[(adult(), &["dental:nil"])]).map(|_| ())), "excess_option");

    let receipt = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "r-1".to_string(),
        amount: ic_money::Money::new(20_000_00, ic_money::Currency::NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    for _ in 0..73 {
        assert!(c.end_of_day().unwrap().breaks.is_empty());
    }
    let today = c.state(0).business_date.unwrap();
    let year_end = d(2028, 11, 15);
    let part = |yearly: i64| yearly * (year_end - today).num_days() / (year_end - start()).num_days();

    let before = (c.state(0).ledger.head(), c.state(0).policies[&id].clone());
    assert_eq!(code(change_excess(&mut c, "claims", &id, "hospital", "nil")), "caller_not_allowed");
    assert_eq!(code(change_excess(&mut c, "policy-servicing", &id, "hospital", "platinum")), "excess_option");
    assert_eq!(code(change_excess(&mut c, "policy-servicing", &id, "dental", "nil")), "excess_option");
    assert_eq!(code(change_excess(&mut c, "policy-servicing", &id, "hospital", "high")), "no_change");
    assert_eq!((c.state(0).ledger.head(), c.state(0).policies[&id].clone()), before);

    // High to nil: the days left are billed at the difference between the two levels.
    c.take_events();
    change_excess(&mut c, "policy-servicing", &id, "hospital", "nil").unwrap();
    let more = adjusted(&mut c);
    let expected = part(nil - (high - dental));
    assert!((more - expected).abs() <= 4, "billed {more}, expected {expected}");
    let layers = &c.state(0).policies[&id].covers["hospital"].layers;
    assert_eq!(layers.len(), 2);
    assert_eq!((layers[0].excess_option.as_deref(), layers[0].end), (Some("high"), Some(today)));
    assert_eq!((layers[1].excess_option.as_deref(), layers[1].start, layers[1].end), (Some("nil"), today, None));
    // And back down: a credit for the days left.
    change_excess(&mut c, "policy-servicing", &id, "hospital", "standard").unwrap();
    let less = adjusted(&mut c);
    let expected = part(standard - nil);
    assert!(less < 0 && (less - expected).abs() <= 4, "credited {less}, expected {expected}");
    assert!(c.reconcile().is_empty());
    assert!(c.end_of_day().unwrap().breaks.is_empty());
}
