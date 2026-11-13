//! Ceded premium: what each reinsurer is paid for each billing period, and
//! how a change to a cession in the past corrects it.

mod common;

use common::*;
use ic_core::event::Event;
use ic_core::{LifeId, PartyId, PolicyId, PolicyStatus, UnwindCause};
use ic_reinsurance::account::LineKind;
use ic_reinsurance::premium::{rated, ChargeKind};
use ic_reinsurance::{InstructionBody, Notice, PostingKind};

/// Anna, 36 and a non-smoker, with 800,000 of life cover: 300,000 ceded, 60/40.
fn anna() -> Bench {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.factors("P1", "L1", d(1990, 3, 10), false);
    b
}

fn charged(b: &Bench, policy: &str, start: chrono::NaiveDate, kind: ChargeKind, reinsurer: &ic_reinsurance::ReinsurerId) -> i64 {
    b.ri.charged(&PolicyId::from(policy), start)
        .unwrap()
        .iter()
        .filter(|(k, _)| k.kind == kind && &k.reinsurer == reinsurer)
        .map(|(_, v)| *v)
        .sum()
}

/// Purpose: a billing period charges each reinsurer its share of the sum ceded at the
/// reinsurer's rate for the days in the period, with commission on top, to the cent.
/// Why it matters: this is the money paid to reinsurers every month. The figures here are
/// worked by hand from the shipped treaty and rate table, so a change to rounding or day count
/// shows up as a failed test and not as a disputed statement.
#[test]
fn a_period_charges_each_reinsurer_its_share_at_its_rate() {
    let mut b = anna();
    let applied = b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("life", 61_84)]);
    // 180,000 at 0.65 per thousand a year is 117.00 a year; 28 days of it is 8.98.
    assert_eq!(rated(180 * K, 650_000, 28), 8_98);
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), 8_98);
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &south()), 5_98);
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Commission, &north()), 1_80);
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Commission, &south()), 1_20);
    // Each charge is an instruction to the core, valued on the first day of the period.
    let posts: Vec<_> = applied
        .instructions
        .iter()
        .filter_map(|i| match &i.body {
            InstructionBody::Post {
                kind,
                reinsurer,
                amount_minor,
                value_date,
                policy,
                reference,
                ..
            } => {
                assert_eq!((*value_date, policy.clone(), i.partition), (d(2027, 2, 1), Some(PolicyId::from("P1")), 1));
                assert_eq!(reference, "P1/life/1/L1/2027-02-01");
                Some((*kind, reinsurer.clone(), *amount_minor))
            }
            _ => None,
        })
        .collect();
    assert_eq!(posts.len(), 4);
    assert!(posts.contains(&(PostingKind::CededPremium, north(), 8_98)) && posts.contains(&(PostingKind::Commission, south(), 1_20)));
    assert_eq!(b.ri.booked(&retail(), &north(), LineKind::CededPremium), 8_98);
    assert_eq!(b.ri.balance(&retail(), &north()), 8_98 - 1_80);
    // The same event again posts nothing.
    let records = b.records(1, vec![]);
    assert!(records.is_empty());
    b.assert_reconciles();
}

/// Purpose: a cover that cannot be priced, because the life's rating facts or the customer's
/// premium are missing, is charged nothing and raised as an exception, and is charged in full
/// as soon as the facts arrive.
/// Why it matters: a guessed rate is a wrong payment nobody can see. An open exception is a
/// visible job for a person, and the later catch-up proves nothing is lost by waiting.
#[test]
fn a_cover_that_cannot_be_priced_is_raised_not_guessed() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    let applied = b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    assert!(b.ri.charged(&PolicyId::from("P1"), d(2027, 2, 1)).unwrap().is_empty());
    assert!(applied
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ExceptionOpened { code, .. } if code == "unpriced_cession")));
    assert!(b.ri.exceptions().contains_key("unpriced:P1/2027-02-01"));
    b.factors("P1", "L1", d(1990, 3, 10), false);
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), 8_98);
    assert!(b.ri.exceptions().is_empty());
    assert!(b.ri.charged(&PolicyId::from("P1"), d(2027, 9, 1)).is_none());
    b.assert_reconciles();
}

