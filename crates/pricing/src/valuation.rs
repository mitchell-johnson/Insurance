//! The fund administrator's valuation: what a price is struck from.
//!
//! The administrator delivers one file per run, holding a valuation for each
//! fund it priced and the assets whose prices were not struck on the pricing
//! date. The file is plain comma-separated text with a record type first:
//!
//! ```text
//! H,<administrator>,<file id>
//! V,<fund>,<pricing date>,<net assets>,<units on issue>,<applications>,<redemptions>,<benchmark return>,<restated Y|N>
//! A,<fund>,<pricing date>,<asset>,<priced on>
//! T,<number of V records>,<number of A records>
//! ```
//!
//! Amounts are in the fund's currency in major units, units have at most six
//! decimal places, dates are `YYYY-MM-DD`, and the benchmark return is a
//! fraction or empty for a fund with no benchmark. The trailer counts the
//! records, so a file cut short in transit is refused whole rather than
//! read in part.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_money::{Dec, DecStr, FundId, MICRO_PER_UNIT};
use serde::{Deserialize, Serialize};

/// The largest file accepted.
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// An asset in the fund whose price is older than the pricing date.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetPrice {
    pub asset: String,
    pub priced_on: NaiveDate,
}

/// One fund's valuation for one pricing date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Valuation {
    pub fund: FundId,
    pub pricing_date: NaiveDate,
    /// Net asset value, in major units of the fund's currency.
    pub nav: DecStr,
    /// The administrator's units on issue at the pricing date's cut-off.
    pub units_on_issue_micro: i64,
    /// The day's applications and redemptions as the administrator has them.
    pub applications: DecStr,
    pub redemptions: DecStr,
    #[serde(default)]
    pub benchmark_return: Option<DecStr>,
    /// The administrator says this replaces a valuation already sent.
    #[serde(default)]
    pub restated: bool,
    #[serde(default)]
    pub stale_assets: Vec<AssetPrice>,
}

/// A whole file from the administrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValuationFile {
    pub administrator: String,
    pub file_id: String,
    pub valuations: Vec<Valuation>,
}

fn plain(field: &str, what: &str) -> Result<String, String> {
    let ok = !field.is_empty() && field.len() <= 64 && field.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if ok {
        Ok(field.to_string())
    } else {
        Err(format!("{what} {field:?} is not 1 to 64 letters, digits, hyphens, underscores or dots"))
    }
}

fn date(field: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(field, "%Y-%m-%d").map_err(|_| format!("{field:?} is not a date (YYYY-MM-DD)"))
}

fn amount(field: &str, what: &str) -> Result<DecStr, String> {
    let d = DecStr::parse(field).map_err(|_| format!("{what} {field:?} is not a decimal"))?;
    if d.get() < Dec::ZERO {
        return Err(format!("{what} {field} is negative"));
    }
    Ok(d)
}

/// Units in decimal form, at most six places, as micro-units.
pub fn units_micro(field: &str) -> Result<i64, String> {
    let (whole, frac) = field.split_once('.').unwrap_or((field, ""));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || !digits(frac) || frac.len() > 6 {
        return Err(format!("units {field:?} are not a positive number with at most six decimal places"));
    }
    let whole: i64 = whole.parse().map_err(|_| format!("units {field} are too large"))?;
    let frac: i64 = format!("{frac:0<6}").parse().unwrap_or(0);
    whole
        .checked_mul(MICRO_PER_UNIT)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| format!("units {field} are too large"))
}

