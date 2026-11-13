//! What a member is covered for under the scheme's benefit formula, and the
//! eligibility rules checked on the day cover would start.

use chrono::NaiveDate;
use ic_money::{round_to_money, Dec};
use ic_product::schedule::age_on;
use ic_product::{CoverDefinition, PremiumType, ProductDefinition};
use serde::{Deserialize, Serialize};

use crate::record::MemberAttrs;
use crate::terms::SchemeTerms;

/// A member's position under one formula-driven cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverAmounts {
    pub cover: String,
    pub cover_type: String,
    /// What the formula gives: the salary multiple, to the category maximum.
    pub formula_minor: i64,
    /// What is covered without underwriting: the formula amount up to the
    /// automatic acceptance limit, and up to the member's own cap if one
    /// has been set.
    pub automatic_minor: i64,
}

impl CoverAmounts {
    /// Cover that exists only if it is individually underwritten.
    pub fn excess_minor(&self) -> i64 {
        self.formula_minor - self.automatic_minor
    }
}

/// The covers whose amount comes from a formula over member data.
pub fn formula_covers(product: &ProductDefinition) -> impl Iterator<Item = &CoverDefinition> {
    product.covers.iter().filter(|c| c.rating.premium_type == PremiumType::GroupUnitRate)
}

pub fn known_category(product: &ProductDefinition, category: &str) -> bool {
    formula_covers(product).any(|c| c.categories.iter().any(|k| k.code == category))
}

/// The lowest age at which any of the scheme's covers expires.
pub fn expiry_age(product: &ProductDefinition) -> Option<u32> {
    formula_covers(product).filter_map(|c| c.expiry_age).min()
}

/// A member's cover under each formula-driven cover. A cover whose formula
/// cannot be worked out for the member (no salary, a category the cover
/// does not have) is left out, as the core leaves it out.
pub fn member_cover(product: &ProductDefinition, attrs: &MemberAttrs, cap_minor: Option<i64>) -> Vec<CoverAmounts> {
    formula_covers(product)
        .filter_map(|def| {
            let category = def.categories.iter().find(|c| Some(&c.code) == attrs.category.as_ref())?;
            let salary = attrs.salary_minor?;
            let raw = round_to_money(
                Dec::from(salary) / Dec::from(100i64) * category.salary_multiple.get(),
                product.currency,
                product.rounding,
            )
            .ok()?;
            let formula_minor = raw.minor.min(category.max_sum_insured_minor);
            let mut automatic_minor = formula_minor;
            if let Some(limit) = def.automatic_acceptance_limit_minor {
                automatic_minor = automatic_minor.min(limit);
            }
            if let Some(cap) = cap_minor {
                automatic_minor = automatic_minor.min(cap.max(0));
            }
            Some(CoverAmounts {
                cover: def.code.clone(),
                cover_type: def.cover_type.clone(),
                formula_minor,
                automatic_minor,
            })
        })
        .collect()
}

/// Why a member cannot be covered from the date asked: a code and the field
/// it concerns.
pub type Ineligible = (&'static str, &'static str);

/// The eligibility rules for cover starting on `effective`: the member's
/// category and billing group exist, the waiting period is served, the
/// member is actively at work, within the age limits and works enough hours.
pub fn check_join(terms: &SchemeTerms, product: &ProductDefinition, attrs: &MemberAttrs, effective: NaiveDate) -> Result<(), Ineligible> {
    check_attrs(terms, product, attrs)?;
    let e = &terms.eligibility;
    if attrs.salary_minor.is_none() {
        return Err(("missing_value", "salary"));
    }
    let max_age = [e.max_age, expiry_age(product)].into_iter().flatten().min();
    if e.min_age.is_some() || max_age.is_some() {
        let dob = attrs.date_of_birth.ok_or(("missing_value", "date_of_birth"))?;
        let age = age_on(dob, effective);
        if e.min_age.is_some_and(|min| age < min) {
            return Err(("under_age", "date_of_birth"));
        }
        if max_age.is_some_and(|max| age >= max) {
            return Err(("over_age", "date_of_birth"));
        }
    }
    if e.waiting_days > 0 {
        let started = attrs.employment_start.ok_or(("missing_value", "employment_start"))?;
        let waived = terms.takeover.as_ref().is_some_and(|t| t.waive_waiting_period && started < t.effective);
        let served = started
            .checked_add_days(chrono::Days::new(u64::from(e.waiting_days)))
            .is_some_and(|end| end <= effective);
        if !waived && !served {
            return Err(("waiting_period", "employment_start"));
        }
    }
    if e.actively_at_work {
        match attrs.actively_at_work {
            Some(true) => {}
            Some(false) => return Err(("not_actively_at_work", "actively_at_work")),
            None => return Err(("missing_value", "actively_at_work")),
        }
    }
    if let Some(min) = e.min_hours_hundredths {
        let hours = attrs.hours_hundredths.ok_or(("missing_value", "hours"))?;
        if hours < min {
            return Err(("insufficient_hours", "hours"));
        }
    }
    Ok(())
}

