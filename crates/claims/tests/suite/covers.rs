//! What the policy record says about a cover beyond its sum insured: a loan
//! against the policy, a cover on two lives that pays once, covers that reduce
//! each other, a waiting period and a chosen health excess.

use ic_claims::command::Periodic;
use ic_claims::port::{CoverFacts, Interest};
use ic_claims::HealthItem;
use ic_claims::{ClaimType, Command, Instruction, Request};

use crate::world::*;

/// Purpose: a policy loan is repaid out of a death benefit in the admission's own instruction,
/// before any lender with an interest, and the owner gets only what is left after both.
/// Why it matters: persona test CLM-039. The loan is the insurer's own asset secured on the
/// policy; paying the lender or the estate first would leave the loan unpaid with nothing left
/// to repay it from.
#[test]
fn a_policy_loan_is_repaid_from_a_death_benefit_before_any_interest() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "life",
        CoverFacts {
            policy_loan_minor: 40_000,
            interests: vec![
                Interest {
                    party: "bank".into(),
                    extent_minor: Some(100_000),
                    priority: 1,
                },
                Interest {
                    party: "second".into(),
                    extent_minor: Some(50_000),
                    priority: 2,
                },
            ],
            ..facts("life")
        },
    );
    w.ok(Command::Notify(Box::new(World::notification("c-1", "pol-1", "life", ClaimType::Death))));
    w.assign("c-1", "ana");
    w.assess("c-1", "CC-0001");
    w.decided("c-1", "ana", World::admit_lump(Some(300_001)), "R-OK");
    w.approved("c-1", "sam");
    assert!(w.core.instructions.iter().any(|i| matches!(
        i,
        Instruction::Admit {
            repay_loan_minor: 40_000,
            amount_minor: 300_001,
            ..
        }
    )));
    let c = w.claim("c-1");
    assert_eq!((c.loan_repaid_minor, c.payable_minor), (40_000, 260_001));
    let due: Vec<_> = c.interests.iter().map(|i| (i.party.as_str(), i.owed_minor)).collect();
    assert_eq!(due, vec![("bank", 100_000), ("second", 50_000)]);
    // What the owner is paid: the benefit less the loan less the interests.
    assert_eq!(c.payable_minor - c.interests_unpaid_minor(), 110_001);
    assert_eq!(w.core.noticed("claim_admitted").last().expect("published")["loan_repaid_minor"], "40000");
}

/// Purpose: a loan larger than the benefit takes all of it and nothing more, and a loan is not
/// taken from a living benefit such as trauma.
/// Why it matters: the loan cannot be repaid with money the claim does not have, and a trauma
/// payment leaves the policy, and its loan, in force.
#[test]
fn a_loan_takes_no_more_than_the_benefit_and_only_on_death() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "life",
        CoverFacts {
            policy_loan_minor: 900_000,
            ..facts("life")
        },
    );
    w.ok(Command::Notify(Box::new(World::notification("c-1", "pol-1", "life", ClaimType::Death))));
    w.assign("c-1", "ana");
    w.assess("c-1", "CC-0001");
    w.decided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK");
    let c = w.claim("c-1");
    assert_eq!((c.loan_repaid_minor, c.payable_minor), (200_000, 0));

    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            policy_loan_minor: 50_000,
            ..facts("trauma")
        },
    );
    w.open_claim("c-2");
    w.assess("c-2", "CC-0003");
    w.decided("c-2", "ana", World::admit_lump(Some(100_000)), "R-OK");
    assert_eq!((w.claim("c-2").loan_repaid_minor, w.claim("c-2").payable_minor), (0, 100_000));
}

/// Purpose: a cover written on two lives that pays once on the first event counts a claim on
/// either life against the same sum insured, and a cover reduced by another (trauma accelerated
/// from life) counts what the other paid for the same life.
/// Why it matters: persona tests CLM-002 and CLM-009. Without this a couple's joint cover could
/// pay in full twice, and an accelerated trauma payment would leave the life cover it came out
/// of untouched.
#[test]
fn joint_and_linked_covers_count_what_the_others_have_paid() {
    let mut w = World::new();
    let joint = CoverFacts {
        joint_lives: vec!["life-1".into(), "life-2".into()],
        ..facts("trauma")
    };
    w.core.cover("pol-1", "trauma", joint);
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    w.decided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK");
    let mut other = World::notification("c-2", "pol-1", "trauma", ClaimType::Trauma);
    other.life = Some("life-2".into());
    w.ok(Command::Notify(Box::new(other)));
    w.assign("c-2", "ana");
    w.assess("c-2", "CC-0003");
    assert_eq!(w.undecided("c-2", "ana", World::admit_lump(Some(300_001)), "R-OK"), "amount_exceeds_cover");
    w.decided("c-2", "ana", World::admit_lump(None), "R-OK");
    w.approved("c-2", "sam");
    assert_eq!(w.claim("c-2").admitted_minor, 300_000);

    // 25% of trauma paid early comes off the life cover it was accelerated from.
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    w.decided("c-1", "ana", World::admit_lump(Some(125_000)), "R-OK");
    w.core.cover(
        "pol-1",
        "life",
        CoverFacts {
            reduced_by: vec!["trauma".into()],
            ..facts("life")
        },
    );
    w.ok(Command::Notify(Box::new(World::notification("c-2", "pol-1", "life", ClaimType::Death))));
    w.assign("c-2", "ana");
    w.assess("c-2", "CC-0001");
    w.decided("c-2", "ana", World::admit_lump(None), "R-OK");
    w.approved("c-2", "sam");
    assert_eq!(w.claim("c-2").admitted_minor, 375_000);
}

