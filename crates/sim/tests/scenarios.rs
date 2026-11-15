//! Scenario tests for the spine: exact postings and state for known cases.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::command::MemberChange;
use ic_core::event::Event;
use ic_core::{Command, CoreState, GroupId, LifeFactors, LifeId, PartyId, PolicyId, PolicyStatus, Standing, APPLY_BUDGET};
use ic_ledger::{resum, verify_chain, PartitionId};
use ic_log::Replica;
use ic_money::{Currency::NZD, Money, FINE_PER_MINOR};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, health_family, issue_command, member, term_life};
use ic_sim::{Cluster, Delivery};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn cluster(partitions: PartitionId, open: NaiveDate) -> Cluster {
    let mut c = Cluster::in_memory(partitions, 42);
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

fn pay(c: &mut Cluster, p: PartitionId, policy: &PolicyId, receipt: &str, minor: i64) {
    c.submit(
        p,
        "cash-matching",
        Command::ApplyReceipt {
            policy: policy.clone(),
            receipt: receipt.to_string(),
            amount: Money::new(minor, NZD),
        },
    )
    .unwrap();
}

/// A 37-year-old non-smoking woman, 500,000 life cover, monthly: 55.84 + 6.00 fee.
fn standard(c: &mut Cluster, n: u64, inception: NaiveDate) -> (PartitionId, PolicyId) {
    let issue = term_life(n, inception, d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    (p, id)
}

const MONTHLY: i64 = 61_84;
const PREMIUM: i64 = 55_84;
const FEE: i64 = 6_00;

/// Fine units earned after `n` of `d` days: each line is released on its own, half-even.
fn earned_after(n: i128, d: i128) -> i128 {
    let he = |minor: i64| {
        let x = minor as i128 * FINE_PER_MINOR * n;
        let (q, r) = (x / d, x % d);
        if 2 * r > d || (2 * r == d && q % 2 == 1) {
            q + 1
        } else {
            q
        }
    };
    he(PREMIUM) + he(FEE)
}

/// Purpose: issuing a policy bills the first month to the cent, binds the definitions in force,
/// earns a third of the premium after ten of thirty days and all of it after thirty, to the fine
/// unit, and bills the next period on its due date.
/// Why it matters: billing and earning are the core of the general ledger for an insurer. Earned
/// plus unearned must equal billed on every day of a period, or income is misstated.
#[test]
fn issue_bills_the_first_period_and_earns_it_exactly() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, id) = standard(&mut c, 1, d(2027, 11, 15));
    let s = c.state(p);
    let pol = &s.policies[&id];
    assert_eq!(pol.status, PolicyStatus::InForce);
    assert_eq!(pol.standing, Some(Standing::Paid));
    // 644.25 * 1.04 / 12 = 55.835 -> 55.84 half-even; fee 72 / 12 = 6.00
    assert_eq!(s.ledger.balance(&accounts::premium_receivable(&id, NZD).id), MONTHLY as i128);
    assert_eq!(
        s.ledger.natural_balance(&accounts::unearned_premium(&id, NZD).id),
        MONTHLY as i128 * FINE_PER_MINOR
    );
    // definitions are bound to the version in force at the cover's start
    assert_eq!(pol.covers["life"].definitions.len(), 2);

    pay(&mut c, p, &id, "r1", MONTHLY);
    days(&mut c, 10);
    // 10 of 30 days earned, exactly a third, in fine units
    let s = c.state(p);
    let earned: i128 = -s.ledger.role_total(accounts::role::EARNED_PREMIUM);
    assert_eq!(earned, earned_after(10, 30));
    days(&mut c, 20);
    // period fully earned to the last fine unit, and the next one billed
    let s = c.state(p);
    assert_eq!(c.business_date(), Some(d(2027, 12, 15)));
    assert_eq!(-s.ledger.role_total(accounts::role::EARNED_PREMIUM), MONTHLY as i128 * FINE_PER_MINOR);
    assert_eq!(s.policies[&id].periods_billed, 2);
    assert_eq!(s.ledger.balance(&accounts::premium_receivable(&id, NZD).id), MONTHLY as i128);
    // every entry carries the product version's content hash
    assert!(c.nodes[&p]
        .journal
        .iter()
        .filter(|e| e.body.policy.is_some())
        .all(|e| e.body.product_hash.as_deref() == Some(s.policies[&id].product_hash.as_str())));
}

