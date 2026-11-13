//! Leaving the scheme: the continuation option and its take-up, and a leave
//! of absence that runs past what the scheme allows.

mod common;

use chrono::Days;
use common::*;
use ic_core::{GroupId, PolicyId, Reject};
use ic_group::command::{caller, Envelope, GroupCommand, Resolution};
use ic_group::event::GroupEvent;
use ic_group::mapping::FileKind;
use ic_group::record::RowChange;
use ic_group::state::OfferStatus;
use ic_group::terms::CoverEnd;
use ic_product::Frequency;

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn take_up(h: &mut Harness, offer: u64, n: u32, product: &str, minor: i64) -> Result<(), Reject> {
    let r = h.send_as(
        caller::MEMBER,
        "party-E002",
        GroupCommand::TakeUpContinuation {
            scheme: SCHEME.into(),
            offer,
            policy: PolicyId(format!("pol-cont-{n}")),
            number: format!("WPL9000000{n}"),
            group: GroupId(format!("grp-cont-{n}")),
            product_code: product.into(),
            cover: "life".into(),
            frequency: Frequency::Monthly,
            sum_insured_minor: minor,
        },
    );
    h.settle();
    r
}

fn offer_for(h: &Harness, id: &str) -> Option<u64> {
    h.scheme().offers.values().find(|o| o.employee_id == id).map(|o| o.id)
}

/// Purpose: a leaver is offered an individual policy for up to the cover they held, and
/// taking it up issues that policy in the core to the member, starting the day scheme
/// cover ended, with no question about health; each way of asking for more than the
/// option gives is refused.
/// Why it matters: the continuation option is a contractual right with a deadline, and it
/// exists so that someone who has become uninsurable while employed is not left without
/// cover. A gap of one day between the two policies, or a policy in the wrong name,
/// defeats it.
#[test]
fn a_leaver_can_continue_their_cover_as_an_individual_policy() {
    let mut h = Harness::new(terms());
    let mut old = attrs("staff", 70_000_00, "akl");
    old.date_of_birth = Some(d(1960, 1, 1));
    let mut known = attrs("staff", 60_000_00, "wlg");
    known.sex = Some(ic_product::Sex::Female);
    known.smoker = Some(false);
    h.upload(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", known.clone()),
            row(3, "E003", attrs("staff", 90_000_00, "akl")),
            row(4, "E005", old.clone()),
        ],
    )
    .unwrap();
    h.approve("f1").unwrap();
    h.resolve("f1", |id| {
        (id == "E003").then(|| Resolution::Possible {
            candidates: vec![party("party-x")],
        })
    });
    h.days(20);
    let left = h.today();
    h.apply(
        "f2",
        FileKind::Changes,
        vec![
            change(1, "E002", RowChange::Leave, left, known),
            change(2, "E003", RowChange::Leave, left, attrs("staff", 90_000_00, "akl")),
            change(3, "E005", RowChange::Leave, left, old),
        ],
    );
    assert_eq!(offer_for(&h, "E005"), None, "no option at or above the scheme's age limit");
    let offer = offer_for(&h, "E002").unwrap();
    let o = &h.scheme().offers[&offer];
    assert_eq!((o.left, o.expires, o.max_sum_insured_minor), (left, left + Days::new(60), 120_000_00));
    assert!(h.saw(|e| matches!(e, GroupEvent::ContinuationOffered { .. })));
    // E003 was never identified: the possible match is closed with the membership, and
    // there is nobody to issue a policy to.
    assert!(h.scheme().possible.is_empty());
    let unidentified = offer_for(&h, "E003").unwrap();
    assert_eq!(code(take_up(&mut h, unidentified, 1, "term-life", 100_000_00)), "not_identified");

    h.days(5);
    assert_eq!(code(take_up(&mut h, 999, 1, "term-life", 100_000_00)), "offer_closed");
    assert_eq!(code(take_up(&mut h, offer, 1, "health-plan", 100_000_00)), "product_not_offered");
    assert_eq!(code(take_up(&mut h, offer, 1, "term-life", 0)), "invalid_amount");
    assert_eq!(code(take_up(&mut h, offer, 1, "term-life", 120_000_01)), "invalid_amount");

    // The core refuses the first attempt: the member is told, and the option is still theirs.
    h.core_faults.push_back(Some("duplicate_policy".into()));
    take_up(&mut h, offer, 1, "term-life", 100_000_00).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::ContinuationRefused { code, .. } if code == "duplicate_policy")));
    assert_eq!(h.scheme().offers[&offer].status, OfferStatus::Open);
    assert!(
        h.scheme().blocked.is_none(),
        "a refusal of the member's new policy says nothing about the scheme"
    );

    take_up(&mut h, offer, 2, "term-life", 100_000_00).unwrap();
    let new_policy = PolicyId("pol-cont-2".into());
    assert_eq!(h.scheme().offers[&offer].status, OfferStatus::TakenUp { policy: new_policy.clone() });
    assert!(h.saw(|e| matches!(e, GroupEvent::ContinuationTakenUp { new_policy: p, .. } if *p == new_policy)));
    let issued = &h.c.state(h.p).policies[&new_policy];
    assert_eq!(issued.inception, left, "cover continues from the day scheme cover ended");
    assert_eq!(issued.policyholder, party("party-E002"));
    // The new policy names the scheme policy and the member it carries on from, for claims.
    let from = issued.continuation_of.as_ref().expect("the link to the scheme");
    assert_eq!((&from.policy, &from.life), (&h.policy, &life("E002")));
    assert_eq!(code(take_up(&mut h, offer, 3, "term-life", 100_000_00)), "offer_closed");
}

