//! The AIA New Zealand products (products/aia_*.star) and the product engine
//! features they brought: option menus on a cover, entry ages, built-in
//! benefits, the multi-benefit discount, limited pay and level term premiums,
//! and cover rules that need one of a set (docs/aia-products.md).

use std::collections::{BTreeMap, BTreeSet};

use ic_money::{dec, Dec};
use ic_product::discount::{Held, Line, PricingState};
use ic_product::options::{pays_premium, rating_age};
use ic_product::rating::{cover_premium, modal_premium_by_line, RatingError, RatingFactors};
use ic_product::{
    load_definitions, load_product, load_rate_table, option_code, BuiltInKind, CoverDefinition, Frequency, OptionError, PremiumBasis, ProductDefinition, Sex,
};

const PRODUCTS: [(&str, &str); 10] = [
    ("aia_living.star", include_str!("../../../products/aia_living.star")),
    ("aia_living_business.star", include_str!("../../../products/aia_living_business.star")),
    ("aia_start_up_income.star", include_str!("../../../products/aia_start_up_income.star")),
    ("aia_cover_for_life.star", include_str!("../../../products/aia_cover_for_life.star")),
    ("aia_essentials_life.star", include_str!("../../../products/aia_essentials_life.star")),
    ("aia_starter.star", include_str!("../../../products/aia_starter.star")),
    ("aia_interim.star", include_str!("../../../products/aia_interim.star")),
    ("aia_private_health.star", include_str!("../../../products/aia_private_health.star")),
    ("aia_cancer_care.star", include_str!("../../../products/aia_cancer_care.star")),
    ("aia_corporate.star", include_str!("../../../products/aia_corporate.star")),
];
const TABLES: [(&str, &str); 19] = [
    ("aia_accidental_death.star", include_str!("../../../products/rates/aia_accidental_death.star")),
    ("aia_accidental_injury.star", include_str!("../../../products/rates/aia_accidental_injury.star")),
    (
        "aia_business_continuity.star",
        include_str!("../../../products/rates/aia_business_continuity.star"),
    ),
    ("aia_cancer_care.star", include_str!("../../../products/rates/aia_cancer_care.star")),
    ("aia_family_protection.star", include_str!("../../../products/rates/aia_family_protection.star")),
    (
        "aia_group_critical_illness.star",
        include_str!("../../../products/rates/aia_group_critical_illness.star"),
    ),
    ("aia_group_income.star", include_str!("../../../products/rates/aia_group_income.star")),
    ("aia_group_life.star", include_str!("../../../products/rates/aia_group_life.star")),
    (
        "aia_group_severe_trauma.star",
        include_str!("../../../products/rates/aia_group_severe_trauma.star"),
    ),
    ("aia_group_tpd.star", include_str!("../../../products/rates/aia_group_tpd.star")),
    ("aia_income.star", include_str!("../../../products/rates/aia_income.star")),
    ("aia_life.star", include_str!("../../../products/rates/aia_life.star")),
    ("aia_private_health.star", include_str!("../../../products/rates/aia_private_health.star")),
    (
        "aia_private_health_plus.star",
        include_str!("../../../products/rates/aia_private_health_plus.star"),
    ),
    ("aia_progressive_care.star", include_str!("../../../products/rates/aia_progressive_care.star")),
    ("aia_severe_trauma.star", include_str!("../../../products/rates/aia_severe_trauma.star")),
    (
        "aia_specialist_testing.star",
        include_str!("../../../products/rates/aia_specialist_testing.star"),
    ),
    ("aia_tpd.star", include_str!("../../../products/rates/aia_tpd.star")),
    ("aia_trauma.star", include_str!("../../../products/rates/aia_trauma.star")),
];
const DEFINITIONS: &str = include_str!("../../../products/definitions/aia.star");
const WORDINGS: &str = include_str!("../../../products/definitions/aia_wordings.star");

fn living() -> ProductDefinition {
    load_product(PRODUCTS[0].0, PRODUCTS[0].1).unwrap()
}

fn cover<'a>(p: &'a ProductDefinition, code: &str) -> &'a CoverDefinition {
    p.cover(code).unwrap_or_else(|| panic!("{} has no cover {code}", p.code))
}

/// The code for a selection: the named choices, and the first choice on every other axis.
fn pick(c: &CoverDefinition, chosen: &[(&str, &str)]) -> String {
    let parts: Vec<(&str, &str)> = c
        .option_axes
        .iter()
        .map(|a| {
            let named = chosen.iter().find(|(axis, _)| *axis == a.code).map(|(_, ch)| *ch);
            (a.code.as_str(), named.unwrap_or(a.choices[0].code.as_str()))
        })
        .collect();
    option_code(&parts)
}

/// Purpose: every AIA product, placeholder rate table and the AIA definitions load; every
/// definition a cover or built-in benefit names is in the AIA library, every table a cover
/// is rated on is shipped, and the first choice of every menu resolves.
/// Why it matters: a product that names a definition nobody activates cannot bind its
/// covers, and one rated on a missing table cannot be quoted; this catches both at build
/// time rather than at the first quote, for all ten products at once.
#[test]
fn every_aia_product_table_and_definition_loads_and_fits_together() {
    let mut defs: BTreeSet<String> = load_definitions("aia.star", DEFINITIONS).unwrap().into_iter().map(|d| d.id).collect();
    defs.extend(load_definitions("aia_wordings.star", WORDINGS).unwrap().into_iter().map(|d| d.id));
    let mut tables: BTreeSet<String> = TABLES.iter().map(|(f, s)| load_rate_table(f, s).unwrap().id).collect();
    tables.insert("no-premium".to_string());
    assert_eq!(tables.len(), TABLES.len() + 1, "table ids are distinct");
    let mut codes = BTreeSet::new();
    for (file, source) in PRODUCTS {
        let p = load_product(file, source).unwrap();
        assert!(codes.insert(p.code.clone()) && p.code.starts_with("aia-"), "{file}");
        assert_eq!(p.effective_from.to_string(), "2026-11-14", "{file}");
        for c in &p.covers {
            assert!(tables.contains(&c.rating.table), "{file}: {} is rated on {}", c.code, c.rating.table);
            for d in c.definitions.iter().chain(c.built_in.iter().flat_map(|b| &b.definitions)) {
                assert!(defs.contains(d), "{file}: {} names definition {d}", c.code);
            }
            if !c.option_axes.is_empty() {
                let first = pick(c, &[]);
                assert!(c.option(&first).is_ok(), "{file}: {}: {first}", c.code);
            }
        }
    }
    assert_eq!(codes.len(), PRODUCTS.len());
}

