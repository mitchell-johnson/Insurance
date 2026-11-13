//! Scenario tests shipped with the products: given factors produce an exact premium.

use std::collections::BTreeMap;

use ic_money::{dec, Dec, DecStr};
use ic_product::discount::{Held, Line, PricingState};
use ic_product::rating::{cover_premium, modal_premium, modal_premium_by_line, modal_premium_with, ModalPremium, RatingFactors};
use ic_product::{load_definitions, load_product, load_rate_table, Frequency, ProductDefinition, Sex};

const TERM: &str = include_str!("../../../products/term_life.star");
const HEALTH: &str = include_str!("../../../products/health_plan.star");
const GROUP: &str = include_str!("../../../products/group_life.star");
const INVEST: &str = include_str!("../../../products/invest_plan.star");
const INCOME: &str = include_str!("../../../products/income_protection.star");
const INCOME_RATES: &str = include_str!("../../../products/rates/income_base.star");
const INTERIM: &str = include_str!("../../../products/interim_accident.star");
const NO_PREMIUM: &str = include_str!("../../../products/rates/no_premium.star");
const FLEX: &str = include_str!("../../../products/health_flex.star");
const FLEX_RATES: [&str; 4] = [
    include_str!("../../../products/rates/flex_hospital.star"),
    include_str!("../../../products/rates/flex_specialist.star"),
    include_str!("../../../products/rates/flex_dental.star"),
    include_str!("../../../products/rates/flex_optical.star"),
];
const LIFE_RATES: &str = include_str!("../../../products/rates/life_base.star");
const TRAUMA_RATES: &str = include_str!("../../../products/rates/trauma_base.star");
const HEALTH_RATES: &str = include_str!("../../../products/rates/health_base.star");
const GROUP_RATES: &str = include_str!("../../../products/rates/group_life_standard.star");
const DEFS: &str = include_str!("../../../products/definitions/library.star");

/// Purpose: every product, rate table and definition library shipped in the repository loads, with
/// a content hash and its features in the engine's order.
/// Why it matters: these files are what a new deployment activates. One that failed to load, or
/// loaded with features out of order, would stop new business on the first day.
#[test]
fn all_shipped_configuration_loads() {
    for (f, s) in [
        ("term_life.star", TERM),
        ("health_plan.star", HEALTH),
        ("group_life.star", GROUP),
        ("invest_plan.star", INVEST),
        ("health_flex.star", FLEX),
        ("interim_accident.star", INTERIM),
        ("income_protection.star", INCOME),
    ] {
        let p = load_product(f, s).unwrap();
        assert_eq!(p.content_hash.len(), 64);
        // features are sorted into the engine's order
        assert!(p.features.windows(2).all(|w| w[0].order() < w[1].order()));
    }
    for (f, s) in [
        ("life", LIFE_RATES),
        ("trauma", TRAUMA_RATES),
        ("health", HEALTH_RATES),
        ("group", GROUP_RATES),
        ("none", NO_PREMIUM),
        ("income", INCOME_RATES),
    ] {
        load_rate_table(f, s).unwrap();
    }
    for source in FLEX_RATES {
        load_rate_table("flex", source).unwrap();
    }
    assert_eq!(load_definitions("library.star", DEFS).unwrap().len(), 9);
}

/// Purpose: a product's content hash changes when a rate in it changes and is the same for the same
/// content under another file name.
/// Why it matters: every journal entry carries the hash of the product version that produced it.
/// The hash must identify what the product said, not what the file was called.
#[test]
fn hash_follows_content() {
    let a = load_product("a", TERM).unwrap();
    let b = load_product("b", &TERM.replace("72.00", "73.00")).unwrap();
    assert_ne!(a.content_hash, b.content_hash);
    assert_eq!(a.content_hash, load_product("c", TERM).unwrap().content_hash);
}

/// Purpose: the shipped term life product prices a 37 year old non-smoker to the exact cent, and
/// pricing the same inputs again gives the same answer.
/// Why it matters: this is the worked example from the rate table, checked by hand. A quote and the
/// policy issued from it are priced by the same function and must not differ by a cent.
#[test]
fn term_life_exact_premium() {
    let p = load_product("term_life.star", TERM).unwrap();
    let life = load_rate_table("life", LIFE_RATES).unwrap();
    let trauma = load_rate_table("trauma", TRAUMA_RATES).unwrap();
    let f = RatingFactors {
        excess_option: None,
        age: 37,
        sex: Some(Sex::Female),
        smoker: Some(false),
        category: None,
        occupation_class: None,
        loadings: vec![],
    };
    // age band 33-37: 0.45 * 1.42^3 = 1.2885 per thousand; 500,000 cover
    let l = cover_premium(p.cover("life").unwrap(), &life, &f, 500_000_00).unwrap();
    assert_eq!(l.to_string(), "644.2500");
    let t = cover_premium(p.cover("trauma").unwrap(), &trauma, &f, 100_000_00).unwrap();
    let mut annual = BTreeMap::new();
    annual.insert("life".to_string(), l);
    annual.insert("trauma".to_string(), t);
    let m = modal_premium(&p, &annual, Frequency::Monthly).unwrap();
    // 644.25 * 0.95 * 1.04 / 12 = 53.0432...
    assert_eq!(m.by_cover["life"].minor, 53_04);
    assert_eq!(m.policy_fee.minor, 6_00);
    assert!(m.gst.is_empty());
    // the same call again is identical: a quote and a policy cannot disagree
    assert_eq!(m, modal_premium(&p, &annual, Frequency::Monthly).unwrap());
}

