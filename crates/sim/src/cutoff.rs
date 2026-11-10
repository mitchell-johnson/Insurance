//! When the business date advances.
//!
//! The business date is separate from the clock and moves only by a logged
//! command. The partition leader proposes that command at the scheduled
//! cutoff: midnight New Zealand time by default. This module is the only
//! place a time zone appears, and it is outside the state machine.

use chrono::{DateTime, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Pacific::Auckland;

/// The instant at which `business_date` closes: the next midnight in
/// New Zealand. Daylight saving makes two days a year 23 or 25 hours long;
/// the business date does not care.
pub fn cutoff_utc(business_date: NaiveDate) -> Option<DateTime<Utc>> {
    let next = business_date.succ_opt()?.and_time(NaiveTime::MIN);
    match Auckland.from_local_datetime(&next) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        // Midnight never falls in New Zealand's daylight saving gap or
        // overlap (the changes are at 2am and 3am), but be exact anyway.
        LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        LocalResult::None => None,
    }
}

/// Length of a business day on the wall clock.
pub fn day_length_hours(business_date: NaiveDate) -> Option<i64> {
    let start = cutoff_utc(business_date.pred_opt()?)?;
    Some((cutoff_utc(business_date)? - start).num_hours())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn clock_changes_make_short_and_long_days() {
        assert_eq!(day_length_hours(d(2028, 3, 15)), Some(24));
        // daylight saving ends on the first Sunday of April: a 25-hour day
        assert_eq!(day_length_hours(d(2028, 4, 2)), Some(25));
        // and starts on the last Sunday of September: a 23-hour day
        assert_eq!(day_length_hours(d(2028, 9, 24)), Some(23));
        // the leap day is an ordinary 24 hours
        assert_eq!(day_length_hours(d(2028, 2, 29)), Some(24));
    }

    #[test]
    fn cutoff_is_midnight_in_auckland() {
        // NZDT is UTC+13 in summer
        assert_eq!(cutoff_utc(d(2027, 12, 31)).unwrap().to_rfc3339(), "2027-12-31T11:00:00+00:00");
        // NZST is UTC+12 in winter
        assert_eq!(cutoff_utc(d(2028, 6, 30)).unwrap().to_rfc3339(), "2028-06-30T12:00:00+00:00");
    }
}
