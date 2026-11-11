//! Accounting periods. A period is a calendar month, plus an adjustment
//! period after the last month of each financial year for the year-end
//! journals.

use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One accounting period. Periods order by time, with a year's adjustment
/// period directly after the month it follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeriodId {
    /// The last day of the month.
    end: NaiveDate,
    adjustment: bool,
}

fn month_end(year: i32, month: u32) -> Option<NaiveDate> {
    let (y, m) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    NaiveDate::from_ymd_opt(y, m, 1)?.pred_opt()
}

impl PeriodId {
    /// The month a date falls in.
    pub fn of(date: NaiveDate) -> Self {
        // Every month has a last day, so this always finds one.
        let end = month_end(date.year(), date.month()).unwrap_or(date);
        Self { end, adjustment: false }
    }

    pub fn month(year: i32, month: u32) -> Option<Self> {
        Some(Self {
            end: month_end(year, month)?,
            adjustment: false,
        })
    }

    /// The adjustment period that follows this month.
    pub fn adjustment(self) -> Self {
        Self {
            end: self.end,
            adjustment: true,
        }
    }

    pub fn is_adjustment(self) -> bool {
        self.adjustment
    }

    /// The first day of the month.
    pub fn start(self) -> NaiveDate {
        self.end.with_day(1).unwrap_or(self.end)
    }

    /// The last day of the month.
    pub fn end(self) -> NaiveDate {
        self.end
    }

    /// The month after this one. An adjustment period is followed by the
    /// same month as the month it adjusts.
    pub fn next_month(self) -> Self {
        Self::of(self.end.succ_opt().unwrap_or(self.end))
    }

    /// Whether a quarter of the financial year ends with this month.
    pub fn is_quarter_end(self, year_end_month: u32) -> bool {
        (self.end.month() + 12 - year_end_month % 12).is_multiple_of(3)
    }

    /// Whether the financial year ends with this month.
    pub fn is_year_end(self, year_end_month: u32) -> bool {
        self.end.month() == year_end_month
    }

    /// The first month of the financial year this period belongs to.
    pub fn year_start(self, year_end_month: u32) -> Self {
        let mut p = Self {
            end: self.end,
            adjustment: false,
        };
        while p.start().month() != year_end_month % 12 + 1 {
            p = Self::of(p.start().pred_opt().unwrap_or(p.start()));
        }
        p
    }
}

impl fmt::Display for PeriodId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}", self.end.year(), self.end.month())?;
        if self.adjustment {
            f.write_str("-ADJ")?;
        }
        Ok(())
    }
}

impl FromStr for PeriodId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("`{s}` is not a period: expected YYYY-MM or YYYY-MM-ADJ");
        let (month, adjustment) = match s.strip_suffix("-ADJ") {
            Some(m) => (m, true),
            None => (s, false),
        };
        let (y, m) = month.split_once('-').ok_or_else(bad)?;
        let period = Self::month(y.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?).ok_or_else(bad)?;
        Ok(if adjustment { period.adjustment() } else { period })
    }
}

impl Serialize for PeriodId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PeriodId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}
