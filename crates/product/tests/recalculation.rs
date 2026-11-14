//! An independent recalculation of premium, policy fee, discount and tax.
//!
//! The engine prices with the decimal library. Here every figure is worked
//! out again as a fraction of two integers, from the digits of the shipped
//! products and rate tables as written, with its own rounding. Nothing below
//! touches a decimal, so a fault in the decimal library, or in how the engine
//! uses it, cannot hide by being on both sides of the comparison.

use std::collections::BTreeMap;

use ic_money::{Dec, DecStr, RoundingMode};
use ic_product::rating::{cover_premium, modal_premium, RatingFactors};
use ic_product::{load_product, load_rate_table, CoverDefinition, Feature, Frequency, ProductDefinition, RateBasis, RateRow, RateTable, Sex};

const PRODUCTS: [(&str, &str); 4] = [
    ("term_life.star", include_str!("../../../products/term_life.star")),
    ("health_plan.star", include_str!("../../../products/health_plan.star")),
    ("health_flex.star", include_str!("../../../products/health_flex.star")),
    ("income_protection.star", include_str!("../../../products/income_protection.star")),
];
const TABLES: [&str; 8] = [
    include_str!("../../../products/rates/life_base.star"),
    include_str!("../../../products/rates/trauma_base.star"),
    include_str!("../../../products/rates/health_base.star"),
    include_str!("../../../products/rates/income_base.star"),
    include_str!("../../../products/rates/flex_hospital.star"),
    include_str!("../../../products/rates/flex_specialist.star"),
    include_str!("../../../products/rates/flex_dental.star"),
    include_str!("../../../products/rates/flex_optical.star"),
];

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 {
        a.abs()
    } else {
        gcd(b, a % b)
    }
}

/// An exact, non-negative fraction of two integers.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fraction {
    top: i128,
    bottom: i128,
}

impl Fraction {
    fn new(top: i128, bottom: i128) -> Self {
        assert!(top >= 0 && bottom > 0, "the recalculation only handles amounts of zero or more");
        let g = gcd(top, bottom).max(1);
        Fraction {
            top: top / g,
            bottom: bottom / g,
        }
    }
    fn whole(n: i128) -> Self {
        Fraction::new(n, 1)
    }
    /// Read a decimal from its digits as written: "1.1813" is 11813 over 10000.
    fn written(s: &str) -> Self {
        let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
        let digits = format!("{whole}{fraction}");
        Fraction::new(digits.parse().unwrap(), 10i128.pow(fraction.len() as u32))
    }
    fn times(self, o: Fraction) -> Self {
        Fraction::new(self.top.checked_mul(o.top).unwrap(), self.bottom.checked_mul(o.bottom).unwrap())
    }
    fn plus(self, o: Fraction) -> Self {
        Fraction::new(self.top * o.bottom + o.top * self.bottom, self.bottom.checked_mul(o.bottom).unwrap())
    }
    fn less(self, o: Fraction) -> Self {
        Fraction::new(self.top * o.bottom - o.top * self.bottom, self.bottom.checked_mul(o.bottom).unwrap())
    }
    fn over(self, n: i128) -> Self {
        Fraction::new(self.top, self.bottom.checked_mul(n).unwrap())
    }
    /// Dollars to cents, rounded once by long division.
    fn cents(self, mode: RoundingMode) -> i64 {
        let top = self.top.checked_mul(100).unwrap();
        let (cut, rest) = (top / self.bottom, top % self.bottom);
        let up = match mode {
            RoundingMode::Down => false,
            RoundingMode::Up => rest != 0,
            RoundingMode::HalfUp => rest * 2 >= self.bottom,
            RoundingMode::HalfEven => rest * 2 > self.bottom || (rest * 2 == self.bottom && cut % 2 == 1),
        };
        i64::try_from(cut + i128::from(up)).unwrap()
    }
}

fn tables() -> BTreeMap<String, RateTable> {
    TABLES
        .iter()
        .map(|s| {
            let t = load_rate_table("rates", s).unwrap();
            (t.id.clone(), t)
        })
        .collect()
}

/// The first row of the table that fits, found without the engine's lookup.
fn row<'a>(table: &'a RateTable, f: &RatingFactors) -> Option<&'a RateRow> {
    table.rows.iter().find(|r| {
        (r.age_from..=r.age_to).contains(&f.age)
            && (r.sex.is_none() || r.sex == f.sex)
            && (r.smoker.is_none() || r.smoker == f.smoker)
            && (r.category.is_none() || r.category == f.category)
            && (r.occupation_class.is_none() || r.occupation_class == f.occupation_class)
    })
}

/// A year's premium for one cover, as a fraction of dollars.
fn annual(cover: &CoverDefinition, rate: &RateRow, f: &RatingFactors, sum_insured_minor: i64) -> Fraction {
    let rate = Fraction::written(rate.annual_rate.as_str());
    let base = match cover.rating.basis {
        RateBasis::PerThousand => Fraction::new(i128::from(sum_insured_minor), 100_000).times(rate),
        RateBasis::PerLife => rate,
    };
    let loaded = f.loadings.iter().fold(Fraction::whole(1), |sum, l| sum.plus(Fraction::written(l.as_str())));
    let excess = match &f.excess_option {
        Some(code) => Fraction::written(cover.excess_option(code).unwrap().premium_factor.as_str()),
        None => Fraction::whole(1),
    };
    base.times(loaded).times(excess)
}

