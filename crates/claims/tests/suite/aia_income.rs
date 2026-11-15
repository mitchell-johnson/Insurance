//! The benefits built into AIA's income and disablement covers that change
//! how a claim is worked: the recurrence window, a waiting period served
//! alongside another policy's, the qualifying period of a disablement cover
//! and its waiver, partial disablement, the Permanent Disablement Benefit,
//! the Peak Season Upgrade, Business Mentors, Rehabilitation and Support, a
//! survival period, and a cover that a claim leaves in place.

use ic_claims::command::{Periodic, ReopenBasis};
use ic_claims::model::EndReason;
use ic_claims::port::{BuiltIn, CoverFacts};
use ic_claims::{ClaimType, Command, Instruction, Request, Status, TaskKind};

use crate::world::*;

fn built_in(code: &str, kind: &str, limit_minor: Option<i64>, applies: bool) -> BuiltIn {
    BuiltIn {
        code: code.into(),
        kind: kind.into(),
        limit_minor,
        applies,
    }
}

fn ip_facts(built_in: Vec<BuiltIn>) -> CoverFacts {
    CoverFacts {
        sum_insured_minor: 4_000,
        built_in,
        ..facts("income_protection")
    }
}

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

/// An income claim notified, owned by ana, with the cause and cover check recorded.
fn ip_claim(w: &mut World, id: &str, policy: &str, event: chrono::NaiveDate) {
    let mut n = World::notification(id, policy, "ip", ClaimType::IncomeProtection);
    n.event_date = event;
    w.ok(Command::Notify(Box::new(n)));
    w.assign(id, "ana");
    w.assess(id, "CC-0002");
}

fn reopen(id: &str) -> Command {
    Command::Reopen {
        claim: id.into(),
        basis: ReopenBasis::Recurrence,
        reserve_minor: 20_000,
        actor: actor("ana"),
    }
}

fn last_gross(w: &World) -> i64 {
    w.core
        .instructions
        .iter()
        .rev()
        .find_map(|i| match i {
            Instruction::PayBenefit { gross_minor, .. } => Some(*gross_minor),
            _ => None,
        })
        .expect("a benefit was paid")
}

/// An income benefit paid for March, ended on 16 April and closed by the April run.
fn ended_benefit(w: &mut World, id: &str) {
    w.benefit_in_payment(id);
    w.pay_run("run-1", d(2027, 3, 1));
    w.decided(
        id,
        "ana",
        Request::EndBenefit {
            end: d(2027, 4, 16),
            reason: EndReason::Recovered,
        },
        "R-OK",
    );
    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    assert_eq!(w.claim(id).status, Status::Ended);
}

/// Purpose: where the product lists its built-in benefits, the Recurrent Disablement Benefit
/// sets the recurrence window at 12 months after payments ceased, beyond the rules' default;
/// a product that lists its benefits without it has no window at all.
/// Why it matters: the AIA wordings promise recurrence without a new waiting period for twelve
/// months, and only on the covers that carry the benefit. The rules' 180 days would refuse a
/// recurrence the policy pays, and a product without the benefit would be paid one it does not.
#[test]
fn the_recurrence_window_is_the_products_recurrent_disablement_benefit() {
    let mut w = World::new();
    w.core
        .cover("pol-2", "ip", ip_facts(vec![built_in("recurrent-disablement", "facility", None, true)]));
    ended_benefit(&mut w, "ip-1");
    // Ten months on is past the rules' 180 days but inside the product's twelve months.
    w.on(d(2028, 2, 1));
    w.ok(reopen("ip-1"));
    assert_eq!(w.claim("ip-1").status, Status::InPayment);

    let mut w = World::new();
    w.core
        .cover("pol-2", "ip", ip_facts(vec![built_in("rehabilitation-support", "facility", None, true)]));
    ended_benefit(&mut w, "ip-1");
    w.on(d(2027, 4, 2));
    assert_eq!(w.refused(reopen("ip-1")), "recurrence_window");
}

