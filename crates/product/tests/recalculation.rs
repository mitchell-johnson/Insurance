//! An independent recalculation of premium, policy fee, discount and tax.
//!
//! The engine prices with the decimal library. Here every figure is worked
//! out again as a fraction of two integers, from the digits of the shipped
//! products and rate tables as written, with its own rounding. Nothing below
//! touches a decimal, so a fault in the decimal library, or in how the engine
//! uses it, cannot hide by being on both sides of the comparison.

use std::collections::BTreeMap;

use ic_money::{Dec, DecStr, RoundingMode};
use ic_product::discount::{Discount, DiscountBasis, DiscountCondition, DiscountStep};
use ic_product::rating::{cover_premium, modal_premium, RatingFactors};
use ic_product::{load_product, load_rate_table, CoverDefinition, Feature, Frequency, Levy, ProductDefinition, RateBasis, RateRow, RateTable, Sex};

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
        // Cross-reduced first, so the products stay inside 128 bits.
        let (g, h) = (gcd(self.top, o.bottom).max(1), gcd(o.top, self.bottom).max(1));
        Fraction::new((self.top / g).checked_mul(o.top / h).unwrap(), (self.bottom / h).checked_mul(o.bottom / g).unwrap())
    }
    /// Over the least common denominator, so long chains of sums stay small.
    fn plus(self, o: Fraction) -> Self {
        let g = gcd(self.bottom, o.bottom);
        let (a, b) = (o.bottom / g, self.bottom / g);
        Fraction::new(self.top.checked_mul(a).unwrap() + o.top.checked_mul(b).unwrap(), self.bottom.checked_mul(a).unwrap())
    }
    fn less(self, o: Fraction) -> Self {
        let g = gcd(self.bottom, o.bottom);
        let (a, b) = (o.bottom / g, self.bottom / g);
        Fraction::new(self.top.checked_mul(a).unwrap() - o.top.checked_mul(b).unwrap(), self.bottom.checked_mul(a).unwrap())
    }
    fn over(self, n: i128) -> Self {
        Fraction::new(self.top, self.bottom.checked_mul(n).unwrap())
    }
    /// The same amount to twelve places of a dollar, for shares split pro rata,
    /// whose exact denominators would outgrow 128 bits over a chain of steps.
    /// Twelve places sit ten below the cent, so a cent can only differ on a
    /// tie the engine's own 76 digits would also have to break.
    fn near(self) -> Self {
        const PLACES: i128 = 1_000_000_000_000;
        if self.bottom <= PLACES {
            return self;
        }
        let (whole, mut rest) = (self.top / self.bottom, self.top % self.bottom);
        let mut digits = 0i128;
        for _ in 0..12 {
            rest = rest.checked_mul(10).unwrap();
            digits = digits * 10 + rest / self.bottom;
            rest %= self.bottom;
        }
        let up = i128::from(rest.checked_mul(2).unwrap() >= self.bottom);
        Fraction::new(whole * PLACES + digits + up, PLACES)
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
                ..
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

/// Purpose: with a percentage discount, a fixed yearly discount, a levy and GST added to the
/// term life product on top of its multi-cover discount, every line of the bill across ages,
/// sexes, smoking and frequencies equals the integer recalculation: each cover's premium after
/// the three discounts in order, each discount's own line, the levy and the tax on the
/// discounted premium, and the lines add up to the cent.
/// Why it matters: no shipped product yet has a fixed discount or a levy, so the other two
/// grids never run that arithmetic, and a fixed amount shared between covers in proportion is
/// a division that does not come out even. These are the lines a product team will switch on
/// next, and they should be right before anyone is billed with them.
#[test]
fn a_chain_of_discounts_a_levy_and_tax_equal_an_independent_recalculation() {
    let tables = tables();
    let mut product = load_product(PRODUCTS[0].0, PRODUCTS[0].1).unwrap();
    let discount = |code: &str, basis| DiscountStep::One {
        discount: Discount {
            code: code.to_string(),
            name: code.to_string(),
            posting_type: format!("discount_{code}"),
            covers: vec![],
            basis,
            condition: DiscountCondition::Always,
            max_annual: None,
        },
    };
    let text = |s: &str| DecStr::parse(s).unwrap();
    product.features.push(Feature::Discounts {
        chain: vec![
            discount("loyalty", DiscountBasis::Percent { percent: text("7.5") }),
            discount("bundle", DiscountBasis::FixedAnnual { amount: text("40.00") }),
        ],
        max_total_percent: None,
        min_net_annual: None,
        override_single_approver_percent: None,
        override_max_percent: None,
    });
    product.covers[0].levies.push(Levy {
        code: "fire".to_string(),
        rate: text("0.0125"),
    });
    product.covers[1].gst_rate = Some(text("0.15"));
    let bill = billing(&product);
    let (multi, loyalty, bundle) = (Fraction::written("0.05"), Fraction::written("0.075"), Fraction::written("40.00"));
    let one = Fraction::whole(1);
    let mut compared = 0;
    for age in 18u32..=69 {
        for (sex, smoker) in [(Sex::Female, false), (Sex::Female, true), (Sex::Male, false), (Sex::Male, true)] {
            let mut years: Vec<Fraction> = vec![];
            let mut engine_years: BTreeMap<String, Dec> = BTreeMap::new();
            for cover in &product.covers {
                let table = &tables[&cover.rating.table];
                let factors = RatingFactors {
                    age,
                    sex: Some(sex),
                    smoker: Some(smoker),
                    loadings: loadings()[(age % 4) as usize].clone(),
                    ..RatingFactors::default()
                };
                let sum = sums(cover)[(age as usize) % sums(cover).len()];
                years.push(annual(cover, row(table, &factors).unwrap(), &factors, sum));
                engine_years.insert(cover.code.clone(), cover_premium(cover, table, &factors, sum).unwrap());
            }
            // The chain on a year's premium: 5% for two covers, 7.5% of what that leaves, then
            // $40 shared between the covers in proportion to what is left on each.
            let after_multi: Vec<Fraction> = years.iter().map(|y| y.times(one.less(multi))).collect();
            let after_loyalty: Vec<Fraction> = after_multi.iter().map(|y| y.times(one.less(loyalty))).collect();
            let reach = after_loyalty[0].plus(after_loyalty[1]);
            let share = |y: Fraction| bundle.times(y).times(Fraction::new(reach.bottom, reach.top));
            let net_years: Vec<Fraction> = after_loyalty.iter().map(|y| y.less(share(*y))).collect();
            for &frequency in &bill.frequencies {
                let m = modal(&bill, frequency);
                let cents = |f: Fraction| f.times(m).cents(product.rounding);
                let engine = modal_premium(&product, &engine_years, frequency).unwrap();
                let at = format!("age {age} {sex:?} smoker {smoker} {frequency:?}");
                let net = [cents(net_years[0]), cents(net_years[1])];
                let gross = [cents(years[0]), cents(years[1])];
                assert_eq!([engine.by_cover["life"].minor, engine.by_cover["trauma"].minor], net, "premiums: {at}");
                assert_eq!(
                    [engine.gross_by_cover["life"].minor, engine.gross_by_cover["trauma"].minor],
                    gross,
                    "before discounts: {at}"
                );
                let multi_line = cents(years[0].plus(years[1]).times(multi));
                let loyalty_line = cents(after_multi[0].plus(after_multi[1]).times(loyalty));
                let bundle_line = gross[0] + gross[1] - net[0] - net[1] - multi_line - loyalty_line;
                let lines: Vec<(&str, i64)> = engine.discounts.iter().map(|d| (d.code.as_str(), d.amount.minor)).collect();
                assert_eq!(
                    lines,
                    [("multi-cover", multi_line), ("loyalty", loyalty_line), ("bundle", bundle_line)],
                    "discount lines: {at}"
                );
                // The fixed discount's line is the share of $40 for the period, give or take the rounding it absorbs.
                assert!((bundle_line - cents(bundle)).abs() <= 2, "the fixed discount: {at}");
                let levy = Fraction::new(i128::from(net[0]), 100)
                    .times(Fraction::written("0.0125"))
                    .cents(product.rounding);
                let gst = Fraction::new(i128::from(net[1]), 100).times(Fraction::written("0.15")).cents(product.rounding);
                assert_eq!(engine.levies["fire"].minor, levy, "levy: {at}");
                assert_eq!(engine.gst["trauma"].minor, gst, "GST: {at}");
                compared += 1;
            }
        }
    }
    assert_eq!(compared, 52 * 4 * 3);
}


fn less_than(a: Fraction, b: Fraction) -> bool {
    a.top * b.bottom < b.top * a.bottom
}

fn min_of(a: Fraction, b: Fraction) -> Fraction {
    if less_than(b, a) {
        b
    } else {
        a
    }
}

fn total(v: &[Fraction]) -> Fraction {
    v.iter().fold(Fraction::whole(0), |a, b| a.plus(*b))
}

/// Purpose: the discount chain a product team can build from the other parts of the engine is
/// recalculated independently in fractions over the grid of ages, sexes and smoker status on a
/// two-cover policy: a percentage read from the tier of a wellness entitlement; a best-of set
/// (6% against $60 shared in proportion, whichever takes more); an additive set of 10% capped
/// at $50 a year and 4%, each on the base premium; then the cap of 20% on all discounts
/// together and the floor of $150 under the net, taken back from the last discount first.
/// Every period's premium, premium before discounts and discount line equals the
/// recalculation to the cent.
/// Why it matters: tiered and capped discounts were billed by the engine with nothing
/// independent to check them against. Their arithmetic is where a discount can quietly give
/// more than its cap, take a share twice, or take back from the wrong discount when the total
/// is too large, and each of those is a billing error on every policy that has it.
#[test]
fn tiered_best_of_capped_and_floored_discounts_equal_an_independent_recalculation() {
    use ic_product::discount::{Held, Line, PricingState};
    use ic_product::rating::modal_premium_by_line;
    let tables = tables();
    let mut product = load_product(PRODUCTS[0].0, PRODUCTS[0].1).unwrap();
    product.features.retain(|f| !matches!(f, Feature::MultiCoverDiscount { .. }));
    let text = |s: &str| DecStr::parse(s).unwrap();
    let discount = |code: &str, basis, condition, cap: Option<&str>| Discount {
        code: code.to_string(),
        name: code.to_string(),
        posting_type: format!("discount_{code}"),
        covers: vec![],
        basis,
        condition,
        max_annual: cap.map(text),
    };
    let tiers: BTreeMap<String, DecStr> = [("bronze", "2.5"), ("silver", "5"), ("gold", "10")].into_iter().map(|(k, v)| (k.to_string(), text(v))).collect();
    product.features.push(Feature::Discounts {
        chain: vec![
            DiscountStep::One {
                discount: discount(
                    "wellness",
                    DiscountBasis::Tier { percent_by_tier: tiers },
                    DiscountCondition::Entitlement { code: "vitality".into() },
                    None,
                ),
            },
            DiscountStep::BestOf {
                discounts: vec![
                    discount("loyalty", DiscountBasis::Percent { percent: text("6") }, DiscountCondition::Always, None),
                    discount("bundle", DiscountBasis::FixedAnnual { amount: text("60.00") }, DiscountCondition::Always, None),
                ],
            },
            DiscountStep::Additive {
                discounts: vec![
                    discount("staff", DiscountBasis::Percent { percent: text("10") }, DiscountCondition::Always, Some("50.00")),
                    discount("promo", DiscountBasis::Percent { percent: text("4") }, DiscountCondition::Always, None),
                ],
            },
        ],
        max_total_percent: Some(text("20")),
        min_net_annual: Some(text("150.00")),
        override_single_approver_percent: None,
        override_max_percent: None,
    });
    let bill = billing(&product);
    let zero = Fraction::whole(0);
    let pct = |s: &str| Fraction::written(s).over(100);
    let (mut compared, mut capped, mut floored, mut bundles, mut staff_capped) = (0, 0, 0, 0, 0);
    for age in 18u32..=69 {
        for (sex, smoker) in [(Sex::Female, false), (Sex::Female, true), (Sex::Male, false), (Sex::Male, true)] {
            let tier = ["", "bronze", "silver", "gold"][(age % 4) as usize];
            let mut base: Vec<Fraction> = vec![];
            let mut engine: BTreeMap<Line, Dec> = BTreeMap::new();
            let mut sums_insured = BTreeMap::new();
            for cover in &product.covers {
                let table = &tables[&cover.rating.table];
                let factors = RatingFactors {
                    age,
                    sex: Some(sex),
                    smoker: Some(smoker),
                    ..RatingFactors::default()
                };
                // Small sums as well as large, so the floor and the fixed share both come into play.
                let all = sums(cover);
                let sum = all[(age as usize * 7) % all.len()];
                base.push(annual(cover, row(table, &factors).unwrap(), &factors, sum));
                let line = Line {
                    cover: cover.code.clone(),
                    life: "a".into(),
                };
                engine.insert(line.clone(), cover_premium(cover, table, &factors, sum).unwrap());
                sums_insured.insert(line, sum);
            }

            // The chain, again, in fractions. Each step's amount per line, in order.
            let mut left = base.clone();
            let mut steps: Vec<(&str, Vec<Fraction>)> = vec![];
            let take = |left: &mut Vec<Fraction>, taken: &[Fraction]| {
                for (l, t) in left.iter_mut().zip(taken) {
                    *l = l.less(*t);
                }
            };
            if !tier.is_empty() {
                let f = pct(match tier {
                    "bronze" => "2.5",
                    "silver" => "5",
                    _ => "10",
                });
                let taken: Vec<Fraction> = left.iter().map(|l| min_of(l.times(f), *l)).collect();
                take(&mut left, &taken);
                steps.push(("wellness", taken));
            }
            let loyalty: Vec<Fraction> = left.iter().map(|l| min_of(l.times(pct("6")), *l)).collect();
            let reach = total(&left);
            let bundle: Vec<Fraction> = left
                .iter()
                .map(|l| min_of(l.times(Fraction::written("60.00").times(Fraction::new(reach.bottom, reach.top)).near()).near(), *l))
                .collect();
            let (code, best) = if less_than(total(&loyalty), total(&bundle)) { ("bundle", bundle) } else { ("loyalty", loyalty) };
            bundles += usize::from(code == "bundle");
            take(&mut left, &best);
            steps.push((code, best));
            let mut staff: Vec<Fraction> = base.iter().zip(&left).map(|(b, l)| min_of(b.times(pct("10")), *l)).collect();
            let cap = Fraction::written("50.00");
            if less_than(cap, total(&staff)) {
                let t = total(&staff);
                let share = cap.times(Fraction::new(t.bottom, t.top)).near();
                staff = staff.iter().map(|s| s.times(share).near()).collect();
                staff_capped += 1;
            }
            take(&mut left, &staff);
            steps.push(("staff", staff));
            let promo: Vec<Fraction> = base.iter().zip(&left).map(|(b, l)| min_of(b.times(pct("4")), *l)).collect();
            take(&mut left, &promo);
            steps.push(("promo", promo));
            // The cap on the total and the floor under the net, from the last discount back.
            let gross = total(&base);
            let floor = Fraction::written("150.00");
            let by_floor = if less_than(floor, gross) { gross.less(floor) } else { zero };
            let by_cap = gross.times(pct("20"));
            let allowed = min_of(by_cap, by_floor);
            let given = steps.iter().fold(zero, |a, (_, v)| a.plus(total(v)));
            if less_than(allowed, given) {
                if less_than(by_floor, by_cap) {
                    floored += 1;
                } else {
                    capped += 1;
                }
                let mut excess = given.less(allowed);
                for (_, amounts) in steps.iter_mut().rev() {
                    let t = total(amounts);
                    if excess == zero || t == zero {
                        continue;
                    }
                    let back = min_of(excess, t);
                    let share = back.times(Fraction::new(t.bottom, t.top)).near();
                    for a in amounts.iter_mut() {
                        *a = a.less(min_of(a.times(share).near(), *a));
                    }
                    excess = excess.less(back);
                }
            }
            // A step the cap or floor took all of may keep a trace from the twelve-place shares.
            let trace = Fraction::new(1, 1_000_000_000);
            steps.retain(|(_, v)| less_than(trace, total(v)));
            let net: Vec<Fraction> = (0..base.len()).map(|i| steps.iter().fold(base[i], |n, (_, v)| n.less(v[i]))).collect();

            let held = [Held {
                code: "vitality".into(),
                tier: tier.into(),
                life: None,
            }];
            for &frequency in &bill.frequencies {
                let state = PricingState {
                    covers: 2,
                    covers_elsewhere: 0,
                    lives: 1,
                    frequency,
                    entitlements: if tier.is_empty() { &[] } else { &held },
                    overrides: &[],
                    sums_insured: &sums_insured,
                };
                let got = modal_premium_by_line(&product, &engine, &state).unwrap();
                let m = modal(&bill, frequency);
                let cents = |f: Fraction| f.times(m).cents(product.rounding);
                let at = format!("age {age} {sex:?} smoker {smoker} tier {tier:?} {frequency:?}");
                let codes: Vec<&str> = product.covers.iter().map(|c| c.code.as_str()).collect();
                let nets: Vec<i64> = net.iter().map(|n| cents(*n)).collect();
                let grosses: Vec<i64> = base.iter().map(|b| cents(*b)).collect();
                assert_eq!(codes.iter().map(|c| got.by_cover[*c].minor).collect::<Vec<_>>(), nets, "premiums: {at}");
                if steps.is_empty() {
                    // Nothing applied (the floor took it all back): no gross lines and no discount lines.
                    assert!(got.gross_by_cover.is_empty() && got.discounts.is_empty(), "no discount lines: {at}");
                    assert_eq!(nets, grosses, "net is gross: {at}");
                    compared += 1;
                    continue;
                }
                assert_eq!(codes.iter().map(|c| got.gross_by_cover[*c].minor).collect::<Vec<_>>(), grosses, "before discounts: {at}");
                let mut expected: Vec<(String, i64)> = steps.iter().map(|(code, v)| (code.to_string(), cents(total(v)))).collect();
                // The last line takes what rounding leaves, so the lines add up to the cent.
                let residual = grosses.iter().sum::<i64>() - nets.iter().sum::<i64>() - expected[..expected.len() - 1].iter().map(|(_, c)| c).sum::<i64>();
                let own = expected.last().unwrap().1;
                assert!((residual - own).abs() <= 2, "the last line moves by rounding only: {at}");
                expected.last_mut().unwrap().1 = residual;
                let lines: Vec<(String, i64)> = got.discounts.iter().map(|d| (d.code.clone(), d.amount.minor)).collect();
                assert_eq!(lines, expected, "discount lines: {at}");
                compared += 1;
            }
        }
    }
    assert_eq!(compared, 52 * 4 * 3);
    assert!(capped > 0 && floored > 0 && bundles > 0 && staff_capped > 0, "every branch is reached: cap {capped}, floor {floored}, bundle {bundles}, staff cap {staff_capped}");
}

const AIA_PRODUCTS: [(&str, &str); 8] = [
    ("aia_living.star", include_str!("../../../products/aia_living.star")),
    ("aia_living_business.star", include_str!("../../../products/aia_living_business.star")),
    ("aia_start_up_income.star", include_str!("../../../products/aia_start_up_income.star")),
    ("aia_cover_for_life.star", include_str!("../../../products/aia_cover_for_life.star")),
    ("aia_essentials_life.star", include_str!("../../../products/aia_essentials_life.star")),
    ("aia_starter.star", include_str!("../../../products/aia_starter.star")),
    ("aia_private_health.star", include_str!("../../../products/aia_private_health.star")),
    ("aia_cancer_care.star", include_str!("../../../products/aia_cancer_care.star")),
];
const AIA_TABLES: [&str; 14] = [
    include_str!("../../../products/rates/aia_accidental_death.star"),
    include_str!("../../../products/rates/aia_accidental_injury.star"),
    include_str!("../../../products/rates/aia_business_continuity.star"),
    include_str!("../../../products/rates/aia_cancer_care.star"),
    include_str!("../../../products/rates/aia_family_protection.star"),
    include_str!("../../../products/rates/aia_income.star"),
    include_str!("../../../products/rates/aia_life.star"),
    include_str!("../../../products/rates/aia_private_health.star"),
    include_str!("../../../products/rates/aia_private_health_plus.star"),
    include_str!("../../../products/rates/aia_progressive_care.star"),
    include_str!("../../../products/rates/aia_severe_trauma.star"),
    include_str!("../../../products/rates/aia_specialist_testing.star"),
    include_str!("../../../products/rates/aia_tpd.star"),
    include_str!("../../../products/rates/aia_trauma.star"),
];

fn aia_tables() -> BTreeMap<String, RateTable> {
    AIA_TABLES
        .iter()
        .map(|s| {
            let t = load_rate_table("rates", s).unwrap();
            (t.id.clone(), t)
        })
        .collect()
}

/// The selections tried on a cover with option axes: the first choice on every axis, then each
/// other choice on its own with the first everywhere else, then the last choice on every axis.
/// Each is returned with its premium factor, multiplied out here from the factors as written.
fn aia_selections(cover: &CoverDefinition) -> Vec<(Option<String>, Fraction)> {
    if cover.option_axes.is_empty() {
        return excess_choices(cover)
            .into_iter()
            .map(|e| {
                let factor = e.as_ref().map_or(Fraction::whole(1), |c| {
                    Fraction::written(cover.excess_option(c).unwrap().premium_factor.as_str())
                });
                (e, factor)
            })
            .collect();
    }
    let n = cover.option_axes.len();
    let mut picks: Vec<Vec<usize>> = vec![vec![0; n]];
    for (i, axis) in cover.option_axes.iter().enumerate() {
        for j in 1..axis.choices.len() {
            let mut p = vec![0; n];
            p[i] = j;
            picks.push(p);
        }
    }
    picks.push(cover.option_axes.iter().map(|a| a.choices.len() - 1).collect());
    picks
        .into_iter()
        .filter_map(|p| {
            let chosen: Vec<_> = cover.option_axes.iter().zip(&p).map(|(a, &j)| (a, &a.choices[j])).collect();
            // A choice offered only with another is tried only where that other is chosen.
            let codes: Vec<String> = chosen.iter().map(|(a, c)| format!("{}={}", a.code, c.code)).collect();
            if chosen
                .iter()
                .any(|(_, c)| !c.only_with.is_empty() && !c.only_with.iter().any(|w| codes.contains(w)))
            {
                return None;
            }
            let factor = chosen
                .iter()
                .fold(Fraction::whole(1), |f, (_, c)| f.times(Fraction::written(c.premium_factor.as_str())));
            Some((Some(codes.join(",")), factor))
        })
        .collect()
}

/// Purpose: for every AIA retail product and cover, every row of its rate table, a spread of
/// sums insured, a loading and every frequency, each option selection tried (each choice on
/// each axis, and every excess level) is billed, to the cent, at the rate as written times the
/// product of the chosen premium factors as written, with the policy fee and GST; and the age
/// each premium type reads, worked out here from the rule as stated, is the one the engine uses.
/// Why it matters: an AIA option code multiplies several factors into the premium, which is
/// where a rounding fault would hide; and a level, level-term or limited-pay premium read at the
/// wrong age misprices for the life of the policy. This check shares no arithmetic with the
/// engine's.
#[test]
fn every_aia_option_premium_equals_an_independent_recalculation() {
    use ic_product::options::{pays_premium, rating_age};
    use ic_product::PremiumBasis;
    let tables = aia_tables();
    let (mut compared, mut bases) = (0u32, 0u32);
    for (file, source) in AIA_PRODUCTS {
        let product = load_product(file, source).unwrap();
        let bill = billing(&product);
        for cover in &product.covers {
            let table = &tables[&cover.rating.table];
            let selections = aia_selections(cover);
            assert!(!selections.is_empty(), "{file} {}", cover.code);
            for (option, factor) in &selections {
                // The age the premium type reads, for a life that started at 30 and is now 47.
                if let Some(basis) = cover.premium_basis(option.as_deref()) {
                    let (entry, now) = (30u32, 47u32);
                    let (age, pays) = match basis {
                        PremiumBasis::Stepped => (now, true),
                        PremiumBasis::Level { to_age } => (if now >= to_age { now } else { entry }, true),
                        PremiumBasis::LevelTerm { years } => (entry + (now - entry) / years * years, true),
                        PremiumBasis::LimitedPay { to_age } => (entry, now < to_age),
                    };
                    assert_eq!(
                        (rating_age(basis, entry, now), pays_premium(basis, now)),
                        (age, pays),
                        "{file} {} {option:?}",
                        cover.code
                    );
                    bases += 1;
                }
                for (n, rate) in table.rows.iter().enumerate() {
                    let factors = RatingFactors {
                        age: rate.age_from,
                        sex: rate.sex,
                        smoker: rate.smoker,
                        category: rate.category.clone(),
                        occupation_class: rate.occupation_class.clone(),
                        excess_option: option.clone(),
                        loadings: loadings()[n % 4].clone(),
                    };
                    let found = row(table, &factors).unwrap();
                    let all = sums(cover);
                    let sum = all[n % all.len()];
                    let no_option = RatingFactors {
                        excess_option: None,
                        ..factors.clone()
                    };
                    let year = annual(cover, found, &no_option, sum).times(*factor);
                    let engine_year = cover_premium(cover, table, &factors, sum).unwrap();
                    for &frequency in &bill.frequencies {
                        let premium = year.times(modal(&bill, frequency)).cents(product.rounding);
                        let fee = bill.fee.over(periods(frequency)).cents(product.rounding);
                        let gst = cover.gst_rate.as_ref().map_or(0, |r| {
                            Fraction::new(i128::from(premium), 100)
                                .times(Fraction::written(r.as_str()))
                                .cents(product.rounding)
                        });
                        let engine = modal_premium(&product, &BTreeMap::from([(cover.code.clone(), engine_year)]), frequency).unwrap();
                        let at = format!("{} {} {:?} sum {sum} {frequency:?}", product.code, cover.code, factors);
                        assert_eq!(engine.by_cover[&cover.code].minor, premium, "premium: {at}");
                        assert_eq!(engine.policy_fee.minor, fee, "policy fee: {at}");
                        assert_eq!(engine.gst.get(&cover.code).map_or(0, |g| g.minor), gst, "GST: {at}");
                        compared += 1;
                    }
                }
            }
        }
    }
    assert!(compared > 30_000 && bases > 100, "{compared} bills and {bases} premium types compared");
}

/// Purpose: one AIA Living life holding life, trauma, TPD and income cover at the
/// Multi-Benefit Discount minimums, across ages, sexes, smoking, occupation classes and
/// frequencies, is billed 15% off each cover, and each cover's premium, the premium before the
/// discount and the discount line equal the integer recalculation and add up to the cent;
/// with TPD and income a dollar under their minimums, only trauma counts beside life cover
/// and the life gets 10%.
/// Why it matters: the discount is judged per life on cover amounts and shared over four
/// covers, so its line has to absorb four roundings; a cent lost there is a cent between the
/// quote and the ledger on most AIA Living policies.
#[test]
fn the_aia_multi_benefit_discount_equals_an_independent_recalculation() {
    use ic_product::discount::{Line, PricingState};
    use ic_product::rating::modal_premium_by_line;
    let tables = aia_tables();
    let product = load_product(AIA_PRODUCTS[0].0, AIA_PRODUCTS[0].1).unwrap();
    let bill = billing(&product);
    let covers = [
        ("life", 10_000_000i64),
        ("critical-conditions", 7_500_000),
        ("tpd-own-occupation", 7_500_000),
        ("income-indemnity", 200_000),
    ];
    let mut compared = 0;
    for age in 18u32..=60 {
        for (sex, smoker) in [(Sex::Female, false), (Sex::Male, true)] {
            for class in ["1", "3", "5"] {
                for (short, off) in [(0i64, "0.15"), (100, "0.10")] {
                    let mut years: BTreeMap<String, Fraction> = BTreeMap::new();
                    let mut engine: BTreeMap<Line, Dec> = BTreeMap::new();
                    let mut held: BTreeMap<Line, i64> = BTreeMap::new();
                    for (i, (code, minimum)) in covers.iter().enumerate() {
                        let cover = product.cover(code).unwrap();
                        let table = &tables[&cover.rating.table];
                        // Trauma stays at its minimum; TPD and income fall short in the second case.
                        let sum = if i >= 2 { minimum - short } else { *minimum };
                        let (option, factor) = aia_selections(cover).swap_remove(0);
                        let factors = RatingFactors {
                            age,
                            sex: Some(sex),
                            smoker: Some(smoker),
                            occupation_class: Some(class.to_string()),
                            excess_option: option,
                            ..RatingFactors::default()
                        };
                        let found = row(table, &factors).unwrap();
                        let plain = RatingFactors {
                            excess_option: None,
                            ..factors.clone()
                        };
                        years.insert(code.to_string(), annual(cover, found, &plain, sum).times(factor));
                        let line = Line {
                            cover: code.to_string(),
                            life: "a".to_string(),
                        };
                        engine.insert(line.clone(), cover_premium(cover, table, &factors, sum).unwrap());
                        held.insert(line, sum);
                    }
                    let state = PricingState {
                        covers_elsewhere: 0,
                        overrides: &[],
                        covers: 4,
                        lives: 1,
                        frequency: Frequency::Yearly,
                        entitlements: &[],
                        sums_insured: &held,
                    };
                    let fraction = Fraction::written(off);
                    for &frequency in &bill.frequencies {
                        let state = PricingState { frequency, ..state };
                        let got = modal_premium_by_line(&product, &engine, &state).unwrap();
                        let m = modal(&bill, frequency);
                        let at = format!("age {age} {sex:?} smoker {smoker} class {class} short {short} {frequency:?}");
                        let (mut gross_total, mut net_total) = (0i64, 0i64);
                        for (code, year) in &years {
                            let gross = year.times(m).cents(product.rounding);
                            let net = year.times(Fraction::whole(1).less(fraction)).times(m).cents(product.rounding);
                            assert_eq!(got.by_cover[code].minor, net, "premium for {code}: {at}");
                            assert_eq!(got.gross_by_cover[code].minor, gross, "before the discount for {code}: {at}");
                            gross_total += gross;
                            net_total += net;
                        }
                        let lines: Vec<i64> = got.discounts.iter().map(|d| d.amount.minor).collect();
                        assert_eq!(lines, vec![gross_total - net_total], "discount: {at}");
                        compared += 1;
                    }
                }
            }
        }
    }
    assert_eq!(compared, 43 * 2 * 3 * 2 * 5);
}

/// Purpose: for every yearly premium from one cent to $300 and every frequency of every
/// shipped retail product, the period's premium equals the integer recalculation, and the run
/// includes over a thousand premiums that come out at exactly half a cent, which must go to
/// the even cent.
/// Why it matters: a grid of real rates and sums insured almost never lands on an exact half
/// cent, so it could not see that the engine divided by the number of periods before
/// multiplying: the rounded 1/26 pushed an exact tie upward and billed $10.99 where half-even
/// gives $10.98. Ties are where rounding rules differ, so they are swept here on purpose.
#[test]
fn premiums_that_land_on_exactly_half_a_cent_go_to_the_even_cent() {
    let (mut ties, mut compared) = (0u32, 0u32);
    for (file, source) in PRODUCTS {
        let product = load_product(file, source).unwrap();
        let bill = billing(&product);
        let cover = product.covers[0].code.clone();
        for cents in 1i128..=30_000 {
            let year = Fraction::new(cents, 100);
            let engine_year = ic_money::dec_i128(cents) / Dec::from(100i64);
            for &frequency in &bill.frequencies {
                let period = year.times(modal(&bill, frequency));
                // In cents the amount is a whole number and a half exactly.
                let in_cents = Fraction::new(period.top * 100, period.bottom);
                ties += u32::from(in_cents.bottom == 2);
                let engine = modal_premium(&product, &BTreeMap::from([(cover.clone(), engine_year)]), frequency).unwrap();
                assert_eq!(
                    engine.by_cover[&cover].minor,
                    period.cents(product.rounding),
                    "{} {cents} cents a year {frequency:?}",
                    product.code
                );
                compared += 1;
            }
        }
    }
    assert!(ties >= 1_000 && compared == 360_000, "{ties} ties in {compared} premiums");
}
