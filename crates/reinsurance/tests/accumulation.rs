//! The per-life accumulation: what the insurer holds on a life across every
//! policy and partition, and the cessions that follow from it.

mod common;

use common::*;
use ic_core::event::{Event, LifeInfo};
use ic_core::{GroupId, LifeId, PartyId, PolicyId, PolicyStatus, UnwindCause};
use ic_reinsurance::cession::UnplacedReason;
use ic_reinsurance::treaty::ExcessPath;
use ic_reinsurance::{Command, Instruction, InstructionBody, LifeKey, Notice};

fn party(id: &str) -> LifeKey {
    LifeKey::Party(PartyId::from(id))
}

fn status(policy: &str, to: PolicyStatus) -> Event {
    Event::PolicyStatusChanged {
        policy: PolicyId::from(policy),
        from: PolicyStatus::InForce,
        to,
    }
}

/// Purpose: two policies on the same person, issued on two different partitions, fill one
/// retention: the second cedes everything above what the first left.
/// Why it matters: this is the reason the module exists beside the core. Each partition sees
/// only its own policies; without the cross-partition accumulation the insurer would keep a
/// full retention on each policy and be over-exposed on the life without knowing.
#[test]
fn one_life_on_two_partitions_fills_one_retention() {
    let mut b = Bench::new();
    b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 300 * K, d(2027, 2, 1))]);
    let applied = b.issue(7, "P2", "anna", vec![layer("life", "life", 1, "L1", 400 * K, d(2027, 2, 1))]);
    let first = b.ri.cession(&key("P1", "life", 1, "L1"), d(2027, 2, 1)).unwrap();
    assert_eq!((first.retained_minor, first.treaty_ceded()), (300 * K, 0));
    let second = b.ri.cession(&key("P2", "life", 1, "L1"), d(2027, 2, 1)).unwrap();
    assert_eq!((second.retained_minor, second.treaty_ceded()), (200 * K, 200 * K));
    assert_eq!(second.ceded, vec![(north(), 120 * K), (south(), 80 * K)]);
    assert!(second.balanced());
    assert_eq!(b.ri.retained_on(&party("anna"), d(2027, 2, 1)), 500 * K);
    // The core is told to record the cession on the partition the cover is on.
    let [instruction] = &applied.instructions[..] else {
        panic!("{:?}", applied.instructions)
    };
    assert_eq!(instruction.partition, 7);
    assert_eq!(instruction.id, format!("ri-{}-1", applied.index));
    assert!(
        matches!(&instruction.body, InstructionBody::RecordCession { key: k, from, segments } if *k == key("P2", "life", 1, "L1") && *from == d(2027, 2, 1) && segments.len() == 1)
    );
    assert_eq!(b.ri.checkpoint(3), 1);
    assert_eq!(b.ri.checkpoint(7), 1);
    assert_eq!(b.ri.checkpoint(9), 0);
    b.assert_reconciles();
}

/// Purpose: the cessions on a life are the same whichever partition's events arrive first.
/// Why it matters: the partitions' streams interleave differently on every run. If the answer
/// depended on arrival order, two replays of the same business would cede different amounts
/// and the reinsurer's listing could not be reproduced.
#[test]
fn arrival_order_does_not_change_the_cessions() {
    let build = |first: u16| {
        let mut b = Bench::new();
        let order: [(u16, &str, i64, u32); 2] = if first == 3 {
            [(3, "P1", 300, 1), (7, "P2", 400, 5)]
        } else {
            [(7, "P2", 400, 5), (3, "P1", 300, 1)]
        };
        for (partition, policy, sum, day) in order {
            b.issue(partition, policy, "anna", vec![layer("life", "life", 1, "L1", sum * K, d(2027, 2, day))]);
        }
        b
    };
    let (a, b) = (build(3), build(7));
    for k in [key("P1", "life", 1, "L1"), key("P2", "life", 1, "L1")] {
        assert_eq!(a.ri.cession_record(&k), b.ri.cession_record(&k));
    }
    // P1 starts first, so it keeps its retention although it arrived second.
    assert_eq!(b.ri.cession(&key("P1", "life", 1, "L1"), d(2027, 2, 5)).unwrap().retained_minor, 300 * K);
    b.assert_reconciles();
}

/// Purpose: a batch with a gap in a partition's sequence is refused whole, and events already
/// seen are skipped when they are delivered again.
/// Why it matters: delivery from the partitions is at least once and can arrive out of step
/// after a failover. Applying past a gap would silently lose a cover from the accumulation;
/// applying a duplicate would count it twice.
#[test]
fn a_gap_is_refused_and_a_duplicate_is_skipped() {
    let mut b = Bench::new();
    b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 300 * K, d(2027, 2, 1))]);
    let mut later = b.records(3, vec![status("P1", PolicyStatus::Lapsed), status("P1", PolicyStatus::InForce)]);
    let gap = vec![later[1].clone()];
    assert_eq!(b.refused("event-feed", Command::CoreEvents { events: gap }), "gap");
    assert_eq!(b.ri.checkpoint(3), 1);
    // The same events delivered twice, the second time in a larger batch.
    b.ok(
        "event-feed",
        Command::CoreEvents {
            events: vec![later[0].clone()],
        },
    );
    let before = without_keys(&b.ri);
    let again = b.ok("event-feed", Command::CoreEvents { events: vec![later.remove(0)] });
    assert!(again.instructions.is_empty());
    assert_eq!(before, without_keys(&b.ri));
    b.ok("event-feed", Command::CoreEvents { events: later });
    assert_eq!(b.ri.checkpoint(3), 3);
    b.assert_reconciles();
}