/// Purpose: an unpaid policy moves from grace to suspended to lapsed on the days its product sets,
/// with the books reconciling at each stage, and is reinstated when the arrears are paid within the
/// window.
/// Why it matters: these dates decide whether a claim is paid. A policy that lapsed a day early, or
/// stayed in force unpaid, is a legal and financial error either way.
#[test]
fn unpaid_premium_moves_through_grace_suspension_and_lapse_then_reinstates() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, id) = standard(&mut c, 2, d(2027, 11, 15));
    days(&mut c, 1);
    assert_eq!(c.state(p).policies[&id].standing, Some(Standing::InGrace));
    days(&mut c, 29); // 30 days overdue: last day of grace
    assert_eq!(c.state(p).policies[&id].standing, Some(Standing::InGrace));
    days(&mut c, 1);
    assert_eq!(c.state(p).policies[&id].standing, Some(Standing::Suspended));
    days(&mut c, 59); // 90 days overdue
    assert_eq!(c.state(p).policies[&id].status, PolicyStatus::InForce);
    days(&mut c, 1); // 91 days: lapse
    let s = c.state(p);
    let pol = &s.policies[&id];
    assert_eq!(pol.status, PolicyStatus::Lapsed);
    assert_eq!(pol.standing, None);
    assert_eq!(s.ledger.balance(&accounts::premium_receivable(&id, NZD).id), 0);
    assert_eq!(s.ledger.balance(&accounts::unearned_premium(&id, NZD).id), 0);
    // four periods were billed (15 Nov, Dec, Jan, Feb); the lapse was on 14 Feb,
    // so three were due. Earned-but-unpaid premium is in the write-off account.
    let written_off = s.ledger.role_total(accounts::role::LAPSE_WRITE_OFF);
    let earned = -s.ledger.role_total(accounts::role::EARNED_PREMIUM);
    assert_eq!(written_off, earned, "everything earned was unpaid, so all of it is written off");
    let arrears = pol.lapse.as_ref().unwrap().arrears_minor;
    assert_eq!(arrears, 3 * MONTHLY);

    // no earning while lapsed
    days(&mut c, 5);
    assert_eq!(-c.state(p).ledger.role_total(accounts::role::EARNED_PREMIUM), earned);

    // reinstatement needs the arrears in suspense first
    assert_eq!(
        c.submit(p, "collections", Command::Reinstate { policy: id.clone() }).unwrap_err().code,
        "arrears_unpaid"
    );
    // 14 Feb + 5 days = 19 Feb: the 15 Feb period was missed while lapsed
    pay(&mut c, p, &id, "catch-up", 4 * MONTHLY);
    c.submit(p, "collections", Command::Reinstate { policy: id.clone() }).unwrap();
    let s = c.state(p);
    let pol = &s.policies[&id];
    assert_eq!(pol.status, PolicyStatus::InForce);
    assert_eq!(pol.standing, Some(Standing::Paid));
    assert_eq!(s.ledger.role_total(accounts::role::LAPSE_WRITE_OFF), 0);
    assert_eq!(s.ledger.balance(&accounts::premium_receivable(&id, NZD).id), 0);
    // the earned premium for the gap is recognised at reinstatement
    assert!(-s.ledger.role_total(accounts::role::EARNED_PREMIUM) > earned);
    assert!(c.reconcile().is_empty());
    days(&mut c, 3);
}

/// Purpose: once the reinstatement window has passed, a lapsed policy cannot be reinstated however
/// much is paid.
/// Why it matters: after the window the insurer is entitled to underwrite again. Reinstating
/// automatically would put a risk back on the books that nobody has assessed.
#[test]
fn reinstatement_window_closes() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, id) = standard(&mut c, 3, d(2027, 11, 15));
    days(&mut c, 91 + 181);
    pay(&mut c, p, &id, "late", 100 * MONTHLY);
    assert_eq!(c.submit(p, "collections", Command::Reinstate { policy: id }).unwrap_err().code, "window_closed");
}

