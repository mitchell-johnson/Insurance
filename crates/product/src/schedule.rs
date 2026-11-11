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

    /// Purpose: a policy anchored on the 31st bills on the last day of shorter months and returns
    /// to the 31st afterwards.
    /// Why it matters: stepping a month at a time from the previous due date would drift to the
    /// 28th for good after one February. Billing dates are always worked out from the anchor.
    #[test]
    fn month_end_anchor_does_not_drift() {
        let a = d(2028, 1, 31);
        assert_eq!(period_start(a, Frequency::Monthly, 1), Some(d(2028, 2, 29)));
        assert_eq!(period_start(a, Frequency::Monthly, 2), Some(d(2028, 3, 31)));
        assert_eq!(period_start(a, Frequency::Monthly, 13), Some(d(2029, 2, 28)));
    }

    /// Purpose: a policy incepted on 29 February has its anniversary on 28 February in ordinary
    /// years and 29 February in leap years, and the policy year turns over on that day.
    /// Why it matters: anniversaries drive indexation and re-rating. A leap-day policy must not
    /// skip an anniversary or have it a day late three years in four.
    #[test]
    fn leap_day_anniversary() {
        let inc = d(2028, 2, 29);
        assert_eq!(anniversary(inc, 1), Some(d(2029, 2, 28)));
        assert_eq!(anniversary(inc, 4), Some(d(2032, 2, 29)));
        assert_eq!(policy_year(inc, d(2029, 2, 27)), 0);
        assert_eq!(policy_year(inc, d(2029, 2, 28)), 1);
    }

    /// Purpose: age is the number of birthdays reached: one less on the day before a birthday than
    /// on the birthday itself.
    /// Why it matters: stepped premiums are rated by age. An age that turned over a day early or
    /// late would put the customer in the wrong rate band on a bill.
    #[test]
    fn ages() {
        assert_eq!(age_on(d(1990, 6, 15), d(2027, 6, 14)), 36);
        assert_eq!(age_on(d(1990, 6, 15), d(2027, 6, 15)), 37);
    }
    /// Purpose: quarterly and half-yearly periods step by three and six months from the
    /// anchor, and fortnightly by fourteen days.
    /// Why it matters: no shipped product bills quarterly or half-yearly yet, so nothing
    /// else exercises these arms. A wrong step would bill a customer on the wrong day.
    #[test]
    fn every_frequency_steps_by_its_own_period() {
        let anchor = d(2027, 1, 31);
        assert_eq!(period_start(anchor, Frequency::Quarterly, 1), Some(d(2027, 4, 30)));
        assert_eq!(period_start(anchor, Frequency::HalfYearly, 1), Some(d(2027, 7, 31)));
        assert_eq!(period_start(anchor, Frequency::Fortnightly, 2), Some(d(2027, 2, 28)));
        assert_eq!(period_start(anchor, Frequency::Yearly, 1), Some(d(2028, 1, 31)));
    }

    /// Purpose: a date before inception is policy year zero, not a negative or wrapped number.
    /// Why it matters: quotes are priced before the policy starts. An underflow here would
    /// turn into a rate lookup at an absurd age.
    #[test]
    fn a_date_before_inception_is_policy_year_zero() {
        assert_eq!(policy_year(d(2027, 6, 1), d(2027, 5, 31)), 0);
        assert_eq!(age_on(d(2030, 1, 1), d(2027, 1, 1)), 0);
    }
}