/// Purpose: a 50% underwriting loading multiplies the cover's base premium by exactly 1.5.
/// Why it matters: loadings are an underwriter's decision about a person's risk. They must scale
/// the premium of the cover they are on, exactly, and nothing else.
#[test]
fn loading_applies_to_base() {
    let p = load_product("term_life.star", TERM).unwrap();
    let life = load_rate_table("life", LIFE_RATES).unwrap();
    let mut f = RatingFactors {
        excess_option: None,
        age: 37,
        sex: Some(Sex::Female),
        smoker: Some(false),
        category: None,
        occupation_class: None,
        loadings: vec![],
    };
    let base = cover_premium(p.cover("life").unwrap(), &life, &f, 500_000_00).unwrap();
    f.loadings.push(DecStr::parse("0.50").unwrap());
    let loaded = cover_premium(p.cover("life").unwrap(), &life, &f, 500_000_00).unwrap();
    assert_eq!(loaded, base * ic_money::dec("1.5").unwrap());
}

/// Purpose: the health plan's premium has 15% GST added per cover, and the total is premium plus
/// tax.
/// Why it matters: health insurance carries GST in New Zealand and life insurance does not. The tax
/// is owed to Inland Revenue from the moment it is billed, so it must be worked out per cover from
/// the start.
#[test]
fn health_premium_carries_gst() {
    let p = load_product("health_plan.star", HEALTH).unwrap();
    let t = load_rate_table("health", HEALTH_RATES).unwrap();
    let f = RatingFactors { age: 35, ..Default::default() };
    let a = cover_premium(p.cover("hospital").unwrap(), &t, &f, 0).unwrap();
    let mut annual = BTreeMap::new();
    annual.insert("hospital".to_string(), a);
    let m = modal_premium(&p, &annual, Frequency::Monthly).unwrap();
    assert_eq!(m.by_cover["hospital"].minor, 80_00);
    assert_eq!(m.gst["hospital"].minor, 12_00);
    assert_eq!(m.total(), 92_00);
}

/// Purpose: a product missing its earning feature, with a lapse period shorter than its grace
/// period, with an unknown line of business, or with an unknown field is refused.
/// Why it matters: a product file is configuration that behaves like code. A mistake in one must
/// fail when it is loaded, not when the first policy reaches the day the mistake matters.
#[test]
fn bad_products_are_rejected() {
    assert!(load_product("x", &TERM.replace("earning(),", "")).is_err());
    assert!(load_product("x", &TERM.replace("lapse_days = 90", "lapse_days = 10")).is_err());
    assert!(load_product("x", &TERM.replace("line = \"life\"", "line = \"pets\"")).is_err());
    assert!(load_product("x", "PRODUCT = {'code': 'x', 'surprise': 1}").is_err());
}

/// Purpose: every validation rule in the product loader refuses the product that breaks
/// it, and says which rule was broken.
/// Why it matters: a product file is code review's last line of defence before a product
/// is activated. Each rule here stops a class of mistake (a duplicate cover, a negative
/// fee, a discount over 100%) that would otherwise surface as wrong premiums in production.
#[test]
fn each_product_rule_refuses_the_file_that_breaks_it() {
    let cases: [(&str, &str, &str); 13] = [
        (
            "reinstatement_evidence_free_days = 60",
            "reinstatement_evidence_free_days = 181",
            "reinstatement_evidence_free_days is longer than reinstatement_days",
        ),
        ("version = 1,", "version = 0,", "version of at least 1"),
        ("code = \"term-life\"", "code = \"\"", "needs a code"),
        ("code = \"trauma\"", "code = \"life\"", "duplicate cover code"),
        (
            "rating = stepped(\"life-base\")",
            "rating = {\"premium_type\": \"level\", \"table\": \"life-base\", \"basis\": \"per_thousand\"}",
            "level premium needs level_to_age",
        ),
        (
            "rating = stepped(\"life-base\")",
            "rating = group_unit_rate(\"life-base\")",
            "need benefit categories",
        ),
        ("expiry_age = 100,", "expiry_age = 100, gst_rate = \"1.5\",", "gst_rate must be a fraction"),
        (
            "min_sum_insured = dollars(50000),",
            "min_sum_insured = dollars(9000000),",
            "limits are inconsistent",
        ),
        ("benefit = lump_sum(),", "benefit = investment_account([]),", "needs a fund menu"),
        ("earning(),", "earning(), earning(),", "declared twice"),
        (
            "frequencies = [\"fortnightly\", \"monthly\", \"yearly\"]",
            "frequencies = []",
            "offers no frequency",
        ),
        ("policy_fee_annual = \"72.00\"", "policy_fee_annual = \"-1\"", "policy fee is negative"),
        ("percent = \"5\"", "percent = \"101\"", "discount percent out of range"),
    ];
    for (from, to, expected) in cases {
        assert!(TERM.contains(from), "the shipped product no longer contains {from:?}");
        let err = load_product("bad.star", &TERM.replace(from, to)).expect_err(expected).to_string();
        assert!(err.contains(expected), "{to}: got {err}");
        assert!(err.contains("bad.star"), "the error names the file: {err}");
    }
    // A product with no covers at all.
    let none = "PRODUCT = product(code = 'x', name = 'x', version = 1, effective_from = '2027-01-01', line = 'life', currency = 'NZD', statutory_fund = 'SF1', covers = [], features = [])";
    assert!(load_product("bad.star", none).unwrap_err().to_string().contains("offers no covers"));
}

