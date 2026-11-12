//! What the core does for underwriting and the migration load: ending
//! interim cover, and policies that arrive from another system.

use chrono::NaiveDate;
use ic_core::{Command, PolicyId, PolicyStatus, Reject};
use ic_product::{Frequency, Sex};
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

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn code(r: Result<(), Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

/// Purpose: underwriting can cancel a policy, as it must to end interim cover when the proposal
/// it stood in for ends; a module with no business cancelling still cannot; and the books
/// reconcile on the days after.
/// Why it matters: interim cover left running beside the policy that replaced it, or after a
/// declined proposal, is cover nobody priced. If underwriting could not end it, it would run
/// until somebody noticed.
#[test]
fn underwriting_can_end_interim_cover() {
    let mut c = cluster();
    let issue = term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 250_000_00, None, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    days(&mut c, 3);

    assert_eq!(code(c.submit(0, "claims", Command::CancelPolicy { policy: id.clone() })), "caller_not_allowed");
    c.submit(0, "underwriting", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(c.state(0).policies[&id].status, PolicyStatus::Cancelled);
    assert_eq!(code(c.submit(0, "underwriting", Command::CancelPolicy { policy: id.clone() })), "not_in_force");
    days(&mut c, 3);
}

/// Purpose: a policy from another system is issued by the migration load through the ordinary
/// issue command, marked as migrated and with its original inception; nobody else can mark a
/// policy as migrated, the migration load cannot issue one that is not, and a command written
/// before the flag existed still reads as an ordinary issue.
/// Why it matters: there is no back door for migrated business, so the flag is the only thing
/// that tells a migrated policy from one written here. If new business could set it, or the
/// migration load could leave it off, that record would be wrong for the life of the policy.
#[test]
fn only_the_migration_load_issues_a_migrated_policy() {
    let mut c = cluster();
    let mut legacy = term_life(2, d(2027, 9, 15), d(1985, 6, 1), Sex::Male, false, 300_000_00, None, Frequency::Monthly, None);
    let id: PolicyId = legacy.policy.clone();

    assert_eq!(code(c.submit(0, "migration", issue_command(legacy.clone()))), "migrated");
    legacy.migrated = true;
    assert_eq!(code(c.submit(0, "new-business", issue_command(legacy.clone()))), "migrated");
    assert!(!c.state(0).policies.contains_key(&id));

    c.submit(0, "migration", issue_command(legacy.clone())).unwrap();
    let policy = &c.state(0).policies[&id];
    assert!(policy.migrated);
    assert_eq!(policy.inception, d(2027, 9, 15));
    days(&mut c, 3);

    // The flag is absent from an ordinary command on the wire, and absence reads as false.
    let ordinary = term_life(3, start(), d(1990, 3, 10), Sex::Female, false, 250_000_00, None, Frequency::Monthly, None);
    let wire = serde_json::to_value(&ordinary).unwrap();
    assert!(wire.get("migrated").is_none());
    let read: ic_core::command::IssuePolicy = serde_json::from_value(wire).unwrap();
    assert!(!read.migrated);
    c.submit(0, "new-business", issue_command(ordinary.clone())).unwrap();
    assert!(!c.state(0).policies[&ordinary.policy].migrated);
}

/// Purpose: a policy issued on quoted terms is issued only while the product version the quote
/// was priced on is still the one in force for the inception date; a different version is
/// refused whole, with nothing written; and only new business may issue this way.
/// Why it matters: an applicant accepts an offer at a price. If the product changed between
/// the offer and the issue, issuing anyway would bind the insurer and the customer to terms
/// neither of them saw.
#[test]
fn a_quoted_issue_is_refused_when_the_product_has_changed() {
    let mut c = cluster();
    let application = term_life(4, start(), d(1990, 3, 10), Sex::Female, false, 250_000_00, None, Frequency::Monthly, None);
    let id = application.policy.clone();
    let quoted = |hash: &str| Command::IssueQuoted {
        expected_product_hash: hash.to_string(),
        issue: Box::new(application.clone()),
    };

    assert_eq!(code(c.submit(0, "new-business", quoted("the-version-before-this-one"))), "product_changed");
    assert!(!c.state(0).policies.contains_key(&id));
    let mut unknown = application.clone();
    unknown.product_code = "no-such-product".into();
    let no_product = Command::IssueQuoted {
        expected_product_hash: "x".into(),
        issue: Box::new(unknown),
    };
    assert_eq!(code(c.submit(0, "new-business", no_product)), "product_changed");

    // The hash the core itself holds for the product is the one a quote carries.
    let other = term_life(5, start(), d(1990, 3, 10), Sex::Female, false, 250_000_00, None, Frequency::Monthly, None);
    c.submit(0, "new-business", issue_command(other.clone())).unwrap();
    let in_force = c.state(0).policies[&other.policy].product_hash.clone();

    assert_eq!(code(c.submit(0, "group-admin", quoted(&in_force))), "caller_not_allowed");
    c.submit(0, "new-business", quoted(&in_force)).unwrap();
    let policy = &c.state(0).policies[&id];
    assert_eq!((policy.status, policy.product_hash.as_str()), (PolicyStatus::InForce, in_force.as_str()));
    assert_eq!(code(c.submit(0, "new-business", quoted(&in_force))), "policy_exists");
    days(&mut c, 3);
}

fn increase(policy: &PolicyId, cover: &str, sum: i64, loading: Option<&str>) -> Command {
    Command::AddUnderwrittenLayer(Box::new(ic_core::command::UnderwrittenLayer {
        policy: policy.clone(),
        cover: cover.into(),
        life: ic_core::LifeId("life-1".into()),
        sum_insured_minor: sum,
        loadings: loading.map(|l| ic_money::DecStr::parse(l).unwrap()).into_iter().collect(),
        exclusions: vec!["EX-BACK".into()],
        reference: "uw-decision-1".into(),
    }))
}

/// Purpose: underwriting adds cover to a retail policy part way through a month already
/// billed. The increase is its own layer from that day with its own loading and exclusion; the
/// days left of the month are billed at once as an adjustment in proportion; the next month's
/// bill prices both layers in full; an increase accepted on the very day a period starts needs
/// no adjustment; and the books reconcile on every day throughout.
/// Why it matters: an accepted increase is cover the customer is owed from the day it is
/// accepted and premium the insurer is owed for the same days. Billing nothing until the next
/// period gives cover away; billing the whole month charges for days before the cover existed;
/// and the original layer must keep its own terms, since a claim is assessed layer by layer.
#[test]
fn an_accepted_increase_is_a_layer_from_today_billed_for_the_days_left() {
    let mut c = cluster();
    let application = term_life(11, start(), d(1988, 6, 2), Sex::Male, false, 400_000_00, None, Frequency::Monthly, None);
    let id = application.policy.clone();
    c.submit(0, "new-business", issue_command(application)).unwrap();
    days(&mut c, 10);
    let today = c.business_date().unwrap();
    assert_eq!(today, d(2027, 11, 25));
    let first = c.state(0).policies[&id].receivables[0].clone();
    assert_eq!((first.period_start, first.period_end), (start(), d(2027, 12, 15)));
    c.take_events();

    c.submit(0, "underwriting", increase(&id, "life", 100_000_00, Some("0.5"))).unwrap();
    let policy = c.state(0).policies[&id].clone();
    let layers = &policy.covers["life"].layers;
    assert_eq!(layers.len(), 2);
    assert_eq!((layers[0].sum_insured_minor, layers[0].loadings.len(), layers[0].exclusions.len()), (400_000_00, 0, 0));
    let added = &layers[1];
    assert_eq!(
        (added.id, added.start, added.end, added.sum_insured_minor, added.origin),
        (2, today, None, 100_000_00, ic_core::LayerOrigin::Increase)
    );
    assert_eq!((added.loadings[0].as_str(), added.exclusions.as_slice()), ("0.5", &["EX-BACK".to_string()][..]));

    // The adjustment: the days left of the billed month, in proportion.
    let adjustment = policy.receivables.last().unwrap().clone();
    assert_eq!(adjustment.kind, ic_core::policy::ReceivableKind::Adjustment);
    assert_eq!((adjustment.period_start, adjustment.period_end, adjustment.due), (today, d(2027, 12, 15), today));
    assert!(adjustment.total_minor > 0 && adjustment.total_minor < first.total_minor, "{} of {}", adjustment.total_minor, first.total_minor);
    let events = c.take_events();
    assert!(events.iter().any(|e| matches!(&e.event,
        ic_core::Event::LayerAdded { layer, origin: ic_core::LayerOrigin::Increase, .. } if layer.layer == 2 && layer.start == today && layer.sum_insured_minor == 100_000_00)));
    assert!(events.iter().any(|e| matches!(&e.event,
        ic_core::Event::PremiumAdjusted { effective, total_minor, .. } if *effective == today && *total_minor == adjustment.total_minor)));
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // The next month prices both layers for the whole month: the adjustment was 20 days of 30
    // of exactly that difference, to the cent or so that rounding each line allows.
    days(&mut c, 21);
    let policy = c.state(0).policies[&id].clone();
    let second = policy.receivables.iter().find(|r| r.period_start == d(2027, 12, 15)).unwrap();
    let month = second.total_minor - first.total_minor;
    assert!(month > 0);
    let expected = month * 20 / 30;
    assert!((adjustment.total_minor - expected).abs() <= 3, "{} against {expected}", adjustment.total_minor);

    // Accepted on the first day of a month already billed: the whole month's difference.
    days(&mut c, 30);
    assert_eq!(c.business_date().unwrap(), d(2028, 1, 15));
    c.submit(0, "underwriting", increase(&id, "life", 50_000_00, None)).unwrap();
    let policy = c.state(0).policies[&id].clone();
    assert_eq!(policy.covers["life"].layers[2].start, d(2028, 1, 15));
    let whole = policy.receivables.last().unwrap().clone();
    assert_eq!((whole.kind, whole.period_start, whole.period_end), (ic_core::policy::ReceivableKind::Adjustment, d(2028, 1, 15), d(2028, 2, 15)));
    // Half the sum of the first increase and none of its 50% loading: a third of its month.
    assert!((whole.total_minor - month / 3).abs() <= 3, "{} against a third of {month}", whole.total_minor);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    days(&mut c, 3);
}

/// Purpose: an increase is refused whole, with nothing written, from any caller but
/// underwriting; on a policy that is not in force; for a cover or a life the policy does not
/// have; for a sum that is not positive or takes the life above the product's maximum; for a
/// negative loading; and while billing has not caught up to today.
/// Why it matters: this command writes cover and premium on a policy in force. Each refusal is
/// a way of writing cover nobody priced: above the product's limit, on a cancelled policy, or
/// for days that billing would then never charge for.
#[test]
fn an_increase_that_cannot_be_priced_or_is_not_underwritings_is_refused() {
    let mut c = cluster();
    let application = term_life(12, start(), d(1988, 6, 2), Sex::Male, false, 400_000_00, None, Frequency::Monthly, None);
    let id = application.policy.clone();
    c.submit(0, "new-business", issue_command(application)).unwrap();
    days(&mut c, 2);
    let untouched = c.state(0).policies[&id].clone();

    assert_eq!(code(c.submit(0, "policy-servicing", increase(&id, "life", 1_00, None))), "caller_not_allowed");
    assert_eq!(code(c.submit(0, "underwriting", increase(&id, "trauma", 50_000_00, None))), "unknown_cover");
    assert_eq!(code(c.submit(0, "underwriting", increase(&id, "life", 0, None))), "sum_insured");
    assert_eq!(code(c.submit(0, "underwriting", increase(&id, "life", 4_700_000_00, None))), "sum_insured");
    assert_eq!(code(c.submit(0, "underwriting", increase(&id, "life", 50_000_00, Some("-0.1")))), "invalid_loading");
    let mut stranger = increase(&id, "life", 50_000_00, None);
    if let Command::AddUnderwrittenLayer(spec) = &mut stranger {
        spec.life = ic_core::LifeId("life-9".into());
    }
    assert_eq!(code(c.submit(0, "underwriting", stranger)), "unknown_life");
    assert!(c.state(0).policies[&id] == untouched, "a refused increase wrote nothing");

    // The date reaches the next period's first day before that day's billing has run: the
    // layer is simply there when the period is billed. A day later billing is behind.
    let mut date = c.business_date().unwrap();
    while date < d(2027, 12, 15) {
        date = date.succ_opt().unwrap();
        c.submit(0, "leader", Command::AdvanceBusinessDate { to: date }).unwrap();
    }
    let billed = c.state(0).policies[&id].receivables.len();
    c.submit(0, "underwriting", increase(&id, "life", 50_000_00, None)).unwrap();
    assert_eq!(c.state(0).policies[&id].receivables.len(), billed, "nothing is billed until the period is");
    c.submit(0, "leader", Command::AdvanceBusinessDate { to: d(2027, 12, 16) }).unwrap();
    assert_eq!(code(c.submit(0, "underwriting", increase(&id, "life", 50_000_00, None))), "billing_behind");
    c.submit(0, "end-of-day", Command::RunEndOfDay { policies: vec![id.clone()] }).unwrap();
    let policy = c.state(0).policies[&id].clone();
    let december = policy.receivables.iter().find(|r| r.period_start == d(2027, 12, 15)).unwrap();
    assert!(december.total_minor > policy.receivables[0].total_minor, "the period was billed with the layer in it");
    c.submit(0, "underwriting", increase(&id, "life", 50_000_00, None)).unwrap();
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    c.submit(0, "underwriting", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(code(c.submit(0, "underwriting", increase(&id, "life", 50_000_00, None))), "not_in_force");
    days(&mut c, 2);
}