/// Purpose: when a policy lapses its cover stops counting against the life's retention from
/// that day, and comes back from the day it is reinstated, with every date in the record.
/// Why it matters: ceded premium and recoveries are worked out from this record by date. A
/// lapse that left the cession running would pay a reinsurer for cover that did not exist.
#[test]
fn a_lapse_suspends_the_cession_and_reinstatement_restores_it() {
    let mut b = Bench::new();
    let k = key("P1", "life", 1, "L1");
    b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.today = d(2027, 5, 1);
    b.feed(3, vec![status("P1", PolicyStatus::Lapsed)]);
    assert!(b.ri.cession(&k, d(2027, 4, 30)).is_some());
    assert!(b.ri.cession(&k, d(2027, 5, 1)).is_none());
    // A second lapse event changes nothing.
    assert!(b.feed(3, vec![status("P1", PolicyStatus::Lapsed)]).instructions.is_empty());
    b.today = d(2027, 6, 10);
    b.feed(3, vec![status("P1", PolicyStatus::InForce)]);
    assert!(b.ri.cession(&k, d(2027, 6, 9)).is_none());
    assert_eq!(b.ri.cession(&k, d(2027, 6, 10)).unwrap().treaty_ceded(), 300 * K);
    assert_eq!(b.ri.cession_record(&k).len(), 3);
    // A layer added while a policy is out of force starts suspended.
    b.feed(3, vec![status("P1", PolicyStatus::Cancelled)]);
    b.add_layer(3, "P1", layer("life", "life", 2, "L1", 100 * K, d(2027, 6, 1)));
    assert!(b.ri.cession(&key("P1", "life", 2, "L1"), d(2027, 6, 20)).is_none());
    // An event for a policy the module has never seen is ignored.
    b.feed(3, vec![status("unknown", PolicyStatus::Lapsed)]);
    b.assert_reconciles();
}

/// Purpose: under the shipped treaty, retention freed when an early cover ends is not handed
/// to later covers; under a treaty that reallocates, it is.
/// Why it matters: treaties differ on this and the difference is money: reallocating when the
/// treaty does not allow it takes cover away from a reinsurer who is being paid for it.
#[test]
fn freed_retention_follows_the_treaty() {
    let run = |reallocate: bool| {
        let mut b = Bench::empty();
        let source = if reallocate {
            RETAIL.replace(
                "recapture_after_years = 10,",
                "recapture_after_years = 10,\n    reallocate_freed_retention = True,",
            )
        } else {
            RETAIL.to_string()
        };
        let treaty = Box::new(ic_reinsurance::loader::load_treaty("t.star", &source).unwrap());
        b.ok("config", Command::ActivateTreaty { treaty });
        b.issue(
            1,
            "P1",
            "anna",
            vec![
                layer("life", "life", 1, "L1", 400 * K, d(2027, 2, 1)),
                layer("trauma", "trauma", 1, "L1", 400 * K, d(2027, 3, 1)),
            ],
        );
        b.today = d(2027, 9, 1);
        b.feed(
            1,
            vec![Event::CoverExpired {
                policy: PolicyId::from("P1"),
                cover: "life".into(),
                on: d(2027, 9, 1),
            }],
        );
        b.assert_reconciles();
        b.ri.cession(&key("P1", "trauma", 1, "L1"), d(2027, 9, 1)).unwrap().retained_minor
    };
    assert_eq!(run(false), 100 * K);
    assert_eq!(run(true), 400 * K);
}

/// Purpose: an increase adds a layer that is ceded on its own start date, merging layers ends
/// the removed ones and re-rates the survivor, and cover above the automatic limit raises an
/// exception that closes when the cover comes back under it.
/// Why it matters: layers are what cessions attach to. If a merge lost a cession or an
/// over-limit cover sat retained with nobody told, the insurer would carry risk it never
/// meant to keep.
#[test]
fn layers_are_ceded_one_by_one_and_over_limit_cover_is_never_silent() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 2000 * K, d(2027, 2, 1))]);
    assert!(b.ri.exceptions().is_empty());
    b.today = d(2027, 4, 1);
    let applied = b.add_layer(1, "P1", layer("life", "life", 2, "L1", 3000 * K, d(2027, 4, 1)));
    let second = b.ri.cession(&key("P1", "life", 2, "L1"), d(2027, 4, 1)).unwrap().clone();
    // 1.5m was ceded by layer 1, so 2.5m of the 4m limit is left.
    assert_eq!((second.retained_minor, second.treaty_ceded(), second.unplaced_minor), (0, 2500 * K, 500 * K));
    assert_eq!(second.unplaced_reason, Some(UnplacedReason::AboveAutomaticLimit));
    let id = "over_retention:P1/life/2/L1";
    assert!(b.ri.exceptions().contains_key(id));
    assert!(applied
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ExceptionOpened { code, .. } if code == "over_retention")));
    b.assert_reconciles();
    // The same event again does not open it twice.
    b.today = d(2027, 7, 1);
    let merged = b.feed(
        1,
        vec![Event::LayersMerged {
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            removed: vec![2],
            into: 1,
            sum_insured_minor: 4200 * K,
        }],
    );
    assert!(b.ri.cession(&key("P1", "life", 2, "L1"), d(2027, 7, 1)).is_none());
    let survivor = b.ri.cession(&key("P1", "life", 1, "L1"), d(2027, 7, 1)).unwrap();
    assert_eq!(
        (survivor.retained_minor, survivor.treaty_ceded(), survivor.unplaced_minor),
        (500 * K, 3700 * K, 0)
    );
    assert!(merged
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ExceptionClosed { id: closed } if closed == id)));
    assert!(b.ri.exceptions().is_empty());
    b.assert_reconciles();
}