/// Purpose: a product can say how long after a lapse paying the arrears is enough to
/// reinstate, as shipped term life does (sixty days of its 180-day window); a product that does not say leaves the whole window free of evidence; and
/// saying nothing leaves the product's file, and so its hash, as it was.
/// Why it matters: the setting decides whether a policy lapsed for months comes back on risk
/// with no question about the life's health. A default that silently started demanding
/// underwriting, or changed the hash of every product in force, would break reinstatement
/// for policies already written.
#[test]
fn a_product_can_limit_reinstatement_on_payment_alone() {
    let shipped = load_product("term_life.star", TERM).unwrap();
    let rules = shipped.arrears().unwrap();
    assert_eq!((rules.reinstatement_days, rules.reinstatement_evidence_free_days), (180, Some(60)));

    let open = TERM.replace(", reinstatement_evidence_free_days = 60", "");
    let unlimited = load_product("term_life.star", &open).unwrap();
    assert_eq!(unlimited.arrears().unwrap().reinstatement_evidence_free_days, None);
    assert!(!serde_json::to_string(&unlimited).unwrap().contains("reinstatement_evidence_free_days"));
}

/// Purpose: a file that does not define the expected symbol, or is not valid Starlark,
/// fails with an error that says so.
/// Why it matters: the commonest authoring mistakes are a typo in the symbol name and a
/// syntax error. The author needs to be told which, not shown a schema error.
#[test]
fn a_missing_symbol_or_a_syntax_error_is_reported_as_such() {
    let missing = load_product("empty.star", "X = 1").unwrap_err().to_string();
    assert!(missing.contains("PRODUCT") && missing.contains("empty.star"), "{missing}");
    assert!(load_product("broken.star", "PRODUCT = (").is_err());
    assert!(load_rate_table("empty.star", "X = 1").is_err());
    assert!(load_definitions("empty.star", "X = 1").is_err());
}

/// Purpose: rate tables and definitions are validated: a table needs an id, a version and
/// rows; an age band cannot be inverted; a rate cannot be negative; a definition needs
/// wording and a unique id and version.
/// Why it matters: rate data and policy wordings are frozen onto covers for decades. A
/// table that cannot rate anyone, or two wordings claiming the same version, must be
/// stopped before activation because they cannot be corrected on policies already bound.
#[test]
fn bad_rate_tables_and_definitions_are_refused() {
    let table = |body: &str| format!("RATE_TABLE = rate_table(id = 't', version = 1, effective_from = '2027-01-01', rows = [{body}])");
    assert!(load_rate_table("t", &table("rate('1.0', age_from = 18, age_to = 30)")).is_ok());
    assert!(load_rate_table("t", &table("")).unwrap_err().to_string().contains("at least one row"));
    assert!(load_rate_table("t", &table("rate('1.0', age_from = 40, age_to = 30)"))
        .unwrap_err()
        .to_string()
        .contains("inverted age band"));
    assert!(load_rate_table("t", &table("rate('-1.0')")).unwrap_err().to_string().contains("negative rate"));
    assert!(load_rate_table("t", &table("rate('1.0')").replace("version = 1", "version = 0")).is_err());

    let defs = |body: &str| format!("DEFINITIONS = [{body}]");
    let one = "definition('death', 1, '2027-01-01', 'Death', 'wording')";
    assert_eq!(load_definitions("d", &defs(one)).unwrap()[0].text_hash.len(), 64);
    assert!(load_definitions("d", &defs(&format!("{one}, {one}")))
        .unwrap_err()
        .to_string()
        .contains("declared twice"));
    assert!(load_definitions("d", &defs("definition('death', 1, '2027-01-01', 'Death', '')"))
        .unwrap_err()
        .to_string()
        .contains("no wording"));
}

/// Purpose: a rate row that leaves the upper age open covers every age.
/// Why it matters: per-life and group tables omit the band; if the default were zero
/// instead of the maximum age, those tables would rate nobody.
#[test]
fn a_rate_row_without_an_upper_age_covers_every_age() {
    let t = load_rate_table(
        "t",
        "RATE_TABLE = {'id': 't', 'version': 1, 'effective_from': '2027-01-01', 'rows': [{'age_from': 0, 'annual_rate': '2.0'}]}",
    )
    .unwrap();
    assert_eq!(t.rows[0].age_to, 150);
}

/// Purpose: rating fails with a named error when no row fits the life, when the sum
/// insured is outside the cover's limits, when the frequency is not offered, and when an
/// annual premium names a cover the product does not have.
/// Why it matters: each of these is a business refusal the caller must be able to explain
/// to a customer or adviser. None may fall through to a zero premium.
#[test]
fn rating_refuses_what_it_cannot_price() {
    use ic_product::rating::{check_sum_insured, RatingError};
    let p = load_product("term_life.star", TERM).unwrap();
    let life = load_rate_table("life", LIFE_RATES).unwrap();
    let child = RatingFactors {
        excess_option: None,
        age: 5,
        sex: Some(Sex::Female),
        smoker: Some(false),
        category: None,
        occupation_class: None,
        loadings: vec![],
    };
    assert!(matches!(
        cover_premium(p.cover("life").unwrap(), &life, &child, 500_000_00),
        Err(RatingError::NoRate { .. })
    ));

    let cover = p.cover("life").unwrap();
    assert!(check_sum_insured(cover, 50_000_00).is_ok());
    assert!(matches!(check_sum_insured(cover, 49_999_99), Err(RatingError::SumInsuredOutOfRange)));
    assert!(matches!(check_sum_insured(cover, 5_000_000_01), Err(RatingError::SumInsuredOutOfRange)));

    let mut annual = BTreeMap::new();
    annual.insert("life".to_string(), ic_money::dec("600").unwrap());
    assert!(matches!(
        modal_premium(&p, &annual, Frequency::Quarterly),
        Err(RatingError::FrequencyNotOffered)
    ));
    annual.insert("pets".to_string(), ic_money::dec("1").unwrap());
    assert!(matches!(modal_premium(&p, &annual, Frequency::Monthly), Err(RatingError::UnknownCover(_))));
}

