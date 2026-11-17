//! What the option a life chose on a product sets for a claim: the longest
//! benefit period and the age it stops at, a limit in all for mental health,
//! claim indexation, benefits built into the cover, and a cap on everything
//! one event pays.

use ic_claims::command::Periodic;
use ic_claims::model::EndReason;
use ic_claims::port::{BuiltIn, CoverFacts};
use ic_claims::{ClaimType, Command, Instruction, Request, Status};

use crate::world::*;

fn income(monthly_minor: i64, first_period: chrono::NaiveDate, end: chrono::NaiveDate) -> Request {
    Request::Admit {
        amount_minor: None,
        periodic: Some(Periodic {
            monthly_minor,
            first_period,
            end,
            expected_end: end,
        }),
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: false,
    }
}

fn ip_cover(w: &mut World, facts_for: CoverFacts) {
    w.core.cover("pol-2", "ip", facts_for);
}

fn ip_facts() -> CoverFacts {
    CoverFacts {
        sum_insured_minor: 4_000,
        ..facts("income_protection")
    }
}

/// An income claim on pol-2, notified, owned by ana, with the cause and cover check recorded.
fn ip_claim(w: &mut World, id: &str, cause: &str) {
    w.ok(Command::Notify(Box::new(World::notification(id, "pol-2", "ip", ClaimType::IncomeProtection))));
    w.ok(Command::Acknowledge {
        claim: id.into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign(id, "ana");
    w.assess(id, cause);
}

/// Purpose: an income benefit cannot be admitted to run past the benefit period of the option the
/// life chose, nor past the day the option's age limit ends it; inside both it is admitted.
/// Why it matters: a two-year option priced as two years and paid to 65 would pay decades of
/// benefit the premium never bought, and the schedule set at admission is what every later
/// payment run follows. The refusal names the limit and the last allowed date (found by the
/// scenario run, SCN2-09, F2-09-3: it said only "the period or the age").
#[test]
fn an_income_benefit_stops_at_the_options_period_and_age() {
    let mut w = World::new();
    ip_cover(
        &mut w,
        CoverFacts {
            benefit_months: Some(24),
            benefit_to: Some(d(2028, 9, 1)),
            ..ip_facts()
        },
    );
    ip_claim(&mut w, "ip-1", "CC-0001");
    let first = d(2027, 3, 1);
    assert_eq!(w.undecided("ip-1", "ana", income(3_000, first, d(2029, 3, 2)), "R-OK"), "beyond_benefit_period");
    // Inside two years, but past the age the option pays to.
    assert_eq!(w.undecided("ip-1", "ana", income(3_000, first, d(2028, 9, 2)), "R-OK"), "beyond_benefit_period");
    let told = w.told(World::decide_command("ip-1", "ana", income(3_000, first, d(2029, 3, 2)), "R-OK"));
    assert_eq!(told, "the benefit runs past the age the option pays to: the latest it can end is 2028-09-01");
    w.decided("ip-1", "ana", income(3_000, first, d(2028, 9, 1)), "R-OK");
    assert_eq!(w.claim("ip-1").status, Status::InPayment);
}

/// Purpose: where the option limits mental health claims to two years in all, an income benefit
/// for a mental health cause may run for two years less what earlier mental health claims on the
/// same cover and life were paid; a claim for another cause is not limited by it.
/// Why it matters: the AIA income options limit mental health claims to two years. Counting each
/// claim on its own would let a recurring condition be paid for far longer than the wording
/// allows, while applying it to a back injury would refuse cover the life paid for.
#[test]
fn a_mental_health_limit_counts_every_claim_in_all() {
    let mut w = World::new();
    ip_cover(
        &mut w,
        CoverFacts {
            mental_health_months: Some(24),
            ..ip_facts()
        },
    );
    // Two years exactly is within the limit; two runs paid, then the life recovers.
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    w.decided(
        "ip-1",
        "ana",
        Request::EndBenefit {
            end: d(2027, 5, 1),
            reason: EndReason::Recovered,
        },
        "R-OK",
    );

    // A relapse: March and April (61 days) were paid already.
    ip_claim(&mut w, "ip-2", "CC-0002");
    let first = d(2027, 6, 1);
    assert_eq!(w.undecided("ip-2", "ana", income(3_000, first, d(2029, 4, 2)), "R-OK"), "mental_health_limit");
    w.decided("ip-2", "ana", income(3_000, first, d(2029, 4, 1)), "R-OK");

    // Not a mental health cause: the limit does not apply.
    ip_claim(&mut w, "ip-3", "CC-0001");
    w.decided("ip-3", "ana", income(3_000, first, d(2030, 6, 1)), "R-OK");
}

/// Purpose: where the cover carries claim indexation, a benefit in payment rises by the price
/// index in force at each anniversary of its first period, compounding; a period from the
/// anniversary paid before the rise is owed the difference with the next payment; without the
/// feature the benefit stays as admitted.
/// Why it matters: the AIA claim indexation option is priced to keep a long claim's benefit in
/// step with prices. Missing a step underpays every month after it, and a rise computed on the
/// first benefit instead of the last would underpay from the second year.
#[test]
fn a_benefit_in_payment_rises_by_the_price_index_at_each_claim_anniversary() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.claim_index_bp = 300;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    ip_cover(
        &mut w,
        CoverFacts {
            claim_indexation: true,
            ..ip_facts()
        },
    );
    ip_claim(&mut w, "ip-1", "CC-0001");
    w.decided("ip-1", "ana", income(3_000, d(2027, 3, 1), d(2030, 3, 1)), "R-OK");
    w.verify_payee("ip-1", "ana");

    // The first anniversary's period is paid before the day's work raises it.
    w.on(d(2028, 3, 1));
    w.pay_run("run-1", d(2028, 3, 1));
    w.ok(Command::DrainDue);
    let s = w.claim("ip-1").schedule.clone().expect("in payment");
    assert_eq!((s.monthly_on(d(2028, 3, 1)), s.monthly_on(d(2028, 2, 1)), s.arrears_minor), (3_090, 3_000, 90));
    let raised = w.core.noticed("benefit_indexed");
    assert_eq!((raised[0]["monthly_minor"].as_str(), raised[0]["index_bp"].as_str()), ("3090", "300"));

    // The second compounds on the first; the end of the benefit has no third.
    w.on(d(2029, 3, 1));
    w.ok(Command::DrainDue);
    let c = w.claim("ip-1");
    assert_eq!(c.schedule.as_ref().expect("in payment").monthly_on(d(2029, 3, 1)), 3_182);
    assert!(!c.due.iter().any(|(_, due)| *due == ic_claims::model::Due::Indexation));
    assert_eq!(w.state.book(d(2029, 3, 1)).monthly_minor, 3_182);

    // A cover without the feature keeps the benefit as admitted.
    w.on(d(2027, 3, 1));
    ip_cover(&mut w, ip_facts());
    ip_claim(&mut w, "ip-2", "CC-0001");
    w.decided("ip-2", "ana", income(3_000, d(2027, 3, 1), d(2030, 3, 1)), "R-OK");
    w.on(d(2028, 3, 2));
    w.ok(Command::DrainDue);
    assert_eq!(w.claim("ip-2").schedule.as_ref().expect("in payment").monthly_on(d(2028, 3, 1)), 3_000);
}

fn built_in(code: &str, kind: &str, limit_minor: Option<i64>, applies: bool) -> BuiltIn {
    BuiltIn {
        code: code.into(),
        kind: kind.into(),
        limit_minor,
        applies,
    }
}

fn admit_built_in(code: &str, amount_minor: i64, settles: bool) -> Request {
    Request::AdmitBuiltIn {
        code: code.into(),
        amount_minor,
        settles,
    }
}

/// Purpose: a benefit built into the cover is admitted only where the cover has it and its
/// feature was chosen, and only up to its own limit across the life's claims; an early payment
/// comes off the cover and out of the main benefit, while other kinds are paid on top; a
/// built-in benefit that is the whole claim is admitted and closes when paid.
/// Why it matters: the AIA products build counselling, early payment and similar benefits into
/// their covers. Paying a premier-only benefit to a life without premier, or an early payment
/// on top of the full sum insured, would pay money the product never promised.
#[test]
fn a_built_in_benefit_is_paid_within_its_limit_and_only_where_chosen() {
    let mut w = World::new();
    let benefits = vec![
        built_in("terminal-early", "early_payment", Some(100_000), true),
        built_in("counselling", "reimbursement", Some(5_000), true),
        built_in("premier-nurse", "facility", None, false),
    ];
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            built_in: benefits.clone(),
            ..facts("trauma")
        },
    );
    w.open_claim("t-1");
    w.assess("t-1", "CC-0003");
    assert_eq!(w.undecided("t-1", "ana", admit_built_in("unknown", 1_000, false), "R-OK"), "unknown_benefit");
    assert_eq!(
        w.undecided("t-1", "ana", admit_built_in("premier-nurse", 1_000, false), "R-OK"),
        "built_in_not_chosen"
    );
    assert_eq!(w.undecided("t-1", "ana", admit_built_in("counselling", 5_001, false), "R-OK"), "built_in_limit");
    assert_eq!(w.undecided("t-1", "ana", admit_built_in("counselling", 0, false), "R-OK"), "built_in_limit");

    w.decided("t-1", "ana", admit_built_in("counselling", 3_000, false), "R-OK");
    let c = w.claim("t-1");
    assert_eq!((c.status, c.admitted_minor, c.built_in["counselling"]), (Status::Open, 3_000, 3_000));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Admit {
            amount_minor: 3_000,
            life: None,
            release_reserve: false,
            ..
        })
    ));
    assert_eq!(w.core.noticed("built_in_admitted")[0]["code"], "counselling");

    // The early payment comes off the cover.
    w.decided("t-1", "ana", admit_built_in("terminal-early", 100_000, false), "R-OK");
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Admit {
            amount_minor: 100_000,
            life: Some(_),
            ..
        })
    ));
    assert_eq!(w.undecided("t-1", "ana", World::admit_lump(Some(400_001)), "R-OK"), "amount_exceeds_cover");
    w.decided("t-1", "ana", World::admit_lump(Some(250_000)), "R-OK");
    assert_eq!(w.claim("t-1").admitted_minor, 353_000);

    // A second claim: the counselling left is 2,000, and it is the whole claim.
    w.open_claim("t-2");
    w.assess("t-2", "CC-0003");
    assert_eq!(w.undecided("t-2", "ana", admit_built_in("counselling", 2_001, true), "R-OK"), "built_in_limit");
    // The early payment's own limit was used on the first claim.
    assert_eq!(w.undecided("t-2", "ana", admit_built_in("terminal-early", 1, true), "R-OK"), "built_in_limit");
    w.decided("t-2", "ana", admit_built_in("counselling", 2_000, true), "R-OK");
    assert_eq!(w.claim("t-2").status, Status::Admitted);
    let admitted = w.core.noticed("claim_admitted");
    assert_eq!(admitted.last().expect("admitted")["built_in"], "counselling");
    assert_eq!(w.undecided("t-2", "ana", admit_built_in("counselling", 1, true), "R-OK"), "not_open");
}

