//! The configuration shipped in this repository, loaded through the same
//! Starlark pipeline production uses.

use ic_core::invest::{load_fund, FundDefinition, InvestCommand};
use ic_core::Command;
use ic_money::DecStr;
use ic_product::{load_definitions, load_product, load_rate_table, LoadError};

const PRODUCTS: [(&str, &str); 7] = [
    ("income_protection.star", include_str!("../../../products/income_protection.star")),
    ("term_life.star", include_str!("../../../products/term_life.star")),
    ("interim_accident.star", include_str!("../../../products/interim_accident.star")),
    ("health_plan.star", include_str!("../../../products/health_plan.star")),
    ("group_life.star", include_str!("../../../products/group_life.star")),
    ("invest_plan.star", include_str!("../../../products/invest_plan.star")),
    ("health_flex.star", include_str!("../../../products/health_flex.star")),
];
const RATES: [(&str, &str); 10] = [
    ("rates/income_base.star", include_str!("../../../products/rates/income_base.star")),
    ("rates/life_base.star", include_str!("../../../products/rates/life_base.star")),
    ("rates/trauma_base.star", include_str!("../../../products/rates/trauma_base.star")),
    ("rates/health_base.star", include_str!("../../../products/rates/health_base.star")),
    ("rates/no_premium.star", include_str!("../../../products/rates/no_premium.star")),
    ("rates/flex_hospital.star", include_str!("../../../products/rates/flex_hospital.star")),
    ("rates/flex_specialist.star", include_str!("../../../products/rates/flex_specialist.star")),
    ("rates/flex_dental.star", include_str!("../../../products/rates/flex_dental.star")),
    ("rates/flex_optical.star", include_str!("../../../products/rates/flex_optical.star")),
    (
        "rates/group_life_standard.star",
        include_str!("../../../products/rates/group_life_standard.star"),
    ),
];
const FUNDS: [(&str, &str); 3] = [
    ("funds/cash.star", include_str!("../../../products/funds/cash.star")),
    ("funds/balanced.star", include_str!("../../../products/funds/balanced.star")),
    ("funds/growth.star", include_str!("../../../products/funds/growth.star")),
];
const DEFINITIONS: &str = include_str!("../../../products/definitions/library.star");

/// The funds shipped in this repository. The unit pricing module activates
/// the same definitions on its own side.
pub fn shipped_funds() -> Result<Vec<FundDefinition>, LoadError> {
    FUNDS.iter().map(|(file, source)| load_fund(file, source)).collect()
}

/// Activation commands for the shipped funds. Separate from the product
/// configuration because a fund is also activated in the unit pricing
/// module, and the two must be given the same definitions together.
pub fn shipped_fund_activations() -> Result<Vec<Command>, LoadError> {
    Ok(shipped_funds()?
        .into_iter()
        .map(|fund| Command::Investment(Box::new(InvestCommand::ActivateFund(Box::new(fund)))))
        .collect())
}

/// Activation commands for everything shipped, in dependency order.
pub fn shipped_activations() -> Result<Vec<Command>, LoadError> {
    let mut out = vec![Command::ActivateDefinitions {
        definitions: load_definitions("definitions/library.star", DEFINITIONS)?,
    }];
    for (file, source) in RATES {
        out.push(Command::ActivateRateTable {
            table: Box::new(load_rate_table(file, source)?),
        });
    }
    for (file, source) in PRODUCTS {
        out.push(Command::ActivateProduct {
            product: Box::new(load_product(file, source)?),
        });
    }
    if let (Some(from), Ok(rate)) = (chrono::NaiveDate::from_ymd_opt(2027, 1, 1), DecStr::parse("0.025")) {
        out.push(Command::ActivateIndexRate {
            code: "cpi".to_string(),
            effective_from: from,
            rate,
        });
    }
    Ok(out)
}
