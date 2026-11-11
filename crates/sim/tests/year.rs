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

/// Purpose: the simulator's command line sets each field of the run it names, leaves the
/// rest at the small insurer's defaults, and refuses a flag it does not know, a missing or
/// non-numeric value, and a partition count outside what the ledger supports.
/// Why it matters: the simulator is the tool for sizing and soak runs. An argument silently
/// ignored or truncated (65,537 partitions read as 1) gives a run that reports clean on a
/// configuration nobody asked for; zero partitions used to crash it with a division by zero.
#[test]
fn the_simulator_command_line_is_parsed_strictly() {
    let args = |line: &str| line.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    let default = YearConfig::small(20271115);
    let cfg = YearConfig::from_args(&args("--seed 7 --policies 12 --members 30 --days 5 --partitions 2 --no-journal")).unwrap();
    assert_eq!(
        (cfg.seed, cfg.retail_policies, cfg.scheme_members, cfg.days, cfg.partitions, cfg.verify_journal),
        (7, 12, 30, 5, 2, false)
    );
    assert_eq!((cfg.start, cfg.transfer_every_days), (default.start, default.transfer_every_days));
    let untouched = YearConfig::from_args(&[]).unwrap();
    assert_eq!((untouched.seed, untouched.days, untouched.verify_journal), (default.seed, default.days, true));

    for (line, says) in [
        ("--bogus", "unknown argument"),
        ("--days", "needs a number"),
        ("--days soon", "not a number"),
        ("--partitions 0", "at least 1"),
        ("--partitions 65", "at most 64"),
        ("--partitions 65537", "at most 64"),
        ("--members 4294967296", "at most"),
    ] {
        let refused = YearConfig::from_args(&args(line)).unwrap_err();
        assert!(refused.contains(says), "{line}: {refused}");
    }
}

/// Purpose: the shipped `ic-simulate` binary runs a short simulation, prints its report as
/// JSON with the same figures as the library gives for the same arguments, exits zero, and
/// exits with status 2 and a message on arguments it does not understand.
/// Why it matters: scripts and CI judge a simulation by the exit status and parse the
/// report from standard output. A binary that printed progress into the report, or exited
/// zero on a bad argument, would make an automated soak run meaningless.
#[test]
fn the_simulator_binary_reports_as_json_and_by_exit_status() {
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_ic-simulate"))
            .args(args)
            .env_clear()
            .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .output()
            .unwrap()
    };
    let line = ["--policies", "8", "--members", "10", "--days", "4", "--partitions", "2"];
    let out = run(&line);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let printed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let args: Vec<String> = line.iter().map(|a| (*a).to_string()).collect();
    let (_, expected) = run_year(&YearConfig::from_args(&args).unwrap()).unwrap();
    assert_eq!(printed, serde_json::to_value(&expected).unwrap());
    assert_eq!(printed["days"], 4);
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("clean: 4 days"));

    let bad = run(&["--partitions", "0"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(bad.stdout.is_empty());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("--partitions"));
}
