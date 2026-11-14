//! An independent recalculation of unit prices.
//!
//! The module strikes prices with the decimal library. Here every figure is
//! worked out again as an exact fraction of two integers, from the digits of
//! the valuation as written, with its own rounding. Nothing below calls the
//! decimal library, so a fault in it, or in how the module uses it, cannot
//! hide by being on both sides of the comparison (the first-dropped-digit
//! rounding fault, E2E-232, was one).
//!
//! Recalculated: the mid (half-even), buy (up) and sell (down) prices at the
//! fund's decimals and spreads; the previous move, benchmark and unexplained
//! move checks against their tolerances; and whether a correction is
//! material against the error threshold.

use chrono::NaiveDate;
use ic_core::invest::FundDefinition;
use ic_money::{DecStr, FundId};
use ic_pricing::fund::{FundRules, PricingTerms};
use ic_pricing::strike::{error_size, strike, CheckKind, Previous};
use ic_pricing::valuation::Valuation;

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 {
        a.abs()
    } else {
        gcd(b, a % b)
    }
}

/// An exact signed fraction, kept in lowest terms with a positive bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Q {
    top: i128,
    bottom: i128,
}

impl Q {
    fn new(top: i128, bottom: i128) -> Self {
        assert!(bottom != 0);
        let sign = if bottom < 0 { -1 } else { 1 };
        let g = gcd(top, bottom).max(1);
        Q {
            top: sign * top / g,
            bottom: sign * bottom / g,
        }
    }
    fn int(n: i128) -> Self {
        Q::new(n, 1)
    }
    /// A decimal from its digits as written: "-1.25" is -125 over 100.
    fn read(text: &str) -> Self {
        let (negative, digits) = text.strip_prefix('-').map_or((false, text), |rest| (true, rest));
        let (whole, part) = digits.split_once('.').unwrap_or((digits, ""));
        let bottom = 10i128.pow(part.len() as u32);
        let top: i128 = format!("{whole}{part}").parse().unwrap();
        Q::new(if negative { -top } else { top }, bottom)
    }
    fn add(self, o: Q) -> Q {
        Q::new(self.top * o.bottom + o.top * self.bottom, self.bottom * o.bottom)
    }
    fn sub(self, o: Q) -> Q {
        self.add(Q::new(-o.top, o.bottom))
    }
    fn mul(self, o: Q) -> Q {
        let (a, b) = (gcd(self.top, o.bottom).max(1), gcd(o.top, self.bottom).max(1));
        Q::new((self.top / a) * (o.top / b), (self.bottom / b) * (o.bottom / a))
    }
    fn div(self, o: Q) -> Q {
        assert!(o.top != 0);
        self.mul(Q::new(o.bottom, o.top))
    }
    fn abs(self) -> Q {
        Q::new(self.top.abs(), self.bottom)
    }
    fn le(self, o: Q) -> bool {
        self.top * o.bottom <= o.top * self.bottom
    }
    /// Rounded to `places` decimals and written with exactly that many.
    fn fixed(self, places: u32, mode: Mode) -> String {
        let scaled = self.mul(Q::int(10i128.pow(places)));
        let (q, r) = (scaled.top.div_euclid(scaled.bottom), scaled.top.rem_euclid(scaled.bottom));
        // q is the floor; r over bottom is what lies above it.
        let n = match mode {
            Mode::Up if self.top >= 0 => q + i128::from(r != 0),
            Mode::Up => q,
            Mode::Down if self.top >= 0 => q,
            Mode::Down => q + i128::from(r != 0),
            Mode::HalfEven => match (2 * r).cmp(&scaled.bottom) {
                std::cmp::Ordering::Less => q,
                std::cmp::Ordering::Greater => q + 1,
                std::cmp::Ordering::Equal => q + (q & 1),
            },
        };
        let scale = 10i128.pow(places);
        let sign = if n < 0 { "-" } else { "" };
        let a = n.abs();
        format!("{sign}{}.{:0width$}", a / scale, a % scale, width = places as usize)
    }
}

/// Up and down are away from and toward zero, as the module means them.
#[derive(Clone, Copy)]
enum Mode {
    Up,
    Down,
    HalfEven,
}

/// A small deterministic generator, so a failure is the same on every run.
struct Seq(u64);

impl Seq {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn between(&mut self, lo: i128, hi: i128) -> i128 {
        lo + (self.next() as i128) % (hi - lo + 1)
    }
}

fn money(cents: i128) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", cents.abs() / 100, cents.abs() % 100)
}

fn fraction(seq: &mut Seq, lo_bp: i128, hi_bp: i128) -> String {
    // A fraction of up to six decimals, between lo and hi basis points.
    let micro = seq.between(lo_bp * 100, hi_bp * 100);
    let sign = if micro < 0 { "-" } else { "" };
    format!("{sign}{}.{:06}", micro.abs() / 1_000_000, micro.abs() % 1_000_000)
}

