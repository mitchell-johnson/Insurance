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

/// Purpose: life cover the same person holds on their Cover for Life policy qualifies the
/// trauma on their AIA Living policy for the multi-benefit discount; it counts for no one else,
/// not for the same person in another group, and not once the Cover for Life policy has ended,
/// when the next bill carries no discount; Cover for Life itself is not discounted.
/// Why it matters: AIA's terms count a life's cover across their AIA policies towards the
/// minimums. Without it a customer who bought life cover first and trauma later would pay full
/// price on the trauma; counting someone else's cover would discount cover that has not earned it.
#[test]
fn cover_the_life_holds_on_another_policy_counts_towards_the_multi_benefit_discount() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let cfl = product("aia_cover_for_life.star");
    let dob = d(1987, 6, 1);
    // Policy `n` for the given person in the given group.
    let mine = |n: u64, product_code: &str, party: &str, group: &str, covers: &[(&str, i64, Option<String>)]| {
        let mut p = policy(n, product_code, dob, covers);
        p.group = GroupId(group.to_string());
        p.policyholder = PartyId(party.to_string());
        p.lives = vec![named_life("life-1", party, dob, Sex::Female, false)];
        p.frequency = Frequency::Monthly;
        p
    };
    let discounts = |c: &mut Cluster, p: IssuePolicy| {
        c.take_events();
        issue(c, p).unwrap();
        first_discounts(c)
    };
    let whole_life = Some(pick(&cfl, "life", &[]));
    let trauma = Some(pick(&living, "critical-conditions", &[]));
    let cover_for_life = mine(1, "aia-cover-for-life", "party-ann", "grp-ann", &[("life", 200_000_00, whole_life)]);
    let cfl_id = cover_for_life.policy.clone();
    assert!(discounts(&mut c, cover_for_life).is_empty(), "Cover for Life is not discounted");
    let ann = mine(2, "aia-living", "party-ann", "grp-ann", &[("critical-conditions", 80_000_00, trauma.clone())]);
    let ann_id = ann.policy.clone();
    assert_eq!(discounts(&mut c, ann), vec!["mbd-10".to_string()]);
    let bob = mine(3, "aia-living", "party-bob", "grp-ann", &[("critical-conditions", 80_000_00, trauma.clone())]);
    assert!(discounts(&mut c, bob).is_empty(), "another person's cover does not count");
    let elsewhere = mine(4, "aia-living", "party-ann", "grp-other", &[("critical-conditions", 80_000_00, trauma)]);
    assert!(discounts(&mut c, elsewhere).is_empty(), "nor the same person's in another group");
    let held = &c.state(0).policies[&ann_id].sums_elsewhere;
    assert_eq!(held["life-1"]["aia-cover-for-life/life"], 200_000_00);

    // The Cover for Life policy ends: from the next bill the trauma is not discounted.
    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: cfl_id }).unwrap();
    c.take_events();
    days(&mut c, 40);
    let later: Vec<Vec<String>> = c
        .take_events()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::PremiumDue { policy, discounts, .. } if policy == ann_id => Some(discounts.into_iter().map(|d| d.code).collect()),
            _ => None,
        })
        .collect();
    assert!(!later.is_empty() && later.iter().all(|d| d.is_empty()), "{later:?}");
}

/// The discount codes on the first bill among the events since they were last taken.
fn first_discounts(c: &mut Cluster) -> Vec<String> {
    c.take_events()
        .into_iter()
        .find_map(|e| match e.event {
            Event::PremiumDue { discounts, .. } => Some(discounts.into_iter().map(|d| d.code).collect::<Vec<_>>()),
            _ => None,
        })
        .unwrap()
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
/// interim product carries its $500,000 cap for one event while other products carry none,
/// and its income cover, which offers no options, its own 24-month benefit period.
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
        &[
            ("life", 400_000_00, None),
            ("accidental-tpd", 150_000_00, None),
            ("accidental-income", 2_000_00, None),
        ],
    );
    let id = issue(&mut c, interim).unwrap();
    for cover in ["life", "accidental-tpd"] {
        let facts = ic_core::cover_facts(c.state(0), &id.0, cover, Some("life-1"), on).unwrap();
        assert_eq!(facts.event_cap_minor, Some(500_000_00), "{cover}");
    }
    // With no option to say otherwise, the cover's own benefit period stands. An option
    // that ends the benefit at an age instead (to 65, above) is not cut to the cover's months.
    let facts = ic_core::cover_facts(c.state(0), &id.0, "accidental-income", Some("life-1"), on).unwrap();
    assert_eq!((facts.benefit_months, facts.benefit_to), (Some(24), None));
}

