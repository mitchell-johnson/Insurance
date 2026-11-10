//! Two timelines on every attribute: when it was true, and when the insurer
//! learned it. Backdated membership changes, late-notified deaths and
//! corrected dates of birth are the norm in insurance.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version<T> {
    /// When the value became true.
    pub valid_from: NaiveDate,
    /// The business date the core learned it.
    pub recorded_on: NaiveDate,
    /// Log index of the command that recorded it: the tie-break.
    pub recorded_at: u64,
    pub value: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bitemporal<T> {
    versions: Vec<Version<T>>,
}

impl<T> Bitemporal<T> {
    pub fn new(valid_from: NaiveDate, recorded_on: NaiveDate, recorded_at: u64, value: T) -> Self {
        Self {
            versions: vec![Version {
                valid_from,
                recorded_on,
                recorded_at,
                value,
            }],
        }
    }

    /// Record a new fact. Nothing is ever overwritten.
    pub fn record(&mut self, valid_from: NaiveDate, recorded_on: NaiveDate, recorded_at: u64, value: T) {
        self.versions.push(Version {
            valid_from,
            recorded_on,
            recorded_at,
            value,
        });
    }

    /// The value true on `valid`, as currently known.
    pub fn as_of(&self, valid: NaiveDate) -> Option<&T> {
        self.as_known(valid, NaiveDate::MAX)
    }

    /// The value true on `valid`, as it was known on `known_on`. Among the
    /// versions already recorded, the one learned last wins if it covers the
    /// date: a later correction supersedes an earlier belief from its
    /// `valid_from` onwards.
    pub fn as_known(&self, valid: NaiveDate, known_on: NaiveDate) -> Option<&T> {
        self.versions
            .iter()
            .filter(|v| v.recorded_on <= known_on && v.valid_from <= valid)
            .max_by_key(|v| v.recorded_at)
            .map(|v| &v.value)
    }

    pub fn versions(&self) -> &[Version<T>] {
        &self.versions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn backdated_change_is_visible_on_both_timelines() {
        // salary known as 80k from January; in June we learn it rose to 90k in March
        let mut b = Bitemporal::new(d(2027, 1, 1), d(2027, 1, 1), 1, 80_000);
        b.record(d(2027, 3, 1), d(2027, 6, 10), 2, 90_000);
        assert_eq!(b.as_of(d(2027, 2, 1)), Some(&80_000));
        assert_eq!(b.as_of(d(2027, 4, 1)), Some(&90_000));
        // what we believed in April about April
        assert_eq!(b.as_known(d(2027, 4, 1), d(2027, 4, 30)), Some(&80_000));
        assert_eq!(b.as_of(d(2026, 12, 1)), None);
    }

    #[test]
    fn correction_from_the_start_supersedes() {
        let mut b = Bitemporal::new(d(2027, 1, 1), d(2027, 1, 1), 1, "1980-01-01");
        b.record(d(2027, 1, 1), d(2027, 5, 1), 9, "1979-01-01");
        assert_eq!(b.as_of(d(2027, 1, 1)), Some(&"1979-01-01"));
        assert_eq!(b.as_known(d(2027, 1, 1), d(2027, 2, 1)), Some(&"1980-01-01"));
    }
}