/// Purpose: when a payment is reversed, the policy's standing is worked out from the original due
/// date, and the same receipt cannot be reversed twice.
/// Why it matters: a bounced direct debit means the premium was never paid. If each bounce
/// restarted the clock, a customer could stay covered indefinitely without paying.
#[test]
fn a_dishonour_does_not_reset_the_lapse_clock() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, id) = standard(&mut c, 4, d(2027, 11, 15));
    days(&mut c, 20);
    pay(&mut c, p, &id, "dd-1", MONTHLY);
    assert_eq!(c.state(p).policies[&id].standing, Some(Standing::Paid));
    days(&mut c, 15);
    c.submit(
        p,
        "cash-matching",
        Command::ReverseReceipt {
            policy: id.clone(),
            receipt: "dd-1".into(),
        },
    )
    .unwrap();
    // 35 days after the original due date: straight to suspended, not back to day one
    let pol = &c.state(p).policies[&id];
    assert_eq!(pol.standing, Some(Standing::Suspended));
    assert_eq!(pol.oldest_unpaid_due(), Some(d(2027, 11, 15)));
    assert_eq!(
        c.submit(
            p,
            "cash-matching",
            Command::ReverseReceipt {
                policy: id,
                receipt: "dd-1".into()
            }
        )
        .unwrap_err()
        .code,
        "already_reversed"
    );
    assert!(c.reconcile().is_empty());
}

/// Purpose: money paid beyond what is owed waits in the policy's suspense account, pays later bills
/// as they fall due, and can be refunded up to the amount held and no further.
/// Why it matters: overpayments are the customer's money until a bill takes them. They must be
/// visible, used before the customer is asked to pay again, and impossible to refund twice.
#[test]
fn money_paid_ahead_waits_in_suspense_and_is_consumed_by_billing() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, id) = standard(&mut c, 5, d(2027, 11, 15));
    pay(&mut c, p, &id, "big", 3 * MONTHLY);
    let suspense = accounts::policy_suspense(&id, NZD).id;
    assert_eq!(c.state(p).ledger.natural_balance(&suspense), 2 * MONTHLY as i128);
    days(&mut c, 30);
    assert_eq!(c.state(p).ledger.natural_balance(&suspense), MONTHLY as i128);
    assert_eq!(c.state(p).policies[&id].outstanding_minor(), 0);
    days(&mut c, 31);
    assert_eq!(c.state(p).ledger.natural_balance(&suspense), 0);
    // the customer can ask for a credit back, but not more than is held
    pay(&mut c, p, &id, "extra", 500);
    assert!(c
        .submit(
            p,
            "policy-servicing",
            Command::RefundSuspense {
                policy: id.clone(),
                amount: Money::new(501, NZD),
                with_claim: None,
            }
        )
        .is_err());
    c.submit(
        p,
        "policy-servicing",
        Command::RefundSuspense {
            policy: id,
            amount: Money::new(500, NZD),
            with_claim: None,
        },
    )
    .unwrap();
    assert!(c.reconcile().is_empty());
}

/// Purpose: cancelling inside the cooling-off period refunds all premium paid, cancelling later
/// refunds the unearned part to the cent, and a cancelled policy is never billed again.
/// Why it matters: the cooling-off refund is a statutory right, and the pro-rata refund is the
/// customer's money for cover not given. Both must be exact, and a cancelled policy must stay
/// cancelled.
#[test]
fn cooling_off_refunds_everything_and_mid_term_refunds_the_unearned_part() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, a) = standard(&mut c, 6, d(2027, 11, 15));
    let (_, b) = standard(&mut c, 7, d(2027, 11, 15));
    pay(&mut c, p, &a, "a1", MONTHLY);
    pay(&mut c, p, &b, "b1", MONTHLY);
    days(&mut c, 10);
    c.submit(p, "policy-servicing", Command::CancelPolicy { policy: a.clone() }).unwrap();
    let s = c.state(p);
    assert_eq!(s.policies[&a].status, PolicyStatus::Cancelled);
    assert_eq!(
        s.ledger.natural_balance(&accounts::policy_suspense(&a, NZD).id),
        MONTHLY as i128,
        "all premium paid is refunded"
    );
    days(&mut c, 10); // day 20: outside the 14-day cooling-off period
    c.submit(p, "policy-servicing", Command::CancelPolicy { policy: b.clone() }).unwrap();
    let s = c.state(p);
    // 20 of 30 days earned; a third is returned: 61.84 / 3 = 20.6133 -> 20.61
    assert_eq!(s.ledger.natural_balance(&accounts::policy_suspense(&b, NZD).id), 20_61);
    assert_eq!(s.ledger.balance(&accounts::unearned_premium(&b, NZD).id), 0);
    // only b's 20 earned days remain in income
    assert_eq!(-s.ledger.role_total(accounts::role::EARNED_PREMIUM), earned_after(20, 30));
    assert!(c.reconcile().is_empty());
    days(&mut c, 40);
    assert_eq!(c.state(p).policies[&b].periods_billed, 1, "a cancelled policy is never billed again");
}