/// Purpose: Cover for Life on a four-year premium term cannot be made paid up after one year
/// or while premium is owing; after two years it can, and the life then holds 47.5% of the
/// sum assured (half the term, times 0.95, cut to the cent) on a layer that is never billed
/// again, with the premium already billed for the rest of the year handed back. The cover
/// check gives the reduced amount, the same request again changes nothing, and a cover with
/// no paid-up option refuses.
/// Why it matters: the paid-up option is a promise in the wording. Made too early or for the
/// wrong amount it gives away cover nobody paid for; left billing, it charges a customer who
/// was told there was nothing more to pay; and if the old amount stayed on the record a
/// claim would be paid at twice what the policy now provides.
#[test]
fn a_limited_pay_cover_is_made_paid_up_for_the_share_of_the_term_paid() {
    use ic_core::paid_up::MakePaidUp;
    let mut c = cluster();
    let cfl = product("aia_cover_for_life.star");
    let to_65 = pick(&cfl, "life", &[("pay-to", "to-65")]);
    // 61 at the start: premiums for four years.
    let id = issue(&mut c, policy(1, "aia-cover-for-life", d(1966, 6, 1), &[("life", 100_001_01, Some(to_65))])).unwrap();
    let ask = |policy: &PolicyId, cover: &str, reference: &str| {
        Command::MakePaidUp(Box::new(MakePaidUp {
            policy: policy.clone(),
            cover: cover.to_string(),
            life: ic_core::LifeId("life-1".to_string()),
            reference: reference.to_string(),
        }))
    };
    let send = |c: &mut Cluster, command: Command| c.submit(0, "policy-servicing", command);
    let refused = |r: Result<(), Reject>| r.unwrap_err().code;

    assert_eq!(refused(send(&mut c, ask(&id, "life", "pu-1"))), "premium_owing");
    // A cover with no paid-up option says so.
    let living = product("aia_living.star");
    let life = pick(&living, "life", &[]);
    let other = issue(&mut c, policy(2, "aia-living", d(1987, 6, 1), &[("life", 200_000_00, Some(life))])).unwrap();
    assert_eq!(refused(send(&mut c, ask(&other, "life", "pu-3"))), "no_paid_up_option");
    days(&mut c, 367);
    assert_eq!(refused(send(&mut c, ask(&id, "life", "pu-1"))), "paid_up_too_early");
    assert_eq!(refused(send(&mut c, ask(&id, "funeral", "pu-1"))), "unknown_cover");
    assert_eq!(refused(send(&mut c, ask(&id, "life", "pu 1"))), "invalid_request");
    days(&mut c, 366); // just over two years in: the third year's premium is billed and paid
    let year = billed(&c, &id, "life");
    assert_eq!(year.len(), 3);
    assert!(year[2] > 0);

    send(&mut c, ask(&id, "life", "pu-1")).unwrap();
    send(&mut c, ask(&id, "life", "pu-1")).unwrap();
    assert_eq!(refused(send(&mut c, ask(&id, "life", "pu-2"))), "life_not_covered");
    let p = &c.state(0).policies[&id];
    let layers = &p.covers["life"].layers;
    let live: Vec<_> = layers.iter().filter(|l| l.end.is_none()).collect();
    // 100,001.01 x 2/4 x 0.95 = 47,500.47975
    assert_eq!((layers.len(), live.len(), live[0].sum_insured_minor, live[0].paid_up), (2, 1, 47_500_47, true));
    assert_eq!(p.paid_up["pu-1"].paid_up_minor, 47_500_47);
    // All but the first days of the third year's premium came back as credit.
    let credit = p.receivables.iter().map(|r| r.total_minor).filter(|t| *t < 0).sum::<i64>();
    let third_year = p.receivables.iter().map(|r| r.total_minor).rfind(|t| *t > 0).unwrap();
    assert!(
        credit < 0 && -credit < third_year && -credit > third_year * 360 / 366,
        "{credit} of {third_year}"
    );
    let on = c.state(0).business_date.unwrap();
    let facts = ic_core::cover_facts(c.state(0), &id.0, "life", Some("life-1"), on).unwrap();
    assert_eq!((facts.in_force, facts.sum_insured_minor), (true, 47_500_47));

    // Two more years: in force, and never billed again.
    days(&mut c, 731);
    let p = &c.state(0).policies[&id];
    assert_eq!((p.status, p.covers["life"].status), (PolicyStatus::InForce, CoverStatus::InForce));
    assert!(
        p.receivables.iter().filter(|r| r.due > on).all(|r| r.total_minor == 0),
        "billed after it was paid up"
    );
    assert_eq!(p.outstanding_minor(), 0);
}

/// Close days to `to`, the customers paying what is owed each day under a receipt named for it.
fn close_to(c: &mut Cluster, to: NaiveDate) {
    while c.state(0).business_date.unwrap() < to {
        let today = c.state(0).business_date.unwrap();
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
                receipt: format!("on-{today}"),
                amount: Money::new(minor, NZD),
            };
            c.submit(0, "cash-matching", pay).unwrap();
        }
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "{:?}", close.breaks);
    }
}

/// Ask to use a cover's option on `policy` for life-1.
fn exercise(c: &mut Cluster, policy: &PolicyId, reference: &str, event: &str, on: NaiveDate, amount: i64) -> Result<(), Reject> {
    let ask = ic_core::command::OptionExercise {
        policy: policy.clone(),
        cover: "life".to_string(),
        life: ic_core::LifeId("life-1".to_string()),
        event: event.to_string(),
        event_date: on,
        sum_insured_minor: amount,
        evidence: "vault/evidence-1".to_string(),
        reference: reference.to_string(),
        from_cover: None,
        excess_option: None,
    };
    c.submit(0, "policy-servicing", Command::ExerciseOption(Box::new(ask)))
}

/// The sum insured of each layer of a policy's life cover, oldest first, leaving out the
/// yearly indexation increases.
fn life_layers(c: &Cluster, id: &PolicyId) -> Vec<i64> {
    let layers = &c.state(0).policies[id].covers["life"].layers;
    layers
        .iter()
        .filter(|l| l.origin != ic_core::LayerOrigin::Indexation)
        .map(|l| l.sum_insured_minor)
        .collect()
}

