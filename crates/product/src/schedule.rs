//! Pure date arithmetic for billing periods, anniversaries and ages.

use chrono::{Datelike, Days, Months, NaiveDate};

use crate::Frequency;

/// Start of the `n`th billing period (0-based) for a schedule anchored on
/// `anchor`. Month-based periods are always counted from the anchor, so a
/// policy anchored on the 31st bills on the last day of shorter months and
/// returns to the 31st afterwards.
pub fn period_start(anchor: NaiveDate, freq: Frequency, n: u32) -> Option<NaiveDate> {
    match freq {
        Frequency::Fortnightly => anchor.checked_add_days(Days::new(14 * n as u64)),
        Frequency::Monthly => anchor.checked_add_months(Months::new(n)),
        Frequency::Quarterly => anchor.checked_add_months(Months::new(3 * n)),
        Frequency::HalfYearly => anchor.checked_add_months(Months::new(6 * n)),
        Frequency::Yearly => anchor.checked_add_months(Months::new(12 * n)),
    }
}

/// The `n`th policy anniversary. A 29 February inception falls on 28 February
/// in years that are not leap years.
pub fn anniversary(inception: NaiveDate, n: u32) -> Option<NaiveDate> {
    inception.checked_add_months(Months::new(12 * n))
}

/// Number of whole policy years completed on `on`.
pub fn policy_year(inception: NaiveDate, on: NaiveDate) -> u32 {
    if on < inception {
        return 0;
    }
    let mut n = (on.year() - inception.year()).max(0) as u32;
    while n > 0 && anniversary(inception, n).is_none_or(|a| a > on) {
        n -= 1;
    }
    n
}

/// Age last birthday on a date.
pub fn age_on(date_of_birth: NaiveDate, on: NaiveDate) -> u32 {
    policy_year(date_of_birth, on)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn month_end_anchor_does_not_drift() {
        let a = d(2028, 1, 31);
        assert_eq!(period_start(a, Frequency::Monthly, 1), Some(d(2028, 2, 29)));
        assert_eq!(period_start(a, Frequency::Monthly, 2), Some(d(2028, 3, 31)));
        assert_eq!(period_start(a, Frequency::Monthly, 13), Some(d(2029, 2, 28)));
    }

    #[test]
    fn leap_day_anniversary() {
        let inc = d(2028, 2, 29);
        assert_eq!(anniversary(inc, 1), Some(d(2029, 2, 28)));
        assert_eq!(anniversary(inc, 4), Some(d(2032, 2, 29)));
        assert_eq!(policy_year(inc, d(2029, 2, 27)), 0);
        assert_eq!(policy_year(inc, d(2029, 2, 28)), 1);
    }

    #[test]
    fn ages() {
        assert_eq!(age_on(d(1990, 6, 15), d(2027, 6, 14)), 36);
        assert_eq!(age_on(d(1990, 6, 15), d(2027, 6, 15)), 37);
    }
}