/// Purpose: with the Concurrent Waiting Period Benefit, an income claim for the same cause as
/// one already in payment on another policy for the same person serves its waiting period from
/// the earlier claim's event; without the benefit it serves its own.
/// Why it matters: someone with two income covers would otherwise wait twice for the same
/// illness, which is exactly what the AIA benefit promises they will not. Applying it to covers
/// without the benefit would pay a month early on each.
#[test]
fn a_concurrent_waiting_period_runs_from_the_earlier_claims_event() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    let waiting = |built: Vec<BuiltIn>| CoverFacts {
        waiting_days: 30,
        ..ip_facts(built)
    };
    w.core.cover("pol-4", "ip", waiting(vec![]));
    w.core
        .cover("pol-5", "ip", waiting(vec![built_in("concurrent-waiting-period", "facility", None, true)]));
    // An earlier claim not yet accepted does not start the waiting period.
    ip_claim(&mut w, "ip-b", "pol-4", d(2027, 2, 24));
    ip_claim(&mut w, "ip-c", "pol-5", d(2027, 2, 25));
    // Thirty days from the first claim's event on 20 February.
    let first = d(2027, 3, 22);
    assert_eq!(w.undecided("ip-b", "ana", income(2_000, first, d(2028, 3, 22)), "R-OK"), "invalid_schedule");
    w.decided("ip-c", "ana", income(2_000, first, d(2028, 3, 22)), "R-OK");
    assert_eq!(w.claim("ip-c").status, Status::InPayment);
    // Without its own: thirty days from its own event.
    w.decided("ip-b", "ana", income(2_000, d(2027, 3, 26), d(2028, 3, 26)), "R-OK");

    // A claim ended in the last twelve months still counts.
    let mut w = World::new();
    ended_benefit(&mut w, "ip-1");
    w.core
        .cover("pol-5", "ip", waiting(vec![built_in("concurrent-waiting-period", "facility", None, true)]));
    w.on(d(2027, 5, 2));
    ip_claim(&mut w, "ip-c", "pol-5", d(2027, 5, 1));
    w.decided("ip-c", "ana", income(2_000, d(2027, 5, 10), d(2028, 5, 10)), "R-OK");
}

/// Purpose: a disablement claim is not admitted before its qualifying period has run, unless the
/// assessor waives it with evidence on a cover that carries the waiver; a cover without the
/// waiver cannot have it waived.
/// Why it matters: the qualifying period is part of what makes the incapacity permanent under
/// the wording, and the waiver is a benefit some covers buy. Paying early without it, or
/// refusing a waiver the cover carries, both get the contract wrong.
#[test]
fn a_disablement_claim_waits_out_its_qualifying_period_unless_waived() {
    let mut w = World::new();
    let tpd = |built: Vec<BuiltIn>| CoverFacts {
        waiting_days: 90,
        built_in: built,
        ..facts("tpd")
    };
    w.core
        .cover("pol-1", "tpd", tpd(vec![built_in("waiver-of-waiting-period", "facility", None, true)]));
    w.core.cover("pol-6", "tpd", tpd(vec![]));
    let waive = |id: &str, evidence: &str| Command::WaiveWaitingPeriod {
        claim: id.into(),
        evidence_ref: evidence.into(),
        actor: actor("ana"),
    };
    for (id, policy) in [("t-1", "pol-1"), ("t-2", "pol-6")] {
        w.ok(Command::Notify(Box::new(World::notification(
            id,
            policy,
            "tpd",
            ClaimType::TotalPermanentDisability,
        ))));
        w.assign(id, "ana");
        w.assess(id, "CC-0001");
    }
    assert_eq!(w.undecided("t-1", "ana", World::admit_lump(Some(100_000)), "R-OK"), "qualifying_period");
    assert_eq!(w.refused(waive("t-2", "vault-9")), "no_waiver_benefit");
    assert_eq!(w.refused(waive("t-1", " ")), "evidence_required");
    w.ok(waive("t-1", "vault-9"));
    assert_eq!(w.claim("t-1").waiting_waived.as_deref(), Some("vault-9"));
    w.decided("t-1", "ana", World::admit_lump(Some(100_000)), "R-OK");
    assert_eq!(w.refused(waive("t-1", "vault-9")), "not_open");
    // Once the period has run, no waiver is needed.
    w.on(d(2027, 5, 21));
    w.decided("t-2", "ana", World::admit_lump(Some(100_000)), "R-OK");
}