/// Purpose: AIA Living Life Cover's special events increases stop at the lower of the original
/// cover and $750,000 in total, and come at most once in 12 months; the Optional Life Future
/// Insurability Benefit adds the greater of 20% and $100,000 at each third anniversary, only
/// where the life chose it, only on the anniversary itself, and is itself one of the increases
/// that keep 12 months apart.
/// Why it matters: these are AIA's limits on cover given without evidence of health (1101
/// AL-LIFE, sections 6, 13 and 16). A third increase past the total, two in one year, or an
/// anniversary increase for a life that never paid for the option is risk nobody priced.
#[test]
fn special_events_and_future_insurability_increases_keep_to_aias_limits() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let plain = pick(&living, "life", &[]);
    let with_fi = pick(&living, "life", &[("future-insurability", "yes")]);
    let born = d(1987, 6, 1);
    let events = issue(&mut c, policy(1, "aia-living", born, &[("life", 400_000_00, Some(plain))])).unwrap();
    let fi = issue(&mut c, policy(2, "aia-living", born, &[("life", 300_000_00, Some(with_fi))])).unwrap();
    let code = |r: Result<(), Reject>| r.unwrap_err().code;

    // Special events: half the original each time, the original in all, once in 12 months.
    exercise(&mut c, &events, "se-1", "marriage", start(), 200_000_00).unwrap();
    close_to(&mut c, start() + chrono::Days::new(30));
    let birth = c.state(0).business_date.unwrap();
    assert_eq!(code(exercise(&mut c, &events, "se-2", "birth_or_adoption", birth, 1_00)), "option_too_soon");
    close_to(&mut c, start() + chrono::Days::new(370));
    let home = c.state(0).business_date.unwrap();
    exercise(&mut c, &events, "se-3", "mortgage", home, 200_000_00).unwrap();
    close_to(&mut c, start() + chrono::Days::new(736));
    let raise = c.state(0).business_date.unwrap();
    assert_eq!(code(exercise(&mut c, &events, "se-4", "salary_increase", raise, 1_00)), "option_total");
    assert_eq!(life_layers(&c, &events), vec![400_000_00, 200_000_00, 200_000_00]);

    // Future insurability: at the third anniversary, within 60 days.
    let third = d(2030, 11, 15);
    close_to(&mut c, third + chrono::Days::new(10));
    let today = c.state(0).business_date.unwrap();
    assert_eq!(code(exercise(&mut c, &fi, "fi-1", "anniversary", today, 100_000_00)), "not_an_anniversary");
    assert_eq!(code(exercise(&mut c, &events, "fi-1", "anniversary", third, 100_000_00)), "no_option");
    // The greater of 20% of $300,000 and $100,000.
    assert_eq!(code(exercise(&mut c, &fi, "fi-1", "anniversary", third, 100_000_01)), "option_limit");
    exercise(&mut c, &fi, "fi-1", "anniversary", third, 100_000_00).unwrap();
    exercise(&mut c, &fi, "fi-1", "anniversary", third, 100_000_00).unwrap();
    assert_eq!(code(exercise(&mut c, &fi, "se-1", "marriage", today, 10_000_00)), "option_too_soon");
    assert_eq!(life_layers(&c, &fi), vec![300_000_00, 100_000_00]);
    let used = c.state(0).policies[&fi].option_uses["fi-1"].clone();
    assert_eq!((used.event.as_str(), used.event_date), ("anniversary", third));
    // Billed from the anniversary's use: the policy owes for the new layer.
    close_to(&mut c, third + chrono::Days::new(11));
    assert!(billed(&c, &fi, "life").last().is_some_and(|b| *b > 0));
}

