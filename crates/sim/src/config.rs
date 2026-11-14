//! The configuration shipped in this repository, loaded through the same
//! Starlark pipeline production uses.

use ic_core::invest::{load_fund, FundDefinition, InvestCommand};
use ic_core::Command;
use ic_money::DecStr;
use ic_product::{load_definitions, load_product, load_rate_table, LoadError};

const PRODUCTS: [(&str, &str); 19] = [
    ("group_health.star", include_str!("../../../products/group_health.star")),
    ("group_benefits.star", include_str!("../../../products/group_benefits.star")),
    ("income_protection.star", include_str!("../../../products/income_protection.star")),
    ("term_life.star", include_str!("../../../products/term_life.star")),
    ("interim_accident.star", include_str!("../../../products/interim_accident.star")),
    ("health_plan.star", include_str!("../../../products/health_plan.star")),
    ("group_life.star", include_str!("../../../products/group_life.star")),
    ("invest_plan.star", include_str!("../../../products/invest_plan.star")),
    ("health_flex.star", include_str!("../../../products/health_flex.star")),
    ("aia_living.star", include_str!("../../../products/aia_living.star")),
    ("aia_living_business.star", include_str!("../../../products/aia_living_business.star")),
    ("aia_start_up_income.star", include_str!("../../../products/aia_start_up_income.star")),
    ("aia_cover_for_life.star", include_str!("../../../products/aia_cover_for_life.star")),
    ("aia_essentials_life.star", include_str!("../../../products/aia_essentials_life.star")),
    ("aia_starter.star", include_str!("../../../products/aia_starter.star")),
    ("aia_interim.star", include_str!("../../../products/aia_interim.star")),
    ("aia_private_health.star", include_str!("../../../products/aia_private_health.star")),
    ("aia_cancer_care.star", include_str!("../../../products/aia_cancer_care.star")),
    ("aia_corporate.star", include_str!("../../../products/aia_corporate.star")),
];
const RATES: [(&str, &str); 29] = [
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
    (
        "rates/aia_accidental_death.star",
        include_str!("../../../products/rates/aia_accidental_death.star"),
    ),
    (
        "rates/aia_accidental_injury.star",
        include_str!("../../../products/rates/aia_accidental_injury.star"),
    ),
    (
        "rates/aia_business_continuity.star",
        include_str!("../../../products/rates/aia_business_continuity.star"),
    ),
    ("rates/aia_cancer_care.star", include_str!("../../../products/rates/aia_cancer_care.star")),
    (
        "rates/aia_family_protection.star",
        include_str!("../../../products/rates/aia_family_protection.star"),
    ),
    (
        "rates/aia_group_critical_illness.star",
        include_str!("../../../products/rates/aia_group_critical_illness.star"),
    ),
    ("rates/aia_group_income.star", include_str!("../../../products/rates/aia_group_income.star")),
    ("rates/aia_group_life.star", include_str!("../../../products/rates/aia_group_life.star")),
    (
        "rates/aia_group_severe_trauma.star",
        include_str!("../../../products/rates/aia_group_severe_trauma.star"),
    ),
    ("rates/aia_group_tpd.star", include_str!("../../../products/rates/aia_group_tpd.star")),
    ("rates/aia_income.star", include_str!("../../../products/rates/aia_income.star")),
    ("rates/aia_life.star", include_str!("../../../products/rates/aia_life.star")),
    ("rates/aia_private_health.star", include_str!("../../../products/rates/aia_private_health.star")),
    (
        "rates/aia_private_health_plus.star",
        include_str!("../../../products/rates/aia_private_health_plus.star"),
    ),
    (
        "rates/aia_progressive_care.star",
        include_str!("../../../products/rates/aia_progressive_care.star"),
    ),
    ("rates/aia_severe_trauma.star", include_str!("../../../products/rates/aia_severe_trauma.star")),
    (
        "rates/aia_specialist_testing.star",
        include_str!("../../../products/rates/aia_specialist_testing.star"),
    ),
    ("rates/aia_tpd.star", include_str!("../../../products/rates/aia_tpd.star")),
    ("rates/aia_trauma.star", include_str!("../../../products/rates/aia_trauma.star")),
];
const FUNDS: [(&str, &str); 3] = [
    ("funds/cash.star", include_str!("../../../products/funds/cash.star")),
    ("funds/balanced.star", include_str!("../../../products/funds/balanced.star")),
    ("funds/growth.star", include_str!("../../../products/funds/growth.star")),
];
const DEFINITIONS: &str = include_str!("../../../products/definitions/library.star");
const AIA_DEFINITIONS: &str = include_str!("../../../products/definitions/aia.star");

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
    out.push(Command::ActivateDefinitions {
        definitions: load_definitions("definitions/aia.star", AIA_DEFINITIONS)?,
    });
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
    // What the owner of a policy with a loan against it is charged a year.
    if let (Some(from), Ok(rate)) = (chrono::NaiveDate::from_ymd_opt(2027, 1, 1), DecStr::parse("0.0795")) {
        out.push(Command::ActivateIndexRate {
            code: "policy-loan-rate".to_string(),
            effective_from: from,
            rate,
        });
    }
    Ok(out)
}