/// Purpose: a policy that moves to another partition keeps its covers, their dates and their
/// cessions; only where its entries are posted changes, and the stub left behind does not end
/// the cover.
/// Why it matters: a transfer between groups must not look like a cancellation and a new
/// policy, which would reset retention order and rewrite cessions that have not changed.
#[test]
fn a_transfer_between_partitions_keeps_the_cessions() {
    let mut b = Bench::new();
    let k = key("P1", "life", 1, "L1");
    b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    let before = b.ri.cession_record(&k).to_vec();
    b.today = d(2027, 5, 1);
    let landed = |b: &mut Bench, policy: &str, members| {
        b.feed(
            8,
            vec![Event::TransferLanded {
                policy: PolicyId::from(policy),
                transfer_id: "T1".into(),
                group: GroupId::from("G2"),
                from_partition: 3,
                lives: vec![LifeInfo::new(LifeId::from("L1"), Some(PartyId::from("anna")))],
                layers: vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))],
                members,
                number: format!("N-{policy}"),
                product_code: "term-life".into(),
                status: PolicyStatus::InForce,
                standing: None,
            }],
        )
    };
    let applied = landed(&mut b, "P1", vec![]);
    b.feed(3, vec![status("P1", PolicyStatus::Transferred)]);
    assert_eq!(b.ri.cession_record(&k), &before[..]);
    // The whole record is sent to the new partition, in case a change sent to the old
    // one while the policy was moving was refused there. Nothing else is issued.
    assert_eq!(applied.instructions.len(), 1);
    assert!(matches!(
        &applied.instructions[0],
        Instruction { partition: 8, body: InstructionBody::RecordCession { key, from, segments }, .. }
            if *key == k && *from == d(2027, 2, 1) && segments[..] == before[..]
    ));
    // The same happens where a policy stays put because its transfer was abandoned.
    let aborted = b.feed(
        8,
        vec![Event::TransferAborted {
            policy: PolicyId::from("P1"),
            transfer_id: "T2".into(),
        }],
    );
    assert_eq!(
        aborted.instructions,
        applied
            .instructions
            .iter()
            .cloned()
            .map(|i| Instruction {
                id: aborted.instructions[0].id.clone(),
                ..i
            })
            .collect::<Vec<_>>()
    );
    // A later change is recorded on the new partition.
    let added = b.add_layer(8, "P1", layer("life", "life", 2, "L1", 100 * K, d(2027, 5, 1)));
    assert!(added.instructions.iter().all(|i| i.partition == 8));
    // A status change from the old partition no longer applies to the policy.
    b.feed(3, vec![status("P1", PolicyStatus::Lapsed)]);
    assert!(b.ri.cession(&k, d(2027, 5, 1)).is_some());
    // A policy first met when it lands is taken on from the landing event.
    landed(&mut b, "P9", vec![member_cover("M1", 50 * K)]);
    assert!(b.ri.cession(&key("P9", "life", 1, "L1"), d(2027, 5, 1)).is_some());
    assert!(b.ri.cession(&key("P9", "group-life", 0, "M1"), d(2027, 5, 1)).is_some());
    b.assert_reconciles();
}

/// Purpose: a death notified late unwinds the indexation layer added after it and ends the
/// policy on the day after the event, so the cession on the day of death is on the sum insured
/// at death.
/// Why it matters: addendum decision 1. The recovery is claimed on the cession at the event
/// date; if the indexed layer stayed in the record the insurer would claim, and have paid
/// premium for, cover that never existed.
#[test]
fn a_late_notified_death_puts_the_cession_back_to_the_sum_insured_at_death() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.today = d(2028, 2, 1);
    b.add_layer(1, "P1", layer("life", "life", 2, "L1", 40 * K, d(2028, 2, 1)));
    assert_eq!(b.ri.cession(&key("P1", "life", 2, "L1"), d(2028, 2, 1)).unwrap().treaty_ceded(), 40 * K);
    // The death was on 20 January 2028, notified on 15 March.
    b.today = d(2028, 3, 15);
    let applied = b.feed(
        1,
        vec![
            Event::LayerUnwound {
                policy: PolicyId::from("P1"),
                cover: "life".into(),
                layer: 2,
                start: d(2028, 2, 1),
                reference: "CLM-1".into(),
            },
            Event::PolicyUnwound {
                policy: PolicyId::from("P1"),
                after: d(2028, 1, 20),
                cause: UnwindCause::Claim,
                reference: "CLM-1".into(),
                receivables: vec![],
            },
            status("P1", PolicyStatus::Terminated),
            Event::PolicyTerminated {
                policy: PolicyId::from("P1"),
                effective: d(2028, 1, 20),
                claim: "CLM-1".into(),
                payable: vec![],
            },
        ],
    );
    assert_eq!(b.ri.cession_record(&key("P1", "life", 2, "L1")), &[(d(2028, 2, 1), None)]);
    let k = key("P1", "life", 1, "L1");
    assert_eq!(b.ri.cession(&k, d(2028, 1, 20)).unwrap().sum_at_risk_minor, 800 * K);
    assert!(b.ri.cession(&k, d(2028, 1, 21)).is_none());
    // The correction reaches back to the date it applies from.
    assert!(applied
        .instructions
        .iter()
        .any(|i| matches!(&i.body, InstructionBody::RecordCession { key, from, .. } if *key == k && *from == d(2028, 1, 21))));
    b.assert_reconciles();
}

/// Purpose: a cover that expired after the unwind date is in force again, and its cession is
/// restored from the day it had ended.
/// Why it matters: an unwind for remediation puts the policy back as it stood. If the expired
/// cover stayed out of the accumulation, a claim on it would have no recovery.
#[test]
fn an_unwound_expiry_restores_the_cession() {
    let mut b = Bench::new();
    let k = key("P1", "trauma", 1, "L1");
    b.issue(1, "P1", "anna", vec![layer("trauma", "trauma", 1, "L1", 900 * K, d(2027, 2, 1))]);
    b.today = d(2027, 8, 1);
    b.feed(
        1,
        vec![Event::CoverExpired {
            policy: PolicyId::from("P1"),
            cover: "trauma".into(),
            on: d(2027, 8, 1),
        }],
    );
    assert!(b.ri.cession(&k, d(2027, 8, 2)).is_none());
    b.today = d(2027, 9, 1);
    b.feed(
        1,
        vec![Event::CoverExpiryUnwound {
            policy: PolicyId::from("P1"),
            cover: "trauma".into(),
            layers: vec![
                layer("trauma", "trauma", 1, "L1", 900 * K, d(2027, 2, 1)),
                layer("trauma", "trauma", 9, "L1", K, d(2027, 2, 1)),
            ],
            reference: "REM-1".into(),
        }],
    );
    assert_eq!(b.ri.cession(&k, d(2027, 8, 2)).unwrap().treaty_ceded(), 400 * K);
    assert_eq!(b.ri.cession_record(&k).len(), 1);
    b.assert_reconciles();
}