/// Purpose: a health policy's bill posts GST to the tax account and premium to unearned, tags every
/// posting with its statutory fund, and reverses the GST on premium that lapses unpaid.
/// Why it matters: GST billed is owed to Inland Revenue, and GST on a bill never paid is not.
/// Statutory fund is how a life insurer keeps each fund's assets apart; a posting without it cannot
/// be reported.
#[test]
fn health_premium_splits_gst_to_the_tax_account() {
    let mut c = cluster(1, d(2027, 11, 15));
    let issue = health_family(8, d(2027, 11, 15), &[d(1992, 5, 1), d(1990, 8, 20), d(2020, 1, 5)], Frequency::Monthly);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    let s = c.state(p);
    // two adults at 960/yr and a child at 420/yr: 195.00 a month, plus 15% GST
    assert_eq!(s.ledger.natural_balance(&accounts::unearned_premium(&id, NZD).id), 195_00 * FINE_PER_MINOR);
    assert_eq!(s.ledger.natural_balance(&accounts::gst_payable(NZD).id), 29_25);
    assert_eq!(s.ledger.balance(&accounts::premium_receivable(&id, NZD).id), 224_25);
    // statutory fund is a dimension on every posting
    assert!(c.nodes[&p]
        .journal
        .iter()
        .flat_map(|e| &e.body.postings)
        .all(|x| x.dims.statutory_fund.as_deref() == Some("SF2-health")));
    // unpaid to lapse: the GST on the unpaid premium is reversed, not left payable
    days(&mut c, 61);
    let s = c.state(p);
    assert_eq!(s.policies[&id].status, PolicyStatus::Lapsed);
    assert_eq!(s.ledger.natural_balance(&accounts::gst_payable(NZD).id), 0);
    assert!(c.reconcile().is_empty());
}

/// Purpose: on the anniversary, cover increases by the index as a new layer effective that day, the
/// stepped premium is re-rated for the new age and cover, and the customer can decline further
/// indexation.
/// Why it matters: indexation keeps cover in step with inflation and is how premiums rise each
/// year. The increase must start on the anniversary, not before, and stop when the customer says
/// so.
#[test]
fn anniversary_indexes_cover_and_re_rates_stepped_premium() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, id) = standard(&mut c, 9, d(2027, 11, 15));
    pay(&mut c, p, &id, "year", 13 * MONTHLY);
    days(&mut c, 366); // 2028 is a leap year
    assert_eq!(c.business_date(), Some(d(2028, 11, 15)));
    assert!(!c.nodes[&p].outbox.iter().any(|e| matches!(e.event, Event::PolicyException { .. })));
    let pol = &c.state(p).policies[&id];
    assert_eq!(pol.anniversaries_processed, 1);
    let cover = &pol.covers["life"];
    assert_eq!(cover.layers.len(), 2);
    assert_eq!(cover.layers[1].sum_insured_minor, 12_500_00, "2.5% of 500,000");
    assert_eq!(cover.sum_insured_on(d(2028, 11, 15)), 512_500_00);
    assert_eq!(cover.sum_insured_on(d(2028, 11, 14)), 500_000_00);
    // age 38 is a new rate band, and the cover is larger: the 13th bill is higher
    let last = pol.receivables.iter().max_by_key(|r| r.id).unwrap();
    assert_eq!(last.period_start, d(2028, 11, 15));
    assert!(last.total_minor > MONTHLY);
    // declining indexation stops the next step
    c.submit(
        p,
        "policy-servicing",
        Command::DeclineIndexation {
            policy: id.clone(),
            declined: true,
        },
    )
    .unwrap();
}