/// The checks that apply to any member record, joining or changing.
pub fn check_attrs(terms: &SchemeTerms, product: &ProductDefinition, attrs: &MemberAttrs) -> Result<(), Ineligible> {
    let category = attrs.category.as_deref().ok_or(("missing_value", "category"))?;
    if !known_category(product, category) {
        return Err(("unknown_value", "category"));
    }
    if attrs.billing_group.as_deref().is_some_and(|g| !terms.has_billing_group(g)) {
        return Err(("unknown_value", "billing_group"));
    }
    if attrs.salary_minor.is_some_and(|s| s < 0) {
        return Err(("bad_number", "salary"));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::terms::tests::{d, sample_terms};
    use crate::terms::TakeoverTerms;

    pub(crate) fn group_life() -> ProductDefinition {
        ic_product::load_product("group_life.star", include_str!("../../../products/group_life.star")).unwrap()
    }

    pub(crate) fn attrs(category: &str, salary_minor: i64) -> MemberAttrs {
        MemberAttrs {
            date_of_birth: Some(d(1980, 5, 1)),
            sex: None,
            smoker: None,
            category: Some(category.to_string()),
            salary_minor: Some(salary_minor),
            hours_hundredths: Some(4000),
            employment_start: Some(d(2020, 2, 1)),
            actively_at_work: Some(true),
            billing_group: Some("akl".to_string()),
            occupation_class: None,
        }
    }

    /// Purpose: the formula amount is the salary multiple to the category maximum; the automatic
    /// amount stops at the automatic acceptance limit and at a member's own cap; the rest is
    /// excess. A member the formula cannot be worked out for has no cover listed.
    /// Why it matters: the split decides what is on risk the day the file is applied and what
    /// waits for underwriting. If the module and the core disagreed on it, the employer would be
    /// shown cover the core does not hold.
    #[test]
    fn cover_splits_into_automatic_and_excess() {
        let p = group_life();
        // Staff: twice salary, maximum $1,000,000; the limit is $750,000.
        let low = member_cover(&p, &attrs("staff", 80_000_00), None);
        assert_eq!(
            (low[0].formula_minor, low[0].automatic_minor, low[0].excess_minor()),
            (160_000_00, 160_000_00, 0)
        );
        assert_eq!((low[0].cover.as_str(), low[0].cover_type.as_str()), ("group-life", "group_life"));
        let over = member_cover(&p, &attrs("staff", 450_000_00), None);
        assert_eq!(
            (over[0].formula_minor, over[0].automatic_minor, over[0].excess_minor()),
            (900_000_00, 750_000_00, 150_000_00)
        );
        // Executive: four times salary, maximum $3,000,000.
        let exec = member_cover(&p, &attrs("executive", 900_000_00), None);
        assert_eq!((exec[0].formula_minor, exec[0].automatic_minor), (3_000_000_00, 750_000_00));
        // A member's own cap lowers the automatic amount, never below zero.
        let capped = member_cover(&p, &attrs("staff", 300_000_00), Some(400_000_00));
        assert_eq!((capped[0].automatic_minor, capped[0].excess_minor()), (400_000_00, 200_000_00));
        assert_eq!(member_cover(&p, &attrs("staff", 300_000_00), Some(-1))[0].automatic_minor, 0);
        assert!(member_cover(&p, &attrs("contractor", 80_000_00), None).is_empty());
        let no_salary = MemberAttrs {
            salary_minor: None,
            ..attrs("staff", 0)
        };
        assert!(member_cover(&p, &no_salary, None).is_empty());
        assert!(known_category(&p, "executive") && !known_category(&p, "contractor"));
        assert_eq!(expiry_age(&p), Some(70));
    }

    /// Purpose: each eligibility rule refuses the member it should, with a code and the field
    /// concerned, and a member who meets every rule is accepted.
    /// Why it matters: cover below the automatic acceptance limit is granted with no underwriting,
    /// so eligibility is the only gate. A member let in while off sick, or before their waiting
    /// period ends, is a claim the scheme was never priced for.
    #[test]
    fn each_eligibility_rule_refuses_its_own_case() {
        let p = group_life();
        let mut t = sample_terms();
        t.eligibility.waiting_days = 30;
        let on = d(2027, 12, 1);
        let ok = attrs("staff", 80_000_00);
        assert_eq!(check_join(&t, &p, &ok, on), Ok(()));
        let cases: Vec<(Box<dyn Fn(&mut MemberAttrs)>, Ineligible)> = vec![
            (Box::new(|a| a.category = None), ("missing_value", "category")),
            (Box::new(|a| a.category = Some("contractor".to_string())), ("unknown_value", "category")),
            (Box::new(|a| a.billing_group = Some("chc".to_string())), ("unknown_value", "billing_group")),
            (Box::new(|a| a.salary_minor = Some(-1)), ("bad_number", "salary")),
            (Box::new(|a| a.salary_minor = None), ("missing_value", "salary")),
            (Box::new(|a| a.date_of_birth = None), ("missing_value", "date_of_birth")),
            (Box::new(|a| a.date_of_birth = Some(d(2012, 1, 1))), ("under_age", "date_of_birth")),
            (Box::new(|a| a.date_of_birth = Some(d(1957, 12, 1))), ("over_age", "date_of_birth")),
            (Box::new(|a| a.employment_start = None), ("missing_value", "employment_start")),
            (Box::new(|a| a.employment_start = Some(d(2027, 11, 15))), ("waiting_period", "employment_start")),
            (Box::new(|a| a.actively_at_work = Some(false)), ("not_actively_at_work", "actively_at_work")),
            (Box::new(|a| a.actively_at_work = None), ("missing_value", "actively_at_work")),
            (Box::new(|a| a.hours_hundredths = None), ("missing_value", "hours")),
            (Box::new(|a| a.hours_hundredths = Some(1999)), ("insufficient_hours", "hours")),
        ];
        for (change, expect) in cases {
            let mut a = ok.clone();
            change(&mut a);
            assert_eq!(check_join(&t, &p, &a, on), Err(expect));
        }
        // The day the waiting period ends is the first day of cover.
        let mut new_start = ok.clone();
        new_start.employment_start = Some(d(2027, 11, 1));
        assert_eq!(check_join(&t, &p, &new_start, d(2027, 12, 1)), Ok(()));
        assert_eq!(check_join(&t, &p, &new_start, d(2027, 11, 30)), Err(("waiting_period", "employment_start")));
        // The day before the 70th birthday is the last day cover can start.
        let mut older = ok.clone();
        older.date_of_birth = Some(d(1957, 12, 2));
        assert_eq!(check_join(&t, &p, &older, on), Ok(()));
    }

    /// Purpose: with no age, hours, waiting or at-work rules, a member needs only a category and
    /// a salary; and takeover terms that waive the waiting period do so only for members employed
    /// before the takeover date.
    /// Why it matters: schemes on the unnamed basis often hold no dates of birth, and must not be
    /// refused for lacking them. On a takeover, existing staff keep continuous cover while new
    /// starters still wait.
    #[test]
    fn rules_apply_only_where_the_terms_set_them() {
        let mut p = group_life();
        for c in &mut p.covers {
            c.expiry_age = None;
        }
        let mut t = sample_terms();
        t.eligibility = crate::terms::Eligibility {
            waiting_days: 0,
            min_age: None,
            max_age: None,
            min_hours_hundredths: None,
            actively_at_work: false,
        };
        let bare = MemberAttrs {
            category: Some("staff".to_string()),
            salary_minor: Some(50_000_00),
            ..MemberAttrs::default()
        };
        assert_eq!(check_join(&t, &p, &bare, d(2027, 12, 1)), Ok(()));
        assert_eq!(expiry_age(&p), None);

        t.eligibility.waiting_days = 90;
        t.takeover = Some(TakeoverTerms {
            previous_insurer: "Southern Mutual".to_string(),
            effective: d(2027, 11, 1),
            waive_waiting_period: true,
            honour_excess_up_to_minor: None,
            reason_code: "takeover-2027".to_string(),
        });
        let mut existing = bare.clone();
        existing.employment_start = Some(d(2027, 10, 20));
        assert_eq!(check_join(&t, &p, &existing, d(2027, 11, 1)), Ok(()));
        let mut starter = bare.clone();
        starter.employment_start = Some(d(2027, 11, 1));
        assert_eq!(check_join(&t, &p, &starter, d(2027, 11, 10)), Err(("waiting_period", "employment_start")));
        t.takeover.as_mut().unwrap().waive_waiting_period = false;
        assert_eq!(check_join(&t, &p, &existing, d(2027, 11, 1)), Err(("waiting_period", "employment_start")));
    }
}
