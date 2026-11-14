//! Cover a scheme member pays for themselves: billed to the member, paid by
//! the member, and kept apart from the employer's invoice and balance.

use chrono::NaiveDate;
use ic_core::command::{ExcessLayer, MemberChange};
use ic_core::event::Event;
use ic_core::voluntary::credit_minor;
use ic_core::{accounts, Command, GroupId, LifeId, MemberPays, PolicyId, PolicyStatus, Reject, Standing};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, DecStr, Money};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member};
use ic_sim::{Cluster, Delivery};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 7);
    c.open(d(2027, 11, 1)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn exec() -> LifeId {
    LifeId("exec-1".into())
}

/// A scheme of two: staff-1 at 11.00 a month and exec-1 at 90.62.
fn scheme(c: &mut Cluster) -> (PartitionId, PolicyId) {
    let issue = group_scheme(
        20,
        d(2027, 11, 1),
        vec![
            member("staff-1", d(1985, 1, 1), "staff", 60_000_00),
            member("exec-1", d(1975, 1, 1), "executive", 240_000_00),
        ],
    );
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    (p, id)
}

/// 210,000 of cover above the automatic amount for exec-1: 25.38 a month at the scheme's rate.
fn layer(id: &PolicyId, loading: Option<&str>, member_pays: MemberPays) -> Command {
    Command::AddExcessLayer(Box::new(ExcessLayer {
        continues: None,
        policy: id.clone(),
        cover: "group-life".into(),
        life: exec(),
        start: d(2027, 11, 1),
        sum_insured_minor: 210_000_00,
        loadings: loading.map(|l| DecStr::parse(l).unwrap()).into_iter().collect(),
        exclusions: vec![],
        loading_itemised: false,
        member_pays,
        reference: "uw-1".into(),
    }))
}

fn code(r: Result<(), Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

fn events(c: &mut Cluster) -> Vec<Event> {
    c.take_events().into_iter().map(|r| r.event).collect()
}

fn employer_owes(c: &Cluster, p: PartitionId, id: &PolicyId) -> i64 {
    c.state(p).policies[id].policyholder_outstanding_minor()
}

fn member_owes(c: &Cluster, p: PartitionId, id: &PolicyId) -> i64 {
    c.state(p).policies[id].member_outstanding_minor(&exec())
}

fn credit(c: &Cluster, p: PartitionId, id: &PolicyId) -> i128 {
    credit_minor(&c.state(p).ledger, &c.state(p).policies[id], &exec())
}

fn member_receipt(id: &PolicyId, life: &LifeId, receipt: &str, minor: i64) -> Command {
    Command::ApplyMemberReceipt {
        policy: id.clone(),
        life: life.clone(),
        receipt: receipt.into(),
        amount: Money::new(minor, NZD),
    }
}

/// Purpose: a layer the member pays for is billed to the member and to nobody else. The
/// employer's invoice, balance and standing are as if the layer were not there; the member's
/// money is set only against the member's bill and the rest is held as the member's credit,
/// which pays the next bill; the employer's money never pays for the member; and ending the
/// layer credits the member for the days left.
/// Why it matters: voluntary cover is a contract between the member and the insurer inside
/// the employer's scheme. If its premium reached the employer's invoice the employer would
/// pay for, and see, something that is the member's own; if the member's money went to the
/// scheme's balance the member would be paying the employer's bill.
#[test]
fn cover_the_member_pays_for_is_billed_and_paid_apart_from_the_scheme() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    assert_eq!(employer_owes(&c, p, &id), 11_00 + 90_62);
    c.take_events();

    c.submit(p, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    assert_eq!(employer_owes(&c, p, &id), 11_00 + 90_62, "the employer owes nothing more");
    assert_eq!(member_owes(&c, p, &id), 25_38);
    let seen = events(&mut c);
    assert!(seen.iter().any(|e| matches!(e,
        Event::MemberPremiumBilled { life, total_minor: 25_38, adjustment: true, .. } if *life == exec())));
    assert!(!seen
        .iter()
        .any(|e| matches!(e, Event::PremiumAdjusted { .. } | Event::LoadingAdjusted { .. } | Event::PolicyBalance { .. })));

    // The employer pays its invoice and more: the rest is the scheme's, not the member's.
    let employer_pays = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "emp-1".into(),
        amount: Money::new(11_00 + 90_62 + 50_00, NZD),
    };
    c.submit(p, "cash-matching", employer_pays).unwrap();
    assert_eq!((employer_owes(&c, p, &id), member_owes(&c, p, &id)), (0, 25_38));
    let scheme_suspense = accounts::policy_suspense(&id, NZD).id;
    assert_eq!(c.state(p).ledger.natural_balance(&scheme_suspense), 50_00);
    days(&mut c, 3);
    assert_eq!(member_owes(&c, p, &id), 25_38, "the scheme's credit is not spent on the member");
    assert_eq!(
        c.state(p).policies[&id].standing,
        Some(Standing::Paid),
        "what the member owes is not the scheme's arrears"
    );

    // The member pays: who may send it, and what is refused.
    assert_eq!(
        code(c.submit(p, "group-admin", member_receipt(&id, &exec(), "m-1", 30_00))),
        "caller_not_allowed"
    );
    let stranger = LifeId("nobody".into());
    assert_eq!(
        code(c.submit(p, "cash-matching", member_receipt(&id, &stranger, "m-1", 30_00))),
        "unknown_member"
    );
    assert_eq!(code(c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-1", 0))), "invalid_amount");
    assert_eq!(
        code(c.submit(p, "cash-matching", member_receipt(&id, &exec(), "emp-1", 30_00))),
        "duplicate_receipt"
    );
    c.take_events();
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-1", 30_00)).unwrap();
    assert_eq!((member_owes(&c, p, &id), credit(&c, p, &id)), (0, 4_62));
    assert_eq!(c.state(p).ledger.natural_balance(&scheme_suspense), 50_00);
    assert!(events(&mut c).iter().any(|e| matches!(
        e,
        Event::MemberReceiptApplied {
            allocated_minor: 25_38,
            to_credit_minor: 4_62,
            ..
        }
    )));

    // December: the employer's invoice has the member at the automatic amount only, and the
    // member's own bill is met first from the member's credit.
    days(&mut c, 28);
    let seen = events(&mut c);
    let lines = seen.iter().rev().find_map(|e| match e {
        Event::SchemeInvoiceChunk { lines, loading_minor: 0, .. } => Some(lines.clone()),
        _ => None,
    });
    assert_eq!(lines.unwrap(), vec![(exec(), 90_62), (LifeId("staff-1".into()), 11_00)]);
    assert!(seen.iter().any(|e| matches!(e,
        Event::MemberPremiumBilled { total_minor: 25_38, adjustment: false, period_start, .. } if *period_start == d(2027, 12, 1))));
    assert!(seen.iter().any(|e| matches!(e, Event::MemberCreditApplied { amount_minor: 4_62, .. })));
    assert_eq!((member_owes(&c, p, &id), credit(&c, p, &id)), (25_38 - 4_62, 0));

    // The first payment's credit has been used, so it cannot simply be reversed.
    let reverse = |receipt: &str| Command::ReverseReceipt {
        policy: id.clone(),
        receipt: receipt.into(),
    };
    assert_eq!(code(c.submit(p, "cash-matching", reverse("m-1"))), "suspense_consumed");
    // A payment that only paid a bill reverses, and the member owes it again.
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-2", 20_76)).unwrap();
    assert_eq!(member_owes(&c, p, &id), 0);
    c.take_events();
    c.submit(p, "cash-matching", reverse("m-2")).unwrap();
    assert_eq!(member_owes(&c, p, &id), 20_76);
    assert!(events(&mut c)
        .iter()
        .any(|e| matches!(e, Event::MemberReceiptReversed { amount_minor: 20_76, .. })));
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-3", 20_76)).unwrap();

    // The layer ends part way through December: the member is credited for the days left,
    // and the credit can be paid back.
    let today = c.business_date().unwrap();
    let end = Command::EndExcessLayer {
        policy: id.clone(),
        cover: "group-life".into(),
        layer: 1,
        effective: today,
    };
    c.submit(p, "group-admin", end).unwrap();
    let back = credit(&c, p, &id);
    assert!(back > 0 && back < 25_38, "{back}");
    assert_eq!(member_owes(&c, p, &id), 0);
    let refund = |minor: i64| Command::RefundMemberCredit {
        policy: id.clone(),
        life: exec(),
        amount: Money::new(minor, NZD),
    };
    assert_eq!(code(c.submit(p, "policy-servicing", refund(back as i64 + 1))), "insufficient_credit");
    assert_eq!(code(c.submit(p, "policy-servicing", refund(0))), "invalid_amount");
    c.submit(p, "policy-servicing", refund(back as i64)).unwrap();
    assert_eq!(credit(&c, p, &id), 0);
    assert!(events(&mut c).iter().any(|e| matches!(e, Event::MemberCreditRefunded { .. })));
    days(&mut c, 35);
    assert_eq!(member_owes(&c, p, &id), 0, "nothing more is billed to the member");
}

/// Purpose: where the member pays only the loading on a layer, the employer is billed the
/// layer at the scheme's rate with no loading anywhere on its invoice, the member is billed
/// exactly the loading, and a layer with no loading cannot be set up this way.
/// Why it matters: this is the rule for schemes where too few members carry a loading for
/// a total to be anonymous. Any loading left in the employer's total would tell the
/// employer about one member's health.
#[test]
fn a_loading_the_member_pays_never_reaches_the_employer() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    assert_eq!(code(c.submit(p, "group-admin", layer(&id, None, MemberPays::Loading))), "invalid_payer");
    c.take_events();
    c.submit(p, "group-admin", layer(&id, Some("0.5"), MemberPays::Loading)).unwrap();
    // The layer at the scheme's rate is 25.38 a month; half as much again is the loading.
    assert_eq!(employer_owes(&c, p, &id), 11_00 + 116_00);
    assert_eq!(member_owes(&c, p, &id), 12_69);
    assert!(!events(&mut c).iter().any(|e| matches!(e, Event::LoadingAdjusted { .. })));
    days(&mut c, 31);
    let seen = events(&mut c);
    let invoice = seen.iter().rev().find_map(|e| match e {
        Event::SchemeInvoiceChunk { lines, loading_minor, .. } => Some((lines.clone(), *loading_minor)),
        _ => None,
    });
    assert_eq!(invoice.unwrap(), (vec![(exec(), 116_00), (LifeId("staff-1".into()), 11_00)], 0));
    assert_eq!(member_owes(&c, p, &id), 2 * 12_69);
}

/// Purpose: when the scheme itself ends, what a member paid ahead for their own cover goes
/// back to the member's credit and not to the employer; what the member still owed is
/// written off.
/// Why it matters: a cancellation refunds unearned premium to whoever paid it. Paying a
/// member's money to the employer is the insurer giving away money that belongs to a
/// named person.
#[test]
fn a_members_money_stays_the_members_when_the_scheme_ends() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    c.submit(p, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-1", 30_00)).unwrap();
    assert_eq!(credit(&c, p, &id), 4_62);

    days(&mut c, 10);
    c.submit(p, "group-admin", Command::CancelPolicy { policy: id.clone() }).unwrap();
    let policy = &c.state(p).policies[&id];
    assert_eq!(policy.status, PolicyStatus::Cancelled);
    assert_eq!(policy.outstanding_minor(), 0);
    // The member paid November in full and had 10 of its 30 days: about two thirds comes back.
    let back = credit(&c, p, &id) - 4_62;
    assert!((16_00..=17_50).contains(&back), "{back}");
    // The employer paid nothing, so nothing of the member's is due to the employer: the
    // scheme's suspense holds at most the cent a cancellation's rounding can leave.
    assert!(c.state(p).ledger.natural_balance(&accounts::policy_suspense(&id, NZD).id) <= 1);
    let refund = Command::RefundMemberCredit {
        policy: id.clone(),
        life: exec(),
        amount: Money::new(credit(&c, p, &id) as i64, NZD),
    };
    c.submit(p, "policy-servicing", refund).unwrap();
    days(&mut c, 3);
}

/// Purpose: a member billed for cover of their own has their own standing. Paying late
/// inside the grace period restores it; a member who does not pay within the product's
/// lapse period loses the cover they pay for and only that, with what they owed written
/// off; and through all of it the scheme's standing, the employer's invoice and the
/// member's employer-paid cover are untouched.
/// Why it matters: the design gives voluntary cover its own standing, separate from the
/// employer's. If one member's missed payment put the scheme into arrears the employer
/// would be chased, and in the end every member's cover lapsed, for a debt that is not
/// the employer's; and if unpaid voluntary cover never lapsed the insurer would be on risk
/// for cover nobody is paying for.
#[test]
fn a_member_who_does_not_pay_loses_only_the_cover_they_pay_for() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    c.submit(p, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    // The employer pays a year ahead, so the scheme is never in arrears.
    let employer_pays = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "emp-1".into(),
        amount: Money::new(12 * (11_00 + 90_62), NZD),
    };
    c.submit(p, "cash-matching", employer_pays).unwrap();
    let standing = |c: &Cluster| c.state(p).policies[&id].member_standing.get(&exec()).copied();

    days(&mut c, 2);
    assert_eq!(
        standing(&c),
        Some(Standing::InGrace),
        "the member's first bill was due on the day the layer was added"
    );
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-1", 25_38)).unwrap();
    assert_eq!(standing(&c), Some(Standing::Paid));
    c.take_events();

    // December's bill and every one after it go unpaid.
    days(&mut c, 29 + 119);
    assert_eq!(standing(&c), Some(Standing::InGrace), "one day short of the lapse period");
    assert_eq!(c.state(p).policies[&id].covers["group-life"].layers[0].end, None);
    let owed = member_owes(&c, p, &id);
    assert!(owed >= 4 * 25_38, "{owed}");
    days(&mut c, 2);

    let policy = c.state(p).policies[&id].clone();
    let today = c.business_date().unwrap();
    assert_eq!(standing(&c), None, "the member has no cover of their own left to stand on");
    assert_eq!(member_owes(&c, p, &id), 0);
    let ended = policy.covers["group-life"].layers[0].end.expect("the member's layer has ended");
    assert!(ended < today);
    assert_eq!((policy.status, policy.standing), (PolicyStatus::InForce, Some(Standing::Paid)));
    assert!(policy.lives[&exec()].in_force_on(today), "the member is still a member");
    let seen = events(&mut c);
    let lapsed = seen.iter().find_map(|e| match e {
        Event::MemberCoverLapsed {
            life,
            layers,
            effective,
            arrears_minor,
            ..
        } if *life == exec() => Some((layers.clone(), *effective, *arrears_minor)),
        _ => None,
    });
    let (layers, effective, arrears) = lapsed.expect("the lapse is announced");
    assert_eq!((layers, effective), (vec![("group-life".to_string(), 1)], ended));
    assert!(arrears > 0 && arrears <= owed, "{arrears} of {owed}");
    assert!(seen.iter().any(|e| matches!(e, Event::LayerEnded { layer: 1, life, .. } if *life == exec())));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::MemberStandingChanged { to: None, life, .. } if *life == exec())));
    assert!(
        seen.iter().any(|e| matches!(e,
            Event::PaymentAuthorityCancelled { claim, life: Some(l), .. } if claim == "member_lapsed" && *l == exec())),
        "payments ends the member's own collection"
    );
    assert!(
        !seen.iter().any(|e| matches!(e, Event::PaymentAuthorityCancelled { life: None, .. })),
        "the employer's collection is not touched"
    );
    assert!(
        !seen.iter().any(|e| matches!(e, Event::StandingChanged { .. })),
        "the scheme's standing never moved"
    );
    assert_eq!(credit(&c, p, &id), 0, "nothing was paid ahead, so nothing comes back");

    // The next invoice still has the member at the automatic amount, and nothing more is
    // billed to the member.
    days(&mut c, 31);
    let seen = events(&mut c);
    let lines = seen.iter().rev().find_map(|e| match e {
        Event::SchemeInvoiceChunk { lines, .. } => Some(lines.clone()),
        _ => None,
    });
    assert_eq!(lines.unwrap(), vec![(exec(), 90_62), (LifeId("staff-1".into()), 11_00)]);
    assert!(!seen.iter().any(|e| matches!(e, Event::MemberPremiumBilled { .. })));
}

