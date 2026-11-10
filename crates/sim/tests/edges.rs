//! The refusals and rarer paths of the policy state machine: everything the
//! happy-path scenarios do not reach. Each test names the rule it protects.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::command::MemberChange;
use ic_core::event::{Event, MessageBody};
use ic_core::{Command, CoverStatus, GroupId, LifeFactors, LifeId, LifeKind, PartyId, PolicyId, PolicyStatus, Reject};
use ic_ledger::PartitionId;
use ic_money::{Currency::AUD, Currency::NZD, Money};
use ic_product::{load_product, Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member, named_life, term_life};
use ic_sim::{Cluster, Delivery};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

const START: (i32, u32, u32) = (2027, 11, 15);

fn start() -> NaiveDate {
    d(START.0, START.1, START.2)
}

fn cluster(partitions: PartitionId) -> Cluster {
    let mut c = Cluster::in_memory(partitions, 42);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

/// Close `n` business days, insisting every reconciliation is clean.
fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn receipt(policy: &PolicyId, id: &str, amount: Money) -> Command {
    Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: id.to_string(),
        amount,
    }
}

fn pay(c: &mut Cluster, policy: &PolicyId, id: &str, minor: i64) {
    c.submit(0, "cash-matching", receipt(policy, id, Money::new(minor, NZD))).unwrap();
}