/// Purpose: scheme members not linked to a party are ceded on the scheme share, each against
/// their own record; joins, changes and leavers move the cession on their effective dates,
/// including a member who leaves and comes back.
/// Why it matters: addendum decision 2, addition 1. Membership files write cover with no
/// capacity step, so the accumulation has to follow every membership event or scheme cover
/// would be invisible to reinsurance.
#[test]
fn membership_events_move_the_cession_on_their_effective_dates() {
    let mut b = Bench::new();
    let scheme = PolicyId::from("S1");
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    let m1 = key("S1", "group-life", 0, "M1");
    let c = b.ri.cession(&m1, d(2027, 2, 1)).unwrap();
    assert_eq!((c.retained_minor, c.treaty_ceded()), (140 * K, 60 * K));
    b.today = d(2027, 3, 1);
    b.feed(
        2,
        vec![
            Event::MemberJoined {
                policy: scheme.clone(),
                life: LifeId::from("M2"),
                effective: d(2027, 3, 1),
                covers: vec![member_cover("M2", 100 * K)],
                party: None,
                info: None,
            },
            // A salary change, backdated to mid February.
            Event::MemberChanged {
                policy: scheme.clone(),
                life: LifeId::from("M1"),
                effective: d(2027, 2, 15),
                covers: vec![member_cover("M1", 300 * K)],
                info: None,
            },
        ],
    );
    assert_eq!(b.ri.cession(&m1, d(2027, 2, 14)).unwrap().treaty_ceded(), 60 * K);
    assert_eq!(b.ri.cession(&m1, d(2027, 2, 15)).unwrap().treaty_ceded(), 90 * K);
    assert_eq!(b.ri.cession(&key("S1", "group-life", 0, "M2"), d(2027, 3, 1)).unwrap().treaty_ceded(), 30 * K);
    // The same details again change nothing.
    let same = b.feed(
        2,
        vec![Event::MemberChanged {
            policy: scheme.clone(),
            life: LifeId::from("M1"),
            effective: d(2027, 2, 20),
            covers: vec![member_cover("M1", 300 * K)],
            info: None,
        }],
    );
    assert!(same.instructions.is_empty());
    b.today = d(2027, 5, 1);
    b.feed(
        2,
        vec![Event::MemberLeft {
            policy: scheme.clone(),
            life: LifeId::from("M1"),
            effective: d(2027, 4, 30),
        }],
    );
    assert!(b.ri.cession(&m1, d(2027, 4, 30)).is_none());
    b.today = d(2027, 7, 1);
    b.feed(
        2,
        vec![Event::MemberJoined {
            policy: scheme,
            life: LifeId::from("M1"),
            effective: d(2027, 7, 1),
            covers: vec![member_cover("M1", 250 * K)],
            party: None,
            info: None,
        }],
    );
    assert!(b.ri.cession(&m1, d(2027, 6, 30)).is_none());
    assert_eq!(b.ri.cession(&m1, d(2027, 7, 1)).unwrap().treaty_ceded(), 75 * K);
    b.assert_reconciles();
}

/// Purpose: when a scheme member is identified as an existing party, their scheme cover joins
/// that person's accumulation, and what takes the life over the retention is ceded
/// facultatively under the treaty's standing terms, up to their limit.
/// Why it matters: addendum decision 2: "never silently over-retained". Before the match the
/// insurer held a retail retention and a scheme retention on one person without knowing.
#[test]
fn an_identified_member_joins_the_life_and_the_excess_is_ceded_facultatively() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 450 * K, d(2027, 2, 1))]);
    b.today = d(2027, 3, 1);
    b.issue_scheme(2, "S1", d(2027, 3, 1), vec![member_cover("M1", 700 * K)]);
    let m1 = key("S1", "group-life", 0, "M1");
    assert_eq!(b.ri.cession(&m1, d(2027, 3, 1)).unwrap().treaty_ceded(), 210 * K);
    let identify = Event::MemberIdentified {
        policy: PolicyId::from("S1"),
        life: LifeId::from("M1"),
        party: PartyId::from("anna"),
    };
    let applied = b.feed(2, vec![identify.clone()]);
    // The retail and scheme treaties share one retention per life: the retail policy holds
    // 450,000 of it, so the scheme cover keeps 50,000.
    let c = b.ri.cession(&m1, d(2027, 3, 1)).unwrap();
    assert_eq!(
        (c.retained_minor, c.treaty_ceded(), c.facultative_ceded(), c.unplaced_minor),
        (50 * K, 0, 650 * K, 0)
    );
    assert!(c.facultative[0].standing && c.facultative[0].reinsurer == north());
    assert!(c.facultative[0].reference.starts_with("FACOB/group-life-surplus@1/"));
    assert_eq!(c.ceded_to(&north()), 650 * K);
    assert!(applied
        .instructions
        .iter()
        .any(|i| matches!(&i.body, InstructionBody::RecordCession { key, .. } if *key == m1)));
    assert_eq!(b.ri.retained_on(&party("anna"), d(2027, 3, 1)), 500 * K);
    assert_eq!(
        b.ri.retained_on(
            &LifeKey::Unmatched {
                policy: PolicyId::from("S1"),
                life: LifeId::from("M1")
            },
            d(2027, 3, 1)
        ),
        0
    );
    // Identifying the same member again changes nothing.
    assert!(b.feed(2, vec![identify]).instructions.is_empty());
    // A second scheme on the same person: the standing terms take up to their limit per life.
    b.issue_scheme(4, "S2", d(2027, 3, 1), vec![member_cover("M7", 1000 * K)]);
    b.feed(
        4,
        vec![Event::MemberIdentified {
            policy: PolicyId::from("S2"),
            life: LifeId::from("M7"),
            party: PartyId::from("anna"),
        }],
    );
    let c = b.ri.cession(&key("S2", "group-life", 0, "M7"), d(2027, 3, 1)).unwrap();
    assert_eq!((c.retained_minor, c.facultative_ceded(), c.unplaced_minor), (0, 350 * K, 650 * K));
    assert_eq!(c.unplaced_reason, Some(UnplacedReason::AboveFacultativeLimit));
    assert!(b.ri.exceptions().contains_key("over_retention:S2/group-life/0/M7"));
    assert_eq!(format!("{}", party("anna")), "party:anna");
    b.assert_reconciles();
}

