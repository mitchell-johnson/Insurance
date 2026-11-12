//! Funds as code: each fund is a versioned definition, and its working-day
//! calendar decides which dates it prices on.
//!
//! The definition is the core's (`ic_core::fund::FundDefinition`), activated
//! by `config` on every partition like a product. The core reads the fields
//! it needs to take and execute orders; this module reads the same file for
//! the spread, the price decimals and the calendar, and keeps its own terms
//! (tolerances, materiality, lag, launch price) in the definition's string
//! map `pricing`, which the core does not read. [`FundDefinition`] here is
//! this module's reading of that file, and [`PricingTerms`] the parsed map.
//!
//! A definition is immutable once active. A change is a new version that
//! takes effect from a date; the fund's currency, and so its unit type, is
//! frozen at the first version.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, Utc, Weekday};
use ic_money::{Currency, Dec, DecStr, FundId, RoundingMode};
use serde::{Deserialize, Serialize};

/// The most decimal places a price may carry.
pub const MAX_PRICE_DECIMALS: u32 = 8;
/// The widest spread either way, in basis points.
pub const MAX_SPREAD_BPS: u32 = 1_000;
/// The furthest ahead the calendar is searched for a pricing day. A fund with
/// no pricing day in a year of dates is a definition error, not a long wait.
pub const CALENDAR_HORIZON_DAYS: u64 = 366;

/// One version of a fund, as this module reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundDefinition {
    pub fund: FundId,
    pub version: u32,
    pub name: String,
    pub currency: Currency,
    /// The first date this version applies to.
    pub effective_from: NaiveDate,
    /// The days of the week the fund prices on, before non-pricing days.
    pub pricing_weekdays: Vec<Weekday>,
    /// Dates the fund does not price, such as overseas market holidays.
    #[serde(default)]
    pub non_pricing_days: BTreeSet<NaiveDate>,
    /// Orders logged at or after this local time take the next pricing date.
    pub cutoff: NaiveTime,
    /// The IANA time zone the cut-off is read in, such as `Pacific/Auckland`.
    pub time_zone: String,
    /// Added to the mid price for buying units.
    pub buy_spread_bps: u32,
    /// Taken off the mid price for selling units.
    pub sell_spread_bps: u32,
    pub price_decimals: u32,
    /// How the core rounds units to micro-units when it executes an order.
    pub unit_rounding: RoundingMode,
    pub open_to_new_money: bool,
    /// This module's terms, as strings; see [`PricingTerms`].
    #[serde(default)]
    pub pricing: BTreeMap<String, String>,
}

/// The limits a price is checked against and the rules for errors, read from
/// a definition's `pricing` map. Fractions are decimal strings (`"0.05"` is
/// five per cent). They are actuarial inputs, owned by the appointed actuary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingTerms {
    /// `previous_move`: the largest move of the mid price against the previous price.
    pub previous_move: DecStr,
    /// `benchmark_gap`: the largest gap between the price's move and the
    /// benchmark's return. Absent for a fund with no benchmark, and the
    /// check is then not run.
    pub benchmark_gap: Option<DecStr>,
    /// `stale_asset_days`: asset prices older than this many calendar days are stale.
    pub stale_asset_days: u32,
    /// `unexplained_move`: the largest movement in net assets, as a fraction
    /// of the previous net assets, that the day's flows and the price move
    /// do not explain.
    pub unexplained_move: DecStr,
    /// `error_threshold`: the materiality of a price error, as a fraction of
    /// the correct price. A smaller error is recorded and corrected, but the
    /// orders at the wrong price are not replayed.
    pub error_threshold: DecStr,
    /// `launch_price`: the price of the first units, used until the fund has units.
    pub launch_price: DecStr,
    /// `pricing_lag_days`: pricing days after a pricing date by which its
    /// price is expected. A price later than this is late.
    pub pricing_lag_days: u32,
}

fn fraction(map: &BTreeMap<String, String>, key: &str, max: i64) -> Result<DecStr, String> {
    let text = map.get(key).ok_or_else(|| format!("pricing.{key} is missing"))?;
    let value = DecStr::parse(text).map_err(|_| format!("pricing.{key} is not a decimal: {text:?}"))?;
    if value.get() < Dec::ZERO || value.get() > Dec::from(max) {
        return Err(format!("pricing.{key} must be between 0 and {max}, not {text}"));
    }
    Ok(value)
}