/// Purpose: a backdated increase re-prices the period it reaches into and posts only the
/// difference, valued on that period's first day.
/// Why it matters: backdated changes are routine. Posting the whole period again would double
/// the premium; posting nothing would leave the reinsurer unpaid for cover it is on risk for.
#[test]
fn a_backdated_change_posts_only_the_difference() {
    let mut b = anna();
    b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    b.today = d(2027, 3, 10);
    b.premium_due(1, "P1", 2, d(2027, 3, 1), d(2027, 4, 1), &[]);
    let march = charged(&b, "P1", d(2027, 3, 1), ChargeKind::Premium, &north());
    let before = b.instructions.len();
    b.add_layer(1, "P1", layer("life", "life", 2, "L1", 100 * K, d(2027, 2, 15)));
    // 60,000 more to north for the last 14 days of February: 1.50.
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), 8_98 + 1_50);
    let new: Vec<_> = b.instructions[before..]
        .iter()
        .filter_map(|i| match &i.body {
            InstructionBody::Post {
                kind: PostingKind::CededPremium,
                reinsurer,
                amount_minor,
                value_date,
                ..
            } if *reinsurer == north() => Some((*value_date, *amount_minor)),
            _ => None,
        })
        .collect();
    assert_eq!(new, vec![(d(2027, 2, 1), 1_50), (d(2027, 3, 1), rated(60 * K, 650_000, 31))]);
    assert_eq!(
        charged(&b, "P1", d(2027, 3, 1), ChargeKind::Premium, &north()),
        march + rated(60 * K, 650_000, 31)
    );
    b.assert_reconciles();
}

/// Purpose: a lapse part-way through a billed period takes back the ceded premium for the
/// days after it, and an unwind of the billing takes back all of it; billing the period again
/// charges it afresh.
/// Why it matters: addendum decision 1. When the core reverses a period's billing the ceded
/// premium for it must reverse too, by new entries with the period's own value date, or the
/// reinsurer keeps premium for cover the customer was refunded.
#[test]
fn lapse_and_unwind_take_ceded_premium_back() {
    let mut b = anna();
    b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    b.today = d(2027, 2, 15);
    b.feed(
        1,
        vec![Event::PolicyStatusChanged {
            policy: PolicyId::from("P1"),
            from: PolicyStatus::InForce,
            to: PolicyStatus::Lapsed,
        }],
    );
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), rated(180 * K, 650_000, 14));
    b.feed(
        1,
        vec![Event::PolicyStatusChanged {
            policy: PolicyId::from("P1"),
            from: PolicyStatus::Lapsed,
            to: PolicyStatus::InForce,
        }],
    );
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), 8_98);
    b.today = d(2027, 3, 20);
    b.premium_due(1, "P1", 2, d(2027, 3, 1), d(2027, 4, 1), &[]);
    let unwound = b.feed(
        1,
        vec![Event::PolicyUnwound {
            policy: PolicyId::from("P1"),
            after: d(2027, 2, 20),
            cause: UnwindCause::Remediation,
            reference: "REM-1".into(),
            receivables: vec![2],
        }],
    );
    assert!(b.ri.charged(&PolicyId::from("P1"), d(2027, 3, 1)).unwrap().is_empty());
    assert!(unwound.instructions.iter().all(|i| matches!(&i.body, InstructionBody::Post { value_date, amount_minor, kind, .. } if *value_date == d(2027, 3, 1) && (*amount_minor < 0) == (*kind == PostingKind::CededPremium || *kind == PostingKind::Commission))));
    assert_eq!(unwound.instructions.len(), 4);
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), 8_98);
    // The same unwind again reverses nothing more.
    let again = b.feed(
        1,
        vec![Event::PolicyUnwound {
            policy: PolicyId::from("P1"),
            after: d(2027, 2, 20),
            cause: UnwindCause::Remediation,
            reference: "REM-1".into(),
            receivables: vec![2],
        }],
    );
    assert!(again.instructions.is_empty());
    // The period is billed again when the policy resumes.
    b.premium_due(1, "P1", 3, d(2027, 3, 1), d(2027, 4, 1), &[]);
    assert_eq!(charged(&b, "P1", d(2027, 3, 1), ChargeKind::Premium, &north()), rated(180 * K, 650_000, 31));
    assert_eq!(b.posted(PostingKind::CededPremium, &north()), 8_98 + rated(180 * K, 650_000, 31));
    b.assert_reconciles();
}