/// Purpose: an option not taken up lapses the day after its last day, and cannot be taken
/// up late even if the request arrives before the morning's housekeeping has run.
/// Why it matters: the take-up period is what makes cover without evidence of health
/// affordable. An option that could be exercised late is an option to buy cover after
/// the diagnosis.
#[test]
fn an_option_not_taken_up_expires() {
    let mut h = Harness::new(terms());
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        ],
    );
    let left = h.today();
    h.apply(
        "f2",
        FileKind::Changes,
        vec![change(1, "E002", RowChange::Leave, left, attrs("staff", 60_000_00, "wlg"))],
    );
    let offer = offer_for(&h, "E002").unwrap();

    // A request dated after the last day, arriving before that day's tick.
    let late = Envelope {
        key: "late-1".into(),
        caller: caller::MEMBER.into(),
        actor: "party-E002".into(),
        today: left + Days::new(61),
        command: GroupCommand::TakeUpContinuation {
            scheme: SCHEME.into(),
            offer,
            policy: PolicyId("pol-cont-1".into()),
            number: "WPL90000001".into(),
            group: GroupId("grp-cont-1".into()),
            product_code: "term-life".into(),
            cover: "life".into(),
            frequency: Frequency::Monthly,
            sum_insured_minor: 100_000_00,
        },
    };
    let mut copy = h.g.clone();
    assert_eq!(copy.apply_envelope(late).result.unwrap_err().code, "offer_expired");

    h.days(60);
    assert_eq!(h.scheme().offers[&offer].status, OfferStatus::Open, "the last day is still inside the period");
    h.days(1);
    assert_eq!(h.scheme().offers[&offer].status, OfferStatus::Expired);
    assert!(h.saw(|e| matches!(e, GroupEvent::ContinuationExpired { .. })));
    assert_eq!(code(take_up(&mut h, offer, 1, "term-life", 100_000_00)), "offer_closed");
}