/// Purpose: an AIA Living policy a year old is suspended at the owner's request for dated
/// days: nothing is billed for them and premium billed ahead comes back, a claim's cover
/// check answers not in force for an event inside them and in force the day before and the
/// day cover returns, the owner can come back early and is billed from that day, and only
/// the days used count against the twelve months the product allows. A product without the
/// right, a policy with premium owing or under a year old, a request for more than the
/// allowance, and collections trying to end it as a hardship override are each refused.
/// Why it matters: suspension is a promise in the wording with a price on both sides. Bill
/// through it and an owner out of work is put into arrears the insurer said they would not
/// have; answer a claim for an event inside it as covered and the insurer pays for risk it
/// took no premium for; count the planned days after an early return and the owner loses
/// months of an allowance they never used.
#[test]
fn a_policy_is_suspended_without_premium_or_cover_and_comes_back_early() {
    use chrono::{Duration, Months};
    use ic_core::suspension::SuspendCover;
    let mut c = cluster();
    let living = product("aia_living.star");
    let id = issue(
        &mut c,
        policy(1, "aia-living", d(1987, 6, 1), &[("life", 400_000_00, Some(pick(&living, "life", &[])))]),
    )
    .unwrap();
    let cfl = product("aia_cover_for_life.star");
    let to_65 = pick(&cfl, "life", &[("pay-to", "to-65")]);
    let other = issue(&mut c, policy(2, "aia-cover-for-life", d(1966, 6, 1), &[("life", 100_000_00, Some(to_65))])).unwrap();
    let ask = |policy: &PolicyId, reference: &str, from: NaiveDate, to: NaiveDate| {
        Command::SuspendCover(Box::new(SuspendCover {
            policy: policy.clone(),
            reference: reference.to_string(),
            from,
            to,
        }))
    };
    let resume = |reference: &str| Command::ResumeCover {
        policy: PolicyId("pol-00000001".to_string()),
        reference: reference.to_string(),
    };
    let send = |c: &mut Cluster, command: Command| c.submit(0, "policy-servicing", command);
    let refused = |r: Result<(), Reject>| r.unwrap_err().code;
    let today = |c: &Cluster| c.state(0).business_date.unwrap();
    let covered = |c: &Cluster, on: NaiveDate| {
        let facts = ic_core::cover_facts(c.state(0), &id.0, "life", Some("life-1"), on).unwrap();
        (facts.in_force, facts.standing)
    };

    let soon = start() + Duration::days(30);
    assert_eq!(
        refused(send(&mut c, ask(&other, "sus-1", soon, soon + Duration::days(60)))),
        "no_suspension_option"
    );
    assert_eq!(refused(send(&mut c, ask(&id, "sus-1", soon, soon + Duration::days(60)))), "premium_owing");
    days(&mut c, 1);
    assert_eq!(refused(send(&mut c, ask(&id, "sus-1", soon, soon + Duration::days(60)))), "suspended_too_early");
    days(&mut c, 366); // the second year's premium is billed and paid
    let year = *c.state(0).policies[&id]
        .receivables
        .iter()
        .map(|r| r.total_minor)
        .collect::<Vec<_>>()
        .last()
        .unwrap();
    assert!(year > 0);

    let from = today(&c) + Duration::days(10);
    let to = from + Duration::days(180);
    assert_eq!(refused(send(&mut c, ask(&id, "sus 1", from, to))), "invalid_request");
    let yesterday = today(&c) - Duration::days(1);
    assert_eq!(refused(send(&mut c, ask(&id, "sus-1", yesterday, to))), "invalid_request");
    assert_eq!(
        refused(send(&mut c, ask(&id, "sus-1", from, from + Duration::days(367)))),
        "suspension_too_long"
    );
    c.take_events();
    send(&mut c, ask(&id, "sus-1", from, to)).unwrap();
    send(&mut c, ask(&id, "sus-1", from, to)).unwrap();
    let announced = c.take_events();
    assert_eq!(
        announced
            .iter()
            .filter(|e| matches!(&e.event, Event::CoverSuspended { reference, .. } if reference == "sus-1"))
            .count(),
        1
    );
    assert_eq!(refused(send(&mut c, ask(&id, "sus-1", from, to + Duration::days(1)))), "duplicate_request");
    assert_eq!(refused(send(&mut c, ask(&id, "sus-2", to, to + Duration::days(30)))), "hardship_exists");
    let end_as_hardship = Command::EndHardship {
        policy: id.clone(),
        hardship: "sus-1".to_string(),
    };
    assert_eq!(refused(c.submit(0, "collections", end_as_hardship)), "cover_suspended");
    assert_eq!(refused(send(&mut c, resume("sus-9"))), "unknown_suspension");

    // 180 of the year's 365 days came back as credit.
    let credit = c.state(0).policies[&id]
        .receivables
        .iter()
        .map(|r| r.total_minor)
        .filter(|t| *t < 0)
        .sum::<i64>();
    let expected = year * 180 / 365;
    assert!((-credit - expected).abs() <= 2, "{credit} against {expected} of {year}");
    assert_eq!(covered(&c, from - Duration::days(1)), (true, Some("paid".to_string())));
    assert_eq!(covered(&c, from), (false, Some("suspended".to_string())));
    assert!(!covered(&c, to - Duration::days(1)).0);
    assert!(covered(&c, to).0);

    // Seventy days on, sixty days into the suspension, the owner is back at work.
    days(&mut c, 70);
    let back = today(&c);
    assert_eq!(back, from + Duration::days(60));
    send(&mut c, resume("sus-1")).unwrap();
    send(&mut c, resume("sus-1")).unwrap();
    assert!(c
        .take_events()
        .iter()
        .any(|e| matches!(&e.event, Event::CoverResumed { on, planned_to, .. } if *on == back && *planned_to == to)));
    assert!(!covered(&c, back - Duration::days(1)).0);
    assert!(covered(&c, back).0);
    let p = &c.state(0).policies[&id];
    assert_eq!((p.suspensions["sus-1"].to, p.suspensions["sus-1"].planned_to), (back, Some(to)));
    // The 120 days not used are billed again.
    let charged = p.receivables.iter().map(|r| r.total_minor).rfind(|t| *t > 0).unwrap();
    assert!((charged - year * 120 / 365).abs() <= 2, "{charged} of {year}");

    // Sixty days are used; what is left of twelve months can be taken later, and no more.
    days(&mut c, 5);
    let again = today(&c) + Duration::days(1);
    let left = (again.checked_add_months(Months::new(12)).unwrap() - again).num_days() - 60;
    assert_eq!(
        refused(send(&mut c, ask(&id, "sus-2", again, again + Duration::days(left + 1)))),
        "suspension_too_long"
    );
    send(&mut c, ask(&id, "sus-2", again, again + Duration::days(left))).unwrap();
    // Through the rest of it and out the other side: in force, nothing owing, books clean.
    days(&mut c, u32::try_from(left).unwrap() + 40);
    let p = &c.state(0).policies[&id];
    assert_eq!((p.status, p.covers["life"].status), (PolicyStatus::InForce, CoverStatus::InForce));
    assert_eq!(covered(&c, today(&c)), (true, Some("paid".to_string())));
    let now = today(&c);
    assert_eq!(refused(send(&mut c, ask(&id, "sus-3", now, now + Duration::days(1)))), "suspension_too_long");
}