/// Purpose: every AIA wording loads as a definition whose text is the wording in full, with
/// a title naming its document code and version; every cover of every product AIA publishes
/// a wording for is bound to at least one of them (Corporate Solutions schemes have their own
/// policy each, and AIA's online Essentials Life wording has no text layer to load, so
/// Essentials stays on its summaries until AIA supplies the text); and every wording is bound
/// by some cover.
/// Why it matters: a cover is bound to a definition's text by hash, so this is what fixes
/// which published words a claim is assessed against; a cover bound to none, or a wording no
/// cover uses, means the contract text and the product have drifted apart.
#[test]
fn every_aia_cover_is_bound_to_the_wordings_that_govern_it() {
    let wordings = load_definitions("aia_wordings.star", WORDINGS).unwrap();
    assert!(wordings.len() >= 34, "{}", wordings.len());
    for w in &wordings {
        assert!(w.id.starts_with("aia-wording-") && w.title.contains(": AIA"), "{}: {}", w.id, w.title);
        assert!(w.text.len() > 2_000, "{} holds {} characters", w.id, w.text.len());
    }
    let mut used = BTreeSet::new();
    for (file, source) in PRODUCTS {
        let p = load_product(file, source).unwrap();
        for c in &p.covers {
            let bound: Vec<&String> = c.definitions.iter().filter(|d| d.starts_with("aia-wording-")).collect();
            let unbound = matches!(p.code.as_str(), "aia-corporate" | "aia-essentials-life");
            assert_eq!(bound.is_empty(), unbound, "{file}: {}", c.code);
            used.extend(bound.into_iter().cloned());
        }
    }
    let all: BTreeSet<String> = wordings.into_iter().map(|w| w.id).collect();
    assert_eq!(used, all, "every wording is bound by a cover");
}

/// Purpose: an income cover's selection resolves to the product of each choice's premium
/// factor and the union of the terms and features each sets, and the premium is the base
/// premium times that factor; a choice offered only with another is refused without it,
/// and a code with an axis missing, out of order, or naming no such choice is unknown.
/// Why it matters: the waiting period, benefit period, premium type and optional benefits
/// are all one code on the layer; a wrong factor misprices every AIA income policy and a
/// wrong term pays claims on the wrong waiting or benefit period.
#[test]
fn an_income_selection_multiplies_factors_merges_terms_and_prices_the_cover() {
    let p = living();
    let ip = cover(&p, "income-agreed-value");
    let premier = pick(
        ip,
        &[("waiting", "4w"), ("period", "to-65"), ("cover", "premier"), ("mental-health", "limited")],
    );
    let o = ip.option(&premier).unwrap();
    assert_eq!(o.premium_factor, dec("1.11").unwrap() * dec("0.90").unwrap());
    assert_eq!(
        (o.terms.waiting_days, o.terms.benefit_to_age, o.terms.expiry_age, o.terms.benefit_months),
        (Some(28), Some(65), Some(65), None)
    );
    assert_eq!(o.terms.premium, Some(PremiumBasis::Stepped));
    assert!(o.has_feature("premier") && o.has_feature("mental-health-limit") && o.has_feature("future-insurability"));
    assert!(!o.has_feature("claim-indexation"));

    let built = |code: &str| ip.built_in.iter().find(|b| b.code == code).unwrap();
    assert!(built("extra-cash").applies(Some(&o)) && built("future-insurability").applies(Some(&o)));
    assert!(!built("redundancy").applies(Some(&o)) && !built("extra-cash").applies(None));
    assert!(built("return-home").applies(None), "a benefit behind no feature always applies");

    let two_years = pick(ip, &[("period", "2y")]);
    let o2 = ip.option(&two_years).unwrap();
    assert_eq!((o2.terms.benefit_months, o2.terms.benefit_to_age), (Some(24), None));
    let limited_on_2y = pick(ip, &[("period", "2y"), ("mental-health", "limited")]);
    assert!(matches!(ip.option(&limited_on_2y), Err(OptionError::Menu(m)) if m.contains("only with")));

    let mut parts: Vec<&str> = premier.split(',').collect();
    for bad in [
        parts[1..].join(","),
        {
            parts.swap(0, 1);
            parts.join(",")
        },
        premier.replace("waiting=4w", "waiting=3w"),
        premier.replace("waiting=4w", "waiting4w"),
    ] {
        assert!(matches!(ip.option(&bad), Err(OptionError::Unknown(code)) if code == bad), "{bad}");
    }

    let table = load_rate_table("aia_income.star", TABLES[10].1).unwrap();
    let at = |option: Option<String>| RatingFactors {
        age: 40,
        sex: Some(Sex::Female),
        smoker: Some(false),
        occupation_class: Some("1".into()),
        excess_option: option,
        ..RatingFactors::default()
    };
    let base = cover_premium(ip, &table, &at(Some(pick(ip, &[("waiting", "4w"), ("period", "to-65")]))), 400_000).unwrap();
    assert!(base > Dec::from(0i64));
    assert_eq!(cover_premium(ip, &table, &at(Some(premier)), 400_000).unwrap(), base * o.premium_factor);
    assert!(matches!(cover_premium(ip, &table, &at(None), 400_000), Err(RatingError::ExcessOption)));
    assert!(ip.accepts_excess(Some(&two_years)) && !ip.accepts_excess(None) && !ip.accepts_excess(Some("nil")));
}

