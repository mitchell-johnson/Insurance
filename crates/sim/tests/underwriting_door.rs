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
    assert_eq!(
        (layers[0].sum_insured_minor, layers[0].loadings.len(), layers[0].exclusions.len()),
        (400_000_00, 0, 0)
    );
    let added = &layers[1];
    assert_eq!(
        (added.id, added.start, added.end, added.sum_insured_minor, added.origin),
        (2, today, None, 100_000_00, ic_core::LayerOrigin::Increase)
    );
    assert_eq!((added.loadings[0].as_str(), added.exclusions.as_slice()), ("0.5", &["EX-BACK".to_string()][..]));

    // The adjustment: the days left of the billed month, in proportion.
    let adjustment = policy.receivables.last().unwrap().clone();
    assert_eq!(adjustment.kind, ic_core::policy::ReceivableKind::Adjustment);
    assert_eq!(
        (adjustment.period_start, adjustment.period_end, adjustment.due),
        (today, d(2027, 12, 15), today)
    );
    assert!(
        adjustment.total_minor > 0 && adjustment.total_minor < first.total_minor,
        "{} of {}",
        adjustment.total_minor,
        first.total_minor
    );
    let events = c.take_events();
    assert!(events.iter().any(|e| matches!(&e.event,
        ic_core::Event::LayerAdded { layer, origin: ic_core::LayerOrigin::Increase, continues: None, .. } if layer.layer == 2 && layer.start == today && layer.sum_insured_minor == 100_000_00)));
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
    assert_eq!(
        (whole.kind, whole.period_start, whole.period_end),
        (ic_core::policy::ReceivableKind::Adjustment, d(2028, 1, 15), d(2028, 2, 15))
    );
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
    assert_eq!(
        code(c.submit(0, "underwriting", increase(&id, "life", 50_000_00, Some("-0.1")))),
        "invalid_loading"
    );
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
    assert!(
        december.total_minor > policy.receivables[0].total_minor,
        "the period was billed with the layer in it"
    );
    c.submit(0, "underwriting", increase(&id, "life", 50_000_00, None)).unwrap();
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    c.submit(0, "underwriting", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(code(c.submit(0, "underwriting", increase(&id, "life", 50_000_00, None))), "not_in_force");
    days(&mut c, 2);
}

fn terms(policy: &PolicyId, life: &str, cover: &str, loading: Option<&str>, exclusions: &[&str]) -> Command {
    Command::AlterUnderwrittenTerms(Box::new(ic_core::command::AlteredTerms {
        policy: policy.clone(),
        cover: cover.into(),
        life: ic_core::LifeId(life.into()),
        loadings: loading.map(|l| ic_money::DecStr::parse(l).unwrap()).into_iter().collect(),
        exclusions: exclusions.iter().map(|e| e.to_string()).collect(),
        reference: "uw-decision-2".into(),
    }))
}

