//! A scheme's negotiated terms. They are commercial and vary by scheme, so
//! they are data on the scheme, set when it is established and amended by
//! command with an effective date.

use std::collections::BTreeMap;

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

/// Who a member's health plan covers. The employer sees the tier, because
/// it sets the premium, and never who the dependants are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthTier {
    Single,
    Couple,
    Family,
}

/// One health plan on a scheme's menu: a set of the product's covers, each
/// with the excess level chosen for the scheme where the cover offers one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthPlan {
    pub code: String,
    pub name: String,
    pub covers: Vec<MenuCover>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuCover {
    pub cover: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excess_option: Option<String>,
}

/// The plans a scheme's members choose from, the plan a member is on until
/// they choose, and the tiers offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthMenu {
    pub plans: Vec<HealthPlan>,
    pub default_plan: String,
    pub tiers: Vec<HealthTier>,
    /// How many days back a member's choice may take effect.
    #[serde(default)]
    pub choice_backdating_days: u32,
}

impl HealthMenu {
    pub fn plan(&self, code: &str) -> Option<&HealthPlan> {
        self.plans.iter().find(|p| p.code == code)
    }

    /// The menu is whole: plans with distinct codes and at least one cover
    /// each, no cover twice in a plan, the default among them, and tiers
    /// starting with single.
    pub fn validate(&self) -> Result<(), String> {
        let mut codes = std::collections::BTreeSet::new();
        for p in &self.plans {
            if p.code.trim().is_empty() || !codes.insert(p.code.as_str()) {
                return Err(format!("health plan codes must be present and distinct: {:?}", p.code));
            }
            let covers: std::collections::BTreeSet<&str> = p.covers.iter().map(|c| c.cover.as_str()).collect();
            if p.covers.is_empty() || covers.len() != p.covers.len() {
                return Err(format!("health plan {} needs at least one cover, each once", p.code));
            }
        }
        if self.plan(&self.default_plan).is_none() {
            return Err(format!("the default health plan {} is not on the menu", self.default_plan));
        }
        if !self.tiers.contains(&HealthTier::Single) {
            return Err("a health menu offers the single tier".to_string());
        }
        Ok(())
    }

    /// Every cover a plan names is one of the product's.
    pub fn check_product(&self, product: &ic_product::ProductDefinition) -> Result<(), String> {
        for p in &self.plans {
            for c in &p.covers {
                let def = product
                    .cover(&c.cover)
                    .ok_or_else(|| format!("health plan {} names {}, which {} does not have", p.code, c.cover, product.code))?;
                if let Some(option) = &c.excess_option {
                    if !def.excess_options.iter().any(|o| &o.code == option) {
                        return Err(format!("{} has no excess option {option}", c.cover));
                    }
                }
            }
        }
        Ok(())
    }
}

/// A delegated underwriting authority the scheme's excess goes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedUnderwriting {
    /// The underwriting module's `DelegatedAuthority` id.
    pub authority: String,
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

/// Who pays the loading on a member's underwritten cover, and what the
/// employer sees of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcessLoading {
    /// The employer pays and sees one total, with no member breakdown.
    #[default]
    EmployerTotal,
    /// The employer pays and sees it on the member's line. Needs the
    /// member's consent, recorded when the cover is accepted.
    EmployerItemised,
    /// The member pays the loading as voluntary cover.
    MemberPays,
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
    /// A leaver may also continue the cover underwriting accepted above the
    /// automatic limit, on the terms it was accepted on and without new
    /// evidence. Off unless the scheme agreed it (Mitchell, 14 Nov 2026).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underwritten_excess: bool,
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
    /// How the loading on underwritten cover is billed.
    #[serde(default)]
    pub excess_loading: ExcessLoading,
    /// If fewer members than this hold underwritten cover when a member's
    /// terms are accepted, that member's loading is billed to the member
    /// instead, so that a total cannot be traced to a person. Zero is no
    /// minimum. Only where the employer pays loadings as one total.
    #[serde(default)]
    pub excess_loading_min_count: u32,
    /// After this many short payments in a row the scheme stops taking new
    /// joiners until it has paid in full. Existing members are not touched.
    #[serde(default)]
    pub suspend_joiners_after_short_payments: Option<u32>,
    /// The broker who placed the scheme, for commission. Absent on a scheme
    /// placed direct, which earns no commission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker: Option<SchemeBroker>,
    /// The employer's broker (or another delegate) underwrites members'
    /// cover above the automatic limit under this authority, which the
    /// underwriting module holds and checks every decision against
    /// (Mitchell, 14 Nov 2026). Absent: our underwriters decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_underwriting: Option<DelegatedUnderwriting>,
    /// The health plans members choose from (Mitchell, 14 Nov 2026).
    /// Absent on a scheme with no health cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_menu: Option<HealthMenu>,
}