/// Purpose: after a retail policy is cancelled, the cover check for an event while it was
/// in force still answers in force with the sum insured of that day, and for an event on or
/// after the day it ended answers not covered.
/// Why it matters: a death the day before the owner's cancellation took effect was insured.
/// Reading cover from the policy's status today would decline every claim notified after a
/// policy ended, however clearly the event fell inside it.
#[test]
fn an_event_before_a_policy_was_cancelled_is_still_covered() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let id = issue(
        &mut c,
        policy(1, "aia-living", d(1987, 6, 1), &[("life", 400_000_00, Some(pick(&living, "life", &[])))]),
    )
    .unwrap();
    days(&mut c, 60);
    let before = c.state(0).business_date.unwrap();
    days(&mut c, 2);
    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
    let ended = c.state(0).business_date.unwrap();
    days(&mut c, 3);
    assert_eq!(c.state(0).policies[&id].status, PolicyStatus::Cancelled);
    let facts = |on: NaiveDate| {
        let f = ic_core::cover_facts(c.state(0), &id.0, "life", Some("life-1"), on).unwrap();
        (f.in_force, f.sum_insured_minor)
    };
    assert_eq!(facts(before), (true, 400_000_00));
    assert_eq!(facts(ended), (false, 0));
}

/// Purpose: the life index row for a layer carries the option the life holds the cover at,
/// as issued and as later changed, and a row written before the field existed still reads.
/// Why it matters: the policy screens show the waiting period, benefit period and premium
/// basis a customer chose from this row. Without it an adviser reads a cover's sum insured
/// with no way to tell a 4-week wait from a 13-week one; a row that failed to load after
/// the upgrade would take the whole policy off the screens.
#[test]
fn the_life_index_shows_the_option_a_cover_is_held_at() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let chosen = pick(&living, "life", &[]);
    let id = issue(&mut c, policy(1, "aia-living", d(1987, 6, 1), &[("life", 400_000_00, Some(chosen.clone()))])).unwrap();
    let mut view = ic_readmodel::Projections::new();
    for record in c.take_events() {
        view.apply(&record).unwrap();
    }
    let rows = view.layers_on_policy(&id);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].option.as_deref(), Some(chosen.as_str()));
    let mut old = serde_json::to_value(rows[0]).unwrap();
    old.as_object_mut().unwrap().remove("option").unwrap();
    let read: ic_readmodel::LifeIndexRow = serde_json::from_value(old).unwrap();
    assert_eq!(read.option, None);
}

/// Ask to use an option on any cover of `policy` for life-1, or to add `cover` under the
/// cover-adding option of `from` with the choices `option`.
#[allow(clippy::too_many_arguments)]
fn use_option(
    c: &mut Cluster,
    policy: &PolicyId,
    cover: &str,
    reference: &str,
    event: &str,
    on: NaiveDate,
    amount: i64,
    from: Option<(&str, String)>,
) -> Result<(), Reject> {
    let ask = ic_core::command::OptionExercise {
        policy: policy.clone(),
        cover: cover.to_string(),
        life: ic_core::LifeId("life-1".to_string()),
        event: event.to_string(),
        event_date: on,
        sum_insured_minor: amount,
        evidence: "vault/evidence-1".to_string(),
        reference: reference.to_string(),
        from_cover: from.as_ref().map(|(f, _)| f.to_string()),
        excess_option: from.map(|(_, o)| o),
    };
    c.submit(0, "policy-servicing", Command::ExerciseOption(Box::new(ask)))
}

/// The life's in-force sum insured on a cover of a policy, without indexation layers.
fn held(c: &Cluster, id: &PolicyId, cover: &str) -> i64 {
    let on = c.state(0).business_date.unwrap();
    c.state(0).policies[id].covers.get(cover).map_or(0, |cv| {
        cv.layers
            .iter()
            .filter(|l| l.in_force_on(on) && l.origin != ic_core::LayerOrigin::Indexation)
            .map(|l| l.sum_insured_minor)
            .sum()
    })
}