/// Purpose: underwriting changes the terms on cover a life already holds, part way through a
/// month already billed. Only the layers whose terms differ are ended and written again, each
/// with its sum insured, its origin and the date its premium is rated from, and announced as
/// carrying on the layer it replaces; a loading put on
/// bills the days left of the month, a loading taken off credits them; asking for the terms
/// the cover already has is refused; and the books reconcile throughout.
/// Why it matters: an alteration must not look like new cover. A layer that lost its origin
/// would pay commission again, one that lost its rating date would reprice a level premium at
/// today's age, and one that kept its old loading after underwriting removed it would go on
/// charging a customer for a risk the insurer has agreed is gone.
#[test]
fn a_change_of_terms_rewrites_each_layer_and_bills_or_credits_the_days_left() {
    let mut c = cluster();
    let application = term_life(13, start(), d(1988, 6, 2), Sex::Male, false, 400_000_00, None, Frequency::Monthly, None);
    let id = application.policy.clone();
    c.submit(0, "new-business", issue_command(application)).unwrap();
    days(&mut c, 10);
    let today = c.business_date().unwrap();
    c.submit(0, "underwriting", increase(&id, "life", 100_000_00, Some("0.5"))).unwrap();
    let before = c.state(0).policies[&id].clone();
    let increase_adjustment = before.receivables.last().unwrap().total_minor;
    c.take_events();

    // The increase already carries these terms, so only the original layer changes.
    c.submit(0, "underwriting", terms(&id, "life-1", "life", Some("0.5"), &["EX-BACK"])).unwrap();
    let policy = c.state(0).policies[&id].clone();
    let layers = &policy.covers["life"].layers;
    assert_eq!(layers.len(), 3);
    assert_eq!((layers[0].end, layers[1].end), (Some(today), None));
    let rewritten = &layers[2];
    assert_eq!(
        (rewritten.id, rewritten.start, rewritten.end, rewritten.sum_insured_minor, rewritten.origin),
        (3, today, None, 400_000_00, ic_core::LayerOrigin::Original)
    );
    assert_eq!(rewritten.rated_from, Some(start()), "the premium is still rated from the day the cover began");
    assert_eq!(
        (rewritten.loadings[0].as_str(), rewritten.exclusions.as_slice()),
        ("0.5", &["EX-BACK".to_string()][..])
    );
    let events = c.take_events();
    assert!(events.iter().any(|e| matches!(&e.event,
        ic_core::Event::LayerEnded { layer: 1, effective, sum_insured_minor: 400_000_00, .. } if *effective == today)));
    assert!(events.iter().any(|e| matches!(&e.event,
        ic_core::Event::LayerAdded { layer, origin: ic_core::LayerOrigin::Original, continues: Some(1), .. } if layer.layer == 3 && layer.start == today && layer.sum_insured_minor == 400_000_00)));
    // Cover that is new says so by saying nothing: only a rewritten layer names the one it carries on.
    let json = serde_json::to_value(&events.iter().find(|e| matches!(&e.event, ic_core::Event::LayerAdded { .. })).unwrap().event).unwrap();
    assert_eq!(json["layer_added"]["continues"], 1);

    // Four times the sum of the increase at the same 50% loading, but the loading alone: the
    // increase's adjustment was 100,000 at 150%, this one is 400,000 at 50%.
    let loaded = policy.receivables.last().unwrap().clone();
    assert_eq!(
        (loaded.kind, loaded.period_start, loaded.period_end, loaded.due),
        (ic_core::policy::ReceivableKind::Adjustment, today, d(2027, 12, 15), today)
    );
    let expected = increase_adjustment * 4 / 3;
    assert!((loaded.total_minor - expected).abs() <= 4, "{} against {expected}", loaded.total_minor);
    assert!(events.iter().any(|e| matches!(&e.event,
        ic_core::Event::PremiumAdjusted { effective, total_minor, .. } if *effective == today && *total_minor == loaded.total_minor)));
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // The same terms again change nothing, and say so.
    let unchanged = c.state(0).policies[&id].clone();
    assert_eq!(
        code(c.submit(0, "underwriting", terms(&id, "life-1", "life", Some("0.5"), &["EX-BACK"]))),
        "no_change"
    );
    assert!(c.state(0).policies[&id] == unchanged);

    // Five days on underwriting takes the loading and the exclusion off all of it: both
    // layers in force are rewritten and the days left are credited.
    days(&mut c, 5);
    let later = c.business_date().unwrap();
    assert_eq!(later, d(2027, 11, 30));
    c.submit(0, "underwriting", terms(&id, "life-1", "life", None, &[])).unwrap();
    let policy = c.state(0).policies[&id].clone();
    let layers = &policy.covers["life"].layers;
    assert_eq!(layers.len(), 5);
    assert!(layers[..3].iter().all(|l| l.end.is_some()));
    let standard: Vec<_> = layers[3..]
        .iter()
        .map(|l| (l.sum_insured_minor, l.origin, l.rated_from, l.loadings.len(), l.exclusions.len(), l.start))
        .collect();
    assert_eq!(
        standard,
        vec![
            (100_000_00, ic_core::LayerOrigin::Increase, Some(today), 0, 0, later),
            (400_000_00, ic_core::LayerOrigin::Original, Some(start()), 0, 0, later),
        ]
    );
    let credit = policy.receivables.last().unwrap().clone();
    assert_eq!(
        (credit.kind, credit.period_start, credit.period_end),
        (ic_core::policy::ReceivableKind::Adjustment, later, d(2027, 12, 15))
    );
    // 500,000 of loading at 50% for 15 days, against 400,000 of it for 20.
    let expected = -(loaded.total_minor * 5 / 4 * 15 / 20);
    assert!(
        credit.total_minor < 0 && (credit.total_minor - expected).abs() <= 4,
        "{} against {expected}",
        credit.total_minor
    );
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // The next month is billed on the terms now in force: standard rates on all 500,000.
    days(&mut c, 16);
    let policy = c.state(0).policies[&id].clone();
    let first = &policy.receivables[0];
    let december = policy.receivables.iter().find(|r| r.period_start == d(2027, 12, 15)).unwrap();
    assert_eq!(december.kind, ic_core::policy::ReceivableKind::Period);
    assert!(december.total_minor > first.total_minor && december.total_minor < first.total_minor * 3 / 2);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}