/// Activates term life again under `code` with its indexation line replaced by `indexation`
/// (an empty string removes the feature) and issues the standard policy on it.
fn on_term_life_variant(c: &mut Cluster, n: u64, code: &str, indexation: &str) -> (PartitionId, PolicyId) {
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", &format!("code = \"{code}\""))
        .replace("indexation(rate_code = \"cpi\"),", indexation);
    let product = ic_product::load_product(&format!("{code}.star"), &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let mut issue = term_life(
        n,
        d(2027, 11, 15),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    issue.product_code = code.into();
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    (p, id)
}

/// Purpose: the product's `indexation(opt_out = ...)` decides whether the owner may decline
/// indexation. With `opt_out = False` declining is refused `indexation_compulsory` and the
/// policy keeps indexing; accepting it is still recorded. A product with no indexation refuses
/// both ways with `no_indexation`.
/// Why it matters: the setting was declared on products and read by nothing, so a policy whose
/// wording makes indexation compulsory could have it switched off, and a request to decline
/// indexation on a product without it was accepted and silently meant nothing.
#[test]
fn the_product_decides_whether_indexation_may_be_declined() {
    let mut c = cluster(1, d(2027, 11, 15));
    let decline = |c: &mut Cluster, p, id: &PolicyId, declined| {
        c.submit(p, "policy-servicing", Command::DeclineIndexation { policy: id.clone(), declined })
            .map_err(|e| e.code)
    };
    let (p, compulsory) = on_term_life_variant(&mut c, 7, "term-life-indexed", "indexation(rate_code = \"cpi\", opt_out = False),");
    assert_eq!(decline(&mut c, p, &compulsory, true), Err("indexation_compulsory".to_string()));
    assert!(!c.state(p).policies[&compulsory].indexation_declined);
    decline(&mut c, p, &compulsory, false).unwrap();

    let (p, plain) = on_term_life_variant(&mut c, 8, "term-life-plain", "");
    assert_eq!(decline(&mut c, p, &plain, true), Err("no_indexation".to_string()));
    assert_eq!(decline(&mut c, p, &plain, false), Err("no_indexation".to_string()));

    // the shipped term life lets the owner decline
    let (p, id) = standard(&mut c, 9, d(2027, 11, 15));
    decline(&mut c, p, &id, true).unwrap();
    assert!(c.state(p).policies[&id].indexation_declined);
}

/// Purpose: a scheme of a thousand members is loaded and billed in chunks that each stay within the
/// work budget of one command, and a leaver notified late has premium credited back to the date
/// they left.
/// Why it matters: one command must not stall a partition while it bills a large employer.
/// Employers always report leavers late, and the scheme is owed back premium charged for people who
/// had gone.
#[test]
fn group_scheme_bills_in_chunks_and_adjusts_for_a_backdated_leaver() {
    let mut c = cluster(1, d(2027, 11, 1));
    let first: Vec<_> = (0..100).map(|i| member(&format!("m{i:05}"), d(1980, 1, 1), "staff", 80_000_00)).collect();
    let issue = group_scheme(10, d(2027, 11, 1), first);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    // staff: 2 x 80,000 = 160,000 at 1.10 per thousand = 176.00 a year = 14.67 a month each
    assert_eq!(c.state(p).ledger.balance(&accounts::premium_receivable(&id, NZD).id), 100 * 14_67);

    // the rest of the membership file arrives in chunks
    for chunk in 0..9 {
        let changes = (0..100)
            .map(|i| MemberChange::Joined {
                life: member(
                    &format!("m{:05}", 100 + chunk * 100 + i),
                    d(1975, 6, 1),
                    if i % 10 == 0 { "executive" } else { "staff" },
                    120_000_00,
                ),
                effective: d(2027, 11, 1),
            })
            .collect();
        c.submit(p, "group-admin", Command::ApplyMembership { policy: id.clone(), changes }).unwrap();
    }
    assert_eq!(c.state(p).policies[&id].lives.len(), 1000);
    // too large a chunk is refused before it can stall the partition
    let too_many = (0..101)
        .map(|i| MemberChange::Left {
            life: LifeId(format!("m{i:05}")),
            effective: d(2027, 11, 2),
        })
        .collect();
    assert_eq!(
        c.submit(
            p,
            "group-admin",
            Command::ApplyMembership {
                policy: id.clone(),
                changes: too_many
            }
        )
        .unwrap_err()
        .code,
        "apply_budget"
    );

    let owed = c.state(p).policies[&id].outstanding_minor();
    pay(&mut c, p, &id, "nov", owed);
    days(&mut c, 30); // 1 December: the invoice for 1,000 members is built over several commands
    let pol = &c.state(p).policies[&id];
    assert_eq!(pol.periods_billed, 2);
    assert!(pol.pending_invoice.is_none());
    let december = pol.outstanding_minor();
    assert!(december > 0);

    // on 10 December we learn a member left on 16 November
    days(&mut c, 9);
    let before = c.state(p).policies[&id].outstanding_minor();
    c.submit(
        p,
        "group-admin",
        Command::ApplyMembership {
            policy: id.clone(),
            changes: vec![MemberChange::Left {
                life: LifeId("m00000".into()),
                effective: d(2027, 11, 16),
            }],
        },
    )
    .unwrap();
    let after = c.state(p).policies[&id].outstanding_minor();
    // half of November (15 of 30 days) and all of December come back as a credit
    assert_eq!(before - after, 7_34 + 14_67);
    // and a salary change learned late is charged from its true date
    c.submit(
        p,
        "group-admin",
        Command::ApplyMembership {
            policy: id.clone(),
            changes: vec![MemberChange::Changed {
                life: LifeId("m00001".into()),
                effective: d(2027, 12, 1),
                factors: LifeFactors {
                    date_of_birth: Some(d(1980, 1, 1)),
                    category: Some("staff".into()),
                    salary_minor: Some(100_000_00),
                    ..Default::default()
                },
            }],
        },
    )
    .unwrap();
    // 2 x 100,000 at 1.10 = 220 a year = 18.33 a month; was 14.67
    assert_eq!(c.state(p).policies[&id].outstanding_minor() - after, 18_33 - 14_67);
    // both timelines are kept: what was true, and when we learned it
    let life = &c.state(p).policies[&id].lives[&LifeId("m00001".into())];
    assert_eq!(life.factors.as_of(d(2027, 12, 5)).unwrap().salary_minor, Some(100_000_00));
    assert_eq!(life.factors.as_known(d(2027, 12, 5), d(2027, 12, 9)).unwrap().salary_minor, Some(80_000_00));

    // identification at claim links the member record to a person
    c.submit(
        p,
        "claims",
        Command::IdentifyMember {
            policy: id.clone(),
            life: LifeId("m00002".into()),
            party: PartyId("party-x".into()),
        },
    )
    .unwrap();
    days(&mut c, 25);
    assert!(
        c.nodes[&p].max_work_units <= APPLY_BUDGET,
        "a command used {} work units",
        c.nodes[&p].max_work_units
    );
}

/// Purpose: a rate table scoped to one scheme prices that scheme, and every other scheme still gets
/// the standard table.
/// Why it matters: large schemes negotiate their own rates. One scheme's special terms must never
/// leak into another's premiums, in either direction.
#[test]
fn a_scoped_rate_table_applies_only_to_its_scheme() {
    let mut c = cluster(1, d(2027, 11, 1));
    let a = group_scheme(11, d(2027, 11, 1), vec![member("m1", d(1980, 1, 1), "staff", 80_000_00)]);
    let b = group_scheme(12, d(2027, 11, 1), vec![member("m1", d(1980, 1, 1), "staff", 80_000_00)]);
    let mut table = ic_product::load_rate_table("x", include_str!("../../../products/rates/group_life_standard.star")).unwrap();
    table.version = 2;
    table.scope = Some(vec![a.group.0.clone()]);
    table.rows[0].annual_rate = ic_money::DecStr::parse("2.20").unwrap();
    c.activate_everywhere(Command::ActivateRateTable { table: Box::new(table) }).unwrap();
    let (ida, idb) = (a.policy.clone(), b.policy.clone());
    c.submit(0, "new-business", issue_command(a)).unwrap();
    c.submit(0, "new-business", issue_command(b)).unwrap();
    assert_eq!(c.state(0).policies[&ida].outstanding_minor(), 29_33);
    assert_eq!(c.state(0).policies[&idb].outstanding_minor(), 14_67);
}

/// Purpose: a policy moves to a group on another partition while the relay delays and duplicates
/// the messages: it is frozen in flight, lands once with its covers, receivables and balances
/// intact, the books reconcile throughout, and earning missed in flight is caught up.
/// Why it matters: a transfer is the one operation that spans partitions, and messages between
/// partitions are delivered at least once in no promised time. Money in transit must never be
/// counted twice or dropped.
#[test]
fn a_policy_transfers_between_groups_under_fault_injection_with_nothing_lost() {
    let mut c = cluster(4, d(2027, 11, 15));
    let (src, id) = standard(&mut c, 13, d(2027, 11, 15));
    pay(&mut c, src, &id, "r1", MONTHLY + 1000); // some credit in suspense too
    days(&mut c, 12);
    let target_group = GroupId(
        (0..)
            .map(|i| format!("grp-new-{i}"))
            .find(|g| c.partition_of(&GroupId(g.clone())) != src)
            .unwrap(),
    );
    let dst = c.partition_of(&target_group);
    let before = c.state(src).policies[&id].clone();
    let unearned_before = c.state(src).ledger.balance(&accounts::unearned_premium(&id, NZD).id);

    c.submit(
        src,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t-1".into(),
            target_group: target_group.clone(),
            target_partition: dst,
        },
    )
    .unwrap();
    // frozen: new commands are refused, and the balances sit in in-transit
    assert_eq!(
        c.submit(
            src,
            "cash-matching",
            Command::ApplyReceipt {
                policy: id.clone(),
                receipt: "r2".into(),
                amount: Money::new(100, NZD)
            }
        )
        .unwrap_err()
        .code,
        "policy_transferring"
    );
    assert_eq!(c.state(src).ledger.balance(&accounts::unearned_premium(&id, NZD).id), 0);
    assert!(c.reconcile().is_empty());

    // the relay delays the record across two end-of-days, then delivers it twice
    days(&mut c, 2);
    assert_eq!(c.relay_messages(|_| Delivery::Delay), 0);
    c.relay_messages(|_| Delivery::Duplicate);
    c.relay_messages(|_| Delivery::Duplicate);
    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    assert_eq!(c.locate(&id), Some(dst));
    assert_eq!(c.state(src).policies[&id].status, PolicyStatus::Transferred);
    let landed = &c.state(dst).policies[&id];
    assert_eq!(landed.group, target_group);
    assert_eq!(landed.history, vec![(src, before.group.clone())]);
    assert_eq!(landed.covers, before.covers);
    assert_eq!(landed.receivables, before.receivables);
    assert_eq!(c.state(dst).ledger.balance(&accounts::unearned_premium(&id, NZD).id), unearned_before);
    assert_eq!(c.state(dst).ledger.natural_balance(&accounts::policy_suspense(&id, NZD).id), 1000);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // the target catches up the two days of earning missed in flight, and life goes on
    days(&mut c, 20);
    let pol = &c.state(dst).policies[&id];
    assert_eq!(pol.periods_billed, 2);
    assert_eq!(pol.outstanding_minor(), MONTHLY - 1000);
    let earned: i128 = [src, dst].iter().map(|p| -c.state(*p).ledger.role_total(accounts::role::EARNED_PREMIUM)).sum();
    assert_eq!(earned, MONTHLY as i128 * FINE_PER_MINOR + earned_after(4, 31));
}

