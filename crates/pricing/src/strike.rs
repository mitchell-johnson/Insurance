//! Striking a price from a valuation, and the checks it must pass.
//!
//! The mid price is net assets divided by units on issue. The buy price adds
//! the fund's buy spread and is rounded up; the sell price takes off the sell
//! spread and is rounded down; the mid is rounded half-even. Rounding the
//! buy and sell prices away from the investor means rounding never dilutes
//! the fund.
//!
//! Before anyone may approve a price, it is checked:
//!
//! - **Units on issue**: the core's units, summed over every partition at the
//!   pricing date's cut-off, equal the administrator's. A difference blocks
//!   the price outright; it cannot be accepted, only fixed.
//! - **Previous move**: the mid moved no more than the tolerance from the
//!   previous price.
//! - **Benchmark**: the price's move is within the tolerance of the
//!   benchmark's return (only for a fund with a benchmark).
//! - **Stale assets**: no asset in the valuation was priced longer ago than
//!   the tolerance allows.
//! - **Unexplained move**: net assets equal the previous net assets grown by
//!   the price move plus the day's applications less redemptions, within the
//!   tolerance. A gap means flows the administrator has that the price does
//!   not reflect, or the other way round.
//!
//! Any of the last four that fails holds the price for review: the approver
//! must accept each failure with a reason, on the record.

use chrono::NaiveDate;
use ic_money::{dec_i128, Dec, DecStr, FastRounding, MICRO_PER_UNIT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fund::{FundDefinition, PricingTerms};
use crate::valuation::Valuation;

/// A check a price is put through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    UnitsOnIssue,
    PreviousMove,
    Benchmark,
    StaleAssets,
    UnexplainedMove,
}

impl CheckKind {
    /// A blocking check cannot be accepted by an approver.
    pub fn blocking(self) -> bool {
        self == CheckKind::UnitsOnIssue
    }
}

/// The outcome of one check, with the figures behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub kind: CheckKind,
    pub passed: bool,
    pub detail: String,
}

/// The price a valuation was compared with: the fund's latest active price
/// before this pricing date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Previous {
    pub price_id: String,
    pub pricing_date: NaiveDate,
    pub mid: DecStr,
    pub nav: DecStr,
}

/// A struck price, before approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Struck {
    pub mid: DecStr,
    pub buy: DecStr,
    pub sell: DecStr,
    pub checks: Vec<Check>,
    /// SHA-256 over everything the price was struck from, so every replica
    /// and every later reader can tell exactly which inputs produced it.
    pub inputs_hash: String,
}

impl Struck {
    /// Whether the units on issue control failed.
    pub fn blocked(&self) -> bool {
        self.checks.iter().any(|c| c.kind.blocking() && !c.passed)
    }

    /// The checks that failed and may be accepted with a reason.
    pub fn held(&self) -> Vec<CheckKind> {
        self.checks.iter().filter(|c| !c.passed && !c.kind.blocking()).map(|c| c.kind).collect()
    }
}

fn pow10(n: u32) -> Dec {
    dec_i128(10i128.pow(n))
}

/// `value` rounded to `places` decimals by `mode`, written with exactly that
/// many decimals, so the same price always has the same string.
pub fn fixed(value: Dec, places: u32, mode: FastRounding) -> Result<DecStr, String> {
    if value.is_nan() || value.is_infinite() {
        return Err("the price is not a number".to_string());
    }
    // Decide from the whole remainder, not the first dropped digit:
    // fastnum's `round(0)` looks only at that digit, so 1.00490680 rounded
    // up to four places came out 1.0049.
    let scaled = value * pow10(places);
    let whole = scaled.trunc();
    let rest = (scaled - whole).abs();
    let toward = |n: i128| if scaled.is_sign_negative() { n - 1 } else { n + 1 };
    let n = whole.to_i128().map_err(|_| "the price is out of range".to_string())?;
    let half = Dec::ONE / Dec::TWO;
    let away = match mode {
        _ if rest.is_zero() => false,
        FastRounding::Up => true,
        FastRounding::Down => false,
        FastRounding::HalfEven => rest > half || (rest == half && n % 2 != 0),
        other => return Err(format!("rounding {other:?} is not used for prices")),
    };
    let n = if away { toward(n) } else { n };
    let scale = 10i128.pow(places);
    let sign = if n < 0 { "-" } else { "" };
    let a = n.unsigned_abs();
    let text = format!("{sign}{}.{:0width$}", a / scale as u128, a % scale as u128, width = places as usize);
    DecStr::parse(&text).map_err(|e| e.to_string())
}