/// Purpose: a treaty priced on the original premium cedes the reinsurer's proportion of the
/// customer's premium for the cover type, and a facultative share is charged at its own rate.
/// Why it matters: the group treaty is priced this way. The proportion must be of the premium
/// the core actually billed, which is why the core now sends it by cover type.
#[test]
fn original_premium_and_facultative_shares_are_charged_on_their_own_terms() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 450 * K, d(2027, 2, 1))]);
    b.factors("P1", "L1", d(1990, 3, 10), false);
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 700 * K), member_cover("M2", 300 * K)]);
    b.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 1000_00)]);
    // Both members are on the scheme share: 30% of the premium, with 15% commission.
    let total = |b: &Bench, kind| charged(b, "S1", d(2027, 2, 1), kind, &north());
    assert_eq!(total(&b, ChargeKind::Premium), 300_00);
    assert_eq!(total(&b, ChargeKind::Commission), 45_00);
    // M1 is identified as anna: 50,000 kept, 650,000 facultative at 1.20 per thousand.
    b.feed(
        2,
        vec![Event::MemberIdentified {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            party: PartyId::from("anna"),
        }],
    );
    assert_eq!(total(&b, ChargeKind::Facultative), rated(650 * K, 1_200_000, 28));
    assert_eq!(total(&b, ChargeKind::Facultative), 59_84);
    assert_eq!(total(&b, ChargeKind::Premium), 90_00);
    assert_eq!(b.ri.booked(&group(), &north(), LineKind::FacultativePremium), 59_84);
    // A period billed without the premium by cover type cannot be priced on this basis.
    b.premium_due(2, "S1", 2, d(2027, 3, 1), d(2027, 4, 1), &[]);
    assert!(b.ri.exceptions().contains_key("unpriced:S1/2027-03-01"));
    assert_eq!(charged(&b, "S1", d(2027, 3, 1), ChargeKind::Premium, &north()), 0);
    assert_eq!(
        charged(&b, "S1", d(2027, 3, 1), ChargeKind::Facultative, &north()),
        rated(650 * K, 1_200_000, 31)
    );
    b.assert_reconciles();
}

/// Purpose: a policy that lapses part way through a billed period and is reinstated after
/// the period has ended is charged for the days before the lapse only; the cover that
/// resumes later adds nothing to the earlier period.
/// Why it matters: the cession record then has a stretch that starts after the period being
/// priced. Counting it, or counting its days as negative, would bill the reinsurer for a
/// time the policy was out of force or hand premium back that was properly ceded.
#[test]
fn cover_that_resumes_after_a_period_adds_nothing_to_it() {
    let mut b = anna();
    b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    let status = |from, to| Event::PolicyStatusChanged {
        policy: PolicyId::from("P1"),
        from,
        to,
    };
    b.today = d(2027, 2, 10);
    b.feed(1, vec![status(PolicyStatus::InForce, PolicyStatus::Lapsed)]);
    b.today = d(2027, 3, 5);
    let resumed = b.feed(1, vec![status(PolicyStatus::Lapsed, PolicyStatus::InForce)]);
    assert_eq!(b.ri.cession_record(&key("P1", "life", 1, "L1")).len(), 3);
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), rated(180 * K, 650_000, 9));
    assert!(
        resumed.instructions.iter().all(|i| !matches!(i.body, InstructionBody::Post { .. })),
        "reinstatement in March moves no money for February"
    );
    // March is billed from the day the cover resumed: the stretch that ended in February
    // is before the period and adds nothing to it either.
    b.premium_due(1, "P1", 2, d(2027, 3, 1), d(2027, 4, 1), &[]);
    assert_eq!(charged(&b, "P1", d(2027, 3, 1), ChargeKind::Premium, &north()), rated(180 * K, 650_000, 27));
    b.assert_reconciles();
}

/// Purpose: a cover that no treaty takes is kept in full and nothing is ceded or charged for
/// it when its premium falls due, while the cover beside it on the same policy is charged as
/// usual.
/// Why it matters: a policy often mixes covers that are reinsured with covers that are not.
/// The unreinsured one must neither stop the billing of the other nor be reported as a cover
/// that could not be priced, which would send people looking for a rate that does not exist.
#[test]
fn cover_under_no_treaty_is_charged_nothing() {
    let mut b = Bench::new();
    b.issue(
        1,
        "P1",
        "anna",
        vec![
            layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1)),
            layer("tpd", "tpd", 1, "L1", 300 * K, d(2027, 2, 1)),
        ],
    );
    b.factors("P1", "L1", d(1990, 3, 10), false);
    let tpd = b.ri.cession(&key("P1", "tpd", 1, "L1"), d(2027, 2, 1)).unwrap().clone();
    assert_eq!((tpd.treaty, tpd.retained_minor), (None, 300 * K));
    let applied = b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[]);
    assert!(b
        .ri
        .charged(&PolicyId::from("P1"), d(2027, 2, 1))
        .unwrap()
        .keys()
        .all(|k| k.risk.cover == "life"));
    assert_eq!(charged(&b, "P1", d(2027, 2, 1), ChargeKind::Premium, &north()), 8_98);
    assert!(
        !applied.notices.iter().any(|n| matches!(n, Notice::ExceptionOpened { .. })),
        "{:?}",
        applied.notices
    );
    assert!(b.ri.exceptions().keys().all(|k| !k.starts_with("unpriced:")));
    b.assert_reconciles();
}

