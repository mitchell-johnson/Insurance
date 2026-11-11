//! The treaty files shipped in the repository, and what the loader refuses.

mod common;

use common::*;
use ic_reinsurance::loader::{load_aggregate_treaty, load_rate_table, load_treaty, LoadError};
use ic_reinsurance::treaty::{AggregateKind, Basis, ExcessPath, PremiumBasis, RiskFactors, StatementPeriod};

/// Purpose: every treaty, aggregate treaty and reinsurer rate table shipped in the repository
/// loads into typed terms with a content hash, and its worked scenarios pass.
/// Why it matters: these files are what a deployment activates. A treaty that failed to load, or
/// whose scenarios no longer held, would stop cessions being calculated on the first day.
#[test]
fn shipped_treaties_load_and_their_scenarios_pass() {
    let retail = load_treaty("retail_life_surplus.star", RETAIL).unwrap();
    assert_eq!(retail.content_hash.len(), 64);
    assert_eq!(
        retail.basis,
        Basis::Surplus {
            retention_minor: 500 * K,
            automatic_limit_minor: 4000 * K
        }
    );
    assert_eq!(retail.commission, 200_000);
    assert_eq!(retail.panel.iter().map(|p| p.share).sum::<u32>(), 1_000_000);
    assert!(retail.covers("term-life", "trauma") && !retail.covers("group-life", "group_life"));
    assert_eq!(retail.premium, PremiumBasis::RateTable { table: "ri-life-base".into() });
    let group = load_treaty("group_life_surplus.star", GROUP).unwrap();
    assert_eq!(group.group.as_ref().unwrap().excess_path, ExcessPath::Facultative);
    assert_eq!(group.group.as_ref().unwrap().facultative_rate, 1_200_000);
    assert_eq!(group.statement.period, StatementPeriod::Monthly);
    let cat = load_aggregate_treaty("catastrophe_2027.star", CATASTROPHE).unwrap();
    assert!(matches!(cat.kind, AggregateKind::Catastrophe { min_lives: 3, .. }));
    assert!(cat.covers("term-life", "life", d(2027, 6, 1)) && !cat.covers("term-life", "life", d(2028, 1, 1)));
    assert!(!cat.covers("term-life", "trauma", d(2027, 6, 1)));
    let stop = load_aggregate_treaty("stop_loss_2027.star", STOP_LOSS).unwrap();
    assert_eq!(stop.reference().to_string(), "stop-loss-2027@1");
    let rates = load_rate_table("ri_life_base.star", RATES).unwrap();
    assert_eq!(rates.rows.len(), 5);
}

/// Purpose: the content hash covers the treaty's source, so any edit gives a different hash.
/// Why it matters: an active treaty version is immutable. The hash is how the module tells the
/// same version from a quietly edited one.
#[test]
fn any_edit_changes_the_content_hash() {
    let a = load_treaty("t.star", RETAIL).unwrap();
    let b = load_treaty("t.star", &RETAIL.replace("article 14", "article 15")).unwrap();
    assert_ne!(a.content_hash, b.content_hash);
    assert_eq!(a.content_hash, load_treaty("other-name.star", RETAIL).unwrap().content_hash);
}

/// Purpose: a treaty whose worked scenario does not give the answer written in the file is
/// refused, and the error names the scenario and the figures.
/// Why it matters: scenarios are the author's statement of what the treaty does. This is the
/// check that a change to the retention or the limits cannot be activated without anyone
/// noticing what it does to a real cover.
#[test]
fn a_treaty_whose_scenario_fails_does_not_load() {
    let changed = RETAIL.replace("retention = dollars(500000)", "retention = dollars(400000)");
    let err = load_treaty("t.star", &changed).unwrap_err();
    let LoadError::Scenario { scenario, error, .. } = &err else { panic!("{err}") };
    assert!(scenario.starts_with("a second cover"));
    assert!(error.contains("cover 2"), "{error}");
    assert!(err.to_string().contains("fails"));
}

