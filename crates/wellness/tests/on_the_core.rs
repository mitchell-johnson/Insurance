//! The wellness module against the real core: the core's events carried
//! in as the host carries them, the module's postings carried to the core,
//! and the AIA Vitality Discount read off the bills.

use chrono::NaiveDate;
use ic_core::command::{CoverSpec, IssuePolicy};
use ic_core::event::Event;
use ic_core::{Command as CoreCommand, GroupId, LifeId, PartyId, PolicyId};
use ic_log::StateMachine;
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_raft::module::{CoreOutcome, CorePosting, Route};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, named_life};
use ic_sim::Cluster;
use ic_wellness::{core_envelope, outcome_command, react, AnniversaryStatus, Command, Envelope, Status, WellnessState, PROGRAMME};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

struct Bench {
    c: Cluster,
    w: WellnessState,
    index: u64,
    /// The last core event carried to the module.
    seen: u64,
}

impl Bench {
    fn new() -> Self {
        let mut c = Cluster::in_memory(1, 42);
        c.open(start()).unwrap();
        for cmd in shipped_activations().unwrap() {
            c.activate_everywhere(cmd).unwrap();
        }
        Bench {
            c,
            w: WellnessState::new(),
            index: 0,
            seen: 0,
        }
    }

    fn apply(&mut self, env: Envelope) -> ic_wellness::Applied {
        self.index += 1;
        self.w.apply(self.index, env)
    }

    /// What the host does: the core's new events to the module, then the module's outbox to the
    /// core, then each outcome back to the module.
    fn carry(&mut self) {
        let events: Vec<_> = self.c.nodes[&0].outbox.iter().filter(|e| e.seq > self.seen).cloned().collect();
        for record in events {
            self.seen = record.seq;
            for env in react(&record) {
                let applied = self.apply(env);
                assert_eq!(applied.result, Ok(()));
            }
        }
        let postings: Vec<_> = self.w.outbox.values().cloned().collect();
        for p in postings {
            let envelope = core_envelope(&p, "salt".into());
            let applied = self.c.submit_keyed(0, &envelope.key, &envelope.caller, envelope.command.clone());
            let outcome = match applied.result {
                Ok(()) => CoreOutcome::Applied {
                    partition: 0,
                    log_index: applied.index,
                    duplicate: applied.duplicate,
                },
                Err(r) => CoreOutcome::Refused(r),
            };
            let posting = CorePosting {
                id: p.id.clone(),
                route: Route::Policy(p.policy.clone()),
                envelope,
            };
            let today = self.c.business_date().unwrap();
            let applied = self.apply(outcome_command(&posting, &outcome, today));
            assert_eq!(applied.result, Ok(()));
        }
        assert!(self.w.refused.is_empty(), "the core refused: {:?}", self.w.refused);
    }

    /// Close days to `to`, the customer paying what is owed each morning, carrying between the
    /// core and the module each day.
    fn days_to(&mut self, policy: &PolicyId, to: NaiveDate) {
        while self.c.business_date().unwrap() < to {
            let owing = self.c.state(0).policies[policy].outstanding_minor();
            if owing > 0 {
                let pay = CoreCommand::ApplyReceipt {
                    policy: policy.clone(),
                    receipt: format!("r-{}", self.c.business_date().unwrap()),
                    amount: Money::new(owing, NZD),
                };
                self.c.submit(0, "cash-matching", pay).unwrap();
            }
            let close = self.c.end_of_day().unwrap();
            assert!(close.breaks.is_empty(), "{:?}", close.breaks);
            self.carry();
        }
    }

    fn programme(&mut self, command: Command) {
        let today = self.c.business_date().unwrap();
        let key = format!("vitality-{}", self.index);
        let applied = self.apply(Envelope {
            key,
            caller: PROGRAMME.into(),
            today,
            command,
        });
        assert_eq!(applied.result, Ok(()));
        self.carry();
    }

    /// Each bill: (period start, premium before discounts, the Vitality discount on it), with
    /// any later adjustment of the period's discount added in.
    fn bills(&self, policy: &PolicyId) -> Vec<(NaiveDate, i64, i64)> {
        let vitality = |discounts: &[ic_core::event::DiscountGiven]| -> i64 { discounts.iter().filter(|g| g.code == "vitality").map(|g| g.amount_minor).sum() };
        let mut bills = std::collections::BTreeMap::new();
        for e in &self.c.nodes[&0].outbox {
            match &e.event {
                Event::PremiumDue {
                    policy: p,
                    receivable,
                    period_start,
                    by_cover_type,
                    discounts,
                    ..
                } if p == policy => {
                    let given: i64 = discounts.iter().map(|g| g.amount_minor).sum();
                    bills.insert(*receivable, (*period_start, by_cover_type.values().sum::<i64>() + given, vitality(discounts)));
                }
                // An adjustment is a receivable of its own, dated from the first day it reaches.
                Event::PremiumAdjusted {
                    policy: p,
                    effective,
                    discounts,
                    ..
                } if p == policy => {
                    if let Some(bill) = bills.values_mut().rev().find(|b: &&mut (NaiveDate, i64, i64)| b.0 <= *effective) {
                        bill.2 += vitality(discounts);
                    }
                }
                _ => {}
            }
        }
        bills.into_values().collect()
    }
}

