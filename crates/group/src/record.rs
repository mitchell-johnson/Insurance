//! The canonical member record: what every employer's file is mapped to.
//!
//! Rating attributes travel with the record. Identity (names, contact
//! details) is split off when the file is mapped and goes only to the
//! customer master, so the module's own log and state never hold it.

use chrono::NaiveDate;
use ic_core::LifeFactors;
use ic_product::Sex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What a row of a change file says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowChange {
    Join,
    Leave,
    Change,
    /// A leave of absence starts.
    AbsenceStart,
    /// The member is back at work.
    AbsenceEnd,
}

/// The attributes the scheme holds about a member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAttrs {
    #[serde(default)]
    pub date_of_birth: Option<NaiveDate>,
    #[serde(default)]
    pub sex: Option<Sex>,
    #[serde(default)]
    pub smoker: Option<bool>,
    #[serde(default)]
    pub category: Option<String>,
    /// Annual salary in minor units.
    #[serde(default)]
    pub salary_minor: Option<i64>,
    /// Weekly hours in hundredths of an hour.
    #[serde(default)]
    pub hours_hundredths: Option<u32>,
    #[serde(default)]
    pub employment_start: Option<NaiveDate>,
    #[serde(default)]
    pub actively_at_work: Option<bool>,
    #[serde(default)]
    pub billing_group: Option<String>,
}

impl MemberAttrs {
    /// The rating factors the core holds for the member.
    pub fn factors(&self) -> LifeFactors {
        LifeFactors {
            date_of_birth: self.date_of_birth,
            sex: self.sex,
            smoker: self.smoker,
            category: self.category.clone(),
            salary_minor: self.salary_minor,
        }
    }

    /// The names of the fields on which `other` differs from this record,
    /// counting only fields `other` states. A file that leaves a column out
    /// says nothing about it.
    pub fn changed_fields(&self, other: &MemberAttrs) -> Vec<&'static str> {
        let mut out = Vec::new();
        let mut check = |name: &'static str, differs: bool| {
            if differs {
                out.push(name);
            }
        };
        check("date_of_birth", other.date_of_birth.is_some() && other.date_of_birth != self.date_of_birth);
        check("sex", other.sex.is_some() && other.sex != self.sex);
        check("smoker", other.smoker.is_some() && other.smoker != self.smoker);
        check("category", other.category.is_some() && other.category != self.category);
        check("salary", other.salary_minor.is_some() && other.salary_minor != self.salary_minor);
        check("hours", other.hours_hundredths.is_some() && other.hours_hundredths != self.hours_hundredths);
        check("billing_group", other.billing_group.is_some() && other.billing_group != self.billing_group);
        out
    }

    /// This record with every field `other` states taken from `other`.
    #[must_use]
    pub fn merged(&self, other: &MemberAttrs) -> MemberAttrs {
        MemberAttrs {
            date_of_birth: other.date_of_birth.or(self.date_of_birth),
            sex: other.sex.or(self.sex),
            smoker: other.smoker.or(self.smoker),
            category: other.category.clone().or_else(|| self.category.clone()),
            salary_minor: other.salary_minor.or(self.salary_minor),
            hours_hundredths: other.hours_hundredths.or(self.hours_hundredths),
            employment_start: other.employment_start.or(self.employment_start),
            actively_at_work: other.actively_at_work.or(self.actively_at_work),
            billing_group: other.billing_group.clone().or_else(|| self.billing_group.clone()),
        }
    }
}

/// One row of a membership file in canonical form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRow {
    /// The data row's number in the file, from 1. With the file ID it keys
    /// the command the row becomes.
    pub row: u32,
    /// The employer's own reference for the member: the matching key.
    pub employee_id: String,
    /// Stated by a change file; absent in a full file.
    #[serde(default)]
    pub change: Option<RowChange>,
    #[serde(default)]
    pub effective: Option<NaiveDate>,
    pub attrs: MemberAttrs,
    /// The employer says this member was previously known by another ID.
    #[serde(default)]
    pub previous_employee_id: Option<String>,
    /// A salted hash of the member's name and date of birth. It lets a
    /// changed ID or a likely duplicate be noticed without the name being
    /// kept.
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// Who a member is. Sent to the customer master and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberIdentity {
    pub row: u32,
    pub employee_id: String,
    pub given_name: String,
    pub family_name: String,
    pub date_of_birth: Option<NaiveDate>,
    pub email: Option<String>,
    pub national_id: Option<String>,
}

