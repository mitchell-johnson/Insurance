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
                    insolvency: false,
                    party: "bank".into(),
                    extent_minor: Some(100_000),
                    priority: 1,
                },
                Interest {
                    insolvency: false,
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
    assert_eq!(w.state.health_used["pol-3/life-1/health/dental/2027-01-01"], 2_000);
    w.ok(dental("h-3", 700));
    assert_eq!(w.claim("h-3").admitted_minor, 0, "the year is used up");

    // The next policy year: the yearly counter is new, the lifetime one is not.
    w.on(d(2028, 1, 10));
    w.core.cover("pol-3", "health", health(d(2028, 1, 1), None));
    w.ok(dental("h-4", 5_000));
    w.ok(dental("h-5", 5_000));
    assert_eq!(w.claim("h-4").admitted_minor + w.claim("h-5").admitted_minor, 2_000);
    assert_eq!(w.state.health_used["pol-3/life-1/health/dental"], 4_000);
    w.on(d(2029, 1, 10));
    w.core.cover("pol-3", "health", health(d(2029, 1, 1), None));
    w.ok(dental("h-6", 5_000));
    w.ok(dental("h-7", 5_000));
    // 5,000 over the life of the policy: only 1,000 was left.
    assert_eq!(w.claim("h-6").admitted_minor + w.claim("h-7").admitted_minor, 1_000);
    assert_eq!(w.state.health_used["pol-3/life-1/health/dental"], 5_000);
}

/// Purpose: the product's own limits for a health category replace the claims rules' for it,
/// field by field, keeping the rules' member share; a category only the product names is paid
/// within the product's limits with no share.
/// Why it matters: the AIA health products set their own lifetime and per-event limits. One
/// schedule for every product would pay one product's members by another's limits.
#[test]
fn a_products_health_limits_replace_the_rules_for_their_category() {
    use ic_claims::port::HealthLimit;
    let mut w = World::new();
    let limit = |category: &str, per_event: Option<i64>, lifetime: Option<i64>, excess: Option<i64>| HealthLimit {
        category: category.into(),
        per_event_minor: per_event,
        per_year_minor: None,
        per_lifetime_minor: lifetime,
        excess_minor: excess,
    };
    w.core.cover(
        "pol-3",
        "health",
        CoverFacts {
            health_limits: vec![limit("dental", None, Some(1_200), Some(0)), limit("optical", Some(400), None, None)],
            ..health(d(2027, 1, 1), None)
        },
    );
    // 900 with no excess, less the rules' 20% share, inside the rules' 1,000 per event.
    w.ok(dental("h-1", 900));
    assert_eq!(w.claim("h-1").admitted_minor, 720);
    // The product's lifetime 1,200 leaves 480.
    w.ok(dental("h-2", 900));
    assert_eq!(w.claim("h-2").admitted_minor, 480);
    w.ok(line("h-3", "health", "optical", 900));
    assert_eq!(w.claim("h-3").admitted_minor, 400);
}

fn line(id: &str, cover: &str, category: &str, invoice: i64) -> Command {
    let mut n = World::notification(id, "pol-3", cover, ClaimType::Health);
    n.health = Some(HealthItem {
        category: category.into(),
        invoice_minor: invoice,
        provider: Some("provider-1".into()),
        pre_approval_request: false,
        settles_pre_approval: None,
        service_code: None,
        service_date: None,
    });
    Command::Notify(Box::new(n))
}

/// Health rules with three more categories, each with its own excess, share and limits.
fn hospital_rules(w: &mut World) {
    use ic_claims::config::HealthBenefit;
    let mut r = rules();
    r.version = 2;
    let benefit = |excess_minor: i64, copay_pct: u32, per_year_minor: i64| HealthBenefit {
        per_event_minor: 100_000,
        per_year_minor,
        lifetime_minor: 1_000_000,
        copay_pct,
        excess_minor,
    };
    r.straight_through_max_minor.insert(ClaimType::Health, 100_000);
    r.health_benefits.insert("surgery".into(), benefit(500, 0, 50_000));
    r.health_benefits.insert("specialist".into(), benefit(0, 20, 2_000));
    r.health_benefits.insert("physio".into(), benefit(20, 50, 1_000));
    w.ok(Command::ActivateRules { rules: Box::new(r) });
}

/// Purpose: one episode of care billed as lines (surgery, a specialist's consultation, physio
/// afterwards) is priced line by line under each category's own excess and share, and each line
/// counts against its own cover's counters.
/// Why it matters: persona test HLT-004. A single excess or share across the whole episode would
/// pay the wrong amount on every line, and counting physio against the surgical limit would
/// exhaust cover the member still has.
#[test]
fn an_episode_is_priced_line_by_line_against_each_covers_counters() {
    let mut w = World::new();
    hospital_rules(&mut w);
    w.core.cover("pol-3", "hospital", health(d(2027, 1, 1), None));
    w.core.cover("pol-3", "daycare", health(d(2027, 1, 1), None));
    w.ok(line("h-surgery", "hospital", "surgery", 12_000));
    w.ok(line("h-specialist", "hospital", "specialist", 400));
    w.ok(line("h-physio", "daycare", "physio", 120));
    let admitted = |id: &str| w.claim(id).admitted_minor;
    // 12,000 less 500 excess; 400 less a 20% share; 120 less 20 excess, then half.
    assert_eq!((admitted("h-surgery"), admitted("h-specialist"), admitted("h-physio")), (11_500, 320, 50));
    let used = &w.state.health_used;
    assert_eq!(used["pol-3/life-1/hospital/surgery/2027-01-01"], 11_500);
    assert_eq!(used["pol-3/life-1/hospital/specialist/2027-01-01"], 320);
    assert_eq!(used["pol-3/life-1/daycare/physio/2027-01-01"], 50);
}

/// Purpose: the same category on two covers counts on each cover's own counters, unless the
/// product's shell declares a counter they share, and then one limit covers both.
/// Why it matters: persona test HLT-009. A member with two covers bought two limits; counting
/// them together pays too little, and counting a declared shared limit twice pays too much.
#[test]
fn counters_never_cross_covers_unless_the_shell_shares_one() {
    let mut w = World::new();
    w.core.cover("pol-3", "health", health(d(2027, 1, 1), None));
    w.core.cover("pol-3", "extras", health(d(2027, 1, 1), None));
    w.ok(line("h-1", "health", "dental", 5_000));
    w.ok(line("h-2", "health", "dental", 5_000));
    // The year's 2,000 is used on one cover; the other still has its own.
    w.ok(line("h-3", "extras", "dental", 5_000));
    assert_eq!(w.claim("h-3").admitted_minor, 1_000);

    let mut w = World::new();
    let shared = CoverFacts {
        shared_counter: Some("family-dental".into()),
        ..health(d(2027, 1, 1), None)
    };
    w.core.cover("pol-3", "health", shared.clone());
    w.core.cover("pol-3", "extras", shared);
    w.ok(line("h-1", "health", "dental", 5_000));
    w.ok(line("h-2", "health", "dental", 5_000));
    w.ok(line("h-3", "extras", "dental", 5_000));
    assert_eq!(w.claim("h-3").admitted_minor, 0, "one shared limit, used up");
    assert_eq!(w.state.health_used["pol-3/life-1/family-dental/dental/2027-01-01"], 2_000);
}