fn whole(map: &BTreeMap<String, String>, key: &str, range: std::ops::RangeInclusive<u32>) -> Result<u32, String> {
    let text = map.get(key).ok_or_else(|| format!("pricing.{key} is missing"))?;
    text.parse::<u32>()
        .ok()
        .filter(|n| range.contains(n))
        .ok_or_else(|| format!("pricing.{key} must be a whole number from {} to {}, not {text:?}", range.start(), range.end()))
}

impl PricingTerms {
    /// Read the terms from a definition's `pricing` map. Unknown keys are
    /// refused, so a misspelt tolerance is not silently ignored.
    pub fn from_map(map: &BTreeMap<String, String>) -> Result<Self, String> {
        const KEYS: [&str; 7] = [
            "previous_move",
            "benchmark_gap",
            "stale_asset_days",
            "unexplained_move",
            "error_threshold",
            "launch_price",
            "pricing_lag_days",
        ];
        if let Some(unknown) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("pricing.{unknown} is not a term of the unit pricing module"));
        }
        let terms = PricingTerms {
            previous_move: fraction(map, "previous_move", 1)?,
            benchmark_gap: map.contains_key("benchmark_gap").then(|| fraction(map, "benchmark_gap", 1)).transpose()?,
            stale_asset_days: whole(map, "stale_asset_days", 1..=30)?,
            unexplained_move: fraction(map, "unexplained_move", 1)?,
            error_threshold: fraction(map, "error_threshold", 1)?,
            launch_price: fraction(map, "launch_price", 1_000_000)?,
            pricing_lag_days: whole(map, "pricing_lag_days", 1..=30)?,
        };
        if terms.launch_price.get() <= Dec::ZERO {
            return Err("pricing.launch_price must be above zero".to_string());
        }
        Ok(terms)
    }
}

impl FundDefinition {
    /// Check a definition on its own and read its terms. Whether it may
    /// follow the versions already active is [`FundDefinition::may_follow`].
    pub fn validate(&self) -> Result<PricingTerms, String> {
        let plain = |s: &str| !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !plain(&self.fund.0) {
            return Err("a fund ID is 1 to 32 letters, digits, hyphens or underscores".to_string());
        }
        if self.version == 0 {
            return Err("versions start at 1".to_string());
        }
        if self.name.trim().is_empty() {
            return Err("a fund needs a name".to_string());
        }
        if self.pricing_weekdays.is_empty() {
            return Err("a fund must price on at least one day of the week".to_string());
        }
        if self.time_zone.parse::<chrono_tz::Tz>().is_err() {
            return Err(format!("unknown time zone {}", self.time_zone));
        }
        if self.buy_spread_bps > MAX_SPREAD_BPS || self.sell_spread_bps > MAX_SPREAD_BPS {
            return Err(format!("a spread is at most {MAX_SPREAD_BPS} basis points"));
        }
        if self.price_decimals == 0 || self.price_decimals > MAX_PRICE_DECIMALS {
            return Err(format!("price_decimals must be 1 to {MAX_PRICE_DECIMALS}"));
        }
        let terms = PricingTerms::from_map(&self.pricing)?;
        // A calendar with no pricing day ahead would leave every order pending.
        self.next_pricing_day(self.effective_from)?;
        Ok(terms)
    }

    /// Whether this version may follow `previous`, the latest active one.
    pub fn may_follow(&self, previous: &FundDefinition) -> Result<(), String> {
        if self.version != previous.version + 1 {
            return Err(format!("the next version of {} is {}", self.fund.0, previous.version + 1));
        }
        if self.currency != previous.currency {
            return Err("a fund's currency, and so its unit type, is frozen once in use".to_string());
        }
        if self.effective_from < previous.effective_from {
            return Err(format!("version {} cannot take effect before version {}", self.version, previous.version));
        }
        Ok(())
    }

    fn tz(&self) -> chrono_tz::Tz {
        // Validated before the definition is active.
        self.time_zone.parse().unwrap_or(chrono_tz::UTC)
    }

    /// Whether the fund prices on `date`.
    pub fn is_pricing_day(&self, date: NaiveDate) -> bool {
        self.pricing_weekdays.contains(&date.weekday()) && !self.non_pricing_days.contains(&date)
    }

    /// `date` itself if the fund prices that day, otherwise the next day it does.
    pub fn next_pricing_day(&self, date: NaiveDate) -> Result<NaiveDate, String> {
        let mut d = date;
        for _ in 0..CALENDAR_HORIZON_DAYS {
            if self.is_pricing_day(d) {
                return Ok(d);
            }
            d = d.checked_add_days(Days::new(1)).ok_or("date out of range")?;
        }
        Err(format!("{} has no pricing day within a year of {date}", self.fund.0))
    }

