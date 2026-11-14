//! Loading a cohort of policies written on another system: opening balances
//! against the cohort's clearing account, control totals, sign-off, release
//! and abandonment.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::command::CoverSpec;
use ic_core::event::Event;
use ic_core::hardship::HardshipOverride;
use ic_core::migration::{CohortStatus, ControlTotals, Load, MigrationCommand, Opening, OpeningBill, SignOff};
use ic_core::{Command, LifeId, PolicyId, PolicyStatus, Reject, Standing};
use ic_money::{Currency::NZD, Money};
use ic_product::{load_definitions, load_product, Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::term_life;
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// The cutover day.
fn cutover() -> NaiveDate {
    d(2027, 11, 15)
}

/// A partition open on the cutover day, with the product generation the old
/// system sold from 1998 and the wording it was sold on loaded as reference
/// data beside today's.
fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.open(cutover()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let wording = r#"DEFINITIONS = [
    definition("death-1998", 1, "1998-01-01", "Death", "Death of the life assured, as worded in the 1998 policy document."),
    definition("terminal-illness-1998", 1, "1998-01-01", "Terminal illness", "Terminal illness, as worded in the 1998 policy document."),
]"#;
    let definitions = load_definitions("definitions/legacy_1998.star", wording).unwrap();
    c.activate_everywhere(Command::ActivateDefinitions { definitions }).unwrap();
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-1998\"")
        .replace("effective_from = \"2027-01-01\"", "effective_from = \"1998-01-01\"")
        .replace(
            "definitions = [\"death\", \"terminal-illness\"]",
            "definitions = [\"death-1998\", \"terminal-illness-1998\"]",
        );
    let product = load_product("term_life_1998.star", &source).unwrap();
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(product) }).unwrap();
    c
}

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

fn step(c: &mut Cluster, command: MigrationCommand) -> Result<(), Reject> {
    c.submit(0, "migration", Command::Migration(Box::new(command)))
}

fn open(c: &mut Cluster, cohort: &str, control: ControlTotals) -> Result<(), Reject> {
    step(
        c,
        MigrationCommand::OpenCohort {
            cohort: cohort.into(),
            currency: NZD,
            cutover: cutover(),
            control,
        },
    )
}

fn signed() -> SignOff {
    SignOff {
        finance: "fin-ana".into(),
        business_owner: "life-ben".into(),
        actuary: Some("act-cy".into()),
        evidence: "sha256:reconciliation-pack".into(),
    }
}

fn bill(from: NaiveDate, to: NaiveDate, due: NaiveDate, life_net: i64, fee: i64, outstanding: i64) -> OpeningBill {
    OpeningBill {
        period_start: from,
        period_end: to,
        due,
        net: [("life".to_string(), life_net), ("policy_fee".to_string(), fee)].into(),
        gst_minor: 0,
        levies: BTreeMap::new(),
        outstanding_minor: outstanding,
    }
}

