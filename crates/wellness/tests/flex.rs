//! AIA's Premium Adjustment Rules as the module applies them, command by command.

use chrono::NaiveDate;
use ic_core::entitlement::SetEntitlement;
use ic_core::{LifeId, PolicyId};
use ic_log::StateMachine;
use ic_wellness::{
    anniversary, flex, whole_months, AnniversaryStatus, Command, CoreFact, Envelope, Status, WellnessState, CORE_EVENTS, CORE_OUTBOX, ELIGIBLE, PROGRAMME,
};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

struct World {
    s: WellnessState,
    index: u64,
    today: NaiveDate,
}

impl World {
    fn new() -> Self {
        World {
            s: WellnessState::new(),
            index: 0,
            today: d(2027, 3, 1),
        }
    }

    fn send(&mut self, caller: &str, command: Command) -> ic_wellness::Applied {
        self.index += 1;
        let key = format!("k{}", self.index);
        self.s.apply(
            self.index,
            Envelope {
                key,
                caller: caller.into(),
                today: self.today,
                command,
            },
        )
    }

    fn ok(&mut self, caller: &str, command: Command) -> Vec<String> {
        let applied = self.send(caller, command);
        assert_eq!(applied.result, Ok(()));
        applied.postings
    }

    fn err(&mut self, caller: &str, command: Command) -> String {
        let (members, outbox) = (self.s.members.clone(), self.s.outbox.clone());
        let code = self.send(caller, command).result.unwrap_err().code;
        assert!(self.s.members == members && self.s.outbox == outbox, "a refusal changed the state");
        code
    }

    fn issued(&mut self, policy: &str, product: &str, inception: NaiveDate) {
        let fact = CoreFact::Issued {
            policy: PolicyId::from(policy),
            product: product.into(),
            inception,
        };
        self.ok(CORE_EVENTS, Command::Core { fact });
    }

    fn join(&mut self, member: &str, policy: &str, joined: NaiveDate) -> Vec<String> {
        self.ok(
            PROGRAMME,
            Command::Join {
                member: member.into(),
                policy: PolicyId::from(policy),
                life: LifeId::from("life-1"),
                joined,
            },
        )
    }

    fn status(&mut self, member: &str, earned: Status, anniversary: Option<(NaiveDate, Status)>) {
        self.ok(
            PROGRAMME,
            Command::ReportStatus {
                member: member.into(),
                earned,
                anniversary: anniversary.map(|(vitality_anniversary, status)| AnniversaryStatus { vitality_anniversary, status }),
            },
        );
    }

    /// Open each day up to and including `to`.
    fn days_to(&mut self, to: NaiveDate) -> Vec<String> {
        let mut made = vec![];
        while self.today < to {
            self.today = self.today.succ_opt().unwrap();
            let today = self.today;
            made.extend(self.ok(
                CORE_EVENTS,
                Command::Core {
                    fact: CoreFact::DayOpened { today },
                },
            ));
        }
        made
    }

    fn sent(&self, posting: &str) -> SetEntitlement {
        match &self.s.outbox[posting].command {
            ic_core::Command::SetEntitlement(e) => (**e).clone(),
            other => panic!("not an entitlement: {other:?}"),
        }
    }

    fn discount(&self, member: &str) -> u32 {
        self.s.members[member].discount
    }
}

/// Purpose: the flex table moves a baseline by status: Platinum +2 up to 20%, Gold keeps it,
/// Silver −1 and Bronze −2 down to 0%; whole months and anniversaries count calendar months,
/// with 29 February falling on 28 February.
/// Why it matters: these are the numbers AIA publishes in its Premium Adjustment Rules. A
/// discount one point out on every member is a premium error on the whole book.
#[test]
fn the_flex_table_moves_the_baseline_by_status_within_its_limits() {
    assert_eq!(
        [Status::Platinum, Status::Gold, Status::Silver, Status::Bronze].map(|s| flex(10, s)),
        [12, 10, 9, 8]
    );
    assert_eq!((flex(19, Status::Platinum), flex(20, Status::Platinum)), (20, 20));
    assert_eq!((flex(1, Status::Bronze), flex(0, Status::Silver)), (0, 0));
    assert_eq!(whole_months(d(2027, 1, 31), d(2027, 2, 28)), 0);
    assert_eq!(whole_months(d(2027, 1, 15), d(2028, 1, 15)), 12);
    assert_eq!(anniversary(d(2028, 2, 29), 1), d(2029, 2, 28));
}

