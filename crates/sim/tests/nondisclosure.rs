//! The remedies for non-disclosure as the core carries them out: avoiding a
//! policy with the premiums refunded or kept, and varying its terms.

use chrono::NaiveDate;
use ic_core::accounts::{self, role};
use ic_core::event::Event;
use ic_core::policy::LayerTerms;
use ic_core::{Command, CoverStatus, LifeId, PolicyId, PolicyStatus, PremiumOnAvoidance, Reject};
use ic_money::{Currency::NZD, DecStr, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member, term_life};
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

/// The standard policy: 500,000 of life cover, 61.84 a month.
fn policy(c: &mut Cluster, n: u64) -> PolicyId {
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

const MONTHLY: i64 = 61_84;

fn pay(c: &mut Cluster, id: &PolicyId, receipt: &str, minor: i64) {
    let cmd = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: receipt.into(),
        amount: Money::new(minor, NZD),
    };
    c.submit(0, "cash-matching", cmd).unwrap();
}

fn avoid(id: &PolicyId, case: &str, premium: PremiumOnAvoidance) -> Command {
    Command::AvoidPolicy {
        policy: id.clone(),
        case: case.into(),
        premium,
    }
}

fn suspense(c: &Cluster, id: &PolicyId) -> i128 {
    c.state(0).ledger.natural_balance(&accounts::policy_suspense(id, NZD).id)
}

fn role_total(c: &Cluster, role: &str) -> i128 {
    c.state(0).ledger.role_total(role)
}

/// Purpose: avoiding a policy with the premiums refunded undoes every premium billed since
/// inception, including those billed so long ago that their receivables have left the
/// policy's state: earned premium goes back to nothing, what the policyholder paid is in
/// policy suspense to be refunded, what was owed is no longer owed, every layer ends on the
/// day it started, and the same decision arriving twice changes nothing more.
/// Why it matters: an avoided policy is treated as never having existed. If any earned
/// premium stayed on the books the insurer would report income on a contract it has
/// disowned, and if the refund left out the early years the customer would be short-paid
/// by exactly the premiums the system had tidied away.
#[test]
fn an_avoidance_with_refund_undoes_every_premium_since_inception() {
    let mut c = cluster();
    let id = policy(&mut c, 1);
    // A second policy on the same product, so that what is left earned is not zero by accident.
    let other = policy(&mut c, 2);
    pay(&mut c, &other, "o-1", 30 * MONTHLY);
    pay(&mut c, &id, "r-1", 30 * MONTHLY);
    // More than 400 days: the first months' receivables have been pruned.
    days(&mut c, 500);
    let state = c.state(0).policies[&id].clone();
    assert!(!state.pruned.is_empty(), "early receivables have left the state");
    let billed: i64 = 30 * MONTHLY - suspense(&c, &id) as i64 + state.outstanding_minor();
    let paid = billed - state.outstanding_minor();
    let earned_other = {
        // Both policies are the same age and price, so half of what is earned is the other's.
        role_total(&c, role::EARNED_PREMIUM) / 2
    };
    let held = suspense(&c, &id);
    c.take_events();

    assert_eq!(
        code(c.submit(0, "policy-servicing", avoid(&id, "nd-1", PremiumOnAvoidance::Refund))),
        "caller_not_allowed"
    );
    assert_eq!(
        code(c.submit(0, "underwriting", avoid(&id, "nd 1", PremiumOnAvoidance::Refund))),
        "invalid_case"
    );
    c.submit(0, "underwriting", avoid(&id, "nd-1", PremiumOnAvoidance::Refund)).unwrap();

    let after = c.state(0).policies[&id].clone();
    assert_eq!((after.status, after.standing, after.outstanding_minor()), (PolicyStatus::Avoided, None, 0));
    assert_eq!(suspense(&c, &id) - held, paid as i128, "everything paid for cover is the policyholder's again");
    let earned_left = role_total(&c, role::EARNED_PREMIUM);
    assert!(
        (earned_left - earned_other).abs() <= ic_money::FINE_PER_MINOR,
        "{earned_left} against {earned_other}"
    );
    for cover in after.covers.values() {
        assert_eq!(cover.status, CoverStatus::Ended);
        assert!(cover.layers.iter().all(|l| l.end == Some(l.start)), "no layer gave a day of cover");
    }
    let record = after.avoidance.clone().unwrap();
    assert_eq!(
        (record.case.as_str(), record.premium, record.refunded_minor),
        ("nd-1", PremiumOnAvoidance::Refund, paid)
    );
    let events = c.take_events();
    assert!(events.iter().any(|e| matches!(&e.event,
        Event::PolicyAvoided { case, premium: PremiumOnAvoidance::Refund, refunded_minor, .. } if case == "nd-1" && *refunded_minor == paid)));
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, Event::PolicyStatusChanged { to: PolicyStatus::Avoided, .. })));

    // The same decision again is accepted and does nothing; another is refused.
    c.submit(0, "underwriting", avoid(&id, "nd-1", PremiumOnAvoidance::Refund)).unwrap();
    assert!(c.take_events().is_empty());
    assert_eq!(
        code(c.submit(0, "underwriting", avoid(&id, "nd-2", PremiumOnAvoidance::Retain))),
        "not_in_force"
    );
    assert_eq!(
        code(c.submit(
            0,
            "cash-matching",
            Command::ApplyReceipt {
                policy: id.clone(),
                receipt: "late".into(),
                amount: Money::new(100, NZD),
            }
        )),
        "policy_ended"
    );

    // The money goes back by the ordinary refund, and the books stay whole.
    let refund = Command::RefundSuspense {
        policy: id.clone(),
        amount: Money::new(suspense(&c, &id) as i64, NZD),
        with_claim: None,
    };
    c.submit(0, "policy-servicing", refund).unwrap();
    days(&mut c, 3);
}

