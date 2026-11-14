//! Prints the shipped Starlark configuration as JSON. See `Cargo.toml`.

use serde_json::{json, Value};

fn value<T: serde::Serialize>(what: &str, loaded: Result<T, impl std::fmt::Display>) -> Result<Value, String> {
    let loaded = loaded.map_err(|e| format!("{what}: {e}"))?;
    serde_json::to_value(loaded).map_err(|e| format!("{what}: {e}"))
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
