//! Scenario tests shipped with the products: given factors produce an exact premium.

use std::collections::BTreeMap;

use ic_money::DecStr;
use ic_product::rating::{cover_premium, modal_premium, RatingFactors};
use ic_product::{load_definitions, load_product, load_rate_table, Frequency, Sex};

const TERM: &str = include_str!("../../../products/term_life.star");
const HEALTH: &str = include_str!("../../../products/health_plan.star");
const GROUP: &str = include_str!("../../../products/group_life.star");
const LIFE_RATES: &str = include_str!("../../../products/rates/life_base.star");
const TRAUMA_RATES: &str = include_str!("../../../products/rates/trauma_base.star");
const HEALTH_RATES: &str = include_str!("../../../products/rates/health_base.star");
const GROUP_RATES: &str = include_str!("../../../products/rates/group_life_standard.star");
const DEFS: &str = include_str!("../../../products/definitions/library.star");

#[test]
fn all_shipped_configuration_loads() {
    for (f, s) in [("term_life.star", TERM), ("health_plan.star", HEALTH), ("group_life.star", GROUP)] {
        let p = load_product(f, s).unwrap();
        assert_eq!(p.content_hash.len(), 64);
        // features are sorted into the engine's order
        assert!(p.features.windows(2).all(|w| w[0].order() < w[1].order()));
    }
    for (f, s) in [("life", LIFE_RATES), ("trauma", TRAUMA_RATES), ("health", HEALTH_RATES), ("group", GROUP_RATES)] {
        load_rate_table(f, s).unwrap();
    }
    assert_eq!(load_definitions("library.star", DEFS).unwrap().len(), 7);
}

#[test]
fn hash_follows_content() {
    let a = load_product("a", TERM).unwrap();
    let b = load_product("b", &TERM.replace("72.00", "73.00")).unwrap();
    assert_ne!(a.content_hash, b.content_hash);
    assert_eq!(a.content_hash, load_product("c", TERM).unwrap().content_hash);
}

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

#[test]
fn loading_applies_to_base() {
    let p = load_product("term_life.star", TERM).unwrap();
    let life = load_rate_table("life", LIFE_RATES).unwrap();
    let mut f = RatingFactors {
        age: 37,
        sex: Some(Sex::Female),
        smoker: Some(false),
        category: None,
        loadings: vec![],
    };
    let base = cover_premium(p.cover("life").unwrap(), &life, &f, 500_000_00).unwrap();
    f.loadings.push(DecStr::parse("0.50").unwrap());
    let loaded = cover_premium(p.cover("life").unwrap(), &life, &f, 500_000_00).unwrap();
    assert_eq!(loaded, base * ic_money::dec("1.5").unwrap());
}

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

#[test]
fn bad_products_are_rejected() {
    assert!(load_product("x", &TERM.replace("earning(),", "")).is_err());
    assert!(load_product("x", &TERM.replace("lapse_days = 90", "lapse_days = 10")).is_err());
    assert!(load_product("x", &TERM.replace("line = \"life\"", "line = \"pets\"")).is_err());
    assert!(load_product("x", "PRODUCT = {'code': 'x', 'surprise': 1}").is_err());
}
