//! Prints the shipped Starlark configuration as JSON. See `Cargo.toml`.

use serde_json::{json, Value};

fn value<T: serde::Serialize>(what: &str, loaded: Result<T, impl std::fmt::Display>) -> Result<Value, String> {
    let loaded = loaded.map_err(|e| format!("{what}: {e}"))?;
    serde_json::to_value(loaded).map_err(|e| format!("{what}: {e}"))
}

/// A group scheme as group administration registers it: the policy new business issues
/// (one founding member, a named life), the scheme's terms on the shipped group life
/// product, and the layout of the employer's membership file. The seeding changes the
/// IDs and the people; the terms are the ones the group module's own tests work under.
fn group(opening: chrono::NaiveDate) -> Result<Value, String> {
    use ic_group::mapping::{Columns, FileKind, FileMapping, SalaryPeriod, SalaryUnit};
    use ic_group::terms::{BillingGroup, ContinuationTerms, CoverEnd, Eligibility, ExcessLoading, ExcessPath, MemberBasis, NoAgreement, SchemeTerms};
    let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).ok_or("a date");
    let year_later = opening.checked_add_months(chrono::Months::new(12)).ok_or("a renewal date")?;
    let product =
        ic_product::load_product("group_life.star", include_str!("../../../products/group_life.star")).map_err(|e| format!("group_life.star: {e}"))?;
    let founder = ic_group::record::MemberAttrs {
        date_of_birth: Some(day(1980, 5, 1)?),
        sex: None,
        smoker: None,
        category: Some("staff".to_string()),
        salary_minor: Some(60_000_00),
        hours_hundredths: Some(4000),
        employment_start: Some(day(2020, 2, 1)?),
        actively_at_work: Some(true),
        billing_group: Some("akl".to_string()),
        occupation_class: None,
    };
    let life = ic_core::LifeId("E001".to_string());
    let party = ic_core::PartyId("party-e001".to_string());
    let issue = ic_sim::persona::group_scheme(
        6,
        opening,
        vec![ic_core::command::LifeSpec {
            id: life.clone(),
            kind: ic_core::LifeKind::Named { party: party.clone() },
            factors: founder.factors(),
        }],
    );
    let terms = SchemeTerms {
        basis: MemberBasis::Named,
        scheme_year_start: opening,
        renewal_date: year_later,
        rate_guarantee_until: None,
        backdating_limit_days: 90,
        safety_share_percent: 20,
        cover_end: CoverEnd::LeavingDate,
        eligibility: Eligibility {
            waiting_days: 0,
            min_age: Some(16),
            max_age: Some(70),
            min_hours_hundredths: None,
            actively_at_work: false,
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
        no_agreement: NoAgreement::ContinueOnOldRates { days: 30 },
        renewal_lead_days: 90,
        declaration_due_days: 30,
        file_max_age_days: 14,
        second_approver: false,
        excess_loading: ExcessLoading::EmployerTotal,
        excess_loading_min_count: 0,
        suspend_joiners_after_short_payments: Some(2),
        broker: None,
        delegated_underwriting: None,
        health_menu: None,
    };
    let mapping = FileMapping {
        id: "kai-fresh".to_string(),
        version: 1,
        kind: FileKind::Full,
        delimiter: ',',
        columns: Columns {
            employee_id: "Emp No".to_string(),
            given_name: Some("First".to_string()),
            family_name: Some("Last".to_string()),
            date_of_birth: Some("DOB".to_string()),
            category: Some("Category".to_string()),
            salary: Some("Salary".to_string()),
            billing_group: Some("Site".to_string()),
            ..Columns::default()
        },
        date_format: "%d/%m/%Y".to_string(),
        salary_unit: SalaryUnit::Major,
        salary_period: SalaryPeriod::Annual,
        categories: std::collections::BTreeMap::new(),
        change_values: std::collections::BTreeMap::new(),
        content_hash: String::new(),
    }
    .seal()
    .map_err(|e| format!("the file layout: {e}"))?;
    let member = ic_group::model::Member {
        employee_id: "E001".to_string(),
        life,
        attrs: founder,
        joined: opening,
        left: None,
        absence_since: None,
        party: Some(party),
        fingerprint: None,
        auto_cap_minor: None,
        health: None,
        dependants: vec![],
    };
    Ok(json!({ "issue": issue, "product": product, "terms": terms, "mapping": mapping, "founder": member }))
}