/// Purpose: avoiding a policy with the premiums kept leaves what was paid with the insurer:
/// nothing goes to policy suspense, premium paid but not yet earned is taken to the account
/// for premium retained on avoidance, what was still owed is written off, and the cover ends
/// from the start all the same. A scheme cannot be avoided this way.
/// Why it matters: where the case decides the premiums are kept, a refund would be money
/// paid out against the decision of the two people who signed it; and premium kept must be
/// visible as such in the accounts, not hidden in earned premium for cover that was void.
#[test]
fn an_avoidance_that_keeps_the_premium_refunds_nothing() {
    let mut c = cluster();
    let id = policy(&mut c, 1);
    pay(&mut c, &id, "r-1", MONTHLY);
    days(&mut c, 40); // December is billed and unpaid; November was paid and is earned.
    let before = c.state(0).policies[&id].clone();
    assert!(before.outstanding_minor() > 0);
    let held = suspense(&c, &id);
    let earned = role_total(&c, role::EARNED_PREMIUM);

    c.submit(0, "underwriting", avoid(&id, "nd-7", PremiumOnAvoidance::Retain)).unwrap();
    let after = c.state(0).policies[&id].clone();
    assert_eq!((after.status, after.outstanding_minor()), (PolicyStatus::Avoided, 0));
    assert_eq!(suspense(&c, &id), held, "nothing goes back to the policyholder");
    assert_eq!(after.avoidance.unwrap().refunded_minor, 0);
    assert!(role_total(&c, role::EARNED_PREMIUM) <= earned, "nothing more is earned on void cover");
    assert!(after.covers.values().all(|cv| cv.layers.iter().all(|l| l.end == Some(l.start))));
    days(&mut c, 3);

    // Paid ahead: the unearned part of what was paid is kept, in its own account.
    let ahead = policy(&mut c, 2);
    let first = c.state(0).policies[&ahead].outstanding_minor();
    pay(&mut c, &ahead, "a-1", first);
    days(&mut c, 5);
    c.submit(0, "underwriting", avoid(&ahead, "nd-8", PremiumOnAvoidance::Retain)).unwrap();
    assert!(-role_total(&c, role::AVOIDANCE_PREMIUM_RETAINED) > 0);
    assert_eq!(suspense(&c, &ahead), 0);
    days(&mut c, 3);

    // A scheme is not a contract with one life to avoid.
    let scheme = group_scheme(20, start(), vec![member("staff-1", d(1985, 1, 1), "staff", 60_000_00)]);
    let (p, scheme_id) = (c.partition_of(&scheme.group), scheme.policy.clone());
    c.submit(p, "new-business", issue_command(scheme)).unwrap();
    assert_eq!(
        code(c.submit(p, "underwriting", avoid(&scheme_id, "nd-9", PremiumOnAvoidance::Refund))),
        "not_retail"
    );
}