/// Purpose: AIA Living trauma cover takes a special events increase asked for up to 60 days
/// before the event and not more, at most half the original; a second trauma increase within
/// 12 months is refused while a life cover increase in the same weeks is not; at the third
/// policy anniversary the trauma Future Insurability Benefit adds the lower of 20% and
/// $200,000, asked for up to 60 days before it.
/// Why it matters: these are the trauma wordings' limits (1105 AL-CC sections 9 and 10). The
/// 12 months are counted across a life's trauma increases, not its life cover, and the trauma
/// anniversary limit is a lower-of where life cover's is a greater-of: getting either the
/// wrong way round gives cover without evidence that AIA never offered.
#[test]
fn trauma_increases_without_evidence_keep_to_the_trauma_wording() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let (life, cc) = (pick(&living, "life", &[]), pick(&living, "critical-conditions", &[]));
    let id = issue(&mut c, policy(1, "aia-living", d(1987, 6, 1), &[("life", 500_000_00, Some(life)), ("critical-conditions", 300_000_00, Some(cc))])).unwrap();
    let code = |r: Result<(), Reject>| r.unwrap_err().code;
    let wedding = start() + chrono::Days::new(30);
    let too_early = start() + chrono::Days::new(61);

    assert_eq!(code(use_option(&mut c, &id, "critical-conditions", "se-0", "marriage", too_early, 1_00, None)), "option_window");
    assert_eq!(code(use_option(&mut c, &id, "critical-conditions", "se-0", "marriage", wedding, 150_000_01, None)), "option_limit");
    use_option(&mut c, &id, "critical-conditions", "se-1", "marriage", wedding, 150_000_00, None).unwrap();
    assert_eq!(code(use_option(&mut c, &id, "critical-conditions", "se-2", "mortgage", start(), 1_00, None)), "option_too_soon");
    // Life cover keeps its own 12 months (and is asked for only after the event).
    assert_eq!(code(use_option(&mut c, &id, "life", "se-3", "marriage", wedding, 1_00, None)), "option_window");
    use_option(&mut c, &id, "life", "se-3", "birth_or_adoption", start(), 100_000_00, None).unwrap();
    assert_eq!(held(&c, &id, "critical-conditions"), 450_000_00);

    // Future Insurability: the lower of 20% of $300,000 and $200,000, asked 26 days ahead.
    let third = d(2030, 11, 15);
    close_to(&mut c, third - chrono::Days::new(26));
    let today = c.state(0).business_date.unwrap();
    assert_eq!(code(use_option(&mut c, &id, "critical-conditions", "fi-0", "anniversary", today, 1_00, None)), "not_an_anniversary");
    assert_eq!(code(use_option(&mut c, &id, "critical-conditions", "fi-0", "anniversary", third, 60_000_01, None)), "option_limit");
    use_option(&mut c, &id, "critical-conditions", "fi-1", "anniversary", third, 60_000_00, None).unwrap();
    assert_eq!(held(&c, &id, "critical-conditions"), 510_000_00);
}

/// Purpose: the Special Events TPD/Trauma Facility on AIA Living Life Cover adds an accelerated
/// trauma or TPD cover the life does not hold, once, without evidence, at most the lower of half
/// the original life cover and $100,000, and only with the choices the wording names (any
/// occupation TPD to age 65); a cover not named, a TPD to 70, too much, or a second use is
/// refused with nothing written.
/// Why it matters: this is cover for a new risk given with no evidence at all. Added twice, to
/// age 70, or above $100,000, it is risk the reinsurer never agreed to take automatically.
#[test]
fn a_special_event_adds_accelerated_trauma_or_tpd_once_within_its_limit() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let life = pick(&living, "life", &[]);
    let id = issue(&mut c, policy(1, "aia-living", d(1987, 6, 1), &[("life", 500_000_00, Some(life))])).unwrap();
    let code = |r: Result<(), Reject>| r.unwrap_err().code;
    let born = start() - chrono::Days::new(10);
    let cc = pick(&living, "critical-conditions-accelerated", &[]);
    let to_70 = pick(&living, "tpd-any-occupation-accelerated", &[("expiry", "to-70")]);
    let to_65 = pick(&living, "tpd-any-occupation-accelerated", &[("expiry", "to-65")]);
    let from = |o: &String| Some(("life", o.clone()));

    assert_eq!(code(use_option(&mut c, &id, "severe-trauma-accelerated", "add-0", "birth_or_adoption", born, 1_00, from(&cc))), "no_option");
    assert_eq!(code(use_option(&mut c, &id, "tpd-any-occupation-accelerated", "add-0", "birth_or_adoption", born, 1_00, from(&to_70))), "excess_option");
    assert_eq!(code(use_option(&mut c, &id, "critical-conditions-accelerated", "add-0", "birth_or_adoption", born, 100_000_01, from(&cc))), "option_limit");
    assert_eq!(code(use_option(&mut c, &id, "critical-conditions-accelerated", "add-0", "birth_or_adoption", born, 1_00, None)), "unknown_cover");
    use_option(&mut c, &id, "critical-conditions-accelerated", "add-1", "birth_or_adoption", born, 100_000_00, from(&cc)).unwrap();
    use_option(&mut c, &id, "critical-conditions-accelerated", "add-1", "birth_or_adoption", born, 100_000_00, from(&cc)).unwrap();
    assert_eq!(held(&c, &id, "critical-conditions-accelerated"), 100_000_00);
    assert_eq!(code(use_option(&mut c, &id, "tpd-any-occupation-accelerated", "add-2", "marriage", born, 50_000_00, from(&to_65))), "options_exhausted");
    let used = &c.state(0).policies[&id].option_uses["add-1"];
    assert_eq!(used.from_cover.as_deref(), Some("life"));
    // The new cover is billed for the rest of the year already billed.
    assert!(c.state(0).policies[&id].receivables.iter().any(|r| r.net_fine.get("trauma").is_some_and(|m| *m > 0)));
}