/// Purpose: where the scheme's terms say excess cover goes down the underwritten-excess path,
/// the excess is flagged to group administration with a stable reference and held as an open
/// exception, not ceded and not quietly retained.
/// Why it matters: the scheme chooses the path. Sending the notice once, with a reference that
/// does not change on replay, is what lets group administration start underwriting exactly
/// once.
#[test]
fn the_underwritten_excess_path_is_flagged_once() {
    let mut b = Bench::new();
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 700 * K)]);
    b.ok(
        "group-admin",
        Command::SetSchemeTerms {
            policy: PolicyId::from("S1"),
            excess_path: ExcessPath::UnderwrittenExcess,
            sites: vec![("auckland".into(), 30), ("wellington".into(), 10)],
        },
    );
    let applied = b.feed(
        2,
        vec![Event::MemberIdentified {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            party: PartyId::from("anna"),
        }],
    );
    let notices: Vec<_> = applied
        .notices
        .iter()
        .filter(|n| matches!(n, Notice::RequireUnderwrittenExcess { .. }))
        .collect();
    assert_eq!(
        notices,
        vec![&Notice::RequireUnderwrittenExcess {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            cover: "group-life".into(),
            excess_minor: 200 * K,
            reference: format!("UWX/S1/group-life/0/M1/{}", 200 * K),
        }]
    );
    let c = b.ri.cession(&key("S1", "group-life", 0, "M1"), d(2027, 2, 1)).unwrap();
    assert_eq!(
        (c.unplaced_minor, c.unplaced_reason),
        (200 * K, Some(UnplacedReason::UnderwrittenExcessPending))
    );
    // A member who joins later under the same scheme follows the scheme's path, and an
    // unrelated event does not send the notice again.
    let later = b.feed(
        2,
        vec![Event::MemberJoined {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M2"),
            effective: d(2027, 2, 1),
            covers: vec![member_cover("M2", 10 * K)],
            party: None,
            info: None,
        }],
    );
    assert!(!later.notices.iter().any(|n| matches!(n, Notice::RequireUnderwrittenExcess { .. })));
    let sites = b.ri.scheme_concentration(d(2027, 2, 1));
    assert_eq!(sites[0].gross_minor, 710 * K);
    assert_eq!(
        sites[0].sites.iter().map(|s| s.2).sum::<i64>(),
        sites[0].retained_minor + sites[0].unplaced_minor
    );
    assert_eq!(sites[0].sites[0].1, 30);
    b.assert_reconciles();
}

/// Purpose: a possible match between a member and a party is an open exception that does not
/// change any cession, and it closes when the match is cleared or confirmed.
/// Why it matters: addendum decision 2: a probable match is a review item, never an automatic
/// merge. Counting an unconfirmed match would expose one person's cover to another's record.
#[test]
fn a_possible_match_is_an_exception_and_never_a_merge() {
    let mut b = Bench::new();
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 700 * K), member_cover("M2", 100 * K)]);
    let note = |party: &str, life: &str| Command::NotePossibleMatch {
        policy: PolicyId::from("S1"),
        life: LifeId::from(life),
        party: PartyId::from(party),
    };
    let applied = b.ok("group-admin", note("anna", "M1"));
    assert!(applied.instructions.is_empty());
    b.ok("group-admin", note("bella", "M1"));
    b.ok("group-admin", note("anna", "M2"));
    assert_eq!(b.ri.exceptions().len(), 3);
    assert_eq!(b.ri.cession(&key("S1", "group-life", 0, "M1"), d(2027, 2, 1)).unwrap().treaty_ceded(), 210 * K);
    b.ok(
        "group-admin",
        Command::ClearPossibleMatch {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            party: PartyId::from("bella"),
        },
    );
    assert_eq!(b.ri.exceptions().len(), 2);
    // Clearing a match that was never noted is harmless.
    b.ok(
        "group-admin",
        Command::ClearPossibleMatch {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M9"),
            party: PartyId::from("bella"),
        },
    );
    // Confirming M1 as anna closes M1's item and leaves M2's.
    b.feed(
        2,
        vec![Event::MemberIdentified {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            party: PartyId::from("anna"),
        }],
    );
    let open: Vec<&String> = b.ri.exceptions().keys().filter(|k| k.starts_with("possible_match")).collect();
    assert_eq!(open, vec!["possible_match:S1/M2/anna"]);
    b.assert_reconciles();
}

/// Purpose: a member who joins already identified goes straight onto that person's
/// accumulation; an accepted underwritten excess arrives as a layer on the member's life and is
/// ceded under the treaty like any underwritten cover; the layer ends when the member leaves
/// and is restored with no gap when the leave was made in error.
/// Why it matters: this is the far end of the underwritten-excess path the module asks group
/// administration to start. If the accepted excess were not ceded, or stayed ceded after the
/// member left, the path would end in exactly the silent over-retention it exists to prevent.
#[test]
fn an_underwritten_excess_layer_is_ceded_and_follows_the_member() {
    let mut b = Bench::new();
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![]);
    let scheme = PolicyId::from("S1");
    let join = Event::MemberJoined {
        policy: scheme.clone(),
        life: LifeId::from("M1"),
        effective: d(2027, 2, 1),
        covers: vec![member_cover("M1", 500 * K)],
        party: Some(PartyId::from("anna")),
        info: None,
    };
    b.feed(2, vec![join.clone()]);
    assert_eq!(b.ri.retained_on(&party("anna"), d(2027, 2, 1)), 500 * K);
    let excess = Event::LayerAdded {
        policy: scheme.clone(),
        layer: layer("group-life", "group_life", 1, "M1", 400 * K, d(2027, 3, 1)),
        origin: ic_core::LayerOrigin::UnderwrittenExcess,
        continues: None,
    };
    b.today = d(2027, 3, 1);
    b.feed(2, vec![excess.clone()]);
    let k = key("S1", "group-life", 1, "M1");
    let c = b.ri.cession(&k, d(2027, 3, 1)).unwrap();
    assert_eq!((c.retained_minor, c.treaty_ceded(), c.unplaced_minor), (0, 400 * K, 0));
    assert!(b.ri.exceptions().is_empty());
    b.today = d(2027, 6, 1);
    b.feed(
        2,
        vec![
            Event::MemberLeft {
                policy: scheme.clone(),
                life: LifeId::from("M1"),
                effective: d(2027, 5, 31),
            },
            Event::LayerEnded {
                policy: scheme,
                cover: "group-life".into(),
                layer: 1,
                life: LifeId::from("M1"),
                effective: d(2027, 5, 31),
                sum_insured_minor: 400 * K,
            },
        ],
    );
    assert!(b.ri.cession(&k, d(2027, 5, 31)).is_none());
    assert_eq!(b.ri.retained_on(&party("anna"), d(2027, 6, 1)), 0);
    // The leave was a mistake: the member and the layer come back from the same date.
    let rejoin = Event::MemberJoined {
        policy: PolicyId::from("S1"),
        life: LifeId::from("M1"),
        effective: d(2027, 5, 31),
        covers: vec![member_cover("M1", 500 * K)],
        party: Some(PartyId::from("anna")),
        info: None,
    };
    b.feed(2, vec![rejoin, excess]);
    assert_eq!(b.ri.cession_record(&k).len(), 1, "no gap is left in the record");
    assert_eq!(b.ri.cession_record(&key("S1", "group-life", 0, "M1")).len(), 1);
    assert_eq!(b.ri.retained_on(&party("anna"), d(2027, 5, 31)), 500 * K);
    b.assert_reconciles();
}