struct Billing {
    frequencies: Vec<Frequency>,
    fee: Fraction,
    loading: BTreeMap<String, Fraction>,
    /// Covers needed for the multi-cover discount, and the fraction it takes off.
    multi_cover: Option<(u32, Fraction)>,
}

fn billing(product: &ProductDefinition) -> Billing {
    let mut out = None;
    let mut multi_cover = None;
    for feature in &product.features {
        match feature {
            Feature::Billing {
                frequencies,
                policy_fee_annual,
                modal_loading,
            } => {
                out = Some((
                    frequencies.clone(),
                    Fraction::written(policy_fee_annual.as_str()),
                    modal_loading.iter().map(|(k, v)| (k.clone(), Fraction::written(v.as_str()))).collect(),
                ));
            }
            Feature::MultiCoverDiscount { min_covers, percent } => multi_cover = Some((*min_covers, Fraction::written(percent.as_str()).over(100))),
            _ => {}
        }
    }
    let (frequencies, fee, loading) = out.unwrap();
    Billing {
        frequencies,
        fee,
        loading,
        multi_cover,
    }
}

fn periods(frequency: Frequency) -> i128 {
    match frequency {
        Frequency::Fortnightly => 26,
        Frequency::Monthly => 12,
        Frequency::Quarterly => 4,
        Frequency::HalfYearly => 2,
        Frequency::Yearly => 1,
    }
}

/// What one period is of a year's premium: one plus the frequency's loading, over the periods.
fn modal(billing: &Billing, frequency: Frequency) -> Fraction {
    let loading = billing.loading.get(frequency.key()).copied().unwrap_or(Fraction::whole(0));
    Fraction::whole(1).plus(loading).over(periods(frequency))
}

fn loadings() -> Vec<Vec<DecStr>> {
    [vec![], vec!["0.50"], vec!["0.75", "0.25"], vec!["1.275"]]
        .into_iter()
        .map(|l| l.into_iter().map(|s| DecStr::parse(s).unwrap()).collect())
        .collect()
}

fn sums(cover: &CoverDefinition) -> Vec<i64> {
    if cover.rating.basis == RateBasis::PerLife {
        return vec![0];
    }
    let (min, max) = (cover.min_sum_insured_minor, cover.max_sum_insured_minor.unwrap_or(500_000_000));
    let mut out = vec![min, min + 1, 12_345_678, 25_000_000, 33_333_333, 99_999_999, max - 1, max];
    out.retain(|s| (min..=max).contains(s));
    out.dedup();
    out
}

fn excess_choices(cover: &CoverDefinition) -> Vec<Option<String>> {
    if cover.excess_options.is_empty() {
        vec![None]
    } else {
        cover.excess_options.iter().map(|o| Some(o.code.clone())).collect()
    }
}