/// Purpose: levies are charged on the rounded premium, per levy code, and summed across
/// covers that carry the same levy.
/// Why it matters: a levy is money collected for someone else (a fire service or
/// regulator). It is rounded per cover and remitted in total, so the per-code sum must
/// equal what was billed line by line.
#[test]
fn levies_are_charged_per_cover_and_summed_per_code() {
    let with_levies = TERM
        .replace("expiry_age = 100,", "expiry_age = 100, levies = [levy(\"fsl\", \"0.10\")],")
        .replace("expiry_age = 70,", "expiry_age = 70, levies = [levy(\"fsl\", \"0.10\")],");
    let p = load_product("levied.star", &with_levies).unwrap();
    let mut annual = BTreeMap::new();
    annual.insert("life".to_string(), ic_money::dec("1200").unwrap());
    annual.insert("trauma".to_string(), ic_money::dec("600").unwrap());
    let m = modal_premium(&p, &annual, Frequency::Yearly).unwrap();
    // Two covers: 5% discount. 1140.00 and 570.00, each levied at 10%.
    assert_eq!(m.by_cover["life"].minor, 1140_00);
    assert_eq!(m.levies["fsl"].minor, 114_00 + 57_00);
    assert_eq!(m.total(), 1140_00 + 570_00 + 72_00 + 171_00);
}

/// Purpose: every billing frequency reports its periods per year and its configuration key.
/// Why it matters: these two tables drive the modal premium and the lookup of the modal
/// loading. A wrong count for a frequency no shipped product uses yet would go unseen.
#[test]
fn every_frequency_has_its_period_count_and_key() {
    let expected = [
        (Frequency::Fortnightly, 26, "fortnightly"),
        (Frequency::Monthly, 12, "monthly"),
        (Frequency::Quarterly, 4, "quarterly"),
        (Frequency::HalfYearly, 2, "half_yearly"),
        (Frequency::Yearly, 1, "yearly"),
    ];
    for (f, periods, key) in expected {
        assert_eq!(f.periods_per_year(), periods);
        assert_eq!(f.key(), key);
        assert_eq!(serde_json::to_value(f).unwrap(), key);
    }
}

/// Purpose: a product's combination rules allow a life any set of covers that has a core
/// cover, has every cover its other covers require and has at most one of each exclusive
/// set; a life with no covers breaks no rule; a product with no rules allows anything; and
/// rules that name a cover the product does not offer, or make a cover require itself,
/// are refused when the product is loaded.
/// Why it matters: these rules decide which policies can exist on a mix-and-match
/// product. A rule naming a cover that is not there would never bite, and nobody would
/// know until a combination the product was never priced for had been sold.
#[test]
fn combination_rules_decide_which_sets_of_covers_a_life_may_hold() {
    use std::collections::BTreeSet;
    let exclusive = FLEX.replace(
        "requires = {\"specialist\": [\"hospital\"]},",
        "requires = {\"specialist\": [\"hospital\"]}, exclusive = [[\"dental\", \"optical\"]],",
    );
    let product = load_product("flex.star", &exclusive).unwrap();
    let check = |held: &[&str]| product.check_composition(&held.iter().copied().collect::<BTreeSet<_>>());
    assert!(check(&[]).is_ok());
    assert!(check(&["hospital", "specialist", "dental"]).is_ok());
    assert!(check(&["specialist"]).unwrap_err().contains("at least one of"));
    assert!(check(&["specialist", "dental"]).unwrap_err().contains("specialist is held only with hospital"));
    assert!(check(&["hospital", "dental", "optical"]).unwrap_err().contains("only one of dental, optical"));
    // A product that declares no rules allows any set.
    let term = load_product("term_life.star", TERM).unwrap();
    assert!(term.check_composition(&["trauma"].into_iter().collect()).is_ok());

    for (from, to) in [
        ("core = [\"hospital\", \"dental\", \"optical\"]", "core = [\"hospital\", \"physio\"]"),
        ("requires = {\"specialist\": [\"hospital\"]}", "requires = {\"specialist\": [\"specialist\"]}"),
    ] {
        let changed = FLEX.replace(from, to);
        assert_ne!(changed, FLEX);
        assert!(load_product("bad.star", &changed).is_err(), "{to} should be refused");
    }
}