/// The standard policy of the scenario tests: 61.84 a month.
fn standard(c: &mut Cluster, n: u64) -> PolicyId {
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

const MONTHLY: i64 = 61_84;

fn code(r: Result<(), Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

fn outstanding(c: &Cluster, id: &PolicyId) -> i64 {
    c.state(0).policies[id].outstanding_minor()
}

fn events(c: &Cluster) -> Vec<&Event> {
    c.nodes[&0].outbox.iter().map(|e| &e.event).collect()
}

/// Purpose: every rule that guards policy issue refuses the application that breaks it,
/// with its own reason code, and leaves no trace of the policy.
/// Why it matters: issue is where bad data would enter the book for decades. New business
/// needs a distinct code for each refusal to tell the adviser what to fix, and a refused
/// application must not leave a half-built policy or any posting behind.
#[test]
fn each_issue_rule_refuses_the_application_that_breaks_it() {
    let mut c = cluster(1);
    let base = || term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let head = c.state(0).ledger.head();
    let mut refuse = |expected: &str, change: &dyn Fn(&mut ic_core::command::IssuePolicy)| {
        let mut app = base();
        change(&mut app);
        assert_eq!(code(c.submit(0, "new-business", issue_command(app))), expected);
    };
    refuse("inception", &|a| a.inception = d(2027, 11, 16));
    refuse("inception", &|a| a.inception = d(2026, 10, 1));
    refuse("unknown_product", &|a| a.product_code = "pet-plan".into());
    refuse("frequency", &|a| a.frequency = Frequency::Quarterly);
    refuse("invalid_policy", &|a| a.lives.clear());
    refuse("invalid_policy", &|a| a.covers.clear());
    refuse("invalid_policy", &|a| a.lives.push(a.lives[0].clone()));
    refuse("unknown_cover", &|a| a.covers[0].code = "funeral".into());
    refuse("invalid_policy", &|a| a.covers[0].life = None);
    refuse("invalid_policy", &|a| a.covers[0].life = Some(LifeId("someone-else".into())));
    refuse("sum_insured", &|a| a.covers[0].sum_insured_minor = 10_000_00);
    // The limit is on the life's total across layers, not on each layer.
    refuse("sum_insured", &|a| {
        let mut second = a.covers[0].clone();
        second.sum_insured_minor = 4_600_000_00;
        a.covers.push(second);
    });
    // Nobody under 18 is in the rate table: unpriceable, so refused whole.
    refuse("premium", &|a| a.lives[0].factors.date_of_birth = Some(d(2020, 1, 1)));
    refuse("apply_budget", &|a| {
        a.lives = (0..101)
            .map(|i| named_life(&format!("l{i}"), &format!("p{i}"), d(1990, 1, 1), Sex::Male, false))
            .collect();
    });
    assert!(c.state(0).policies.is_empty());
    assert_eq!(c.state(0).ledger.head(), head, "a refused application posts nothing");

    c.submit(0, "new-business", issue_command(base())).unwrap();
    assert_eq!(code(c.submit(0, "new-business", issue_command(base()))), "policy_exists");
}

/// Purpose: at the anniversary where the life reaches a cover's expiry age, that cover
/// ends and stops being billed while the rest of the policy carries on.
/// Why it matters: trauma cover ends at 70 but life cover runs to 100. Charging premium
/// for a cover that can no longer pay a claim is exactly the kind of error a regulator
/// fines for, and it would only appear decades after issue.
#[test]
fn a_cover_expires_at_its_age_limit_and_the_rest_of_the_policy_continues() {
    let mut c = cluster(1);
    // Aged 69 at issue: trauma (expiry age 70) has one year left, life (100) has many.
    let issue = term_life(
        1,
        start(),
        d(1958, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        Some(100_000_00),
        Frequency::Yearly,
        None,
    );
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let first_bill = outstanding(&c, &id);
    pay(&mut c, &id, "year-1", first_bill);
    days(&mut c, 366);

    let pol = &c.state(0).policies[&id];
    assert_eq!(pol.status, PolicyStatus::InForce);
    assert_eq!(pol.covers["trauma"].status, CoverStatus::Expired);
    assert_eq!(pol.covers["trauma"].layers[0].end, Some(d(2028, 11, 15)));
    assert_eq!(pol.covers["trauma"].layers.len(), 1, "an expired cover is not indexed");
    assert_eq!(pol.covers["life"].status, CoverStatus::InForce);
    assert!(events(&c).iter().any(|e| matches!(e, Event::CoverExpired { cover, .. } if cover == "trauma")));
    // The second year's bill has a life line and no trauma line.
    let second = pol.receivables.iter().max_by_key(|r| r.id).unwrap();
    assert_eq!(second.period_start, d(2028, 11, 15));
    assert!(
        second.net_fine.contains_key("life") && !second.net_fine.contains_key("trauma"),
        "{:?}",
        second.net_fine
    );
}

/// Purpose: when the last cover on a policy expires, the policy itself becomes Expired,
/// loses its standing, is never billed again, and refuses further money.
/// Why it matters: an expired policy that kept billing would build a receivable nobody
/// owes and then lapse for non-payment, sending arrears letters to a customer whose cover
/// simply ran its course.
#[test]
fn a_policy_whose_last_cover_expires_ends_cleanly() {
    let mut c = cluster(1);
    let mut issue = term_life(
        1,
        start(),
        d(1958, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        Some(100_000_00),
        Frequency::Yearly,
        None,
    );
    issue.covers.retain(|cover| cover.code == "trauma");
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let bill = outstanding(&c, &id);
    pay(&mut c, &id, "year-1", bill);
    days(&mut c, 366 + 30);

    let pol = &c.state(0).policies[&id];
    assert_eq!(pol.status, PolicyStatus::Expired);
    assert_eq!(pol.standing, None);
    assert_eq!(pol.periods_billed, 1, "nothing is billed after expiry");
    assert_eq!(pol.outstanding_minor(), 0);
    assert_eq!(pol.unearned_fine(), 0, "the one year billed was earned in full");
    assert!(events(&c)
        .iter()
        .any(|e| matches!(e, Event::PolicyStatusChanged { to: PolicyStatus::Expired, .. })));
    assert_eq!(code(c.submit(0, "cash-matching", receipt(&id, "late", Money::new(100, NZD)))), "policy_ended");
    assert_eq!(
        code(c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() })),
        "not_in_force"
    );
}

/// Purpose: indexation layers older than the backdating limit, with identical terms, merge
/// into one layer with the summed amount; newer layers stay separate; the total cover is
/// unchanged and the policy keeps billing cleanly.
/// Why it matters: without merging, a 40-year policy carries 40 layers and every bill
/// costs 40 rate lookups. Merging must never change what the customer is covered for, and
/// must not touch layers a backdated correction could still reach.
#[test]
fn old_identical_layers_merge_without_changing_the_cover() {
    let mut c = cluster(1);
    let id = standard(&mut c, 1);
    pay(&mut c, &id, "ahead", 60 * MONTHLY);
    // Before any anniversary there is one layer and nothing to merge.
    let merge = |cover: &str| Command::MergeLayers {
        policy: id.clone(),
        cover: cover.to_string(),
    };
    assert_eq!(code(c.submit(0, "policy-servicing", merge("life"))), "nothing_to_merge");
    assert_eq!(code(c.submit(0, "policy-servicing", merge("funeral"))), "unknown_cover");

    days(&mut c, 800); // two anniversaries: layers start 2027, 2028 and 2029
    let today = c.business_date().unwrap();
    let before = c.state(0).policies[&id].covers["life"].clone();
    assert_eq!(before.layers.len(), 3);

    c.submit(0, "policy-servicing", merge("life")).unwrap();
    let after = &c.state(0).policies[&id].covers["life"];
    assert_eq!(after.layers.len(), 2, "the 2029 layer is inside the backdating limit and stays apart");
    assert_eq!(after.layers[0].sum_insured_minor, 512_500_00);
    assert_eq!(after.sum_insured_on(today), before.sum_insured_on(today));
    assert!(events(&c)
        .iter()
        .any(|e| matches!(e, Event::LayersMerged { removed, into: 1, .. } if removed == &vec![2])));
    assert_eq!(code(c.submit(0, "policy-servicing", merge("life"))), "nothing_to_merge");
    days(&mut c, 45);
}

/// Purpose: cancellation and reinstatement refuse a policy in the wrong state, and every
/// policy command refuses a policy that is not on the partition.
/// Why it matters: cancelling twice would refund twice. Reinstating a policy that never
/// lapsed would reverse a write-off that never happened.
#[test]
fn lifecycle_commands_refuse_a_policy_in_the_wrong_state() {
    let mut c = cluster(1);
    let id = standard(&mut c, 1);
    assert_eq!(code(c.submit(0, "collections", Command::Reinstate { policy: id.clone() })), "not_lapsed");
    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(
        code(c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() })),
        "not_in_force"
    );
    let ghost = PolicyId("pol-ghost".into());
    assert_eq!(
        code(c.submit(0, "policy-servicing", Command::CancelPolicy { policy: ghost.clone() })),
        "unknown_policy"
    );
    assert_eq!(code(c.submit(0, "cash-matching", receipt(&ghost, "r", Money::new(1, NZD)))), "unknown_policy");
    assert!(c.reconcile().is_empty());
}