fn show(d: Dec) -> String {
    fixed(d, 6, FastRounding::HalfEven).map_or_else(|e| e, |s| s.as_str().to_string())
}

fn within(measured: Dec, limit: &DecStr) -> bool {
    measured.abs() <= limit.get()
}

/// The hash of a price's inputs. Every field that could change the price or
/// its checks is in it.
pub fn inputs_hash(def: &FundDefinition, valuation: &Valuation, previous: Option<&Previous>, core_units_micro: Option<i64>) -> String {
    let inputs = serde_json::json!({
        "fund": def.id,
        "definition_version": def.version,
        "spreads_bps": [def.buy_spread_bps, def.sell_spread_bps],
        "price_decimals": def.price_decimals,
        "pricing": def.pricing,
        "valuation": valuation,
        "previous": previous,
        "core_units_micro": core_units_micro,
    });
    hex::encode(Sha256::digest(inputs.to_string().as_bytes()))
}

/// Strike the price for `valuation` and run every check. `core_units_micro`
/// is the core's units on issue summed over every partition, or `None` while
/// some partition has not reported.
pub fn strike(
    def: &FundDefinition,
    terms: &PricingTerms,
    valuation: &Valuation,
    previous: Option<&Previous>,
    core_units_micro: Option<i64>,
) -> Result<Struck, String> {
    if valuation.fund != def.id {
        return Err(format!("the valuation is for {}, not {}", valuation.fund.0, def.id.0));
    }
    let units = dec_i128(valuation.units_on_issue_micro as i128) / dec_i128(MICRO_PER_UNIT as i128);
    let nav = valuation.nav.get();
    // A fund with no units prices at its last price, or at launch.
    let raw_mid = if valuation.units_on_issue_micro == 0 {
        previous.map_or_else(|| terms.launch_price.get(), |p| p.mid.get())
    } else {
        nav / units
    };
    if raw_mid <= Dec::ZERO {
        return Err("net assets give a price of zero or less".to_string());
    }
    let bps = |b: u32| dec_i128(b as i128) / dec_i128(10_000);
    let places = def.price_decimals;
    let mid = fixed(raw_mid, places, FastRounding::HalfEven)?;
    let buy = fixed(raw_mid * (Dec::ONE + bps(def.buy_spread_bps)), places, FastRounding::Up)?;
    let sell = fixed(raw_mid * (Dec::ONE - bps(def.sell_spread_bps)), places, FastRounding::Down)?;

    let mut checks = Vec::new();
    checks.push(match core_units_micro {
        None => Check {
            kind: CheckKind::UnitsOnIssue,
            passed: false,
            detail: "the core's units on issue have not been reported by every partition".to_string(),
        },
        Some(core) => Check {
            kind: CheckKind::UnitsOnIssue,
            passed: core == valuation.units_on_issue_micro,
            detail: format!(
                "core {core} micro-units, administrator {}, difference {}",
                valuation.units_on_issue_micro,
                valuation.units_on_issue_micro as i128 - core as i128
            ),
        },
    });

    let movement = previous.map(|p| mid.get() / p.mid.get() - Dec::ONE);
    checks.push(match movement {
        None => Check {
            kind: CheckKind::PreviousMove,
            passed: true,
            detail: "first price of the fund".to_string(),
        },
        Some(m) => Check {
            kind: CheckKind::PreviousMove,
            passed: within(m, &terms.previous_move),
            detail: format!("moved {} against a tolerance of {}", show(m), terms.previous_move.as_str()),
        },
    });

    if let Some(gap) = &terms.benchmark_gap {
        checks.push(match (movement, &valuation.benchmark_return) {
            (_, None) => Check {
                kind: CheckKind::Benchmark,
                passed: false,
                detail: "the fund has a benchmark but the valuation gives no benchmark return".to_string(),
            },
            (None, Some(_)) => Check {
                kind: CheckKind::Benchmark,
                passed: true,
                detail: "first price of the fund".to_string(),
            },
            (Some(m), Some(b)) => {
                let diff = m - b.get();
                Check {
                    kind: CheckKind::Benchmark,
                    passed: within(diff, gap),
                    detail: format!(
                        "moved {} against a benchmark of {}, gap {} against a tolerance of {}",
                        show(m),
                        b.as_str(),
                        show(diff),
                        gap.as_str()
                    ),
                }
            }
        });
    }

    let oldest = valuation.stale_assets.iter().map(|a| a.priced_on).min();
    let stale: Vec<&str> = valuation
        .stale_assets
        .iter()
        .filter(|a| (valuation.pricing_date - a.priced_on).num_days() > terms.stale_asset_days as i64)
        .map(|a| a.asset.as_str())
        .collect();
    checks.push(Check {
        kind: CheckKind::StaleAssets,
        passed: stale.is_empty(),
        detail: match (stale.is_empty(), oldest) {
            (true, None) => "every asset priced on the pricing date".to_string(),
            (true, Some(o)) => format!("oldest asset price {o}, within {} days", terms.stale_asset_days),
            (false, _) => format!("priced more than {} days ago: {}", terms.stale_asset_days, stale.join(", ")),
        },
    });

    checks.push(match previous {
        None => Check {
            kind: CheckKind::UnexplainedMove,
            passed: true,
            detail: "first price of the fund".to_string(),
        },
        Some(p) if p.nav.get() == Dec::ZERO => Check {
            kind: CheckKind::UnexplainedMove,
            passed: true,
            detail: "the fund held no assets at the previous price".to_string(),
        },
        Some(p) => {
            let grown = p.nav.get() * (mid.get() / p.mid.get());
            let expected = grown + valuation.applications.get() - valuation.redemptions.get();
            let unexplained = (nav - expected) / p.nav.get();
            Check {
                kind: CheckKind::UnexplainedMove,
                passed: within(unexplained, &terms.unexplained_move),
                detail: format!(
                    "net assets {} against {} expected from the price move and flows, unexplained {} against a tolerance of {}",
                    valuation.nav.as_str(),
                    show(expected),
                    show(unexplained),
                    terms.unexplained_move.as_str()
                ),
            }
        }
    });

    Ok(Struck {
        mid,
        buy,
        sell,
        checks,
        inputs_hash: inputs_hash(def, valuation, previous, core_units_micro),
    })
}

