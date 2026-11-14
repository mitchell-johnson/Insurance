//! The AIA New Zealand products in the core: entry ages at issue, options
//! chosen per life that set the premium structure, the term and the waiting
//! period, limited pay, and the multi-benefit discount on the bill
//! (docs/aia-products.md).

use chrono::NaiveDate;
use ic_core::command::{CoverSpec, IssuePolicy};
use ic_core::event::Event;
use ic_core::{Command, CoverStatus, GroupId, PartyId, PolicyId, PolicyStatus, Reject};
use ic_money::{Currency::NZD, Money};
use ic_product::{load_product, option_code, Frequency, ProductDefinition, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, named_life};
use ic_sim::Cluster;

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

/// Close `n` business days, the customers paying what is owed each day, insisting every
/// reconciliation is clean.
fn days(c: &mut Cluster, n: u32) {
    for day in 0..n {
        let owing: Vec<(PolicyId, i64)> = c
            .state(0)
            .policies
            .iter()
            .map(|(id, p)| (id.clone(), p.outstanding_minor()))
            .filter(|(_, o)| *o > 0)
            .collect();
        for (id, minor) in owing {
            let pay = Command::ApplyReceipt {
                policy: id,
                receipt: format!("day-{day}"),
                amount: Money::new(minor, NZD),
            };
            c.submit(0, "cash-matching", pay).unwrap();
        }
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn product(file: &str) -> ProductDefinition {
    let source = std::fs::read_to_string(format!("{}/../../products/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    load_product(file, &source).unwrap()
}

/// The option code for a cover: the named choices, and the first choice on every other axis.
fn pick(p: &ProductDefinition, cover: &str, chosen: &[(&str, &str)]) -> String {
    let parts: Vec<(&str, &str)> = p
        .cover(cover)
        .unwrap()
        .option_axes
        .iter()
        .map(|a| {
            let named = chosen.iter().find(|(axis, _)| *axis == a.code).map(|(_, ch)| *ch);
            (a.code.as_str(), named.unwrap_or(a.choices[0].code.as_str()))
        })
        .collect();
    option_code(&parts)
}

/// One life on an AIA policy, holding the covers given as (cover, sum insured, option).
fn policy(n: u64, product_code: &str, dob: NaiveDate, covers: &[(&str, i64, Option<String>)]) -> IssuePolicy {
    let party = format!("party-{n:08}");
    IssuePolicy {
        continuation_of: None,
        policy: PolicyId(format!("pol-{n:08}")),
        number: format!("AIA{n:08}"),
        group: GroupId(format!("grp-{n:08}")),
        policyholder: PartyId(party.clone()),
        product_code: product_code.to_string(),
        inception: start(),
        frequency: Frequency::Yearly,
        lives: vec![named_life("life-1", &party, dob, Sex::Female, false)],
        covers: covers
            .iter()
            .map(|(code, si, option)| CoverSpec {
                excess_option: option.clone(),
                code: code.to_string(),
                life: Some(ic_core::LifeId("life-1".to_string())),
                sum_insured_minor: *si,
                loadings: vec![],
                exclusions: vec![],
            })
            .collect(),
        migrated: false,
    }
}

fn issue(c: &mut Cluster, p: IssuePolicy) -> Result<PolicyId, Reject> {
    let id = p.policy.clone();
    c.submit(0, "new-business", issue_command(p)).map(|()| id)
}

/// What each period was billed on a cover, oldest first.
fn billed(c: &Cluster, id: &PolicyId, cover: &str) -> Vec<i128> {
    let mut r: Vec<_> = c.state(0).policies[id].receivables.iter().collect();
    r.sort_by_key(|r| r.id);
    r.iter().map(|r| r.net_fine.get(cover).copied().unwrap_or(0)).collect()
}

/// Purpose: a cover with entry ages refuses a life a day short of the youngest age or past
/// the oldest at the cover's start, with the reason, and accepts a life inside them.
/// Why it matters: AIA's entry ages are part of the contract it filed; issuing life cover to
/// a nine-year-old or a 71-year-old is cover nobody priced, found only at claim time.
#[test]
fn a_life_outside_a_covers_entry_ages_is_refused_at_issue() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let option = Some(pick(&living, "life", &[]));
    let life = |n, dob| policy(n, "aia-living", dob, &[("life", 500_000_00, option.clone())]);
    let young = issue(&mut c, life(1, d(2017, 11, 16))).unwrap_err();
    assert_eq!(young.code, "entry_age");
    assert!(young.message.contains("aged 10 to 70; life life-1 is 9"), "{}", young.message);
    assert_eq!(issue(&mut c, life(2, d(1956, 11, 14))).unwrap_err().code, "entry_age", "71 at the start");
    issue(&mut c, life(3, d(2017, 11, 15))).unwrap();
    issue(&mut c, life(4, d(1957, 11, 15))).unwrap();
    let none = issue(&mut c, policy(5, "aia-living", d(1990, 1, 1), &[("life", 500_000_00, None)])).unwrap_err();
    assert_eq!(none.code, "excess_option", "a cover with a menu needs a choice from it");
}

/// What the life covers of a policy were billed each period, in cents, oldest first.
fn life_bills(c: &Cluster, id: &PolicyId) -> Vec<i64> {
    c.nodes[&0]
        .outbox
        .iter()
        .filter_map(|e| match &e.event {
            Event::PremiumDue { policy, by_cover_type, .. } if policy == id => Some(by_cover_type.get("life").copied().unwrap_or(0)),
            _ => None,
        })
        .collect()
}

/// Purpose: across five years, on four policies for lives of the same age, life cover on a
/// premium level for ten years keeps its rate while the same cover on rate for age jumps
/// into the next age band; Family Protection to age 31 ends at the anniversary the life
/// turns 31, and Family Protection for a fixed five-year term ends at the fifth anniversary,
/// with nothing billed for either afterwards.
/// Why it matters: these are choices the customer makes per cover, held as one option code;
/// a level premium re-rated, or a term that never ends, bills for a contract the customer
/// did not buy, and the error would show only years after issue.
#[test]
fn the_premium_type_and_term_chosen_shape_billing_over_the_years() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let dob = d(1997, 6, 1); // 30 at issue, 31 at the first anniversary, 35 at the fifth
    let one = |c: &mut Cluster, n, cover: &str, si, option: String| issue(c, policy(n, "aia-living", dob, &[(cover, si, Some(option))])).unwrap();
    let level = one(&mut c, 1, "life", 500_000_00, pick(&living, "life", &[("premium", "level-10y")]));
    let stepped = one(&mut c, 2, "life", 500_000_00, pick(&living, "life", &[("premium", "rate-for-age")]));
    let five = one(&mut c, 3, "family-protection", 2_000_00, pick(&living, "family-protection", &[("term", "5y")]));
    let to_31 = one(
        &mut c,
        4,
        "family-protection",
        2_000_00,
        pick(&living, "family-protection", &[("term", "to-31")]),
    );
    days(&mut c, 366 * 5 + 2);

    // Indexation adds a layer each year, priced at the age it starts: both rise a little
    // each year, and only rate for age moves the first layer into the band from 35.
    let (l, s) = (life_bills(&c, &level), life_bills(&c, &stepped));
    assert_eq!((l.len(), s.len()), (6, 6), "{l:?} {s:?}");
    assert!(l[0] > s[0], "the level premium costs more at the start: {l:?} {s:?}");
    assert!(l[5] * 100 < l[4] * 105, "level: {l:?}");
    assert!(s[5] * 100 > s[4] * 130, "rate for age at 35: {s:?}");

    let fp = |id: &PolicyId| c.state(0).policies[id].covers["family-protection"].clone();
    assert_eq!((fp(&to_31).status, fp(&to_31).layers[0].end), (CoverStatus::Expired, Some(d(2028, 11, 15))));
    assert_eq!(life_bills(&c, &to_31).len(), 1, "billed for its one year only");
    assert_eq!((fp(&five).status, fp(&five).layers[0].end), (CoverStatus::Expired, Some(d(2032, 11, 15))));
    let bills = life_bills(&c, &five);
    assert!(bills.len() == 5 && bills.iter().all(|v| *v > 0), "{bills:?}");
    assert_eq!(c.state(0).policies[&five].status, PolicyStatus::Expired, "its only cover has ended");
}

/// Purpose: Cover for Life with premiums paid to 65, on a life of 64, is billed for its one
/// year and then stays in force with nothing more billed for the cover.
/// Why it matters: a pay-to-age policy that kept billing past its age would take premiums
/// the customer was promised they would never pay, and one that ended would take away the
/// lifelong cover they paid up.
#[test]
fn a_limited_pay_cover_stops_billing_and_stays_in_force() {
    let mut c = cluster();
    let cfl = product("aia_cover_for_life.star");
    let to_65 = pick(&cfl, "life", &[("pay-to", "to-65")]);
    let id = issue(&mut c, policy(1, "aia-cover-for-life", d(1963, 6, 1), &[("life", 200_000_00, Some(to_65))])).unwrap();
    assert!(billed(&c, &id, "life")[0] > 0);
    days(&mut c, 400);
    let p = &c.state(0).policies[&id];
    assert_eq!((p.status, p.covers["life"].status), (PolicyStatus::InForce, CoverStatus::InForce));
    assert!(billed(&c, &id, "life")[1..].iter().all(|v| *v == 0), "{:?}", billed(&c, &id, "life"));
}

/// Purpose: a policy where the life holds life cover and trauma at AIA's minimums is billed
/// with the 10% multi-benefit discount as its own line, and the same life with trauma below
/// its minimum is billed none.
/// Why it matters: the core is what passes each life's cover amounts to the discount; if it
/// passed none, no AIA customer would ever get the discount, and if it passed the policy's
/// total the minimums would mean nothing.
#[test]
fn the_multi_benefit_discount_is_on_the_bill_from_each_lifes_cover_amounts() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let life = Some(pick(&living, "life", &[]));
    let cc = Some(pick(&living, "critical-conditions", &[]));
    let dob = d(1987, 6, 1);
    let discounts = |c: &mut Cluster, p: IssuePolicy| {
        c.take_events();
        issue(c, p).unwrap();
        c.take_events()
            .into_iter()
            .find_map(|e| match e.event {
                Event::PremiumDue { discounts, .. } => Some(discounts.into_iter().map(|d| d.code).collect::<Vec<_>>()),
                _ => None,
            })
            .unwrap()
    };
    let with = policy(
        1,
        "aia-living",
        dob,
        &[("life", 100_000_00, life.clone()), ("critical-conditions", 75_000_00, cc.clone())],
    );
    assert_eq!(discounts(&mut c, with), vec!["mbd-10".to_string()]);
    let below = policy(2, "aia-living", dob, &[("life", 100_000_00, life), ("critical-conditions", 74_999_00, cc)]);
    assert_eq!(discounts(&mut c, below), Vec::<String>::new());
}

/// Purpose: the waiting period a life chose for its income cover is the one the claim facts
/// carry, not the cover's fallback.
/// Why it matters: claims starts paying from the end of the waiting period; reading the
/// fallback four weeks instead of the thirteen chosen would pay nine weeks the customer
/// gave up for a lower premium.
#[test]
fn the_waiting_period_chosen_is_the_one_a_claim_is_assessed_on() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let thirteen = pick(&living, "income-agreed-value", &[("waiting", "13w"), ("period", "to-65")]);
    let mut p = policy(1, "aia-living", d(1987, 6, 1), &[("income-agreed-value", 5_000_00, Some(thirteen))]);
    p.lives[0].factors.occupation_class = Some("2".to_string());
    let id = issue(&mut c, p).unwrap();
    let facts = ic_core::cover_facts(c.state(0), &id.0, "income-agreed-value", Some("life-1"), d(2028, 1, 10)).unwrap();
    assert_eq!((facts.waiting_days, facts.chosen_excess_minor), (91, None));
}