fn config() -> Result<Value, String> {
    let chart = ic_finance::Chart::load("chart.star", include_str!("../../../config/finance/chart.star")).map_err(|e| format!("chart.star: {e}"))?;
    let close = ic_finance::CloseConfig::load("close.star", include_str!("../../../config/finance/close.star")).map_err(|e| format!("close.star: {e}"))?;
    let (chart_id, close_id) = (
        format!("chart:{}:{}", chart.version, chart.content_hash),
        format!("close:{}:{}", close.version, close.content_hash),
    );
    let underwriting = ic_underwriting::shipped::load().map_err(|e| format!("underwriting: {e}"))?;
    let day = |y, m, d| chrono::NaiveDate::from_ymd_opt(y, m, d).ok_or("a date");
    let (opening, born) = (day(2027, 11, 15)?, day(1988, 6, 2)?);
    use ic_product::{Frequency, Sex};
    use ic_sim::persona;
    // One application of each shipped kind, in the API's own JSON. The seeding changes the IDs, people and amounts.
    let applications = json!({
        "term_life": persona::term_life(1, opening, born, Sex::Female, false, 500_000_00, Some(100_000_00), Frequency::Monthly, None),
        "income_protection": persona::income_protection(2, opening, born, Sex::Male, false, 6_000_00, Frequency::Monthly),
        "health_family": persona::health_family(3, opening, &[born, day(1990, 1, 20)?, day(2015, 3, 9)?], Frequency::Monthly),
        "invest_plan": persona::invest_plan(4, opening, born, Sex::Female),
        "group_scheme": persona::group_scheme(5, opening, vec![
            persona::member("m-1", day(1980, 1, 1)?, "staff", 80_000_00),
            persona::member("m-2", day(1975, 5, 5)?, "staff", 95_000_00),
            persona::member("m-3", day(1969, 9, 9)?, "managers", 160_000_00),
        ]),
    });
    Ok(json!({
        "applications": applications,
        "group": group(opening)?,
        "finance": { "chart": chart, "close": close, "chart_id": chart_id, "close_id": close_id },
        "underwriting": {
            "exclusions": underwriting.exclusions, "reasons": underwriting.reasons,
            "question_sets": underwriting.question_sets, "rule_sets": underwriting.rule_sets, "grades": underwriting.grades,
        },
        "reinsurance": {
            "treaties": [
                value("retail_life_surplus.star", ic_reinsurance::loader::load_treaty("retail_life_surplus.star", include_str!("../../../treaties/retail_life_surplus.star")))?,
                value("group_life_surplus.star", ic_reinsurance::loader::load_treaty("group_life_surplus.star", include_str!("../../../treaties/group_life_surplus.star")))?,
            ],
            "aggregate_treaties": [
                value("catastrophe_2027.star", ic_reinsurance::loader::load_aggregate_treaty("catastrophe_2027.star", include_str!("../../../treaties/catastrophe_2027.star")))?,
                value("stop_loss_2027.star", ic_reinsurance::loader::load_aggregate_treaty("stop_loss_2027.star", include_str!("../../../treaties/stop_loss_2027.star")))?,
            ],
            "rate_tables": [
                value("ri_life_base.star", ic_reinsurance::loader::load_rate_table("ri_life_base.star", include_str!("../../../treaties/rates/ri_life_base.star")))?,
            ],
        },
    }))
}

fn main() {
    match config() {
        Ok(config) => println!("{config}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