/// Purpose: the partition's own housekeeping commands are guarded: the business date
/// opens once, an end-of-day chunk and a membership chunk have a maximum size, and a
/// command for a partition that does not exist is refused.
/// Why it matters: every command must finish inside its apply budget so one oversized
/// request cannot stall a partition's log for everyone else.
#[test]
fn housekeeping_commands_are_bounded() {
    let mut c = cluster(1);
    assert_eq!(code(c.submit(0, "leader", Command::OpenBusinessDate { date: start() })), "business_date");
    let too_many: Vec<PolicyId> = (0..65).map(|i| PolicyId(format!("p{i}"))).collect();
    assert_eq!(code(c.submit(0, "end-of-day", Command::RunEndOfDay { policies: too_many })), "apply_budget");
    let changes: Vec<MemberChange> = (0..101)
        .map(|i| MemberChange::Left {
            life: LifeId(format!("m{i}")),
            effective: start(),
        })
        .collect();
    assert_eq!(
        code(c.submit(
            0,
            "group-admin",
            Command::ApplyMembership {
                policy: PolicyId("x".into()),
                changes
            }
        )),
        "apply_budget"
    );
    assert_eq!(code(c.submit(9, "leader", Command::OpenBusinessDate { date: start() })), "unknown_partition");

    // A partition with no business date refuses everything but opening it.
    let mut unopened = Cluster::in_memory(1, 1);
    let advance = Command::AdvanceBusinessDate { to: start() };
    assert_eq!(code(unopened.submit(0, "leader", advance)), "business_date");
    assert!(unopened.end_of_day().is_err());
}