/// The size of a price error: how far the wrong mid is from the correct one,
/// as a fraction of the correct one.
pub fn error_size(wrong_mid: &DecStr, correct_mid: &DecStr) -> Dec {
    ((wrong_mid.get() - correct_mid.get()) / correct_mid.get()).abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fund::{sample, FundRules};
    use crate::valuation::AssetPrice;
    use ic_money::FundId;

    fn d(s: &str) -> DecStr {
        DecStr::parse(s).unwrap()
    }

    fn day(n: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, n).unwrap_or_default()
    }

    fn valuation(nav: &str, units: i64) -> Valuation {
        Valuation {
            fund: FundId("growth".into()),
            pricing_date: day(12),
            nav: d(nav),
            units_on_issue_micro: units,
            applications: d("0"),
            redemptions: d("0"),
            benchmark_return: Some(d("0.01")),
            restated: false,
            stale_assets: vec![],
        }
    }

    fn previous(mid: &str, nav: &str) -> Previous {
        Previous {
            price_id: "growth:2026-11-11.1".into(),
            pricing_date: day(11),
            mid: d(mid),
            nav: d(nav),
        }
    }

    fn check(s: &Struck, kind: CheckKind) -> &Check {
        s.checks.iter().find(|c| c.kind == kind).expect("the check ran")
    }

    /// Purpose: the worked example of the design (net assets over units, a
    /// 25 basis point spread each way) gives the mid, a buy rounded up and a
    /// sell rounded down, each to the fund's four decimals.
    /// Why it matters: these three numbers are what every order executes at;
    /// rounding the wrong way on either side moves value between investors.
    #[test]
    fn strikes_mid_buy_and_sell() {
        let def = sample("growth");
        let terms = def.terms().unwrap();
        // 2,010,000 / 1,000,000 units = 2.01; buy 2.015025 -> 2.0151; sell 2.004975 -> 2.0049.
        let mut v = valuation("2010000", 1_000_000_000_000);
        // An asset priced two days ago is within the three days allowed.
        v.stale_assets = vec![AssetPrice {
            asset: "NZ-BOND-7".into(),
            priced_on: day(10),
        }];
        let s = strike(&def, &terms, &v, Some(&previous("1.9900", "1990000")), Some(1_000_000_000_000)).unwrap();
        assert_eq!((s.mid.as_str(), s.buy.as_str(), s.sell.as_str()), ("2.0100", "2.0151", "2.0049"));
        assert!(check(&s, CheckKind::StaleAssets).detail.contains("2026-11-10"));
        assert!(s.checks.iter().all(|c| c.passed), "{:?}", s.checks);
        assert!(!s.blocked());
        assert!(s.held().is_empty());
        assert_eq!(s.inputs_hash.len(), 64);
    }

    /// Purpose: a difference between the core's and the administrator's units
    /// on issue, or a partition not yet reported, blocks the price.
    /// Why it matters: a price struck over the wrong number of units is wrong
    /// for every investor in the fund; the design makes this the one check
    /// nobody can accept.
    #[test]
    fn units_on_issue_difference_blocks() {
        let def = sample("growth");
        let terms = def.terms().unwrap();
        let v = valuation("2010000", 1_000_000_000_000);
        let p = previous("1.9900", "1990000");
        let off = strike(&def, &terms, &v, Some(&p), Some(999_999_000_000)).unwrap();
        assert!(off.blocked());
        assert!(check(&off, CheckKind::UnitsOnIssue).detail.contains("difference 1000000"));
        assert!(off.held().is_empty(), "a blocking check is not one an approver accepts");
        let missing = strike(&def, &terms, &v, Some(&p), None).unwrap();
        assert!(missing.blocked());
        assert!(check(&missing, CheckKind::UnitsOnIssue).detail.contains("every partition"));
    }

    /// Purpose: a large move against the previous price, a move far from the
    /// benchmark, a stale asset and net assets the flows do not explain each
    /// hold the price, naming the figures.
    /// Why it matters: these are the design's checks as code; each one is a
    /// way a wrong valuation has reached investors in practice.
    #[test]
    fn failed_checks_hold_the_price() {
        let def = sample("growth");
        let terms = def.terms().unwrap();
        // 10% up against a 5% tolerance and a 1% benchmark.
        let mut v = valuation("2189000", 1_000_000_000_000);
        v.stale_assets = vec![
            AssetPrice {
                asset: "OLD".into(),
                priced_on: day(5),
            },
            AssetPrice {
                asset: "FINE".into(),
                priced_on: day(10),
            },
        ];
        // The administrator reports applications the units do not reflect.
        v.applications = d("50000");
        let s = strike(&def, &terms, &v, Some(&previous("1.9900", "1990000")), Some(1_000_000_000_000)).unwrap();
        assert!(!s.blocked());
        assert_eq!(
            s.held(),
            vec![
                CheckKind::PreviousMove,
                CheckKind::Benchmark,
                CheckKind::StaleAssets,
                CheckKind::UnexplainedMove
            ]
        );
        assert!(check(&s, CheckKind::StaleAssets).detail.contains("OLD"));
        assert!(!check(&s, CheckKind::StaleAssets).detail.contains("FINE"));
        assert!(check(&s, CheckKind::PreviousMove).detail.contains("0.100000"));
    }

    /// Purpose: the first price of a fund passes the comparisons it has
    /// nothing to compare with, a fund with no units prices at launch, and a
    /// benchmark fund whose valuation omits the return is held.
    /// Why it matters: launch day and an empty fund are real cases; neither
    /// may divide by zero or pass a benchmark check it never ran.
    #[test]
    fn first_price_and_empty_fund() {
        let def = sample("growth");
        let terms = def.terms().unwrap();
        let mut v = valuation("0", 0);
        let s = strike(&def, &terms, &v, None, Some(0)).unwrap();
        assert_eq!(s.mid.as_str(), "1.0000");
        assert!(s.checks.iter().all(|c| c.passed));
        v.benchmark_return = None;
        let s = strike(&def, &terms, &v, None, Some(0)).unwrap();
        assert_eq!(s.held(), vec![CheckKind::Benchmark]);
        // Empty after having units: last price, and no assets to grow.
        let s = strike(&def, &terms, &valuation("0", 0), Some(&previous("1.5000", "0")), Some(0)).unwrap();
        assert_eq!(s.mid.as_str(), "1.5000");
        assert!(check(&s, CheckKind::UnexplainedMove).detail.contains("no assets"));
        // A fund without a benchmark does not run the check.
        let mut plain = sample("growth");
        plain.pricing.remove("benchmark_gap");
        let pt = plain.terms().unwrap();
        let s = strike(&plain, &pt, &valuation("100", 100_000_000), None, Some(100_000_000)).unwrap();
        assert!(s.checks.iter().all(|c| c.kind != CheckKind::Benchmark));
    }

    /// Purpose: a valuation for another fund, and one that would give a price
    /// of zero, are refused rather than struck.
    /// Why it matters: a price of zero would hand out unlimited units.
    #[test]
    fn refuses_impossible_prices() {
        let def = sample("cash");
        let terms = def.terms().unwrap();
        assert!(strike(&def, &terms, &valuation("1", 1), None, None).unwrap_err().contains("not cash"));
        let def = sample("growth");
        assert!(strike(&def, &terms, &valuation("0", 1_000_000), None, None).unwrap_err().contains("zero"));
        assert!(fixed(Dec::NAN, 2, FastRounding::Up).is_err());
    }

    /// Purpose: rounding looks at every dropped digit, not only the first:
    /// up rounds any remainder up, down drops it, and half-even rounds a
    /// remainder above a half up even when the first dropped digit is 5 or 0.
    /// The cases are the persona suite's (E2E-232).
    /// Why it matters: the buy price must never be below the exact one, or
    /// a buyer pays less than the units are worth and the fund's other
    /// investors carry the difference.
    #[test]
    fn rounds_on_the_whole_remainder() {
        let f = |v: &str, mode| fixed(DecStr::parse(v).unwrap().get(), 4, mode).unwrap().as_str().to_string();
        assert_eq!(f("1.00490680", FastRounding::Up), "1.0050");
        assert_eq!(f("1.00170092", FastRounding::Up), "1.0018");
        assert_eq!(f("1.00490000", FastRounding::Up), "1.0049");
        assert_eq!(f("1.00499999", FastRounding::Down), "1.0049");
        assert_eq!(f("1.00125001", FastRounding::HalfEven), "1.0013");
        assert_eq!(f("1.00125000", FastRounding::HalfEven), "1.0012");
        assert_eq!(f("1.00135000", FastRounding::HalfEven), "1.0014");
        assert_eq!(f("1.00124999", FastRounding::HalfEven), "1.0012");
        assert_eq!(f("-1.00490680", FastRounding::Up), "-1.0050");
        assert!(fixed(DecStr::parse("1.00001").unwrap().get(), 4, FastRounding::Floor).is_err());
        // The reported fund: 1,002,400.80 over 1,000,000 units, 25 bps.
        let def = sample("growth");
        let struck = strike(&def, &def.terms().unwrap(), &valuation("1002400.80", 1_000_000 * MICRO_PER_UNIT), None, None).unwrap();
        assert_eq!(struck.buy.as_str(), "1.0050");
    }

    /// Purpose: the inputs hash changes when any input changes and is stable otherwise.
    /// Why it matters: the hash travels with the price to every partition;
    /// it is how an auditor proves which valuation a price came from.
    #[test]
    fn inputs_hash_covers_every_input() {
        let def = sample("growth");
        let v = valuation("2010000", 1_000_000_000_000);
        let p = previous("1.9900", "1990000");
        let base = inputs_hash(&def, &v, Some(&p), Some(1));
        assert_eq!(base, inputs_hash(&def, &v, Some(&p), Some(1)));
        assert_ne!(base, inputs_hash(&def, &v, Some(&p), Some(2)));
        assert_ne!(base, inputs_hash(&def, &v, None, Some(1)));
        assert_ne!(base, inputs_hash(&def, &valuation("2010001", 1_000_000_000_000), Some(&p), Some(1)));
        let mut v2 = def.clone();
        v2.buy_spread_bps = 30;
        assert_ne!(base, inputs_hash(&v2, &v, Some(&p), Some(1)));
    }

    /// Purpose: an error's size is the distance from the correct mid as a fraction of it.
    /// Why it matters: materiality is judged on this figure, and so is whether
    /// investors are compensated.
    #[test]
    fn error_size_is_relative_to_the_correct_price() {
        assert_eq!(show(error_size(&d("2.0200"), &d("2.0000"))), "0.010000");
        assert_eq!(show(error_size(&d("1.9800"), &d("2.0000"))), "0.010000");
    }
}