/// Purpose: a member who pays for cover of their own and leaves the scheme owing nothing
/// has their own collection ended at once (`PaymentAuthorityCancelled` naming the life,
/// reason `member_left`); one who leaves owing is not, because what they owe can still
/// be collected, and their collection ends only when the arrears run writes it off.
/// Why it matters: payments asked for the life on this event because without it the
/// only signal was the payer's death, which ends every authority on the policy, the
/// employer's included. A member who has gone must not go on being debited, and one who
/// owes must not have the authority that would pay it cancelled first.
#[test]
fn a_member_who_leaves_has_only_their_own_collection_ended() {
    let ended = |seen: &[Event], reason: &str| {
        seen.iter().any(|e| {
            matches!(e,
            Event::PaymentAuthorityCancelled { claim, life: Some(l), .. } if claim == reason && *l == exec())
        })
    };
    // Paid up, then gone.
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    c.submit(p, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-1", 25_38)).unwrap();
    days(&mut c, 2);
    c.take_events();
    let leave = Command::ApplyMembership {
        policy: id.clone(),
        changes: vec![MemberChange::Left {
            life: exec(),
            effective: d(2027, 11, 3),
        }],
    };
    c.submit(p, "group-admin", leave.clone()).unwrap();
    let seen = events(&mut c);
    assert!(ended(&seen, "member_left"), "{seen:?}");
    assert!(!seen.iter().any(|e| matches!(e, Event::PaymentAuthorityCancelled { life: None, .. })));

    // Billed, unpaid, then gone: collection carries on until the debt is written off.
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    c.submit(p, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    // The employer pays a year ahead, so the scheme itself never lapses.
    let employer_pays = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "emp-1".into(),
        amount: Money::new(12 * (11_00 + 90_62), NZD),
    };
    c.submit(p, "cash-matching", employer_pays).unwrap();
    days(&mut c, 2);
    c.take_events();
    c.submit(p, "group-admin", leave).unwrap();
    assert!(member_owes(&c, p, &id) > 0, "what was covered before leaving is still owed");
    let seen = events(&mut c);
    assert!(!seen.iter().any(|e| matches!(e, Event::PaymentAuthorityCancelled { .. })), "{seen:?}");
    days(&mut c, 150);
    assert_eq!(member_owes(&c, p, &id), 0);
    assert!(ended(&events(&mut c), "member_lapsed"));
}

/// Purpose: a member whose own cover lapsed for non-payment is reinstated inside the
/// product's 90-day window once their credit holds what the lapse wrote off and the
/// premium for the periods since: the cover is in force again from the day it ended,
/// those periods are billed to the member and paid from the credit, and the member is
/// paid up. Short of that sum it is refused `arrears_unpaid` with nothing changed; a
/// member with no lapse is refused `not_lapsed`; after the window, `window_closed`; with no
/// reference, for someone not a member, or on a scheme no longer in force, likewise refused.
/// Why it matters: a retail policy can be reinstated by paying what it owes, and the
/// design gives a member's own cover the same standing as a policy of its own. Without
/// this a member who paid a week late would have to apply and be underwritten again,
/// and the insurer could not take the money it is owed; and a reinstatement that left a
/// gap, or did not charge for it, would give cover nobody paid for.
#[test]
fn a_lapsed_members_cover_is_reinstated_when_they_pay_what_it_needs() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    c.submit(p, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    let employer_pays = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "emp-1".into(),
        amount: Money::new(12 * (11_00 + 90_62), NZD),
    };
    c.submit(p, "cash-matching", employer_pays).unwrap();
    let reinstate = |life: LifeId| Command::ReinstateMemberCover {
        policy: id.clone(),
        life,
        reference: "rein-1".into(),
    };
    assert_eq!(code(c.submit(p, "group-admin", reinstate(exec()))), "not_lapsed");
    days(&mut c, 152);
    let lapsed = c.state(p).policies[&id].member_lapses[&exec()].clone();
    assert!(lapsed.arrears_minor > 0);
    assert!(c.state(p).policies[&id].covers["group-life"].layers[0].end.is_some());
    days(&mut c, 20);
    c.take_events();

    // A part payment is not enough, and changes nothing.
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-1", lapsed.arrears_minor)).unwrap();
    let before = c.state(p).policies[&id].clone();
    assert_eq!(code(c.submit(p, "group-admin", reinstate(exec()))), "arrears_unpaid");
    assert_eq!(c.state(p).policies[&id], before);

    // The arrears and the month since the lapse, paid: cover again with no gap.
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-2", 2 * 25_38)).unwrap();
    let paid = lapsed.arrears_minor + 2 * 25_38;
    c.take_events();
    c.submit(p, "group-admin", reinstate(exec())).unwrap();
    let policy = c.state(p).policies[&id].clone();
    assert_eq!(policy.covers["group-life"].layers[0].end, None, "in force again from the day it ended");
    assert!(policy.member_lapses.is_empty());
    assert_eq!(member_owes(&c, p, &id), 0, "the credit paid everything");
    assert_eq!(policy.member_standing.get(&exec()).copied(), Some(Standing::Paid));
    let seen = events(&mut c);
    let billed = seen
        .iter()
        .find_map(|e| match e {
            Event::MemberCoverReinstated {
                life,
                layers,
                effective,
                arrears_minor,
                billed_minor,
                ..
            } if *life == exec() => {
                assert_eq!((layers, *effective, *arrears_minor), (&lapsed.layers, lapsed.lapsed_on, lapsed.arrears_minor));
                Some(*billed_minor)
            }
            _ => None,
        })
        .expect("the reinstatement is announced");
    assert!(billed > 0, "the periods since the lapse are charged");
    assert_eq!(
        credit(&c, p, &id),
        i128::from(paid - lapsed.arrears_minor - billed),
        "the rest stays the member's"
    );
    assert!(seen.iter().any(|e| matches!(e, Event::LayerAdded { .. })));

    // The member's cover carries on being billed and paid as before.
    days(&mut c, 31);
    assert!(c.state(p).policies[&id].covers["group-life"].layers[0].end.is_none());

    // A second lapse, left past the window, cannot be reinstated.
    days(&mut c, 160);
    assert!(c.state(p).policies[&id].member_lapses.contains_key(&exec()));
    days(&mut c, 91);
    c.submit(p, "cash-matching", member_receipt(&id, &exec(), "m-3", 1_000_00)).unwrap();
    assert_eq!(code(c.submit(p, "group-admin", reinstate(exec()))), "window_closed");
    let unnamed = Command::ReinstateMemberCover {
        policy: id.clone(),
        life: exec(),
        reference: " ".into(),
    };
    assert_eq!(code(c.submit(p, "group-admin", unnamed)), "invalid_request");
    assert_eq!(code(c.submit(p, "group-admin", reinstate(LifeId("nobody".into())))), "unknown_member");
    c.submit(p, "group-admin", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(code(c.submit(p, "group-admin", reinstate(exec()))), "not_in_force");
}

/// Purpose: a scheme holding a member's own money moves to another partition with that
/// money as the member's, in an account of the member's own on the target, and the
/// source keeps none of it; the money still meets the member's next bill there, and the
/// partitions reconcile.
/// Why it matters: the transfer used to be refused while any member held credit, so an
/// employer's change of group could wait on one member's few dollars. Moving the money
/// as the scheme's would hand a named person's money to the employer; leaving it behind
/// would strand it on a partition the policy no longer lives on.
#[test]
fn a_members_credit_moves_with_the_scheme_to_another_partition() {
    let mut c = Cluster::in_memory(2, 7);
    c.open(d(2027, 11, 1)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let (src, id) = scheme(&mut c);
    let dst = 1 - src;
    c.submit(src, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    c.submit(src, "cash-matching", member_receipt(&id, &exec(), "m-1", 30_00)).unwrap();
    assert_eq!(credit(&c, src, &id), 4_62);
    let target_group = GroupId(
        (0..)
            .map(|i| format!("grp-new-{i}"))
            .find(|g| c.partition_of(&GroupId(g.clone())) == dst)
            .unwrap(),
    );
    let transfer = Command::BeginTransfer {
        policy: id.clone(),
        transfer_id: "t-1".into(),
        target_group,
        target_partition: dst,
    };
    c.submit(src, "transfer", transfer).unwrap();
    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    assert_eq!(c.state(src).policies[&id].status, PolicyStatus::Transferred);
    let member_account = accounts::member_suspense(&id, &exec(), NZD).id;
    assert_eq!(c.state(src).ledger.natural_balance(&member_account), 0, "nothing of the member's stays behind");
    assert_eq!(credit(&c, dst, &id), 4_62, "the member's money is the member's on the target");
    assert_eq!(
        c.state(dst).ledger.natural_balance(&accounts::policy_suspense(&id, NZD).id),
        0,
        "and not the scheme's"
    );
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());

    // December's bill is met first from the credit that moved.
    days(&mut c, 30);
    let owes = c.state(dst).policies[&id].member_outstanding_minor(&exec());
    assert_eq!(owes, 25_38 - 4_62);
    assert_eq!(credit(&c, dst, &id), 0);
}

/// Purpose: a member's own cover does not lapse before the earliest effect of the lapse
/// notice documents last proved delivered to them (purpose `member_lapse:<life>`): past
/// the lapse period it is held until that day and lapses on it.
/// Why it matters: a policy's lapse already waits for its proven notice, because cover
/// must not end before the person was told it would. A member paying for their own cover
/// is owed the same warning; without it the member's cover could end on a notice that
/// reached them late, or not at all.
#[test]
fn a_members_cover_waits_for_their_proven_lapse_notice() {
    let mut c = cluster();
    let (p, id) = scheme(&mut c);
    c.submit(p, "group-admin", layer(&id, None, MemberPays::All)).unwrap();
    let employer_pays = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "emp-1".into(),
        amount: Money::new(12 * (11_00 + 90_62), NZD),
    };
    c.submit(p, "cash-matching", employer_pays).unwrap();
    days(&mut c, 100);
    let today = c.business_date().unwrap();
    let notice = Command::RecordNoticeDelivery {
        policy: id.clone(),
        purpose: ic_core::voluntary::member_lapse_notice(&exec()),
        notice: "doc-77".into(),
        delivered_on: today,
        earliest_effect: today + chrono::Days::new(40),
    };
    c.submit(p, "documents", notice).unwrap();
    let ended = |c: &Cluster| c.state(p).policies[&id].covers["group-life"].layers[0].end;
    // The lapse period runs out about 22 days from here; the notice holds it to day 40.
    days(&mut c, 39);
    assert_eq!(ended(&c), None, "held by the notice");
    assert_eq!(c.state(p).policies[&id].member_standing.get(&exec()).copied(), Some(Standing::InGrace));
    days(&mut c, 2);
    assert_eq!(ended(&c), Some(today + chrono::Days::new(40)), "lapses on the notice's earliest effect");
}
