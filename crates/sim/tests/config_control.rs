//! Taking configuration out of use: the kill switch after a bad activation,
//! retiring a product version, and the register of what is in use.

use chrono::NaiveDate;
use ic_core::catalog::{ConfigKind, Withdrawal};
use ic_core::event::Event;
use ic_core::{Command, PolicyId, Reject};
use ic_money::{Currency::NZD, Money};
use ic_product::{load_product, load_rate_table, Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

fn until(c: &mut Cluster, date: NaiveDate) {
    while c.state(0).business_date.unwrap() < date {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn issue(c: &mut Cluster, n: u64) -> PolicyId {
    let issue = term_life(
        n,
        c.business_date().unwrap(),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let receipt = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: format!("r-{n}"),
        amount: Money::new(5_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    id
}

/// Each bill for the policy since last asked.
fn bills(c: &mut Cluster, id: &PolicyId) -> Vec<(NaiveDate, i64)> {
    c.take_events()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::PremiumDue {
                policy,
                period_start,
                total_minor,
                ..
            } if policy == *id => Some((period_start, total_minor)),
            _ => None,
        })
        .collect()
}

fn withdrawal(kind: ConfigKind, id: &str, version: u32, retire: bool) -> Withdrawal {
    Withdrawal {
        kind,
        id: id.into(),
        version,
        on: None,
        reason: "rates keyed a factor of ten too high".into(),
        change: "CHG-2041".into(),
        approved_by: vec!["ops-ann".into()],
        retire,
    }
}

fn withdraw(c: &mut Cluster, w: Withdrawal) -> Result<(), Reject> {
    c.submit(0, "config", Command::WithdrawConfig(Box::new(w)))
}

/// Purpose: matrix CFG-009 and CFG-010. A rate table version activated with rates ten times too
/// high is taken out of use by the kill switch: one prepared command with one approver, its
/// reason and change record. From then on the version before it prices the next bill and new
/// business; the bill already raised at the wrong rate is left as posted for remediation by
/// replay. The register shows each version with its effective date and whether it is in use,
/// and the withdrawal with who approved it. Only configuration may send it, it needs its
/// reason, record and approver, and the same request again changes nothing.
/// Why it matters: a bad activation is found while it is pricing. The fastest safe move is to
/// stop it pricing anything more, without a rollback rewriting entries that customers have
/// already been sent; a register that could not show which version is live would leave the
/// incident team guessing which customers to remediate.
#[test]
fn the_kill_switch_takes_a_bad_rate_table_out_of_use() {
    let mut c = cluster();
    let id = issue(&mut c, 1);
    let good = bills(&mut c, &id)[0].1;

    // Version 2, for the same dates as version 1 (stepped premium is rated as at the last
    // anniversary), with every rate ten times too high.
    let source = include_str!("../../../products/rates/life_base.star").replace("version = 1,", "version = 2,");
    let source = regex_free_scale(&source);
    let bad = load_rate_table("life_base_v2.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateRateTable { table: Box::new(bad) }).unwrap();
    until(&mut c, d(2027, 12, 16));
    let wrong = bills(&mut c, &id);
    assert_eq!(wrong.len(), 1);
    assert!(wrong[0].1 > 5 * good, "billed {} against {good}", wrong[0].1);
    let register = c.state(0).catalog.register(c.business_date().unwrap());
    let row = |v: u32| {
        register
            .iter()
            .find(|r| r.kind == ConfigKind::RateTable && r.id == "life-base" && r.version == v)
            .unwrap()
            .clone()
    };
    assert!((row(2).in_use, row(1).in_use) == (true, false));

    let w = withdrawal(ConfigKind::RateTable, "life-base", 2, false);
    assert_eq!(
        code(c.submit(0, "new-business", Command::WithdrawConfig(Box::new(w.clone())))),
        "caller_not_allowed"
    );
    let unapproved = Withdrawal {
        approved_by: vec![],
        ..w.clone()
    };
    assert_eq!(code(withdraw(&mut c, unapproved)), "invalid_withdrawal");
    let unknown = Withdrawal { version: 9, ..w.clone() };
    assert_eq!(code(withdraw(&mut c, unknown)), "config_conflict");
    let retire_table = Withdrawal { retire: true, ..w.clone() };
    assert_eq!(code(withdraw(&mut c, retire_table)), "config_conflict");
    withdraw(&mut c, w.clone()).unwrap();
    withdraw(&mut c, w).unwrap();
    let announced = c.take_events().into_iter().filter(|e| matches!(e.event, Event::ConfigWithdrawn(_))).count();
    assert_eq!(announced, 1);

    let register = c.state(0).catalog.register(c.business_date().unwrap());
    let row = |v: u32| {
        register
            .iter()
            .find(|r| r.kind == ConfigKind::RateTable && r.id == "life-base" && r.version == v)
            .unwrap()
            .clone()
    };
    assert!((row(2).in_use, row(1).in_use) == (false, true));
    let recorded = row(2).withdrawn.unwrap();
    assert_eq!(
        (recorded.on, recorded.change.as_str(), recorded.approved_by.clone()),
        (Some(d(2027, 12, 16)), "CHG-2041", vec!["ops-ann".to_string()])
    );

    // The bill raised on the bad version stands; the next is at the old rates.
    let raised = c.state(0).policies[&id]
        .receivables
        .iter()
        .find(|r| r.period_start == d(2027, 12, 15))
        .unwrap()
        .total_minor;
    assert_eq!(raised, wrong[0].1);
    until(&mut c, d(2028, 1, 16));
    assert_eq!(bills(&mut c, &id), vec![(d(2028, 1, 15), good)]);
    let fresh = issue(&mut c, 2);
    assert_eq!(bills(&mut c, &fresh)[0].1, good);
}

/// Every rate in a rate table source multiplied by ten.
fn regex_free_scale(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.trim_start().strip_prefix("rate(\"") {
            Some(rest) => {
                let (value, tail) = rest.split_once('"').unwrap();
                let scaled = ic_money::dec(value).unwrap() * ic_money::dec("10").unwrap();
                format!("        rate(\"{scaled}\"{tail}")
            }
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Purpose: matrix CFG-011. A product version still held by a policy in force cannot be
/// retired; a later version nobody bought can be, after which new business is written on the
/// version before it again; and withdrawing first and retiring after is allowed.
/// Why it matters: a policy is priced and bound by its product version for life. Retiring one
/// that policies still hold would leave them with nothing to bill or settle claims against.
#[test]
fn a_product_version_still_in_force_cannot_be_retired() {
    let mut c = cluster();
    let held = issue(&mut c, 1);
    let v1 = c.state(0).policies[&held].product_hash.clone();
    let source = include_str!("../../../products/term_life.star")
        .replace("version = 1,", "version = 2,")
        .replace("effective_from = \"2027-01-01\"", "effective_from = \"2027-11-15\"")
        .replace("policy_fee_annual = \"72.00\"", "policy_fee_annual = \"84.00\"");
    let v2 = load_product("term_life_v2.star", &source).unwrap();
    let v2_hash = v2.content_hash.clone();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(v2) }).unwrap();
    assert_eq!(c.state(0).catalog.product_on("term-life", start()).unwrap().content_hash, v2_hash);

    let refused = withdraw(&mut c, withdrawal(ConfigKind::Product, "term-life", 1, true)).unwrap_err();
    assert_eq!(refused.code, "version_in_use");
    assert!(refused.message.contains("held by 1 policies"), "{}", refused.message);

    withdraw(&mut c, withdrawal(ConfigKind::Product, "term-life", 2, false)).unwrap();
    withdraw(&mut c, withdrawal(ConfigKind::Product, "term-life", 2, true)).unwrap();
    assert!(c.state(0).catalog.withdrawal(ConfigKind::Product, "term-life", 2).unwrap().retire);
    let fresh = issue(&mut c, 2);
    assert_eq!(c.state(0).policies[&fresh].product_hash, v1);
}