/// Purpose: a treaty with no scenarios, or a scenario whose covers and expectations do not
/// line up, is refused.
/// Why it matters: an untested treaty is the one most likely to be wrong. The loader makes
/// worked examples a condition of activation, not a convention.
#[test]
fn a_treaty_needs_well_formed_scenarios() {
    let cut = RETAIL.find("SCENARIOS").unwrap();
    let none = format!("{}SCENARIOS = []\n", &RETAIL[..cut]);
    assert!(matches!(load_treaty("t.star", &none), Err(LoadError::Invalid { .. })));
    let lopsided = format!(
        "{}SCENARIOS = [scenario(\"x\", covers = [covered(1, \"2027-02-01\")], expect = [])]\n",
        &RETAIL[..cut]
    );
    assert!(matches!(load_treaty("t.star", &lopsided), Err(LoadError::Scenario { .. })));
    let missing = &RETAIL[..cut];
    let err = load_treaty("t.star", missing).unwrap_err();
    assert!(matches!(err, LoadError::MissingSymbol { symbol: "SCENARIOS", .. }), "{err}");
    assert!(err.to_string().contains("not defined"));
}

/// Purpose: each kind of broken treaty file gets its own kind of error: bad Starlark, a field
/// the schema does not know, a share written as a float, and terms that do not hang together.
/// Why it matters: treaty files are edited by people. An error that says exactly what is wrong
/// is the difference between a five minute fix and a failed release.
#[test]
fn broken_treaty_files_are_refused_with_the_reason() {
    assert!(matches!(load_treaty("t.star", "TREATY = ("), Err(LoadError::Starlark(_))));
    assert!(matches!(load_treaty("t.star", "x = 1"), Err(LoadError::MissingSymbol { symbol: "TREATY", .. })));
    let unknown = RETAIL.replace("version = 1,", "version = 1,\n    surprise = 1,");
    assert!(matches!(load_treaty("t.star", &unknown), Err(LoadError::Starlark(_))));
    let schema = RETAIL.replace("\"0.60\"", "0.6");
    let err = load_treaty("t.star", &schema).unwrap_err();
    assert!(matches!(err, LoadError::Schema { .. }), "{err}");
    assert!(err.to_string().contains("schema"));
    for (from, to) in [
        ("\"0.60\"", "\"0.50\""),
        ("version = 1", "version = 0"),
        ("automatic_limit = dollars(4000000)", "automatic_limit = 0"),
        ("consent_days = 10", "consent_days = 0"),
        ("notify_at = dollars(1000000)", "notify_at = 0"),
        ("reinsurer(\"reinsurer-south\", \"0.40\")", "reinsurer(\"reinsurer-north\", \"0.40\")"),
        ("rate_table_premium(\"ri-life-base\")", "rate_table_premium(\"\")"),
        ("scope = [scope(\"term-life\", [\"life\", \"trauma\"])]", "scope = []"),
        (
            "effective_from = \"2027-01-01\",",
            "effective_from = \"2027-01-01\",\n    closed_to_new_business_from = \"2026-01-01\",",
        ),
    ] {
        let err = load_treaty("t.star", &RETAIL.replace(from, to)).unwrap_err();
        assert!(matches!(err, LoadError::Invalid { .. }), "{from}: {err}");
        assert!(err.to_string().contains("invalid"));
    }
    for (from, to) in [
        ("facultative_limit = dollars(1000000)", "facultative_limit = -1"),
        ("scheme_share(\"0.30\")", "excess_per_member(-1)"),
    ] {
        assert!(
            matches!(load_treaty("t.star", &GROUP.replace(from, to)), Err(LoadError::Invalid { .. })),
            "{from}"
        );
    }
    let share = RETAIL.replace("\"0.60\"", "\"1.60\"");
    assert!(matches!(load_treaty("t.star", &share), Err(LoadError::Schema { .. })));
    let rate = GROUP.replace("\"1.20\"", "\"-1.20\"");
    assert!(matches!(load_treaty("t.star", &rate), Err(LoadError::Schema { .. })));
    let places = GROUP.replace("\"1.20\"", "\"1.2000001\"");
    assert!(matches!(load_treaty("t.star", &places), Err(LoadError::Schema { .. })));
}