/// Purpose: a cover's excess levels load with their excess and price factor; a level with
/// a repeated code, a negative excess or a factor of zero is refused when the product is
/// loaded; and rating multiplies the cover's premium by the chosen level's factor and refuses
/// a level the cover does not offer, a level on a cover that offers none, and no level on a
/// cover that offers some.
/// Why it matters: the excess level is a price input chosen by the customer. A mistyped
/// level that rated at the standard price, or a factor of zero that made a cover free, would
/// be wrong premium on every policy that chose it.
#[test]
fn excess_levels_price_a_cover_and_bad_ones_are_refused() {
    use ic_product::rating::{cover_premium, RatingError};
    let p = load_product("health_flex.star", FLEX).unwrap();
    let hospital = p.cover("hospital").unwrap();
    let dental = p.cover("dental").unwrap();
    assert_eq!(
        hospital.excess_options.iter().map(|o| (o.code.as_str(), o.excess_minor)).collect::<Vec<_>>(),
        [("nil", 0), ("standard", 500_00), ("high", 2_000_00)]
    );
    assert!(hospital.accepts_excess(Some("high")) && !hospital.accepts_excess(None) && !hospital.accepts_excess(Some("platinum")));
    assert!(dental.accepts_excess(None) && !dental.accepts_excess(Some("nil")));

    let rates = load_rate_table("flex-hospital", FLEX_RATES[0]).unwrap();
    let chosen = |option: Option<&str>| RatingFactors {
        excess_option: option.map(str::to_string),
        age: 42,
        sex: None,
        smoker: None,
        category: None,
        occupation_class: None,
        loadings: vec![],
    };
    let price = |option| cover_premium(hospital, &rates, &chosen(option), 0);
    let standard = price(Some("standard")).unwrap();
    assert_eq!(price(Some("nil")).unwrap(), standard * ic_money::dec("1.20").unwrap());
    assert_eq!(price(Some("high")).unwrap(), standard * ic_money::dec("0.80").unwrap());
    assert!(matches!(price(Some("platinum")), Err(RatingError::ExcessOption)));
    assert!(matches!(price(None), Err(RatingError::ExcessOption)));
    let dental_rates = load_rate_table("flex-dental", FLEX_RATES[2]).unwrap();
    assert!(matches!(
        cover_premium(dental, &dental_rates, &chosen(Some("nil")), 0),
        Err(RatingError::ExcessOption)
    ));

    for (from, to) in [
        ("excess_option(\"standard\", dollars(500)", "excess_option(\"nil\", dollars(500)"),
        ("excess_option(\"high\", dollars(2000)", "excess_option(\"high\", -1"),
        ("premium_factor = \"0.80\"", "premium_factor = \"0\""),
    ] {
        assert_ne!(FLEX.replace(from, to), FLEX, "{from} is in the shipped product");
        let err = load_product("bad.star", &FLEX.replace(from, to)).unwrap_err().to_string();
        assert!(err.contains("excess option"), "{to}: {err}");
    }
}

/// Purpose: a life whose rating sex is not recorded, as for a life who is neither male nor
/// female, is priced on a sex-rated table at the lower of the female and male rates for the
/// life's other facts by default, and at the stated blend where the product says so. A life
/// whose sex is recorded keeps its own row, a table that does not rate by sex is unaffected,
/// an age the table does not hold is still refused, and a blend outside 0 to 1 does not load.
/// Why it matters: decision 1 in `docs/decisions.md` (persona test PRD-011). Before this the
/// life could not be priced at all, so the only way to issue the policy was to record a sex
/// that is not the person's. The lower rate is the one that cannot be unfair to the customer.
#[test]
fn a_life_whose_sex_the_table_does_not_hold_is_rated_at_the_lower_rate() {
    use ic_product::rating::RatingError;
    use ic_product::UnmatchedSexRating;
    let life = load_rate_table("life", LIFE_RATES).unwrap();
    let factors = |age, sex| RatingFactors {
        age,
        sex,
        smoker: Some(false),
        ..RatingFactors::default()
    };
    let p = load_product("term_life.star", TERM).unwrap();
    let cover = p.cover("life").unwrap();
    assert_eq!(cover.rating.unmatched_sex_rating, UnmatchedSexRating::LowerOf);
    let price = |cover, sex| cover_premium(cover, &life, &factors(40, sex), 500_000_00).unwrap();
    let (female, male) = (price(cover, Some(Sex::Female)), price(cover, Some(Sex::Male)));
    assert_ne!(female, male, "the shipped life table rates by sex");
    assert_eq!(price(cover, None), female.min(male));

    let blended = load_product(
        "term_life.star",
        &TERM.replace("stepped(\"life-base\")", "stepped(\"life-base\", unmatched_sex_rating = blended(\"0.25\"))"),
    )
    .unwrap();
    let cover = blended.cover("life").unwrap();
    let quarter = ic_money::dec("0.25").unwrap();
    assert_eq!(price(cover, None), female * quarter + male * (ic_money::dec("1").unwrap() - quarter));
    assert_eq!(price(cover, Some(Sex::Male)), male, "a recorded sex keeps its own row");

    // No row for the age under either sex: still no rate, never half a guess.
    assert!(matches!(
        cover_premium(cover, &life, &factors(5, None), 500_000_00),
        Err(RatingError::NoRate { .. })
    ));

    // A table that does not rate by sex prices the life from its own row.
    let health = load_product("health_plan.star", HEALTH).unwrap();
    let rates = load_rate_table("health", HEALTH_RATES).unwrap();
    let hospital = health.covers.first().unwrap();
    let with = |sex| {
        cover_premium(
            hospital,
            &rates,
            &RatingFactors {
                excess_option: None,
                ..factors(40, sex)
            },
            0,
        )
        .unwrap()
    };
    assert_eq!(with(None), with(Some(Sex::Female)));

    for share in ["1.5", "-0.1"] {
        let bad = TERM.replace(
            "stepped(\"life-base\")",
            &format!("stepped(\"life-base\", unmatched_sex_rating = blended(\"{share}\"))"),
        );
        assert!(load_product("term_life.star", &bad).is_err(), "{share}");
    }
}