/// Purpose: one policy failing in the end-of-day run becomes an exception item for that
/// policy, and the rest of the chunk is still processed.
/// Why it matters: the nightly run covers the whole book. If one bad policy aborted the
/// chunk, every other customer in it would miss billing and earning for the day.
#[test]
fn one_failing_policy_does_not_stop_the_end_of_day_chunk() {
    let mut c = cluster(1);
    let good = standard(&mut c, 1);
    pay(&mut c, &good, "r1", MONTHLY);
    let ghost = PolicyId("pol-ghost".into());
    c.submit(0, "leader", Command::AdvanceBusinessDate { to: d(2027, 11, 16) }).unwrap();
    c.submit(
        0,
        "end-of-day",
        Command::RunEndOfDay {
            policies: vec![ghost.clone(), good.clone()],
        },
    )
    .unwrap();
    assert!(events(&c)
        .iter()
        .any(|e| matches!(e, Event::PolicyException { policy, code, .. } if policy == &ghost && code == "unknown_policy")));
    // The good policy earned its day regardless.
    assert!(-c.state(0).ledger.role_total(accounts::role::EARNED_PREMIUM) > 0);
}

/// Purpose: a receipt must be a positive amount in the policy's currency and new to the
/// policy; a reversal must name a receipt that exists and has not been reversed.
/// Why it matters: these are the checks that stop a payment file being applied twice
/// under different keys, and stop a dishonour being processed twice, either of which
/// would misstate what the customer owes.
#[test]
fn receipts_and_reversals_are_validated() {
    let mut c = cluster(1);
    let id = standard(&mut c, 1);
    for bad in [Money::new(0, NZD), Money::new(-5, NZD), Money::new(MONTHLY, AUD)] {
        assert_eq!(code(c.submit(0, "cash-matching", receipt(&id, "bad", bad))), "invalid_amount");
    }
    pay(&mut c, &id, "r1", MONTHLY);
    // The same receipt under a fresh idempotency key is still the same receipt.
    assert_eq!(
        code(c.submit(0, "cash-matching", receipt(&id, "r1", Money::new(MONTHLY, NZD)))),
        "duplicate_receipt"
    );
    let reverse = |r: &str| Command::ReverseReceipt {
        policy: id.clone(),
        receipt: r.to_string(),
    };
    assert_eq!(code(c.submit(0, "cash-matching", reverse("never-seen"))), "unknown_receipt");
    c.submit(0, "cash-matching", reverse("r1")).unwrap();
    assert_eq!(code(c.submit(0, "cash-matching", reverse("r1"))), "already_reversed");
    assert_eq!(outstanding(&c, &id), MONTHLY);
    assert!(c.reconcile().is_empty());
}

/// Purpose: a refund from suspense must be positive, in currency, and no more than the
/// policy holds; and a receipt whose overpayment has since been refunded cannot be
/// reversed automatically.
/// Why it matters: suspense is the customer's money. Refunding more than is held, or
/// reversing a receipt after its surplus has already left the building, would drive the
/// suspense account into debit: money paid out that the insurer never received.
#[test]
fn suspense_cannot_be_refunded_or_reversed_below_zero() {
    let mut c = cluster(1);
    let id = standard(&mut c, 1);
    pay(&mut c, &id, "r1", MONTHLY + 500);
    let refund = |amount: Money| Command::RefundSuspense { policy: id.clone(), amount };
    assert_eq!(code(c.submit(0, "policy-servicing", refund(Money::new(0, NZD)))), "invalid_amount");
    assert_eq!(code(c.submit(0, "policy-servicing", refund(Money::new(500, AUD)))), "invalid_amount");
    assert_eq!(code(c.submit(0, "policy-servicing", refund(Money::new(501, NZD)))), "insufficient_suspense");
    c.submit(0, "policy-servicing", refund(Money::new(500, NZD))).unwrap();
    let reverse = Command::ReverseReceipt {
        policy: id.clone(),
        receipt: "r1".into(),
    };
    assert_eq!(code(c.submit(0, "cash-matching", reverse)), "suspense_consumed");
    assert_eq!(c.state(0).ledger.natural_balance(&accounts::policy_suspense(&id, NZD).id), 0);
    assert!(c.reconcile().is_empty());
}