fn terms(life: &str, loading: Option<&str>, exclusions: &[&str]) -> LayerTerms {
    LayerTerms {
        cover: "life".into(),
        life: LifeId(life.into()),
        loadings: loading.map(|l| DecStr::parse(l).unwrap()).into_iter().collect(),
        exclusions: exclusions.iter().map(|e| e.to_string()).collect(),
    }
}

/// Purpose: varying a policy for non-disclosure rewrites the terms of every layer the life
/// has, as from the layer's start; what each layer was written on is kept; nothing already
/// billed changes; the next period is priced on the varied terms; a repeat of the same case
/// does nothing; and requests that name no such cover or life, carry a negative loading or
/// change nothing are refused.
/// Why it matters: a proportionate remedy puts the customer on the terms they would have had
/// with full disclosure. A claim must then be assessed against those terms from the start,
/// the customer must not be billed arrears for the past, and the record must still show what
/// was originally agreed, because the variation itself can be challenged.
#[test]
fn a_variation_rewrites_terms_from_the_start_and_bills_nothing_back() {
    let mut c = cluster();
    let id = policy(&mut c, 1);
    pay(&mut c, &id, "r-1", 20 * MONTHLY);
    days(&mut c, 400); // one anniversary: an original layer and an indexation layer
    let before = c.state(0).policies[&id].clone();
    assert_eq!(before.covers["life"].layers.len(), 2);
    let owed_before = before.outstanding_minor();
    let last_bill = before.receivables.iter().max_by_key(|r| r.period_start).unwrap().total_minor;
    let vary = |case: &str, t: Vec<LayerTerms>| Command::VaryForNonDisclosure {
        policy: id.clone(),
        case: case.into(),
        terms: t,
    };

    assert_eq!(
        code(c.submit(0, "claims", vary("nd-1", vec![terms("life-1", Some("0.5"), &["EX-BACK"])]))),
        "caller_not_allowed"
    );
    assert_eq!(
        code(c.submit(0, "underwriting", vary("", vec![terms("life-1", Some("0.5"), &[])]))),
        "invalid_case"
    );
    assert_eq!(
        code(c.submit(0, "underwriting", vary("nd-1", vec![terms("life-1", Some("-0.5"), &[])]))),
        "invalid_loading"
    );
    assert_eq!(
        code(c.submit(0, "underwriting", vary("nd-1", vec![terms("life-9", Some("0.5"), &[])]))),
        "unknown_life"
    );
    let mut wrong_cover = terms("life-1", Some("0.5"), &[]);
    wrong_cover.cover = "funeral".into();
    assert_eq!(code(c.submit(0, "underwriting", vary("nd-1", vec![wrong_cover]))), "unknown_cover");
    assert_eq!(code(c.submit(0, "underwriting", vary("nd-1", vec![terms("life-1", None, &[])]))), "no_change");
    assert_eq!(c.state(0).policies[&id], before, "a refusal changes nothing");
    c.take_events();

    c.submit(0, "underwriting", vary("nd-1", vec![terms("life-1", Some("0.5"), &["EX-BACK"])]))
        .unwrap();
    let after = c.state(0).policies[&id].clone();
    for l in &after.covers["life"].layers {
        assert_eq!((l.loadings.len(), l.exclusions.clone()), (1, vec!["EX-BACK".to_string()]));
        assert_eq!(l.start, before.covers["life"].layers.iter().find(|b| b.id == l.id).unwrap().start);
    }
    assert_eq!(after.variations.len(), 2);
    assert!(after
        .variations
        .iter()
        .all(|v| v.case == "nd-1" && v.loadings_before.is_empty() && v.exclusions_before.is_empty()));
    assert_eq!(after.outstanding_minor(), owed_before, "nothing already billed is billed again");
    assert_eq!(after.receivables.len(), before.receivables.len());
    let events = c.take_events();
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, Event::TermsVaried { case, layers, .. } if case == "nd-1" && layers.len() == 2)));
    assert!(!format!("{events:?}").contains("EX-BACK"), "the terms themselves are in no event");

    // The same case again does nothing.
    c.submit(0, "underwriting", vary("nd-1", vec![terms("life-1", Some("0.75"), &[])])).unwrap();
    assert_eq!(c.state(0).policies[&id].variations.len(), 2);

    // The next period is priced with the loading.
    days(&mut c, 31);
    let next = c.state(0).policies[&id].receivables.iter().max_by_key(|r| r.period_start).unwrap().total_minor;
    assert!(next > last_bill + last_bill / 3, "{next} against {last_bill}");
}