/// Purpose: the shipped income protection product prices a monthly benefit per thousand of
/// that benefit from its own table, exactly; its cover has a 28-day waiting period, pays for
/// up to 60 months and suspends the benefit in custody; and a product may say `pay` instead,
/// while any other word does not load.
/// Why it matters: decision 5 in `docs/decisions.md` (persona test CLM-040) makes custody a
/// matter of the product's wording. Claims acts on this setting and the waiting period, so a
/// product that loaded with a misspelt setting would silently pay or suspend the wrong way.
#[test]
fn income_protection_prices_a_monthly_benefit_and_carries_its_custody_setting() {
    use ic_product::{Benefit, WhileImprisoned};
    let p = load_product("income_protection.star", INCOME).unwrap();
    let rates = load_rate_table("income", INCOME_RATES).unwrap();
    let cover = p.cover("income").unwrap();
    assert_eq!(
        cover.benefit,
        Benefit::MonthlyBenefit {
            waiting_days: 28,
            benefit_months: 60,
            benefit_while_imprisoned: WhileImprisoned::Suspend,
        }
    );
    // Female non-smoker aged 20: 72.0000 per thousand of a 5,000 monthly benefit.
    let f = RatingFactors {
        age: 20,
        sex: Some(Sex::Female),
        smoker: Some(false),
        ..RatingFactors::default()
    };
    assert_eq!(cover_premium(cover, &rates, &f, 5_000_00).unwrap(), ic_money::dec("360").unwrap());

    let pays = load_product("income_protection.star", &INCOME.replace("\"suspend\"", "\"pay\"")).unwrap();
    assert!(matches!(
        pays.cover("income").unwrap().benefit,
        Benefit::MonthlyBenefit {
            benefit_while_imprisoned: WhileImprisoned::Pay,
            ..
        }
    ));
    assert!(load_product("income_protection.star", &INCOME.replace("\"suspend\"", "\"sometimes\"")).is_err());
}

/// Purpose: two covers of a health product can name one shared counter for their benefit
/// limits, and a cover may say whether premium is waived while a benefit is paid; a counter
/// named on one cover alone, or with no name, does not load.
/// Why it matters: claims counts limits per cover unless the product says two covers draw on
/// one. A top-up that named a counter its base plan did not would silently double the
/// customer's limit, so a shared counter nobody shares is a configuration error to stop.
#[test]
fn covers_can_share_a_limit_counter_and_declare_a_premium_waiver() {
    let shared = FLEX
        .replace(
            "code = \"hospital\",",
            "code = \"hospital\",\n            shared_counter = \"surgical\",\n            premium_waiver = False,",
        )
        .replace("code = \"specialist\",", "code = \"specialist\",\n            shared_counter = \"surgical\",");
    assert_ne!(shared, FLEX, "the shipped product has the covers this test edits");
    let p = load_product("health_flex.star", &shared).unwrap();
    let cover = |code| p.cover(code).unwrap();
    assert_eq!(cover("hospital").shared_counter.as_deref(), Some("surgical"));
    assert_eq!(cover("specialist").shared_counter, cover("hospital").shared_counter);
    assert_eq!((cover("hospital").premium_waiver, cover("dental").premium_waiver), (Some(false), None));
    assert_eq!(cover("dental").shared_counter, None);

    let alone = FLEX.replace("code = \"hospital\",", "code = \"hospital\",\n            shared_counter = \"surgical\",");
    assert!(load_product("health_flex.star", &alone).is_err());
    let unnamed = shared.replace("\"surgical\"", "\"\"");
    assert!(load_product("health_flex.star", &unnamed).is_err());
}

/// Purpose: a lump sum cover can name the covers a payment on it reduces and can be declared
/// joint on the first event; the shipped term life product declares neither; and a cover that
/// reduces itself, a cover that is not there, or a cover that pays no lump sum does not load,
/// nor does a joint cover that pays a monthly benefit.
/// Why it matters: the core takes cover off on a claim from these two settings alone. A link
/// to a cover that does not exist would silently reduce nothing, and a joint income benefit
/// has no single sum to pay once, so each is a configuration error to stop at load.
#[test]
fn a_cover_can_reduce_another_and_be_joint_on_the_first_event() {
    let shipped = load_product("term_life.star", TERM).unwrap();
    assert!(shipped.covers.iter().all(|c| c.reduces.is_empty() && !c.joint_first_event));
    let linked = |to: &str| TERM.replace("code = \"trauma\",", &format!("code = \"trauma\",\n            reduces = [\"{to}\"],"));
    let p = load_product("term_life.star", &linked("life")).unwrap();
    assert_eq!(p.cover("trauma").unwrap().reduces, vec!["life".to_string()]);
    assert!(load_product("term_life.star", &linked("trauma")).is_err());
    assert!(load_product("term_life.star", &linked("funeral")).is_err());

    let joint = TERM.replace("code = \"life\",", "code = \"life\",\n            joint_first_event = True,");
    assert!(load_product("term_life.star", &joint).unwrap().cover("life").unwrap().joint_first_event);
    let income = INCOME.replace("code = \"income\",", "code = \"income\",\n            joint_first_event = True,");
    assert_ne!(income, INCOME, "the shipped product has the cover this test edits");
    assert!(load_product("income_protection.star", &income).is_err());
}

/// A term life variant whose features end with the given discounts feature in place of the
/// shipped multi-cover discount.
fn with_discounts(feature: &str) -> ProductDefinition {
    let source = TERM.replace("multi_cover_discount(min_covers = 2, percent = \"5\"),", feature);
    assert_ne!(source, TERM, "the shipped product has the feature this test replaces");
    load_product("term_life.star", &source).unwrap()
}