fn valuation(fund: &FundId, nav: &str, units_micro: i64, flows: (&str, &str), benchmark: Option<String>) -> Valuation {
    Valuation {
        fund: fund.clone(),
        pricing_date: NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        nav: DecStr::parse(nav).unwrap(),
        units_on_issue_micro: units_micro,
        applications: DecStr::parse(flows.0).unwrap(),
        redemptions: DecStr::parse(flows.1).unwrap(),
        benchmark_return: benchmark.map(|b| DecStr::parse(&b).unwrap()),
        restated: false,
        stale_assets: Vec::new(),
    }
}

fn passed(checks: &[ic_pricing::strike::Check], kind: CheckKind) -> Option<bool> {
    checks.iter().find(|c| c.kind == kind).map(|c| c.passed)
}

/// The shipped funds, each also with other decimals and spreads, so the
/// recalculation reaches past the three shipped settings.
fn funds(seq: &mut Seq) -> Vec<FundDefinition> {
    let mut out = Vec::new();
    for def in ic_sim::config::shipped_funds().expect("the shipped fund files load") {
        out.push(def.clone());
        for _ in 0..3 {
            let mut v = def.clone();
            v.price_decimals = seq.between(2, 6) as u32;
            v.buy_spread_bps = seq.between(0, 300) as u32;
            v.sell_spread_bps = seq.between(0, 300) as u32;
            out.push(v);
        }
    }
    out
}

/// Purpose: for thousands of valuations across the shipped funds (and
/// variants of them at other decimals and spreads), the struck mid, buy and
/// sell equal an exact recalculation from the valuation's digits: mid is net
/// assets over units rounded half-even, buy is mid times one plus the buy
/// spread rounded up, and sell is mid times one minus the sell spread
/// rounded down. Every case also holds buy >= exact >= sell.
/// Why it matters: these are the prices every order in the fund executes
/// at; a price a fraction of a cent wrong on a large fund moves real money
/// between investors, and a fault in the decimal library or its use would
/// otherwise be invisible to tests that compute with the same library.
#[test]
fn struck_prices_equal_an_exact_recalculation() {
    let mut seq = Seq(0x5eed_0001);
    let mut cases = 0;
    for def in funds(&mut seq) {
        let terms = def.terms().unwrap();
        for _ in 0..400 {
            let cents = seq.between(1, 100_000_000_000_00);
            let units_micro = seq.between(1, 1_000_000_000_000_000) as i64;
            let nav = money(cents);
            let v = valuation(&def.id, &nav, units_micro, ("0", "0"), Some("0".to_string()));
            let struck = strike(&def, &terms, &v, None, Some(units_micro)).unwrap();

            let exact = Q::read(&nav).div(Q::new(units_micro as i128, 1_000_000));
            let p = def.price_decimals;
            let spread = |bps: u32| Q::new(bps as i128, 10_000);
            let buy = exact.mul(Q::int(1).add(spread(def.buy_spread_bps)));
            let sell = exact.mul(Q::int(1).sub(spread(def.sell_spread_bps)));
            let case = format!("{} nav {nav} units {units_micro} at {p} places", def.id.0);
            assert_eq!(struck.mid.as_str(), exact.fixed(p, Mode::HalfEven), "mid: {case}");
            assert_eq!(struck.buy.as_str(), buy.fixed(p, Mode::Up), "buy: {case}");
            assert_eq!(struck.sell.as_str(), sell.fixed(p, Mode::Down), "sell: {case}");
            assert!(buy.le(Q::read(struck.buy.as_str())), "buy below the exact buy: {case}");
            assert!(Q::read(struck.sell.as_str()).le(sell), "sell above the exact sell: {case}");
            cases += 1;
        }
    }
    assert!(cases >= 4_000, "{cases} cases");
}