/// Purpose: a cover whose product offers the Partial Disablement Benefit, not chosen, refuses
/// income declared while a benefit is paid; the same cover with it chosen takes the income off
/// the benefit.
/// Why it matters: without the partial benefit the cover pays only for total disablement, so
/// earning again ends the claim rather than reducing it. Paying a reduced benefit the policy
/// never bought is a leak; refusing one it did buy is a breach.
#[test]
fn income_while_earning_needs_the_partial_disablement_benefit() {
    for (chosen, outcome) in [(false, Some("no_partial_cover")), (true, None)] {
        let mut w = World::new();
        w.core
            .cover("pol-2", "ip", ip_facts(vec![built_in("partial-disablement", "facility", None, chosen)]));
        w.benefit_in_payment("ip-1");
        w.pay_run("run-1", d(2027, 3, 1));
        w.on(d(2027, 4, 1));
        let declare = Command::DeclareIncome {
            claim: "ip-1".into(),
            from: d(2027, 4, 1),
            other_income_minor: 1_000,
            actor: actor("ana"),
        };
        match outcome {
            Some(code) => assert_eq!(w.refused(declare), code),
            None => {
                w.ok(declare);
                w.pay_run("run-2", d(2027, 4, 1));
                assert_eq!(last_gross(&w), 2_000);
            }
        }
    }
}

/// Purpose: the Permanent Disablement Benefit adds a third of the monthly benefit once it has
/// been paid for 24 consecutive months, only on a cover that carries it, once, and stops when
/// ended; each step is refused when it does not fit.
/// Why it matters: the AIA premier income cover pays it to someone unlikely ever to return to
/// work. Starting it early, twice, or on a cover without it overpays; never adding it underpays
/// the people who most rely on the cover.
#[test]
fn the_permanent_disablement_benefit_adds_a_third_after_24_months() {
    let mut w = World::new();
    w.core
        .cover("pol-2", "ip", ip_facts(vec![built_in("permanent-disablement", "facility", None, true)]));
    w.core.cover("pol-4", "ip", ip_facts(vec![]));
    ip_claim(&mut w, "ip-1", "pol-2", d(2027, 2, 20));
    ip_claim(&mut w, "ip-2", "pol-4", d(2027, 2, 20));
    let permanent = |ends: bool| Request::PermanentDisablement { ends };
    assert_eq!(w.undecided("ip-1", "ana", permanent(false), "R-OK"), "not_in_payment");
    for id in ["ip-1", "ip-2"] {
        w.decided(id, "ana", income(3_000, d(2027, 3, 1), d(2032, 3, 1)), "R-OK");
        w.verify_payee(id, "ana");
    }
    assert_eq!(w.undecided("ip-2", "ana", permanent(false), "R-OK"), "no_permanent_benefit");
    assert_eq!(w.undecided("ip-1", "ana", permanent(true), "R-OK"), "not_running");
    let mut month = d(2027, 3, 1);
    for n in 0..24 {
        w.on(month);
        w.pay_run(&format!("run-{n}"), month);
        if n == 22 {
            assert_eq!(w.undecided("ip-1", "ana", permanent(false), "R-OK"), "not_24_months");
        }
        month = month.checked_add_months(chrono::Months::new(1)).expect("a real date");
    }
    w.on(month);
    w.decided("ip-1", "ana", permanent(false), "R-OK");
    let notice = w.core.noticed("permanent_disablement_accepted");
    assert_eq!((notice[0]["monthly_minor"].as_str(), notice[0]["from"].as_str()), ("1000", "2029-03-01"));
    assert_eq!(w.undecided("ip-1", "ana", permanent(false), "R-OK"), "already_started");
    w.pay_run("run-24", month);
    let paid: Vec<_> = w
        .core
        .instructions
        .iter()
        .filter_map(|i| match i {
            Instruction::PayBenefit { claim, gross_minor, .. } if claim == "ip-1" => Some(*gross_minor),
            _ => None,
        })
        .collect();
    assert_eq!(paid.last(), Some(&4_000));

    w.on(d(2029, 3, 15));
    w.decided("ip-1", "ana", permanent(true), "R-OK");
    assert_eq!(w.core.noticed("permanent_disablement_ended").len(), 1);
    w.on(d(2029, 4, 1));
    w.pay_run("run-25", d(2029, 4, 1));
    assert_eq!(last_gross(&w), 3_000);
}

