//! Loading and validating the chart, the close configuration and periods.

mod common;

use common::*;
use ic_finance::chart::ChartAccount;
use ic_finance::close::{AgedDecl, CheckDecl, CheckSource};
use ic_finance::config::ConfigError;
use ic_finance::report::working_days_after;
use ic_finance::{Chart, CloseConfig, PeriodId};
use ic_ledger::AccountClass;

fn invalid<T: std::fmt::Debug>(result: Result<T, ConfigError>) -> String {
    match result {
        Err(ConfigError::Invalid(message)) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// Purpose: the shipped chart loads, maps every account role the core posts to exactly one
/// locked account, and gets a content hash that changes when the source does.
/// Why it matters: a role the chart does not map stops the feed in production, and the hash
/// is what an approver is approving. Both need to be true of the file actually shipped.
#[test]
fn the_shipped_chart_maps_every_core_role() {
    let chart = chart();
    assert_eq!((chart.version, chart.retained_earnings.as_str()), (1, "3900"));
    for role in [
        "settlement",
        "premium_receivable",
        "unearned_premium",
        "earned_premium",
        "policy_suspense",
        "benefit_payable",
        "gst_payable",
        "levy_payable",
        "unit_account",
        "fund_unit_position",
        "fund_cash_position",
        "pending_investment_cash",
        "policy_loan",
        "transfer_in_transit",
        "lapse_write_off",
        "rounding",
    ] {
        let account = chart.account_for_role(role).unwrap_or_else(|| panic!("{role} is not mapped"));
        assert!(account.control, "{role} is fed by the core and must be locked");
    }
    assert_eq!(chart.account_for_role("earned_premium").unwrap().class, AccountClass::Income);
    assert!(chart.account_for_role("nonsense").is_none());
    assert_eq!(chart.content_hash.len(), 64);
    let edited = Chart::load("chart.star", &CHART.replace("Rounding", "Rounding differences")).unwrap();
    assert_ne!(edited.content_hash, chart.content_hash);
}

/// Purpose: a chart is refused when it has no version or accounts, an account without a
/// name or with a slash in its code, a duplicate account, a role mapped twice or mapped to
/// an unlocked account, or a retained earnings account that is missing, locked or not a
/// liability.
/// Why it matters: these are the mistakes that would let two balances share an account
/// record, let a person journal over a balance the core owns, or leave the year-end close
/// with nowhere valid to post.
#[test]
fn a_malformed_chart_is_refused_with_the_reason() {
    let base = chart();
    let with = |change: &dyn Fn(&mut Chart)| {
        let mut c = base.clone();
        change(&mut c);
        invalid(c.validate())
    };
    assert!(with(&|c| c.version = 0).contains("version"));
    assert!(with(&|c| c.accounts.clear()).contains("at least one account"));
    assert!(with(&|c| c.accounts[0].name.clear()).contains("needs a code"));
    assert!(with(&|c| c.accounts[0].code = "10/00".to_string()).contains("needs a code"));
    assert!(with(&|c| c.accounts[1].code = "1000".to_string()).contains("declared twice"));
    assert!(with(&|c| c.accounts[1].roles.push("settlement".to_string())).contains("mapped to two accounts"));
    assert!(with(&|c| c.accounts[0].control = false).contains("must be a control account"));
    assert!(with(&|c| c.retained_earnings = "0000".to_string()).contains("retained_earnings"));
    assert!(with(&|c| c.retained_earnings = "2000".to_string()).contains("retained_earnings"));
    assert!(with(&|c| c.retained_earnings = "6500".to_string()).contains("retained_earnings"));
}

/// Purpose: a new chart version may only follow the current one and add to it: removing an
/// account, changing a class or a lock, moving a role, or changing the retained earnings
/// account is refused.
/// Why it matters: balances already posted keep their account codes for ever. A chart
/// change that redefined a code would silently change what years of history mean.
#[test]
fn a_chart_version_can_add_but_never_redefine() {
    let v1 = chart();
    let next = |change: &dyn Fn(&mut Chart)| {
        let mut c = v1.clone();
        c.version = 2;
        change(&mut c);
        c.may_replace(&v1)
    };
    assert_eq!(next(&|_| {}), Ok(()));
    let add = |c: &mut Chart| {
        c.accounts.push(ChartAccount {
            code: "1160".to_string(),
            name: "Reinsurance recoverable".to_string(),
            class: AccountClass::Asset,
            control: true,
            roles: vec!["reinsurance_recoverable".to_string()],
        })
    };
    assert_eq!(next(&add), Ok(()));
    assert!(invalid(next(&|c| c.version = 3)).contains("cannot follow"));
    assert!(invalid(next(&|c| c.accounts.remove(3).code.clear())).contains("cannot be removed"));
    assert!(invalid(next(&|c| c.accounts[0].class = AccountClass::Liability)).contains("class or its lock"));
    assert!(invalid(next(&|c| c.accounts[21].control = true)).contains("class or its lock"));
    assert!(invalid(next(&|c| c.accounts[0].roles.clear())).contains("cannot be moved"));
    assert!(invalid(next(&|c| c.retained_earnings = "2700".to_string())).contains("retained earnings"));
}

/// Purpose: a close configuration is refused when it has no version, declares a check twice
/// or under the reserved prefix, declares an automatic check the ledger does not have or a
/// reported check under an automatic name or with no reporter, leaves the control check or
/// trial balance out or makes them optional, or declares aged items badly.
/// Why it matters: the configuration decides what can stop a close. It must not be possible
/// to configure away the two checks that prove the books equal the core and balance.
#[test]
fn a_close_configuration_cannot_drop_the_checks_the_close_rests_on() {
    let base = close_config();
    assert!(base.checks.iter().any(|c| c.id == "fund_balance" && c.blocking));
    assert_eq!(base.aged.iter().find(|a| a.kind == "held_balance").unwrap().check_id(), "aged:held_balance");
    let with = |change: &dyn Fn(&mut CloseConfig)| {
        let mut c = base.clone();
        change(&mut c);
        invalid(c.validate())
    };
    let check = |id: &str, source: CheckSource| CheckDecl {
        id: id.to_string(),
        name: "x".to_string(),
        owner: "x".to_string(),
        day: 1,
        blocking: true,
        quarterly: false,
        source,
    };
    let by = |who: &str| CheckSource::Reported { by: who.to_string() };
    assert!(with(&|c| c.version = 0).contains("version"));
    assert!(with(&|c| c.checks.push(check("commission_sweep", by("commission")))).contains("declared twice"));
    assert!(with(&|c| c.checks.push(check("aged:x", by("commission")))).contains("reserved"));
    assert!(with(&|c| c.checks.push(check("", by("commission")))).contains("unnamed"));
    assert!(with(&|c| c.checks.push(check("magic", CheckSource::Automatic))).contains("no automatic check"));
    assert!(with(&|c| c.checks[0].source = by("feed")).contains("automatic and cannot be reported"));
    assert!(with(&|c| c.checks.push(check("new_step", by("")))).contains("names nobody"));
    assert!(with(&|c| c.checks.retain(|x| x.id != "trial_balance")).contains("trial_balance must be declared"));
    assert!(with(&|c| c.checks[0].blocking = false).contains("control_check must be declared"));
    assert!(with(&|c| c.checks[0].quarterly = true).contains("control_check must be declared"));
    let aged = |kind: &str, by: &str, max: i64| AgedDecl {
        kind: kind.to_string(),
        name: "x".to_string(),
        owner: "x".to_string(),
        day: 2,
        by: by.to_string(),
        max_age_days: 30,
        max_amount_minor: max,
        blocking: true,
    };
    assert!(with(&|c| c.aged.push(aged("held_balance", "commission", 1))).contains("declared twice"));
    assert!(with(&|c| c.aged.push(aged("", "commission", 1))).contains("unnamed"));
    assert!(with(&|c| c.aged.push(aged("escheat", "", 1))).contains("no reporter"));
    assert!(with(&|c| c.aged.push(aged("escheat", "payments", -1))).contains("negative threshold"));
}

/// Purpose: a configuration file with a syntax error, without the symbol it must define, or
/// with a field of the wrong shape or an unknown field is reported as that, with the file
/// named, and a file that parses but breaks a rule is reported as invalid.
/// Why it matters: finance staff author these files. An error that said only "failed to
/// load" would send them to an engineer for a typo.
#[test]
fn a_configuration_file_that_cannot_be_read_says_why() {
    assert!(matches!(Chart::load("chart.star", "CHART = chart("), Err(ConfigError::Starlark(_))));
    assert!(matches!(Chart::load("chart.star", "CHART = undefined_helper()"), Err(ConfigError::Starlark(_))));
    let missing = Chart::load("chart.star", "OTHER = 1").unwrap_err();
    assert_eq!(missing.to_string(), "chart.star: `CHART` is not defined");
    let schema = CloseConfig::load("close.star", "CLOSE = {\"version\": \"one\", \"checks\": []}").unwrap_err();
    assert!(matches!(&schema, ConfigError::Schema { file, .. } if file == "close.star"), "{schema}");
    assert!(schema.to_string().starts_with("close.star: does not match the schema"));
    let extra = Chart::load(
        "chart.star",
        "CHART = {\"version\": 1, \"effective_from\": \"2027-01-01\", \"retained_earnings\": \"1\", \"accounts\": [], \"colour\": 1}",
    );
    assert!(matches!(extra, Err(ConfigError::Schema { .. })));
    let empty = Chart::load("chart.star", "CHART = chart(1, \"2027-01-01\", \"3900\", [])").unwrap_err();
    assert!(matches!(&empty, ConfigError::Invalid(m) if m.contains("at least one account")), "{empty}");
    let no_control = CloseConfig::load("close.star", "CLOSE = close_config(1, [])").unwrap_err();
    assert!(no_control.to_string().contains("control_check must be declared"));
}

/// Purpose: a period is a calendar month with an optional adjustment period after it. It is
/// written `YYYY-MM` or `YYYY-MM-ADJ`, orders by time with the adjustment period directly
/// after its month, knows its first and last day across leap years and year ends, and knows
/// the quarter ends and first month of a financial year ending in any month.
/// Why it matters: every balance in the books is keyed by period. An off-by-one at a year
/// boundary, or an adjustment period that sorted into the wrong year, would put postings in
/// the wrong accounts for a regulator-facing year end.
#[test]
fn periods_order_by_time_and_know_their_financial_year() {
    let feb = PeriodId::of(d(2028, 2, 10));
    assert_eq!((feb.start(), feb.end()), (d(2028, 2, 1), d(2028, 2, 29)));
    assert_eq!(feb, period(2028, 2));
    assert_eq!(feb.to_string(), "2028-02");
    assert_eq!(feb.adjustment().to_string(), "2028-02-ADJ");
    assert!(feb.adjustment().is_adjustment() && !feb.is_adjustment());
    assert!(feb < feb.adjustment() && feb.adjustment() < period(2028, 3));
    assert_eq!(period(2027, 12).next_month(), period(2028, 1));
    assert_eq!(period(2027, 12).adjustment().next_month(), period(2028, 1));
    assert_eq!(PeriodId::month(2028, 13), None);

    for text in ["2028-02", "2027-12-ADJ"] {
        let parsed: PeriodId = text.parse().unwrap();
        assert_eq!(parsed.to_string(), text);
        let json = serde_json::to_string(&parsed).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<PeriodId>(&json).unwrap(), parsed);
    }
    for bad in ["2028", "2028-13", "x-02", "2028-0x", "2028-02-adj"] {
        assert!(bad.parse::<PeriodId>().unwrap_err().contains("is not a period"), "{bad}");
        assert!(serde_json::from_str::<PeriodId>(&format!("\"{bad}\"")).is_err());
    }

    // A year ending in March has quarters ending in June, September, December and March.
    let quarter_ends: Vec<u32> = (1..=12).filter(|m| period(2028, *m).is_quarter_end(3)).collect();
    assert_eq!(quarter_ends, vec![3, 6, 9, 12]);
    assert_eq!((1..=12).filter(|m| period(2028, *m).is_quarter_end(12)).collect::<Vec<_>>(), vec![3, 6, 9, 12]);
    assert_eq!((1..=12).filter(|m| period(2028, *m).is_quarter_end(1)).collect::<Vec<_>>(), vec![1, 4, 7, 10]);
    assert!(period(2028, 3).is_year_end(3) && !period(2028, 4).is_year_end(3));
    assert_eq!(period(2028, 3).year_start(3), period(2027, 4));
    assert_eq!(period(2028, 3).adjustment().year_start(3), period(2027, 4));
    assert_eq!(period(2028, 4).year_start(3), period(2028, 4));
    assert_eq!(period(2028, 7).year_start(12), period(2028, 1));
}

/// Purpose: a close step due a number of working days after period end falls on a weekday,
/// skipping weekends, and a step due on day zero is due at period end.
/// Why it matters: the dashboard flags steps as overdue from this date. Counting weekends
/// would mark the whole team late every time a month ends on a Friday.
#[test]
fn close_steps_fall_due_on_working_days() {
    // 31 March 2028 is a Friday.
    assert_eq!(working_days_after(d(2028, 3, 31), 0), d(2028, 3, 31));
    assert_eq!(working_days_after(d(2028, 3, 31), 1), d(2028, 4, 3));
    assert_eq!(working_days_after(d(2028, 3, 31), 5), d(2028, 4, 7));
    assert_eq!(working_days_after(d(2028, 3, 31), 6), d(2028, 4, 10));
    assert_eq!(working_days_after(chrono::NaiveDate::MAX, 3), chrono::NaiveDate::MAX);
}