/// Purpose: once a policy is cancelled, a dishonour of an earlier receipt is not reversed
/// automatically, and new money is refused.
/// Why it matters: cancellation closes the receivables and may already have refunded the
/// customer. Reopening a closed receivable would resurrect a debt on a dead policy; this
/// is a case for a person, so the core must refuse and say why.
#[test]
fn a_cancelled_policy_takes_no_new_money_and_no_automatic_reversal() {
    let mut c = cluster(1);
    let id = standard(&mut c, 1);
    pay(&mut c, &id, "r1", MONTHLY);
    days(&mut c, 20);
    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(c.state(0).policies[&id].status, PolicyStatus::Cancelled);
    let reverse = Command::ReverseReceipt {
        policy: id.clone(),
        receipt: "r1".into(),
    };
    assert_eq!(code(c.submit(0, "cash-matching", reverse)), "receivable_closed");
    assert_eq!(code(c.submit(0, "cash-matching", receipt(&id, "r2", Money::new(100, NZD)))), "policy_ended");
    days(&mut c, 2);
}

/// A copy of the shipped term life product whose life cover carries a 10% levy.
fn levied_product(c: &mut Cluster) {
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-levied\"")
        .replace("expiry_age = 100,", "expiry_age = 100, levies = [levy(\"fsl\", \"0.10\")],");
    let product = load_product("term_life_levied.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
}