/// Purpose: when a member first matched to one person is corrected to another, the scheme
/// cover leaves the first person's accumulation, their own cover gets its retention back, and
/// the member's cover is ceded again on the second person's life.
/// Why it matters: matches are made by people and are sometimes wrong. A correction that
/// left the cover counted against the first person would under-retain on them for good, and
/// one that lost the cover in the move would leave it with no cession at all.
#[test]
fn a_corrected_identification_moves_the_cover_to_the_right_person() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 450 * K, d(2027, 2, 1))]);
    b.today = d(2027, 3, 1);
    b.issue_scheme(2, "S1", d(2027, 3, 1), vec![member_cover("M1", 700 * K)]);
    let m1 = key("S1", "group-life", 0, "M1");
    let identified = |who: &str| Event::MemberIdentified {
        policy: PolicyId::from("S1"),
        life: LifeId::from("M1"),
        party: PartyId::from(who),
    };
    b.feed(2, vec![identified("anna")]);
    assert_eq!(b.ri.cession(&m1, d(2027, 3, 1)).unwrap().retained_minor, 50 * K);
    assert_eq!(b.ri.retained_on(&party("anna"), d(2027, 3, 1)), 500 * K);

    let corrected = b.feed(2, vec![identified("ben")]);
    assert_eq!(b.ri.retained_on(&party("anna"), d(2027, 3, 1)), 450 * K);
    assert_eq!(b.ri.retained_on(&party("ben"), d(2027, 3, 1)), 500 * K);
    let c = b.ri.cession(&m1, d(2027, 3, 1)).unwrap();
    assert_eq!(
        (c.retained_minor, c.treaty_ceded() + c.facultative_ceded(), c.unplaced_minor),
        (500 * K, 200 * K, 0)
    );
    assert!(corrected
        .instructions
        .iter()
        .any(|i| matches!(&i.body, InstructionBody::RecordCession { key, .. } if *key == m1)));
    let unmatched = LifeKey::Unmatched {
        policy: PolicyId::from("S1"),
        life: LifeId::from("M1"),
    };
    assert_eq!(b.ri.retained_on(&unmatched, d(2027, 3, 1)), 0);
    assert_eq!(unmatched.to_string(), "member:S1/M1");
    b.assert_reconciles();
}

/// Purpose: merging two layers of a cover ends the one removed and moves its sum to the
/// survivor, and a third layer of the same cover that the merge does not name is left exactly
/// as it was.
/// Why it matters: the merge event names the cover, and every layer of that cover is looked
/// at. A layer swept up by mistake would lose or change a cession nobody asked to change.
#[test]
fn a_merge_leaves_the_layers_it_does_not_name_alone() {
    let mut b = Bench::new();
    b.issue(
        1,
        "P1",
        "anna",
        vec![
            layer("life", "life", 1, "L1", 300 * K, d(2027, 2, 1)),
            layer("life", "life", 2, "L1", 300 * K, d(2027, 2, 1)),
            layer("life", "life", 3, "L1", 300 * K, d(2027, 2, 1)),
        ],
    );
    let third = key("P1", "life", 3, "L1");
    let before = b.ri.cession_record(&third).to_vec();
    b.today = d(2027, 7, 1);
    b.feed(
        1,
        vec![Event::LayersMerged {
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            removed: vec![2],
            into: 1,
            sum_insured_minor: 600 * K,
        }],
    );
    assert!(b.ri.cession(&key("P1", "life", 2, "L1"), d(2027, 7, 1)).is_none());
    assert_eq!(b.ri.cession(&key("P1", "life", 1, "L1"), d(2027, 7, 1)).unwrap().sum_at_risk_minor, 600 * K);
    assert_eq!(b.ri.cession_record(&third), &before[..]);
    assert_eq!(b.ri.cession(&third, d(2027, 7, 1)).unwrap().sum_at_risk_minor, 300 * K);
    b.assert_reconciles();
}