/// Purpose: the Peak Season Upgrade adds its amount to the periods that start in the peak months
/// the schedule names, and nothing to the others.
/// Why it matters: a seasonal business loses most in its peak months, which is what the upgrade
/// is bought for. Paying it every month, or never, gets the benefit wrong both ways.
#[test]
fn the_peak_season_upgrade_is_paid_in_the_peak_months() {
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        CoverFacts {
            peak_months: vec![4, 13],
            ..ip_facts(vec![built_in("peak-season", "monthly_addition", Some(1_000), true)])
        },
    );
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    assert_eq!(last_gross(&w), 3_000);
    w.on(d(2027, 4, 1));
    w.pay_run("run-2", d(2027, 4, 1));
    assert_eq!(last_gross(&w), 4_000);
    assert_eq!(w.claim("ip-1").schedule.as_ref().expect("in payment").peak_months, vec![4]);
}

/// Purpose: the Business Mentors Benefit is paid once for each business, while the income
/// benefit is paid, up to the rules' amount, and refused until that amount is set; other
/// facilities, and additions paid with the benefit, are not admitted as payments.
/// Why it matters: the wording pays a two-year membership and nothing more. Paying it on an
/// unaccepted claim, twice for one business, or treating a service such as a nurse line as cash
/// would each pay money the cover does not promise.
#[test]
fn business_mentors_is_paid_once_while_the_benefit_is_paid() {
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        ip_facts(vec![
            built_in("business-mentors", "facility", None, true),
            built_in("premier-nurse", "facility", None, true),
            built_in("peak-season", "monthly_addition", Some(1_000), true),
        ]),
    );
    let mentors = |amount_minor: i64| Request::AdmitBuiltIn {
        code: "business-mentors".into(),
        amount_minor,
        settles: false,
    };
    ip_claim(&mut w, "ip-1", "pol-2", d(2027, 2, 20));
    assert_eq!(w.undecided("ip-1", "ana", mentors(500), "R-OK"), "not_in_payment");
    w.decided("ip-1", "ana", income(3_000, d(2027, 3, 1), d(2029, 3, 1)), "R-OK");
    assert_eq!(w.undecided("ip-1", "ana", mentors(500), "R-OK"), "not_configured");
    // Rules that leave the amount out take the best-estimate placeholder.
    let mut written = serde_json::to_value(rules()).expect("rules write");
    written.as_object_mut().expect("an object").remove("business_mentors_minor");
    let read: ic_claims::config::Rules = serde_json::from_value(written).expect("rules read");
    assert_eq!(read.business_mentors_minor, 100_000);
    let mut r = rules();
    r.version = 2;
    r.business_mentors_minor = 2_000;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    let other = |code: &str| Request::AdmitBuiltIn {
        code: code.into(),
        amount_minor: 500,
        settles: false,
    };
    assert_eq!(w.undecided("ip-1", "ana", other("premier-nurse"), "R-OK"), "not_payable");
    assert_eq!(w.undecided("ip-1", "ana", other("peak-season"), "R-OK"), "paid_with_the_benefit");
    assert_eq!(w.undecided("ip-1", "ana", mentors(2_001), "R-OK"), "built_in_limit");
    w.decided("ip-1", "ana", mentors(2_000), "R-OK");
    assert_eq!(w.claim("ip-1").built_in["business-mentors"], 2_000);
    assert_eq!(w.undecided("ip-1", "ana", mentors(1), "R-OK"), "built_in_limit");
}