/// Purpose: the premium type a life chooses sets the layer's premium basis, and a cover's
/// own rating is the fallback; level, level term and limited pay read the age the premium
/// is fixed at, and limited pay stops at its age while the others keep paying.
/// Why it matters: AIA's level premiums and Cover for Life's pay-to-age are what customers
/// buy for; re-rating a level premium at attained age, or billing a paid-up policy, is a
/// pricing error on every such policy.
#[test]
fn premium_types_set_the_age_a_premium_is_read_at_and_when_it_stops() {
    let p = living();
    let life = cover(&p, "life");
    let basis = |choice: &str| life.premium_basis(Some(&pick(life, &[("premium", choice)])));
    assert_eq!(basis("rate-for-age"), Some(PremiumBasis::Stepped));
    assert_eq!(basis("level-10y"), Some(PremiumBasis::LevelTerm { years: 10 }));
    assert_eq!(basis("level-to-80"), Some(PremiumBasis::Level { to_age: 80 }));
    assert_eq!(life.premium_basis(None), Some(PremiumBasis::Stepped), "no choice: the cover's own rating");
    assert_eq!(life.premium_basis(Some("premium=nope")), Some(PremiumBasis::Stepped));
    assert!(life.may_be_level());
    let cfl = load_product(PRODUCTS[3].0, PRODUCTS[3].1).unwrap();
    let whole = &cfl.covers[0];
    assert!(matches!(whole.premium_basis(Some(&pick(whole, &[]))), Some(PremiumBasis::LimitedPay { .. })));
    let corporate = load_product(PRODUCTS[9].0, PRODUCTS[9].1).unwrap();
    assert_eq!(cover(&corporate, "group-life").premium_basis(None), None, "a scheme's unit rates have no basis");
    assert!(!cover(&corporate, "group-life").may_be_level());
    let level = mini(&[("HEALTH_EXCESS", "")]).unwrap();
    let mut income = cover(&level, "income").clone();
    income.rating.premium_type = ic_product::PremiumType::Level;
    income.rating.level_to_age = Some(65);
    assert_eq!(
        income.premium_basis(None),
        Some(PremiumBasis::Level { to_age: 65 }),
        "a level cover with no menu"
    );
    assert!(income.may_be_level());

    assert_eq!(rating_age(PremiumBasis::Stepped, 30, 47), 47);
    assert_eq!(rating_age(PremiumBasis::Level { to_age: 65 }, 30, 47), 30);
    assert_eq!(rating_age(PremiumBasis::Level { to_age: 65 }, 30, 66), 66, "after the level age, attained age");
    assert_eq!(rating_age(PremiumBasis::LimitedPay { to_age: 65 }, 30, 64), 30);
    for (attained, block_start) in [(30, 30), (39, 30), (40, 40), (52, 50)] {
        assert_eq!(rating_age(PremiumBasis::LevelTerm { years: 10 }, 30, attained), block_start, "{attained}");
    }
    assert!(pays_premium(PremiumBasis::LimitedPay { to_age: 65 }, 64) && !pays_premium(PremiumBasis::LimitedPay { to_age: 65 }, 65));
    assert!(pays_premium(PremiumBasis::Level { to_age: 65 }, 90) && pays_premium(PremiumBasis::Stepped, 90));
}

/// Purpose: Family Protection's term choice ends the cover after a number of years or at an
/// age, and an income cover's benefit period sets its expiry age; a cover with no menu
/// falls back on its own expiry age.
/// Why it matters: a term that does not end the cover keeps billing for cover the customer
/// no longer has, and one that ends early leaves a family without the payments they bought.
#[test]
fn the_term_chosen_sets_when_the_cover_ends() {
    let p = living();
    let fp = cover(&p, "family-protection");
    assert!(fp.may_expire());
    let years = pick(fp, &[("term", "20y")]);
    assert_eq!((fp.expiry_after_years_for(Some(&years)), fp.expiry_age_for(Some(&years))), (Some(20), None));
    let to_age = pick(fp, &[("term", "to-60")]);
    assert_eq!((fp.expiry_after_years_for(Some(&to_age)), fp.expiry_age_for(Some(&to_age))), (None, Some(60)));
    assert_eq!((fp.expiry_after_years_for(None), fp.expiry_age_for(None)), (None, None));
    let ip = cover(&p, "income-indemnity");
    assert_eq!(ip.expiry_age_for(Some(&pick(ip, &[("period", "to-70")]))), Some(70));
    assert_eq!(
        ip.expiry_age_for(Some("not=a,code=at-all")),
        Some(65),
        "an unknown code falls back on the cover"
    );
    assert!(!cover(&p, "life").may_expire(), "AIA life cover runs for life");
}

/// Purpose: entry ages admit a life from the youngest to the oldest age the wording allows
/// and no other; a cover without limits admits every age.
/// Why it matters: AIA's entry ages differ by benefit (life from 10, TPD and income from
/// 16, trauma from birth); issuing outside them is cover the insurer never agreed to.
#[test]
fn entry_ages_admit_only_the_ages_the_wording_allows() {
    let p = living();
    let life = cover(&p, "life");
    assert!(!life.accepts_entry_age(9) && life.accepts_entry_age(10) && life.accepts_entry_age(70) && !life.accepts_entry_age(71));
    let tpd = cover(&p, "tpd-own-occupation");
    assert!(!tpd.accepts_entry_age(15) && tpd.accepts_entry_age(60) && !tpd.accepts_entry_age(61));
    let interim = load_product(PRODUCTS[6].0, PRODUCTS[6].1).unwrap();
    assert!(interim.covers.iter().all(|c| c.accepts_entry_age(0) && c.accepts_entry_age(120)));
}

/// Purpose: a built-in benefit's limit is the lowest of its money cap, its share of the
/// cover and its multiple of the cover, cut down to whole cents; a facility has none.
/// Why it matters: these limits are what claims pays on a terminal illness advance or a
/// partial disablement; a limit that rounded up, or took the higher cap, overpays.
#[test]
fn a_built_in_benefit_pays_the_lowest_of_its_limits() {
    let p = living();
    let life = cover(&p, "life");
    let stc = life.built_in.iter().find(|b| b.code == "specified-terminal-conditions").unwrap();
    assert_eq!(stc.kind, BuiltInKind::EarlyPayment);
    assert_eq!(stc.limit_minor(100_000_000), Some(25_000_000), "$1m: the $250,000 cap");
    assert_eq!(stc.limit_minor(50_000_000), Some(15_000_000), "$500,000: 30%");
    let ip = cover(&p, "income-agreed-value");
    let bridging = ip.built_in.iter().find(|b| b.code == "partial-disability-bridging").unwrap();
    assert_eq!(bridging.limit_minor(300_001), Some(99_990), "0.3333 of $3,000.01 is 99,990.33 cents, cut down");
    let facility = life.built_in.iter().find(|b| b.kind == BuiltInKind::Facility).unwrap();
    assert_eq!(facility.limit_minor(100_000_000), None);
}