fn annual(life: &str, trauma: &str) -> BTreeMap<String, Dec> {
    [("life".to_string(), dec(life).unwrap()), ("trauma".to_string(), dec(trauma).unwrap())].into()
}

fn lines(p: &ModalPremium) -> Vec<(&str, i64)> {
    p.discounts.iter().map(|d| (d.code.as_str(), d.amount.minor)).collect()
}

/// Purpose: a chain of discounts runs in the product's order: a single discount takes its
/// percentage of what earlier ones left, an additive set takes each member's share of the
/// base premium, and of a best-of set only the largest applies; a discount whose condition
/// does not hold (too few lives, a frequency not named, an entitlement not held) is not on
/// the bill; each discount that applied is its own line; and the premium before discounts
/// less the lines is the net premium to the cent.
/// Why it matters: customers are shown each discount by name and finance costs each
/// programme from its own line, so a chain that stacked in the wrong order, or lines that
/// did not sum to what is billed, would misstate both the notice and the ledger.
#[test]
fn discounts_stack_in_the_declared_order_and_each_is_a_line_that_sums_to_the_net() {
    let p = with_discounts(
        r#"discounts(chain = [
            discount("multi-cover", "Multi-cover", "discount_multi_cover", percent_off("10"), min_covers(2)),
            additive([
                discount("couple", "Two lives", "discount_multi_life", percent_off("5"), min_lives(2)),
                discount("annual", "Paid yearly", "discount_frequency", percent_off("4"), paid(["yearly"])),
                discount("staff", "Staff", "discount_staff", percent_off("2"), entitled("staff")),
            ]),
            best_of([
                discount("loyal", "Loyalty", "discount_loyalty", fixed_annual("30.00")),
                discount("welcome", "Welcome", "discount_welcome", percent_off("1"), covers = ["life"]),
            ]),
        ]),"#,
    );
    let held = [Held {
        code: "staff".to_string(),
        tier: String::new(),
        life: None,
    }];
    let state = |lives, frequency| PricingState {
        overrides: &[],
        covers: 2,
        lives,
        frequency,
        entitlements: &held,
    };
    // Base 1,000 + 200 a year, paid yearly by two lives: 10% of each, then (5 + 4 + 2)% of
    // base, then the fixed 30.00 (more than 1% of what is left on life) across both covers.
    let bill = modal_premium_with(&p, &annual("1000", "200"), &state(2, Frequency::Yearly)).unwrap();
    assert_eq!(
        lines(&bill),
        vec![
            ("multi-cover", 120_00),
            ("couple", 60_00),
            ("annual", 48_00),
            ("staff", 24_00),
            ("loyal", 30_00)
        ]
    );
    let gross: i64 = bill.gross_by_cover.values().map(|m| m.minor).sum();
    let net: i64 = bill.by_cover.values().map(|m| m.minor).sum();
    assert_eq!((gross, net), (1_200_00, 918_00));
    assert_eq!(gross - lines(&bill).iter().map(|l| l.1).sum::<i64>(), net);

    // One life paying monthly: the two-lives and yearly discounts are not on the bill.
    let monthly = modal_premium_with(&p, &annual("1000", "200"), &state(1, Frequency::Monthly)).unwrap();
    let codes: Vec<&str> = lines(&monthly).iter().map(|l| l.0).collect();
    assert_eq!(codes, vec!["multi-cover", "staff", "loyal"]);
    let gross: i64 = monthly.gross_by_cover.values().map(|m| m.minor).sum();
    let net: i64 = monthly.by_cover.values().map(|m| m.minor).sum();
    assert_eq!(
        gross - lines(&monthly).iter().map(|l| l.1).sum::<i64>(),
        net,
        "the last line absorbs the rounding"
    );

    // The shipped product's multi-cover discount is a line too, and without a second cover
    // there is no discount and no line before discounts.
    let shipped = load_product("term_life.star", TERM).unwrap();
    let two = modal_premium(&shipped, &annual("1000", "200"), Frequency::Yearly).unwrap();
    assert_eq!(lines(&two), vec![("multi-cover", 60_00)]);
    let one: BTreeMap<String, Dec> = [("life".to_string(), dec("1000").unwrap())].into();
    let alone = modal_premium(&shipped, &one, Frequency::Yearly).unwrap();
    assert!(alone.discounts.is_empty() && alone.gross_by_cover.is_empty());
}