/// Purpose: a life on an eligible AIA product joins and is given the 10% Initial Discount from
/// the day they join, as one entitlement for the core on that life; a policy the core has not
/// issued, a product AIA excludes (Cover for Life, Essentials Life), a join before the policy
/// starts and a second join are refused, and only the programme may send membership.
/// Why it matters: the Initial Discount is the first thing a member sees on their premium. Given
/// to the wrong product or twice, the insurer discounts cover AIA's rules do not discount.
#[test]
fn a_member_joins_with_the_initial_discount_on_eligible_cover_only() {
    let mut w = World::new();
    assert!(ELIGIBLE.contains(&"aia-living") && !ELIGIBLE.contains(&"aia-cover-for-life"));
    w.issued("P1", "aia-living", d(2027, 1, 10));
    w.issued("P2", "aia-cover-for-life", d(2027, 1, 10));
    let join = |member: &str, policy: &str, joined| Command::Join {
        member: member.into(),
        policy: PolicyId::from(policy),
        life: LifeId::from("life-1"),
        joined,
    };
    assert_eq!(w.err(CORE_EVENTS, join("V1", "P1", d(2027, 2, 1))), "not_allowed");
    assert_eq!(w.err(PROGRAMME, join("V1", "P9", d(2027, 2, 1))), "unknown_policy");
    assert_eq!(w.err(PROGRAMME, join("V1", "P2", d(2027, 2, 1))), "not_eligible");
    assert_eq!(w.err(PROGRAMME, join("V1", "P1", d(2027, 1, 9))), "before_inception");
    let made = w.join("V1", "P1", d(2027, 2, 1));
    let sent = w.sent(&made[0]);
    assert_eq!(
        (sent.code.as_str(), sent.tier.as_deref(), sent.life.clone(), sent.held, sent.from),
        ("aia-vitality", Some("10"), Some(LifeId::from("life-1")), true, d(2027, 2, 1))
    );
    assert_eq!(w.err(PROGRAMME, join("V1", "P1", d(2027, 2, 2))), "already_member");
    assert_eq!(w.discount("V1"), 10);
}

/// Purpose: a member in their first ten months at the policy anniversary keeps 10% unless they
/// reached Platinum, which raises it to 12%; the decision is taken two weeks after the
/// anniversary and applies from the anniversary; nothing is sent when the discount stays.
/// Why it matters: AIA promises new members that Premium Flex will not work against them in
/// their first ten months. Applying the Bronze step to them breaks that promise.
#[test]
fn in_the_first_ten_months_the_discount_only_rises() {
    let mut w = World::new();
    w.issued("P1", "aia-living", d(2027, 1, 10));
    w.issued("P2", "aia-living", d(2027, 1, 10));
    w.join("V1", "P1", d(2027, 6, 1));
    w.join("V2", "P2", d(2027, 6, 1));
    w.status("V2", Status::Platinum, None);
    // The anniversary is 10 January 2028, seven months in; decided on 24 January.
    assert!(w.days_to(d(2028, 1, 23)).is_empty());
    let made = w.days_to(d(2028, 1, 24));
    assert_eq!(made.len(), 1, "only the Platinum member's discount changes");
    let sent = w.sent(&made[0]);
    assert_eq!((sent.tier.as_deref(), sent.from), (Some("12"), d(2028, 1, 10)));
    assert_eq!((w.discount("V1"), w.discount("V2")), (10, 12));
    assert_eq!(w.s.members["V1"].flexed[0].read, "earned");
    assert!(w.days_to(d(2028, 3, 1)).is_empty(), "an anniversary is flexed once");
}

