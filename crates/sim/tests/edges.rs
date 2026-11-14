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

/// A copy of the shipped term life product whose life cover is on a level
/// premium to the given age.
fn level_policy(c: &mut Cluster, n: u64, to_age: u32) -> PolicyId {
    let code = format!("term-life-level-{to_age}");
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", &format!("code = \"{code}\""))
        .replace("stepped(\"life-base\")", &format!("level(\"life-base\", {to_age})"));
    let product = load_product("term_life_level.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let mut issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    issue.product_code = code;
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

/// The total of the latest bill raised on a policy.
fn latest_bill(c: &Cluster, id: &PolicyId) -> i64 {
    c.state(0).policies[id].receivables.iter().max_by_key(|r| r.period_start).unwrap().total_minor
}

/// Purpose: a level premium prices each layer on the life's age when that layer was first
/// rated, until the cover's stated age, from which it is priced on attained age like a
/// stepped premium; and layers on a level cover that were rated on different dates do not
/// merge.
/// Why it matters: the customer on a level premium paid more at the start so that the
/// price would not climb with age. Pricing a layer on today's age, or merging a later
/// indexation layer into the first one and so repricing it at the entry age, would charge
/// an amount the contract does not allow, in one direction or the other.
#[test]
fn a_level_premium_keeps_each_layers_rating_age_and_such_layers_do_not_merge() {
    let mut c = cluster(1);
    let stepped = standard(&mut c, 1);
    let level = level_policy(&mut c, 2, 65);
    let short = level_policy(&mut c, 3, 39);
    for id in [&stepped, &level, &short] {
        pay(&mut c, id, "ahead", 80 * MONTHLY);
    }
    // At issue the life is 37 on all three, so they are billed alike.
    assert_eq!(latest_bill(&c, &level), latest_bill(&c, &stepped));

    days(&mut c, 400); // one anniversary: the life is 38, in the next rate band
    let (s1, l1) = (latest_bill(&c, &stepped), latest_bill(&c, &level));
    assert!(l1 < s1, "the first layer stays at the age 37 rate: {l1} against {s1}");
    assert_eq!(latest_bill(&c, &short), l1, "still under the age the level premium runs to");

    days(&mut c, 400); // two anniversaries: the life is 39
    let s2 = latest_bill(&c, &stepped);
    assert!(latest_bill(&c, &level) < s2);
    assert_eq!(latest_bill(&c, &short), s2, "from 39 this cover is priced on attained age");

    // Three layers, each rated on its own date: nothing merges and the bill is unchanged.
    let merge = |id: &PolicyId| Command::MergeLayers {
        policy: id.clone(),
        cover: "life".to_string(),
    };
    let before = latest_bill(&c, &level);
    assert_eq!(c.state(0).policies[&level].covers["life"].layers.len(), 3);
    assert_eq!(code(c.submit(0, "policy-servicing", merge(&level))), "nothing_to_merge");
    c.submit(0, "policy-servicing", merge(&stepped)).unwrap();
    days(&mut c, 45);
    assert_eq!(latest_bill(&c, &level), before);
    assert_eq!(c.state(0).policies[&level].covers["life"].layers.len(), 3);
}

/// Purpose: a rate table can carry rows for an occupation class. A life in that class is
/// priced on its row, a life with no class or another class on the general row, and the
/// class the life was priced on is published with the policy. A change of occupation after
/// issue reaches the life with underwriting's altered terms and reprices it from that day.
/// Why it matters: occupation is a rating factor in the design. A table that could not be
/// split by it would force every hazardous occupation onto a manual loading, and a life
/// priced on a class the extracts cannot see could never be checked against experience.
#[test]
fn a_rate_row_for_an_occupation_class_prices_only_lives_in_that_class() {
    let mut c = cluster(1);
    let rates = include_str!("../../../products/rates/life_base.star")
        .replace("id = \"life-base\"", "id = \"life-occ\"")
        .replace(
            "rows = [",
            "rows = [\n        rate(\"2.5770\", age_from = 33, age_to = 37, sex = \"female\", smoker = False, occupation_class = \"4\"),",
        );
    let table = ic_product::load_rate_table("life_occ.star", &rates).unwrap();
    c.activate_everywhere(Command::ActivateRateTable { table: Box::new(table) }).unwrap();
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-occ\"")
        .replace("stepped(\"life-base\")", "stepped(\"life-occ\")");
    let product = load_product("term_life_occ.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();

    let mut issue = |n: u64, class: Option<&str>| {
        let mut issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        issue.product_code = "term-life-occ".into();
        issue.lives[0].factors.occupation_class = class.map(str::to_string);
        let id = issue.policy.clone();
        c.submit(0, "new-business", issue_command(issue)).unwrap();
        id
    };
    let (plain, office, miner) = (issue(1, None), issue(2, Some("1")), issue(3, Some("4")));
    // The general rate is 1.2885 per thousand and the class 4 row is twice it: premium
    // 55.84 against 111.67 a month, each with the 6.00 fee.
    assert_eq!(outstanding(&c, &plain), MONTHLY);
    assert_eq!(outstanding(&c, &office), MONTHLY);
    assert_eq!(outstanding(&c, &miner), 111_67 + 6_00);
    let published = events(&c).into_iter().find_map(|e| match e {
        Event::PolicyIssued { policy, lives, .. } if *policy == miner => lives[0].occupation_class.clone(),
        _ => None,
    });
    assert_eq!(published.as_deref(), Some("4"));
    days(&mut c, 2);

    // The office worker goes down the mine. Underwriting alters the terms with the new class
    // and no other change: the life is rated on class 4 from today, the days left of the
    // billed month are charged the difference, and the class held before today is kept.
    let today = c.business_date().unwrap();
    let reclass = |class: &str| {
        Command::AlterUnderwrittenTerms(Box::new(ic_core::command::AlteredTerms {
            policy: office.clone(),
            cover: "life".into(),
            life: ic_core::LifeId("life-1".into()),
            loadings: vec![],
            exclusions: vec![],
            reference: "uw-occupation-1".into(),
            occupation_class: Some(class.into()),
        }))
    };
    assert_eq!(code(c.submit(0, "underwriting", reclass("1"))), "no_change");
    let owed = outstanding(&c, &office);
    c.submit(0, "underwriting", reclass("4")).unwrap();
    assert!(outstanding(&c, &office) > owed, "the rest of the month is billed at the higher rate");
    let life = &c.state(0).policies[&office].lives[&ic_core::LifeId("life-1".into())];
    let class_on = |on: NaiveDate| life.factors.as_of(on).and_then(|f| f.occupation_class.clone());
    assert_eq!(
        (class_on(today.pred_opt().unwrap()).as_deref(), class_on(today).as_deref()),
        (Some("1"), Some("4"))
    );
    assert_eq!(code(c.submit(0, "underwriting", reclass("4"))), "no_change");
    // The next full month is billed as the miner's is.
    days(&mut c, 31);
    assert_eq!(latest_bill(&c, &office), latest_bill(&c, &miner));
    assert_eq!(latest_bill(&c, &miner), 111_67 + 6_00);
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
    let refund = |amount: Money| Command::RefundSuspense {
        policy: id.clone(),
        amount,
        with_claim: None,
    };
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
/// Purpose: a policy cancelled after its cooling-off period with nothing ever paid owes
/// nothing and is owed nothing, whichever day of the billed period it is cancelled on: the
/// unearned premium goes against the unpaid receivable, the rest of the receivable is
/// written off, policy suspense stays at zero and the partition reconciles.
/// Why it matters: unearned premium is held to a fraction of a cent. Compared in whole
/// cents with what was owed, the fraction was rounded up and handed back, so a customer who
/// never paid was left with one cent to refund, a refund item for payments and a suspense
/// balance on a closed policy for finance to explain.
#[test]
fn cancelling_a_policy_nobody_paid_for_leaves_nothing_to_refund() {
    for after in 15..46 {
        let mut c = cluster(1);
        let id = standard(&mut c, 1);
        days(&mut c, after);
        c.submit(0, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
        let suspense = c.state(0).ledger.natural_balance(&accounts::policy_suspense(&id, NZD).id);
        assert_eq!(suspense, 0, "cancelled {after} days in");
        assert_eq!(outstanding(&c, &id), 0);
        assert!(c.reconcile().is_empty(), "{after} days in: {:?}", c.reconcile());
    }
}

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

/// Purpose: configuration that is active cannot be rewritten. The same content sent again is
/// accepted, a new version is accepted, and each way of changing what a version says is refused:
/// other content under an active product hash, other content under an active product version,
/// a rate table or a definition changed in place, configuration with no content hash, and an
/// index rate already in effect given another value. An index rate still in the future can be
/// amended.
/// Why it matters: a policy is priced from the product and rate table versions it was issued
/// under for as long as it lives, and its covers are bound to the wording of a definition by
/// hash. If a later request could replace what a version says, every policy on it would be
/// repriced or redefined from the next bill onward, with nothing in the journal to show the
/// terms had changed.
#[test]
fn active_configuration_cannot_be_rewritten() {
    let mut c = cluster(1);
    let conflict = |r: Result<(), Reject>| assert_eq!(r.unwrap_err().code, "config_conflict");
    let hash = c.state(0).catalog.product_on("term-life", start()).unwrap().content_hash.clone();
    for command in shipped_activations().unwrap() {
        // Repeating an activation changes nothing and is not an error.
        c.submit(0, "config", command.clone()).unwrap();
        match command {
            Command::ActivateProduct { product } => {
                let mut same_hash = product.clone();
                same_hash.statutory_fund.push('x');
                conflict(c.submit(0, "config", Command::ActivateProduct { product: same_hash }));
                let mut same_version = product.clone();
                same_version.statutory_fund.push('x');
                same_version.content_hash = format!("{:f>64}", product.code);
                conflict(c.submit(0, "config", Command::ActivateProduct { product: same_version.clone() }));
                let mut unhashed = product.clone();
                unhashed.content_hash.clear();
                conflict(c.submit(0, "config", Command::ActivateProduct { product: unhashed }));
                // Changed content under a new version and its own hash is how a product changes.
                same_version.version += 1;
                c.submit(0, "config", Command::ActivateProduct { product: same_version }).unwrap();
            }
            Command::ActivateRateTable { table } => {
                let mut changed = table.clone();
                changed.rows[0].annual_rate = ic_money::DecStr::parse("99").unwrap();
                conflict(c.submit(0, "config", Command::ActivateRateTable { table: changed }));
                let mut unhashed = table.clone();
                unhashed.content_hash.clear();
                conflict(c.submit(0, "config", Command::ActivateRateTable { table: unhashed }));
            }
            Command::ActivateDefinitions { definitions } => {
                let mut reworded = definitions.clone();
                reworded[0].text_hash = "0".repeat(64);
                conflict(c.submit(0, "config", Command::ActivateDefinitions { definitions: reworded }));
                let mut unhashed = definitions.clone();
                unhashed[0].text_hash.clear();
                conflict(c.submit(0, "config", Command::ActivateDefinitions { definitions: unhashed }));
            }
            Command::ActivateIndexRate { code, effective_from, .. } => {
                let rate = |r: &str| ic_money::DecStr::parse(r).unwrap();
                let activate = |code: &str, effective_from, r: &str| Command::ActivateIndexRate {
                    code: code.to_string(),
                    effective_from,
                    rate: rate(r),
                };
                conflict(c.submit(0, "config", activate(&code, effective_from, "0.5")));
                let next_year = d(2028, 1, 1);
                c.submit(0, "config", activate(&code, next_year, "0.03")).unwrap();
                c.submit(0, "config", activate(&code, next_year, "0.031")).unwrap();
                assert_eq!(c.state(0).catalog.index_rate_on(&code, next_year), Some(&rate("0.031")));
                assert_eq!(
                    c.state(0).catalog.index_rate_on(&code, start()),
                    c.state(0).catalog.index_rate_on(&code, effective_from)
                );
            }
            other => panic!("unexpected shipped command {}", other.name()),
        }
    }
    // The refused requests left the product exactly as it was, and the new version is in force.
    assert!(c.state(0).catalog.product(&hash).is_some_and(|p| !p.statutory_fund.ends_with('x')));
    assert_eq!(
        c.state(0).catalog.product_on("term-life", start()).unwrap().content_hash,
        format!("{:f>64}", "term-life")
    );
}

/// Purpose: a validation recorded for a rate table version is published once with the
/// report's hash, schema and who ran it, whether it arrives before or after the table is
/// activated; the same record twice is published once; a newer report replaces it; and a
/// record for content other than what is active under that version is refused.
/// Why it matters: finance holds the month's close for any table that is active without
/// a passed report. A record that could be attached to different content than the report
/// was run on would turn that check green for a table nobody validated.
#[test]
fn a_rate_table_validation_is_recorded_against_the_content_it_was_run_on() {
    use ic_core::command::TableValidation;
    let mut c = cluster(1);
    let active = c
        .take_events()
        .into_iter()
        .find_map(|e| match e.event {
            Event::RateTableActivated { id, version, content_hash, .. } if id == "life-base" => Some((version, content_hash)),
            _ => None,
        })
        .expect("the shipped life table is active");
    let record = |version: u32, content: &str, report: &str| {
        Command::RecordRateTableValidation(Box::new(TableValidation {
            id: "life-base".into(),
            version,
            content_hash: content.into(),
            report_hash: report.into(),
            schema_version: 1,
            by: "svc-finance".into(),
        }))
    };
    let validated = |c: &mut Cluster| {
        c.take_events()
            .into_iter()
            .filter_map(|e| match e.event {
                Event::RateTableValidated { version, report_hash, .. } => Some((version, report_hash)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(code(c.submit(0, "finance", record(active.0, "other-content", "report-1"))), "config_conflict");
    assert_eq!(code(c.submit(0, "finance", record(active.0, &active.1, ""))), "invalid_validation");
    assert_eq!(
        code(c.submit(0, "policy-servicing", record(active.0, &active.1, "report-1"))),
        "caller_not_allowed"
    );
    c.submit(0, "finance", record(active.0, &active.1, "report-1")).unwrap();
    c.submit(0, "finance", record(active.0, &active.1, "report-1")).unwrap();
    assert_eq!(validated(&mut c), vec![(active.0, "report-1".to_string())]);
    c.submit(0, "config", record(active.0, &active.1, "report-2")).unwrap();
    // A version not yet active can be validated ahead of its activation.
    c.submit(0, "finance", record(active.0 + 1, "next-content", "report-3")).unwrap();
    assert_eq!(
        validated(&mut c),
        vec![(active.0, "report-2".to_string()), (active.0 + 1, "report-3".to_string())]
    );
    assert_eq!(c.state(0).table_validations.len(), 2);
}

fn entitlement(policy: &PolicyId, code: &str, tier: Option<&str>, life: Option<&str>, held: bool, from: NaiveDate, reference: &str) -> Command {
    Command::SetEntitlement(Box::new(ic_core::entitlement::SetEntitlement {
        policy: policy.clone(),
        code: code.to_string(),
        tier: tier.map(str::to_string),
        life: life.map(|l| LifeId(l.to_string())),
        held,
        from,
        until: None,
        reference: reference.to_string(),
    }))
}

/// Purpose: matrix DSC-003, DSC-004, DSC-005 and DSC-009. A wellness tier is awarded to one of
/// two lives by one command carrying a tier code and dates: dated for the next billing period
/// it changes nothing already billed and the next bill carries the discount on that life's
/// cover alone, as its own ledger line; a tier lost from a later period ends the discount
/// there without clawing back what was given; a manual award arrives by the same command and
/// records who awarded it and why; an award dated back inside a billed period hands back the
/// discount for the days from that date in an adjustment posted at that date; and a code no
/// discount of the product is given for, a life not on the policy, a tier on a loss and a
/// caller that is not such a module are each refused.
/// Why it matters: the programme's rules and the activity data live outside the core, so the
/// one command is the whole contract. A tier that reached back when it was meant for next
/// quarter would rebill customers; one that could not reach back when the programme's terms
/// say so would leave them out of pocket; and a person who cannot take part must be able to
/// hold the same discount, reportably, through a manual award.
#[test]
fn a_wellness_tier_is_one_command_and_discounts_that_lifes_cover_from_its_date() {
    let mut c = cluster(1);
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-well\"")
        .replace(
            "multi_cover_discount(min_covers = 2, percent = \"5\"),",
            r#"discounts(chain = [discount("wellness", "Wellness", "discount_wellness", by_tier({"1": "5", "2": "10"}), entitled("wellness"))]),"#,
        );
    let product = load_product("term_life_well.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let mut issue = term_life(7, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    issue.product_code = "term-life-well".to_string();
    issue.lives.push(named_life("life-2", "party-partner", d(1990, 3, 10), Sex::Female, false));
    let mut second = issue.covers[0].clone();
    second.life = Some(LifeId("life-2".to_string()));
    issue.covers.push(second);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let given = accounts::discounts_given("term-life-well", NZD).id;
    let cost = |c: &Cluster| -c.state(0).ledger.natural_balance(&given);
    let full = latest_bill(&c, &id);
    let fee = 6_00;
    let each = (full - fee) / 2;
    let second_period = d(2027, 12, 15);

    // Refused: not a module that awards entitlements, an unknown code, a stranger, a tier on a loss.
    let award = entitlement(&id, "wellness", Some("2"), Some("life-2"), true, second_period, "quarter-2027Q4");
    assert!(c.submit(0, "policy-servicing", award.clone()).is_err());
    let refused = |c: &mut Cluster, command| code(c.submit(0, "wellness", command));
    assert_eq!(
        refused(&mut c, entitlement(&id, "loyalty", None, None, true, second_period, "r")),
        "unknown_entitlement"
    );
    assert_eq!(
        refused(&mut c, entitlement(&id, "wellness", Some("2"), Some("life-9"), true, second_period, "r")),
        "unknown_life"
    );
    assert_eq!(
        refused(&mut c, entitlement(&id, "wellness", Some("2"), Some("life-2"), false, second_period, "r")),
        "invalid_entitlement"
    );

    // Awarded for the next period: nothing billed changes, and the next bill is 10% less on her cover.
    c.submit(0, "wellness", award).unwrap();
    assert_eq!((outstanding(&c, &id), cost(&c)), (full, 0));
    assert!(events(&c)
        .iter()
        .any(|e| matches!(e, Event::EntitlementChanged { tier: Some(t), held: true, .. } if t == "2")));
    pay(&mut c, &id, "r-1", full);
    days(&mut c, 30);
    let discounted = latest_bill(&c, &id);
    let tenth = i128::from(full - discounted);
    assert!((tenth - i128::from(each) / 10).abs() <= 1, "{tenth} of {each}");
    assert_eq!(cost(&c), tenth, "the discount is its own ledger line");

    // Lost from the third period: the third bill is whole again and nothing is clawed back.
    let third_period = d(2028, 1, 15);
    c.submit(
        0,
        "wellness",
        entitlement(&id, "wellness", None, Some("life-2"), false, third_period, "below-threshold"),
    )
    .unwrap();
    assert_eq!(cost(&c), tenth);
    pay(&mut c, &id, "r-2", discounted);
    days(&mut c, 31);
    assert_eq!((latest_bill(&c, &id), cost(&c)), (full, tenth));

    // A manual award, dated back ten days into the period already billed: tier one, 5%, for
    // the 21 days left of the 31, posted at that date.
    let today = c.business_date().unwrap();
    assert_eq!(today, d(2028, 1, 15));
    days(&mut c, 20);
    let back = d(2028, 1, 25);
    let manual = entitlement(&id, "wellness", Some("1"), Some("life-2"), true, back, "manual-award-cannot-take-part");
    c.submit(0, "wellness", manual).unwrap();
    let handed_back = cost(&c) - tenth;
    let expected = i128::from(each) * 5 / 100 * 21 / 31;
    assert!((handed_back - expected).abs() <= 2, "{handed_back} against {expected}");
    let pol = &c.state(0).policies[&id];
    let adjustment = pol
        .receivables
        .iter()
        .find(|r| r.period_start == back)
        .expect("an adjustment at its value date");
    assert_eq!(i128::from(adjustment.total_minor), -handed_back);
    let record = pol.entitlements.last().unwrap();
    assert_eq!(
        (record.source.as_str(), record.reference.as_str(), record.tier.as_str()),
        ("wellness", "manual-award-cannot-take-part", "1")
    );
    assert_eq!(pol.entitlements.len(), 2, "the earlier tier is kept with its dates, not edited away");
    assert_eq!(pol.entitlements[0].until, Some(third_period));
    days(&mut c, 1);
}

fn override_of(policy: &PolicyId, id: &str, percent: &str, until: NaiveDate, second: Option<&str>) -> Command {
    Command::SetDiscountOverride(Box::new(ic_core::discount_override::SetDiscountOverride {
        policy: policy.clone(),
        terms: ic_core::discount_override::DiscountOverride {
            id: id.to_string(),
            percent: ic_money::DecStr::parse(percent).unwrap(),
            covers: vec![],
            life: None,
            reason: "retention".to_string(),
            from: d(2027, 12, 15),
            until,
            approver: "officer-1".to_string(),
            second_approver: second.map(str::to_string),
        },
    }))
}

/// Purpose: matrix DSC-006 and DSC-010. A retention discount is an approved override with a
/// reason code and a date range: within the product's limit for one approver it is given on
/// one approval; above that limit it is refused until a second, different approver is named;
/// above the product's most it is refused whoever approves; and a product that sets no limit
/// allows none. Once given it is its own line on each bill whose period starts inside its
/// dates, it is reported on the event feed with its size, reason and approvers, it is still
/// on the policy after a lapse, one dated today reaches the period already billed, and the one
/// change allowed to it afterwards is an earlier end, which bills the days after it again.
/// Why it matters: a one-off discount is where pricing discipline leaks. The limits make a
/// large one a two-person decision, the dates stop it running for ever, and the record lets
/// the insurer show who was given what and why. A lapse that silently dropped it would
/// rebill a customer more than they were promised when they came back.
#[test]
fn a_retention_discount_needs_the_approvals_its_size_calls_for_and_runs_for_its_dates() {
    let mut c = cluster(1);
    // The shipped product sets no limit, so it allows no overrides.
    let plain = standard(&mut c, 1);
    let end = d(2028, 3, 15);
    assert_eq!(
        code(c.submit(0, "collections", override_of(&plain, "ovr-0", "5", end, None))),
        "overrides_not_offered"
    );

    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-ovr\"")
        .replace(
            "multi_cover_discount(min_covers = 2, percent = \"5\"),",
            "discounts(chain = [], override_single_approver_percent = \"10\", override_max_percent = \"25\"),",
        );
    let product = load_product("term_life_ovr.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let mut issue = term_life(8, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    issue.product_code = "term-life-ovr".to_string();
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let full = latest_bill(&c, &id);
    let premium = full - 6_00;

    let refused = |c: &mut Cluster, command| code(c.submit(0, "collections", command));
    assert_eq!(refused(&mut c, override_of(&id, "ovr-1", "20", end, None)), "second_approver_required");
    assert_eq!(
        refused(&mut c, override_of(&id, "ovr-1", "20", end, Some("officer-1"))),
        "second_approver_required"
    );
    assert_eq!(refused(&mut c, override_of(&id, "ovr-1", "30", end, Some("manager-1"))), "override_too_large");
    assert_eq!(refused(&mut c, override_of(&id, "ovr-1", "0", end, Some("manager-1"))), "override_too_large");
    assert_eq!(
        refused(&mut c, override_of(&id, "ovr-1", "20", d(2027, 12, 15), Some("manager-1"))),
        "invalid_override"
    );
    assert!(
        c.submit(0, "wellness", override_of(&id, "ovr-1", "5", end, None)).is_err(),
        "not a caller that gives overrides"
    );
    assert!(c.state(0).policies[&id].discount_overrides.is_empty());

    // Twenty per cent from the second period to 15 March, on two approvals.
    c.submit(0, "collections", override_of(&id, "ovr-1", "20", end, Some("manager-1"))).unwrap();
    assert!(events(&c).iter().any(|e| matches!(e,
        Event::DiscountOverrideSet { reason, approver, second_approver: Some(second), .. }
            if reason == "retention" && approver == "officer-1" && second == "manager-1")));
    assert_eq!(outstanding(&c, &id), full, "dated for the next period, it changes nothing billed");
    assert_eq!(
        refused(&mut c, override_of(&id, "ovr-1", "20", d(2028, 6, 15), Some("manager-1"))),
        "override_exists"
    );
    assert_eq!(refused(&mut c, override_of(&id, "ovr-1", "15", end, Some("manager-1"))), "override_exists");

    pay(&mut c, &id, "r-1", full);
    days(&mut c, 30);
    let discounted = latest_bill(&c, &id);
    assert!(
        (i128::from(full - discounted) - i128::from(premium) / 5).abs() <= 1,
        "{discounted} against {full}"
    );
    let given = accounts::discounts_given("term-life-ovr", NZD).id;
    assert_eq!(-c.state(0).ledger.natural_balance(&given), i128::from(full - discounted));

    // Unpaid, the policy lapses, and the override is still on it for a reinstatement to find.
    days(&mut c, 95);
    assert_eq!(c.state(0).policies[&id].status, PolicyStatus::Lapsed);
    assert_eq!(c.state(0).policies[&id].discount_overrides.len(), 1, "a lapse does not take the override away");

    // An override dated today reaches the period already billed.
    let today = c.business_date().unwrap();
    assert!(today > end, "by now its own end date has passed");
    let other = {
        let mut issue = term_life(9, today, d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        issue.product_code = "term-life-ovr".to_string();
        let other = issue.policy.clone();
        c.submit(0, "new-business", issue_command(issue)).unwrap();
        other
    };
    let mut short = override_of(&other, "ovr-2", "10", d(2029, 1, 1), None);
    if let Command::SetDiscountOverride(o) = &mut short {
        o.terms.from = today;
    }
    let whole = latest_bill(&c, &other);
    c.submit(0, "collections", short.clone()).unwrap();
    let after = outstanding(&c, &other);
    assert!(after < whole, "dated today, it reaches the period already billed");
    // Ten days on its end date is brought forward to that day: the ten days are kept and
    // the rest of the period already billed is billed again.
    pay(&mut c, &other, "r-o", after);
    days(&mut c, 10);
    let stop = c.business_date().unwrap();
    if let Command::SetDiscountOverride(o) = &mut short {
        o.terms.until = stop;
    }
    c.submit(0, "collections", short).unwrap();
    let billed_again = outstanding(&c, &other);
    assert!(0 < billed_again && billed_again < whole - after, "{billed_again} of {}", whole - after);
    assert_eq!(c.state(0).policies[&other].discount_overrides[0].until, stop);
    days(&mut c, 1);
}

/// Purpose: matrix DSC-008. A discount for covers held across a customer's policies counts
/// the covers on all of the policyholder's own policies in the group, so a second policy
/// earns it for itself at issue and for the first policy from its next bill; a scheme the
/// same party holds as an employer is not counted, another customer's policy is not
/// counted, and when the second policy is cancelled the first loses the discount from its
/// next bill.
/// Why it matters: the discount is sold as "three covers with us". A director whose
/// company scheme counted would get it for cover the company bought, a count that read
/// only the one policy would never give it to the customers it is meant for, and one that
/// never dropped would keep paying for covers no longer held.
#[test]
fn a_discount_for_covers_across_a_customers_policies_counts_theirs_and_not_the_scheme() {
    let mut c = cluster(1);
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-house\"")
        .replace(
            "multi_cover_discount(min_covers = 2, percent = \"5\"),",
            r#"discounts(chain = [discount("household", "Household", "discount_household", percent_off("10"), min_group_covers(3))]),"#,
        );
    let product = load_product("term_life_house.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    let (group, holder) = (GroupId("grp-waititi".to_string()), PartyId("party-hone".to_string()));
    let discounts_on_latest_bill = |c: &mut Cluster, id: &PolicyId| -> Vec<String> {
        c.take_events()
            .into_iter()
            .filter_map(|e| match e.event {
                Event::PremiumDue { policy, discounts, .. } if policy == *id => Some(discounts.into_iter().map(|d| d.code).collect()),
                _ => None,
            })
            .next_back()
            .expect("the policy was billed")
    };
    let policy = |n: u64, trauma: Option<i64>, holder: &PartyId| {
        let mut p = term_life(n, start(), d(1982, 3, 10), Sex::Male, false, 500_000_00, trauma, Frequency::Monthly, None);
        p.product_code = "term-life-house".to_string();
        p.group = group.clone();
        p.policyholder = holder.clone();
        p
    };

    // The company's scheme, held by the same party, and then his own life and trauma cover.
    let mut scheme = group_scheme(60, start(), vec![member("staff-1", d(1985, 1, 1), "staff", 60_000_00)]);
    scheme.group = group.clone();
    scheme.policyholder = holder.clone();
    c.submit(0, "new-business", issue_command(scheme)).unwrap();
    let first = policy(61, Some(100_000_00), &holder);
    let first_id = first.policy.clone();
    c.take_events();
    c.submit(0, "new-business", issue_command(first)).unwrap();
    assert!(
        discounts_on_latest_bill(&mut c, &first_id).is_empty(),
        "two covers of his own; the scheme is the company's"
    );

    // Somebody else's policy in the same group changes nothing for him.
    let other = policy(62, None, &PartyId("party-other".to_string()));
    let other_id = other.policy.clone();
    c.submit(0, "new-business", issue_command(other)).unwrap();
    assert!(discounts_on_latest_bill(&mut c, &other_id).is_empty());

    // A second policy of his own makes three covers: it has the discount from its first bill.
    let second = policy(63, None, &holder);
    let second_id = second.policy.clone();
    c.submit(0, "new-business", issue_command(second)).unwrap();
    assert_eq!(discounts_on_latest_bill(&mut c, &second_id), vec!["household".to_string()]);
    assert_eq!(c.state(0).policies[&second_id].covers_elsewhere, 2);

    // The first policy has it from its next bill, and the other customer still has none.
    let undiscounted = latest_bill(&c, &first_id);
    days(&mut c, 30);
    let seen: Vec<Event> = c.take_events().into_iter().map(|r| r.event).collect();
    let bill = |id: &PolicyId| {
        seen.iter()
            .rev()
            .find_map(|e| match e {
                Event::PremiumDue { policy, discounts, .. } if policy == id => Some(discounts.len()),
                _ => None,
            })
            .expect("December was billed")
    };
    assert_eq!((bill(&first_id), bill(&second_id), bill(&other_id)), (1, 1, 0));
    assert!(latest_bill(&c, &first_id) < undiscounted);

    // He cancels the second policy: the first is back to two covers from its next bill.
    let cancel = Command::CancelPolicy { policy: second_id.clone() };
    c.submit(0, "policy-servicing", cancel).unwrap();
    c.take_events();
    days(&mut c, 31);
    assert!(discounts_on_latest_bill(&mut c, &first_id).is_empty());
    assert_eq!(latest_bill(&c, &first_id), undiscounted);
}
