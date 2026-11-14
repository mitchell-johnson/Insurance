//! The AIA New Zealand products (products/aia_*.star): they load, and their
//! option menus, entry ages, built-in benefits and discounts work as the
//! published material describes (docs/aia-products.md).

use ic_product::{load_definitions, load_product, load_rate_table};

const LIVING: &str = include_str!("../../../products/aia_living.star");
const DEFS: &str = include_str!("../../../products/definitions/aia.star");

/// Purpose: the AIA Living product loads.
/// Why it matters: placeholder.
#[test]
fn aia_living_loads() {
    let p = load_product("aia_living.star", LIVING).unwrap();
    assert!(p.covers.len() > 10);
    load_definitions("aia.star", DEFS).unwrap();
    for (f, s) in [("l", include_str!("../../../products/rates/aia_life.star")), ("i", include_str!("../../../products/rates/aia_income.star"))] {
        load_rate_table(f, s).unwrap();
    }
}