/// Purpose: a member in months 11 and 12 at the anniversary is flexed on their Earned Status;
/// a member longer in the programme is flexed on the status they reached at their latest
/// Vitality anniversary, from the discount of the previous policy anniversary, year on year,
/// down to nothing on Bronze and up to 20% on Platinum; no status reported counts as Bronze.
/// Why it matters: which status is read, and from which baseline, is the whole of Premium Flex.
/// Reading the earned status for a long-standing member, or flexing from 10% each year, gives
/// every member the wrong discount after their first year.
#[test]
fn later_anniversaries_flex_the_baseline_on_the_status_reached() {
    let mut w = World::new();
    for p in ["P1", "P2", "P3"] {
        w.issued(p, "aia-living", d(2027, 1, 10));
    }
    // At the anniversary of 10 January 2028, V1 is 11 months in and V2 and V3 are 10 months
    // in: each is flexed on what they have earned.
    w.join("V1", "P1", d(2027, 2, 1));
    w.join("V2", "P2", d(2027, 3, 1));
    w.join("V3", "P3", d(2027, 3, 1));
    w.status("V1", Status::Silver, None);
    w.status("V2", Status::Gold, None);
    w.days_to(d(2028, 1, 24));
    assert_eq!((w.discount("V1"), w.discount("V2"), w.discount("V3")), (9, 10, 8));
    assert!(["V1", "V2", "V3"].iter().all(|m| w.s.members[*m].flexed[0].read == "earned"));

    // A year on, each is read on the status reached at their Vitality anniversary, from the
    // discount of the previous policy anniversary. V3 has reported nothing: Bronze.
    w.status("V1", Status::Bronze, Some((d(2028, 2, 1), Status::Silver)));
    w.status("V2", Status::Bronze, Some((d(2028, 3, 1), Status::Platinum)));
    w.days_to(d(2029, 1, 24));
    assert_eq!((w.discount("V1"), w.discount("V2"), w.discount("V3")), (8, 12, 6));
    let last = w.s.members["V2"].flexed.last().unwrap().clone();
    assert_eq!(
        (last.read.as_str(), last.baseline, last.status, last.anniversary),
        ("anniversary", 10, Status::Platinum, d(2029, 1, 10))
    );

    // A status from more than a year before the decision is not read again: Bronze.
    w.status("V2", Status::Bronze, Some((d(2029, 3, 1), Status::Platinum)));
    w.days_to(d(2030, 1, 24));
    assert_eq!((w.discount("V1"), w.discount("V2"), w.discount("V3")), (6, 14, 4));
    // Platinum every year reaches 20% and stays there; Bronze every year reaches nothing.
    for year in 2030..2036 {
        w.status("V2", Status::Bronze, Some((d(year, 3, 1), Status::Platinum)));
        w.days_to(d(year + 1, 1, 24));
    }
    assert_eq!((w.discount("V2"), w.discount("V3")), (20, 0));
}

/// Purpose: a member who leaves has the discount removed from the policy's next premium due
/// date, which the module learns from the core's bills, even a bill raised before it is due; a member who has left is no longer
/// flexed or reported on; a policy that has ended is not flexed; the core's answer to a posting
/// takes it out of the outbox, and a refusal is kept for operations.
/// Why it matters: AIA removes the discount from the next premium due date, not from the day
/// the programme hears of it. A removal dated in the past would claw back discount already
/// given; one never sent leaves a discount on cover whose holder is no longer a member.
#[test]
fn leaving_removes_the_discount_from_the_next_premium_due() {
    let mut w = World::new();
    w.issued("P1", "aia-living", d(2027, 1, 10));
    w.issued("P2", "aia-living", d(2027, 1, 10));
    let first = w.join("V1", "P1", d(2027, 2, 1));
    w.join("V2", "P2", d(2027, 2, 1));
    w.ok(
        CORE_EVENTS,
        Command::Core {
            fact: CoreFact::Billed {
                policy: PolicyId::from("P1"),
                period_start: d(2027, 3, 10),
                period_end: d(2027, 4, 10),
            },
        },
    );
    let made = w.ok(
        PROGRAMME,
        Command::Leave {
            member: "V1".into(),
            on: d(2027, 3, 1),
        },
    );
    let sent = w.sent(&made[0]);
    assert_eq!((sent.held, sent.tier.clone(), sent.from), (false, None, d(2027, 3, 10)));
    let report = Command::ReportStatus {
        member: "V1".into(),
        earned: Status::Gold,
        anniversary: None,
    };
    assert_eq!(w.err(PROGRAMME, report), "unknown_member");
    w.ok(
        CORE_EVENTS,
        Command::Core {
            fact: CoreFact::Ended { policy: PolicyId::from("P2") },
        },
    );
    assert!(w.days_to(d(2028, 2, 1)).is_empty(), "neither a member who left nor an ended policy is flexed");
    assert!(w.s.members["V1"].flexed.is_empty() && w.s.members["V2"].flexed.is_empty());

    w.ok(
        CORE_OUTBOX,
        Command::RecordCoreOutcome {
            posting: first[0].clone(),
            applied: true,
            code: None,
            message: None,
        },
    );
    w.ok(
        CORE_OUTBOX,
        Command::RecordCoreOutcome {
            posting: made[0].clone(),
            applied: false,
            code: Some("x".into()),
            message: Some("y".into()),
        },
    );
    assert_eq!(w.s.outbox.len(), 1);
    assert_eq!(w.s.refused[0].code, "x");
}