/// Purpose: when the target partition refuses a transfer, the policy is unfrozen where it was, with
/// its balances as before and the books reconciling.
/// Why it matters: a transfer can fail for ordinary reasons, such as a product not active on the
/// target. The policy must come back into service by itself, not sit frozen until someone notices.
#[test]
fn a_rejected_transfer_unfreezes_and_nothing_has_moved() {
    let mut c = Cluster::in_memory(2, 7);
    c.open(d(2027, 11, 15)).unwrap();
    // only partition 0 has the configuration, so partition 1 must refuse the policy
    for cmd in shipped_activations().unwrap() {
        c.submit(0, "config", cmd).unwrap();
    }
    let n = (1..).find(|n| c.partition_of(&GroupId(format!("grp-{n:08}"))) == 0).unwrap();
    let (src, id) = standard(&mut c, n, d(2027, 11, 15));
    assert_eq!(src, 0);
    let digest_before = c.state(0).ledger.balance(&accounts::premium_receivable(&id, NZD).id);
    let target_group = GroupId(
        (0..)
            .map(|i| format!("grp-new-{i}"))
            .find(|g| c.partition_of(&GroupId(g.clone())) == 1)
            .unwrap(),
    );
    c.submit(
        0,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t-2".into(),
            target_group,
            target_partition: 1,
        },
    )
    .unwrap();
    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    let pol = &c.state(0).policies[&id];
    assert_eq!(pol.status, PolicyStatus::InForce);
    assert!(!pol.is_frozen());
    assert_eq!(c.state(0).ledger.balance(&accounts::premium_receivable(&id, NZD).id), digest_before);
    assert!(!c.state(1).policies.contains_key(&id));
    assert!(c.reconcile().is_empty());
}

