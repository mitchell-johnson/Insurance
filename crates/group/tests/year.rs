//! The scheme year: renewal agreed, guaranteed or not agreed, ending a
//! scheme, and the declarations of a pool scheme.

mod common;

use common::*;
use ic_core::Reject;
use ic_group::command::{caller, GroupCommand};
use ic_group::event::{ExceptionKind, GroupEvent, ReinsuranceNotice};
use ic_group::mapping::FileKind;
use ic_group::state::SchemeStatus;
use ic_group::terms::{MemberBasis, NoAgreement, SchemeTerms};

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

/// Terms whose year ends on 1 February 2028, with renewal opening 30 days before.
fn short_year() -> SchemeTerms {
    let mut t = terms();
    t.renewal_date = d(2028, 2, 1);
    t.renewal_lead_days = 30;
    t
}

fn next_year(from: &SchemeTerms) -> SchemeTerms {
    let mut t = from.clone();
    t.scheme_year_start = from.renewal_date;
    t.renewal_date = d(2029, 2, 1);
    t.safety_share_percent = 25;
    t
}

fn with_members(t: SchemeTerms) -> Harness {
    let mut h = Harness::new(t);
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 60_000_00, "wlg")),
            row(3, "E003", attrs("staff", 90_000_00, "akl")),
        ],
    );
    h
}

/// Move to a date, paying what is owed each week so the scheme stays in force.
fn go_to(h: &mut Harness, date: chrono::NaiveDate) {
    while h.today() < date {
        h.days(1);
        if h.owed() > 0 {
            h.pay(h.owed());
        }
    }
}

fn agree(h: &mut Harness, t: SchemeTerms) -> Result<(), Reject> {
    h.staff(GroupCommand::AgreeRenewal {
        scheme: SCHEME.into(),
        terms: Box::new(t),
        rate_table: "group-life-2028".into(),
    })
}

/// Purpose: renewal opens at the lead time with the membership it will be priced on,
/// agreed terms take effect on the renewal date and not before, and reinsurance is told
/// the new terms.
/// Why it matters: terms that took effect when agreed would change the rules for files
/// in the old year; terms that never took effect would leave the scheme on last year's
/// agreement while being billed on this year's rates.
#[test]
fn agreed_terms_take_effect_on_the_renewal_date() {
    let mut h = with_members(short_year());
    let new_terms = next_year(&short_year());
    assert_eq!(code(agree(&mut h, new_terms.clone())), "renewal_not_open");
    go_to(&mut h, d(2028, 1, 1));
    assert!(h.scheme().renewal.is_none());
    go_to(&mut h, d(2028, 1, 2));
    assert!(h.saw(|e| matches!(e, GroupEvent::RenewalOpened { members: 3, .. })));

    let mut bad = new_terms.clone();
    bad.safety_share_percent = 0;
    assert_eq!(code(agree(&mut h, bad)), "invalid_terms");
    let mut wrong_start = new_terms.clone();
    wrong_start.scheme_year_start = d(2028, 3, 1);
    assert_eq!(code(agree(&mut h, wrong_start)), "renewal_dates");
    agree(&mut h, new_terms.clone()).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::RenewalAgreed { terms_version: 2, .. })));
    assert_eq!(h.scheme().terms.safety_share_percent, 20, "not until the day");

    go_to(&mut h, d(2028, 2, 1));
    assert_eq!(h.scheme().terms, new_terms);
    assert_eq!(h.scheme().terms_version, 2);
    assert!(h.scheme().renewal.is_none());
    assert!(h.saw(|e| matches!(e, GroupEvent::SchemeRenewed { scheme_year_start, terms_version: 2, .. } if *scheme_year_start == d(2028, 2, 1))));
    assert_eq!(h.notices.iter().filter(|n| matches!(n, ReinsuranceNotice::SetSchemeTerms { .. })).count(), 2);
    assert_eq!(h.scheme().status, SchemeStatus::Active);
    assert_eq!(h.core_lives().len(), 3);
}