/// Purpose: a cover with Rehabilitation and Support opens a case manager's rehabilitation task
/// on an income claim as soon as the cover is checked, and its spending is paid; a product that
/// lists its benefits without it pays nothing towards rehabilitation.
/// Why it matters: the wording appoints a case manager on notification, so waiting for someone
/// to remember loses the early weeks that matter most. Spending on a cover without the benefit
/// is money the policy never promised.
#[test]
fn rehabilitation_support_opens_a_case_manager_and_pays_only_where_carried() {
    let complete = |id: &str, task: &str, spend_minor: i64| Command::CompleteTask {
        claim: id.into(),
        task: task.into(),
        outcome: "plan agreed".into(),
        attendees: vec![],
        vault_ref: None,
        spend_minor,
        actor: actor("ana"),
    };
    let mut w = World::new();
    w.core
        .cover("pol-2", "ip", ip_facts(vec![built_in("rehabilitation-support", "facility", None, true)]));
    w.core
        .cover("pol-4", "ip", ip_facts(vec![built_in("recurrent-disablement", "facility", None, true)]));
    ip_claim(&mut w, "ip-1", "pol-2", d(2027, 2, 20));
    ip_claim(&mut w, "ip-2", "pol-4", d(2027, 2, 20));
    assert!(w.claim("ip-1").has_open_task(TaskKind::Rehabilitation));
    assert!(!w.claim("ip-2").has_open_task(TaskKind::Rehabilitation));
    assert_eq!(w.core.noticed("rehabilitation_case_manager").len(), 1);
    w.ok(complete("ip-1", "rehabilitation/case-manager", 500));

    w.task("ip-2", "rehab-1", TaskKind::Rehabilitation, "ana");
    assert_eq!(w.refused(complete("ip-2", "rehab-1", 500)), "no_rehabilitation_benefit");
    w.ok(complete("ip-2", "rehab-1", 0));
}

/// Purpose: a lump sum with a survival period is not admitted until the life has survived it
/// after the event, and is admitted once it has.
/// Why it matters: the AIA business covers pay only if the life survives 14 days. Paying sooner
/// pays a living benefit for what may turn out to be a death, which is a different claim on a
/// different cover.
#[test]
fn a_lump_sum_waits_for_its_survival_period() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            survival_days: 14,
            ..facts("trauma")
        },
    );
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(100_000)), "R-OK"), "survival_period");
    w.on(d(2027, 3, 6));
    w.decided("c-1", "ana", World::admit_lump(Some(100_000)), "R-OK");
    assert_eq!(w.claim("c-1").status, Status::Admitted);
}

/// Purpose: a lump sum on a cover the product says a claim does not end is admitted without
/// taking it off the life's cover, and an earlier claim does not use up the sum insured.
/// Why it matters: some AIA business covers stay in place after a claim. Reducing them would
/// cancel cover the customer still pays for, and counting earlier claims would refuse a second
/// event the cover pays in full.
#[test]
fn a_cover_a_claim_leaves_in_place_is_not_used_up() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            ends_cover: Some(false),
            ..facts("trauma")
        },
    );
    for id in ["c-1", "c-2"] {
        w.open_claim(id);
        w.assess(id, "CC-0003");
        w.decided(id, "ana", World::admit_lump(Some(500_000)), "R-OK");
        w.approved(id, "sam");
        assert_eq!(w.claim(id).status, Status::Admitted);
    }
    let admits: Vec<_> = w
        .core
        .instructions
        .iter()
        .filter_map(|i| match i {
            Instruction::Admit { life, amount_minor, .. } => Some((life.clone(), *amount_minor)),
            _ => None,
        })
        .collect();
    assert_eq!(admits, vec![(None, 500_000), (None, 500_000)]);
}