/// Purpose: cover under a treaty with its own retention does not count against the retention
/// two other treaties share, and is not reduced by what they hold.
/// Why it matters: the retention is per life "across the treaties that share it". Pooling a
/// treaty that does not share it would cede cover its reinsurers never agreed to take, or
/// keep more on one life than the insurer's retention allows.
#[test]
fn a_treaty_with_its_own_retention_is_not_pooled_with_the_others() {
    let mut b = Bench::new();
    let own = RETAIL
        .replace("id = \"retail-life-surplus\"", "id = \"retail-tpd-surplus\"")
        .replace("scope(\"term-life\", [\"life\", \"trauma\"])", "scope(\"term-life\", [\"tpd\"])")
        .replace("    retention_group = \"life\",\n", "");
    let treaty = Box::new(ic_reinsurance::loader::load_treaty("retail_tpd_surplus.star", &own).unwrap());
    assert_eq!(treaty.retention_key(), "retail-tpd-surplus");
    b.ok("config", Command::ActivateTreaty { treaty });
    b.issue(
        1,
        "P1",
        "anna",
        vec![
            layer("life", "life", 1, "L1", 450 * K, d(2027, 2, 1)),
            layer("tpd", "tpd", 1, "L1", 450 * K, d(2027, 3, 1)),
            layer("trauma", "trauma", 1, "L1", 450 * K, d(2027, 4, 1)),
        ],
    );
    let on = d(2027, 4, 1);
    let tpd = b.ri.cession(&key("P1", "tpd", 1, "L1"), on).unwrap();
    assert_eq!((tpd.retained_minor, tpd.treaty_ceded()), (450 * K, 0), "its own 500,000 is untouched");
    let trauma = b.ri.cession(&key("P1", "trauma", 1, "L1"), on).unwrap();
    assert_eq!(
        (trauma.retained_minor, trauma.treaty_ceded()),
        (50 * K, 400 * K),
        "shares 500,000 with life only"
    );
    b.assert_reconciles();
}

/// Purpose: a scheme treaty that cedes members not linked to a person as an excess over a
/// limit per member keeps each member's cover up to the limit and cedes the rest.
/// Why it matters: this is the second of the two bases a treaty can name for unlinked
/// members, and no shipped treaty uses it. A treaty file choosing it must cede what it says,
/// not fall back to the scheme share.
#[test]
fn unlinked_members_can_be_ceded_as_an_excess_per_member() {
    let mut b = Bench::empty();
    let source = GROUP.replace("scheme_share(\"0.30\")", "excess_per_member(dollars(140000))");
    let treaty = Box::new(ic_reinsurance::loader::load_treaty("group_life_surplus.star", &source).unwrap());
    b.ok("config", Command::ActivateTreaty { treaty });
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K), member_cover("M2", 100 * K)]);
    let on = d(2027, 2, 1);
    let over = b.ri.cession(&key("S1", "group-life", 0, "M1"), on).unwrap();
    assert_eq!((over.retained_minor, over.treaty_ceded(), over.unplaced_minor), (140 * K, 60 * K, 0));
    let under = b.ri.cession(&key("S1", "group-life", 0, "M2"), on).unwrap();
    assert_eq!((under.retained_minor, under.treaty_ceded()), (100 * K, 0));
    b.assert_reconciles();
}

/// What the core emits when underwriting changes the terms of a layer: the layer ends and
/// is written again from the same day, the new one naming the one it continues or not.
fn terms_altered(policy: &str, cover: &str, old: u32, new: u32, sum: i64, on: chrono::NaiveDate, continues: Option<u32>) -> Vec<Event> {
    vec![
        Event::LayerEnded {
            policy: PolicyId::from(policy),
            cover: cover.into(),
            layer: old,
            life: "L1".into(),
            effective: on,
            sum_insured_minor: sum,
        },
        Event::LayerAdded {
            policy: PolicyId::from(policy),
            layer: layer(cover, cover, new, "L1", sum, on),
            origin: ic_core::LayerOrigin::Original,
            continues,
        },
    ]
}

/// Purpose: when underwriting changes the loadings or exclusions on cover in force, the
/// layer the core writes in its place keeps the old layer's place in the life's retention
/// order, so what is retained and ceded on every cover of the life is the same the day
/// after as the day before; a second change of terms keeps it again, whether the core ends the
/// old layer before writing the new one or after (group's move of a loading to the member
/// writes first). The same pair of events without the link is new business and goes to the
/// back of the order.
/// Why it matters: under a treaty that reallocates freed retention, treating a change of
/// terms as an ending and a new start hands the retention to the life's later covers and
/// cedes the altered cover instead. Nothing about the risk's size changed, yet premiums
/// would move between the insurer and its reinsurers on two covers and the reinsurer's
/// listing would show a cancellation and new business that never happened.
#[test]
fn a_change_of_underwriting_terms_keeps_the_cover_s_place_in_the_retention() {
    let run = |continues: bool, written_first: bool| {
        let in_order = |mut events: Vec<Event>| {
            if written_first {
                events.reverse();
            }
            events
        };
        let mut b = Bench::empty();
        let source = RETAIL.replace(
            "recapture_after_years = 10,",
            "recapture_after_years = 10,\n    reallocate_freed_retention = True,",
        );
        let treaty = Box::new(ic_reinsurance::loader::load_treaty("t.star", &source).unwrap());
        b.ok("config", Command::ActivateTreaty { treaty });
        b.issue(
            1,
            "P1",
            "anna",
            vec![
                layer("life", "life", 1, "L1", 400 * K, d(2027, 2, 1)),
                layer("trauma", "trauma", 1, "L1", 400 * K, d(2027, 3, 1)),
            ],
        );
        let trauma_before = b.ri.cession_record(&key("P1", "trauma", 1, "L1")).to_vec();
        b.today = d(2027, 9, 1);
        b.feed(1, in_order(terms_altered("P1", "life", 1, 2, 400 * K, d(2027, 9, 1), continues.then_some(1))));
        b.today = d(2027, 11, 1);
        b.feed(1, in_order(terms_altered("P1", "life", 2, 3, 400 * K, d(2027, 11, 1), continues.then_some(2))));
        b.assert_reconciles();
        assert!(b.ri.cession(&key("P1", "life", 1, "L1"), d(2027, 9, 1)).is_none(), "the old layer has ended");
        assert!(b.ri.cession(&key("P1", "life", 2, "L1"), d(2027, 11, 1)).is_none());
        let unchanged = b.ri.cession_record(&key("P1", "trauma", 1, "L1")) == &trauma_before[..];
        let retained = |cover: &str, layer: u32, on| b.ri.cession(&key("P1", cover, layer, "L1"), on).unwrap().retained_minor;
        (
            retained("life", 2, d(2027, 9, 1)),
            retained("life", 3, d(2027, 11, 1)),
            retained("trauma", 1, d(2027, 11, 1)),
            unchanged,
        )
    };
    assert_eq!(run(true, false), (400 * K, 400 * K, 100 * K, true));
    assert_eq!(run(true, true), (400 * K, 400 * K, 100 * K, true));
    assert_eq!(run(false, false), (100 * K, 100 * K, 400 * K, false));
}

