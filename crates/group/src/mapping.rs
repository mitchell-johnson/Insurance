//! File mappings. Every employer's file looks different, so each scheme has
//! a versioned mapping from its columns to the canonical member record. A
//! mapping is data, identified by a content hash like a product, and proven
//! by tests built from the employer's sample files.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_product::Sex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::record::{fingerprint, MemberAttrs, MemberIdentity, MemberRow, RowChange};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Every current member. Joiners, leavers and changes are derived by
    /// comparison with the membership in force.
    Full,
    /// Each row states what changed.
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SalaryUnit {
    /// Dollars, with or without cents.
    Major,
    /// Whole cents.
    Minor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SalaryPeriod {
    Annual,
    Monthly,
    Fortnightly,
    Weekly,
}

impl SalaryPeriod {
    fn per_year(self) -> i64 {
        match self {
            SalaryPeriod::Annual => 1,
            SalaryPeriod::Monthly => 12,
            SalaryPeriod::Fortnightly => 26,
            SalaryPeriod::Weekly => 52,
        }
    }
}

/// The header of the column each canonical field is read from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Columns {
    pub employee_id: String,
    #[serde(default)]
    pub given_name: Option<String>,
    #[serde(default)]
    pub family_name: Option<String>,
    #[serde(default)]
    pub date_of_birth: Option<String>,
    #[serde(default)]
    pub sex: Option<String>,
    #[serde(default)]
    pub smoker: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub salary: Option<String>,
    #[serde(default)]
    pub hours: Option<String>,
    #[serde(default)]
    pub employment_start: Option<String>,
    #[serde(default)]
    pub actively_at_work: Option<String>,
    #[serde(default)]
    pub billing_group: Option<String>,
    #[serde(default)]
    pub change: Option<String>,
    #[serde(default)]
    pub effective: Option<String>,
    #[serde(default)]
    pub previous_employee_id: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub national_id: Option<String>,
}