/// The four policies of the cohort, as the old system's extract gives them.
///
/// - 1: monthly from 15 Mar 2019, October's bill unpaid and ten days into grace.
/// - 2: yearly from 1 Jun 2020, paid to June 2028, with NZD 50 held and only
///   today's sum insured known.
/// - 3: monthly from 1 Feb 2021 with an increase on 1 Feb 2024 and a premium
///   holiday granted in 2021, paid up.
/// - 4: monthly from 20 Aug 2022, September's bill unpaid since 20 Sep.
fn loads(cohort: &str) -> Vec<Load> {
    let legacy = |n: u64, inception: NaiveDate, frequency: Frequency| {
        let mut issue = term_life(n, inception, d(1980, 5, 5), Sex::Female, false, 300_000_00, None, frequency, None);
        issue.product_code = "term-life-1998".into();
        issue.migrated = true;
        issue
    };
    let single = vec!["single_layer".to_string()];
    let mut layered = legacy(3, d(2021, 2, 1), Frequency::Monthly);
    layered.covers.push(CoverSpec {
        excess_option: None,
        code: "life".into(),
        life: Some(LifeId("life-1".into())),
        sum_insured_minor: 100_000_00,
        loadings: vec![],
        exclusions: vec![],
    });
    vec![
        Load {
            cohort: cohort.into(),
            issue: legacy(1, d(2019, 3, 15), Frequency::Monthly),
            opening: Opening {
                periods_billed: 104,
                bills: vec![bill(d(2027, 10, 15), d(2027, 11, 15), d(2027, 11, 5), 54_00, 6_00, 60_00)],
                assumptions: single.clone(),
                ..Opening::default()
            },
        },
        Load {
            cohort: cohort.into(),
            issue: legacy(2, d(2020, 6, 1), Frequency::Yearly),
            opening: Opening {
                periods_billed: 8,
                bills: vec![bill(d(2027, 6, 1), d(2028, 6, 1), d(2027, 6, 1), 660_00, 72_00, 0)],
                suspense_minor: 50_00,
                assumptions: single.clone(),
                ..Opening::default()
            },
        },
        Load {
            cohort: cohort.into(),
            issue: layered,
            opening: Opening {
                periods_billed: 82,
                bills: vec![bill(d(2027, 11, 1), d(2027, 12, 1), d(2027, 11, 1), 70_00, 6_00, 0)],
                indexation_declined: true,
                hardship: Some(HardshipOverride {
                    id: "legacy-2021-17".into(),
                    from: d(2021, 6, 1),
                    to: d(2021, 9, 1),
                    share_bps: 0,
                    planned_to: None,
                }),
                layer_starts: vec![d(2021, 2, 1), d(2024, 2, 1)],
                ..Opening::default()
            },
        },
        Load {
            cohort: cohort.into(),
            issue: legacy(4, d(2022, 8, 20), Frequency::Monthly),
            opening: Opening {
                periods_billed: 63,
                bills: vec![
                    bill(d(2027, 9, 20), d(2027, 10, 20), d(2027, 9, 20), 54_00, 6_00, 60_00),
                    bill(d(2027, 10, 20), d(2027, 11, 20), d(2027, 10, 20), 54_00, 6_00, 60_00),
                ],
                assumptions: single,
                ..Opening::default()
            },
        },
    ]
}

fn load_all(c: &mut Cluster, cohort: &str) {
    for load in loads(cohort) {
        step(c, MigrationCommand::Load(Box::new(load))).unwrap();
    }
}

fn pol(n: u64) -> PolicyId {
    PolicyId(format!("pol-{n:08}"))
}

fn balance(c: &Cluster, account: ic_ledger::Account) -> i128 {
    c.state(0).ledger.balance(&account.id)
}