/// Purpose: a change of terms is refused whole, with nothing written, from any caller but
/// underwriting; for a cover the policy does not have; for a negative loading; for a life with
/// no cover to change; while billing has not caught up to today; and on a policy no longer in
/// force. A change on the first day of a period not yet billed writes the layers and bills
/// nothing, since the period's own bill will price them.
/// Why it matters: the command rewrites the terms a claim is assessed on and the price of the
/// cover. Accepting it from another caller would let terms change without a decision behind
/// them; accepting it while billing is behind would leave days nobody is charged for.
#[test]
fn a_change_of_terms_that_cannot_be_priced_or_is_not_underwritings_is_refused() {
    let mut c = cluster();
    let application = term_life(14, start(), d(1988, 6, 2), Sex::Male, false, 400_000_00, None, Frequency::Monthly, None);
    let id = application.policy.clone();
    c.submit(0, "new-business", issue_command(application)).unwrap();
    days(&mut c, 2);
    let untouched = c.state(0).policies[&id].clone();

    assert_eq!(
        code(c.submit(0, "policy-servicing", terms(&id, "life-1", "life", Some("0.25"), &[]))),
        "caller_not_allowed"
    );
    assert_eq!(
        code(c.submit(0, "underwriting", terms(&id, "life-1", "trauma", Some("0.25"), &[]))),
        "unknown_cover"
    );
    assert_eq!(
        code(c.submit(0, "underwriting", terms(&id, "life-1", "life", Some("-0.25"), &[]))),
        "invalid_loading"
    );
    assert_eq!(code(c.submit(0, "underwriting", terms(&id, "life-9", "life", Some("0.25"), &[]))), "no_change");
    assert_eq!(code(c.submit(0, "underwriting", terms(&id, "life-1", "life", None, &[]))), "no_change");
    assert!(c.state(0).policies[&id] == untouched, "a refused change wrote nothing");

    let mut date = c.business_date().unwrap();
    while date < d(2027, 12, 15) {
        date = date.succ_opt().unwrap();
        c.submit(0, "leader", Command::AdvanceBusinessDate { to: date }).unwrap();
    }
    let billed = c.state(0).policies[&id].receivables.len();
    c.submit(0, "underwriting", terms(&id, "life-1", "life", None, &["EX-BACK"])).unwrap();
    let policy = c.state(0).policies[&id].clone();
    assert_eq!(policy.receivables.len(), billed, "nothing is billed until the period is");
    assert_eq!(policy.covers["life"].layers.len(), 2);
    c.submit(0, "leader", Command::AdvanceBusinessDate { to: d(2027, 12, 16) }).unwrap();
    assert_eq!(
        code(c.submit(0, "underwriting", terms(&id, "life-1", "life", Some("0.25"), &["EX-BACK"]))),
        "billing_behind"
    );
    c.submit(0, "end-of-day", Command::RunEndOfDay { policies: vec![id.clone()] }).unwrap();
    c.submit(0, "underwriting", terms(&id, "life-1", "life", Some("0.25"), &["EX-BACK"])).unwrap();
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    c.submit(0, "underwriting", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(code(c.submit(0, "underwriting", terms(&id, "life-1", "life", None, &[]))), "not_in_force");
    days(&mut c, 2);
}

/// A term life policy on a copy of the shipped product that lets a lapse of up to thirty days
/// be reinstated on payment alone.
fn evidence_after_thirty_days(c: &mut Cluster, n: u64) -> PolicyId {
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-evidence\"")
        .replace("reinstatement_evidence_free_days = 60", "reinstatement_evidence_free_days = 30");
    let product = ic_product::loader::load_product("term_life_evidence.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let mut issue = term_life(n, start(), d(1988, 6, 2), Sex::Male, false, 400_000_00, None, Frequency::Monthly, None);
    issue.product_code = "term-life-evidence".into();
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

fn approve(policy: &PolicyId, valid_until: NaiveDate, terms: Vec<ic_core::policy::LayerTerms>) -> Command {
    Command::ApproveReinstatement(Box::new(ic_core::command::ReinstatementTerms {
        policy: policy.clone(),
        reference: "uw-decision-3".into(),
        valid_until,
        terms,
    }))
}

fn loaded(life: &str, cover: &str, loading: &str) -> ic_core::policy::LayerTerms {
    ic_core::policy::LayerTerms {
        cover: cover.into(),
        life: ic_core::LifeId(life.into()),
        loadings: vec![ic_money::DecStr::parse(loading).unwrap()],
        exclusions: vec!["EX-BACK".into()],
    }
}

fn pay(c: &mut Cluster, policy: &PolicyId, receipt: &str) {
    let amount = ic_money::Money::new(20_000_00, ic_money::Currency::NZD);
    let receipt = receipt.to_string();
    c.submit(
        0,
        "cash-matching",
        Command::ApplyReceipt {
            policy: policy.clone(),
            receipt,
            amount,
        },
    )
    .unwrap();
}

fn reinstate(policy: &PolicyId) -> Command {
    Command::Reinstate { policy: policy.clone() }
}

/// Purpose: where the product limits reinstatement on payment alone, a policy lapsed for less
/// than that comes back when the arrears are paid; one lapsed longer is refused until
/// underwriting has decided, and a decision that has gone stale counts for nothing. With the
/// decision, the policy is reinstated and the cover goes onto underwriting's terms from that
/// day: the layer is rewritten with its sum insured, origin and rating date, the days before
/// are owed on the old terms, the days left of the month on the new, and the books reconcile.
/// Why it matters: after months off risk the insurer is entitled to ask about the life's
/// health before taking the risk back. Reinstating on payment alone lets a customer who has
/// just had bad news buy back cover at the old price; and terms that were decided but not
/// applied leave the policy priced, and claims assessed, as if nobody had asked.
#[test]
fn a_long_lapse_is_reinstated_only_on_underwritings_decision_and_on_its_terms() {
    let mut c = cluster();
    let soon = evidence_after_thirty_days(&mut c, 21);
    let late = evidence_after_thirty_days(&mut c, 22);
    let stale = evidence_after_thirty_days(&mut c, 23);
    days(&mut c, 91);
    let lapsed_on = c.business_date().unwrap();
    assert_eq!(lapsed_on, d(2028, 2, 14));
    assert!([&soon, &late, &stale].iter().all(|id| c.state(0).policies[*id].status == PolicyStatus::Lapsed));

    // Thirty days lapsed: payment is still enough.
    days(&mut c, 30);
    pay(&mut c, &soon, "soon");
    c.submit(0, "collections", reinstate(&soon)).unwrap();
    assert_eq!(c.state(0).policies[&soon].status, PolicyStatus::InForce);

    // Forty days: not without underwriting, however much is paid.
    days(&mut c, 10);
    let today = c.business_date().unwrap();
    assert_eq!(today, d(2028, 3, 25));
    pay(&mut c, &late, "late");
    pay(&mut c, &stale, "stale");
    assert_eq!(code(c.submit(0, "collections", reinstate(&late))), "underwriting_required");
    assert_eq!(c.state(0).policies[&late].status, PolicyStatus::Lapsed);

    c.submit(0, "underwriting", approve(&late, d(2028, 4, 24), vec![loaded("life-1", "life", "0.5")]))
        .unwrap();
    c.take_events();
    c.submit(0, "collections", reinstate(&late)).unwrap();
    let policy = c.state(0).policies[&late].clone();
    assert_eq!((policy.status, policy.lapse.is_none()), (PolicyStatus::InForce, true));
    let layers = &policy.covers["life"].layers;
    assert_eq!(layers.len(), 2);
    assert_eq!((layers[0].end, layers[1].start, layers[1].end), (Some(today), today, None));
    assert_eq!(
        (layers[1].sum_insured_minor, layers[1].origin, layers[1].rated_from),
        (400_000_00, ic_core::LayerOrigin::Original, Some(start()))
    );
    assert_eq!(
        (layers[1].loadings[0].as_str(), layers[1].exclusions.as_slice()),
        ("0.5", &["EX-BACK".to_string()][..])
    );
    // The month from 15 March was billed on the old terms; its last 21 days carry the loading.
    let month = policy.receivables.iter().find(|r| r.period_start == d(2028, 3, 15)).unwrap().clone();
    let adjustment = policy.receivables.last().unwrap().clone();
    assert_eq!(
        (adjustment.kind, adjustment.period_start, adjustment.period_end),
        (ic_core::policy::ReceivableKind::Adjustment, today, d(2028, 4, 15))
    );
    assert!(
        adjustment.total_minor > 0 && adjustment.total_minor < month.total_minor / 2,
        "{} against a month of {}",
        adjustment.total_minor,
        month.total_minor
    );
    let events = c.take_events();
    let at = |wanted: fn(&ic_core::Event) -> bool| events.iter().position(|e| wanted(&e.event)).unwrap();
    assert!(
        at(|e| matches!(e, ic_core::Event::PolicyReinstated { .. })) < at(|e| matches!(e, ic_core::Event::LayerEnded { .. })),
        "the policy is back in force before its terms change"
    );
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // A decision good until today is no decision tomorrow.
    c.submit(0, "underwriting", approve(&stale, today, vec![])).unwrap();
    days(&mut c, 1);
    assert_eq!(code(c.submit(0, "collections", reinstate(&stale))), "underwriting_required");
    c.submit(0, "underwriting", approve(&stale, d(2028, 4, 24), vec![])).unwrap();
    c.submit(0, "collections", reinstate(&stale)).unwrap();
    let policy = c.state(0).policies[&stale].clone();
    assert_eq!((policy.status, policy.covers["life"].layers.len()), (PolicyStatus::InForce, 1));
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    days(&mut c, 25);
}

/// Purpose: underwriting's decision on a lapsed policy is refused, with nothing written, from
/// any other caller; on a policy that is not lapsed; with no reference; already out of date;
/// for a cover or a life the policy does not have; for a negative loading; and after the
/// reinstatement window has closed. Inside the product's evidence-free days, a decision that
/// carries terms is still applied when the policy is reinstated.
/// Why it matters: the decision is what lets a risk back onto the books after months off
/// them, and it can change the terms a claim is assessed on. Each refusal is a way of
/// recording one that nobody made, or that could not be applied when the day came.
#[test]
fn a_reinstatement_decision_that_cannot_stand_is_refused() {
    let mut c = cluster();
    let application = term_life(24, start(), d(1988, 6, 2), Sex::Male, false, 400_000_00, None, Frequency::Monthly, None);
    let id = application.policy.clone();
    c.submit(0, "new-business", issue_command(application)).unwrap();
    let good_until = d(2028, 6, 1);
    assert_eq!(code(c.submit(0, "underwriting", approve(&id, good_until, vec![]))), "not_lapsed");
    days(&mut c, 96);
    let today = c.business_date().unwrap();
    let lapsed = c.state(0).policies[&id].clone();
    assert_eq!(lapsed.status, PolicyStatus::Lapsed);

    assert_eq!(code(c.submit(0, "collections", approve(&id, good_until, vec![]))), "caller_not_allowed");
    let mut unnamed = approve(&id, good_until, vec![]);
    if let Command::ApproveReinstatement(spec) = &mut unnamed {
        spec.reference.clear();
    }
    assert_eq!(code(c.submit(0, "underwriting", unnamed)), "reference");
    assert_eq!(
        code(c.submit(0, "underwriting", approve(&id, today.pred_opt().unwrap(), vec![]))),
        "decision_expired"
    );
    for (terms, refusal) in [
        (loaded("life-1", "trauma", "0.5"), "unknown_cover"),
        (loaded("life-9", "life", "0.5"), "unknown_life"),
        (loaded("life-1", "life", "-0.5"), "invalid_loading"),
    ] {
        assert_eq!(code(c.submit(0, "underwriting", approve(&id, good_until, vec![terms]))), refusal);
    }
    assert!(c.state(0).policies[&id] == lapsed, "a refused decision wrote nothing");

    // Five days lapsed is inside the shipped product's evidence-free days, but terms
    // underwriting decided still apply.
    c.submit(0, "underwriting", approve(&id, good_until, vec![loaded("life-1", "life", "0.25")]))
        .unwrap();
    pay(&mut c, &id, "catch-up");
    c.submit(0, "collections", reinstate(&id)).unwrap();
    let policy = c.state(0).policies[&id].clone();
    assert_eq!(policy.status, PolicyStatus::InForce);
    assert_eq!(policy.covers["life"].layers.last().unwrap().loadings[0].as_str(), "0.25");
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    let closed = term_life(
        25,
        c.business_date().unwrap(),
        d(1988, 6, 2),
        Sex::Male,
        false,
        400_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    let other = closed.policy.clone();
    c.submit(0, "new-business", issue_command(closed)).unwrap();
    days(&mut c, 91 + 181);
    assert_eq!(code(c.submit(0, "underwriting", approve(&other, d(2029, 6, 1), vec![]))), "window_closed");
}
