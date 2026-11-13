//! Persona tests for the spine: rows of the persona test matrix
//! (`docs/testing/personas/`) run against the real core, each named for the
//! persona and the matrix row it carries out. Everyone here is invented.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::{Command, CoreState, LifeId, PolicyId, PolicyStatus, Standing};
use ic_money::{Currency::NZD, Money, FINE_PER_MINOR};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, named_life, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn cluster(open: NaiveDate) -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.open(open).unwrap();
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

fn pay(c: &mut Cluster, policy: &PolicyId, receipt: &str, minor: i128) {
    c.submit(
        0,
        "cash-matching",
        Command::ApplyReceipt {
            policy: policy.clone(),
            receipt: receipt.to_string(),
            amount: Money::new(minor as i64, NZD),
        },
    )
    .unwrap();
}

fn code(r: Result<(), ic_core::Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

fn receivable(s: &CoreState, id: &PolicyId) -> i128 {
    s.ledger.balance(&accounts::premium_receivable(id, NZD).id)
}

fn suspense(s: &CoreState, id: &PolicyId) -> i128 {
    s.ledger.natural_balance(&accounts::policy_suspense(id, NZD).id)
}

fn issue(c: &mut Cluster, n: u64, inception: NaiveDate, frequency: Frequency) -> PolicyId {
    let p = term_life(n, inception, d(1990, 3, 10), Sex::Female, false, 500_000_00, None, frequency, None);
    let id = p.policy.clone();
    c.submit(0, "new-business", issue_command(p)).unwrap();
    id
}

/// Purpose: matrix POL-002. A couple's term life policy holds a cover for each of them, each
/// with its own sum insured and its own layer on its own life, and nothing on the policy joins
/// the two lives: each life's cover is billed and counted alone.
/// Why it matters: two single covers and a joint cover pay out differently on the first death.
/// If one life's layer carried the other's amount, a claim on either would pay the wrong sum.
#[test]
fn a_couple_holds_a_single_cover_each_on_one_policy() {
    let mut c = cluster(d(2027, 3, 1));
    let mut p = term_life(1, d(2027, 3, 1), d(1988, 6, 2), Sex::Female, false, 600_000_00, None, Frequency::Monthly, None);
    p.lives.push(named_life("life-2", "party-partner", d(1986, 1, 20), Sex::Male, true));
    let mut second = p.covers[0].clone();
    second.life = Some(LifeId("life-2".to_string()));
    second.sum_insured_minor = 250_000_00;
    p.covers.push(second);
    let id = p.policy.clone();
    c.submit(0, "new-business", issue_command(p)).unwrap();

    let s = c.state(0);
    let pol = &s.policies[&id];
    assert_eq!(pol.status, PolicyStatus::InForce);
    let layers: Vec<_> = pol.covers.values().flat_map(|cv| cv.layers.iter()).collect();
    assert_eq!(layers.len(), 2, "one layer for each life");
    let on = |life: &str| -> i64 { layers.iter().filter(|l| l.life.0 == life).map(|l| l.sum_insured_minor).sum() };
    assert_eq!(on("life-1"), 600_000_00);
    assert_eq!(on("life-2"), 250_000_00);
    // A single cover for one smoker alongside a non-smoker costs more than the non-smoker's
    // cover alone: each life was rated on its own facts.
    let alone = {
        let mut c2 = cluster(d(2027, 3, 1));
        let id2 = issue(&mut c2, 2, d(2027, 3, 1), Frequency::Monthly);
        receivable(c2.state(0), &id2)
    };
    assert!(receivable(s, &id) > alone);
    days(&mut c, 1);
}

/// Purpose: matrix LED-006. A yearly premium billed on 1 March 2027 is earned day by day over
/// the 366 days to 29 February 2028, leap day included, and on the day the period ends the
/// unearned premium is exactly zero and earned premium equals what was billed to the fine unit.
/// Why it matters: a leap year is where a daily release that assumes 365 days leaves a residue
/// on the balance sheet, or earns a day's premium twice. Either misstates income every fourth year.
#[test]
fn a_yearly_premium_is_earned_exactly_across_the_leap_day() {
    let mut c = cluster(d(2027, 3, 1));
    let id = issue(&mut c, 3, d(2027, 3, 1), Frequency::Yearly);
    let billed = receivable(c.state(0), &id);
    assert!(billed > 0);
    pay(&mut c, &id, "r1", billed);
    let unearned = accounts::unearned_premium(&id, NZD).id;
    assert_eq!(c.state(0).ledger.natural_balance(&unearned), billed * FINE_PER_MINOR);

    days(&mut c, 365);
    assert_eq!(c.business_date(), Some(d(2028, 2, 29)));
    // One day of the 366 is still to be earned.
    let left = c.state(0).ledger.natural_balance(&unearned);
    assert!(left > 0 && left < billed * FINE_PER_MINOR / 365, "one day left, got {left}");

    days(&mut c, 1);
    let s = c.state(0);
    assert_eq!(c.business_date(), Some(d(2028, 3, 1)));
    // The first year is wholly earned; what is unearned now is the second year, billed today.
    assert_eq!(s.policies[&id].periods_billed, 2);
    let second = receivable(s, &id);
    assert_eq!(s.ledger.natural_balance(&unearned), second * FINE_PER_MINOR);
    assert_eq!(-s.ledger.role_total(accounts::role::EARNED_PREMIUM), billed * FINE_PER_MINOR);
}

/// Purpose: matrix BIL-014 (P46 Pita Walker, who pays the same premium three times in a month).
/// The first payment settles the premium; the second and third wait in the policy's suspense as
/// a credit, are never income, and can be refunded in full.
/// Why it matters: an elderly customer paying twice by mistake must get the money back. If the
/// extra payments were taken to income or lost against the receivable, the insurer would hold
/// money that is not its own.
#[test]
fn a_premium_paid_three_times_leaves_two_as_a_credit_that_can_be_refunded() {
    let mut c = cluster(d(2027, 5, 1));
    let id = issue(&mut c, 4, d(2027, 5, 1), Frequency::Monthly);
    let due = receivable(c.state(0), &id);
    pay(&mut c, &id, "r1", due);
    days(&mut c, 3);
    pay(&mut c, &id, "r2", due);
    days(&mut c, 9);
    pay(&mut c, &id, "r3", due);
    let s = c.state(0);
    assert_eq!(receivable(s, &id), 0);
    assert_eq!(suspense(s, &id), 2 * due);
    assert_eq!(s.policies[&id].standing, Some(Standing::Paid));
    c.submit(
        0,
        "policy-servicing",
        Command::RefundSuspense {
            policy: id.clone(),
            amount: Money::new((2 * due) as i64, NZD),
            with_claim: None,
        },
    )
    .unwrap();
    assert_eq!(suspense(c.state(0), &id), 0);
    assert!(c.reconcile().is_empty());
    days(&mut c, 1);
}

/// Purpose: matrix LED-012. Two refunds race for the same $200 credit. They are applied one at a
/// time in log order: the first takes the credit, the second sees the reduced balance and is
/// refused, and a third asking for only what is left is applied.
/// Why it matters: if both refunds read the balance before either posted, the customer would be
/// paid $400 from a $200 credit and suspense would go into debit.
#[test]
fn two_refunds_of_one_credit_are_applied_in_log_order() {
    let mut c = cluster(d(2027, 5, 1));
    let id = issue(&mut c, 5, d(2027, 5, 1), Frequency::Monthly);
    let due = receivable(c.state(0), &id);
    pay(&mut c, &id, "r1", due + 200_00);
    let refund = |minor: i64| Command::RefundSuspense {
        policy: id.clone(),
        amount: Money::new(minor, NZD),
        with_claim: None,
    };
    c.submit(0, "policy-servicing", refund(150_00)).unwrap();
    assert_eq!(code(c.submit(0, "policy-servicing", refund(150_00))), "insufficient_suspense");
    assert_eq!(suspense(c.state(0), &id), 50_00);
    c.submit(0, "policy-servicing", refund(50_00)).unwrap();
    assert_eq!(suspense(c.state(0), &id), 0);
    assert!(c.reconcile().is_empty());
}

/// Purpose: matrix POL-007 and COL-018 (P1 Aroha Ngata, a fortnightly payer who falls behind over
/// a winter). The policy goes from paid to in grace to suspended without its status ever leaving
/// in force, and paying the arrears while suspended makes the standing paid at once, before any
/// end-of-day, so the dunning sequence has nothing left to chase.
/// Why it matters: standing decides whether a claim is paid. It must follow the money and nothing
/// else: a payment made in the app has to restore cover the moment it lands.
#[test]
fn paying_the_arrears_while_suspended_restores_the_standing_at_once() {
    let mut c = cluster(d(2027, 6, 1));
    let id = issue(&mut c, 6, d(2027, 6, 1), Frequency::Fortnightly);
    let first = receivable(c.state(0), &id);
    let mut seen = vec![c.state(0).policies[&id].standing];
    for _ in 0..44 {
        days(&mut c, 1);
        let pol = &c.state(0).policies[&id];
        assert_eq!(pol.status, PolicyStatus::InForce);
        if seen.last() != Some(&pol.standing) {
            seen.push(pol.standing);
        }
    }
    assert_eq!(seen, vec![Some(Standing::Paid), Some(Standing::InGrace), Some(Standing::Suspended)]);
    let owed = receivable(c.state(0), &id);
    assert!(owed > first, "several fortnights are now owed");
    pay(&mut c, &id, "card-1", owed);
    let pol = &c.state(0).policies[&id];
    assert_eq!(pol.standing, Some(Standing::Paid));
    assert_eq!(pol.status, PolicyStatus::InForce);
    assert_eq!(receivable(c.state(0), &id), 0);
    days(&mut c, 1);
    assert_eq!(c.state(0).policies[&id].standing, Some(Standing::Paid));
}

/// Purpose: matrix PRD-011. A policy on a life whose rating sex is not recorded is issued, and
/// its first bill is the lower of what the same life would be billed as female and as male.
/// Why it matters: the product crate proves the rate; this proves the whole path from the issue
/// command to the receivable uses it. Before decision 1 in `docs/decisions.md` the issue was
/// refused for want of a rate, so a person who is neither male nor female could not be insured
/// without recording a sex that is not theirs.
#[test]
fn a_life_with_no_recorded_sex_is_issued_at_the_lower_premium() {
    let mut c = cluster(d(2027, 3, 1));
    let mut billed = |n: u64, sex: Option<Sex>| {
        let mut p = term_life(n, d(2027, 3, 1), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        p.lives[0].factors.sex = sex;
        let id = p.policy.clone();
        c.submit(0, "new-business", issue_command(p)).unwrap();
        receivable(c.state(0), &id)
    };
    let (female, male, unrecorded) = (billed(1, Some(Sex::Female)), billed(2, Some(Sex::Male)), billed(3, None));
    assert_ne!(female, male, "the shipped table rates by sex");
    assert!(unrecorded > 0);
    assert_eq!(unrecorded, female.min(male));
}
