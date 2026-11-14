//! The group module's record of a scheme against the core's: members by
//! billing group and cover in force, after a year of the scheme's life.

mod common;

use common::*;
use ic_group::command::ExcessDecision;
use ic_group::mapping::FileKind;
use serde_json::json;

/// Purpose: for a scheme whose members sit in two billing groups (GRP-036: one scheme, one
/// policy, several billing groups), the group module's scheme view counts the same members
/// as are in force in the core, by billing group adding up to the core's total, and states
/// the same cover in force per cover as the core computes from its own record of each life
/// plus the underwritten layers it holds, after joins, a salary change, a leaver and an
/// accepted excess.
/// Why it matters: the employer, the console and finance read the group module's record;
/// claims and billing read the core's. If the two drift a member is billed for cover the
/// core does not hold, or holds cover nobody is billed for, and nobody sees it until a claim.
#[test]
fn the_schemes_record_here_matches_the_core_member_for_member() {
    let mut h = Harness::new(terms());
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 80_000_00, "wlg")),
            row(3, "E003", attrs("staff", 95_000_00, "wlg")),
            row(4, "E004", attrs("executive", 240_000_00, "akl")),
        ],
    );
    let case = h.requests[0].case;
    h.underwriting(
        case,
        ExcessDecision::Accepted {
            sum_insured_minor: 100_000_00,
            loadings: vec![],
            exclusions: vec![],
            member_consents_to_itemising: false,
            member_pays_loading: false,
        },
    )
    .unwrap();
    h.days(20);
    h.salary("f2", "E002", "staff", 90_000_00);
    h.days(20);
    let today = h.today();
    h.apply(
        "f3",
        FileKind::Changes,
        vec![change(1, "E003", ic_group::record::RowChange::Leave, today, attrs("staff", 95_000_00, "wlg"))],
    );

    let view = ic_group::answer(&h.g, json!({"ask": "scheme", "scheme": SCHEME})).unwrap();
    let core = h.c.state(h.p);
    let policy = &core.policies[&h.policy];
    let product = product();
    let today = h.today();
    let in_force: Vec<_> = policy.lives.values().filter(|l| l.in_force_on(today)).collect();

    let by_group: u64 = view["members_by_group"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum();
    assert_eq!((view["members"].as_u64(), by_group), (Some(in_force.len() as u64), in_force.len() as u64));
    assert_eq!(view["members_by_group"], json!({"akl": 2, "wlg": 1}));

    for def in ic_group::benefit::formula_covers(&product) {
        let automatic: i64 = in_force
            .iter()
            .map(|l| {
                let f = l.factors.as_of(today).unwrap();
                ic_core::premium::member_sum_insured(def, f, &l.id, product.rounding, product.currency).unwrap()
            })
            .sum();
        let underwritten: i64 = policy.covers[&def.code]
            .layers
            .iter()
            .filter(|l| l.origin == ic_core::LayerOrigin::UnderwrittenExcess && l.start <= today && l.end.is_none_or(|e| e > today))
            .map(|l| l.sum_insured_minor)
            .sum();
        assert_eq!(view["cover_minor"][&def.code].as_i64(), Some(automatic + underwritten), "{}", def.code);
    }
}