/// Purpose: a premium waiver built into the cover is admitted on a waiver claim for its own
/// length (six months for pregnancy, two years after a death), from a date inside its window,
/// only with evidence on file, only where the cover carries it, and for pregnancy only where the
/// birth falls at least a year after the cover started; the core is asked to waive that span.
/// Why it matters: these benefits pay nothing out, so the only thing claims controls is the
/// span waived. Waiving too long, without the doctor's confirmation or death certificate, or on a
/// cover too new for the pregnancy benefit, gives away premium the policy does not promise.
#[test]
fn a_built_in_premium_waiver_runs_for_its_own_term() {
    let waiver = |code: &str, from: chrono::NaiveDate| Request::BuiltInWaiver { code: code.into(), from };
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        ip_facts(vec![
            built_in("pregnancy-premium-waiver", "premium_waiver", None, true),
            built_in("waiver-on-death", "premium_waiver", None, false),
            built_in("other-waiver", "premium_waiver", None, true),
            built_in("rehabilitation-support", "facility", None, true),
        ]),
    );
    w.core.cover(
        "pol-4",
        "ip",
        CoverFacts {
            cover_start: Some(d(2026, 12, 1)),
            ..ip_facts(vec![built_in("pregnancy-premium-waiver", "premium_waiver", None, true)])
        },
    );
    for (id, policy) in [("w-1", "pol-2"), ("w-2", "pol-4")] {
        w.ok(Command::Notify(Box::new(World::notification(id, policy, "ip", ClaimType::PremiumWaiver))));
        w.assign(id, "ana");
        w.assess(id, "CC-0002");
    }
    let from = d(2027, 3, 1);
    assert_eq!(w.undecided("ip-x", "ana", waiver("pregnancy-premium-waiver", from), "R-OK"), "unknown_claim");
    assert_eq!(w.undecided("w-1", "ana", waiver("rehabilitation-support", from), "R-OK"), "unknown_benefit");
    assert_eq!(w.undecided("w-1", "ana", waiver("waiver-on-death", from), "R-OK"), "built_in_not_chosen");
    assert_eq!(w.undecided("w-1", "ana", waiver("pregnancy-premium-waiver", from), "R-OK"), "evidence_required");
    for id in ["w-1", "w-2"] {
        w.ok(Command::RequestEvidence {
            claim: id.into(),
            evidence: "doctor".into(),
            kind: "medical_report".into(),
            from: "treating-doctor".into(),
            actor: actor("ana"),
        });
        w.ok(Command::ReceiveEvidence {
            claim: id.into(),
            evidence: "doctor".into(),
            vault_ref: "vault-7".into(),
            cost_minor: 0,
        });
    }
    assert_eq!(w.undecided("w-1", "ana", waiver("other-waiver", from), "R-OK"), "no_waiver_terms");
    assert_eq!(
        w.undecided("w-2", "ana", waiver("pregnancy-premium-waiver", from), "R-OK"),
        "too_soon_after_start"
    );
    assert_eq!(
        w.undecided("w-1", "ana", waiver("pregnancy-premium-waiver", d(2027, 2, 19)), "R-OK"),
        "outside_waiver_window"
    );
    assert_eq!(
        w.undecided("w-1", "ana", waiver("pregnancy-premium-waiver", d(2028, 2, 21)), "R-OK"),
        "outside_waiver_window"
    );
    w.decided("w-1", "ana", waiver("pregnancy-premium-waiver", from), "R-OK");
    let c = w.claim("w-1");
    assert_eq!((c.status, c.waiver_from), (Status::InPayment, Some(from)));
    assert_eq!(c.schedule.as_ref().expect("waiving").end, d(2027, 9, 1));
    assert_eq!(w.core.noticed("claim_admitted").last().expect("published")["until"], "2027-09-01");
    w.ok(Command::DrainDue);
    assert!(w.core.instructions.iter().any(|i| matches!(
        i,
        Instruction::WaivePremium { claim, from: f, until, only_cover: true, .. } if claim == "w-1" && *f == from && *until == d(2027, 9, 1)
    )));
    assert_eq!(w.undecided("w-1", "ana", waiver("pregnancy-premium-waiver", from), "R-OK"), "not_open");

    // Two years after the policy owner's death.
    let mut w = World::new();
    w.core
        .cover("pol-2", "ip", ip_facts(vec![built_in("waiver-on-death", "premium_waiver", None, true)]));
    w.ok(Command::Notify(Box::new(World::notification("w-3", "pol-2", "ip", ClaimType::PremiumWaiver))));
    w.assign("w-3", "ana");
    w.assess("w-3", "CC-0002");
    w.ok(Command::RequestEvidence {
        claim: "w-3".into(),
        evidence: "death".into(),
        kind: "death_certificate".into(),
        from: "registrar".into(),
        actor: actor("ana"),
    });
    w.ok(Command::ReceiveEvidence {
        claim: "w-3".into(),
        evidence: "death".into(),
        vault_ref: "vault-8".into(),
        cost_minor: 0,
    });
    w.decided("w-3", "ana", waiver("waiver-on-death", from), "R-OK");
    assert_eq!(w.claim("w-3").schedule.as_ref().expect("waiving").end, d(2029, 3, 1));
    // After a death the whole policy's premium is waived, not one cover's.
    w.ok(Command::DrainDue);
    assert!(w
        .core
        .instructions
        .iter()
        .any(|i| matches!(i, Instruction::WaivePremium { claim, only_cover: false, .. } if claim == "w-3")));
}