/// Read an administrator's file. Any fault refuses the whole file, naming
/// the line.
pub fn parse(bytes: &[u8]) -> Result<ValuationFile, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!("the file is larger than {MAX_FILE_BYTES} bytes"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "the file is not UTF-8 text".to_string())?;
    let mut header: Option<(String, String)> = None;
    let mut valuations: BTreeMap<(FundId, NaiveDate), Valuation> = BTreeMap::new();
    let mut assets: Vec<(usize, FundId, NaiveDate, AssetPrice)> = Vec::new();
    let mut trailer: Option<(usize, usize)> = None;
    let mut asset_count = 0usize;
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let at = |e: String| format!("line {n}: {e}");
        if trailer.is_some() {
            return Err(at("a record after the trailer".to_string()));
        }
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        match (f[0], header.is_some()) {
            ("H", false) if f.len() == 3 => header = Some((plain(f[1], "administrator").map_err(at)?, plain(f[2], "file id").map_err(at)?)),
            ("H", true) => return Err(at("a second header".to_string())),
            (_, false) => return Err(at("the file must start with a header (H)".to_string())),
            ("V", true) if f.len() == 9 => {
                let v = Valuation {
                    fund: FundId(plain(f[1], "fund").map_err(at)?),
                    pricing_date: date(f[2]).map_err(at)?,
                    nav: amount(f[3], "net assets").map_err(at)?,
                    units_on_issue_micro: units_micro(f[4]).map_err(at)?,
                    applications: amount(f[5], "applications").map_err(at)?,
                    redemptions: amount(f[6], "redemptions").map_err(at)?,
                    benchmark_return: match f[7] {
                        "" => None,
                        r => Some(DecStr::parse(r).map_err(|_| at(format!("benchmark return {r:?} is not a decimal")))?),
                    },
                    restated: match f[8] {
                        "Y" => true,
                        "N" => false,
                        other => return Err(at(format!("restated is Y or N, not {other:?}"))),
                    },
                    stale_assets: Vec::new(),
                };
                let key = (v.fund.clone(), v.pricing_date);
                if valuations.insert(key, v).is_some() {
                    return Err(at("a second valuation for the same fund and date".to_string()));
                }
            }
            ("A", true) if f.len() == 5 => {
                asset_count += 1;
                let fund = FundId(plain(f[1], "fund").map_err(at)?);
                let pricing_date = date(f[2]).map_err(at)?;
                let asset = AssetPrice {
                    asset: plain(f[3], "asset").map_err(at)?,
                    priced_on: date(f[4]).map_err(at)?,
                };
                if asset.priced_on > pricing_date {
                    return Err(at("an asset priced after the pricing date".to_string()));
                }
                assets.push((n, fund, pricing_date, asset));
            }
            ("T", true) if f.len() == 3 => {
                let count = |s: &str| s.parse::<usize>().map_err(|_| at(format!("{s:?} is not a count")));
                trailer = Some((count(f[1])?, count(f[2])?));
            }
            (kind, true) => return Err(at(format!("record {kind:?} with {} fields is not one this file may hold", f.len()))),
        }
    }
    let (administrator, file_id) = header.ok_or("the file is empty")?;
    let (v_count, a_count) = trailer.ok_or("the file has no trailer (T): it may have been cut short")?;
    if v_count != valuations.len() || a_count != asset_count {
        return Err(format!(
            "the trailer counts {v_count} valuations and {a_count} assets; the file holds {} and {asset_count}",
            valuations.len()
        ));
    }
    for (n, fund, pricing_date, asset) in assets {
        let v = valuations
            .get_mut(&(fund, pricing_date))
            .ok_or_else(|| format!("line {n}: an asset for a fund and date with no valuation in the file"))?;
        v.stale_assets.push(asset);
    }
    let mut valuations: Vec<Valuation> = valuations.into_values().collect();
    for v in &mut valuations {
        v.stale_assets.sort();
    }
    if valuations.is_empty() {
        return Err("the file holds no valuation".to_string());
    }
    Ok(ValuationFile {
        administrator,
        file_id,
        valuations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "H,acme-admin,VAL-20261112-01\n\
        V,growth,2026-11-12,2010000.00,1000000.5,1000.00,500.00,0.0012,N\n\
        V,cash,2026-11-12,500000.00,500000,0,0,,Y\n\
        A,growth,2026-11-12,NZ-BOND-7,2026-11-09\n\
        \n\
        T,2,1\n";

    /// Purpose: a well-formed file is read into one valuation per fund, with
    /// units in micro-units, the benchmark optional and stale assets attached.
    /// Why it matters: this is the only way a valuation enters the platform;
    /// every figure a price is struck from passes through here.
    #[test]
    fn reads_a_good_file() {
        let file = parse(GOOD.as_bytes()).unwrap();
        assert_eq!(file.administrator, "acme-admin");
        assert_eq!(file.file_id, "VAL-20261112-01");
        assert_eq!(file.valuations.len(), 2);
        let cash = &file.valuations[0];
        assert_eq!(cash.fund.0, "cash");
        assert!(cash.restated);
        assert_eq!(cash.benchmark_return, None);
        let growth = &file.valuations[1];
        assert_eq!(growth.units_on_issue_micro, 1_000_000_500_000);
        assert_eq!(growth.benchmark_return.as_ref().map(DecStr::as_str), Some("0.0012"));
        assert_eq!(growth.stale_assets[0].asset, "NZ-BOND-7");
    }

    /// Purpose: each kind of fault refuses the whole file with the line and the reason.
    /// Why it matters: a price struck from a half-read file is a price error
    /// waiting to happen; refusing the file whole sends it back to the
    /// administrator before anyone prepares a price from it.
    #[test]
    fn refuses_faulty_files() {
        let cases = [
            ("", "empty"),
            ("V,growth,2026-11-12,1,1,0,0,,N\n", "start with a header"),
            ("H,acme,F1\nH,acme,F2\n", "second header"),
            ("H,ac me,F1\n", "administrator"),
            ("H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,N\n", "no trailer"),
            ("H,acme,F1\nV,growth,2026-13-12,1,1,0,0,,N\nT,1,0\n", "not a date"),
            ("H,acme,F1\nV,growth,2026-11-12,-1,1,0,0,,N\nT,1,0\n", "negative"),
            ("H,acme,F1\nV,growth,2026-11-12,x,1,0,0,,N\nT,1,0\n", "not a decimal"),
            ("H,acme,F1\nV,growth,2026-11-12,1,1.1234567,0,0,,N\nT,1,0\n", "six decimal"),
            ("H,acme,F1\nV,growth,2026-11-12,1,99999999999999,0,0,,N\nT,1,0\n", "too large"),
            ("H,acme,F1\nV,growth,2026-11-12,1,1,0,0,abc,N\nT,1,0\n", "benchmark return"),
            ("H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,X\nT,1,0\n", "Y or N"),
            (
                "H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,N\nV,growth,2026-11-12,1,1,0,0,,N\nT,2,0\n",
                "second valuation",
            ),
            (
                "H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,N\nA,growth,2026-11-12,B1,2026-11-13\nT,1,1\n",
                "after the pricing date",
            ),
            (
                "H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,N\nA,cash,2026-11-12,B1,2026-11-10\nT,1,1\n",
                "no valuation in the file",
            ),
            ("H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,N\nT,2,0\n", "trailer counts"),
            ("H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,N\nT,x,0\n", "not a count"),
            (
                "H,acme,F1\nV,growth,2026-11-12,1,1,0,0,,N\nT,1,0\nV,cash,2026-11-12,1,1,0,0,,N\n",
                "after the trailer",
            ),
            ("H,acme,F1\nZ,1\nT,0,0\n", "not one this file may hold"),
            ("H,acme,F1\nT,0,0\n", "no valuation"),
        ];
        for (text, expect) in cases {
            let err = parse(text.as_bytes()).err().unwrap_or_default();
            assert!(err.contains(expect), "{expect}: {err}");
        }
        assert!(parse(&[0xff, 0xfe]).unwrap_err().contains("UTF-8"));
        assert!(parse(&vec![b' '; MAX_FILE_BYTES + 1]).unwrap_err().contains("larger"));
    }

    /// Purpose: units convert to micro-units exactly, padding short fractions.
    /// Why it matters: units on issue must match the core to the micro-unit,
    /// so the conversion cannot go through floating point or lose a digit.
    #[test]
    fn units_convert_exactly() {
        assert_eq!(units_micro("1"), Ok(1_000_000));
        assert_eq!(units_micro("1.5"), Ok(1_500_000));
        assert_eq!(units_micro("0.000001"), Ok(1));
        assert!(units_micro(".5").is_err());
        assert!(units_micro("1e3").is_err());
    }
}