/// Purpose: for every shipped retail product, every cover, every row of its rate table at both
/// ends of the age band, a spread of sums insured (round, awkward and at the limits), several
/// loadings, every excess choice and every billing frequency, the premium, the policy fee and
/// the GST the engine bills equal, to the cent, what integer fractions give from the same
/// rates as written. About 30,000 bills are compared.
/// Why it matters: the engine's own tests check it against figures the engine produced, so
/// they could not see a rounding fault in the decimal library that made one premium in twenty
/// a cent low. This is the check that would have: a second calculation that shares no
/// arithmetic with the first. Any figure a cent out, anywhere on the grid, fails here.
#[test]
fn every_premium_on_the_grid_equals_an_independent_recalculation() {
    let tables = tables();
    let mut compared = 0u32;
    for (file, source) in PRODUCTS {
        let product = load_product(file, source).unwrap();
        let bill = billing(&product);
        for cover in &product.covers {
            let table = &tables[&cover.rating.table];
            for rate in &table.rows {
                let top = if rate.age_to > 120 { rate.age_from + 1 } else { rate.age_to };
                for age in [rate.age_from, top] {
                    for (loading, excess) in loadings()
                        .into_iter()
                        .flat_map(|l| excess_choices(cover).into_iter().map(move |e| (l.clone(), e)))
                    {
                        let factors = RatingFactors {
                            age,
                            sex: rate.sex,
                            smoker: rate.smoker,
                            category: rate.category.clone(),
                            occupation_class: rate.occupation_class.clone(),
                            excess_option: excess,
                            loadings: loading,
                        };
                        let found = row(table, &factors).unwrap();
                        for sum in sums(cover) {
                            let year = annual(cover, found, &factors, sum);
                            let engine_year = cover_premium(cover, table, &factors, sum).unwrap();
                            for &frequency in &bill.frequencies {
                                let m = modal(&bill, frequency);
                                let premium = year.times(m).cents(product.rounding);
                                let fee = bill.fee.over(periods(frequency)).cents(product.rounding);
                                let gst = cover.gst_rate.as_ref().map_or(0, |r| {
                                    Fraction::new(i128::from(premium), 100)
                                        .times(Fraction::written(r.as_str()))
                                        .cents(product.rounding)
                                });
                                let engine = modal_premium(&product, &BTreeMap::from([(cover.code.clone(), engine_year)]), frequency).unwrap();
                                let at = format!("{} {} age {age} {:?} sum {sum} {frequency:?}", product.code, cover.code, factors);
                                assert_eq!(engine.by_cover[&cover.code].minor, premium, "premium: {at}");
                                assert_eq!(engine.policy_fee.minor, fee, "policy fee: {at}");
                                assert_eq!(engine.gst.get(&cover.code).map_or(0, |g| g.minor), gst, "GST: {at}");
                                assert!(engine.discounts.is_empty(), "a discount on one cover: {at}");
                                compared += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(compared > 30_000, "only {compared} bills were compared");
}

/// Purpose: for a policy holding every cover of a product at once, across every age, both
/// sexes, smokers and non-smokers, every occupation class and every frequency, each cover's
/// premium after the multi-cover discount, each cover's premium before it, the discount line
/// and the GST all equal the integer recalculation, and the bill adds up: premium before
/// discount less the discount is the premium billed, to the cent.
/// Why it matters: a discount multiplies in another factor and adds a line that has to absorb
/// the rounding, which is where a cent goes missing between what the customer is quoted and
/// what the ledger posts. The single-cover grid cannot see that.
#[test]
fn a_policy_with_every_cover_equals_an_independent_recalculation() {
    let tables = tables();
    let mut compared = 0u32;
    let mut discounted = 0u32;
    for (file, source) in PRODUCTS {
        let product = load_product(file, source).unwrap();
        let bill = billing(&product);
        let mut classes: Vec<Option<String>> = vec![None];
        for cover in &product.covers {
            for r in &tables[&cover.rating.table].rows {
                if r.occupation_class.is_some() && !classes.contains(&r.occupation_class) {
                    classes.push(r.occupation_class.clone());
                }
            }
        }
        for age in 18u32..=75 {
            for (sex, smoker) in [(Sex::Female, false), (Sex::Female, true), (Sex::Male, false), (Sex::Male, true)] {
                for class in &classes {
                    let mut years: BTreeMap<String, Fraction> = BTreeMap::new();
                    let mut engine_years: BTreeMap<String, Dec> = BTreeMap::new();
                    for cover in &product.covers {
                        let table = &tables[&cover.rating.table];
                        let factors = RatingFactors {
                            age,
                            sex: Some(sex),
                            smoker: Some(smoker),
                            occupation_class: class.clone(),
                            excess_option: excess_choices(cover).pop().unwrap(),
                            loadings: loadings()[(age % 4) as usize].clone(),
                            ..RatingFactors::default()
                        };
                        // A cover that does not rate this life is left off the policy.
                        let Some(found) = row(table, &factors) else { continue };
                        let sum = sums(cover)[(age as usize) % sums(cover).len()];
                        years.insert(cover.code.clone(), annual(cover, found, &factors, sum));
                        engine_years.insert(cover.code.clone(), cover_premium(cover, table, &factors, sum).unwrap());
                    }
                    if years.is_empty() {
                        continue;
                    }
                    let off = bill.multi_cover.filter(|(min, _)| years.len() as u32 >= *min).map(|(_, fraction)| fraction);
                    for &frequency in &bill.frequencies {
                        let m = modal(&bill, frequency);
                        let engine = modal_premium(&product, &engine_years, frequency).unwrap();
                        let at = format!("{} age {age} {sex:?} smoker {smoker} class {class:?} {frequency:?}", product.code);
                        let (mut gross_total, mut net_total) = (0i64, 0i64);
                        for (code, year) in &years {
                            let gross = year.times(m).cents(product.rounding);
                            let net = match off {
                                Some(fraction) => year.times(Fraction::whole(1).less(fraction)).times(m).cents(product.rounding),
                                None => gross,
                            };
                            let rate = product.cover(code).unwrap().gst_rate.as_ref();
                            let gst = rate.map_or(0, |r| {
                                Fraction::new(i128::from(net), 100).times(Fraction::written(r.as_str())).cents(product.rounding)
                            });
                            assert_eq!(engine.by_cover[code].minor, net, "premium for {code}: {at}");
                            assert_eq!(engine.gst.get(code).map_or(0, |g| g.minor), gst, "GST for {code}: {at}");
                            if off.is_some() {
                                assert_eq!(engine.gross_by_cover[code].minor, gross, "premium before discount for {code}: {at}");
                            }
                            gross_total += gross;
                            net_total += net;
                        }
                        let lines: i64 = engine.discounts.iter().map(|d| d.amount.minor).sum();
                        assert_eq!(lines, gross_total - net_total, "discount: {at}");
                        discounted += u32::from(off.is_some());
                        compared += 1;
                    }
                }
            }
        }
    }
    assert!(compared > 2_000 && discounted > 500, "{compared} bills compared, {discounted} with a discount");
}