fn comma() -> char {
    ','
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMapping {
    pub id: String,
    pub version: u32,
    pub kind: FileKind,
    #[serde(default = "comma")]
    pub delimiter: char,
    pub columns: Columns,
    /// A `strftime` pattern, for example `%d/%m/%Y`.
    pub date_format: String,
    pub salary_unit: SalaryUnit,
    pub salary_period: SalaryPeriod,
    /// The employer's category values (compared without case) to the scheme's
    /// benefit categories. Empty means the file uses the scheme's own codes.
    #[serde(default)]
    pub categories: BTreeMap<String, String>,
    /// The employer's values in the change column, compared without case.
    #[serde(default)]
    pub change_values: BTreeMap<String, RowChange>,
    /// SHA-256 of the mapping. Filled by [`FileMapping::seal`].
    #[serde(default)]
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MappingError {
    #[error("the mapping is not usable: {0}")]
    Invalid(String),
    #[error("the file has no header row")]
    NoHeader,
    #[error("the file has no column headed {0:?}")]
    MissingColumn(String),
    #[error("the file has two columns headed {0:?}")]
    DuplicateColumn(String),
}

/// A row the mapping could not read. The code and the field say what was
/// wrong; the value itself is never repeated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowReject {
    pub row: u32,
    pub employee_id: Option<String>,
    pub code: String,
    #[serde(default)]
    pub field: Option<String>,
}

impl RowReject {
    pub fn new(row: u32, employee_id: Option<&str>, code: &str, field: Option<&str>) -> Self {
        Self {
            row,
            employee_id: employee_id.map(str::to_string),
            code: code.to_string(),
            field: field.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mapped {
    pub rows: Vec<MemberRow>,
    /// Identity for the rows that carried names. For the customer master only.
    pub identities: Vec<MemberIdentity>,
    pub rejected: Vec<RowReject>,
}

/// A number with at most two decimal places, as hundredths. Currency signs,
/// thousands separators and spaces are ignored. Negative numbers are refused.
pub fn parse_hundredths(s: &str) -> Option<i64> {
    let cleaned: String = s.chars().filter(|c| !matches!(c, '$' | ',' | ' ' | '\u{a0}')).collect();
    let (whole, frac) = cleaned.split_once('.').unwrap_or((&cleaned, ""));
    if whole.is_empty() && frac.is_empty() || frac.len() > 2 || !whole.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let whole: i64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let frac: i64 = format!("{frac:0<2}").parse().ok()?;
    whole.checked_mul(100)?.checked_add(frac)
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_lowercase().as_str() {
        "y" | "yes" | "true" | "1" => Some(true),
        "n" | "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn parse_sex(s: &str) -> Option<Sex> {
    match s.to_lowercase().as_str() {
        "f" | "female" => Some(Sex::Female),
        "m" | "male" => Some(Sex::Male),
        _ => None,
    }
}

impl FileMapping {
    fn hash(&self) -> String {
        let mut copy = self.clone();
        copy.content_hash = String::new();
        let mut h = Sha256::new();
        h.update(b"ic.group.mapping.v1\0");
        h.update(serde_json::to_vec(&copy).unwrap_or_default());
        hex::encode(h.finalize())
    }

    /// Check the mapping and fill its content hash.
    pub fn seal(mut self) -> Result<Self, MappingError> {
        let invalid = |m: &str| Err(MappingError::Invalid(m.to_string()));
        if self.id.is_empty() || self.version == 0 {
            return invalid("a mapping needs an ID and a version from 1");
        }
        if self.columns.employee_id.trim().is_empty() {
            return invalid("the employee ID column must be named");
        }
        let probe = NaiveDate::from_ymd_opt(2027, 11, 23).unwrap_or(NaiveDate::MIN);
        let round_trip = {
            use std::fmt::Write;
            let mut text = String::new();
            write!(text, "{}", probe.format(&self.date_format))
                .ok()
                .and_then(|()| NaiveDate::parse_from_str(&text, &self.date_format).ok())
        };
        if round_trip != Some(probe) {
            return invalid("the date format does not describe a whole date");
        }
        if self.kind == FileKind::Changes && (self.columns.change.is_none() || self.change_values.is_empty()) {
            return invalid("a change file mapping needs a change column and its values");
        }
        self.categories = self.categories.into_iter().map(|(k, v)| (k.trim().to_lowercase(), v)).collect();
        self.change_values = self.change_values.into_iter().map(|(k, v)| (k.trim().to_lowercase(), v)).collect();
        self.content_hash = self.hash();
        Ok(self)
    }

    /// Whether the content hash is the hash of the content.
    pub fn is_sealed(&self) -> bool {
        !self.content_hash.is_empty() && self.content_hash == self.hash()
    }

    /// Map a parsed file to canonical rows. `salt` is the scheme's
    /// fingerprint salt.
    pub fn map(&self, table: &[Vec<String>], salt: &str) -> Result<Mapped, MappingError> {
        let header = table.first().ok_or(MappingError::NoHeader)?;
        let mut index: BTreeMap<String, usize> = BTreeMap::new();
        for (i, h) in header.iter().enumerate() {
            let key = h.trim().to_lowercase();
            if !key.is_empty() && index.insert(key, i).is_some() {
                return Err(MappingError::DuplicateColumn(h.trim().to_string()));
            }
        }
        let find = |name: &Option<String>| -> Result<Option<usize>, MappingError> {
            match name {
                None => Ok(None),
                Some(n) => index
                    .get(&n.trim().to_lowercase())
                    .copied()
                    .map(Some)
                    .ok_or_else(|| MappingError::MissingColumn(n.clone())),
            }
        };
        let c = &self.columns;
        let id_col = find(&Some(c.employee_id.clone()))?.unwrap_or(0);
        let cols = [
            find(&c.given_name)?,
            find(&c.family_name)?,
            find(&c.date_of_birth)?,
            find(&c.sex)?,
            find(&c.smoker)?,
            find(&c.category)?,
            find(&c.salary)?,
            find(&c.hours)?,
            find(&c.employment_start)?,
            find(&c.actively_at_work)?,
            find(&c.billing_group)?,
            find(&c.change)?,
            find(&c.effective)?,
            find(&c.previous_employee_id)?,
            find(&c.email)?,
            find(&c.national_id)?,
        ];
        let [given, family, dob, sex, smoker, category, salary, hours, started, at_work, billing_group, change, effective, previous, email, national_id] = cols;

        let mut out = Mapped::default();
        for (n, cells) in table.iter().enumerate().skip(1) {
            let row = n as u32;
            let cell = |col: Option<usize>| -> Option<&str> { col.and_then(|i| cells.get(i)).map(|s| s.trim()).filter(|s| !s.is_empty()) };
            let Some(employee_id) = cell(Some(id_col)) else {
                out.rejected.push(RowReject::new(row, None, "missing_employee_id", Some("employee_id")));
                continue;
            };
            match self.map_row(
                row,
                employee_id,
                &cell,
                [
                    dob,
                    sex,
                    smoker,
                    category,
                    salary,
                    hours,
                    started,
                    at_work,
                    billing_group,
                    change,
                    effective,
                    previous,
                ],
            ) {
                Ok(mut member) => {
                    if let (Some(g), Some(f)) = (cell(given), cell(family)) {
                        member.fingerprint = Some(fingerprint(salt, g, f, member.attrs.date_of_birth));
                        out.identities.push(MemberIdentity {
                            row,
                            employee_id: employee_id.to_string(),
                            given_name: g.to_string(),
                            family_name: f.to_string(),
                            date_of_birth: member.attrs.date_of_birth,
                            email: cell(email).map(str::to_string),
                            national_id: cell(national_id).map(str::to_string),
                        });
                    }
                    out.rows.push(member);
                }
                Err((code, field)) => out.rejected.push(RowReject::new(row, Some(employee_id), code, Some(field))),
            }
        }
        Ok(out)
    }

    fn map_row<'a>(
        &self,
        row: u32,
        employee_id: &str,
        cell: &impl Fn(Option<usize>) -> Option<&'a str>,
        cols: [Option<usize>; 12],
    ) -> Result<MemberRow, (&'static str, &'static str)> {
        let [dob, sex, smoker, category, salary, hours, started, at_work, billing_group, change, effective, previous] = cols;
        let date = |col: Option<usize>, field: &'static str| -> Result<Option<NaiveDate>, (&'static str, &'static str)> {
            cell(col)
                .map(|s| NaiveDate::parse_from_str(s, &self.date_format).map_err(|_| ("bad_date", field)))
                .transpose()
        };
        let flag = |col: Option<usize>, field: &'static str| cell(col).map(|s| parse_bool(s).ok_or(("bad_boolean", field))).transpose();
        let salary_minor = cell(salary)
            .map(|s| {
                let amount = match self.salary_unit {
                    SalaryUnit::Major => parse_hundredths(s),
                    SalaryUnit::Minor => s.parse::<i64>().ok().filter(|v| *v >= 0),
                };
                amount
                    .and_then(|a| a.checked_mul(self.salary_period.per_year()))
                    .ok_or(("bad_number", "salary"))
            })
            .transpose()?;
        let hours_hundredths = cell(hours)
            .map(|s| parse_hundredths(s).and_then(|h| u32::try_from(h).ok()).ok_or(("bad_number", "hours")))
            .transpose()?;
        let category = cell(category)
            .map(|s| {
                if self.categories.is_empty() {
                    Ok(s.to_string())
                } else {
                    self.categories.get(&s.to_lowercase()).cloned().ok_or(("unknown_value", "category"))
                }
            })
            .transpose()?;
        let change = cell(change)
            .map(|s| self.change_values.get(&s.to_lowercase()).copied().ok_or(("unknown_value", "change")))
            .transpose()?;
        if self.kind == FileKind::Changes && change.is_none() {
            return Err(("missing_value", "change"));
        }
        Ok(MemberRow {
            row,
            employee_id: employee_id.to_string(),
            change,
            effective: date(effective, "effective")?,
            attrs: MemberAttrs {
                date_of_birth: date(dob, "date_of_birth")?,
                sex: cell(sex).map(|s| parse_sex(s).ok_or(("unknown_value", "sex"))).transpose()?,
                smoker: flag(smoker, "smoker")?,
                category,
                salary_minor,
                hours_hundredths,
                employment_start: date(started, "employment_start")?,
                actively_at_work: flag(at_work, "actively_at_work")?,
                billing_group: cell(billing_group).map(str::to_string),
            },
            previous_employee_id: cell(previous).map(str::to_string),
            fingerprint: None,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::csv;
    use crate::terms::tests::d;

    /// The full-file layout of a fictional employer, Harbour Freight.
    pub(crate) fn full_mapping() -> FileMapping {
        FileMapping {
            id: "harbour-freight-full".to_string(),
            version: 1,
            kind: FileKind::Full,
            delimiter: ',',
            columns: Columns {
                employee_id: "Emp No".to_string(),
                given_name: Some("First Name".to_string()),
                family_name: Some("Surname".to_string()),
                date_of_birth: Some("DOB".to_string()),
                sex: Some("Gender".to_string()),
                smoker: None,
                category: Some("Grade".to_string()),
                salary: Some("Annual Salary".to_string()),
                hours: Some("Hours".to_string()),
                employment_start: Some("Start Date".to_string()),
                actively_at_work: Some("At Work".to_string()),
                billing_group: Some("Site".to_string()),
                change: None,
                effective: None,
                previous_employee_id: Some("Old Emp No".to_string()),
                email: Some("Email".to_string()),
                national_id: None,
            },
            date_format: "%d/%m/%Y".to_string(),
            salary_unit: SalaryUnit::Major,
            salary_period: SalaryPeriod::Annual,
            categories: [("Staff", "staff"), ("EXEC", "executive")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            change_values: BTreeMap::new(),
            content_hash: String::new(),
        }
        .seal()
        .unwrap()
    }

    /// The change-file layout of the same employer's payroll feed.
    pub(crate) fn change_mapping() -> FileMapping {
        FileMapping {
            id: "harbour-freight-changes".to_string(),
            version: 1,
            kind: FileKind::Changes,
            delimiter: ';',
            columns: Columns {
                employee_id: "id".to_string(),
                given_name: Some("given".to_string()),
                family_name: Some("family".to_string()),
                date_of_birth: Some("born".to_string()),
                smoker: Some("smoker".to_string()),
                category: Some("category".to_string()),
                salary: Some("pay".to_string()),
                change: Some("event".to_string()),
                effective: Some("from".to_string()),
                national_id: Some("ird".to_string()),
                ..Columns::default()
            },
            date_format: "%Y-%m-%d".to_string(),
            salary_unit: SalaryUnit::Minor,
            salary_period: SalaryPeriod::Monthly,
            categories: BTreeMap::new(),
            change_values: [
                ("NEW", RowChange::Join),
                ("TERM", RowChange::Leave),
                ("UPD", RowChange::Change),
                ("LOA", RowChange::AbsenceStart),
                ("RTW", RowChange::AbsenceEnd),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
            content_hash: String::new(),
        }
        .seal()
        .unwrap()
    }

    pub(crate) const FULL_SAMPLE: &str = "\
Emp No,First Name,Surname,DOB,Gender,Grade,Annual Salary,Hours,Start Date,At Work,Site,Old Emp No,Email
E001,Aroha,Ngata,01/05/1980,F,Staff,\"$85,000.00\",40,01/02/2020,Y,akl,,aroha@example.test
E002,Tane,Parata,12/11/1975,M,EXEC,210000,37.5,15/06/2015,yes,wlg,,
E003,Mele,Tupou,30/09/1992,female,staff,62000.5,20,01/10/2027,TRUE,akl,X9,
";

    /// Purpose: the sample full file of a fictional employer maps to the canonical records it
    /// should: dates in the employer's format, salaries with currency signs and separators,
    /// categories translated, hours with a fraction, and identity split off from the member record.
    /// Why it matters: this is the test "built from sample files" that proves a mapping before an
    /// employer's first real file. If a salary is misread, every benefit and premium for that
    /// member is wrong.
    #[test]
    fn the_sample_full_file_maps_to_canonical_records() {
        let m = full_mapping();
        let mapped = m.map(&csv::parse(FULL_SAMPLE.as_bytes(), m.delimiter).unwrap(), "salt").unwrap();
        assert!(mapped.rejected.is_empty(), "{:?}", mapped.rejected);
        assert_eq!(mapped.rows.len(), 3);
        let aroha = &mapped.rows[0];
        assert_eq!((aroha.row, aroha.employee_id.as_str(), aroha.change, aroha.effective), (1, "E001", None, None));
        assert_eq!(
            aroha.attrs,
            MemberAttrs {
                date_of_birth: Some(d(1980, 5, 1)),
                sex: Some(Sex::Female),
                smoker: None,
                category: Some("staff".to_string()),
                salary_minor: Some(85_000_00),
                hours_hundredths: Some(4000),
                employment_start: Some(d(2020, 2, 1)),
                actively_at_work: Some(true),
                billing_group: Some("akl".to_string()),
            }
        );
        assert_eq!(aroha.fingerprint, Some(fingerprint("salt", "Aroha", "Ngata", Some(d(1980, 5, 1)))));
        let tane = &mapped.rows[1];
        assert_eq!((tane.attrs.sex, tane.attrs.category.as_deref()), (Some(Sex::Male), Some("executive")));
        assert_eq!((tane.attrs.salary_minor, tane.attrs.hours_hundredths), (Some(210_000_00), Some(3750)));
        let mele = &mapped.rows[2];
        assert_eq!((mele.attrs.salary_minor, mele.previous_employee_id.as_deref()), (Some(62_000_50), Some("X9")));
        // Identity is separate, and nothing in the member record carries a name.
        assert_eq!(mapped.identities.len(), 3);
        assert_eq!(mapped.identities[0].email.as_deref(), Some("aroha@example.test"));
        assert_eq!(mapped.identities[1].email, None);
        assert!(!serde_json::to_string(&mapped.rows).unwrap().contains("Ngata"));
    }

    /// Purpose: the sample change file maps each event code, annualises monthly pay given in
    /// cents, and reads a semicolon-delimited layout with its own date format.
    /// Why it matters: payroll feeds state events rather than a full list. A join read as a leave,
    /// or a monthly figure taken as annual, would end cover or cut it to a twelfth.
    #[test]
    fn the_sample_change_file_maps_each_event() {
        let m = change_mapping();
        let text = "id;given;family;born;smoker;category;pay;event;from;ird\n\
                    E010;Wiremu;Kereama;1988-03-04;N;staff;700000;new;2027-12-01;123\n\
                    E001;;;;;;;TERM;2027-12-15;\n\
                    E002;;;;1;executive;1800000;upd;2027-12-01;\n\
                    E003;;;;;;;LOA;2027-12-10;\n\
                    E003;;;;;;;rtw;2028-01-10;\n";
        let mapped = m.map(&csv::parse(text.as_bytes(), m.delimiter).unwrap(), "salt").unwrap();
        assert!(mapped.rejected.is_empty(), "{:?}", mapped.rejected);
        let changes: Vec<_> = mapped.rows.iter().map(|r| (r.employee_id.as_str(), r.change, r.effective)).collect();
        assert_eq!(
            changes,
            vec![
                ("E010", Some(RowChange::Join), Some(d(2027, 12, 1))),
                ("E001", Some(RowChange::Leave), Some(d(2027, 12, 15))),
                ("E002", Some(RowChange::Change), Some(d(2027, 12, 1))),
                ("E003", Some(RowChange::AbsenceStart), Some(d(2027, 12, 10))),
                ("E003", Some(RowChange::AbsenceEnd), Some(d(2028, 1, 10))),
            ]
        );
        assert_eq!(mapped.rows[0].attrs.salary_minor, Some(84_000_00));
        assert_eq!(mapped.rows[0].attrs.smoker, Some(false));
        assert_eq!(mapped.rows[2].attrs.smoker, Some(true));
        assert_eq!(mapped.identities.len(), 1);
        assert_eq!(mapped.identities[0].national_id.as_deref(), Some("123"));
    }

    /// Purpose: each kind of unreadable cell rejects its own row with a code and the field's name,
    /// never the value, and the other rows still map.
    /// Why it matters: one bad date must not stop a thousand good rows, and the employer needs to
    /// know which field to fix. The rejected value is left out because reject lists are shown and
    /// logged widely.
    #[test]
    fn unreadable_rows_are_rejected_one_by_one() {
        let m = full_mapping();
        let head = "Emp No,First Name,Surname,DOB,Gender,Grade,Annual Salary,Hours,Start Date,At Work,Site,Old Emp No,Email\n";
        let body = "\
,No,Id,01/05/1980,F,Staff,1,1,01/02/2020,Y,akl,,
E1,A,B,1980-05-01,F,Staff,1,1,01/02/2020,Y,akl,,
E2,A,B,01/05/1980,X,Staff,1,1,01/02/2020,Y,akl,,
E3,A,B,01/05/1980,F,Contractor,1,1,01/02/2020,Y,akl,,
E4,A,B,01/05/1980,F,Staff,-5,1,01/02/2020,Y,akl,,
E5,A,B,01/05/1980,F,Staff,1,many,01/02/2020,Y,akl,,
E6,A,B,01/05/1980,F,Staff,1,1,soon,Y,akl,,
E7,A,B,01/05/1980,F,Staff,1,1,01/02/2020,maybe,akl,,
E8,A,B,01/05/1980,F,Staff,1.005,1,01/02/2020,Y,akl,,
E9,A,B,01/05/1980,F,Staff,85000,40,01/02/2020,N,akl,,
";
        let mapped = m.map(&csv::parse(format!("{head}{body}").as_bytes(), ',').unwrap(), "salt").unwrap();
        let rejects: Vec<_> = mapped.rejected.iter().map(|r| (r.row, r.code.as_str(), r.field.as_deref().unwrap())).collect();
        assert_eq!(
            rejects,
            vec![
                (1, "missing_employee_id", "employee_id"),
                (2, "bad_date", "date_of_birth"),
                (3, "unknown_value", "sex"),
                (4, "unknown_value", "category"),
                (5, "bad_number", "salary"),
                (6, "bad_number", "hours"),
                (7, "bad_date", "employment_start"),
                (8, "bad_boolean", "actively_at_work"),
                (9, "bad_number", "salary"),
            ]
        );
        assert_eq!(mapped.rejected[1].employee_id.as_deref(), Some("E1"));
        assert_eq!(mapped.rows.len(), 1);
        assert_eq!(mapped.rows[0].attrs.actively_at_work, Some(false));
        assert!(!serde_json::to_string(&mapped.rejected).unwrap().contains("Contractor"));

        // A change file row with no event, or an event the mapping does not know.
        let c = change_mapping();
        let text = "id;given;family;born;smoker;category;pay;event;from;ird\nE1;;;;;;;;2027-12-01;\nE2;;;;;;;HIRE;2027-12-01;\nE3;;;;;;12x;UPD;2027-12-01;\nE4;;;;;;;UPD;1 Dec;\nE5;;;;perhaps;;;UPD;2027-12-01;\n";
        let mapped = c.map(&csv::parse(text.as_bytes(), ';').unwrap(), "salt").unwrap();
        let rejects: Vec<_> = mapped.rejected.iter().map(|r| (r.code.as_str(), r.field.as_deref().unwrap())).collect();
        assert_eq!(
            rejects,
            vec![
                ("missing_value", "change"),
                ("unknown_value", "change"),
                ("bad_number", "salary"),
                ("bad_date", "effective"),
                ("bad_boolean", "smoker")
            ]
        );
    }

    /// Purpose: a file whose header lacks a mapped column, repeats one, or is missing altogether is
    /// refused whole, and headers are matched without regard to case or spacing.
    /// Why it matters: when an employer changes their export, columns move. Reading the wrong
    /// column as salary is far worse than refusing the file and asking for the mapping to be
    /// updated.
    #[test]
    fn a_file_that_does_not_fit_the_mapping_is_refused_whole() {
        let m = full_mapping();
        assert_eq!(m.map(&[], "s"), Err(MappingError::NoHeader));
        let missing = csv::parse(b"Emp No,First Name\nE1,A\n", ',').unwrap();
        assert_eq!(m.map(&missing, "s"), Err(MappingError::MissingColumn("Surname".to_string())));
        let no_id = csv::parse(b"First Name\nA\n", ',').unwrap();
        assert_eq!(m.map(&no_id, "s"), Err(MappingError::MissingColumn("Emp No".to_string())));
        let twice = csv::parse(b"Emp No,emp no\nE1,E1\n", ',').unwrap();
        assert_eq!(m.map(&twice, "s"), Err(MappingError::DuplicateColumn("emp no".to_string())));
        let shouting = FULL_SAMPLE.replacen("Emp No,First Name", " EMP NO ,first name", 1);
        let mapped = m.map(&csv::parse(shouting.as_bytes(), ',').unwrap(), "s").unwrap();
        assert_eq!(mapped.rows.len(), 3);
        assert_eq!(
            MappingError::MissingColumn("Site".to_string()).to_string(),
            "the file has no column headed \"Site\""
        );
    }

    /// Purpose: a mapping is refused when sealed if it has no ID, no employee ID column, a date
    /// format that is not a whole date, or is a change mapping without change values; a sealed
    /// mapping's hash covers its content.
    /// Why it matters: a mapping is activated once and then trusted for every file. A date format
    /// of month and year alone would parse nothing, or worse, parse wrongly; the hash is what shows
    /// which mapping version read a given file.
    #[test]
    fn mappings_are_checked_and_hashed_when_sealed() {
        let good = full_mapping();
        assert!(good.is_sealed());
        let mut edited = good.clone();
        edited.salary_period = SalaryPeriod::Weekly;
        assert!(!edited.is_sealed());
        assert_ne!(edited.clone().seal().unwrap().content_hash, good.content_hash);
        let unsealed = FileMapping {
            content_hash: String::new(),
            ..good.clone()
        };
        assert!(!unsealed.is_sealed());

        let broken: Vec<Box<dyn Fn(&mut FileMapping)>> = vec![
            Box::new(|m| m.id = String::new()),
            Box::new(|m| m.version = 0),
            Box::new(|m| m.columns.employee_id = " ".to_string()),
            Box::new(|m| m.date_format = "%m/%Y".to_string()),
            Box::new(|m| m.date_format = "%Q".to_string()),
            Box::new(|m| m.kind = FileKind::Changes),
        ];
        for breakage in broken {
            let mut m = good.clone();
            breakage(&mut m);
            assert!(matches!(m.seal(), Err(MappingError::Invalid(_))));
        }
        let mut no_values = change_mapping();
        no_values.change_values.clear();
        assert!(no_values.seal().unwrap_err().to_string().contains("change column"));
        // The delimiter defaults to a comma when a stored mapping leaves it out.
        let mut json = serde_json::to_value(&good).unwrap();
        json.as_object_mut().unwrap().remove("delimiter");
        assert_eq!(serde_json::from_value::<FileMapping>(json).unwrap().delimiter, ',');
    }

    /// Purpose: amounts parse with currency signs, separators and up to two decimals; anything
    /// else, including negatives and overflow, is refused; each pay period annualises.
    /// Why it matters: salary drives the sum insured. "85,000" read as 85 or a weekly figure taken
    /// as annual is a wrong benefit on a death claim.
    #[test]
    fn amounts_parse_exactly_or_not_at_all() {
        assert_eq!(parse_hundredths("$85,000.00"), Some(85_000_00));
        assert_eq!(parse_hundredths("85 000"), Some(85_000_00));
        assert_eq!(parse_hundredths("37.5"), Some(37_50));
        assert_eq!(parse_hundredths(".5"), Some(50));
        assert_eq!(parse_hundredths("12."), Some(12_00));
        for bad in ["", ".", "-1", "1.234", "1e5", "abc", "99999999999999999999", "92233720368547758.08"] {
            assert_eq!(parse_hundredths(bad), None, "{bad}");
        }
        assert_eq!(
            [SalaryPeriod::Annual, SalaryPeriod::Monthly, SalaryPeriod::Fortnightly, SalaryPeriod::Weekly].map(SalaryPeriod::per_year),
            [1, 12, 26, 52]
        );
    }
}
