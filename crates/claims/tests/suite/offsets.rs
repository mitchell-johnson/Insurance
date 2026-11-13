//! Offsets across partitions: an overpayment held on one partition is
//! recovered from the same person's benefit paid on another.

use ic_claims::model::{EndReason, RecoveryStatus, TaskKind};
use ic_claims::{Command, Instruction, Request};

use crate::world::*;

/// A partition of a deployment with `count` partitions.
fn partition(p: u16, count: u16) -> World {
    let mut w = World::new();
    w.core.partition = p;
    let mut r = rules();
    r.version = 2;
    r.partitions = count;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w
}

/// Hand every message `from` has queued for `to` over, as the relay would.
fn deliver(from: &mut World, to: &mut World) -> usize {
    let here = to.core.partition;
    let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut from.core.sent).into_iter().partition(|(p, _)| *p == here);
    from.core.sent = rest;
    let n = mine.len();
    for (_, command) in mine {
        to.ok(command);
    }
    n
}

/// On `w`: ip-1 paid for March then ended on 17 March, leaving 1,451
/// overpaid, which sam puts into offset at 10%.
fn overpaid(w: &mut World) -> String {
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    let end = Request::EndBenefit {
        end: d(2027, 3, 17),
        reason: EndReason::Recovered,
    };
    w.decided("ip-1", "ana", end, "R-OK");
    let op = w.claim("ip-1").overpayments[0].id.clone();
    let offset = Request::RecoveryStatus {
        overpayment: op.clone(),
        status: RecoveryStatus::Offset,
        offset_pct: 10,
        instalment_minor: 0,
    };
    w.decided("ip-1", "sam", offset, "R-OK");
    op
}

/// Purpose: an overpayment in offset on partition 0 is shared with partition 1, the person's
/// benefit there keeps back the agreed share into offset in transit, and partition 0 takes it
/// up against the receivable and shares the new position, once however often the messages come.
/// Why it matters: policies sit on different partitions, so without this a person who owes on
/// one policy is paid in full on another while the insurer chases them for the debt.
#[test]
fn an_overpayment_on_one_partition_is_recovered_from_a_benefit_on_another() {
    let mut a = partition(0, 2);
    let mut b = partition(1, 2);
    let op = overpaid(&mut a);
    assert_eq!(a.claim("ip-1").overpayments[0].outstanding_minor(), 1_451);
    assert!(matches!(
        &a.core.sent[..],
        [(
            1,
            Command::OffsetSource {
                outstanding_minor: 1_451,
                pct: 10,
                seq: 1,
                ..
            }
        )]
    ));
    let source = a.core.sent[0].1.clone();
    assert_eq!(deliver(&mut a, &mut b), 1);
    // The same position again changes nothing.
    b.ok(source);

    b.benefit_in_payment("ip-9");
    b.pay_run("run-9", d(2027, 3, 1));
    let Some(Instruction::PayBenefit { offset: Some(o), .. }) = b.core.instructions.last().cloned() else {
        panic!("the benefit was paid without the offset");
    };
    assert_eq!(
        (o.claim.as_str(), o.overpayment.as_str(), o.amount_minor, o.partition),
        ("ip-1", op.as_str(), 300, Some(0))
    );
    let taken = b.core.sent[0].1.clone();
    assert_eq!(deliver(&mut b, &mut a), 1);
    assert!(matches!(a.core.instructions.last(), Some(Instruction::ReceiveOffset { amount_minor: 300, .. })));
    let o = &a.claim("ip-1").overpayments[0];
    assert_eq!((o.recovered_minor, o.remote_received[&1]), (300, 300));

    // Delivered twice: nothing is taken up twice.
    let instructions = a.core.instructions.len();
    a.ok(taken);
    assert_eq!(a.core.instructions.len(), instructions);

    // The new position reaches partition 1, which can keep back what is still owed.
    assert!(matches!(
        &a.core.sent[..],
        [(
            1,
            Command::OffsetSource {
                outstanding_minor: 1_151,
                seq: 2,
                settled_minor: 300,
                ..
            }
        )]
    ));
    deliver(&mut a, &mut b);
    let remote = b.state.remote_offsets.values().next().expect("shared").clone();
    assert_eq!((remote.sent_minor, remote.settled_minor, remote.available_minor()), (300, 300, 1_151));
}

