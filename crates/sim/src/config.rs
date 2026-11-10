//! The configuration shipped in this repository, loaded through the same
//! Starlark pipeline production uses.

use ic_core::Command;
use ic_money::DecStr;
use ic_product::{load_definitions, load_product, load_rate_table, LoadError};

const PRODUCTS: [(&str, &str); 3] = [
    ("term_life.star", include_str!("../../../products/term_life.star")),
    ("health_plan.star", include_str!("../../../products/health_plan.star")),
    ("group_life.star", include_str!("../../../products/group_life.star")),
];
const RATES: [(&str, &str); 4] = [
    ("rates/life_base.star", include_str!("../../../products/rates/life_base.star")),
    ("rates/trauma_base.star", include_str!("../../../products/rates/trauma_base.star")),
    ("rates/health_base.star", include_str!("../../../products/rates/health_base.star")),
    (
        "rates/group_life_standard.star",
        include_str!("../../../products/rates/group_life_standard.star"),
    ),
];
const DEFINITIONS: &str = include_str!("../../../products/definitions/library.star");

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