/// Purpose: a benefit in payment cannot start inside the cover's waiting period, and starts on
/// the first day after it.
/// Why it matters: persona test CLM-016. The waiting period is the trade the policyholder made
/// for a lower premium; paying inside it is paying for cover that was never bought.
#[test]
fn a_benefit_starts_only_after_the_waiting_period() {
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        CoverFacts {
            sum_insured_minor: 4_000,
            waiting_days: 91,
            ..facts("income_protection")
        },
    );
    w.ok(Command::Notify(Box::new(World::notification(
        "ip-1",
        "pol-2",
        "ip",
        ClaimType::IncomeProtection,
    ))));
    w.assign("ip-1", "ana");
    w.assess("ip-1", "CC-0002");
    let admit = |first: chrono::NaiveDate| Request::Admit {
        amount_minor: None,
        periodic: Some(Periodic {
            monthly_minor: 3_000,
            first_period: first,
            end: d(2029, 5, 22),
            expected_end: d(2028, 5, 22),
        }),
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    // The event was on 20 February 2027: thirteen weeks later is 22 May.
    assert_eq!(w.undecided("ip-1", "ana", admit(d(2027, 5, 21)), "R-OK"), "invalid_schedule");
    w.decided("ip-1", "ana", admit(d(2027, 5, 22)), "R-OK");
    assert_eq!(w.claim("ip-1").schedule.as_ref().expect("in payment").next_period, d(2027, 5, 22));
}

fn dental(id: &str, invoice: i64) -> Command {
    let mut n = World::notification(id, "pol-3", "health", ClaimType::Health);
    n.health = Some(HealthItem {
        category: "dental".into(),
        invoice_minor: invoice,
        provider: Some("provider-1".into()),
        pre_approval_request: false,
        settles_pre_approval: None,
        service_code: None,
        service_date: None,
    });
    Command::Notify(Box::new(n))
}

fn health(year_start: chrono::NaiveDate, chosen_excess_minor: Option<i64>) -> CoverFacts {
    CoverFacts {
        sum_insured_minor: 0,
        policy_year_start: Some(year_start),
        chosen_excess_minor,
        ..facts("health")
    }
}

/// Purpose: where the life chose an excess on a cover that offers a choice, that excess is taken
/// from a health invoice instead of the one in the claims rules' schedule.
/// Why it matters: a member who chose a higher excess pays a lower premium for it; taking the
/// schedule's lower excess would pay claims the premium never covered.
#[test]
fn the_excess_the_life_chose_replaces_the_schedules() {
    let mut w = World::new();
    w.core.cover("pol-3", "health", health(d(2027, 1, 1), Some(250)));
    w.ok(dental("h-1", 600));
    // 600 less the chosen 250, less the member's 20% share; the schedule's 50 would give 440.
    assert_eq!(w.claim("h-1").admitted_minor, 280);
}

/// Purpose: when a category's yearly limit is used up, the rules stop paying in it; at the policy
/// anniversary the yearly counter starts again, while the lifetime counter keeps everything paid.
/// Why it matters: persona test CLM-012. A family that reached its yearly dental limit is covered
/// again from the anniversary, but never beyond the lifetime limit the product promises.
#[test]
fn the_yearly_limit_resets_at_the_anniversary_and_the_lifetime_limit_does_not() {
    let mut w = World::new();
    w.core.cover("pol-3", "health", health(d(2027, 1, 1), None));
    w.ok(dental("h-1", 5_000));
    w.ok(dental("h-2", 5_000));
    assert_eq!(w.state.health_used["pol-3/life-1/dental/2027-01-01"], 2_000);
    w.ok(dental("h-3", 700));
    assert_eq!(w.claim("h-3").admitted_minor, 0, "the year is used up");

    // The next policy year: the yearly counter is new, the lifetime one is not.
    w.on(d(2028, 1, 10));
    w.core.cover("pol-3", "health", health(d(2028, 1, 1), None));
    w.ok(dental("h-4", 5_000));
    w.ok(dental("h-5", 5_000));
    assert_eq!(w.claim("h-4").admitted_minor + w.claim("h-5").admitted_minor, 2_000);
    assert_eq!(w.state.health_used["pol-3/life-1/dental"], 4_000);
    w.on(d(2029, 1, 10));
    w.core.cover("pol-3", "health", health(d(2029, 1, 1), None));
    w.ok(dental("h-6", 5_000));
    w.ok(dental("h-7", 5_000));
    // 5,000 over the life of the policy: only 1,000 was left.
    assert_eq!(w.claim("h-6").admitted_minor + w.claim("h-7").admitted_minor, 1_000);
    assert_eq!(w.state.health_used["pol-3/life-1/dental"], 5_000);
}