/// The fingerprint of a name and date of birth under a scheme's salt.
pub fn fingerprint(salt: &str, given_name: &str, family_name: &str, date_of_birth: Option<NaiveDate>) -> String {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let mut h = Sha256::new();
    h.update(b"ic.group.fingerprint.v1\0");
    h.update(salt.as_bytes());
    h.update([0]);
    h.update(norm(given_name).as_bytes());
    h.update([0]);
    h.update(norm(family_name).as_bytes());
    h.update([0]);
    h.update(date_of_birth.map(|d| d.to_string()).unwrap_or_default().as_bytes());
    hex::encode(&h.finalize()[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terms::tests::d;

    /// Purpose: comparing a member with a file row reports only fields the row states and that
    /// differ, and merging takes exactly those fields.
    /// Why it matters: employers' files often omit columns. A missing column must never be read as
    /// the value having been cleared, or a file without salaries would wipe every member's cover.
    #[test]
    fn a_row_changes_only_what_it_states() {
        let held = MemberAttrs {
            date_of_birth: Some(d(1980, 5, 1)),
            sex: Some(Sex::Female),
            smoker: Some(false),
            category: Some("staff".to_string()),
            salary_minor: Some(80_000_00),
            hours_hundredths: Some(4000),
            employment_start: Some(d(2020, 1, 1)),
            actively_at_work: Some(true),
            billing_group: Some("akl".to_string()),
        };
        assert!(held.changed_fields(&MemberAttrs::default()).is_empty());
        assert_eq!(held.merged(&MemberAttrs::default()), held);
        let row = MemberAttrs {
            date_of_birth: Some(d(1981, 5, 1)),
            sex: Some(Sex::Male),
            smoker: Some(true),
            category: Some("executive".to_string()),
            salary_minor: Some(90_000_00),
            hours_hundredths: Some(3000),
            employment_start: Some(d(2021, 1, 1)),
            actively_at_work: Some(false),
            billing_group: Some("wlg".to_string()),
        };
        assert_eq!(
            held.changed_fields(&row),
            vec!["date_of_birth", "sex", "smoker", "category", "salary", "hours", "billing_group"]
        );
        assert_eq!(held.merged(&row), row);
        assert_eq!(row.factors().salary_minor, Some(90_000_00));
        assert_eq!(row.factors().category.as_deref(), Some("executive"));
    }

    /// Purpose: the fingerprint ignores case and spacing, and changes with the name, the date of
    /// birth and the scheme's salt.
    /// Why it matters: the fingerprint is how a re-keyed employee ID or a duplicate is noticed. It
    /// must match the same person typed slightly differently, and must not let one scheme's members
    /// be correlated with another's.
    #[test]
    fn fingerprints_match_the_same_person_and_no_one_else() {
        let a = fingerprint("s1", "Aroha", "Ngata", Some(d(1980, 5, 1)));
        assert_eq!(a, fingerprint("s1", "  aroha ", "NGATA", Some(d(1980, 5, 1))));
        assert_ne!(a, fingerprint("s1", "Aroha", "Ngata", Some(d(1980, 5, 2))));
        assert_ne!(a, fingerprint("s1", "Aroha", "Ngata", None));
        assert_ne!(a, fingerprint("s1", "Aroha", "Ngatai", Some(d(1980, 5, 1))));
        assert_ne!(a, fingerprint("s2", "Aroha", "Ngata", Some(d(1980, 5, 1))));
        assert_eq!(a.len(), 32);
    }
}