/// Purpose: a member on leave of absence past the scheme's limit is taken off cover on the
/// date the terms give, once, with notice published; a member who comes back in time is not.
/// Why it matters: employers forget to report that someone on long leave has gone. The
/// limit is in the policy wording, so cover past it is cover the reinsurer will not share.
#[test]
fn absence_past_the_limit_ends_cover_on_the_terms_date() {
    let mut t = terms();
    t.cover_end = CoverEnd::EndOfMonth;
    t.leave_of_absence_days = 30;
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
    let start = h.today();
    h.apply(
        "f2",
        FileKind::Changes,
        vec![
            change(1, "E002", RowChange::AbsenceStart, start, attrs("staff", 60_000_00, "wlg")),
            change(2, "E003", RowChange::AbsenceStart, start, attrs("staff", 90_000_00, "akl")),
        ],
    );
    assert_eq!(h.scheme().membership.active("E002").unwrap().absence_since, Some(start));
    assert_eq!(h.sent.len(), 1, "an absence is the scheme's business: the core is not told");
    h.days(10);
    let back = h.today();
    h.apply(
        "f3",
        FileKind::Changes,
        vec![change(1, "E003", RowChange::AbsenceEnd, back, attrs("staff", 90_000_00, "akl"))],
    );
    assert_eq!(h.scheme().membership.active("E003").unwrap().absence_since, None);

    // 1 November plus 30 days is 1 December; cover runs to the end of that month.
    h.days(19);
    assert!(!h.saw(|e| matches!(e, GroupEvent::AbsenceLimitReached { .. })));
    h.days(1);
    assert!(h.saw(|e| matches!(e, GroupEvent::AbsenceLimitReached { cover_ends, .. } if *cover_ends == d(2028, 1, 1))));
    assert!(h.scheme().membership.active("E002").is_some());
    h.days(31);
    assert_eq!(h.today(), d(2028, 1, 1));
    assert!(h.scheme().membership.active("E002").is_none());
    assert_eq!(h.scheme().membership.members["E002"].left, Some(d(2028, 1, 1)));
    assert_eq!(h.core_lives(), vec!["E001", "E003"]);
    let notices = h.published.iter().filter(|e| matches!(e, GroupEvent::AbsenceLimitReached { .. })).count();
    assert_eq!(notices, 1, "said once, not every morning");
    assert!(h.scheme().absence_leaves.is_empty());
}

/// Purpose: a part-timer whose hours cross the scheme's minimum from week to week is
/// covered exactly while she qualifies. The week her hours fall short her cover ends in
/// the core from that date and she is offered continuation like any leaver; the week
/// they rise again she rejoins from that date as a new life; and a change that keeps her
/// above the minimum is only a change. Her colleagues are untouched throughout.
/// Why it matters: eligibility is a term of the policy. Cover that stayed on through the
/// short weeks would be cover the scheme was not priced for and a claim would be argued
/// over; cover that did not come back would leave a working member uninsured while her
/// employer believes she is covered.
#[test]
fn cover_follows_a_member_whose_hours_cross_the_minimum() {
    let mut t = terms();
    t.eligibility.min_hours_hundredths = Some(2000);
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
    assert_eq!(h.core_lives().len(), 3);
    let with_hours = |hundredths: u32| {
        let mut a = attrs("staff", 60_000_00, "wlg");
        a.hours_hundredths = Some(hundredths);
        a
    };

    // Week one: still enough hours. A change, nothing more.
    h.days(7);
    let today = h.today();
    h.apply("w1", FileKind::Changes, vec![change(1, "E002", RowChange::Change, today, with_hours(2500))]);
    assert_eq!(h.core_lives().len(), 3);

    // Week two: below the minimum. Cover ends that day and she may continue it herself.
    h.days(7);
    let short_from = h.today();
    h.apply(
        "w2",
        FileKind::Changes,
        vec![change(1, "E002", RowChange::Change, short_from, with_hours(1500))],
    );
    assert!(h.scheme().membership.active("E002").is_none());
    assert_eq!(h.core_lives(), vec!["E001".to_string(), "E003".to_string()]);
    assert!(h.scheme().offers.values().any(|o| o.employee_id == "E002" && o.status == OfferStatus::Open));
    assert_eq!(h.scheme().files["w2"].rejected, vec![]);

    // Week three: enough hours again. She joins from that day; while she was out a change
    // for her is refused, because she is not a member.
    h.days(7);
    let back_from = h.today();
    h.apply(
        "w3a",
        FileKind::Changes,
        vec![change(1, "E002", RowChange::Change, back_from, with_hours(3000))],
    );
    assert_eq!(h.scheme().files["w3a"].rejected[0].code, "unknown_member");
    h.apply("w3b", FileKind::Changes, vec![change(1, "E002", RowChange::Join, back_from, with_hours(3000))]);
    let again = h.scheme().membership.active("E002").expect("she is a member again");
    assert_eq!(again.joined, back_from);
    assert_eq!(h.core_lives().len(), 3);
    // The days in between are not covered: the first life ended and a new one began.
    let lives = &h.c.state(h.p).policies[&h.policy].lives;
    let hers: Vec<_> = lives.values().filter(|l| l.id.0.starts_with("E002")).map(|l| (l.joined, l.left)).collect();
    assert_eq!(hers, vec![(d(2027, 11, 1), Some(short_from)), (back_from, None)]);
}

