//! AIA Corporate Solutions group schemes on the shipped `aia-corporate`
//! product: life, TPD, critical illness, severe trauma and income by
//! benefit category, administered by this module like any scheme.

mod common;

use common::*;
use ic_group::event::GroupEvent;
use ic_group::mapping::FileKind;

fn aia_product() -> ic_product::ProductDefinition {
    ic_product::load_product("aia_corporate.star", include_str!("../../../products/aia_corporate.star")).unwrap()
}

/// Purpose: an AIA Corporate Solutions scheme registers on the shipped product with all five
/// covers, staff and executives join from the employer's file with cover on each worked out
/// from their category's multiple and maximum (income as a monthly share of salary), an
/// executive whose life cover is above the automatic acceptance limit is sent to
/// underwriting for the rest, and the employer is invoiced for the members by category.
/// Why it matters: AIA's corporate schemes are the group book this system is built for. A
/// cover whose formula the module could not read would leave members uncovered or covered
/// for the wrong amount, and the employer billed for it.
#[test]
fn an_aia_corporate_scheme_is_administered_like_any_scheme() {
    let product = aia_product();
    let covers: Vec<String> = ic_group::benefit::formula_covers(&product).map(|c| c.code.clone()).collect();
    assert_eq!(covers.len(), 5, "{covers:?}");
    let mut h = Harness::on(terms(), product.clone());
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 90_000_00, "akl")),
            row(3, "E003", attrs("executive", 400_000_00, "wlg")),
        ],
    );
    assert_eq!(h.scheme().membership.active_count(), 3);
    assert!(!h.saw(|e| matches!(e, GroupEvent::RowRefused { .. })));

    // Staff on 90,000: life 3x, income 0.0625 of salary a month.
    let staff = ic_group::benefit::member_cover(&product, &h.scheme().membership.members["E002"].attrs, None);
    let amount = |code: &str| staff.iter().find(|c| c.cover == code).map(|c| c.formula_minor);
    assert_eq!(amount("group-life"), Some(270_000_00));
    assert_eq!(amount("group-income"), Some(5_625_00));
    assert_eq!(amount("group-critical-illness"), Some(90_000_00));

    // An executive on 400,000: life 5x is 2,000,000, over the automatic acceptance limit.
    assert!(h.requests.iter().any(|r| r.life == life("E003")), "the executive's excess goes to underwriting");

    h.days(31);
    let invoice = h.scheme().invoices.back().cloned().expect("invoiced");
    assert_eq!(invoice.members(), 3);
    assert!(invoice.total_minor() > 0);
}
