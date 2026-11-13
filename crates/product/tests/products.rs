//! Scenario tests shipped with the products: given factors produce an exact premium.

use std::collections::BTreeMap;

use ic_money::DecStr;
use ic_product::rating::{cover_premium, modal_premium, RatingFactors};
use ic_product::{load_definitions, load_product, load_rate_table, Frequency, Sex};

const TERM: &str = include_str!("../../../products/term_life.star");
const HEALTH: &str = include_str!("../../../products/health_plan.star");
const GROUP: &str = include_str!("../../../products/group_life.star");
const INVEST: &str = include_str!("../../../products/invest_plan.star");
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
    ] {
        load_rate_table(f, s).unwrap();
    }
    for source in FLEX_RATES {
        load_rate_table("flex", source).unwrap();
    }
    assert_eq!(load_definitions("library.star", DEFS).unwrap().len(), 7);
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