/// Purpose: AIA Living's cover rules: a life holds a main benefit, an accelerated benefit
/// only with life cover, one of each pair of stand-alone and accelerated trauma, one income
/// basis, and Retirement Protection only with one of the income covers it rides on.
/// Why it matters: these are the combinations the wording allows; selling an accelerated
/// benefit with no life cover to reduce, or Retirement Protection with no income cover to
/// pay alongside, is cover that cannot be claimed as written.
#[test]
fn aia_living_cover_rules_decide_what_a_life_may_hold() {
    let p = living();
    let held = |codes: &[&'static str]| p.check_composition(&codes.iter().copied().collect());
    assert!(held(&["life", "critical-conditions-accelerated", "income-agreed-value", "retirement-protection"]).is_ok());
    assert!(held(&[]).is_ok());
    assert!(held(&["specialist-testing"]).unwrap_err().contains("at least one of"));
    assert!(held(&["critical-conditions-accelerated", "progressive-care"])
        .unwrap_err()
        .contains("held only with life"));
    assert!(held(&["life", "income-indemnity", "income-agreed-value"]).unwrap_err().contains("only one of"));
    let err = held(&["mortgage-income-rent", "retirement-protection"]).unwrap_err();
    assert!(err.contains("retirement-protection is held only with one of income-agreed-value"), "{err}");
}

fn line(life: &str, cover: &str) -> Line {
    Line {
        cover: cover.to_string(),
        life: life.to_string(),
    }
}

/// Purpose: the Multi-Benefit Discount is judged for each life: life cover (or Family
/// Protection) at its minimum plus one, two or three other categories at theirs gives 10%,
/// 12.5% or 15%, only the best applies, a life below a minimum gets none, a second life
/// without life cover gets none on its own covers, and the Vitality tier then takes its
/// share of what is left on the life that holds it.
/// Why it matters: this is the main discount on AIA Living and it is per life on minimum
/// amounts; judging it per policy, or ignoring the minimums, discounts customers AIA's
/// rules would not, and in the wrong order with Vitality overstates both.
#[test]
fn the_multi_benefit_discount_counts_categories_per_life_at_their_minimums() {
    let p = living();
    let run = |annual: &[(&str, &str, &str)], sums: &[(&str, &str, i64)], held: &[Held]| {
        let annual: BTreeMap<Line, Dec> = annual.iter().map(|(l, c, a)| (line(l, c), dec(a).unwrap())).collect();
        let sums: BTreeMap<Line, i64> = sums.iter().map(|(l, c, s)| (line(l, c), *s)).collect();
        let state = PricingState {
            covers_elsewhere: 0,
            overrides: &[],
            covers: annual.len() as u32,
            lives: 1,
            frequency: Frequency::Yearly,
            entitlements: held,
            sums_insured: &sums,
        };
        let bill = modal_premium_by_line(&p, &annual, &state).unwrap();
        bill.discounts.iter().map(|d| (d.code.clone(), d.amount.minor)).collect::<Vec<_>>()
    };
    let at = |code: &str, minor: i64| vec![(code.to_string(), minor)];
    let two = [("a", "life", "1000"), ("a", "critical-conditions", "500")];
    let two_sums = [("a", "life", 10_000_000), ("a", "critical-conditions", 7_500_000)];
    assert_eq!(run(&two, &two_sums, &[]), at("mbd-10", 150_00));

    let four = [
        ("a", "life", "1000"),
        ("a", "critical-conditions", "500"),
        ("a", "tpd-own-occupation", "300"),
        ("a", "income-indemnity", "200"),
    ];
    let four_sums = [
        ("a", "life", 10_000_000),
        ("a", "critical-conditions", 7_500_000),
        ("a", "tpd-own-occupation", 7_500_000),
        ("a", "income-indemnity", 200_000),
    ];
    assert_eq!(run(&four, &four_sums, &[]), at("mbd-15", 300_00));
    let three_sums = [four_sums[0], four_sums[1], four_sums[2], ("a", "income-indemnity", 199_999)];
    assert_eq!(run(&four, &three_sums, &[]), at("mbd-12-5", 250_00), "income below its minimum does not count");

    assert_eq!(run(&two, &[("a", "life", 9_999_999), two_sums[1]], &[]), vec![], "life cover below $100,000");
    let fp = [("a", "family-protection", "800"), ("a", "severe-trauma", "200")];
    assert_eq!(
        run(&fp, &[("a", "family-protection", 100_000), ("a", "severe-trauma", 7_500_000)], &[]),
        at("mbd-10", 100_00)
    );

    let couple = [two[0], two[1], ("b", "critical-conditions", "400")];
    let couple_sums = [two_sums[0], two_sums[1], ("b", "critical-conditions", 7_500_000)];
    assert_eq!(run(&couple, &couple_sums, &[]), at("mbd-10", 150_00), "the second life holds no life cover");

    let vitality = [Held {
        code: "aia-vitality".to_string(),
        tier: "20".to_string(),
        life: Some("a".to_string()),
    }];
    assert_eq!(
        run(&four, &four_sums, &vitality),
        vec![("mbd-15".to_string(), 300_00), ("vitality".to_string(), 340_00)]
    );
}

const MINI: &str = r#"
PRODUCT = product(
    code = "mini",
    name = "Mini",
    version = 1,
    effective_from = "2026-11-14",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    covers = [
        cover(code = "life", cover_type = "life", benefit = lump_sum(), rating = stepped("t"),
              option_axes = LIFE_AXES, built_in = LIFE_BUILT_IN, min_entry_age = MIN_AGE, max_entry_age = 70),
        cover(code = "income", cover_type = "income_protection", benefit = monthly_benefit(28, 24), rating = stepped("t"),
              option_axes = INCOME_AXES),
        cover(code = "health", cover_type = "health", rating = per_life("h"), HEALTH_EXCESS
              benefit = health_schedule([benefit_limit("surgical", per_year = dollars(1000))]), built_in = HEALTH_BUILT_IN),
        GROUP
    ],
    features = [billing(frequencies = ["monthly"]), earning(), arrears(grace_days = 30, lapse_days = 60), FEATURE],
)
"#;

fn mini(changes: &[(&str, &str)]) -> Result<ProductDefinition, String> {
    let defaults = [
        ("LIFE_AXES", "[]"),
        ("LIFE_BUILT_IN", "[]"),
        ("MIN_AGE", "10"),
        ("INCOME_AXES", "[]"),
        ("HEALTH_EXCESS", ""),
        ("HEALTH_BUILT_IN", "[]"),
        ("GROUP", ""),
        ("FEATURE", "cooling_off(days = 14)"),
    ];
    let mut source = MINI.to_string();
    for (key, default) in defaults {
        let value = changes.iter().find(|(k, _)| *k == key).map_or(default, |(_, v)| *v);
        source = source.replace(key, value);
    }
    load_product("mini.star", &source).map_err(|e| e.to_string())
}

const PREMIUM: &str = r#"option_axis("premium", "Premium", [choice("stepped", "Stepped", premium = stepped_premium()), choice("level", "Level", premium_factor = "1.5", premium = level_premium(65))])"#;

/// Purpose: the loader refuses each unsound option menu with the reason: excess levels and
/// axes together, axis or choice codes that are not plain or not distinct, an axis with no
/// choice, a premium factor of zero, a term of zero or a negative excess, a feature that is
/// not a plain code, an only-with naming the same axis or no such choice, income terms on a
/// cover that pays no income, and a premium structure on a scheme's unit rates.
/// Why it matters: a product is checked once, when it is loaded; a menu that loaded broken
/// would refuse or misprice each quote that chose from it, long after the release.
#[test]
fn the_loader_refuses_each_unsound_option_menu() {
    assert!(mini(&[("LIFE_AXES", &format!("[{PREMIUM}]"))]).is_ok());
    let axis = |choices: &str| format!(r#"[option_axis("x", "X", [{choices}])]"#);
    let ok = r#"choice("a", "A")"#;
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "HEALTH_EXCESS",
            r#"excess_options = [excess_option("nil", 0)], option_axes = [option_axis("x", "X", [choice("a", "A")])],"#.into(),
            "excess levels or option axes, not both",
        ),
        (
            "LIFE_AXES",
            format!(r#"[option_axis("X", "X", [{ok}])]"#),
            "axis codes are plain and distinct (X)",
        ),
        (
            "LIFE_AXES",
            format!(r#"[option_axis("x", "X", [{ok}]), option_axis("x", "Y", [{ok}])]"#),
            "plain and distinct (x)",
        ),
        ("LIFE_AXES", r#"[option_axis("x", "X", [])]"#.into(), "axis x offers no choice"),
        ("LIFE_AXES", axis(&format!("{ok}, {ok}")), "choice codes are plain and distinct (a)"),
        ("LIFE_AXES", axis(r#"choice("a b", "A")"#), "choice codes are plain and distinct (a b)"),
        ("LIFE_AXES", axis(r#"choice("a", "A", premium_factor = "0")"#), "premium factor above zero"),
        ("LIFE_AXES", axis(r#"choice("a", "A", expiry_age = 0)"#), "sets a term out of range"),
        ("LIFE_AXES", axis(r#"choice("a", "A", premium = level_premium(0))"#), "sets a term out of range"),
        (
            "LIFE_AXES",
            axis(r#"choice("a", "A", premium = level_term_premium(0))"#),
            "sets a term out of range",
        ),
        (
            "LIFE_AXES",
            axis(r#"choice("a", "A", premium = limited_pay_premium(0))"#),
            "sets a term out of range",
        ),
        ("LIFE_AXES", axis(r#"choice("a", "A", excess = -1)"#), "sets a term out of range"),
        ("LIFE_AXES", axis(r#"choice("a", "A", features = ["Not Plain"])"#), "sets a term out of range"),
        (
            "LIFE_AXES",
            axis(r#"choice("a", "A", only_with = ["x=a"])"#),
            "offered with x=a, which another axis does not offer",
        ),
        (
            "LIFE_AXES",
            format!(r#"[{PREMIUM}, option_axis("x", "X", [choice("a", "A", only_with = ["premium=monthly"])])]"#),
            "premium=monthly",
        ),
        (
            "LIFE_AXES",
            axis(r#"choice("a", "A", only_with = ["premium"])"#),
            "which another axis does not offer",
        ),
        (
            "LIFE_AXES",
            axis(r#"choice("a", "A", waiting_days = 28)"#),
            "income terms on a cover that pays no income",
        ),
        (
            "LIFE_AXES",
            axis(r#"choice("a", "A", benefit_to_age = 65)"#),
            "income terms on a cover that pays no income",
        ),
        (
            "GROUP",
            format!(
                r#"cover(code = "g", cover_type = "group_life", benefit = lump_sum(), rating = group_unit_rate("g"),
                   categories = [benefit_category("staff", salary_multiple = "1", max_sum_insured = dollars(1))],
                   option_axes = [{PREMIUM}]),"#
            ),
            "a scheme's unit rates have no premium structure",
        ),
        ("MIN_AGE", "71".into(), "the youngest entry age is above the oldest"),
    ];
    for (key, value, why) in &cases {
        let err = mini(&[(key, value)]).unwrap_err();
        assert!(err.contains(why), "{key} = {value}: {err}");
    }
    assert!(mini(&[("INCOME_AXES", &axis(r#"choice("a", "A", waiting_days = 28, benefit_months = 12)"#))]).is_ok());
}

/// Purpose: two choices of one selection that both set the same term are refused when the
/// selection is resolved, whichever term it is; a cover with no menu resolves no code; a flat
/// excess list resolves its levels as options carrying the excess.
/// Why it matters: a selection whose terms conflict has no meaning, so it must fail rather
/// than take one axis's term silently; and the excess list that health covers already use
/// must keep working unchanged through the same resolution.
#[test]
fn a_selection_whose_choices_set_the_same_term_is_refused() {
    for term in ["expiry_age = 60", "expiry_after_years = 5", "excess = 100", r#"premium = stepped_premium()"#] {
        let axes = format!(r#"[option_axis("x", "X", [choice("a", "A", {term})]), option_axis("y", "Y", [choice("b", "B", {term})])]"#);
        let p = mini(&[("LIFE_AXES", &axes)]).unwrap();
        assert!(
            matches!(cover(&p, "life").option("x=a,y=b"), Err(OptionError::Menu(m)) if m.contains("both set")),
            "{term}"
        );
    }
    for term in ["waiting_days = 7", "benefit_months = 12", "benefit_to_age = 65"] {
        let axes = format!(r#"[option_axis("x", "X", [choice("a", "A", {term})]), option_axis("y", "Y", [choice("b", "B", {term})])]"#);
        let p = mini(&[("INCOME_AXES", &axes)]).unwrap();
        assert!(
            matches!(cover(&p, "income").option("x=a,y=b"), Err(OptionError::Menu(m)) if m.contains("both set")),
            "{term}"
        );
    }
    let features = r#"[option_axis("x", "X", [choice("a", "A", features = ["f"])]), option_axis("y", "Y", [choice("b", "B", features = ["f", "g"])])]"#;
    let p = mini(&[("LIFE_AXES", features)]).unwrap();
    assert_eq!(
        cover(&p, "life").option("x=a,y=b").unwrap().terms.features,
        vec!["f".to_string(), "g".to_string()],
        "a feature twice is kept once"
    );
    assert!(matches!(cover(&p, "income").option("x=a"), Err(OptionError::Unknown(_))));
    assert!(!cover(&p, "income").has_options() && cover(&p, "income").accepts_excess(None));

    let health = load_product(PRODUCTS[7].0, PRODUCTS[7].1).unwrap();
    let ph = cover(&health, "private-health");
    let first = ph.excess_options[1].clone();
    let o = ph.option(&first.code).unwrap();
    assert_eq!((o.terms.excess_minor, o.premium_factor), (Some(first.excess_minor), first.premium_factor.get()));
    assert!(matches!(ph.option("no-such-excess"), Err(OptionError::Unknown(_))));
    assert_eq!(OptionError::Unknown("z".into()).to_string(), "the cover offers no such choice: z");
}

/// Purpose: the loader refuses each unsound built-in benefit: a duplicate code or no name, a
/// benefit that pays money with no limit, one that pays nothing but sets a limit, a limit of
/// zero, a monthly addition on a lump sum cover, an early payment out of a health cover, and
/// a benefit behind a feature no choice switches on.
/// Why it matters: claims pays these from the definition; a money benefit with no limit is
/// unbounded, and one behind a feature nobody can choose is a benefit no customer can have.
#[test]
fn the_loader_refuses_each_unsound_built_in_benefit() {
    let one = |b: &str| format!("[{b}]");
    assert!(mini(&[(
        "LIFE_BUILT_IN",
        &one(r#"built_in("ti", "Terminal illness", "early_payment", percent_of_cover = "100")"#)
    )])
    .is_ok());
    let cases: Vec<(&str, String, &str)> = vec![
        (
            "LIFE_BUILT_IN",
            r#"[built_in("a", "A", "facility"), built_in("a", "B", "facility")]"#.into(),
            "a code no other built-in",
        ),
        ("LIFE_BUILT_IN", one(r#"built_in("a", "", "facility")"#), "needs a name"),
        ("LIFE_BUILT_IN", one(r#"built_in("a", "A", "additional")"#), "sets the most it pays"),
        (
            "LIFE_BUILT_IN",
            one(r#"built_in("a", "A", "premium_waiver", max = 1)"#),
            "pays nothing, so it sets no limit",
        ),
        ("LIFE_BUILT_IN", one(r#"built_in("a", "A", "additional", max = 0)"#), "limits are above zero"),
        (
            "LIFE_BUILT_IN",
            one(r#"built_in("a", "A", "additional", percent_of_cover = "0")"#),
            "limits are above zero",
        ),
        (
            "LIFE_BUILT_IN",
            one(r#"built_in("a", "A", "monthly_addition", multiple_of_cover = "1")"#),
            "belongs to an income cover",
        ),
        (
            "HEALTH_BUILT_IN",
            one(r#"built_in("a", "A", "early_payment", max = 1)"#),
            "belongs to a lump sum or income cover",
        ),
        (
            "LIFE_BUILT_IN",
            one(r#"built_in("a", "A", "facility", requires = "f")"#),
            "names a feature none of the cover's options",
        ),
    ];
    for (key, value, why) in &cases {
        let err = mini(&[(key, value)]).unwrap_err();
        assert!(err.contains(why), "{value}: {err}");
    }
}

/// Purpose: the loader refuses a multi-benefit condition that names a cover the product does
/// not have, has no required category, repeats a category, has an empty one, or asks for more
/// categories than it lists, or names cover held elsewhere other than as `product/cover`; and a
/// one-of-a-set cover rule that names no cover or the cover
/// itself.
/// Why it matters: such a discount either never applies or applies to covers that do not
/// exist, and such a rule either blocks every sale or none; both must fail at load.
#[test]
fn the_loader_refuses_unsound_benefit_categories_and_one_of_a_set_rules() {
    let mbd = |required: &str, categories: &str, count: u32| {
        format!(
            r#"discounts(chain = [discount("mbd", "MBD", "multi_benefit_discount", percent_off("10"), benefit_categories({required}, {categories}, {count}))])"#
        )
    };
    let life = r#"[discount_category("life", ["life"], min_sum_insured = 100)]"#;
    let income = r#"[discount_category("income", ["income"])]"#;
    assert!(mini(&[("FEATURE", &mbd(life, income, 1))]).is_ok());
    let elsewhere = r#"[discount_category("income", ["income"], elsewhere = ["aia-start-up-income/start-up-income"])]"#;
    assert!(mini(&[("FEATURE", &mbd(life, elsewhere, 1))]).is_ok());
    let never = "can never be told apart from none";
    let cases = [
        (mbd(life, r#"[discount_category("x", ["nope"])]"#, 1), "names a cover the product does not have"),
        (mbd("[]", income, 1), never),
        (mbd(life, r#"[discount_category("life", ["income"])]"#, 1), never),
        (mbd(life, r#"[discount_category("income", [])]"#, 0), never),
        (mbd(life, r#"[discount_category("", ["income"])]"#, 0), never),
        (mbd(life, r#"[discount_category("income", ["income"], min_sum_insured = -1)]"#, 0), never),
        (mbd(life, income, 2), never),
        // Cover held elsewhere is named `product/cover`.
        (mbd(life, r#"[discount_category("income", ["income"], elsewhere = ["income"])]"#, 1), never),
        (mbd(life, r#"[discount_category("income", ["income"], elsewhere = ["/income"])]"#, 1), never),
        (mbd(life, r#"[discount_category("income", ["income"], elsewhere = ["a/b/c"])]"#, 1), never),
        (r#"composition(requires_any = {"income": []})"#.to_string(), "names at least one cover"),
        (r#"composition(requires_any = {"income": ["income"]})"#.to_string(), "cannot require itself"),
        (
            r#"composition(requires_any = {"income": ["nope"]})"#.to_string(),
            "which the product does not offer",
        ),
    ];
    for (feature, why) in &cases {
        let err = mini(&[("FEATURE", feature)]).unwrap_err();
        assert!(err.contains(why), "{feature}: {err}");
    }
}

/// Purpose: the loader takes AIA Living's limits on cover added without evidence as the wording
/// sets them (life special events totals and the 12-month gap, Life Future Insurability,
/// trauma and TPD special events and Future Insurability, the Special Events TPD/Trauma
/// Facility, and Business Safeguard), and refuses each unsound variant with the reason: totals
/// below one use, a gap of zero days, a window count without a window, an age range that is
/// empty, an empty feature, an anniversary option with no period or minimum or on a cover that
/// pays an income, an event option naming the reserved `anniversary` event, and a cover-adding
/// option that adds itself, a cover the product lacks, a choice the cover does not offer, or
/// adds nothing; and a buy-back of a cover the claimed cover does not reduce, with no feature,
/// no waits, a zero window or maximum, twice for one cover, or on an income cover.
/// Why it matters: these limits are what keep cover given without evidence of health inside
/// what AIA priced; a limit that loaded broken would let a life add cover without end, or never.
#[test]
fn the_loader_refuses_unsound_option_totals_and_anniversary_options() {
    let read = |file: &str| {
        let path = format!("{}/../../products/{file}", env!("CARGO_MANIFEST_DIR"));
        load_product(file, &std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let living = read("aia_living.star");
    let life = living.cover("life").unwrap();
    let events = life.insurability_option.as_ref().unwrap();
    assert_eq!(
        (
            events.max_total_increase_minor,
            events.once_in_days,
            events.max_total_percent_of_original.as_ref().map(|p| p.get())
        ),
        (Some(750_000_00), Some(365), Some(Dec::from(100i64)))
    );
    let anniversary = life.anniversary_option.as_ref().unwrap();
    assert_eq!(
        (
            anniversary.every_years,
            anniversary.at_least_minor,
            anniversary.max_total_increase_minor,
            anniversary.within_days,
            anniversary.to_age,
            anniversary.lesser
        ),
        (3, 100_000_00, 1_000_000_00, 60, 60, false)
    );
    assert_eq!(anniversary.requires_feature.as_deref(), Some("future-insurability"));
    let adds = life.adds_cover.as_ref().unwrap();
    assert_eq!((adds.max_increase_minor, adds.max_uses, adds.days_before), (100_000_00, 1, Some(60)));
    assert_eq!(adds.covers["tpd-any-occupation-accelerated"]["expiry"], "to-65");
    for code in ["critical-conditions", "progressive-care-accelerated", "severe-trauma", "tpd-own-occupation"] {
        let c = living.cover(code).unwrap();
        let fi = c.anniversary_option.as_ref().unwrap();
        assert_eq!(
            (fi.lesser, fi.at_least_minor, fi.max_total_increase_minor, fi.days_before),
            (true, 200_000_00, 500_000_00, Some(60)),
            "{code}"
        );
        let se = c.insurability_option.as_ref().unwrap();
        assert_eq!((se.within_days, se.days_before, se.to_age), (60, Some(60), 55), "{code}");
    }
    let business = read("aia_living_business.star");
    for (code, cap) in [
        ("life", 10_000_000_00),
        ("tpd-any-occupation", 5_000_000_00),
        ("critical-conditions-accelerated", 2_000_000_00),
        ("severe-trauma", 2_000_000_00),
    ] {
        let c = business.cover(code).unwrap();
        let safeguard = c.insurability_option.as_ref().unwrap();
        assert_eq!(
            (
                safeguard.max_cover_after_minor,
                safeguard.uses_in_window,
                safeguard.not_before_days,
                safeguard.requires_feature.as_deref()
            ),
            (Some(cap), Some(2), Some(183), Some("business-safeguard")),
            "{code}"
        );
        assert!(c.anniversary_option.is_none());
    }
    assert!(business.cover("progressive-care").unwrap().insurability_option.is_none());

    let event = |extra: &str| format!(r#"[], insurability_option = insurability_option(["marriage"], "50", 100, 2, 60, 55{extra})"#);
    let yearly = |extra: &str| format!(r#"[], anniversary_option = anniversary_option(3, "20", 100, "40", 200, 60, 60{extra})"#);
    let trauma = r#"cover(code = "trauma", cover_type = "trauma", benefit = lump_sum(), rating = stepped("t"),
              option_axes = [option_axis("expiry", "Expiry", [choice("to-65", "To 65", expiry_age = 65)])]),"#;
    let adding = |covers: &str, extra: &str| format!(r#"[], adds_cover = adds_cover_option(["marriage"], {covers}, "50", 100, 1, 60, 55{extra})"#);
    let good = [
        event(
            r#", max_total_percent_of_original = "100", max_total_increase = 200, once_in_days = 365, uses_in_window = 2, days_before = 60, from_age = 16, requires = "x", not_before_days = 183, max_cover_after = 1000"#,
        ),
        yearly(r#", lesser = True, days_before = 60, from_age = 16, once_in_days = 365, requires = "x""#),
        adding(r#"{"trauma": {"expiry": "to-65"}}"#, ", days_before = 60, from_age = 16, once_in_days = 365"),
    ];
    for axes in &good {
        assert!(mini(&[("LIFE_AXES", axes), ("GROUP", trauma)]).is_ok(), "{axes}");
    }
    let events_wrong = "an insurability option is on a lump sum or monthly benefit cover";
    let anniversary_wrong = "an anniversary option is on a lump sum or monthly benefit cover";
    let adding_wrong = "a cover-adding option is on a lump sum cover";
    let cases = [
        (event(r#", max_total_percent_of_original = "40""#), events_wrong),
        (event(", max_total_increase = 99"), events_wrong),
        (event(", once_in_days = 0"), events_wrong),
        (event(", uses_in_window = 2"), events_wrong),
        (event(", once_in_days = 365, uses_in_window = 0"), events_wrong),
        (event(", days_before = 0"), events_wrong),
        (event(", from_age = 55"), events_wrong),
        (event(r#", requires = """#), events_wrong),
        (event(", not_before_days = 0"), events_wrong),
        (event(", max_cover_after = 0"), events_wrong),
        (
            r#"[], insurability_option = insurability_option(["anniversary"], "50", 100, 2, 60, 55)"#.to_string(),
            events_wrong,
        ),
        (
            r#"[], anniversary_option = anniversary_option(0, "20", 100, "40", 200, 60, 60)"#.to_string(),
            anniversary_wrong,
        ),
        (
            r#"[], anniversary_option = anniversary_option(3, "20", 0, "40", 200, 60, 60)"#.to_string(),
            anniversary_wrong,
        ),
        (
            r#"[], anniversary_option = anniversary_option(3, "20", 100, "10", 200, 60, 60)"#.to_string(),
            anniversary_wrong,
        ),
        (
            r#"[], anniversary_option = anniversary_option(3, "20", 100, "40", 99, 60, 60)"#.to_string(),
            anniversary_wrong,
        ),
        (
            r#"[], anniversary_option = anniversary_option(3, "20", 100, "40", 200, 0, 60)"#.to_string(),
            anniversary_wrong,
        ),
        (yearly(r#", requires = """#), anniversary_wrong),
        (yearly(", once_in_days = 0"), anniversary_wrong),
        (yearly(", days_before = 0"), anniversary_wrong),
        (yearly(", from_age = 60"), anniversary_wrong),
        (adding(r#"{"life": {}}"#, ""), adding_wrong),
        (adding(r#"{"nope": {}}"#, ""), adding_wrong),
        (adding(r#"{"income": {}}"#, ""), adding_wrong),
        (adding(r#"{"trauma": {"expiry": "to-70"}}"#, ""), adding_wrong),
        (adding("{}", ""), adding_wrong),
        (
            r#"[], adds_cover = adds_cover_option(["marriage"], {"trauma": {}}, "50", 100, 0, 60, 55)"#.to_string(),
            adding_wrong,
        ),
        (adding(r#"{"trauma": {}}"#, ", from_age = 55"), adding_wrong),
    ];
    for (axes, why) in &cases {
        let err = mini(&[("LIFE_AXES", axes), ("GROUP", trauma)]).unwrap_err();
        assert!(err.contains(why), "{axes}: {err}");
    }
    // Buy-backs: this cover or one it reduces, once each, with a feature, waits and a window.
    let cc = living.cover("critical-conditions-accelerated").unwrap();
    let restores: Vec<(&str, bool, Option<i64>)> = cc.buybacks.iter().map(|b| (b.restores.as_str(), b.only_after_nil, b.max_minor)).collect();
    assert_eq!(
        restores,
        vec![
            ("critical-conditions-accelerated", true, Some(2_000_000_00)),
            ("life", false, Some(2_000_000_00))
        ]
    );
    assert!(living.cover("progressive-care").unwrap().buybacks.is_empty());
    let back = |b: &str| format!(r#"[], buybacks = [{b}]"#);
    assert!(mini(&[("LIFE_AXES", &back(r#"buyback("life", "x", [183, 365], 60, max = 100, only_after_nil = True)"#))]).is_ok());
    let buyback_wrong = "a buy-back is on a lump sum cover";
    for b in [
        r#"buyback("income", "x", [365], 60)"#,
        r#"buyback("life", "", [365], 60)"#,
        r#"buyback("life", "x", [], 60)"#,
        r#"buyback("life", "x", [0], 60)"#,
        r#"buyback("life", "x", [365], 0)"#,
        r#"buyback("life", "x", [365], 60, max = 0)"#,
        r#"buyback("life", "x", [365], 60), buyback("life", "y", [365], 60)"#,
    ] {
        let err = mini(&[("LIFE_AXES", &back(b)), ("GROUP", trauma)]).unwrap_err();
        assert!(err.contains(buyback_wrong), "{b}: {err}");
    }
    let err = mini(&[("INCOME_AXES", &back(r#"buyback("income", "x", [365], 60)"#)), ("GROUP", trauma)]).unwrap_err();
    assert!(err.contains(buyback_wrong), "{err}");
    // On a cover that pays an income the options grow the monthly benefit; a health cover has
    // no sum to grow.
    let income = format!(
        "{}, insurability_option = insurability_option([\"salary_increase\"], \"10\", 100, 99, 60, 56)",
        yearly(", max_cover_after = 2000, shared_total = True")
    );
    assert!(mini(&[("INCOME_AXES", &income), ("GROUP", trauma)]).is_ok());
    for bad in [yearly(", shared_total = True"), yearly(", max_cover_after = 0")] {
        let err = mini(&[("INCOME_AXES", &bad), ("GROUP", trauma)]).unwrap_err();
        assert!(err.contains(anniversary_wrong), "{bad}: {err}");
    }
    let health = r#"anniversary_option = anniversary_option(3, "20", 100, "40", 200, 60, 60),"#;
    let err = mini(&[("HEALTH_EXCESS", health), ("GROUP", trauma)]).unwrap_err();
    assert!(err.contains(anniversary_wrong), "{err}");
    // Conversions: to the cover's own axis choices on any cover; to another cover only
    // between lump sum covers.
    let conv = |axes: &str, c: &str| format!("[{axes}], conversions = [{c}]");
    let level = r#"conversion("income", 65, axis = "premium", choices = ["stepped", "level"], from_choices = ["stepped"], requires_evidence = True)"#;
    assert!(mini(&[("INCOME_AXES", &conv(PREMIUM, level)), ("GROUP", trauma)]).is_ok());
    let to_life = r#"conversion("trauma", 60, standard_terms_only = True)"#;
    assert!(mini(&[("LIFE_AXES", &conv(PREMIUM, to_life)), ("GROUP", trauma)]).is_ok());
    let conversion_wrong = "a conversion is to choices the cover's own axis offers";
    for (slot, c) in [
        ("INCOME_AXES", r#"conversion("life", 60)"#),
        ("INCOME_AXES", r#"conversion("income", 65, axis = "premium", choices = ["nope"])"#),
        ("INCOME_AXES", r#"conversion("income", 65, axis = "premium", choices = [])"#),
        ("INCOME_AXES", r#"conversion("income", 0, axis = "premium", choices = ["level"])"#),
        ("LIFE_AXES", r#"conversion("trauma", 60, with_cover = "life")"#),
        ("LIFE_AXES", r#"conversion("income", 60)"#),
        (
            "INCOME_AXES",
            r#"conversion("income", 65, axis = "premium", choices = ["level"], from_choices = ["stepped"])"#,
        ),
        ("LIFE_AXES", r#"conversion("trauma", 60, from_choices = ["level"])"#),
    ] {
        let err = mini(&[(slot, &conv(PREMIUM, c)), ("GROUP", trauma)]).unwrap_err();
        assert!(err.contains(conversion_wrong), "{c}: {err}");
    }
    let err = mini(&[("INCOME_AXES", &adding(r#"{"trauma": {}}"#, "")), ("GROUP", trauma)]).unwrap_err();
    assert!(err.contains(adding_wrong), "{err}");
}