/// Purpose: a seasonal worker leaves at the end of the season and is hired again months
/// later. Leaving ends cover in the core from that day and opens the continuation option;
/// coming back with a new employment start, the scheme's waiting period runs again from it
/// and cover starts the day it ends, as a new life; a colleague who comes back with their
/// original start date is covered from the day of the file.
/// Why it matters: seasonal employers send the same people in and out every year. Cover
/// that started the day they came back would ignore a term the scheme was priced on, and
/// cover dated from a waiting period served years ago would backdate it over months when
/// they were not employed.
#[test]
fn a_seasonal_worker_serves_the_waiting_period_again_on_rejoining() {
    let mut t = terms();
    t.eligibility.waiting_days = 30;
    let mut h = Harness::new(t);
    let wait = |h: &mut Harness, days: u32| {
        for _ in 0..days {
            h.days(1);
            if h.owed() > 0 {
                h.pay(h.owed());
            }
        }
    };
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 60_000_00, "wlg")),
            row(3, "E003", attrs("staff", 90_000_00, "akl")),
        ],
    );
    assert_eq!(h.core_lives().len(), 3);
    let first_life = h.scheme().membership.active("E002").unwrap().life.clone();

    // The season ends for two of them.
    wait(&mut h, 10);
    let left = h.today();
    h.apply(
        "f2",
        FileKind::Changes,
        vec![
            change(1, "E002", RowChange::Leave, left, attrs("staff", 60_000_00, "wlg")),
            change(2, "E003", RowChange::Leave, left, attrs("staff", 90_000_00, "akl")),
        ],
    );
    assert_eq!(h.core_lives(), vec!["E001".to_string()]);
    assert_eq!(h.scheme().membership.members["E002"].left, Some(left));
    assert_eq!(
        h.scheme().offers.values().filter(|o| o.status == OfferStatus::Open && o.left == left).count(),
        2
    );

    // Next season: one is hired again from today, the other is shown with the old start date.
    wait(&mut h, 60);
    let back = h.today();
    let mut rehired = attrs("staff", 60_000_00, "wlg");
    rehired.employment_start = Some(back);
    // Neither row states a date: the scheme's rules decide when cover starts.
    let undated = |n, id: &str, a| ic_group::record::MemberRow {
        effective: None,
        ..change(n, id, RowChange::Join, back, a)
    };
    h.apply(
        "f3",
        FileKind::Changes,
        vec![undated(1, "E002", rehired), undated(2, "E003", attrs("staff", 90_000_00, "akl"))],
    );
    assert_eq!(h.scheme().files["f3"].rejected, vec![]);
    assert_eq!(h.scheme().membership.active("E003").unwrap().joined, back);
    assert!(h.scheme().membership.active("E002").is_none(), "the waiting period has not run");
    let due = back + chrono::Days::new(30);
    assert_eq!(h.scheme().scheduled.keys().map(|(date, _)| *date).collect::<Vec<_>>(), vec![due]);

    // The day comes.
    wait(&mut h, 30);
    let again = h.scheme().membership.active("E002").expect("covered once the waiting period has run");
    assert_eq!(again.joined, due);
    assert_ne!(again.life, first_life, "a new spell of cover, the first kept as it was");
    assert_eq!(h.core_lives().len(), 3);
}