/// Purpose: while rates are guaranteed past the renewal date nobody can agree new terms
/// and the year rolls over by itself on the same terms.
/// Why it matters: a rate guarantee is a promise in the contract. A renewal that could be
/// repriced inside it breaks the promise; one that waited for an agreement nobody needs
/// would end the scheme.
#[test]
fn a_rate_guarantee_rolls_the_year_without_an_agreement() {
    let mut t = short_year();
    t.rate_guarantee_until = Some(d(2029, 1, 31));
    let mut h = with_members(t.clone());
    go_to(&mut h, d(2028, 1, 5));
    assert_eq!(code(agree(&mut h, next_year(&t))), "rate_guarantee");
    go_to(&mut h, d(2028, 2, 1));
    assert_eq!(
        (h.scheme().terms.scheme_year_start, h.scheme().terms.renewal_date),
        (d(2028, 2, 1), d(2029, 2, 1))
    );
    assert_eq!(h.scheme().terms.safety_share_percent, 20);
    assert_eq!(h.scheme().terms_version, 2);
    assert_eq!(h.scheme().status, SchemeStatus::Active);
}

/// Purpose: with no agreement by the renewal date the scheme runs on its old rates for
/// the days the terms allow, raised to staff once; an agreement inside that time renews
/// it, and without one every member is taken off cover with a continuation option and
/// the policy is cancelled.
/// Why it matters: ending a scheme ends cover for every employee. It must happen only on
/// the terms' own timetable, visibly, and each member must still get their option.
#[test]
fn without_an_agreement_the_scheme_runs_on_old_rates_and_then_ends() {
    let mut h = with_members(short_year());
    go_to(&mut h, d(2028, 2, 3));
    assert!(h.saw(|e| matches!(e, GroupEvent::RenewalRunningOnOldRates { until, .. } if *until == d(2028, 3, 2))));
    let raised = h.published.iter().filter(|e| matches!(e, GroupEvent::RenewalRunningOnOldRates { .. })).count();
    assert_eq!(raised, 1);
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::RenewalNotAgreed));
    // Agreed late, inside the time allowed: renewed the next morning.
    let mut late = with_members(short_year());
    go_to(&mut late, d(2028, 2, 10));
    agree(&mut late, next_year(&short_year())).unwrap();
    assert!(!late.g.exceptions.values().any(|e| e.kind == ExceptionKind::RenewalNotAgreed));
    go_to(&mut late, d(2028, 2, 11));
    assert_eq!(late.scheme().terms_version, 2);
    assert_eq!(late.scheme().status, SchemeStatus::Active);

    // Never agreed: it ends on 2 March.
    go_to(&mut h, d(2028, 3, 1));
    assert_eq!(h.scheme().status, SchemeStatus::Active);
    go_to(&mut h, d(2028, 3, 2));
    assert_eq!(h.scheme().status, SchemeStatus::Terminated { effective: d(2028, 3, 2) });
    assert!(h.saw(|e| matches!(e, GroupEvent::TerminationStarted { reason, .. } if reason == "renewal_not_agreed")));
    assert!(h.saw(|e| matches!(e, GroupEvent::SchemeTerminated { .. })));
    assert_eq!(h.scheme().membership.active_count(), 0);
    assert!(h.core_lives().is_empty());
    assert_eq!(h.scheme().offers.len(), 3, "every member gets a continuation option");
    assert!(matches!(h.sent.last().unwrap().1, ic_core::Command::CancelPolicy { .. }));
    // An ended scheme takes nothing more, and mornings pass quietly.
    let receive = GroupCommand::ReceiveFile {
        scheme: SCHEME.into(),
        file: "f9".into(),
        content_hash: "h9".into(),
    };
    assert_eq!(code(h.employer(receive)), "scheme_ending");
    assert_eq!(code(agree(&mut h, next_year(&short_year()))), "renewal_not_open");
    let sent = h.sent.len();
    h.days(3);
    assert_eq!(h.sent.len(), sent);

    // Terms that end the scheme outright do so on the renewal date.
    let mut t = short_year();
    t.no_agreement = NoAgreement::End;
    let mut h = with_members(t);
    go_to(&mut h, d(2028, 2, 1));
    assert_eq!(h.scheme().status, SchemeStatus::Terminated { effective: d(2028, 2, 1) });
}