/// Purpose: matrix MIG-001, MIG-004, MIG-005, MIG-006, MIG-008 and MIG-015. A cohort of four
/// policies from another system is loaded on its cutover day through the migration door: each
/// keeps its original inception, its wording by hash, its layers (with the assumption recorded
/// where the old system kept only today's amount), an old premium holiday with its original
/// dates, and its bills with their original due dates, so standing is what the old system
/// had and nothing is billed or announced again. Opening balances sit on the cohort's
/// clearing account; a sign-off that does not agree with the control totals is refused naming
/// each difference, the load is taken out again leaving nothing, and a second load that does
/// agree is signed off, leaving the clearing account at zero and the control account holding
/// what came across. Until the cohort is released its policies take no other command and the
/// business day does not advance.
/// Why it matters: migration is the one time a whole book's money is written without a
/// customer transaction behind it. If the load could be wrong by a cent and still be signed,
/// re-bill a period the customer has paid, lose a due date and with it the notice history, or
/// be half taken out, the error would be in every later statement for those customers.
#[test]
fn a_cohort_is_loaded_reconciled_signed_off_and_released() {
    let mut c = cluster();
    c.take_events();

    // Only the migration load opens a cohort, and only on its cutover day.
    let wrong_day = MigrationCommand::OpenCohort {
        cohort: "retail-1".into(),
        currency: NZD,
        cutover: d(2027, 11, 16),
        control: ControlTotals::default(),
    };
    assert_eq!(code(step(&mut c, wrong_day)), "cutover_date");
    let open_it = MigrationCommand::OpenCohort {
        cohort: "retail-1".into(),
        currency: NZD,
        cutover: cutover(),
        control: ControlTotals::default(),
    };
    assert_eq!(code(c.submit(0, "new-business", Command::Migration(Box::new(open_it)))), "caller_not_allowed");

    // The first attempt: the old system's totals as extracted, one policy short.
    let guess = ControlTotals {
        policies: 3,
        ..ControlTotals::default()
    };
    open(&mut c, "retail-1", guess).unwrap();
    load_all(&mut c, "retail-1");
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    let refused = step(
        &mut c,
        MigrationCommand::SignOff {
            cohort: "retail-1".into(),
            sign_off: signed(),
        },
    )
    .unwrap_err();
    assert_eq!(refused.code, "control_totals");
    assert!(refused.message.contains("policies: loaded 4, control 3"), "{}", refused.message);
    assert!(refused.message.contains("receivable_minor: loaded 18000, control 0"), "{}", refused.message);

    // What was loaded, worked out here from the extract. Unearned: policy 2 has 199 of 366
    // days of its year left, policy 3 has 16 of 30 days, policy 4 has 5 of 31.
    let loaded = c.state(0).migration.cohorts["retail-1"].loaded;
    let unearned = |net: i64, left: i64, days: i64| -> i64 {
        let twice = 2 * net * left;
        let (q, r) = (twice / (2 * days), twice % (2 * days));
        q + i64::from(r >= days)
    };
    let expected_unearned = unearned(660_00, 199, 366)
        + unearned(72_00, 199, 366)
        + unearned(70_00, 16, 30)
        + unearned(6_00, 16, 30)
        + unearned(54_00, 5, 31)
        + unearned(6_00, 5, 31);
    assert_eq!(
        (loaded.policies, loaded.lives, loaded.layers, loaded.sum_insured_minor),
        (4, 4, 5, 1_300_000_00)
    );
    assert_eq!((loaded.receivable_minor, loaded.suspense_minor), (180_00, 50_00));
    assert!(
        (loaded.unearned_minor - expected_unearned).abs() <= 1,
        "{} against {expected_unearned}",
        loaded.unearned_minor
    );
    assert!(loaded.period_premium_minor > 0);
    let clearing = accounts::migration_clearing("retail-1", NZD);
    assert_eq!(balance(&c, clearing.clone()), (loaded.unearned_minor + 50_00 - 180_00) as i128);

    // Held: nothing else reaches the policies and the day does not move on.
    let receipt = Command::ApplyReceipt {
        policy: pol(1),
        receipt: "r-1".into(),
        amount: Money::new(60_00, NZD),
    };
    assert_eq!(code(c.submit(0, "cash-matching", receipt.clone())), "cohort_loading");
    let tomorrow = Command::AdvanceBusinessDate { to: d(2027, 11, 16) };
    assert_eq!(code(c.submit(0, "leader", tomorrow.clone())), "cohort_open");
    assert_eq!(
        code(step(
            &mut c,
            MigrationCommand::Release {
                cohort: "retail-1".into(),
                limit: 10
            }
        )),
        "cohort_closed"
    );

    // Abandoned in two steps: every balance goes back and the policies are gone.
    let abandon = |limit| MigrationCommand::Abandon {
        cohort: "retail-1".into(),
        limit,
    };
    assert_eq!(code(step(&mut c, abandon(0))), "apply_budget");
    step(&mut c, abandon(3)).unwrap();
    assert_eq!(c.state(0).migration.cohorts["retail-1"].status, CohortStatus::Abandoning);
    assert_eq!(
        code(step(&mut c, MigrationCommand::Load(Box::new(loads("retail-1").remove(0))))),
        "cohort_closed"
    );
    step(&mut c, abandon(3)).unwrap();
    step(&mut c, abandon(3)).unwrap();
    assert_eq!(c.state(0).migration.cohorts["retail-1"].status, CohortStatus::Abandoned);
    assert!(c.state(0).policies.is_empty());
    assert_eq!(balance(&c, clearing), 0);
    for n in 1..=4 {
        for account in accounts::policy_accounts(&pol(n), NZD) {
            assert_eq!(balance(&c, account), 0);
        }
    }
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    let announced: Vec<_> = c.take_events().into_iter().map(|e| e.event).collect();
    assert!(
        announced
            .iter()
            .all(|e| matches!(e, Event::CohortOpened { .. } | Event::CohortAbandoned { .. } | Event::EntryPosted { .. })),
        "beyond its ledger entries, nothing about a policy was announced: {announced:?}"
    );
    // Finance sees each opening entry and its taking out, which cancel line for line.
    let mut by_line: BTreeMap<String, i128> = BTreeMap::new();
    for e in &announced {
        if let Event::EntryPosted { lines, .. } = e {
            for l in lines {
                *by_line
                    .entry(format!("{}/{:?}/{:?}", l.role, l.dims.cover_type, l.dims.posting_type))
                    .or_insert(0) += l.signed;
            }
        }
    }
    assert!(by_line.values().all(|v| *v == 0), "{by_line:?}");

    // The second attempt, with the totals the extract should have given.
    open(&mut c, "retail-2", loaded).unwrap();
    open(&mut c, "retail-2", loaded).unwrap();
    assert_eq!(code(open(&mut c, "retail-2", guess)), "cohort_exists");
    load_all(&mut c, "retail-2");
    assert_eq!(c.state(0).migration.cohorts["retail-2"].loaded, loaded);

    let p1 = &c.state(0).policies[&pol(1)];
    assert!(p1.migrated);
    assert_eq!((p1.inception, p1.periods_billed, p1.anniversaries_processed), (d(2019, 3, 15), 104, 8));
    assert_eq!(p1.standing, Some(Standing::InGrace), "ten days past the old system's due date");
    assert_eq!(p1.oldest_unpaid_due(), Some(d(2027, 11, 5)));
    assert_eq!(p1.covers["life"].definitions[0].id, "death-1998", "bound to the wording it was sold on");
    assert_eq!(p1.migration.as_ref().unwrap().assumptions, vec!["single_layer".to_string()]);
    let p3 = &c.state(0).policies[&pol(3)];
    let layers: Vec<_> = p3.covers["life"].layers.iter().map(|l| (l.start, l.sum_insured_minor)).collect();
    assert_eq!(layers, vec![(d(2021, 2, 1), 300_000_00), (d(2024, 2, 1), 100_000_00)]);
    assert!(p3.indexation_declined);
    assert_eq!(p3.hardship.as_ref().map(|h| (h.from, h.to)), Some((d(2021, 6, 1), d(2021, 9, 1))));
    assert_eq!(c.state(0).policies[&pol(4)].standing, Some(Standing::Suspended));

    // Sign-off needs different people and named evidence.
    let alone = SignOff {
        business_owner: "fin-ana".into(),
        ..signed()
    };
    let sign = |sign_off| MigrationCommand::SignOff {
        cohort: "retail-2".into(),
        sign_off,
    };
    assert_eq!(code(step(&mut c, sign(alone))), "invalid_sign_off");
    step(&mut c, sign(signed())).unwrap();
    step(&mut c, sign(signed())).unwrap();
    assert_eq!(balance(&c, accounts::migration_clearing("retail-2", NZD)), 0);
    assert_eq!(
        balance(&c, accounts::policy_migration_control(NZD)),
        (loaded.unearned_minor + 50_00 - 180_00) as i128
    );
    assert_eq!(
        code(step(
            &mut c,
            MigrationCommand::Abandon {
                cohort: "retail-2".into(),
                limit: 10
            }
        )),
        "cohort_closed"
    );
    assert_eq!(code(c.submit(0, "leader", tomorrow.clone())), "cohort_open");

    // Released in two steps: each policy is announced once, as migrated, with no bill.
    let release = |limit| MigrationCommand::Release {
        cohort: "retail-2".into(),
        limit,
    };
    step(&mut c, release(3)).unwrap();
    assert_eq!(code(c.submit(0, "cash-matching", receipt.clone())), "accepted", "policy 1 is released");
    assert_eq!(c.state(0).migration.cohorts["retail-2"].status, CohortStatus::SignedOff);
    step(&mut c, release(3)).unwrap();
    step(&mut c, release(3)).unwrap();
    assert_eq!(c.state(0).migration.cohorts["retail-2"].status, CohortStatus::Released);
    let events: Vec<_> = c.take_events().into_iter().map(|e| e.event).collect();
    let count = |f: fn(&Event) -> bool| events.iter().filter(|e| f(e)).count();
    assert_eq!(count(|e| matches!(e, Event::PolicyIssued { .. })), 4);
    assert_eq!(count(|e| matches!(e, Event::PolicyMigrated { .. })), 4);
    assert_eq!(count(|e| matches!(e, Event::CohortSignedOff { .. })), 1);
    assert_eq!(count(|e| matches!(e, Event::CohortReleased { .. })), 1);
    assert_eq!(
        count(|e| matches!(e, Event::PremiumDue { .. })),
        0,
        "nothing the old system billed is billed again"
    );
    assert!(events.iter().any(|e| matches!(e, Event::PolicyMigrated { policy, standing: Some(Standing::Suspended), oldest_unpaid_due: Some(due), .. } if *policy == pol(4) && *due == d(2027, 9, 20))));
    assert_eq!(
        c.state(0).policies[&pol(1)].standing,
        Some(Standing::Paid),
        "the receipt cleared October's bill"
    );
}