/// Purpose: a command repeated with the same key is applied once, a command from a module not
/// allowed to send it is refused, and the business date moves one day at a time and only by the
/// leader.
/// Why it matters: every caller retries, so a retry must never post twice. Authorisation in the
/// core is the second check behind the API: a compromised module still cannot send another module's
/// commands.
#[test]
fn commands_are_idempotent_and_only_accepted_from_the_right_module() {
    let mut c = cluster(1, d(2027, 11, 15));
    let (p, id) = standard(&mut c, 14, d(2027, 11, 15));
    let cmd = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "r1".into(),
        amount: Money::new(MONTHLY, NZD),
    };
    let first = c.submit_keyed(p, "rail-123", "cash-matching", cmd.clone());
    assert!(first.result.is_ok() && !first.duplicate);
    let hash = c.state(p).ledger.head();
    let again = c.submit_keyed(p, "rail-123", "cash-matching", cmd.clone());
    assert!(again.duplicate && again.result.is_ok() && again.entries.is_empty());
    assert_eq!(c.state(p).ledger.head(), hash, "a duplicate posts nothing");
    let wrong = c.submit_keyed(p, "other", "notifications", cmd);
    assert_eq!(wrong.result.unwrap_err().code, "caller_not_allowed");
    // the business date only moves one day at a time, by the leader
    assert!(c.submit(p, "leader", Command::AdvanceBusinessDate { to: d(2027, 11, 17) }).is_err());
    assert!(c.submit(p, "end-of-day", Command::AdvanceBusinessDate { to: d(2027, 11, 16) }).is_err());
}