/// Purpose: quota share and excess of loss treaties validate their own terms, and aggregate
/// treaties and rate tables refuse inverted periods, bands and empty tables.
/// Why it matters: the shipped files only exercise surplus. The other bases are what the next
/// treaty negotiation will use, and their validation must already work.
#[test]
fn other_bases_and_tables_validate_their_terms() {
    let qs = RETAIL
        .replace("surplus(retention = dollars(500000), automatic_limit = dollars(4000000))", "quota_share(\"0.40\", dollars(1000000))")
        .replace(&RETAIL[RETAIL.find("SCENARIOS").unwrap()..], "SCENARIOS = [scenario(\"forty percent\", covers = [covered(dollars(100000), \"2027-02-01\")], expect = [expect(retained = dollars(60000), ceded = dollars(40000))])]\n");
    assert!(matches!(load_treaty("t.star", &qs).unwrap().basis, Basis::QuotaShare { share: 400_000, .. }));
    assert!(matches!(
        load_treaty("t.star", &qs.replace("\"0.40\", dollars(1000000)", "\"0\", dollars(1000000)")),
        Err(LoadError::Invalid { .. })
    ));
    let xol = qs.replace("quota_share(\"0.40\", dollars(1000000))", "excess_of_loss(dollars(60000), dollars(500000))");
    assert!(!load_treaty("t.star", &xol).unwrap().basis.proportional());
    assert!(matches!(
        load_treaty("t.star", &xol.replace("dollars(500000))", "0)")),
        Err(LoadError::Invalid { .. })
    ));

    for (from, to) in [
        ("period_end = \"2027-12-31\"", "period_end = \"2026-12-31\""),
        ("limit = dollars(20000000)", "limit = 0"),
        ("version = 1", "version = 0"),
        ("\"0.50\"), reinsurer(\"reinsurer-south\", \"0.50\")", "\"0.50\")"),
    ] {
        assert!(
            matches!(load_aggregate_treaty("c.star", &CATASTROPHE.replace(from, to)), Err(LoadError::Invalid { .. })),
            "{from}"
        );
    }
    assert!(matches!(
        load_aggregate_treaty("c.star", &STOP_LOSS.replace("attachment = dollars(30000000)", "attachment = -1")),
        Err(LoadError::Invalid { .. })
    ));
    assert!(matches!(
        load_rate_table(
            "r.star",
            &RATES.replace("age_from = 40, age_to = 59, smoker = False", "age_from = 60, age_to = 59")
        ),
        Err(LoadError::Invalid { .. })
    ));
    assert!(matches!(
        load_rate_table("r.star", "RATE_TABLE = reinsurer_rate_table(\"x\", 1, \"2027-01-01\", [])"),
        Err(LoadError::Invalid { .. })
    ));
}

/// Purpose: a reinsurer's rate is found by age last birthday, smoker status and sex, and a row
/// that asks about a fact the module does not hold does not match.
/// Why it matters: the ceded premium is this rate times the sum ceded. A rate guessed for a
/// life whose facts are missing would be a wrong payment to a reinsurer that nobody could see.
#[test]
fn a_rate_is_found_by_the_facts_held_and_never_guessed() {
    let rates = load_rate_table("r.star", RATES).unwrap();
    let life = |dob, smoker| RiskFactors {
        date_of_birth: Some(dob),
        sex: None,
        smoker,
    };
    let on = d(2027, 3, 10);
    assert_eq!(rates.rate(Some(&life(d(1987, 3, 11), Some(false))), on), Some(650_000));
    assert_eq!(rates.rate(Some(&life(d(1987, 3, 10), Some(false))), on), Some(1_800_000));
    assert_eq!(rates.rate(Some(&life(d(1987, 3, 10), Some(true))), on), Some(3_900_000));
    assert_eq!(rates.rate(Some(&life(d(1960, 1, 1), None)), on), Some(9_500_000));
    // Under 60 the table needs the smoker status.
    assert_eq!(rates.rate(Some(&life(d(1990, 1, 1), None)), on), None);
    assert_eq!(rates.rate(None, on), None);
    assert_eq!(rates.rate(Some(&RiskFactors::default()), on), None);
    // Someone not born yet has no age.
    assert_eq!(life(d(2030, 1, 1), None).age_on(on), None);
    let by_sex = load_rate_table(
        "r.star",
        &RATES.replace("reinsurer_rate(\"9.50\", age_from = 60)", "reinsurer_rate(\"9.50\", sex = \"male\")"),
    )
    .unwrap();
    assert_eq!(
        by_sex.rate(
            Some(&RiskFactors {
                sex: Some(ic_product::Sex::Male),
                ..RiskFactors::default()
            }),
            on
        ),
        Some(9_500_000)
    );
    assert_eq!(
        by_sex.rate(
            Some(&RiskFactors {
                sex: Some(ic_product::Sex::Female),
                ..RiskFactors::default()
            }),
            on
        ),
        None
    );
}