/// Purpose: matrix MIG-004 and MIG-008. After release the cohort runs like any other business:
/// the first end of day bills the period that falls due on the cutover day and no earlier one,
/// premium the old system billed goes on earning by the day to exactly nothing at the end of its
/// period, a policy whose bill was already overdue lapses on the day its original due date
/// implies, and every day's reconciliation is clean.
/// Why it matters: the opening position has to be one the ordinary machinery can carry on
/// from. A cutover that reset the lapse clock would give months of free cover; one that left
/// unearned premium that never earns out would overstate the liability for good.
#[test]
fn a_released_cohort_carries_on_from_where_the_old_system_stopped() {
    let mut c = cluster();
    open(&mut c, "retail-1", ControlTotals::default()).unwrap();
    load_all(&mut c, "retail-1");
    let loaded = c.state(0).migration.cohorts["retail-1"].loaded;
    step(
        &mut c,
        MigrationCommand::Abandon {
            cohort: "retail-1".into(),
            limit: 10,
        },
    )
    .unwrap();
    open(&mut c, "retail-2", loaded).unwrap();
    load_all(&mut c, "retail-2");
    step(
        &mut c,
        MigrationCommand::SignOff {
            cohort: "retail-2".into(),
            sign_off: SignOff { actuary: None, ..signed() },
        },
    )
    .unwrap();
    step(
        &mut c,
        MigrationCommand::Release {
            cohort: "retail-2".into(),
            limit: 10,
        },
    )
    .unwrap();
    // Policy 2 pays nothing more this year; policies 1 and 3 have money to pay from.
    for n in [1, 3] {
        let receipt = Command::ApplyReceipt {
            policy: pol(n),
            receipt: format!("r-{n}"),
            amount: Money::new(2_000_00, NZD),
        };
        c.submit(0, "cash-matching", receipt).unwrap();
    }
    c.take_events();

    let mut bills: Vec<(PolicyId, NaiveDate)> = Vec::new();
    let mut lapsed_on = None;
    while c.business_date().unwrap() < d(2028, 6, 3) {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
        for e in c.take_events() {
            match e.event {
                Event::PremiumDue { policy, period_start, .. } => bills.push((policy, period_start)),
                Event::PolicyStatusChanged {
                    policy,
                    to: PolicyStatus::Lapsed,
                    ..
                } if policy == pol(4) => lapsed_on = Some(e.business_date),
                _ => {}
            }
        }
    }
    let first = |n: u64| bills.iter().find(|(p, _)| *p == pol(n)).map(|(_, start)| *start);
    assert_eq!(first(1), Some(d(2027, 11, 15)), "the period due on the cutover day is this system's to bill");
    assert_eq!(first(3), Some(d(2027, 12, 1)));
    assert_eq!(first(2), Some(d(2028, 6, 1)), "the year the old system billed is not billed again");
    // Ninety days from 20 September 2027, the old system's due date.
    assert_eq!(lapsed_on, Some(d(2027, 12, 20)));
    let p2 = &c.state(0).policies[&pol(2)];
    let old = &p2.receivables[0];
    assert_eq!((old.period_end, old.unearned_fine()), (d(2028, 6, 1), 0));
    assert_eq!(balance(&c, accounts::policy_suspense(&pol(2), NZD)), 0, "money held went to the next bill");
}

