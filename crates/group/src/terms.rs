//! A scheme's negotiated terms. They are commercial and vary by scheme, so
//! they are data on the scheme, set when it is established and amended by
//! command with an effective date.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// How members are held under the scheme's covers. One of the three kinds of
/// life record in the policy model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberBasis {
    /// Each member is a party in the customer master.
    Named,
    /// Each member has a record with rating attributes and no identity.
    Unnamed,
    /// Aggregates per category, declared by the employer.
    Pool,
}

/// When a leaver's cover ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverEnd {
    LeavingDate,
    /// The first day of the month after the leaving date.
    EndOfMonth,
}

/// What happens when no renewal is agreed by the renewal date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoAgreement {
    ContinueOnOldRates { days: u32 },
    End,
}

/// What happens to cover that takes a life over the insurer's retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcessPath {
    /// The reinsurance module cedes the excess facultatively.
    Facultative,
    /// The member's cover above the retained amount is underwritten.
    UnderwrittenExcess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Eligibility {
    /// Days of service before cover can start.
    #[serde(default)]
    pub waiting_days: u32,
    #[serde(default)]
    pub min_age: Option<u32>,
    /// Cover cannot start at or above this age.
    #[serde(default)]
    pub max_age: Option<u32>,
    /// Minimum weekly hours, in hundredths of an hour.
    #[serde(default)]
    pub min_hours_hundredths: Option<u32>,
    /// The member must be actively at work on the day cover starts.
    #[serde(default)]
    pub actively_at_work: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationTerms {
    /// How long a leaver has to take up an individual policy.
    pub take_up_days: u32,
    /// The retail products a leaver may continue onto.
    pub products: Vec<String>,
    /// No offer is made to a leaver at or above this age.
    #[serde(default)]
    pub max_age: Option<u32>,
}

/// Continuity of cover for members transferring from a previous insurer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakeoverTerms {
    pub previous_insurer: String,
    /// The day this insurer came on risk.
    pub effective: NaiveDate,
    /// Members employed before the takeover have served their waiting period.
    #[serde(default)]
    pub waive_waiting_period: bool,
    /// Cover above the automatic acceptance limit that the previous insurer
    /// had accepted is honoured without evidence up to this amount.
    #[serde(default)]
    pub honour_excess_up_to_minor: Option<i64>,
    /// Recorded on every decision honoured under these terms.
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingGroup {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemeTerms {
    pub basis: MemberBasis,
    /// The first day of the current scheme year.
    pub scheme_year_start: NaiveDate,
    pub renewal_date: NaiveDate,
    /// Rates are guaranteed until this date: no renewal re-rate before it.
    #[serde(default)]
    pub rate_guarantee_until: Option<NaiveDate>,
    /// How far back the employer may date a change. Beyond it a change needs
    /// staff approval; the core's own limit is absolute.
    pub backdating_limit_days: u32,
    /// A full file that would remove more than this share of the membership
    /// is held for explicit confirmation.
    pub safety_share_percent: u8,
    pub cover_end: CoverEnd,
    pub eligibility: Eligibility,
    pub continuation: ContinuationTerms,
    /// Cover continues this many days into a leave of absence, then ends.
    pub leave_of_absence_days: u32,
    #[serde(default)]
    pub takeover: Option<TakeoverTerms>,
    /// Subsidiaries, sites or cost centres with their own invoice split.
    pub billing_groups: Vec<BillingGroup>,
    pub excess_path: ExcessPath,
    /// Headcount per site: many lives in one building is a catastrophe
    /// exposure the per-life view cannot see.
    #[serde(default)]
    pub sites: Vec<(String, u32)>,
    pub no_agreement: NoAgreement,
    /// The renewal workflow starts this many days before the renewal date.
    pub renewal_lead_days: u32,
    /// A year-end declaration is due this many days after the scheme year ends.
    pub declaration_due_days: u32,
    /// A file not approved or applied after this many days is a break.
    pub file_max_age_days: u32,
    /// A file needs approval by someone other than whoever uploaded it.
    #[serde(default)]
    pub second_approver: bool,
}

/// The core's own backdating limit: nothing can be dated further back.
pub const CORE_BACKDATING_LIMIT_DAYS: u32 = 400;

impl SchemeTerms {
    /// Check the terms are usable. Returns what is wrong, in words.
    pub fn validate(&self) -> Result<(), String> {
        if self.renewal_date <= self.scheme_year_start {
            return Err("the renewal date must be after the start of the scheme year".to_string());
        }
        if self.backdating_limit_days > CORE_BACKDATING_LIMIT_DAYS {
            return Err(format!("the backdating limit cannot exceed {CORE_BACKDATING_LIMIT_DAYS} days"));
        }
        if self.safety_share_percent == 0 || self.safety_share_percent > 100 {
            return Err("the safety share must be between 1 and 100 percent".to_string());
        }
        if self.billing_groups.is_empty() {
            return Err("a scheme needs at least one billing group".to_string());
        }
        let mut codes: Vec<&str> = self.billing_groups.iter().map(|g| g.code.as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        if codes.len() != self.billing_groups.len() || codes.iter().any(|c| c.is_empty()) {
            return Err("billing group codes must be unique and not empty".to_string());
        }
        if self.eligibility.min_age.zip(self.eligibility.max_age).is_some_and(|(lo, hi)| lo >= hi) {
            return Err("the minimum age must be below the maximum age".to_string());
        }
        if self.file_max_age_days == 0 {
            return Err("the file age limit must be at least one day".to_string());
        }
        Ok(())
    }

    pub fn has_billing_group(&self, code: &str) -> bool {
        self.billing_groups.iter().any(|g| g.code == code)
    }

    /// The billing group a member falls in when the file names none.
    pub fn default_billing_group(&self) -> &str {
        self.billing_groups.first().map_or("", |g| g.code.as_str())
    }

    /// The day a leaver's cover ends, given the day they left.
    pub fn cover_end_for(&self, left_on: NaiveDate) -> NaiveDate {
        use chrono::Datelike;
        match self.cover_end {
            CoverEnd::LeavingDate => left_on,
            CoverEnd::EndOfMonth => {
                let (y, m) = if left_on.month() == 12 {
                    (left_on.year() + 1, 1)
                } else {
                    (left_on.year(), left_on.month() + 1)
                };
                NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(left_on)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    pub(crate) fn sample_terms() -> SchemeTerms {
        SchemeTerms {
            basis: MemberBasis::Named,
            scheme_year_start: d(2027, 11, 1),
            renewal_date: d(2028, 11, 1),
            rate_guarantee_until: None,
            backdating_limit_days: 90,
            safety_share_percent: 20,
            cover_end: CoverEnd::LeavingDate,
            eligibility: Eligibility {
                waiting_days: 0,
                min_age: Some(16),
                max_age: Some(70),
                min_hours_hundredths: Some(2000),
                actively_at_work: true,
            },
            continuation: ContinuationTerms {
                take_up_days: 60,
                products: vec!["term-life".to_string()],
                max_age: Some(65),
            },
            leave_of_absence_days: 90,
            takeover: None,
            billing_groups: vec![
                BillingGroup {
                    code: "akl".to_string(),
                    name: "Auckland".to_string(),
                },
                BillingGroup {
                    code: "wlg".to_string(),
                    name: "Wellington".to_string(),
                },
            ],
            excess_path: ExcessPath::UnderwrittenExcess,
            sites: vec![("akl-hq".to_string(), 120)],
            no_agreement: NoAgreement::ContinueOnOldRates { days: 60 },
            renewal_lead_days: 90,
            declaration_due_days: 30,
            file_max_age_days: 14,
            second_approver: false,
        }
    }

    /// Purpose: every rule in the terms check refuses the terms it is there to refuse, and accepts
    /// the sample terms.
    /// Why it matters: scheme terms drive eligibility, the file safety check and billing splits. Terms
    /// that contradict themselves must be refused when the scheme is set up, not found when a file
    /// misbehaves.
    #[test]
    fn terms_are_checked_when_set() {
        let good = sample_terms();
        assert_eq!(good.validate(), Ok(()));
        let broken: Vec<(&str, Box<dyn Fn(&mut SchemeTerms)>)> = vec![
            ("renewal date", Box::new(|t| t.renewal_date = t.scheme_year_start)),
            ("backdating limit", Box::new(|t| t.backdating_limit_days = 401)),
            ("safety share", Box::new(|t| t.safety_share_percent = 0)),
            ("safety share", Box::new(|t| t.safety_share_percent = 101)),
            ("billing group", Box::new(|t| t.billing_groups.clear())),
            ("unique", Box::new(|t| t.billing_groups[1].code = "akl".to_string())),
            ("unique", Box::new(|t| t.billing_groups[1].code = String::new())),
            ("minimum age", Box::new(|t| t.eligibility.min_age = Some(70))),
            ("file age", Box::new(|t| t.file_max_age_days = 0)),
        ];
        for (expect, breakage) in broken {
            let mut t = sample_terms();
            breakage(&mut t);
            let err = t.validate().unwrap_err();
            assert!(err.contains(expect), "{err} should mention {expect}");
        }
    }

    /// Purpose: a leaver's cover ends on the leaving date or on the first of the next month, as the
    /// scheme's terms say, including across a year end.
    /// Why it matters: the end date decides whether a claim in the member's last weeks is payable and
    /// how much premium the employer is refunded.
    #[test]
    fn cover_ends_as_the_terms_say() {
        let mut t = sample_terms();
        assert_eq!(t.cover_end_for(d(2027, 12, 14)), d(2027, 12, 14));
        t.cover_end = CoverEnd::EndOfMonth;
        assert_eq!(t.cover_end_for(d(2027, 11, 14)), d(2027, 12, 1));
        assert_eq!(t.cover_end_for(d(2027, 12, 14)), d(2028, 1, 1));
        assert!(t.has_billing_group("wlg") && !t.has_billing_group("chc"));
        assert_eq!(t.default_billing_group(), "akl");
    }
}