fn levied(c: &mut Cluster, n: u64) -> PolicyId {
    let mut issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    issue.product_code = "term-life-levied".into();
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

/// Purpose: a levy is billed on top of the premium into its own payable account, and a
/// mid-term cancellation hands back the levy for the unused days along with the premium.
/// Why it matters: a levy is collected on behalf of a third party. What is remitted must
/// equal what customers were finally charged, so a refund has to unwind the levy too or
/// the insurer over-remits out of its own pocket.
#[test]
fn a_levy_is_billed_to_its_own_account_and_refunded_pro_rata_on_cancellation() {
    let mut c = cluster(1);
    levied_product(&mut c);
    let id = levied(&mut c, 1);
    let levy_account = accounts::levy_payable("fsl", NZD).id;
    // Premium 55.84, levy 10% = 5.58, fee 6.00.
    assert_eq!(c.state(0).ledger.natural_balance(&levy_account), 5_58);
    assert_eq!(outstanding(&c, &id), 55_84 + 5_58 + 6_00);
    pay(&mut c, &id, "r1", 55_84 + 5_58 + 6_00);
    days(&mut c, 20); // past cooling-off: 10 of 30 days unused
    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
    // A third of 5.58 is 1.86: the levy now owed is for the 20 days of cover given.
    assert_eq!(c.state(0).ledger.natural_balance(&levy_account), 5_58 - 1_86);
    assert!(
        c.state(0).ledger.natural_balance(&accounts::policy_suspense(&id, NZD).id) > 1_86,
        "premium and levy are both refunded"
    );
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}

/// Purpose: when a levied policy lapses without ever paying, the levy that was billed but
/// never collected is reversed in full.
/// Why it matters: the insurer must not owe a third party a levy on premium it wrote off.
#[test]
fn a_lapse_reverses_the_levy_on_unpaid_premium() {
    let mut c = cluster(1);
    levied_product(&mut c);
    let id = levied(&mut c, 1);
    days(&mut c, 91);
    assert_eq!(c.state(0).policies[&id].status, PolicyStatus::Lapsed);
    assert_eq!(c.state(0).ledger.natural_balance(&accounts::levy_payable("fsl", NZD).id), 0);
}

fn scheme(c: &mut Cluster, n: u64) -> PolicyId {
    let members = (1..=3).map(|i| member(&format!("m{i}"), d(1980, 1, 1), "staff", 80_000_00)).collect();
    let issue = group_scheme(n, start(), members);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

fn membership(c: &mut Cluster, id: &PolicyId, change: MemberChange) -> Result<(), Reject> {
    c.submit(
        0,
        "group-admin",
        Command::ApplyMembership {
            policy: id.clone(),
            changes: vec![change],
        },
    )
}

/// Purpose: each rule on scheme membership refuses the change that breaks it: not a
/// scheme, backdated before inception, already a member, not a member, leaving before
/// joining, and a scheme no longer in force.
/// Why it matters: membership files come from employers' payroll systems and are full of
/// mistakes. Each must be refused with a reason the employer can act on, and a refused
/// chunk must change nothing, because every accepted change re-prices billed periods.
#[test]
fn each_membership_rule_refuses_the_change_that_breaks_it() {
    let mut c = cluster(1);
    let retail = standard(&mut c, 1);
    let id = scheme(&mut c, 2);
    let owed = outstanding(&c, &id);
    let joiner = |name: &str, effective| MemberChange::Joined {
        life: member(name, d(1985, 5, 5), "staff", 90_000_00),
        effective,
    };
    let leaver = |name: &str, effective| MemberChange::Left {
        life: LifeId(name.into()),
        effective,
    };

    assert_eq!(code(membership(&mut c, &retail, joiner("m9", start()))), "not_a_scheme");
    assert_eq!(code(membership(&mut c, &id, joiner("m9", d(2027, 11, 1)))), "backdating_limit");
    assert_eq!(code(membership(&mut c, &id, joiner("m1", start()))), "member_exists");
    assert_eq!(code(membership(&mut c, &id, leaver("nobody", start()))), "unknown_member");
    let change = MemberChange::Changed {
        life: LifeId("nobody".into()),
        effective: start(),
        factors: LifeFactors::default(),
    };
    assert_eq!(code(membership(&mut c, &id, change)), "unknown_member");
    assert_eq!(outstanding(&c, &id), owed, "refused changes re-price nothing");

    days(&mut c, 10);
    membership(&mut c, &id, joiner("m4", d(2027, 11, 20))).unwrap();
    assert_eq!(code(membership(&mut c, &id, leaver("m4", d(2027, 11, 18)))), "invalid_change");
    // A member who has left cannot leave again.
    membership(&mut c, &id, leaver("m3", d(2027, 11, 22))).unwrap();
    assert_eq!(code(membership(&mut c, &id, leaver("m3", d(2027, 11, 23)))), "unknown_member");

    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(code(membership(&mut c, &id, joiner("m5", d(2027, 11, 20)))), "not_in_force");
    // The scheme held a credit adjustment (the leaver) when it was cancelled: the books
    // still balance, and stay balanced.
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    days(&mut c, 2);
}

/// Purpose: when a scheme that has paid in full learns late that a member left, the
/// credit for the unused cover goes to the scheme's suspense account.
/// Why it matters: the employer has overpaid. The credit must be held as their money and
/// consumed by the next invoice; leaving the receivable negative would understate the
/// insurer's assets and break the receivable control.
#[test]
fn a_credit_on_a_fully_paid_scheme_goes_to_suspense() {
    let mut c = cluster(1);
    let id = scheme(&mut c, 1);
    let owed = outstanding(&c, &id);
    assert_eq!(owed, 3 * 14_67);
    pay(&mut c, &id, "nov", owed);
    days(&mut c, 10);
    membership(
        &mut c,
        &id,
        MemberChange::Left {
            life: LifeId("m1".into()),
            effective: start(),
        },
    )
    .unwrap();
    assert_eq!(outstanding(&c, &id), 0);
    assert_eq!(c.state(0).ledger.natural_balance(&accounts::policy_suspense(&id, NZD).id), 14_67);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    days(&mut c, 25); // the December invoice for two members consumes the credit
    assert_eq!(outstanding(&c, &id), 2 * 14_67 - 14_67);
}

/// Purpose: identification links an unnamed member to one person, is safe to repeat, and
/// refuses a second, different person, a named life, and a member who does not exist.
/// Why it matters: identification happens at claim time, when money is about to be paid.
/// Re-pointing a member record at a different person would redirect a death benefit.
#[test]
fn a_member_is_identified_once_and_only_as_one_person() {
    let mut c = cluster(1);
    let retail = standard(&mut c, 1);
    let id = scheme(&mut c, 2);
    let identify = |policy: &PolicyId, life: &str, party: &str| Command::IdentifyMember {
        policy: policy.clone(),
        life: LifeId(life.into()),
        party: PartyId(party.into()),
    };
    c.submit(0, "claims", identify(&id, "m1", "party-a")).unwrap();
    c.submit(0, "group-admin", identify(&id, "m1", "party-a")).unwrap();
    assert_eq!(code(c.submit(0, "claims", identify(&id, "m1", "party-b"))), "already_identified");
    assert_eq!(code(c.submit(0, "claims", identify(&id, "ghost", "party-a"))), "unknown_member");
    assert_eq!(code(c.submit(0, "claims", identify(&retail, "life-1", "party-a"))), "not_unnamed");
    let kind = &c.state(0).policies[&id].lives[&LifeId("m1".into())].kind;
    assert!(matches!(kind, LifeKind::UnnamedMember { linked_party: Some(p), .. } if p.0 == "party-a"));
}

/// A group whose home is the given partition.
fn group_on(c: &Cluster, partition: PartitionId) -> GroupId {
    (0..)
        .map(|i| GroupId(format!("grp-move-{i}")))
        .find(|g| c.partition_of(g) == partition)
        .unwrap()
}

/// Purpose: a transfer cannot begin to the policy's own group, for a policy that has
/// ended, or while money on the policy is held.
/// Why it matters: a transfer moves every balance through in-transit accounts. Starting
/// one that cannot land, or moving a balance with a hold on it, would strand money
/// between partitions where no reconciliation on either side can see it whole.
#[test]
fn a_transfer_that_cannot_land_cleanly_does_not_begin() {
    let mut c = cluster(2);
    let n = (1..).find(|n| c.partition_of(&GroupId(format!("grp-{n:08}"))) == 0).unwrap();
    let id = standard(&mut c, n);
    let own_group = c.state(0).policies[&id].group.clone();
    let begin = |group: GroupId, to: PartitionId| Command::BeginTransfer {
        policy: id.clone(),
        transfer_id: "t1".into(),
        target_group: group,
        target_partition: to,
    };
    assert_eq!(code(c.submit(0, "transfer", begin(own_group, 0))), "same_group");

    // Holds have no command of their own in this phase, so place one directly.
    pay(&mut c, &id, "r1", MONTHLY + 500);
    let suspense = accounts::policy_suspense(&id, NZD).id;
    let node = c.nodes.get_mut(&0).unwrap();
    node.replica
        .state
        .ledger
        .place_hold(ic_ledger::Hold {
            id: "h".into(),
            account: suspense,
            amount: 100,
            expires_on: None,
        })
        .unwrap();
    let target = group_on(&c, 1);
    assert_eq!(code(c.submit(0, "transfer", begin(target.clone(), 1))), "holds_outstanding");
    c.nodes.get_mut(&0).unwrap().replica.state.ledger.release_hold("h").unwrap();

    c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(code(c.submit(0, "transfer", begin(target, 1))), "not_transferable");
    assert_eq!(c.messages_in_flight(), 0, "nothing was sent");
}

/// Purpose: the target refuses a transfer record addressed to another partition or one
/// whose contents do not match its hash; the genuine record then lands, and late or
/// repeated acknowledgements change nothing. Afterwards the source refuses commands for
/// the policy because it has moved.
/// Why it matters: the record carries a whole policy and its balances between two
/// independent logs. A misrouted or altered record must never create a policy, and
/// at-least-once delivery means every step will sometimes arrive twice.
#[test]
fn a_transfer_record_is_checked_on_arrival_and_acknowledgements_are_idempotent() {
    let mut c = cluster(2);
    let n = (1..).find(|n| c.partition_of(&GroupId(format!("grp-{n:08}"))) == 0).unwrap();
    let id = standard(&mut c, n);
    pay(&mut c, &id, "r1", MONTHLY);
    let target = group_on(&c, 1);
    c.submit(
        0,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t1".into(),
            target_group: target,
            target_partition: 1,
        },
    )
    .unwrap();
    let MessageBody::TransferRecord(record) = c.nodes[&0].messages[0].body.clone() else {
        panic!("the first message of a transfer is its record");
    };

    assert_eq!(
        code(c.submit(0, "transfer", Command::LandTransfer { record: record.clone() })),
        "wrong_partition"
    );
    let mut altered = record.clone();
    altered.policy.number = "FORGED".into();
    assert_eq!(code(c.submit(1, "transfer", Command::LandTransfer { record: altered })), "bad_record");
    assert!(!c.state(1).policies.contains_key(&id));

    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    assert!(c.relay_errors.is_empty(), "{:?}", c.relay_errors);
    assert_eq!(c.locate(&id), Some(1));
    let head = (c.state(0).ledger.head(), c.state(1).ledger.head());
    // The record again under a new key, a repeated acknowledgement, and an abort for a
    // transfer that already completed: none of them moves anything.
    c.submit(1, "transfer", Command::LandTransfer { record }).unwrap();
    c.submit(
        0,
        "transfer",
        Command::CompleteTransfer {
            policy: id.clone(),
            transfer_id: "t1".into(),
        },
    )
    .unwrap();
    c.submit(
        0,
        "transfer",
        Command::AbortTransfer {
            policy: id.clone(),
            transfer_id: "t1".into(),
        },
    )
    .unwrap();
    assert_eq!((c.state(0).ledger.head(), c.state(1).ledger.head()), head);
    assert_eq!(c.messages_in_flight(), 0);

    assert_eq!(
        code(c.submit(0, "cash-matching", receipt(&id, "r2", Money::new(100, NZD)))),
        "policy_transferred"
    );
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}

/// Purpose: a rate table scoped to one scheme travels with the scheme when it moves, and
/// is extended to cover the scheme's new group.
/// Why it matters: a scheme's negotiated rates exist only for that scheme. If they stayed
/// behind, the target partition would bill the next invoice at standard rates.
#[test]
fn a_schemes_own_rate_table_moves_with_it() {
    let mut c = cluster(2);
    let n = (1..).find(|n| c.partition_of(&GroupId(format!("grp-{n:08}"))) == 0).unwrap();
    let issue = group_scheme(n, start(), vec![member("m1", d(1980, 1, 1), "staff", 80_000_00)]);
    let (id, group) = (issue.policy.clone(), issue.group.clone());
    let mut table = ic_product::load_rate_table("x", include_str!("../../../products/rates/group_life_standard.star")).unwrap();
    table.version = 2;
    table.scope = Some(vec![group.0.clone()]);
    table.rows[0].annual_rate = ic_money::DecStr::parse("2.20").unwrap();
    // Only the source partition knows the scheme's own rates.
    c.submit(0, "config", Command::ActivateRateTable { table: Box::new(table) }).unwrap();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    assert_eq!(outstanding(&c, &id), 29_33);
    pay(&mut c, &id, "nov", 29_33);

    let target = group_on(&c, 1);
    c.submit(
        0,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t1".into(),
            target_group: target,
            target_partition: 1,
        },
    )
    .unwrap();
    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    assert_eq!(c.locate(&id), Some(1));
    days(&mut c, 30);
    // December is billed on the target at the scheme's own rate, not the standard 14.67.
    assert_eq!(c.state(1).policies[&id].outstanding_minor(), 29_33);
}