/// Purpose: a tier discount reads its percentage from the tier of the entitlement held and
/// gives nothing for a tier the table does not name; a discount's own yearly cap, the cap on
/// all discounts and the floor under the net premium are kept, and what the cap and the
/// floor take back comes off the last discount in the order, leaving earlier ones in full.
/// Why it matters: the wellness tier a module awards must turn into exactly the product's
/// percentage, and a customer holding several discounts must never be billed less than the
/// floor the product was priced on, while still being shown the earlier discounts whole.
#[test]
fn a_tier_sets_the_percentage_and_the_cap_and_floor_come_off_the_last_discount() {
    let chain = r#"chain = [
            discount("wellness", "Wellness", "discount_wellness", by_tier({"1": "5", "2": "10"}), entitled("wellness")),
            discount("loyal", "Loyalty", "discount_loyalty", percent_off("50"), max_annual = "300.00"),
            discount("retain", "Retention", "discount_retention", percent_off("40")),
        ]"#;
    let tier = |t: &str| {
        vec![Held {
            code: "wellness".to_string(),
            tier: t.to_string(),
            life: None,
        }]
    };
    let run = |p: &ProductDefinition, held: &Vec<Held>| {
        let state = PricingState {
            overrides: &[],
            covers: 2,
            lives: 1,
            frequency: Frequency::Yearly,
            entitlements: held,
        };
        modal_premium_with(p, &annual("1000", "0"), &state).unwrap()
    };
    // Tier two: 10% of 1,000; loyalty 50% of 900 capped at 300; retention 40% of 600.
    let open = with_discounts(&format!("discounts({chain}),"));
    assert_eq!(
        lines(&run(&open, &tier("2"))),
        vec![("wellness", 100_00), ("loyal", 300_00), ("retain", 240_00)]
    );
    assert_eq!(lines(&run(&open, &tier("1")))[0], ("wellness", 50_00));
    assert_eq!(lines(&run(&open, &tier("9")))[0].0, "loyal", "a tier the table does not name gives nothing");

    // All discounts together are capped at 45% of base: 190 comes off retention.
    let capped = with_discounts(&format!("discounts({chain}, max_total_percent = \"45\"),"));
    assert_eq!(
        lines(&run(&capped, &tier("2"))),
        vec![("wellness", 100_00), ("loyal", 300_00), ("retain", 50_00)]
    );
    // A floor of 700 leaves room for 300 of discount: retention goes and loyalty gives 200.
    let floored = with_discounts(&format!("discounts({chain}, max_total_percent = \"45\", min_net_annual = \"700.00\"),"));
    let bill = run(&floored, &tier("2"));
    assert_eq!(lines(&bill), vec![("wellness", 100_00), ("loyal", 200_00)]);
    assert_eq!(bill.by_cover["life"].minor, 700_00);

    // A tier is per life: held by one of two lives on the cover, it reduces that life's
    // premium only, and a tier held by nobody on the policy reduces nothing.
    let only = with_discounts(r#"discounts(chain = [discount("wellness", "Wellness", "discount_wellness", by_tier({"2": "10"}), entitled("wellness"))]),"#);
    let line = |life: &str, annual: &str| {
        let line = Line {
            cover: "life".to_string(),
            life: life.to_string(),
        };
        (line, dec(annual).unwrap())
    };
    let couple: BTreeMap<Line, Dec> = [line("life-1", "1000"), line("life-2", "600")].into();
    let by_life = |held: &[Held]| {
        let state = PricingState {
            overrides: &[],
            covers: 1,
            lives: 2,
            frequency: Frequency::Yearly,
            entitlements: held,
        };
        modal_premium_by_line(&only, &couple, &state).unwrap()
    };
    let hers = Held {
        code: "wellness".to_string(),
        tier: "2".to_string(),
        life: Some("life-2".to_string()),
    };
    let bill = by_life(std::slice::from_ref(&hers));
    assert_eq!((lines(&bill), bill.by_cover["life"].minor), (vec![("wellness", 60_00)], 1_540_00));
    let stranger = Held {
        life: Some("life-9".to_string()),
        ..hers
    };
    assert!(by_life(&[stranger]).discounts.is_empty());
}

/// Purpose: a discount that would increase the premium (a negative percentage or amount, or
/// more than the whole premium) does not load, nor does a tier without an entitlement, two
/// discounts with one code, a discount on a cover the product does not have, an empty set, a
/// condition that counts from nothing, or a cap that is not a percentage.
/// Why it matters: a component that raises the premium is a loading, governed by
/// underwriting, or a fee. Letting one in as a discount would put a price increase on a
/// customer's bill under a name that promises a saving, and the other mistakes would each
/// bill a discount nobody can explain.
#[test]
fn a_discount_that_could_raise_the_premium_or_cannot_be_explained_does_not_load() {
    let load = |inner: &str| {
        let source = TERM.replace("multi_cover_discount(min_covers = 2, percent = \"5\"),", &format!("discounts({inner}),"));
        load_product("term_life.star", &source)
    };
    let one = |basis: &str, condition: &str| format!(r#"chain = [discount("d", "D", "discount_d", {basis}, {condition})]"#);
    assert!(load(&one("percent_off(\"10\")", "min_covers(2)")).is_ok());
    for (basis, condition) in [
        ("percent_off(\"-1\")", "None"),
        ("percent_off(\"100.5\")", "None"),
        ("fixed_annual(\"-5\")", "None"),
        ("by_tier({\"1\": \"5\"})", "min_covers(2)"),
        ("by_tier({})", "entitled(\"w\")"),
        ("by_tier({\"1\": \"-5\"})", "entitled(\"w\")"),
        ("percent_off(\"10\")", "min_covers(0)"),
        ("percent_off(\"10\")", "paid([])"),
        ("percent_off(\"10\")", "entitled(\"\")"),
    ] {
        assert!(load(&one(basis, condition)).is_err(), "{basis} with {condition}");
    }
    let d = |code: &str, extra: &str| format!(r#"discount("{code}", "D", "discount_d", percent_off("10"){extra})"#);
    assert!(load(&format!("chain = [{}, {}]", d("a", ""), d("a", ""))).is_err());
    assert!(load(&format!("chain = [{}]", d("a", ", covers = [\"funeral\"]"))).is_err());
    assert!(load(&format!("chain = [{}]", d("a", ", max_annual = \"-1\""))).is_err());
    assert!(load(&format!("chain = [{}]", d("", ""))).is_err());
    assert!(load("chain = [best_of([])]").is_err());
    assert!(load(&format!("chain = [{}], max_total_percent = \"120\"", d("a", ""))).is_err());
    assert!(load(&format!("chain = [{}], min_net_annual = \"-1\"", d("a", ""))).is_err());
}