/// Within a cent of `percent` of `gross`.
fn is_percent(vitality: i64, gross: i64, percent: i64) -> bool {
    (vitality * 100 - gross * percent).abs() <= 100
}

/// Purpose: an AIA Living policy's life joins AIA Vitality on the day the policy starts; the
/// module's 10% Initial Discount reaches the core and is on every monthly bill of the first
/// year, the bill raised at issue by adjustment; the life reaches Platinum at their Vitality anniversary, and two weeks after the policy
/// anniversary the module raises the discount to 12%, applied from the anniversary, which is on
/// every bill after it; when they leave, the bills after the next premium due carry none.
/// Why it matters: this is the AIA Vitality Discount as a customer sees it. The products declare
/// the discount, the core applies what it is told, and only this module tells it; if any hand-off
/// failed the discount would be missing from bills or stay on them after the member left.
#[test]
fn the_vitality_discount_is_on_the_bill_and_flexes_at_the_anniversary() {
    let mut b = Bench::new();
    let party = "party-00000001";
    let policy = PolicyId::from("pol-00000001");
    let issue = IssuePolicy {
        continuation_of: None,
        policy: policy.clone(),
        number: "AIA00000001".into(),
        group: GroupId::from("grp-00000001"),
        policyholder: PartyId::from(party),
        product_code: "aia-living".into(),
        inception: start(),
        frequency: Frequency::Monthly,
        lives: vec![named_life("life-1", party, d(1987, 4, 1), Sex::Female, false)],
        covers: vec![CoverSpec {
            excess_option: None,
            code: "life".into(),
            life: Some(LifeId::from("life-1")),
            sum_insured_minor: 500_000_00,
            loadings: vec![],
            exclusions: vec![],
        }],
        migrated: false,
    };
    // The first choice on each axis of the life cover's menu.
    let product = b.c.state(0).catalog.product_on("aia-living", start()).unwrap().clone();
    let axes = &product.cover("life").unwrap().option_axes;
    let parts: Vec<(&str, &str)> = axes.iter().map(|a| (a.code.as_str(), a.choices[0].code.as_str())).collect();
    let mut issue = issue;
    issue.covers[0].excess_option = Some(ic_product::option_code(&parts));
    b.c.submit(0, "new-business", issue_command(issue)).unwrap();
    b.carry();
    b.programme(Command::Join {
        member: "V1".into(),
        policy: policy.clone(),
        life: LifeId::from("life-1"),
        joined: start(),
    });

    let anniversary = d(2028, 11, 15);
    b.days_to(&policy, anniversary);
    // The first bill was raised at issue, before the life joined: the core adjusted it.
    let first_year: Vec<_> = b.bills(&policy).into_iter().filter(|(from, _, _)| *from < anniversary).collect();
    assert_eq!(first_year.len(), 12, "{first_year:?}");
    assert!(first_year.iter().all(|(_, gross, v)| is_percent(*v, *gross, 10)), "{first_year:?}");

    // The member reached Platinum at their Vitality anniversary, on the policy anniversary.
    b.programme(Command::ReportStatus {
        member: "V1".into(),
        earned: Status::Bronze,
        anniversary: Some(AnniversaryStatus {
            vitality_anniversary: anniversary,
            status: Status::Platinum,
        }),
    });
    b.days_to(&policy, d(2029, 3, 1));
    assert_eq!(b.w.members["V1"].discount, 12);
    let held = &b.c.state(0).policies[&policy].entitlements;
    let latest = held.iter().filter(|e| e.code == "aia-vitality").max_by_key(|e| e.from).unwrap();
    assert_eq!((latest.tier.as_str(), latest.from), ("12", anniversary), "applied from the anniversary");
    let later: Vec<_> = b.bills(&policy).into_iter().filter(|(from, _, _)| *from >= anniversary).collect();
    assert!(!later.is_empty() && later.iter().all(|(_, gross, v)| is_percent(*v, *gross, 12)), "{later:?}");

    b.programme(Command::Leave {
        member: "V1".into(),
        on: d(2029, 3, 1),
    });
    b.days_to(&policy, d(2029, 6, 1));
    let after: Vec<_> = b.bills(&policy).into_iter().filter(|(from, _, _)| *from >= d(2029, 3, 15)).collect();
    assert!(!after.is_empty() && after.iter().all(|(_, _, v)| *v == 0), "{after:?}");
}