/// Purpose: when two partitions keep back more between them than is still owed, the holder
/// applies what is owed and sends the rest back; the paying partition owes it to the claimant
/// again with their next benefit, and a take for a claim the holder does not have goes back
/// whole.
/// Why it matters: each partition only knows the position as last shared, so overtaking is
/// possible. The excess is the claimant's money and must reach them, not sit in transit.
#[test]
fn what_is_kept_back_beyond_the_debt_goes_back_to_the_claimant() {
    let mut a = partition(0, 3);
    let mut b = partition(1, 3);
    let op = overpaid(&mut a);
    a.core.sent.clear();
    // Partition 2 has already kept back most of it.
    a.ok(Command::OffsetTaken {
        from: 2,
        take_ref: "run-x/ip-x/2027-03-01".into(),
        paid_claim: "ip-x".into(),
        claim: "ip-1".into(),
        overpayment: op.clone(),
        amount_minor: 1_300,
    });
    a.core.sent.clear();
    // Partition 1 keeps back 300 on the old position: 151 is owed, 149 goes back.
    a.ok(Command::OffsetTaken {
        from: 1,
        take_ref: "run-9/ip-9/2027-03-01".into(),
        paid_claim: "ip-9".into(),
        claim: "ip-1".into(),
        overpayment: op.clone(),
        amount_minor: 300,
    });
    let o = &a.claim("ip-1").overpayments[0];
    assert_eq!((o.recovered_minor, o.status), (1_451, RecoveryStatus::Recovered));
    assert!(a
        .core
        .sent
        .iter()
        .any(|(p, c)| *p == 1 && matches!(c, Command::OffsetReturned { amount_minor: 149, claim, .. } if claim == "ip-9")));

    b.benefit_in_payment("ip-9");
    b.pay_run("run-9", d(2027, 3, 1));
    let returned = a
        .core
        .sent
        .iter()
        .find(|(_, c)| matches!(c, Command::OffsetReturned { .. }))
        .expect("sent back")
        .1
        .clone();
    b.ok(returned.clone());
    assert!(matches!(b.core.instructions.last(), Some(Instruction::ReturnOffset { amount_minor: 149, .. })));
    assert_eq!(b.claim("ip-9").schedule.as_ref().expect("in payment").arrears_minor, 149);
    assert_eq!(b.core.noticed("underpayment_owed").last().expect("told")["amount_minor"], "149");
    // Again: nothing more is owed.
    b.ok(returned);
    assert_eq!(b.claim("ip-9").schedule.as_ref().expect("in payment").arrears_minor, 149);

    // A take for a claim partition 0 does not hold goes back in full.
    a.core.sent.clear();
    a.ok(Command::OffsetTaken {
        from: 1,
        take_ref: "run-10/ip-9/2027-04-01".into(),
        paid_claim: "ip-9".into(),
        claim: "gone".into(),
        overpayment: op,
        amount_minor: 250,
    });
    assert!(matches!(&a.core.sent[..], [(1, Command::OffsetReturned { amount_minor: 250, .. })]));
}

/// Purpose: money sent back for a claim with no further benefit opens a senior review, and
/// money sent back for a claim this partition no longer has is published for a person to place.
/// Why it matters: arrears ride the next benefit, so where none is coming the money would
/// otherwise wait for ever without anyone being told.
#[test]
fn a_returned_offset_with_no_benefit_to_carry_it_reaches_a_person() {
    let mut b = partition(1, 2);
    overpaid(&mut b);
    let returned = |claim: &str, take_ref: &str| Command::OffsetReturned {
        from: 0,
        take_ref: take_ref.into(),
        claim: claim.into(),
        amount_minor: 80,
    };
    // ip-1 has ended: no run will pay its arrears.
    b.ok(returned("ip-1", "run-2/ip-1/2027-03-01"));
    assert!(b
        .claim("ip-1")
        .tasks
        .iter()
        .any(|t| t.kind == TaskKind::SeniorReview && t.id == "senior-review/offset-returned/run-2/ip-1/2027-03-01"));
    b.ok(returned("gone", "run-3/gone/2027-03-01"));
    assert_eq!(b.core.noticed("offset_return_unplaced").last().expect("published")["amount_minor"], "80");
}

/// Purpose: positions are applied in order of their sequence, a message claiming to come from
/// this partition or with an impossible position is refused, and an overpayment never put into
/// offset is never shared.
/// Why it matters: delivery is at least once and in no promised order; an old position applied
/// over a new one would let a partition keep back money that is no longer owed.
#[test]
fn positions_are_applied_in_order_and_only_from_elsewhere() {
    let mut b = partition(1, 2);
    let source = |seq: u64, outstanding: i64, from: u16| Command::OffsetSource {
        from,
        claim: "ip-1".into(),
        policy: "pol-2".into(),
        overpayment: "op-1".into(),
        party: "party-life".into(),
        outstanding_minor: outstanding,
        pct: 10,
        seq,
        settled_minor: 0,
    };
    b.ok(source(2, 500, 0));
    b.ok(source(1, 900, 0));
    assert_eq!(b.state.remote_offsets.values().next().expect("held").outstanding_minor, 500);
    assert_eq!(b.refused(source(3, 500, 1)), "invalid_offset");
    assert_eq!(b.refused(source(3, -1, 0)), "invalid_offset");
    let bad = Command::OffsetTaken {
        from: 0,
        take_ref: "r".into(),
        paid_claim: "c".into(),
        claim: "ip-1".into(),
        overpayment: "op-1".into(),
        amount_minor: 0,
    };
    assert_eq!(b.refused(bad), "invalid_offset");
    let bad = Command::OffsetReturned {
        from: 1,
        take_ref: "r".into(),
        claim: "c".into(),
        amount_minor: 5,
    };
    assert_eq!(b.refused(bad), "invalid_offset");

    // Held but not in offset: nothing is shared.
    let mut a = partition(0, 2);
    a.benefit_in_payment("ip-1");
    a.pay_run("run-1", d(2027, 3, 1));
    let end = Request::EndBenefit {
        end: d(2027, 3, 17),
        reason: EndReason::Recovered,
    };
    a.decided("ip-1", "ana", end, "R-OK");
    assert!(a.core.sent.is_empty());
}