/// Purpose: staff can end a scheme from today or a later date; until that date nothing
/// changes for members, and on it each leaves and the policy is cancelled.
/// Why it matters: an employer gives notice ahead. Ending cover early is uninsured
/// employees; ending it late is premium the employer does not owe.
#[test]
fn staff_end_a_scheme_from_a_date() {
    let mut h = with_members(terms());
    h.days(5);
    let today = h.today();
    let end = today + chrono::Days::new(10);
    let terminate = |effective| GroupCommand::Terminate {
        scheme: SCHEME.into(),
        effective,
        reason: "employer_notice".into(),
    };
    assert_eq!(code(h.send_as(caller::EMPLOYER, "admin-bo", terminate(end))), "caller_not_allowed");
    assert_eq!(code(h.staff(terminate(today - chrono::Days::new(1)))), "past_date");
    h.staff(terminate(end)).unwrap();
    assert_eq!(h.scheme().status, SchemeStatus::Terminating { effective: end });
    assert_eq!(code(h.staff(terminate(end))), "scheme_ending");
    assert_eq!(h.core_lives().len(), 3);
    go_to(&mut h, end - chrono::Days::new(1));
    assert_eq!(h.core_lives().len(), 3);
    go_to(&mut h, end);
    assert_eq!(h.scheme().status, SchemeStatus::Terminated { effective: end });
    assert!(h.core_lives().is_empty());
    assert!(h.scheme().membership.members.values().all(|m| m.left == Some(end)));
}