    /// The pricing day after `date`.
    pub fn following_pricing_day(&self, date: NaiveDate) -> Result<NaiveDate, String> {
        self.next_pricing_day(date.checked_add_days(Days::new(1)).ok_or("date out of range")?)
    }

    /// The pricing date of an order logged at `at`: today's if before the
    /// local cut-off on a pricing day, otherwise the next pricing day. Worked
    /// out at the edge and carried in the order's command, so the core never
    /// reads a clock.
    pub fn pricing_date_for(&self, at: DateTime<Utc>) -> Result<NaiveDate, String> {
        let local = at.with_timezone(&self.tz());
        let day = local.date_naive();
        if local.time() < self.cutoff && self.is_pricing_day(day) {
            Ok(day)
        } else {
            self.following_pricing_day(day)
        }
    }

    /// The date by which the price for `pricing_date` is expected: `lag`
    /// pricing days after it.
    pub fn price_due_by(&self, pricing_date: NaiveDate, lag: u32) -> Result<NaiveDate, String> {
        let mut d = pricing_date;
        for _ in 0..lag {
            d = self.following_pricing_day(d)?;
        }
        Ok(d)
    }
}

/// A sample definition for this crate's tests.
#[cfg(test)]
pub(crate) fn sample(fund: &str) -> FundDefinition {
    let pricing = [
        ("previous_move", "0.05"),
        ("benchmark_gap", "0.01"),
        ("stale_asset_days", "3"),
        ("unexplained_move", "0.005"),
        ("error_threshold", "0.003"),
        ("launch_price", "1.0000"),
        ("pricing_lag_days", "1"),
    ];
    FundDefinition {
        fund: FundId(fund.to_string()),
        version: 1,
        name: format!("{fund} fund"),
        currency: Currency::NZD,
        effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap_or_default(),
        pricing_weekdays: vec![Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri],
        non_pricing_days: BTreeSet::new(),
        cutoff: NaiveTime::from_hms_opt(15, 0, 0).unwrap_or_default(),
        time_zone: "Pacific/Auckland".to_string(),
        buy_spread_bps: 25,
        sell_spread_bps: 25,
        price_decimals: 4,
        unit_rounding: RoundingMode::Down,
        open_to_new_money: true,
        pricing: pricing.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap_or_default()
    }

    /// Purpose: an order before the local cut-off on a pricing day takes that
    /// day; at or after cut-off, or on a weekend or non-pricing day, it takes
    /// the next pricing day, read in New Zealand time.
    /// Why it matters: forward pricing is only fair if nobody can buy at a
    /// price they could already know. The cut-off is in local time, so a UTC
    /// mistake would let a whole afternoon trade on a known price.
    #[test]
    fn pricing_date_follows_cutoff_and_calendar() {
        let mut f = sample("growth");
        f.non_pricing_days.insert(day(2026, 11, 16));
        // Thursday 12 November, 14:59 in Auckland (UTC+13) is 01:59 UTC.
        let before = Utc.with_ymd_and_hms(2026, 11, 12, 1, 59, 0).unwrap();
        assert_eq!(f.pricing_date_for(before), Ok(day(2026, 11, 12)));
        let at = Utc.with_ymd_and_hms(2026, 11, 12, 2, 0, 0).unwrap();
        assert_eq!(f.pricing_date_for(at), Ok(day(2026, 11, 13)));
        // Friday after cut-off skips the weekend and the Monday holiday.
        let friday = Utc.with_ymd_and_hms(2026, 11, 13, 3, 0, 0).unwrap();
        assert_eq!(f.pricing_date_for(friday), Ok(day(2026, 11, 17)));
        // Saturday morning before cut-off is still not a pricing day.
        let saturday = Utc.with_ymd_and_hms(2026, 11, 13, 20, 0, 0).unwrap();
        assert_eq!(f.pricing_date_for(saturday), Ok(day(2026, 11, 17)));
        assert_eq!(f.price_due_by(day(2026, 11, 13), 1), Ok(day(2026, 11, 17)));
        assert_eq!(f.price_due_by(day(2026, 11, 13), 2), Ok(day(2026, 11, 18)));
    }

    /// Purpose: every malformed field of a definition is refused with a reason.
    /// Why it matters: a definition is activated once and never edited, so a
    /// bad spread or an empty calendar caught here never reaches a price.
    #[test]
    fn validate_refuses_each_bad_field() {
        assert!(sample("growth").validate().is_ok());
        let cases: Vec<(&str, Box<dyn Fn(&mut FundDefinition)>)> = vec![
            ("fund ID", Box::new(|f| f.fund = FundId("bad id".into()))),
            ("versions start", Box::new(|f| f.version = 0)),
            ("needs a name", Box::new(|f| f.name = " ".into())),
            ("at least one day", Box::new(|f| f.pricing_weekdays.clear())),
            ("unknown time zone", Box::new(|f| f.time_zone = "Mars/Base".into())),
            ("spread", Box::new(|f| f.buy_spread_bps = 1_001)),
            ("spread", Box::new(|f| f.sell_spread_bps = 1_001)),
            ("price_decimals", Box::new(|f| f.price_decimals = 9)),
            ("price_decimals", Box::new(|f| f.price_decimals = 0)),
        ];
        for (expect, change) in cases {
            let mut f = sample("growth");
            change(&mut f);
            let err = f.validate().err().unwrap_or_default();
            assert!(err.contains(expect), "{expect}: {err}");
        }
    }

    /// Purpose: each pricing term is read from the string map, and a missing,
    /// malformed, out of range or unknown term is refused by name.
    /// Why it matters: the core stores the map without reading it, so this is
    /// the only place a misspelt tolerance or a negative threshold is caught
    /// before it decides whether a price may be approved.
    #[test]
    fn pricing_terms_are_read_and_checked() {
        let terms = sample("growth").validate().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(terms.previous_move.as_str(), "0.05");
        assert_eq!(terms.stale_asset_days, 3);
        assert_eq!(terms.pricing_lag_days, 1);
        let mut no_benchmark = sample("growth");
        no_benchmark.pricing.remove("benchmark_gap");
        assert_eq!(no_benchmark.validate().map(|t| t.benchmark_gap), Ok(None));
        let cases = [
            ("previous_move", None, "missing"),
            ("previous_move", Some("abc"), "not a decimal"),
            ("unexplained_move", Some("1.5"), "between 0 and 1"),
            ("error_threshold", Some("-0.1"), "between 0 and 1"),
            ("benchmark_gap", Some("2"), "between 0 and 1"),
            ("stale_asset_days", Some("0"), "whole number"),
            ("pricing_lag_days", Some("x"), "whole number"),
            ("pricing_lag_days", None, "missing"),
            ("launch_price", Some("0"), "above zero"),
            ("spread", Some("1"), "not a term"),
        ];
        for (key, value, expect) in cases {
            let mut f = sample("growth");
            match value {
                Some(v) => f.pricing.insert(key.to_string(), v.to_string()),
                None => f.pricing.remove(key),
            };
            let err = f.validate().err().unwrap_or_default();
            assert!(err.contains(expect), "{key}: {err}");
        }
    }

    /// Purpose: a calendar whose every pricing weekday is a non-pricing day
    /// for a year is refused, and stepping past the last date is an error.
    /// Why it matters: such a fund would accept orders that could never
    /// execute; the search is bounded so a bad calendar is an error, not a hang.
    #[test]
    fn calendar_with_no_pricing_day_is_refused() {
        let mut f = sample("growth");
        f.pricing_weekdays = vec![Weekday::Mon];
        let mut d = f.effective_from;
        for _ in 0..400 {
            f.non_pricing_days.insert(d);
            d = d.succ_opt().unwrap_or(d);
        }
        assert!(f.validate().unwrap_err().contains("no pricing day"));
        assert!(f.following_pricing_day(NaiveDate::MAX).is_err());
    }

    /// Purpose: a new version must be the next number, keep the currency and
    /// not take effect before its predecessor.
    /// Why it matters: the unit type is frozen once units exist, and versions
    /// out of order would make "the definition on a date" ambiguous.
    #[test]
    fn versions_follow_in_order_with_frozen_currency() {
        let v1 = sample("growth");
        let mut v2 = sample("growth");
        v2.version = 2;
        assert_eq!(v2.may_follow(&v1), Ok(()));
        let mut skip = v2.clone();
        skip.version = 3;
        assert!(skip.may_follow(&v1).unwrap_err().contains("next version"));
        let mut usd = v2.clone();
        usd.currency = Currency::USD;
        assert!(usd.may_follow(&v1).unwrap_err().contains("frozen"));
        let mut early = v2;
        early.effective_from = day(2025, 1, 1);
        assert!(early.may_follow(&v1).unwrap_err().contains("before version"));
    }
}