/// The broker who placed a scheme, as the commission module knows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemeBroker {
    /// The broker's engagement with the insurer.
    pub engagement: String,
    /// The book the scheme's commission is earned in.
    pub book: String,
    /// The commission schedule and option agreed for the scheme.
    pub schedule: String,
    pub option: String,
    /// The scheme's expected annual premium for each cover driven by
    /// member data, by cover code: what initial commission is worked from.
    pub expected_annual_premium_minor: BTreeMap<String, i64>,
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
        if let Some(menu) = &self.health_menu {
            menu.validate()?;
        }
        if self.delegated_underwriting.as_ref().is_some_and(|d| d.authority.trim().is_empty()) {
            return Err("a delegated authority needs the underwriting module's authority id".to_string());
        }
        if let Some(b) = &self.broker {
            if [&b.engagement, &b.book, &b.schedule, &b.option].iter().any(|v| v.trim().is_empty()) {
                return Err("a broker needs an engagement, a book, a schedule and an option".to_string());
            }
            if b.expected_annual_premium_minor.is_empty() || b.expected_annual_premium_minor.values().any(|m| *m <= 0) {
                return Err("a broker's expected annual premium is positive for each cover".to_string());
            }
        }
        if self.eligibility.min_age.zip(self.eligibility.max_age).is_some_and(|(lo, hi)| lo >= hi) {
            return Err("the minimum age must be below the maximum age".to_string());
        }
        if self.file_max_age_days == 0 {
            return Err("the file age limit must be at least one day".to_string());
        }
        // The count protects a total from being traced to a person. Where
        // the member pays anyway, or each member is itemised with consent,
        // there is no total for it to protect.
        if self.excess_loading_min_count > 0 && self.excess_loading != ExcessLoading::EmployerTotal {
            return Err("the minimum count applies only where the employer pays loadings as one total".to_string());
        }
        if self.suspend_joiners_after_short_payments == Some(0) {
            return Err("the short payment count must be at least one".to_string());
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
                underwritten_excess: false,
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
            excess_loading: ExcessLoading::EmployerTotal,
            excess_loading_min_count: 0,
            suspend_joiners_after_short_payments: Some(3),
            broker: None,
            delegated_underwriting: None,
            health_menu: None,
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
            (
                "minimum count",
                Box::new(|t| {
                    t.excess_loading = ExcessLoading::MemberPays;
                    t.excess_loading_min_count = 3;
                }),
            ),
            (
                "minimum count",
                Box::new(|t| {
                    t.excess_loading = ExcessLoading::EmployerItemised;
                    t.excess_loading_min_count = 3;
                }),
            ),
            ("short payment count", Box::new(|t| t.suspend_joiners_after_short_payments = Some(0))),
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

    fn plan(code: &str, covers: &[(&str, Option<&str>)]) -> HealthPlan {
        HealthPlan {
            code: code.into(),
            name: code.into(),
            covers: covers
                .iter()
                .map(|(c, o)| MenuCover {
                    cover: (*c).into(),
                    excess_option: o.map(str::to_string),
                })
                .collect(),
        }
    }

    /// Purpose: a health menu is accepted whole or not at all: plans with distinct codes, each
    /// with covers named once, the default plan on the menu and the single tier offered; and
    /// every cover and excess level a plan names is one of the product's.
    /// Why it matters: a member can only choose from the menu. A plan naming a cover the product
    /// lacks, or a default that is not a plan, would leave members on cover nobody can price.
    #[test]
    fn a_health_menu_is_checked_whole_and_against_the_product() {
        let product = ic_product::load_product("health_flex.star", include_str!("../../../products/health_flex.star")).unwrap();
        let menu = HealthMenu {
            plans: vec![
                plan("everyday", &[("dental", None), ("optical", None)]),
                plan("hospital-plus", &[("hospital", Some("high")), ("specialist", None)]),
            ],
            default_plan: "everyday".into(),
            tiers: vec![HealthTier::Single, HealthTier::Family],
            choice_backdating_days: 30,
        };
        menu.validate().unwrap();
        menu.check_product(&product).unwrap();
        let mut t = sample_terms();
        t.health_menu = Some(menu.clone());
        t.validate().unwrap();

        let broken = |f: &dyn Fn(&mut HealthMenu)| {
            let mut m = menu.clone();
            f(&mut m);
            m.validate().is_err()
        };
        assert!(broken(&|m| m.plans[1].code = "everyday".into()));
        assert!(broken(&|m| m.plans[0].covers.clear()));
        assert!(broken(&|m| m.plans[0].covers.push(MenuCover {
            cover: "dental".into(),
            excess_option: None
        })));
        assert!(broken(&|m| m.default_plan = "gold".into()));
        assert!(broken(&|m| m.tiers = vec![HealthTier::Family]));
        let mut t = sample_terms();
        let mut bad = menu.clone();
        bad.default_plan = "gold".into();
        t.health_menu = Some(bad);
        assert!(t.validate().is_err());

        let mut unknown = menu.clone();
        unknown.plans[0].covers[0].cover = "maternity".into();
        assert!(unknown.check_product(&product).is_err());
        let mut level = menu;
        level.plans[1].covers[0].excess_option = Some("platinum".into());
        assert!(level.check_product(&product).is_err());
    }
}