/// Purpose: a pool scheme takes declarations of headcount and total salary in place of
/// files, each replaces the last from its date, a first declaration that is late is raised
/// to staff, a malformed one is refused, and none is taken once the scheme is ending. Only
/// group administration adds a pool; a total salary averaging over $1,000,000 a head is
/// refused in dollars; and a pool declared by mistake is ended with a headcount of nought,
/// keeping the scheme's last pool.
/// Why it matters: a pool is billed on what was declared. A declaration that was lost, or
/// that silently did not replace the one before, misprices every month until the next. Found
/// by the scenario run, SCN-07 (F-SCN07-3, F-SCN07-4): a salary entered a hundred times too
/// large was billed, and a mistyped pool name became a new insured pool that could not be ended.
#[test]
fn a_pool_scheme_is_run_on_declarations() {
    let mut t = terms();
    t.basis = MemberBasis::Pool;
    let mut h = Harness::new(t);
    let declare = |h: &mut Harness, pool: &str, category: &str, effective, headcount, total| {
        h.employer(GroupCommand::DeclarePool {
            scheme: SCHEME.into(),
            pool: pool.into(),
            category: category.into(),
            effective,
            headcount,
            total_salary_minor: total,
        })
    };
    let receive = GroupCommand::ReceiveFile {
        scheme: SCHEME.into(),
        file: "f1".into(),
        content_hash: "h1".into(),
    };
    assert_eq!(code(h.employer(receive)), "pool_scheme");
    let start = h.today();
    assert_eq!(
        code(declare(&mut h, "", "staff", start, 10, 1)),
        "unknown_pool",
        "a pool that is not the scheme's"
    );
    let unnamed = GroupCommand::DeclarePool {
        scheme: SCHEME.into(),
        pool: String::new(),
        category: "staff".into(),
        effective: start,
        headcount: 10,
        total_salary_minor: 1,
    };
    assert_eq!(code(h.staff(unnamed)), "unknown_value", "staff add pools, each with a name");
    assert_eq!(code(declare(&mut h, "seed", "pilots", start, 10, 1)), "unknown_value");
    assert_eq!(code(declare(&mut h, "seed", "staff", start, 0, 1)), "invalid_amount");
    assert_eq!(code(declare(&mut h, "seed", "staff", start, 10, 0)), "invalid_amount");
    assert_eq!(code(declare(&mut h, "seed", "staff", start + chrono::Days::new(1), 10, 1)), "backdating_limit");
    assert_eq!(code(declare(&mut h, "seed", "staff", start - chrono::Days::new(1), 10, 1)), "backdating_limit");

    // Thirty days pass with nothing declared.
    h.days(30);
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::DeclarationOverdue));
    h.days(1);
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::DeclarationOverdue));

    declare(&mut h, "seed", "staff", start, 40, 2_400_000_00).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::PoolDeclared { headcount: 40, .. })));
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::DeclarationOverdue));
    let pool = |h: &Harness, id: &str| {
        let l = &h.c.state(h.p).policies[&h.policy].lives[&life(id)];
        l.factors.as_of(h.today()).unwrap().pool.map(|p| (p.headcount, p.total_salary_minor))
    };
    assert_eq!(pool(&h, "pool:seed"), Some((40, 2_400_000_00)));

    // A second pool joins the policy, added by group administration, and the first is restated.
    let today = h.today();
    let drivers = GroupCommand::DeclarePool {
        scheme: SCHEME.into(),
        pool: "drivers".into(),
        category: "staff".into(),
        effective: today,
        headcount: 12,
        total_salary_minor: 700_000_00,
    };
    let (why, words) = h.employer(drivers.clone()).map_err(|r| (r.code, r.message)).unwrap_err();
    assert_eq!(why, "unknown_pool", "a pool the scheme does not have is not the employer's to add");
    assert!(words.contains("drivers is not one of this scheme's pools (seed)"), "{words}");
    h.staff(drivers).unwrap();
    declare(&mut h, "seed", "staff", today, 42, 2_500_000_00).unwrap();
    assert_eq!(pool(&h, "pool:drivers"), Some((12, 700_000_00)));
    assert_eq!(pool(&h, "pool:seed"), Some((42, 2_500_000_00)));
    assert_eq!(h.scheme().pools.len(), 2);

    // A salary typed a hundred times too large is refused, in dollars, before it is billed.
    let (why, words) = declare(&mut h, "seed", "staff", today, 140, 78_400_000_000_00)
        .map_err(|r| (r.code, r.message))
        .unwrap_err();
    assert_eq!(why, "implausible_salary");
    assert!(
        words.starts_with("a total salary of $78,400,000,000.00 for 140 people is more than $1,000,000.00 a head"),
        "{words}"
    );
    assert_eq!(pool(&h, "pool:seed"), Some((42, 2_500_000_00)));

    // A pool declared by mistake is ended with a headcount and salary of nought; the last one is kept.
    declare(&mut h, "drivers", "staff", today, 0, 0).unwrap();
    assert_eq!(h.scheme().pools.keys().collect::<Vec<_>>(), vec!["seed"]);
    let drivers_life = &h.c.state(h.p).policies[&h.policy].lives[&life("pool:drivers")];
    assert!(drivers_life.left.is_some(), "the core ended the mistaken pool");
    assert!(h.saw(|e| matches!(e, GroupEvent::PoolDeclared { headcount: 0, .. })));
    assert_eq!(code(declare(&mut h, "seed", "staff", today, 0, 0)), "last_pool");
    assert_eq!(code(declare(&mut h, "seed", "staff", today, 0, 5)), "invalid_amount");

    // A named scheme has nothing to declare.
    let mut named = Harness::new(terms());
    let today = named.today();
    assert_eq!(code(declare(&mut named, "seed", "staff", today, 10, 1)), "not_a_pool_scheme");

    // Once the scheme is ending, nothing more is declared.
    let today = h.today();
    h.staff(GroupCommand::Terminate {
        scheme: SCHEME.into(),
        effective: today + chrono::Days::new(30),
        reason: "the employer gave notice".into(),
    })
    .unwrap();
    assert_eq!(code(declare(&mut h, "seed", "staff", today, 43, 2_600_000_00)), "scheme_ending");
    assert_eq!(pool(&h, "pool:seed"), Some((42, 2_500_000_00)));
}

/// Purpose: a pool scheme whose declaration is overdue has its renewal held: terms cannot
/// be agreed while the timer's exception is open, and can be once the declaration is in.
/// Why it matters: a pool is priced on the headcount and salaries it declares. Agreeing a
/// year's rates on a declaration that never came prices the scheme on last year's numbers,
/// and the insurer has given away its only lever to get the figures.
#[test]
fn a_pools_renewal_waits_for_an_overdue_declaration() {
    let mut t = short_year();
    t.basis = MemberBasis::Pool;
    let mut h = Harness::new(t.clone());
    go_to(&mut h, d(2028, 1, 2));
    assert!(h.scheme().renewal.is_some());
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::DeclarationOverdue));
    let next = next_year(&t);
    assert_eq!(code(agree(&mut h, next.clone())), "declaration_overdue");
    assert!(h.scheme().renewal.as_ref().unwrap().agreed.is_none());

    let today = h.today();
    h.employer(GroupCommand::DeclarePool {
        scheme: SCHEME.into(),
        pool: "seed".into(),
        category: "staff".into(),
        effective: today,
        headcount: 40,
        total_salary_minor: 2_400_000_00,
    })
    .unwrap();
    agree(&mut h, next).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::RenewalAgreed { .. })));
}