/// Purpose: after a hundred days of business, replaying each partition's log gives the same state
/// hash, the journal's chain verifies, re-summing the journal gives every account's balance, and
/// the event sequence has no gaps.
/// Why it matters: determinism is what makes replication and audit possible. Two replicas, or an
/// auditor a year later, must reach exactly this state from exactly this log.
#[test]
fn a_recorded_log_replays_to_an_identical_state_hash_and_the_chain_verifies() {
    let mut c = cluster(2, d(2027, 11, 15));
    let mut ids = vec![];
    for n in 20..40 {
        ids.push(standard(&mut c, n, d(2027, 11, 15)));
    }
    for (i, (p, id)) in ids.iter().enumerate() {
        if i % 3 != 0 {
            pay(&mut c, *p, id, &format!("r-{i}"), MONTHLY);
        }
    }
    days(&mut c, 100);
    for (p, node) in &c.nodes {
        let replayed = Replica::recover(node.replica.log.clone(), CoreState::new(*p)).unwrap();
        assert_eq!(replayed.state.state_hash(), node.state().state_hash());
        // an independent verifier recomputes every link and commitment
        assert_eq!(verify_chain(*p, &node.journal).unwrap(), node.state().ledger.head());
        // re-summing the postings gives the materialised balances
        let sums = resum(&node.journal);
        for (account, balance) in node.state().ledger.accounts() {
            assert_eq!(sums.get(&account.id).copied().unwrap_or(0), balance, "{}", account.id);
        }
        // events are gapless
        assert!(node.outbox.iter().enumerate().all(|(i, e)| e.seq == i as u64 + 1));
        assert!(node
            .outbox
            .iter()
            .any(|e| matches!(e.event, Event::PolicyStatusChanged { to: PolicyStatus::Lapsed, .. })));
    }
}