/// Purpose: a reinsurer rate table that rates by sex prices a life whose sex it does not hold,
/// such as a life who is neither male nor female, at the lower of its female and male rates
/// for the life's other facts, or at a blend when the table says so. A life whose sex is
/// recorded gets its own row, a table that does not rate by sex is unaffected, and a life
/// the module knows nothing about is still not priced.
/// Why it matters: decision 1 in `docs/decisions.md` (persona test PRD-011). The customer is
/// rated on the lower rate, and the reinsurer must be paid on the same basis; before this the
/// cover was raised as "cannot be priced" and nothing was ceded in premium.
#[test]
fn a_life_whose_sex_the_table_does_not_hold_is_rated_at_the_lower_rate() {
    use ic_product::Sex;
    use ic_reinsurance::loader::load_rate_table;
    use ic_reinsurance::treaty::{RiskFactors, UnmatchedSex};
    let source = |unmatched: &str| {
        format!(
            r#"
RATE_TABLE = reinsurer_rate_table(
    id = "ri-sexed",
    version = 1,
    effective_from = "2027-01-01",
    rows = [
        reinsurer_rate("0.90", age_to = 39, sex = "female"),
        reinsurer_rate("1.20", age_to = 39, sex = "male"),
        reinsurer_rate("4.00", age_from = 40, sex = "female"),
        reinsurer_rate("3.00", age_from = 40, sex = "male"),
    ],
    {unmatched}
)
"#
        )
    };
    let on = d(2027, 3, 1);
    let life = |dob, sex| RiskFactors {
        date_of_birth: Some(dob),
        sex,
        smoker: Some(false),
    };
    let young = d(1995, 1, 1);
    let old = d(1970, 1, 1);

    let lower = load_rate_table("sexed.star", &source("")).unwrap();
    assert_eq!(lower.unmatched_sex, UnmatchedSex::LowerOf);
    assert_eq!(lower.rate(Some(&life(young, None)), on), Some(900_000), "female is lower under 40");
    assert_eq!(lower.rate(Some(&life(old, None)), on), Some(3_000_000), "male is lower from 40");
    assert_eq!(lower.rate(Some(&life(young, Some(Sex::Male))), on), Some(1_200_000));
    assert_eq!(lower.rate(Some(&life(old, Some(Sex::Female))), on), Some(4_000_000));
    assert_eq!(lower.rate(None, on), None, "a life with no facts is never guessed");
    let no_age = RiskFactors::default();
    assert_eq!(lower.rate(Some(&no_age), on), None, "nor one with no age");

    let blended = load_rate_table("sexed.star", &source("unmatched_sex = sex_blended(\"0.25\"),")).unwrap();
    assert_eq!(blended.unmatched_sex, UnmatchedSex::Blended { female_share: 250_000 });
    // A quarter of 0.90 and three quarters of 1.20.
    assert_eq!(blended.rate(Some(&life(young, None)), on), Some(1_125_000));
    let explicit = load_rate_table("sexed.star", &source("unmatched_sex = sex_lower_of(),")).unwrap();
    assert_eq!(explicit.rate(Some(&life(old, None)), on), Some(3_000_000));
    assert!(load_rate_table("sexed.star", &source("unmatched_sex = sex_blended(\"1.5\"),")).is_err());

    // The shipped table does not rate by sex: a life without one gets the same rate as before.
    let shipped = load_rate_table("ri_life_base.star", RATES).unwrap();
    assert_eq!(
        shipped.rate(Some(&life(young, None)), on),
        shipped.rate(Some(&life(young, Some(Sex::Female))), on)
    );
    // And the setting survives the module's own record of the table.
    let kept: ic_reinsurance::treaty::ReinsurerRateTable = serde_json::from_value(serde_json::to_value(&blended).unwrap()).unwrap();
    assert_eq!(kept, blended);
}