/// Purpose: the previous move, benchmark and unexplained move checks pass
/// or fail exactly where an exact recalculation puts them, for moves and
/// flows spread on both sides of each tolerance; and a correction is
/// material exactly when the exact error size reaches the error threshold.
/// Why it matters: these decide whether a price needs a person's attention
/// and whether investors are compensated; a check that passes a move it
/// should hold lets a wrong price through without anyone seeing it.
#[test]
fn checks_and_materiality_equal_an_exact_recalculation() {
    let mut seq = Seq(0x5eed_0002);
    let mut cases = 0;
    // Passes and failures seen for each check, so both sides are reached.
    let mut seen: std::collections::BTreeMap<CheckKind, (u32, u32)> = Default::default();
    for def in funds(&mut seq) {
        let terms: PricingTerms = def.terms().unwrap();
        let p = def.price_decimals;
        for _ in 0..300 {
            // The previous price, struck the same way, then today's valuation
            // moved by up to twice each tolerance either way.
            let units_micro = seq.between(1_000_000_000, 1_000_000_000_000_000) as i64;
            let units = Q::new(units_micro as i128, 1_000_000);
            let prev_cents = seq.between(units_micro as i128 / 10_000 + 1, units_micro as i128 / 10);
            let prev_nav = money(prev_cents);
            let prev_mid = Q::read(&prev_nav).div(units).fixed(p, Mode::HalfEven);
            if Q::read(&prev_mid).top == 0 {
                continue;
            }
            let previous = Previous {
                price_id: format!("{}:2026-11-03.1", def.id.0),
                pricing_date: NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
                mid: DecStr::parse(&prev_mid).unwrap(),
                nav: DecStr::parse(&prev_nav).unwrap(),
            };
            let limit = Q::read(terms.previous_move.as_str());
            let swing = Q::read(&fraction(&mut seq, -1000, 1000)).mul(limit).mul(Q::int(20));
            let cents = Q::int(prev_cents).mul(Q::int(1).add(swing));
            let cents = cents.top / cents.bottom;
            let apps = seq.between(0, prev_cents / 100);
            let reds = seq.between(0, prev_cents / 100);
            let (nav, apps, reds) = (money(cents.max(1) + apps - reds), money(apps), money(reds));
            if Q::read(&nav).top <= 0 {
                continue;
            }
            // The benchmark near the fund's own move, either side of the gap.
            let near = swing.add(Q::read(&fraction(&mut seq, -200, 200)));
            let benchmark = near.fixed(6, Mode::HalfEven);
            let v = valuation(&def.id, &nav, units_micro, (&apps, &reds), Some(benchmark.clone()));
            let struck = strike(&def, &terms, &v, Some(&previous), Some(units_micro)).unwrap();
            let case = format!(
                "{} prev {prev_nav} nav {nav} units {units_micro} flows {apps}/{reds} benchmark {benchmark}",
                def.id.0
            );

            let mid = Q::read(struck.mid.as_str());
            let prev = Q::read(&prev_mid);
            let moved = mid.div(prev).sub(Q::int(1));
            assert_eq!(
                passed(&struck.checks, CheckKind::PreviousMove),
                Some(moved.abs().le(limit)),
                "previous move: {case}"
            );
            if let Some(gap) = &terms.benchmark_gap {
                let diff = moved.sub(Q::read(&benchmark));
                assert_eq!(
                    passed(&struck.checks, CheckKind::Benchmark),
                    Some(diff.abs().le(Q::read(gap.as_str()))),
                    "benchmark: {case}"
                );
            }
            let grown = Q::read(&prev_nav).mul(mid.div(prev));
            let expected = grown.add(Q::read(&apps)).sub(Q::read(&reds));
            let unexplained = Q::read(&nav).sub(expected).div(Q::read(&prev_nav));
            let tolerance = Q::read(terms.unexplained_move.as_str());
            assert_eq!(
                passed(&struck.checks, CheckKind::UnexplainedMove),
                Some(unexplained.abs().le(tolerance)),
                "unexplained move: {case}"
            );

            // Materiality: today's mid as the wrong price, the previous as the right one.
            let size = mid.sub(prev).div(prev).abs();
            let threshold = Q::read(terms.error_threshold.as_str());
            let module_material = error_size(&struck.mid, &previous.mid) >= terms.error_threshold.get();
            assert_eq!(module_material, threshold.le(size), "materiality: {case}");

            for c in &struck.checks {
                let e = seen.entry(c.kind).or_default();
                if c.passed {
                    e.0 += 1;
                } else {
                    e.1 += 1;
                }
            }
            cases += 1;
        }
    }
    assert!(cases >= 3_000, "{cases} cases");
    // The swings straddle the tolerances, so both outcomes are exercised.
    for kind in [CheckKind::PreviousMove, CheckKind::Benchmark, CheckKind::UnexplainedMove] {
        let (pass, fail) = seen.get(&kind).copied().unwrap_or_default();
        assert!(pass > cases / 20 && fail > cases / 20, "{kind:?}: passed {pass}, failed {fail} of {cases}");
    }
}

/// Purpose: the recalculation's own rounding agrees with hand-worked
/// figures, including the E2E-232 cases and every tie and sign.
/// Why it matters: the recalculation is the reference; if its rounding were
/// wrong in the same way as the module's, the comparison would prove
/// nothing.
#[test]
fn the_reference_rounding_is_right() {
    let r = |v: &str, mode| Q::read(v).fixed(4, mode);
    assert_eq!(r("1.00490680", Mode::Up), "1.0050");
    assert_eq!(r("1.00170092", Mode::Up), "1.0018");
    assert_eq!(r("1.00490000", Mode::Up), "1.0049");
    assert_eq!(r("1.00499999", Mode::Down), "1.0049");
    assert_eq!(r("1.00125000", Mode::HalfEven), "1.0012");
    assert_eq!(r("1.00135000", Mode::HalfEven), "1.0014");
    assert_eq!(r("1.00125001", Mode::HalfEven), "1.0013");
    assert_eq!(r("-1.00490680", Mode::Up), "-1.0050");
    assert_eq!(r("-1.00490680", Mode::Down), "-1.0049");
    assert_eq!(Q::new(1, 3).fixed(6, Mode::HalfEven), "0.333333");
    assert_eq!(Q::new(2, 3).fixed(6, Mode::HalfEven), "0.666667");
    assert_eq!(
        Q::read("1002400.80").div(Q::int(1_000_000)).mul(Q::new(10025, 10000)).fixed(4, Mode::Up),
        "1.0050"
    );
}
