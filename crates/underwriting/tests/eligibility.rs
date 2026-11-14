//! A product's conditions on who may apply for what, checked as the application arrives.

mod common;

use chrono::{Datelike, NaiveDate};
use common::*;
use ic_core::PolicyId;
use ic_underwriting::command::{Command, Effect, OpenProposal};
use ic_underwriting::model::Entry;

/// An application for one cover on one life aged `age` today, in occupation `class`.
fn apply(w: &World, id: &str, product: &str, cover: (&str, &str, i64), option: Option<&str>, age: i32, class: Option<&str>) -> OpenProposal {
    let mut open = w.open_command(id, &["L1"], &[("L1", cover.0, cover.1, cover.2)], true);
    open.product_code = product.into();
    open.covers[0].option = option.map(Into::into);
    let today = w.today();
    open.lives[0].factors.date_of_birth = NaiveDate::from_ymd_opt(today.year() - age, 1, 1);
    open.lives[0].factors.occupation_class = class.map(Into::into);
    open
}

fn outcome(w: &mut World, open: OpenProposal) -> Result<(), String> {
    w.send("new-business", Command::OpenProposal(Box::new(open))).map(|_| ()).map_err(|r| r.code)
}

/// Purpose: each AIA product's conditions on who may apply are checked as the application
/// arrives: TPD, income and accident cover need the life's occupation class; TPD is taken to
/// 55, or 60 in a professional class; income to 70 only in an accepted class; Family
/// Protection to an age runs at least 15 years; a child under 16 has at most $250,000 of
/// trauma; and Essentials Life is limited to $500,000 from 61. Inside each limit the same
/// application is taken. More cover on a policy in force is not checked again.
/// Why it matters: these are the product's terms. An application outside them would be
/// underwritten and priced, and then either refused at issue after the customer accepted an
/// offer or, worse, issued on terms the product does not allow.
#[test]
fn applications_outside_a_products_conditions_are_refused_at_the_door() {
    let mut w = World::new();
    let tpd = ("tpd-own-occupation", "tpd", 500_000_00);
    let income = ("income", "income_protection", 5_000_00);
    let family = ("family-protection", "life", 2_000_00);
    let trauma = ("critical-conditions", "trauma", 300_000_00);
    let essentials = ("life", "life", 600_000_00);
    let cases = [
        ("A1", "aia-living", tpd, None, 40, None, false),
        ("A2", "aia-living", tpd, None, 57, Some("3"), false),
        ("A3", "aia-living", tpd, None, 57, Some("1"), true),
        ("A4", "aia-living", income, Some("waiting=4w,period=to-70"), 40, Some("3"), false),
        ("A5", "aia-living", income, Some("waiting=4w,period=to-70"), 40, Some("2"), true),
        ("A6", "aia-living", income, Some("waiting=4w,period=to-65"), 40, Some("3"), true),
        ("A7", "aia-living", family, Some("term=to-50"), 40, None, false),
        ("A8", "aia-living", family, Some("term=to-55"), 40, None, true),
        ("A9", "aia-living", family, Some("term=10y"), 40, None, true),
        ("A10", "aia-living", trauma, None, 10, None, false),
        ("A11", "aia-living", (trauma.0, trauma.1, 250_000_00), None, 10, None, true),
        ("A12", "aia-living", trauma, None, 16, None, true),
        ("A13", "aia-essentials-life", essentials, None, 65, None, false),
        ("A14", "aia-essentials-life", (essentials.0, essentials.1, 500_000_00), None, 65, None, true),
        ("A15", "aia-essentials-life", essentials, None, 60, None, true),
    ];
    for (id, product, cover, option, age, class, taken) in cases {
        let open = apply(&w, id, product, cover, option, age, class);
        let got = outcome(&mut w, open);
        assert_eq!(got.is_ok(), taken, "{id}: {got:?}");
        if !taken {
            assert_eq!(got, Err("eligibility".into()), "{id}");
        }
    }
    let mut more = apply(&w, "A16", "aia-living", tpd, None, 57, Some("3"));
    more.entry = Entry::Increase {
        policy: PolicyId::from("POL-A3"),
    };
    more.policy = PolicyId::from("POL-A3");
    assert_ne!(outcome(&mut w, more), Err("eligibility".into()), "more cover is not checked again");
}

/// Purpose: the option chosen on a cover's menu is carried from the application to the core,
/// and an AIA Living application gets AIA's own interim cover (product `aia-interim`, life
/// cover up to $500,000) in place of the general interim product.
/// Why it matters: a policy issued without the customer's chosen waiting period or benefit
/// term is not the policy they were quoted; and AIA Living promises its own interim cover
/// while the application is underwritten, which another product's cover would not honour.
#[test]
fn an_aia_living_application_keeps_its_options_and_has_aias_interim_cover() {
    let mut w = World::new();
    let open = apply(
        &w,
        "B1",
        "aia-living",
        ("income", "income_protection", 5_000_00),
        Some("waiting=4w,period=to-65"),
        40,
        Some("2"),
    );
    let opened = w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let Some(Effect::IssueInterim { command, .. }) = opened.effects.iter().find(|e| matches!(e, Effect::IssueInterim { .. })) else {
        panic!("no interim cover: {:?}", opened.effects)
    };
    assert_eq!((command.product_code.as_str(), command.covers[0].code.as_str()), ("aia-interim", "life"));
    assert_eq!(command.covers[0].sum_insured_minor, 5_000_00, "no more than the cover applied for");
    let disclosed = w.disclose("B1", "L1", &clean());
    let applied = w.automatic(&disclosed);
    let Some(Effect::IssuePolicy { command, .. }) = applied.effects.iter().find(|e| matches!(e, Effect::IssuePolicy { .. })) else {
        panic!("not issued: {:?}", applied.effects)
    };
    assert_eq!(command.covers[0].excess_option.as_deref(), Some("waiting=4w,period=to-65"));
}