/// Purpose: AIA Living Business's Optional Business Safeguard Facility adds cover after a
/// business event only where the life chose it, not in the first six months, at most twice in
/// 12 months, and never to more than five times the original cover; AIA Living Business has no
/// personal special events increase.
/// Why it matters: Business Safeguard is priced as an option and its limits are the business
/// wording's (1151 ALB-LIFE section 8); a key person cover grown without evidence past five
/// times its start, or in its first months, is a risk no one underwrote.
#[test]
fn business_safeguard_grows_cover_with_the_business_inside_its_limits() {
    let mut c = cluster();
    let business = product("aia_living_business.star");
    let with = pick(&business, "life", &[("safeguard", "yes")]);
    let without = pick(&business, "life", &[]);
    let id = issue(&mut c, policy(1, "aia-living-business", d(1980, 6, 1), &[("life", 1_000_000_00, Some(with))])).unwrap();
    let plain = issue(&mut c, policy(2, "aia-living-business", d(1980, 6, 1), &[("life", 1_000_000_00, Some(without))])).unwrap();
    let code = |r: Result<(), Reject>| r.unwrap_err().code;

    assert_eq!(code(exercise(&mut c, &id, "m-0", "marriage", start(), 1_00)), "event_not_covered");
    assert_eq!(code(exercise(&mut c, &id, "bs-0", "business_interest_increase", start(), 1_00)), "option_too_soon");
    close_to(&mut c, start() + chrono::Days::new(183));
    let today = c.state(0).business_date.unwrap();
    assert_eq!(code(exercise(&mut c, &plain, "bs-0", "loan_guarantee_increase", today, 1_00)), "no_option");
    exercise(&mut c, &id, "bs-1", "business_interest_increase", today, 3_000_000_00).unwrap();
    assert_eq!(code(exercise(&mut c, &id, "bs-2", "loan_guarantee_increase", today, 1_000_000_01)), "option_total");
    exercise(&mut c, &id, "bs-2", "loan_guarantee_increase", today, 500_000_00).unwrap();
    assert_eq!(code(exercise(&mut c, &id, "bs-3", "value_to_business_increase", today, 1_00)), "option_too_soon");
    assert_eq!(life_layers(&c, &id), vec![1_000_000_00, 3_000_000_00, 500_000_00]);
}

/// Purpose: on AIA Living the claim cover check says premium is waived on disability only for
/// a life that chose the optional Waiver of Premium on that cover, and says so for the cover of
/// a life that chose it, so a premium waiver claim is admitted for one and refused for the other.
/// Why it matters: Waiver of Premium is an option the customer pays 6% more for. Reported as
/// absent, claims refuses the waiver the customer bought; reported as present for everyone, the
/// insurer waives premium for customers who never paid for the option.
#[test]
fn the_waiver_of_premium_option_is_on_the_cover_check_only_where_chosen() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let with = pick(&living, "life", &[("waiver", "yes")]);
    let without = pick(&living, "life", &[]);
    let a = issue(&mut c, policy(1, "aia-living", d(1987, 6, 1), &[("life", 500_000_00, Some(with))])).unwrap();
    let b = issue(&mut c, policy(2, "aia-living", d(1987, 6, 1), &[("life", 500_000_00, Some(without))])).unwrap();
    let on = c.state(0).business_date.unwrap();
    let waiver = |id: &PolicyId| ic_core::cover_facts(c.state(0), &id.0, "life", Some("life-1"), on).unwrap().premium_waiver;
    assert_eq!((waiver(&a), waiver(&b)), (Some(true), Some(false)));
}

/// Ask to convert `amount` of life-1's `cover` on `policy` to `to` at the choices `option`.
fn convert(c: &mut Cluster, policy: &PolicyId, cover: &str, to: &str, option: &str, amount: i64, reference: &str) -> Result<(), Reject> {
    let ask = ic_core::conversion::ConvertCover {
        policy: policy.clone(),
        cover: cover.to_string(),
        life: ic_core::LifeId("life-1".to_string()),
        sum_insured_minor: amount,
        to_cover: to.to_string(),
        excess_option: option.to_string(),
        reference: reference.to_string(),
    };
    c.submit(0, "policy-servicing", Command::ConvertCover(Box::new(ask)))
}

/// The life's in-force sum insured on a cover held at an option.
fn held_at(c: &Cluster, id: &PolicyId, cover: &str, option: &str) -> i64 {
    let on = c.state(0).business_date.unwrap();
    c.state(0).policies[id].covers[cover]
        .layers
        .iter()
        .filter(|l| l.in_force_on(on) && l.excess_option.as_deref() == Some(option))
        .map(|l| l.sum_insured_minor)
        .sum()
}

/// Close today, every customer paying what is owed.
fn next_day(c: &mut Cluster) {
    let today = c.state(0).business_date.unwrap();
    close_to(c, today + chrono::Days::new(1));
}

/// Everything billed on a policy for a cover type.
fn billed_for(c: &Cluster, id: &PolicyId, cover_type: &str) -> i128 {
    c.state(0).policies[id].receivables.iter().map(|r| r.net_fine.get(cover_type).copied().unwrap_or(0)).sum()
}

