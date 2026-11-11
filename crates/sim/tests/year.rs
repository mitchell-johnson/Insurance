//! The spine's exit test: the synthetic insurer runs a simulated year.

use ic_sim::year::{run_year, YearConfig};

/// Purpose: a synthetic insurer with retail policies and a group scheme runs a full year of
/// payments, bounces, lapses, reinstatements, cancellations, anniversaries, transfers and
/// membership files, reconciling cleanly on every day with no policy exceptions.
/// Why it matters: the scenario tests check each behaviour alone. This checks that they hold
/// together over a year of everything happening at once, which is where interactions between them
/// show.
#[test]
fn the_synthetic_insurer_runs_a_year_with_every_reconciliation_clean() {
    let cfg = YearConfig::small(20271115);
    let (_, report) = run_year(&cfg).unwrap_or_else(|e| panic!("{e}"));
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(report.days, 366);
    assert_eq!(report.policies_issued, 241);
    // the year exercised everything the spine has
    assert!(report.lapses > 0, "nobody lapsed");
    assert!(report.reinstatements > 0, "nobody reinstated");
    assert!(report.cancellations > 0, "nobody cancelled");
    assert!(report.dishonours > 0, "nothing bounced");
    assert!(report.anniversaries > 0, "no anniversaries");
    assert!(report.transfers_completed > 0, "no transfers");
    assert!(report.membership_changes > 900, "no membership files");
    assert!(report.cash_exceptions > 0, "no cash exceptions");
    assert_eq!(report.policy_exceptions, 0);
}

/// Purpose: two runs of the synthetic insurer from the same seed give identical reports and state
/// hashes, and a different seed gives different ones.
/// Why it matters: a failure found in a long run can only be investigated if the run can be
/// repeated exactly. The second half shows the seed really does drive the run, so the first half is
/// not trivially true.
#[test]
fn the_same_seed_gives_the_same_ledger_and_a_different_seed_does_not() {
    let mut cfg = YearConfig::small(7);
    cfg.retail_policies = 40;
    cfg.scheme_members = 120;
    cfg.days = 120;
    let (_, a) = run_year(&cfg).unwrap_or_else(|e| panic!("{e}"));
    let (_, b) = run_year(&cfg).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(a, b);
    cfg.seed = 8;
    let (_, c) = run_year(&cfg).unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(a.state_hashes, c.state_hashes);
}