/// Purpose: where the policy caps what one event pays across all its benefits, a claim on one
/// cover is admitted only up to what the other claims for the same life and event left of the
/// cap.
/// Why it matters: AIA Interim Cover pays at most $500,000 for one event however many of its
/// benefits the event touches. Each cover paying up to its own sum insured would pay the same
/// death twice over.
#[test]
fn an_event_cap_holds_across_every_benefit_for_one_event() {
    let mut w = World::new();
    for cover in ["life", "trauma"] {
        w.core.cover(
            "pol-1",
            cover,
            CoverFacts {
                event_cap_minor: Some(300_000),
                ..facts(cover)
            },
        );
    }
    w.open_claim("t-1");
    w.assess("t-1", "CC-0003");
    w.decided("t-1", "ana", World::admit_lump(Some(200_000)), "R-OK");

    w.ok(Command::Notify(Box::new(World::notification("c-1", "pol-1", "life", ClaimType::Death))));
    w.ok(Command::Acknowledge {
        claim: "c-1".into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign("c-1", "ana");
    w.assess("c-1", "CC-0001");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(100_001)), "R-OK"), "amount_exceeds_cover");
    w.decided("c-1", "ana", World::admit_lump(Some(100_000)), "R-OK");
    assert_eq!(w.claim("c-1").admitted_minor, 100_000);
}