/// Purpose: each rule on a load refuses the policy that breaks it, leaving nothing behind: an
/// unknown cohort, an issue command not marked as migrated, a policy already here, layers at
/// inception with no assumption recorded, layer dates that do not match the covers or fall
/// outside the policy's life, a period the old system should have billed and did not, a bill
/// for a period it had not reached, more owing than was billed, a spent bill, a hardship
/// override with no dates, an assumption that is not a code, negative money held, a cohort
/// opened for another day, and a policy in another currency than its cohort's. A policy the
/// old system let fall overdue to lapse comes across suspended, and one written on the cutover
/// day has passed no anniversary.
/// Why it matters: the extract is written by mapping code from another system's data, and its
/// faults are found here or not at all. Each refusal is an exception for a person to clear
/// before the cohort moves; a load that accepted any of them would bake the fault into the
/// policy's opening position.
#[test]
fn a_load_that_breaks_a_rule_is_refused_whole() {
    let mut c = cluster();
    let first = || loads("retail-1").remove(0);
    let try_load = |c: &mut Cluster, load: Load| code(step(c, MigrationCommand::Load(Box::new(load))));
    assert_eq!(try_load(&mut c, first()), "unknown_cohort");
    assert_eq!(code(open(&mut c, "not a name", ControlTotals::default())), "invalid_cohort");
    let negative = ControlTotals {
        receivable_minor: -1,
        ..ControlTotals::default()
    };
    assert_eq!(code(open(&mut c, "retail-1", negative)), "invalid_cohort");
    open(&mut c, "retail-1", ControlTotals::default()).unwrap();

    let mut load = first();
    load.issue.migrated = false;
    assert_eq!(try_load(&mut c, load), "migrated");
    let mut load = first();
    load.opening.assumptions.clear();
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.assumptions.push("see the note on file".into());
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.layer_starts = vec![d(2019, 3, 15), d(2020, 3, 15)];
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.layer_starts = vec![d(2018, 3, 15)];
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.periods_billed = 103;
    assert_eq!(try_load(&mut c, load), "opening_billing");
    let mut load = first();
    load.opening.bills[0].period_end = d(2027, 12, 15);
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.bills[0].outstanding_minor = 60_01;
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.bills[0].outstanding_minor = 0;
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.hardship = Some(HardshipOverride {
        id: "h".into(),
        from: d(2021, 1, 1),
        to: d(2021, 1, 1),
        share_bps: 0,
        planned_to: None,
    });
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.opening.suspense_minor = -1;
    assert_eq!(try_load(&mut c, load), "invalid_opening");
    let mut load = first();
    load.issue.product_code = "no-such-product".into();
    assert_eq!(try_load(&mut c, load), "unknown_product");

    // A cohort is opened only on its cutover day, and one in another currency takes no
    // policy written in this one.
    let tomorrow = MigrationCommand::OpenCohort {
        cohort: "tomorrow".into(),
        currency: NZD,
        cutover: cutover().succ_opt().unwrap(),
        control: ControlTotals::default(),
    };
    assert_eq!(code(step(&mut c, tomorrow)), "cutover_date");
    step(
        &mut c,
        MigrationCommand::OpenCohort {
            cohort: "australia".into(),
            currency: ic_money::Currency::AUD,
            cutover: cutover(),
            control: ControlTotals::default(),
        },
    )
    .unwrap();
    let mut load = first();
    load.cohort = "australia".into();
    assert_eq!(try_load(&mut c, load), "currency");

    assert!(c.state(0).policies.is_empty());
    assert_eq!(c.state(0).migration.cohorts["retail-1"].loaded, ControlTotals::default());
    assert_eq!(try_load(&mut c, first()), "accepted");
    assert_eq!(try_load(&mut c, first()), "policy_exists");
    // A policy the old system had let fall overdue to lapse comes across suspended, and
    // the first end of day here lapses it with its notice.
    let mut overdue = loads("retail-1").remove(3);
    overdue.opening.bills = (6..=10)
        .map(|m| bill(d(2027, m, 20), d(2027, m + 1, 20), d(2027, m, 20), 54_00, 6_00, 60_00))
        .collect();
    assert_eq!(try_load(&mut c, overdue), "accepted");
    assert_eq!(c.state(0).policies[&pol(4)].standing, Some(Standing::Suspended));
    // One the old system wrote on the cutover day itself has passed no anniversary.
    let mut fresh = loads("retail-1").remove(0);
    fresh.issue = {
        let mut issue = term_life(9, cutover(), d(1980, 5, 5), Sex::Female, false, 300_000_00, None, Frequency::Monthly, None);
        issue.product_code = "term-life-1998".into();
        issue.migrated = true;
        issue
    };
    fresh.opening = Opening {
        periods_billed: 1,
        bills: vec![bill(cutover(), d(2027, 12, 15), cutover(), 54_00, 6_00, 60_00)],
        ..Opening::default()
    };
    assert_eq!(try_load(&mut c, fresh), "accepted");
    assert_eq!(c.state(0).policies[&pol(9)].anniversaries_processed, 0);
    assert_eq!(
        code(step(
            &mut c,
            MigrationCommand::Release {
                cohort: "retail-1".into(),
                limit: 500
            }
        )),
        "apply_budget"
    );
    assert_eq!(
        code(step(
            &mut c,
            MigrationCommand::SignOff {
                cohort: "nowhere".into(),
                sign_off: signed()
            }
        )),
        "unknown_cohort"
    );
    // An empty cohort signs off and is released at once.
    open(&mut c, "empty", ControlTotals::default()).unwrap();
    step(
        &mut c,
        MigrationCommand::SignOff {
            cohort: "empty".into(),
            sign_off: signed(),
        },
    )
    .unwrap();
    assert_eq!(c.state(0).migration.cohorts["empty"].status, CohortStatus::Released);
    step(
        &mut c,
        MigrationCommand::Release {
            cohort: "empty".into(),
            limit: 1,
        },
    )
    .unwrap();
}