/// Purpose: a layer written again on new terms stays on the treaty version it was ceded
/// under, counts its years toward recapture from when the cover first began, is listed to
/// the reinsurer from that date, and keeps its facultative placement; because a facultative
/// reinsurer took the risk on the old terms, an exception asks a person to tell it, and
/// closes only when they record where its answer is filed. A layer whose predecessor this
/// module never saw is ceded as new cover.
/// Why it matters: in-force cover moves to a new treaty version only by negotiation, and a
/// facultative placement is a contract on one named risk. Losing either because a loading
/// changed would leave cover above the automatic limit unplaced, or priced on terms the
/// reinsurer never agreed for it; and a reinsurer not told of a material change in a risk
/// it accepted individually can decline the claim.
#[test]
fn a_change_of_underwriting_terms_keeps_the_treaty_and_the_facultative_placement() {
    let mut b = Bench::new();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 5000 * K, d(2027, 2, 1))]);
    let old = key("P1", "life", 1, "L1");
    let fac = Command::RecordFacultativeTerms(Box::new(ic_reinsurance::machine::FacultativeTerms {
        target: ic_reinsurance::machine::FacTarget::Risk(old.clone()),
        reference: "FAC-1".into(),
        reinsurer: south(),
        amount_minor: 500 * K,
        rate: 2_000_000,
        terms_ref: "vault/FAC-1".into(),
        loading: Some("+50".into()),
        exclusions: vec![],
    }));
    b.ok("reinsurance-ops", fac);
    let before = b.ri.cession(&old, d(2027, 2, 1)).unwrap().clone();
    assert_eq!((before.facultative_ceded(), before.unplaced_minor), (500 * K, 0));
    // A new version of the treaty is in force by the time the terms change.
    let v2 = ic_reinsurance::loader::load_treaty(
        "t.star",
        &RETAIL
            .replace("version = 1", "version = 2")
            .replace("effective_from = \"2027-01-01\"", "effective_from = \"2028-01-01\"")
            .replace("commission = \"0.20\"", "commission = \"0.25\""),
    )
    .unwrap();
    b.ok("config", Command::ActivateTreaty { treaty: Box::new(v2) });

    b.today = d(2028, 6, 1);
    let applied = b.feed(1, terms_altered("P1", "life", 1, 2, 5000 * K, d(2028, 6, 1), Some(1)));
    let new = key("P1", "life", 2, "L1");
    let after = b.ri.cession(&new, d(2028, 6, 1)).unwrap().clone();
    assert_eq!(after, before, "the same cession, on the same treaty version, with the same facultative share");
    assert_eq!(after.treaty.as_ref().unwrap().version, 1);
    let id = "facultative_terms_altered:P1/life/2/L1";
    assert!(applied.notices.iter().any(
        |n| matches!(n, Notice::ExceptionOpened { id: opened, code, detail } if opened == id && code == "facultative_terms_altered" && detail.contains("FAC-1"))
    ));
    b.assert_reconciles();

    // Ten years run from 2027, when the cover began, not from the day its terms changed.
    let recapture = |effective| Command::Recapture {
        treaty: retail(),
        effective,
        keys: vec![new.clone()],
    };
    assert_eq!(b.refused("reinsurance-ops", recapture(d(2037, 1, 31))), "recapture_not_allowed");
    let mut later = Bench {
        ri: b.ri.clone(),
        ..Bench::empty()
    };
    later.index = b.index;
    later.today = d(2037, 2, 1);
    later.ok("reinsurance-ops", recapture(d(2037, 2, 1)));

    // Telling the reinsurer closes the exception; a second change before that moves it on.
    let told = |key: &ic_reinsurance::RiskKey, reference: &str| Command::FacultativeReinsurerTold {
        key: key.clone(),
        reference: reference.into(),
    };
    assert_eq!(b.refused("reinsurance-ops", told(&new, " ")), "reference_required");
    assert_eq!(b.refused("reinsurance-ops", told(&old, "vault/re-1")), "nothing_to_tell");
    assert_eq!(b.refused("claims", told(&new, "vault/re-1")), "caller_not_allowed");
    b.today = d(2028, 7, 1);
    let again = b.feed(1, terms_altered("P1", "life", 2, 3, 5000 * K, d(2028, 7, 1), Some(2)));
    assert!(again
        .notices
        .iter()
        .any(|n| matches!(n, Notice::ExceptionClosed { id: closed } if closed == id)));
    let newest = key("P1", "life", 3, "L1");
    assert_eq!(b.ri.exceptions().keys().collect::<Vec<_>>(), vec!["facultative_terms_altered:P1/life/3/L1"]);
    assert_eq!(b.ri.cession(&newest, d(2028, 7, 1)), Some(&before));
    b.ok("reinsurance-ops", told(&newest, "vault/re-1"));
    assert!(b.ri.exceptions().is_empty());
    b.assert_reconciles();
    // The layer arriving again (the policy landing on another partition) asks nobody twice.
    let landed = b.feed(1, terms_altered("P1", "life", 2, 3, 5000 * K, d(2028, 7, 1), Some(2)));
    assert!(landed.notices.is_empty() && b.ri.exceptions().is_empty());
    assert_eq!(b.ri.cession(&newest, d(2028, 7, 1)), Some(&before));

    // A layer said to continue one this module never saw is simply new cover, on today's version.
    b.today = d(2028, 8, 1);
    b.issue(2, "P7", "bella", vec![]);
    b.feed(2, terms_altered("P7", "life", 4, 5, 300 * K, d(2028, 8, 1), Some(4)));
    let orphan = b.ri.cession(&key("P7", "life", 5, "L1"), d(2028, 8, 1)).unwrap();
    assert_eq!((orphan.retained_minor, orphan.treaty.as_ref().unwrap().version), (300 * K, 2));
    b.assert_reconciles();
}