/// Purpose: AIA Living's Premium Conversion Facility moves part of a life's cover from rate for
/// age to a level premium, and later from level to age 80 to level to age 65, without evidence:
/// the amount held is unchanged, the converted part is billed at its new premium type from today,
/// and a conversion back to a stepped premium, to the same type, of more than is held at that
/// type, of another option too, or from age 65 is refused with nothing written.
/// Why it matters: the facility is a promise in the wording (1101 AL-LIFE) that customers rely
/// on as premiums rise with age. A conversion the wording does not give (back to rate for age,
/// after 65, or a waiver added on the way) is a change of risk no one underwrote.
#[test]
fn premium_conversion_moves_cover_to_a_level_premium_without_evidence() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let stepped = pick(&living, "life", &[]);
    let to_80 = pick(&living, "life", &[("premium", "level-to-80")]);
    let to_65 = pick(&living, "life", &[("premium", "level-to-65")]);
    let with_waiver = pick(&living, "life", &[("premium", "level-to-80"), ("waiver", "yes")]);
    let id = issue(&mut c, policy(1, "aia-living", d(1987, 6, 1), &[("life", 500_000_00, Some(stepped.clone()))])).unwrap();
    let old = issue(&mut c, policy(2, "aia-living", d(1960, 6, 1), &[("life", 100_000_00, Some(stepped.clone()))])).unwrap();
    let code = |r: Result<(), Reject>| r.unwrap_err().code;
    assert_eq!(code(convert(&mut c, &id, "life", "life", &to_80, 200_000_00, "pc-0")), "premium_owing");
    next_day(&mut c);
    let billed = billed_for(&c, &id, "life");

    assert_eq!(code(convert(&mut c, &id, "life", "life", &with_waiver, 200_000_00, "pc-0")), "excess_option");
    assert_eq!(code(convert(&mut c, &id, "life", "life", &to_80, 500_000_01, "pc-0")), "life_not_covered");
    assert_eq!(code(convert(&mut c, &old, "life", "life", &to_80, 50_000_00, "pc-0")), "conversion_age");
    convert(&mut c, &id, "life", "life", &to_80, 200_000_00, "pc-1").unwrap();
    convert(&mut c, &id, "life", "life", &to_80, 200_000_00, "pc-1").unwrap();
    assert_eq!(code(convert(&mut c, &id, "life", "life", &to_65, 100_000_00, "pc-1")), "duplicate_request");
    assert_eq!((held(&c, &id, "life"), held_at(&c, &id, "life", &to_80)), (500_000_00, 200_000_00));
    // The level premium costs more than the stepped one for the rest of the year billed.
    assert!(billed_for(&c, &id, "life") > billed);
    next_day(&mut c);

    assert_eq!(code(convert(&mut c, &id, "life", "life", &stepped, 100_000_00, "pc-2")), "excess_option");
    assert_eq!(code(convert(&mut c, &id, "life", "life", &to_80, 100_000_00, "pc-2")), "excess_option");
    assert_eq!(code(convert(&mut c, &id, "life", "life", &to_65, 200_000_01, "pc-2")), "life_not_covered");
    convert(&mut c, &id, "life", "life", &to_65, 100_000_00, "pc-2").unwrap();
    assert_eq!((held_at(&c, &id, "life", &to_80), held_at(&c, &id, "life", &to_65)), (100_000_00, 100_000_00));
    assert_eq!(held(&c, &id, "life"), 500_000_00);
}

/// Purpose: AIA Living's Standalone Conversion Facility moves stand-alone Critical Conditions to
/// accelerated Critical Conditions with as much Life Cover added, without evidence, before age
/// 60; a conversion of more than is held, to a cover the facility does not name, or from age 60
/// is refused with nothing written.
/// Why it matters: the customer keeps their trauma cover and gains life cover the wording
/// promises without evidence. Converted without the life cover, the accelerated claim would cut
/// life cover the customer never had; converted too late, it is cover AIA does not offer.
#[test]
fn standalone_trauma_converts_to_accelerated_trauma_with_life_cover() {
    let mut c = cluster();
    let living = product("aia_living.star");
    let life = pick(&living, "life", &[]);
    let cc = pick(&living, "critical-conditions", &[]);
    let cca = pick(&living, "critical-conditions-accelerated", &[]);
    let covers = |l: &String, t: &String| vec![("life", 500_000_00, Some(l.clone())), ("critical-conditions", 200_000_00, Some(t.clone()))];
    let id = issue(&mut c, policy(1, "aia-living", d(1987, 6, 1), &covers(&life, &cc))).unwrap();
    let old = issue(&mut c, policy(2, "aia-living", d(1966, 6, 1), &covers(&life, &cc))).unwrap();
    let code = |r: Result<(), Reject>| r.unwrap_err().code;
    next_day(&mut c);

    assert_eq!(code(convert(&mut c, &id, "critical-conditions", "severe-trauma-accelerated", &cca, 1_00, "sc-0")), "no_conversion");
    assert_eq!(code(convert(&mut c, &id, "critical-conditions", "critical-conditions-accelerated", &cca, 200_000_01, "sc-0")), "life_not_covered");
    assert_eq!(code(convert(&mut c, &old, "critical-conditions", "critical-conditions-accelerated", &cca, 1_00, "sc-0")), "conversion_age");
    assert_eq!(held(&c, &id, "critical-conditions"), 200_000_00);
    convert(&mut c, &id, "critical-conditions", "critical-conditions-accelerated", &cca, 200_000_00, "sc-1").unwrap();
    assert_eq!(
        (held(&c, &id, "critical-conditions"), held(&c, &id, "critical-conditions-accelerated"), held(&c, &id, "life")),
        (0, 200_000_00, 700_000_00)
    );
    let done = &c.state(0).policies[&id].conversions["sc-1"];
    assert_eq!((done.to_cover.as_str(), done.sum_insured_minor), ("critical-conditions-accelerated", 200_000_00));
}