/// Purpose: a cover added to a policy after issue is held to the same entry ages, read on
/// the day it is added: a life of 61 cannot be given own-occupation TPD (oldest entry 60),
/// while a life of 40 on the same product can.
/// Why it matters: adding cover later is the other door into a cover; if only issue checked
/// entry ages, any limit could be passed by issuing first and adding the cover the next day.
#[test]
fn a_cover_added_later_is_held_to_its_entry_ages() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let life = Some(pick(&living, "life", &[]));
    let tpd = pick(&living, "tpd-own-occupation", &[]);
    let add = |c: &mut Cluster, id: &PolicyId| {
        let command = Command::AddCover(Box::new(ic_core::command::AddedCover {
            excess_option: Some(tpd.clone()),
            policy: id.clone(),
            cover: "tpd-own-occupation".to_string(),
            life: ic_core::LifeId("life-1".to_string()),
            sum_insured_minor: 200_000_00,
            loadings: vec![],
            exclusions: vec![],
            reference: "req-1".to_string(),
        }));
        c.submit(0, "underwriting", command)
    };
    let mut older = policy(1, "aia-living", d(1966, 6, 1), &[("life", 500_000_00, life.clone())]);
    older.lives[0].factors.occupation_class = Some("1".to_string());
    let older = issue(&mut c, older).unwrap();
    assert_eq!(add(&mut c, &older).unwrap_err().code, "entry_age");
    let mut younger = policy(2, "aia-living", d(1987, 6, 1), &[("life", 500_000_00, life)]);
    younger.lives[0].factors.occupation_class = Some("1".to_string());
    let younger = issue(&mut c, younger).unwrap();
    add(&mut c, &younger).unwrap();
    assert!(c.state(0).policies[&younger].covers.contains_key("tpd-own-occupation"));
}

/// Purpose: the claim facts carry what the product and the option a life chose say about a
/// claim. An income cover to age 65 with the mental health limit and claim indexation gives
/// the 65th birthday as the last day's limit, 24 months for mental health and indexation on,
/// with the built-in benefits those choices switch on marked as applying and each limit
/// worked out from the monthly benefit; the same cover on a two-year period gives 24 months
/// and no age limit; a health cover carries its schedule with lifetime limits; and the
/// interim product carries its $500,000 cap for one event while other products carry none.
/// Why it matters: claims decides from these facts alone and never reads a product. A term
/// left out is a term not applied: benefit paid past 65, a mental health claim paid without
/// limit, a built-in benefit paid to someone who did not buy it, or one accident paid in
/// full on every interim cover at once.
#[test]
fn the_claim_facts_carry_the_terms_of_the_option_chosen_and_the_products_limits() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let on = d(2028, 1, 10);
    let income = |c: &mut Cluster, n: u64, chosen: &[(&str, &str)]| {
        let option = pick(&living, "income-agreed-value", chosen);
        let mut p = policy(n, "aia-living", d(1987, 6, 1), &[("income-agreed-value", 5_000_00, Some(option))]);
        p.lives[0].factors.occupation_class = Some("2".to_string());
        let id = issue(c, p).unwrap();
        ic_core::cover_facts(c.state(0), &id.0, "income-agreed-value", Some("life-1"), on).unwrap()
    };
    let built = |facts: &ic_core::claims_module::port::CoverFacts, code: &str| {
        let b = facts.built_in.iter().find(|b| b.code == code).unwrap_or_else(|| panic!("no built-in {code}"));
        (b.kind.clone(), b.limit_minor, b.applies)
    };

    let to_65 = income(&mut c, 1, &[("period", "to-65"), ("mental-health", "limited"), ("claim-indexation", "cpi")]);
    assert_eq!((to_65.benefit_months, to_65.benefit_to), (None, Some(d(2052, 6, 1))));
    assert_eq!(
        (to_65.mental_health_months, to_65.claim_indexation, to_65.event_cap_minor),
        (Some(24), true, None)
    );
    assert_eq!(built(&to_65, "claim-indexation"), ("facility".to_string(), None, true));
    assert!(built(&to_65, "mental-health-limit").2);
    assert!(!built(&to_65, "future-insurability").2);
    // Three months' benefit, and a fixed amount whatever the benefit.
    assert_eq!(built(&to_65, "back-to-work"), ("additional".to_string(), Some(15_000_00), true));
    assert_eq!(built(&to_65, "return-home"), ("reimbursement".to_string(), Some(10_000_00), true));
    assert_eq!(built(&to_65, "pregnancy-premium-waiver").0, "premium_waiver");

    let two_years = income(&mut c, 2, &[("period", "2y")]);
    assert_eq!((two_years.benefit_months, two_years.benefit_to), (Some(24), None));
    assert_eq!((two_years.mental_health_months, two_years.claim_indexation), (None, false));
    assert_eq!(
        (built(&two_years, "future-insurability").2, built(&two_years, "claim-indexation").2),
        (true, false)
    );

    // A health schedule travels whole, lifetime limits included.
    let cancer = policy(3, "aia-cancer-care", d(1987, 6, 1), &[("cancer-care", 0, Some("500".to_string()))]);
    let id = issue(&mut c, cancer).unwrap();
    let facts = ic_core::cover_facts(c.state(0), &id.0, "cancer-care", Some("life-1"), on).unwrap();
    let limit = |category: &str| facts.health_limits.iter().find(|l| l.category == category).unwrap();
    assert_eq!(facts.health_limits.len(), 14);
    assert_eq!(
        (
            limit("public-hospital-cancer-cash").per_lifetime_minor,
            limit("public-hospital-cancer-cash").per_year_minor
        ),
        (Some(5_000_00), None)
    );
    assert_eq!(
        (limit("cancer-treatment").per_year_minor, limit("cancer-treatment").excess_minor),
        (Some(500_000_00), Some(0))
    );
    assert_eq!(
        (facts.chosen_excess_minor, facts.benefit_months, facts.claim_indexation),
        (Some(500_00), None, false)
    );

    // The interim product's cap for one event is on every one of its covers' facts.
    let interim = policy(
        4,
        "aia-interim",
        d(1987, 6, 1),
        &[("life", 400_000_00, None), ("accidental-tpd", 150_000_00, None)],
    );
    let id = issue(&mut c, interim).unwrap();
    for cover in ["life", "accidental-tpd"] {
        let facts = ic_core::cover_facts(c.state(0), &id.0, cover, Some("life-1"), on).unwrap();
        assert_eq!(facts.event_cap_minor, Some(500_000_00), "{cover}");
    }
}
