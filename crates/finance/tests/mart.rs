//! The data mart and the extracts cut from it.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use common::*;
use ic_core::command::MemberChange;
use ic_core::event::{Event, EventRecord, LayerInfo, LifeInfo, MemberCover, PayableCover};
use ic_core::{Command, LayerOrigin, LifeId, PolicyId, PolicyStatus, UnwindCause};
use ic_finance::close::{CheckStatus, VALUATION_EXTRACTS};
use ic_finance::mart::{Extract, Mart, MartError, EXPERIENCE, IN_FORCE, MOVEMENT};
use ic_finance::FinCommand;
use ic_ledger::Hash32;
use ic_product::Sex;
use ic_sim::persona::{group_scheme, issue_command, member};

fn mart_of(partitions: u16, events: &[EventRecord]) -> Mart {
    let mut mart = Mart::new(partitions);
    for e in events {
        mart.apply(e).unwrap();
    }
    mart
}

fn total(x: &Extract, name: &str) -> i128 {
    x.control[name]
}

/// The rows of an extract as maps from column name to value.
fn rows(x: &Extract) -> Vec<BTreeMap<String, String>> {
    let header: Vec<&str> = x.lines[0].split(',').collect();
    x.lines[1..]
        .iter()
        .map(|line| header.iter().zip(line.split(',')).map(|(h, v)| (h.to_string(), v.to_string())).collect())
        .collect()
}

/// Purpose: a book of retail policies and a group scheme is run through November and
/// December 2027 on two partitions, with a member joining and a death notified in December
/// for a date in November, after December's premium was billed. The November extracts cut after November closed and cut again
/// after all of December are the same file to the byte. Each month's movement extract
/// reconciles opening plus movements to closing, the backdated termination is a December
/// movement, and the real extracts registered against the published Merkle root turn the
/// ledger's `valuation_extracts` check green.
/// Why it matters: the actuary's valuation and every return are calculated from these
/// files. An extract that changed after it was cut, or whose closing book could not be
/// explained by the movements, would make every reserve calculated from it unprovable.
#[test]
fn extracts_reconcile_are_frozen_and_satisfy_the_close() {
    let start = d(2027, 11, 15);
    let mut w = World::new(2, start);
    let mut retail = Vec::new();
    for n in 1..=3 {
        let (p, id) = w.issue(n, start);
        w.pay(p, &id, &format!("r{n}"), 3 * MONTHLY);
        retail.push((p, id));
    }
    let scheme = group_scheme(
        9,
        start,
        vec![
            member("staff-1", d(1985, 1, 1), "staff", 60_000_00),
            member("exec-1", d(1975, 1, 1), "executive", 240_000_00),
        ],
    );
    let (sp, sid) = (w.core.partition_of(&scheme.group), scheme.policy.clone());
    w.core.submit(sp, "new-business", issue_command(scheme)).unwrap();
    let (nov, dec) = (period(2027, 11), period(2027, 12));
    w.until(d(2027, 12, 3));

    let mart = mart_of(2, &w.events);
    let in_force_nov = mart.in_force(nov.end()).unwrap();
    let movement_nov = mart.movement(nov.start(), nov.end()).unwrap();
    assert!(in_force_nov.reconciled && movement_nov.reconciled);
    assert_eq!(total(&in_force_nov, "policies"), 4);
    // Three retail covers and the two members' scheme cover.
    assert_eq!(total(&in_force_nov, "covers"), 5);
    assert_eq!(in_force_nov.rows(), 5);
    assert_eq!(total(&movement_nov, "opening_policies"), 0);
    assert_eq!(total(&movement_nov, "sum_insured_minor"), total(&in_force_nov, "sum_insured_minor"));
    assert!(total(&movement_nov, "premium_minor") >= 3 * i128::from(MONTHLY));
    // No name, no party and no date of birth: a life is known only by its record ID,
    // with its age at the extract date, sex and smoker status.
    assert!(in_force_nov
        .lines
        .iter()
        .all(|l| !l.contains("party") && !l.contains("employer") && !l.contains("1990")));
    let book = rows(&in_force_nov);
    let insured = book.iter().find(|r| r["product"] == "term-life").unwrap();
    assert_eq!(
        (insured["age"].as_str(), insured["sex"].as_str(), insured["smoker"].as_str()),
        ("37", "female", "false")
    );
    // A scheme member is rated on age alone.
    let exec = book.iter().find(|r| r["life"] == "exec-1").unwrap();
    assert_eq!((exec["age"].as_str(), exec["sex"].as_str(), exec["smoker"].as_str()), ("52", "", ""));

    // December: a member joins, and a death on 20 November is notified.
    let join = Command::ApplyMembership {
        policy: sid.clone(),
        changes: vec![MemberChange::Joined {
            life: member("staff-2", d(1990, 1, 1), "staff", 60_000_00),
            effective: d(2027, 12, 1),
        }],
    };
    w.core.submit(sp, "group-admin", join).unwrap();
    w.until(d(2027, 12, 16));
    let (p, id) = retail[0].clone();
    let death = Command::TerminateOnClaim {
        policy: id.clone(),
        event_date: d(2027, 11, 20),
        claim: "clm-1".to_string(),
        payer_deceased: true,
    };
    w.core.submit(p, "claims", death).unwrap();
    w.until(d(2028, 1, 4));

    let later = mart_of(2, &w.events);
    for (was, now) in [
        (&in_force_nov, later.in_force(nov.end()).unwrap()),
        (&movement_nov, later.movement(nov.start(), nov.end()).unwrap()),
    ] {
        assert_eq!(was.bytes(), now.bytes(), "the {} extract for November changed", was.kind);
        assert_eq!(was.content_hash(), now.content_hash());
        assert_eq!(was.control, now.control);
    }

    let in_force_dec = later.in_force(dec.end()).unwrap();
    let movement_dec = later.movement(dec.start(), dec.end()).unwrap();
    assert!(in_force_dec.reconciled && movement_dec.reconciled);
    assert_eq!((total(&in_force_dec, "policies"), total(&in_force_dec, "covers")), (3, 5));
    assert_eq!(total(&movement_dec, "opening_policies"), 4);
    let moved = rows(&movement_dec);
    let ended: Vec<_> = moved.iter().filter(|r| r["kind"] == "terminated").collect();
    assert_eq!(ended.iter().map(|r| r["policies"].parse::<i64>().unwrap()).sum::<i64>(), -1);
    assert!(ended.iter().all(|r| r["policy"] == id.0));
    let joined: Vec<_> = moved.iter().filter(|r| r["kind"] == "member_joined").collect();
    assert_eq!(
        (joined.len(), joined[0]["effective"].as_str(), joined[0]["life"].as_str()),
        (1, "2027-12-01", "staff-2")
    );
    // The premium billed after the death was taken back in December.
    let unwound: i64 = moved
        .iter()
        .filter(|r| r["kind"] == "premium_unwound")
        .map(|r| r["premium_minor"].parse::<i64>().unwrap())
        .sum();
    assert!(unwound < 0);

    let experience = later.experience(dec.start(), dec.end()).unwrap();
    assert!(experience.reconciled);
    assert_eq!(total(&experience, "ending_claims"), 1);
    assert!(total(&experience, "cover_days") > 31 * 4);
    let claimed = rows(&experience).into_iter().find(|r| r["ending_claims"] == "1").unwrap();
    assert_eq!(
        (claimed["age_band"].as_str(), claimed["sex"].as_str(), claimed["smoker"].as_str()),
        ("35-39", "female", "false")
    );

    // The files as they would be stored, registered with the ledger.
    assert_eq!(in_force_nov.name(), "extracts/2027-11-30/in_force-v5.csv");
    assert_eq!(in_force_nov.id(), "in_force:2027-11-30:v5");
    assert_eq!(in_force_nov.bytes().iter().filter(|b| **b == b'\n').count() as u64, in_force_nov.rows() + 1);
    let red = |w: &World| {
        let checks = w.books.state.checks(nov);
        let check = checks.iter().find(|c| c.id == VALUATION_EXTRACTS).unwrap();
        matches!(check.status, CheckStatus::Red { .. })
    };
    assert!(red(&w));
    for x in [&in_force_nov, &movement_nov] {
        let extract = x.record(w.roots[&nov.end()], &format!("worm://{}", x.name()));
        assert_eq!((extract.rows, extract.content_hash, extract.reconciled), (x.rows(), x.content_hash(), true));
        w.books.send("reporting", None, FinCommand::RegisterExtract { extract }).unwrap();
    }
    assert!(!red(&w));
}

/// Purpose: the same events fed partition by partition instead of day by day, and a mart
/// saved halfway and restored, give extracts identical to the byte.
/// Why it matters: the mart is a read model that is rebuilt after a failure or a redeploy,
/// and partitions are never read in step with each other. If the order of arrival or a
/// restart could change a file, a rebuilt mart could not reproduce a registered extract
/// and its hash would no longer prove anything.
#[test]
fn a_rebuilt_mart_cuts_the_same_files() {
    let start = d(2027, 11, 15);
    let mut w = World::new(2, start);
    for n in 1..=4 {
        w.issue(n, start);
    }
    w.until(d(2027, 12, 2));
    let as_at = d(2027, 11, 30);
    let first = mart_of(2, &w.events);

    let mut by_partition = w.events.clone();
    by_partition.sort_by_key(|e| std::cmp::Reverse(e.partition));
    let reordered = mart_of(2, &by_partition);

    let (head, tail) = w.events.split_at(w.events.len() / 2);
    let saved = serde_json::to_string(&mart_of(2, head)).unwrap();
    let mut restored: Mart = serde_json::from_str(&saved).unwrap();
    // A restart replays from a little before where it stopped.
    for e in head.iter().rev().take(5).chain(tail) {
        restored.apply(e).unwrap();
    }
    assert_eq!(restored, first);

    for other in [&reordered, &restored] {
        assert_eq!(other.in_force(as_at).unwrap().bytes(), first.in_force(as_at).unwrap().bytes());
        assert_eq!(other.movement(start, as_at).unwrap().bytes(), first.movement(start, as_at).unwrap().bytes());
        assert_eq!(other.experience(start, as_at).unwrap().bytes(), first.experience(start, as_at).unwrap().bytes());
    }
    assert_eq!(first.in_force(as_at).unwrap().kind, IN_FORCE);
    assert_eq!(first.movement(start, as_at).unwrap().kind, MOVEMENT);
    assert_eq!(first.experience(start, as_at).unwrap().kind, EXPERIENCE);
}

/// Purpose: an extract is refused as at a date some partition has not closed, an event that
/// skips ahead is refused with the sequence wanted, an event from a partition that does not
/// exist is refused, and a period that ends before it starts is refused.
/// Why it matters: an extract cut while one partition was still trading would be missing
/// that partition's last movements and would still hash and register cleanly. The refusals
/// are what stop a wrong file from being produced at all. The refusal is in a finance
/// user's words, not the cluster's (found by the scenario run, SCN-17, F-SCN17-8).
#[test]
fn an_extract_is_cut_only_from_a_complete_closed_book() {
    let start = d(2027, 11, 15);
    let mut w = World::new(2, start);
    w.issue(1, start);
    w.days(2);
    let closed = d(2027, 11, 16);

    let whole = mart_of(2, &w.events);
    assert_eq!(whole.closed_through(), Some(closed));
    assert!(whole.in_force(closed).is_ok());
    let open = d(2027, 11, 17);
    assert_eq!(whole.in_force(open).unwrap_err(), MartError::NotClosed { partition: 0, date: open });
    assert!(matches!(whole.movement(start, open), Err(MartError::NotClosed { .. })));
    assert!(matches!(whole.experience(start, open), Err(MartError::NotClosed { .. })));
    assert_eq!(whole.movement(closed, start).unwrap_err(), MartError::Range { from: closed, to: start });
    assert_eq!(whole.experience(closed, start).unwrap_err(), MartError::Range { from: closed, to: start });

    // One partition's events only: the other has closed nothing.
    let one: Vec<EventRecord> = w.events.iter().filter(|e| e.partition == 0).cloned().collect();
    let half = mart_of(2, &one);
    assert_eq!(half.closed_through(), None);
    assert_eq!(half.in_force(closed).unwrap_err(), MartError::NotClosed { partition: 1, date: closed });
    assert_eq!(
        half.in_force(closed).unwrap_err().to_string(),
        "the books are not yet fed to 2027-11-16: policy store 1 has not closed that day, and the extract can be made once it has"
    );

    let mut mart = Mart::new(2);
    assert_eq!(mart.next_seq(0), Some(1));
    assert_eq!(mart.next_seq(2), None);
    assert_eq!(
        mart.apply(&one[1]).unwrap_err(),
        MartError::Gap {
            partition: 0,
            expected: 1,
            got: 2
        }
    );
    let mut stray = one[0].clone();
    stray.partition = 7;
    assert_eq!(mart.apply(&stray).unwrap_err(), MartError::Partition(7));
    assert_eq!(mart, Mart::new(2), "a refused event leaves the mart as it was");
}

/// Builds one partition's event stream by hand.
struct Stream {
    seq: u64,
    date: NaiveDate,
    events: Vec<EventRecord>,
}

impl Stream {
    fn new(date: NaiveDate) -> Self {
        let mut s = Self { seq: 0, date, events: vec![] };
        s.push(Event::BusinessDateAdvanced {
            from: None,
            to: date,
            closing_head: Hash32::ZERO,
        });
        s
    }

    fn push(&mut self, event: Event) {
        self.seq += 1;
        self.events.push(EventRecord {
            id: format!("0-{}", self.seq),
            partition: 0,
            seq: self.seq,
            business_date: self.date,
            log_index: self.seq,
            traceparent: None,
            event,
        });
    }

    /// Close the open date and open `to`.
    fn close(&mut self, to: NaiveDate) {
        let from = self.date;
        self.date = to;
        self.push(Event::BusinessDateAdvanced {
            from: Some(from),
            to,
            closing_head: Hash32::ZERO,
        });
    }
}

fn layer(cover: &str, layer: u32, life: &str, sum: i64, start: NaiveDate) -> LayerInfo {
    LayerInfo {
        cover: cover.to_string(),
        cover_type: format!("{cover}-type"),
        layer,
        life: LifeId::from(life),
        sum_insured_minor: sum,
        start,
        option: None,
    }
}

fn member_cover(life: &str, sum: i64) -> MemberCover {
    MemberCover {
        cover: "group-life".to_string(),
        cover_type: "life".to_string(),
        life: LifeId::from(life),
        sum_insured_minor: sum,
    }
}

/// Purpose: one retail policy and one scheme are walked through every kind of change the
/// core can publish (increase, indexation, merge, a layer given up, an expiry and its
/// unwind, an unwound layer, lapse and reinstatement, members changing and leaving,
/// premium adjustments, a transfer out and a transfer in, a terminating claim), closing a
/// business day after each. After every day the in-force book equals everything that has
/// moved, each day's movement reconciles to the day before, and the totals are the ones
/// worked out by hand. Then the mart's history is trimmed at each day in turn, after all
/// the events and part way through them: every extract from the cut day on is the same
/// file to the byte, anything earlier is refused, and the mart is smaller.
/// Why it matters: the mart lives in replicated state and would otherwise grow for ever;
/// trimming is only safe if no file that can still be cut changes by a byte.
/// The simulator's ordinary months exercise only a few of these events.
/// Each one changes the sum insured the actuary reserves for, and one handled wrongly
/// would put the book and its movements out by an amount nobody would look for.
#[test]
fn every_kind_of_change_keeps_the_book_and_its_movements_in_step() {
    let day = |n: u32| d(2028, 3, n);
    let (pol, sch, moved_in) = (PolicyId::from("pol-1"), PolicyId::from("sch-1"), PolicyId::from("pol-9"));
    let mut s = Stream::new(day(1));
    // After each close: (policies, covers, sum insured) as at the day just closed.
    let mut expected: Vec<(NaiveDate, i128, i128, i128)> = Vec::new();
    let mut step = |s: &mut Stream, events: Vec<Event>, totals: (i128, i128, i128)| {
        for e in events {
            s.push(e);
        }
        let closed = s.date;
        s.close(closed.succ_opt().unwrap());
        expected.push((closed, totals.0, totals.1, totals.2));
    };

    let issued = |policy: &PolicyId, product: &str, layers: Vec<LayerInfo>, members: Vec<MemberCover>| Event::PolicyIssued {
        policy: policy.clone(),
        number: "N".to_string(),
        group: "grp".into(),
        policyholder: "party-x".into(),
        product_code: product.to_string(),
        product_hash: "h".to_string(),
        inception: day(1),
        lives: vec![LifeInfo {
            date_of_birth: Some(d(1988, 3, 5)),
            sex: Some(Sex::Male),
            smoker: Some(true),
            ..LifeInfo::new("l1".into(), None)
        }],
        layers,
        members,
    };
    let status = |policy: &PolicyId, from, to| Event::PolicyStatusChanged {
        policy: policy.clone(),
        from,
        to,
    };

    // Day 1: a retail policy with life and trauma cover, and a scheme with two members.
    step(
        &mut s,
        vec![
            issued(
                &pol,
                "term-life",
                vec![layer("life", 1, "l1", 500, day(1)), layer("trauma", 1, "l1", 100, day(1))],
                vec![],
            ),
            issued(&sch, "group-life", vec![], vec![member_cover("m1", 300), member_cover("m2", 400)]),
            Event::PremiumDue {
                discounts: vec![],
                policy: pol.clone(),
                receivable: 1,
                period_start: day(1),
                period_end: d(2028, 3, 31),
                total_minor: 60,
                members: None,
                by_cover_type: BTreeMap::new(),
            },
        ],
        (2, 4, 1300),
    );
    // Day 2: an increase and an indexation step on the life cover.
    step(
        &mut s,
        vec![
            Event::LayerAdded {
                policy: pol.clone(),
                layer: layer("life", 2, "l1", 200, day(2)),
                origin: LayerOrigin::Increase,
                continues: None,
            },
            Event::LayerAdded {
                policy: pol.clone(),
                layer: layer("life", 3, "l1", 50, day(2)),
                origin: LayerOrigin::Indexation,
                continues: None,
            },
        ],
        (2, 6, 1550),
    );
    // Day 3: layers 2 and 3 are merged into layer 1. Two covers fewer, the same sum insured.
    step(
        &mut s,
        vec![Event::LayersMerged {
            policy: pol.clone(),
            cover: "life".to_string(),
            removed: vec![2, 3],
            into: 1,
            sum_insured_minor: 750,
        }],
        (2, 4, 1550),
    );
    // Day 4: trauma expires; a member's salary changes; an underwritten layer is written
    // for a member and the premium is adjusted for it.
    step(
        &mut s,
        vec![
            Event::CoverExpired {
                policy: pol.clone(),
                cover: "trauma".to_string(),
                on: day(4),
            },
            Event::MemberChanged {
                policy: sch.clone(),
                life: "m1".into(),
                effective: day(2),
                covers: vec![member_cover("m1", 350)],
                info: None,
            },
            Event::LayerAdded {
                policy: sch.clone(),
                layer: layer("group-life", 1, "m2", 1000, day(4)),
                origin: LayerOrigin::UnderwrittenExcess,
                continues: None,
            },
            Event::PremiumAdjusted {
                discounts: vec![],
                policy: sch.clone(),
                receivable: 2,
                life: "m2".into(),
                effective: day(4),
                total_minor: 15,
            },
            Event::LoadingAdjusted {
                policy: sch.clone(),
                receivable: 3,
                total_minor: 5,
            },
        ],
        (2, 4, 2500),
    );
    // Day 5: the expiry is unwound, so trauma is back; a new trauma layer written in error is unwound.
    step(
        &mut s,
        vec![
            Event::CoverExpiryUnwound {
                policy: pol.clone(),
                cover: "trauma".to_string(),
                layers: vec![layer("trauma", 1, "l1", 100, day(1))],
                reference: "u1".to_string(),
            },
            Event::LayerAdded {
                policy: pol.clone(),
                layer: layer("trauma", 2, "l1", 40, day(5)),
                origin: LayerOrigin::Original,
                continues: None,
            },
            Event::LayerUnwound {
                policy: pol.clone(),
                cover: "trauma".to_string(),
                layer: 2,
                start: day(5),
                reference: "u1".to_string(),
            },
        ],
        (2, 5, 2600),
    );
    // Day 6: the retail policy lapses. Its covers leave the book though none has ended.
    step(&mut s, vec![status(&pol, PolicyStatus::InForce, PolicyStatus::Lapsed)], (1, 3, 1750));
    // Day 7: a member leaves, with the underwritten layer; a change on the lapsed policy moves nothing.
    step(
        &mut s,
        vec![
            Event::MemberLeft {
                policy: sch.clone(),
                life: "m2".into(),
                effective: day(7),
            },
            Event::LayerEnded {
                policy: sch.clone(),
                cover: "group-life".to_string(),
                layer: 1,
                life: "m2".into(),
                effective: day(7),
                sum_insured_minor: 1000,
            },
            Event::LayerAdded {
                policy: pol.clone(),
                layer: layer("life", 4, "l1", 25, day(7)),
                origin: LayerOrigin::Increase,
                continues: None,
            },
        ],
        (1, 1, 350),
    );
    // Day 8: the retail policy is reinstated, with the layer added while it was lapsed.
    step(
        &mut s,
        vec![
            status(&pol, PolicyStatus::Lapsed, PolicyStatus::InForce),
            Event::PolicyReinstated { policy: pol.clone() },
        ],
        (2, 4, 1225),
    );
    // Day 9: a member joins; a policy arrives from another partition; the scheme's first
    // premium is billed and then unwound.
    step(
        &mut s,
        vec![
            Event::MemberJoined {
                policy: sch.clone(),
                life: "m3".into(),
                effective: day(9),
                covers: vec![member_cover("m3", 500)],
                party: None,
                info: None,
            },
            Event::TransferLanded {
                policy: moved_in.clone(),
                transfer_id: "t1".to_string(),
                group: "grp-9".into(),
                from_partition: 3,
                lives: vec![],
                layers: vec![layer("life", 1, "l9", 900, d(2026, 1, 1))],
                members: vec![member_cover("m9", 10)],
                number: "N9".to_string(),
                product_code: "term-life".to_string(),
                status: PolicyStatus::InForce,
                standing: None,
            },
            Event::PolicyUnwound {
                policy: sch.clone(),
                after: day(3),
                cause: UnwindCause::Claim,
                reference: "u2".to_string(),
                receivables: vec![2, 3, 77],
            },
        ],
        (3, 7, 2635),
    );
    // Day 10: the retail policy ends with a death claim; the scheme moves to another
    // partition. Events about a policy the mart never met are ignored.
    let unknown = PolicyId::from("pol-unknown");
    step(
        &mut s,
        vec![
            status(&pol, PolicyStatus::InForce, PolicyStatus::Terminated),
            Event::PolicyTerminated {
                policy: pol.clone(),
                effective: day(8),
                claim: "clm-1".to_string(),
                payable: vec![PayableCover {
                    cover: "life".to_string(),
                    cover_type: "life-type".to_string(),
                    sum_insured_minor: 775,
                }],
            },
            status(&sch, PolicyStatus::InForce, PolicyStatus::Transferred),
            status(&unknown, PolicyStatus::InForce, PolicyStatus::Lapsed),
            Event::PolicyTerminated {
                policy: unknown.clone(),
                effective: day(8),
                claim: "clm-2".to_string(),
                payable: vec![],
            },
            Event::PolicyUnwound {
                policy: unknown.clone(),
                after: day(3),
                cause: UnwindCause::Claim,
                reference: "u3".to_string(),
                receivables: vec![1],
            },
            Event::LoadingAdjusted {
                policy: unknown,
                receivable: 1,
                total_minor: 5,
            },
        ],
        (1, 2, 910),
    );

    let mart = mart_of(1, &s.events);
    let mut previous = (0, 0, 0);
    for (date, policies, covers, sum) in expected {
        let book = mart.in_force(date).unwrap();
        assert!(book.reconciled, "the book as at {date} is not what has moved");
        assert_eq!(
            (total(&book, "policies"), total(&book, "covers"), total(&book, "sum_insured_minor")),
            (policies, covers, sum),
            "as at {date}"
        );
        assert_eq!(book.rows() as i128, covers);
        let moved = mart.movement(date, date).unwrap();
        assert!(moved.reconciled, "the movements of {date} do not explain its close");
        assert_eq!(
            (
                total(&moved, "opening_policies"),
                total(&moved, "opening_covers"),
                total(&moved, "opening_sum_insured_minor")
            ),
            previous
        );
        previous = (policies, covers, sum);
    }

    let all = mart.movement(day(1), day(10)).unwrap();
    assert!(all.reconciled);
    // Billed 60, adjusted 15 and 5, and the 20 unwound: the receivable never seen is not guessed at.
    assert_eq!(total(&all, "premium_minor"), 60);
    let moved = rows(&all);
    let kinds: Vec<&str> = moved.iter().map(|r| r["kind"].as_str()).collect();
    for kind in [
        "new_business",
        "increase",
        "indexation",
        "layers_merged",
        "cover_expired",
        "member_changed",
        "underwritten_excess",
        "premium_adjusted",
        "loading_adjusted",
        "expiry_unwound",
        "new_cover",
        "unwound",
        "lapsed",
        "member_left",
        "cover_ended",
        "in_force",
        "member_joined",
        "transfer_in",
        "premium_unwound",
        "terminated",
        "transfer_out",
    ] {
        assert!(kinds.contains(&kind), "no {kind} movement");
    }
    // The member's change is one row for the difference, dated when it took effect.
    let changed = moved.iter().find(|r| r["kind"] == "member_changed").unwrap();
    assert_eq!(
        (changed["sum_insured_minor"].as_str(), changed["covers"].as_str(), changed["effective"].as_str()),
        ("50", "0", "2028-03-02")
    );
    // A change while the policy was lapsed moved nothing until it was reinstated.
    assert!(moved.iter().all(|r| !(r["business_date"] == "2028-03-07" && r["policy"] == "pol-1")));

    // Exposure: days in force only, and the claim that ended the policy.
    let experience = mart.experience(day(1), day(10)).unwrap();
    assert!(experience.reconciled);
    assert_eq!(total(&experience, "ending_claims"), 1);
    let cells = rows(&experience);
    let life: Vec<_> = cells.iter().filter(|r| r["product"] == "term-life" && r["cover_type"] == "life-type").collect();
    // pol-9 arrived with no factors: days 9 and 10, in a cell of its own.
    assert_eq!(
        (life[0]["age_band"].as_str(), life[0]["sex"].as_str(), life[0]["cover_days"].as_str()),
        ("", "", "2")
    );
    // pol-1 life: one layer on day 1, three on day 2, one on days 3 to 5, two on days 8
    // and 9, none while lapsed or after the claim. The life turned 40 on 5 March.
    assert_eq!(
        (
            life[1]["age_band"].as_str(),
            life[1]["sex"].as_str(),
            life[1]["smoker"].as_str(),
            life[1]["cover_days"].as_str()
        ),
        ("40-44", "male", "true", "11")
    );
    assert_eq!(life[1]["ending_claim_sum_insured_minor"], "775");
    // Age is as at the day of each movement: 39 when issued, 40 when the layer was added on 7 March.
    let ages = |kind: &str| {
        moved
            .iter()
            .filter(|r| r["kind"] == kind && r["policy"] == "pol-1" && !r["life"].is_empty())
            .map(|r| r["age"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ages("new_business"), ["39", "39"]);
    assert_eq!(ages("in_force").last().map(String::as_str), Some("40"));
    let lapsed_days = mart.experience(day(6), day(7)).unwrap();
    assert!(rows(&lapsed_days).iter().all(|r| r["product"] != "term-life"));

    // Trimming. Whatever day the history is cut at, and whether it is cut after all the
    // events or part way through them, every extract from that day on is the same file.
    let through = mart.closed_through().unwrap();
    let same = |trimmed: &Mart, cut: u32| {
        let mut n = day(cut);
        while n <= through {
            let files = |m: &Mart| {
                let (book, moved, lived, one) = (
                    m.in_force(n).unwrap(),
                    m.movement(day(cut), n).unwrap(),
                    m.experience(day(cut), n).unwrap(),
                    m.movement(n, n).unwrap(),
                );
                assert!(
                    book.reconciled && moved.reconciled && lived.reconciled && one.reconciled,
                    "cut at {cut}, as at {n}"
                );
                [book.bytes(), moved.bytes(), lived.bytes(), one.bytes()]
            };
            assert_eq!(files(trimmed), files(&mart), "cut at {cut}, as at {n}");
            n = n.succ_opt().unwrap();
        }
    };
    let size = |m: &Mart| serde_json::to_vec(m).unwrap().len();
    for cut in 2..=10 {
        let mut after = mart.clone();
        after.trim(day(cut));
        same(&after, cut);
        assert!(size(&after) < size(&mart));
        let gone = MartError::Trimmed {
            date: day(cut - 1),
            kept_from: day(cut),
        };
        assert_eq!(after.in_force(day(cut - 1)).unwrap_err(), gone);
        assert_eq!(after.movement(day(cut - 1), through).unwrap_err(), gone);
        assert_eq!(after.experience(day(cut - 1), through).unwrap_err(), gone);
        // Trimming to an earlier day afterwards gives nothing back and takes nothing more.
        let before = after.clone();
        after.trim(day(1));
        assert_eq!(after, before);

        // Cut part way through: the events of the cut day and later arrive after the trim.
        let mut during = Mart::new(1);
        let mut cut_done = false;
        for e in &s.events {
            if !cut_done && e.business_date > day(cut) {
                during.trim(day(cut));
                cut_done = true;
            }
            during.apply(e).unwrap();
        }
        assert_eq!(during.kept_from(), cut_done.then_some(day(cut)));
        if cut_done {
            same(&during, cut);
        }
    }
    // Two cuts, one after the other, are the later cut.
    let (mut twice, mut once) = (mart.clone(), mart.clone());
    twice.trim(day(4));
    twice.trim(day(7));
    once.trim(day(7));
    assert_eq!(twice, once);
}

/// Purpose: a scheme is issued with one member, a second joins the next day with her
/// factors (born 1980, female, non-smoker, occupation class 2, benefit category staff), and
/// three days later the employer corrects her to a smoker in a class and a category each
/// typed with a comma in it. The in-force and movement extracts carry her age, sex,
/// smoker status, class and category from the day she
/// joined; the extract as at the day she joined is the same file to the byte after the
/// correction; the later extract carries the corrected factors with the comma replaced;
/// a notice with no factors changes nothing; and the experience extract splits her
/// exposure between the two classes by what was recorded at each period end. The covers
/// handed to a table dry run carry her category as the core published it.
/// Why it matters: most of a scheme's members join after issue, so without this the
/// extracts for group business would be blank in every rating column. And a correction
/// must not rewrite a file the actuary has already valued from.
#[test]
fn a_member_who_joins_after_issue_carries_her_factors_from_that_day() {
    let day = |n: u32| d(2028, 3, n);
    let sch = PolicyId::from("sch-1");
    let mut s = Stream::new(day(1));
    let info = |smoker: bool, class: &str| {
        Some(LifeInfo {
            date_of_birth: Some(d(1980, 6, 1)),
            sex: Some(Sex::Female),
            smoker: Some(smoker),
            occupation_class: Some(class.to_string()),
            category: Some(if smoker { "exec, senior" } else { "staff" }.to_string()),
            ..LifeInfo::new("m2".into(), None)
        })
    };
    s.push(Event::PolicyIssued {
        policy: sch.clone(),
        number: "N".to_string(),
        group: "grp".into(),
        policyholder: "party-x".into(),
        product_code: "group-life".to_string(),
        product_hash: "h".to_string(),
        inception: day(1),
        lives: vec![],
        layers: vec![],
        members: vec![member_cover("m1", 300)],
    });
    s.close(day(2));
    s.push(Event::MemberJoined {
        policy: sch.clone(),
        life: "m2".into(),
        effective: day(2),
        covers: vec![member_cover("m2", 400)],
        party: None,
        info: info(false, "2"),
    });
    s.close(day(3));
    let before = mart_of(1, &s.events).in_force(day(2)).unwrap();
    let joined = rows(&before).into_iter().find(|r| r["life"] == "m2").unwrap();
    assert_eq!(
        (
            joined["age"].as_str(),
            joined["sex"].as_str(),
            joined["smoker"].as_str(),
            joined["occupation_class"].as_str(),
            joined["category"].as_str()
        ),
        ("47", "female", "false", "2", "staff")
    );
    let first = rows(&before).into_iter().find(|r| r["life"] == "m1").unwrap();
    assert_eq!((first["age"].as_str(), first["occupation_class"].as_str()), ("", ""));

    s.close(day(4));
    s.close(day(5));
    // A notice with no factors, then the correction.
    s.push(Event::MemberChanged {
        policy: sch.clone(),
        life: "m2".into(),
        effective: day(5),
        covers: vec![member_cover("m2", 400)],
        info: None,
    });
    s.push(Event::MemberChanged {
        policy: sch.clone(),
        life: "m2".into(),
        effective: day(5),
        covers: vec![member_cover("m2", 450)],
        info: info(true, "4,heavy"),
    });
    s.close(day(6));
    let mart = mart_of(1, &s.events);
    assert_eq!(mart.in_force(day(2)).unwrap().bytes(), before.bytes(), "the extract as at 2 March changed");
    let after = rows(&mart.in_force(day(5)).unwrap()).into_iter().find(|r| r["life"] == "m2").unwrap();
    assert_eq!(
        (
            after["smoker"].as_str(),
            after["occupation_class"].as_str(),
            after["category"].as_str(),
            after["sum_insured_minor"].as_str()
        ),
        ("true", "4_heavy", "exec__senior", "450")
    );

    let moved = rows(&mart.movement(day(1), day(5)).unwrap());
    let classes: Vec<&str> = moved.iter().filter(|r| r["life"] == "m2").map(|r| r["occupation_class"].as_str()).collect();
    assert_eq!(classes.first(), Some(&"2"));
    assert_eq!(classes.last(), Some(&"4_heavy"));
    assert!(mart.movement(day(1), day(5)).unwrap().reconciled);

    // Exposure sits in the cell of the factors recorded at the end of the period asked.
    let cell = |from, to, class: &str| {
        let x = mart.experience(from, to).unwrap();
        assert!(x.reconciled);
        rows(&x).into_iter().find(|r| r["occupation_class"] == class).map(|r| r["cover_days"].clone())
    };
    assert_eq!(cell(day(2), day(4), "2").as_deref(), Some("3"));
    assert_eq!(cell(day(2), day(5), "4_heavy").as_deref(), Some("4"));
    assert_eq!(cell(day(2), day(5), "2"), None);
    let categories: BTreeSet<String> = rows(&mart.experience(day(2), day(5)).unwrap())
        .into_iter()
        .map(|r| r["category"].clone())
        .collect();
    assert_eq!(categories, BTreeSet::from([String::new(), "exec__senior".to_string()]));
    let priced = mart.rated_covers(day(5), &BTreeSet::from(["group-life".to_string()])).unwrap();
    assert!(priced.iter().any(|c| c.category.as_deref() == Some("exec, senior")), "{priced:?}");
}

/// A fact as the claims module publishes it through the core.
fn claim_fact(kind: &str, claim: &str, detail: &[(&str, &str)]) -> Event {
    Event::Module {
        module: "claims".to_string(),
        kind: kind.to_string(),
        subject: claim.to_string(),
        detail: detail.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

/// Purpose: a policy with life and trauma cover on a 39 year old male smoker has a trauma
/// claim admitted for 100 on 2 March and a benefit in payment of 50 a month admitted on
/// the life cover on 3 March; neither ends the policy. A further 20 is admitted on the
/// trauma claim on 4 March and the whole admission is reversed on 5 March. The experience
/// extract counts each claim once in its rating cell on the day it was admitted, with the
/// sum admitted and the monthly benefit; the further admission adds money and no claim;
/// the reversal takes one claim and 120 back on the day it was decided, so the extract for
/// 1 to 3 March is the same file afterwards and the whole period nets to nothing for
/// trauma. Exposure is untouched throughout. Facts the mart cannot place (an admission
/// for a claim never registered, an amount that is not a number, a second registration, a
/// reversal of nothing) change no cell that has a product.
/// Why it matters: trauma, income and health claims leave the policy in force, so an
/// experience study built from terminations alone sees none of them and understates
/// morbidity for every product that is not plain death cover.
#[test]
fn claims_that_leave_the_policy_in_force_are_on_the_experience_extract() {
    let day = |n: u32| d(2028, 3, n);
    let pol = PolicyId::from("pol-1");
    let mut s = Stream::new(day(1));
    s.push(Event::PolicyIssued {
        policy: pol.clone(),
        number: "N".to_string(),
        group: "grp".into(),
        policyholder: "party-x".into(),
        product_code: "term-life".to_string(),
        product_hash: "h".to_string(),
        inception: day(1),
        lives: vec![LifeInfo {
            date_of_birth: Some(d(1988, 6, 5)),
            sex: Some(Sex::Male),
            smoker: Some(true),
            ..LifeInfo::new("l1".into(), None)
        }],
        layers: vec![layer("life", 1, "l1", 500, day(1)), layer("trauma", 1, "l1", 100, day(1))],
        members: vec![],
    });
    s.close(day(2));
    let reserved = |claim: &str, cover: &str, life: &str| {
        claim_fact(
            "claim_reserved",
            claim,
            &[
                ("policy", "pol-1"),
                ("cover", cover),
                ("life", life),
                ("event_date", "2028-03-01"),
                ("gross_reserve_minor", "100"),
            ],
        )
    };
    s.push(reserved("clm-1", "trauma", "l1"));
    s.push(claim_fact("claim_admitted", "clm-1", &[("admitted_minor", "100")]));
    s.close(day(3));
    // No life named: the cover is on one life, so the claim is hers.
    s.push(reserved("clm-2", "life", ""));
    s.push(claim_fact("claim_admitted", "clm-2", &[("admitted_minor", "0"), ("monthly_minor", "50")]));
    s.close(day(4));
    let early = mart_of(1, &s.events).experience(day(1), day(3)).unwrap();
    let cell = |x: &Extract, cover_type: &str| {
        let r = rows(x)
            .into_iter()
            .find(|r| r["product"] == "term-life" && r["cover_type"] == cover_type)
            .unwrap();
        assert_eq!((r["age_band"].as_str(), r["sex"].as_str(), r["smoker"].as_str()), ("35-39", "male", "true"));
        (
            r["cover_days"].clone(),
            r["admitted_claims"].clone(),
            r["admitted_minor"].clone(),
            r["admitted_monthly_minor"].clone(),
        )
    };
    let of = |a: &str, b: &str, c: &str, e: &str| (a.to_string(), b.to_string(), c.to_string(), e.to_string());
    assert!(early.reconciled);
    assert_eq!(cell(&early, "trauma-type"), of("3", "1", "100", "0"));
    assert_eq!(cell(&early, "life-type"), of("3", "1", "0", "50"));
    assert_eq!(
        (
            total(&early, "admitted_claims"),
            total(&early, "admitted_minor"),
            total(&early, "ending_claims")
        ),
        (2, 100, 0)
    );

    // 4 March: more admitted on the same claim, and facts the mart cannot place.
    s.push(claim_fact("claim_admitted", "clm-1", &[("admitted_minor", "20")]));
    s.push(reserved("clm-1", "life", "l1"));
    s.push(claim_fact("claim_admitted", "clm-lost", &[("admitted_minor", "not-a-number")]));
    s.push(claim_fact("admission_reversed", "clm-never", &[("decided_on", "2028-03-04")]));
    s.push(claim_fact("claim_closed", "clm-2", &[("outcome", "paid")]));
    s.close(day(5));
    s.push(claim_fact("admission_reversed", "clm-1", &[("decided_on", "2028-03-05")]));
    s.close(day(6));

    let mart = mart_of(1, &s.events);
    assert_eq!(
        mart.experience(day(1), day(3)).unwrap().bytes(),
        early.bytes(),
        "the extract for 1 to 3 March changed"
    );
    let late = mart.experience(day(4), day(5)).unwrap();
    assert_eq!(cell(&late, "trauma-type"), of("2", "-1", "-100", "0"));
    assert_eq!(cell(&late, "life-type"), of("2", "0", "0", "0"));
    // The claim nobody registered is counted where it can be seen, with no product.
    let lost = rows(&late).into_iter().find(|r| r["product"].is_empty()).unwrap();
    assert_eq!(
        (lost["admitted_claims"].as_str(), lost["admitted_minor"].as_str(), lost["cover_days"].as_str()),
        ("1", "0", "0")
    );
    let whole = mart.experience(day(1), day(5)).unwrap();
    assert_eq!(cell(&whole, "trauma-type"), of("5", "0", "0", "0"));
    assert_eq!(cell(&whole, "life-type"), of("5", "1", "0", "50"));
    assert!(whole.reconciled);
    // A snapshot of the mart carries the claims with it.
    let copy: Mart = serde_json::from_value(serde_json::to_value(&mart).unwrap()).unwrap();
    assert_eq!(copy.experience(day(1), day(5)).unwrap().bytes(), whole.bytes());
}

/// Purpose: the real claims module, hosted in the real core, admits five provider invoices
/// of $95 on a health policy straight through, each capped at the $60 limit and each
/// with the cause code GP-INF (tag `infection.viral`). The mart, fed only the core's
/// event stream, counts five admitted claims of $300 in the health product's cell, with
/// the insured's age band, and the policy stays in force; the claims by cause extract
/// shows them under `infection`, with neither the code nor the lower tag.
/// Why it matters: the mart reads the claims module's facts by the names of their keys.
/// The other tests write those facts by hand, so this is the one that fails if the
/// claims module renames a key or stops publishing a fact, which would otherwise show
/// only as claims quietly missing from the actuary's experience study.
#[test]
fn the_real_claims_module_feeds_the_experience_extract() {
    use ic_core::claims_module as claims;
    let start = d(2027, 11, 15);
    let mut w = World::new(2, start);
    let approved = || ("config-ann".to_string(), vec!["config-bob".to_string(), "config-cat".to_string()]);
    let rules = claims::Rules {
        version: 1,
        straight_through_max_minor: [(claims::ClaimType::Health, 2_000_00)].into(),
        health_benefits: [(
            "gp".to_string(),
            claims::config::HealthBenefit {
                per_event_minor: 60_00,
                per_year_minor: 600_00,
                lifetime_minor: i64::MAX / 4,
                copay_pct: 0,
                excess_minor: 0,
            },
        )]
        .into(),
        cause_codes: [("GP-INF".to_string(), vec!["infection.viral".to_string()])].into(),
        prepared_by: approved().0,
        approved_by: approved().1,
        ..claims::Rules::default()
    };
    let wrap = |c: claims::Command| Command::Claims(Box::new(c));
    w.core
        .activate_everywhere(wrap(claims::Command::ActivateRules { rules: Box::new(rules) }))
        .unwrap();

    let book = claims::AuthorityBook {
        version: 1,
        profiles: Default::default(),
        staff: Default::default(),
        prepared_by: approved().0,
        approved_by: approved().1,
    };
    w.core
        .activate_everywhere(wrap(claims::Command::ActivateAuthority { book: Box::new(book) }))
        .unwrap();

    let issue = ic_sim::persona::health_family(1, start, &[d(1988, 5, 5)], ic_product::Frequency::Monthly);
    let (p, id) = (w.core.partition_of(&issue.group), issue.policy.clone());
    w.core.submit(p, "new-business", issue_command(issue)).unwrap();
    w.pay(p, &id, "r-h", 2_000_00);
    w.until(d(2027, 11, 25));
    // Five invoices, so that the claims by cause row is large enough to show.
    for n in 0..5 {
        let invoice = claims::command::Notification {
            claim: format!("clm-h{n}"),
            policy: id.0.clone(),
            cover: "hospital".into(),
            life: Some("life-1".into()),
            claim_type: claims::ClaimType::Health,
            event_date: d(2027, 11, 25),
            channel: claims::Channel::Provider,
            notifier: claims::Notifier::Provider,
            claimant: Some("party-claimant".into()),
            claimed_minor: None,
            flags: claims::Flags::default(),
            health: Some(claims::HealthItem {
                category: "gp".into(),
                invoice_minor: 95_00,
                provider: Some("provider-7".into()),
                pre_approval_request: false,
                settles_pre_approval: None,
                service_code: None,
                service_date: None,
            }),
            underwriters: vec![],
            catastrophe_event: None,
            cause_code: Some("GP-INF".into()),
        };
        w.core.submit(p, "claims", wrap(claims::Command::Notify(Box::new(invoice)))).unwrap();
    }
    w.until(d(2027, 12, 2));

    let nov = period(2027, 11);
    let mart = mart_of(2, &w.events);
    let experience = mart.experience(nov.start(), nov.end()).unwrap();
    assert!(experience.reconciled);
    assert_eq!((total(&experience, "admitted_claims"), total(&experience, "admitted_minor")), (5, 300_00));
    let cell = rows(&experience).into_iter().find(|r| r["admitted_claims"] == "5").unwrap();
    assert!(
        !cell["product"].is_empty() && !cell["cover_type"].is_empty(),
        "the claim was not placed: {cell:?}"
    );
    assert_eq!(cell["age_band"], "35-39");
    assert_ne!(cell["cover_days"], "0", "the policy the claim is on has no exposure in its cell");
    assert_eq!(total(&experience, "ending_claims"), 0);
    // The cause reaches the mart as its category only.
    let by_cause = mart.claims_by_cause(nov.start(), nov.end()).unwrap();
    let row = rows(&by_cause).into_iter().next().unwrap();
    assert_eq!(
        (row["cause"].as_str(), row["claims"].as_str(), row["admitted_minor"].as_str()),
        ("infection", "5", "30000")
    );
    assert!(!by_cause.lines.concat().contains("GP-INF") && !by_cause.lines.concat().contains("viral"));
}

/// Purpose: two policies each billed 60 are avoided for non-disclosure on 3 March, one
/// with every premium refunded and one with the premium kept. Both leave the in-force
/// book that day as an `avoided` movement and the movements still reconcile. The refund
/// takes the 60 billed back off premium as its own movement, dated to inception; where
/// the insurer keeps the premium the 60 stands. The in-force extract as at 2 March still
/// shows both policies.
/// Why it matters: an avoided policy is treated as never having been in force, so its
/// refunded premium must not stay in the premium the actuary sees, and the policy must
/// not sit in the book attracting reserves. But the files already cut said it was in
/// force, and they must keep saying so.
#[test]
fn an_avoided_policy_leaves_the_book_and_a_refund_takes_its_premium_back() {
    use ic_core::PremiumOnAvoidance;
    let day = |n: u32| d(2028, 3, n);
    let mut s = Stream::new(day(1));
    for name in ["pol-refund", "pol-retain"] {
        s.push(Event::PolicyIssued {
            policy: PolicyId::from(name),
            number: "N".to_string(),
            group: "grp".into(),
            policyholder: "party-x".into(),
            product_code: "term-life".to_string(),
            product_hash: "h".to_string(),
            inception: day(1),
            lives: vec![],
            layers: vec![layer("life", 1, "l1", 500, day(1))],
            members: vec![],
        });
        s.push(Event::PremiumDue {
            discounts: vec![],
            policy: PolicyId::from(name),
            receivable: 1,
            period_start: day(1),
            period_end: d(2028, 3, 31),
            total_minor: 60,
            members: None,
            by_cover_type: BTreeMap::new(),
        });
    }
    s.close(day(2));
    s.close(day(3));
    for (name, premium) in [("pol-refund", PremiumOnAvoidance::Refund), ("pol-retain", PremiumOnAvoidance::Retain)] {
        s.push(Event::PolicyStatusChanged {
            policy: PolicyId::from(name),
            from: PolicyStatus::InForce,
            to: PolicyStatus::Avoided,
        });
        s.push(Event::PolicyAvoided {
            policy: PolicyId::from(name),
            case: "nd-1".to_string(),
            premium,
            refunded_minor: 0,
        });
    }
    s.close(day(4));
    let mart = mart_of(1, &s.events);
    assert_eq!(total(&mart.in_force(day(2)).unwrap(), "policies"), 2);
    let after = mart.in_force(day(3)).unwrap();
    assert_eq!((total(&after, "policies"), total(&after, "sum_insured_minor")), (0, 0));
    assert!(after.reconciled);

    let moved = mart.movement(day(3), day(3)).unwrap();
    assert!(moved.reconciled);
    let all = rows(&moved);
    let kinds = |policy: &str| -> Vec<(String, String)> {
        all.iter()
            .filter(|r| r["policy"] == policy)
            .map(|r| (r["kind"].clone(), r["premium_minor"].clone()))
            .collect()
    };
    let refund = kinds("pol-refund");
    assert!(refund.iter().any(|(kind, _)| kind == "avoided"), "{refund:?}");
    assert_eq!(refund.last().unwrap(), &("premium_refunded_on_avoidance".to_string(), "-60".to_string()));
    let refund_row = all.iter().find(|r| r["kind"] == "premium_refunded_on_avoidance").unwrap();
    assert_eq!(refund_row["effective"], "2028-03-01");
    assert!(kinds("pol-retain").iter().all(|(kind, premium)| kind == "avoided" && premium == "0"));
    // Over the whole life of the two policies: 120 billed, 60 given back.
    assert_eq!(total(&mart.movement(day(1), day(3)).unwrap(), "premium_minor"), 60);
}

/// Purpose: a policy issued in November 2027 is run for fourteen months with every month
/// closed in turn. Until the thirteenth close nothing is trimmed. Closing December 2028
/// lets go of everything before December 2027: November 2027's extracts can no longer be
/// cut and the refusal says the stored files are the record, while December 2027's
/// in-force extract is still the file it was when first cut, and the current month's
/// extracts reconcile.
/// Why it matters: the mart is part of the ledger's replicated state. Without a rule that
/// runs by itself it grows with every movement for the life of the insurer, and every
/// snapshot and every node restart grows with it.
#[test]
fn closing_a_period_lets_go_of_history_more_than_a_year_behind_it() {
    let start = d(2027, 11, 15);
    let mut w = World::new(1, start);
    let (p, id) = w.issue(1, start);
    w.pay(p, &id, "r1", 20 * MONTHLY);
    let ask = |w: &World, kind: &str, period: ic_finance::PeriodId| {
        ic_finance::host::answer(&w.books.state, serde_json::json!({ "q": "extract", "kind": kind, "period": period }))
    };
    let mut december = None;
    let mut month = period(2027, 11);
    while month <= period(2028, 12) {
        let next = ic_finance::PeriodId::of(month.end().succ_opt().unwrap());
        w.until(next.start() + chrono::Days::new(2));
        if month == period(2027, 12) {
            december = Some(ask(&w, IN_FORCE, month).unwrap()["content_hash"].clone());
        }
        assert_eq!(w.books.state.mart.kept_from().filter(|k| *k > start), None, "trimmed before {month} closed");
        w.close(month);
        month = next;
    }
    assert_eq!(w.books.state.mart.kept_from(), Some(d(2027, 12, 1)));
    let gone = ask(&w, IN_FORCE, period(2027, 11)).unwrap_err();
    assert!(gone.contains("keeps history from 2027-12-01") && gone.contains("already stored"), "{gone}");
    assert!(ask(&w, MOVEMENT, period(2027, 11)).is_err());
    assert_eq!(ask(&w, IN_FORCE, period(2027, 12)).unwrap()["content_hash"], december.unwrap());
    for kind in [IN_FORCE, MOVEMENT, EXPERIENCE] {
        assert_eq!(ask(&w, kind, period(2028, 12)).unwrap()["reconciled"], serde_json::json!(true), "{kind}");
    }
}

/// Purpose: the claims module's source still publishes every fact and key the mart reads:
/// `claim_reserved` with `policy`, `cover`, `life` and `event_date`; `claim_admitted` with
/// `admitted_minor` and, for a benefit in payment, `monthly_minor`; and
/// `admission_reversed`.
/// Why it matters: only a straight-through admission is run end to end against the real
/// claims module. A reversal or a benefit in payment whose fact was renamed would not
/// fail anything: the mart would count nothing, and the gap would first be seen by an
/// actuary. Reading the source is crude, and it fails the build on the day of the rename.
#[test]
fn the_claims_module_still_publishes_the_facts_the_mart_reads() {
    let decide = include_str!("../../claims/src/decide.rs");
    let intake = include_str!("../../claims/src/intake.rs");
    for needed in ["\"claim_admitted\"", "(\"admitted_minor\", ", "(\"monthly_minor\", ", "\"admission_reversed\""] {
        assert!(decide.contains(needed), "crates/claims/src/decide.rs no longer has {needed}");
    }
    for needed in ["\"claim_reserved\"", "(\"policy\", ", "(\"cover\", ", "(\"event_date\", ", "\"life\""] {
        assert!(intake.contains(needed), "crates/claims/src/intake.rs no longer has {needed}");
    }
}

/// Purpose: a policy is issued to a named policyholder on a life with a party record, a
/// date of birth and a policy number, and a claim is registered and admitted on it. The
/// three extracts have exactly the columns the layouts list; no line of any of them
/// carries the date of birth, the policyholder, the life's party, the policy number or
/// the claim's reference; and the mart as stored holds no party at all.
/// Why it matters: the mart is what goes to actuaries, the reinsurer and the regulator.
/// It is allowed opaque IDs and rating facts. A column added for convenience that carries
/// a person's identity or anything medical would leave the building in every file cut
/// from then on, and this is the test that would have to be changed to let it.
#[test]
fn the_mart_and_its_files_hold_ids_and_rating_facts_only() {
    let day = |n: u32| d(2028, 3, n);
    let policy = PolicyId::from("pol-1");
    let mut s = Stream::new(day(1));
    s.push(Event::PolicyIssued {
        policy: policy.clone(),
        number: "NUMBER-778899".to_string(),
        group: "grp".into(),
        policyholder: "party-holder-555".into(),
        product_code: "term-life".to_string(),
        product_hash: "h".to_string(),
        inception: day(1),
        lives: vec![LifeInfo {
            date_of_birth: Some(d(1980, 6, 17)),
            sex: Some(Sex::Female),
            smoker: Some(false),
            ..LifeInfo::new("life-1".into(), Some("party-life-444".into()))
        }],
        layers: vec![layer("life", 1, "life-1", 500_000_00, day(1))],
        members: vec![],
    });
    s.close(day(2));
    s.push(claim_fact(
        "claim_reserved",
        "CLM-REF-321",
        &[("policy", "pol-1"), ("cover", "life"), ("life", "life-1"), ("event_date", "2028-03-01")],
    ));
    s.push(claim_fact("claim_admitted", "CLM-REF-321", &[("admitted_minor", "50000")]));
    s.close(day(3));
    let mart = mart_of(1, &s.events);

    let files = [
        mart.in_force(day(2)).unwrap(),
        mart.movement(day(1), day(2)).unwrap(),
        mart.experience(day(1), day(2)).unwrap(),
    ];
    let headers: Vec<&str> = files.iter().map(|x| x.lines[0].as_str()).collect();
    assert_eq!(
        headers,
        [
            "partition,policy,product,inception,status,cover,cover_type,layer,life,age,sex,smoker,occupation_class,category,start,sum_insured_minor",
            "business_date,partition,policy,product,kind,effective,cover,cover_type,layer,life,age,sex,smoker,occupation_class,category,policies,covers,sum_insured_minor,premium_minor",
            "product,cover_type,age_band,sex,smoker,occupation_class,category,cover_days,sum_insured_days_minor,ending_claims,ending_claim_sum_insured_minor,admitted_claims,admitted_minor,admitted_monthly_minor",
        ]
    );
    let private = ["1980-06-17", "1980", "party-holder-555", "party-life-444", "NUMBER-778899", "CLM-REF-321"];
    for file in &files {
        assert!(file.lines.len() > 1, "{} is empty", file.kind);
        for line in &file.lines {
            assert!(private.iter().all(|p| !line.contains(p)), "{}: {line}", file.kind);
        }
    }
    assert_eq!(total(&files[2], "admitted_claims"), 1);
    let stored = serde_json::to_string(&mart).unwrap();
    assert!(["party-holder-555", "party-life-444", "NUMBER-778899"].iter().all(|p| !stored.contains(p)));
}

/// Purpose: the conduct measures are read from the feed itself. On a term life policy,
/// claim c1 is registered on 1 March and admitted on 11 March, then a further amount is
/// admitted; claim c2 is registered on 1 March and declined on 5 March; c3 is declined on
/// 9 March and reopened on new evidence; the policy lapses on 20 March and is reinstated
/// on 25 March. In April c2's decline is overturned on internal review and c1's admission
/// is reversed. March counts one acceptance taking 10 days, two declines taking 4 and 8,
/// a lapse and a reinstatement; April counts the overturn and the reversal, each against
/// March; March's report says one of its declines has since been overturned. Nothing is
/// counted twice, and the counts are the same for the product and for the whole book.
/// Why it matters: conduct reporting is a read model of what the systems did, not a
/// figure someone sends in. If a decision reaches the books it is counted, with the time
/// it took, and a reopening on new evidence is not dressed up as an overturn.
#[test]
fn claims_decisions_lapses_and_their_turnaround_are_counted_from_the_feed() {
    let day = |m: u32, n: u32| d(2028, m, n);
    let mut s = Stream::new(day(3, 1));
    s.push(Event::PolicyIssued {
        policy: PolicyId::from("pol-1"),
        number: "N".to_string(),
        group: "grp".into(),
        policyholder: "party-x".into(),
        product_code: "term-life".to_string(),
        product_hash: "h".to_string(),
        inception: day(3, 1),
        lives: vec![LifeInfo::new("life-1".into(), None)],
        layers: vec![layer("life", 1, "life-1", 500_000_00, day(3, 1))],
        members: vec![],
    });
    let registered = [("policy", "pol-1"), ("cover", "life"), ("life", "life-1"), ("event_date", "2028-02-20")];
    for claim in ["c1", "c2", "c3"] {
        s.push(claim_fact("claim_reserved", claim, &registered));
    }
    s.close(day(3, 5));
    s.push(claim_fact("claim_closed", "c2", &[("outcome", "declined")]));
    s.close(day(3, 9));
    s.push(claim_fact("claim_closed", "c3", &[("outcome", "declined")]));
    s.push(claim_fact("claim_reopened", "c3", &[("basis", "NewEvidence")]));
    s.close(day(3, 11));
    s.push(claim_fact("claim_admitted", "c1", &[("admitted_minor", "100")]));
    s.push(claim_fact("claim_admitted", "c1", &[("admitted_minor", "20")]));
    s.push(claim_fact("claim_closed", "c1", &[("outcome", "paid")]));
    s.close(day(3, 20));
    s.push(Event::PolicyStatusChanged {
        policy: PolicyId::from("pol-1"),
        from: PolicyStatus::InForce,
        to: PolicyStatus::Lapsed,
    });
    s.close(day(3, 25));
    s.push(Event::PolicyStatusChanged {
        policy: PolicyId::from("pol-1"),
        from: PolicyStatus::Lapsed,
        to: PolicyStatus::InForce,
    });
    s.close(day(4, 3));
    s.push(claim_fact("claim_reopened", "c2", &[("basis", "InternalReview")]));
    s.push(claim_fact("claim_reopened", "c2", &[("basis", "DisputeScheme")]));
    s.push(claim_fact("admission_reversed", "c1", &[]));
    s.close(day(4, 4));

    let mut state = ic_finance::FinanceState::default();
    state.mart = mart_of(1, &s.events);
    let (mar, apr) = (period(2028, 3), period(2028, 4));
    for product in [None, Some("term-life")] {
        let march = state.conduct_report(mar, product);
        assert_eq!((march.accepted, march.declined, march.lapsed, march.reinstated), (1, 2, 1, 1), "{product:?}");
        assert_eq!(
            (
                march.accepted_turnaround.decisions,
                march.accepted_turnaround.total_days,
                march.accepted_turnaround.max_days
            ),
            (1, 10, 10)
        );
        assert_eq!(
            (
                march.declined_turnaround.decisions,
                march.declined_turnaround.total_days,
                march.declined_turnaround.max_days
            ),
            (2, 12, 8)
        );
        assert_eq!(march.declines_later_overturned, 1);
        assert!(march.overturned.is_empty() && march.admissions_reversed.is_empty());
        let april = state.conduct_report(apr, product);
        assert_eq!((april.accepted, april.declined, april.lapsed), (0, 0, 0));
        assert_eq!(april.overturned.into_iter().collect::<Vec<_>>(), [(mar, 1)]);
        assert_eq!(april.admissions_reversed.into_iter().collect::<Vec<_>>(), [(mar, 1)]);
    }
    assert_eq!(state.conduct_report(mar, Some("health")), ic_finance::returns::ConductReport::default());
    // The feed's counts cannot be sent in by any caller.
    let lapse = ic_finance::returns::ConductFact {
        id: "x".to_string(),
        measure: ic_finance::returns::ConductMeasure::PolicyLapsed,
        date: day(3, 20),
        product: None,
        relates_to: None,
    };
    let mut books = Books::ready(1);
    for caller in ["claims", "complaints"] {
        let refused = books.code(caller, None, FinCommand::RecordConductFacts { facts: vec![lapse.clone()] });
        assert_eq!(refused, "caller");
    }
}

/// Purpose: the claims by cause extract counts each claim registered, admitted or
/// declined in a period under the top of its cause tree, taken from whichever fact last
/// carried it; folds rows under five claims into "(other)", then drops their age band
/// and sex, and withholds what is still small; and keeps a cause typed as words out of
/// the file.
/// Why it matters: the appointed actuary studies experience by cause of claim, but a
/// cause beside a product, age and sex can name a person. Only categories leave the
/// claims module, and no row a reader could trace to fewer than five claims is shown,
/// nor any total from which a withheld amount could be worked out.
#[test]
fn claims_by_cause_are_categories_with_small_rows_folded_or_withheld() {
    let day = |m: u32, n: u32| d(2028, m, n);
    let mut s = Stream::new(day(3, 1));
    for (policy, product) in [("pol-1", "term-life"), ("pol-2", "trauma")] {
        s.push(Event::PolicyIssued {
            policy: PolicyId::from(policy),
            number: policy.to_string(),
            group: "grp".into(),
            policyholder: "party-x".into(),
            product_code: product.to_string(),
            product_hash: "h".to_string(),
            inception: day(3, 1),
            lives: vec![LifeInfo::new("life-1".into(), None)],
            layers: vec![layer("life", 1, "life-1", 500_000_00, day(3, 1))],
            members: vec![],
        });
    }
    let on = |policy: &'static str, cause: &'static str| {
        let mut detail = vec![("policy", policy), ("cover", "life"), ("life", "life-1"), ("event_date", "2028-02-20")];
        if !cause.is_empty() {
            detail.push(("cause_category", cause));
        }
        detail
    };
    // Six cancer claims, two of them given their cause only when decided.
    for n in 0..6 {
        let cause = if n < 4 { "cancer.breast" } else { "" };
        s.push(claim_fact("claim_reserved", &format!("ca{n}"), &on("pol-1", cause)));
    }
    // Three mental, two injury and one typed as words: each too small, six together.
    for (n, cause) in ["mental", "mental", "mental", "injury.fall", "injury", "Fell off J Smith's roof"]
        .iter()
        .enumerate()
    {
        s.push(claim_fact("claim_reserved", &format!("o{n}"), &on("pol-1", cause)));
    }
    // Two trauma claims: small however they are folded.
    for n in 0..2 {
        s.push(claim_fact("claim_reserved", &format!("t{n}"), &on("pol-2", "cardiovascular")));
    }
    s.close(day(3, 10));
    s.push(claim_fact("claim_admitted", "ca4", &[("admitted_minor", "100"), ("cause_category", "cancer")]));
    s.push(claim_fact("claim_admitted", "ca0", &[("admitted_minor", "50")]));
    s.push(claim_fact(
        "claim_closed",
        "ca5",
        &[("outcome", "declined"), ("cause_category", "cancer.bowel")],
    ));
    s.push(claim_fact("claim_admitted", "t0", &[("admitted_minor", "999")]));
    s.close(day(3, 20));
    // A claim decided in April counts in April only.
    s.close(day(4, 2));
    s.push(claim_fact("claim_admitted", "ca1", &[("admitted_minor", "7")]));
    s.close(d(2028, 5, 1));
    s.close(d(2028, 5, 2));

    let mut state = ic_finance::FinanceState::default();
    state.mart = mart_of(1, &s.events);
    let march = state.mart.claims_by_cause(day(3, 1), day(3, 31)).unwrap();
    assert_eq!(
        march.lines,
        [
            "product,cover_type,age_band,sex,cause,claims,registered,admitted_claims,admitted_minor,declined",
            "term-life,life-type,,,(other),6,6,0,0,0",
            "term-life,life-type,,,cancer,6,6,2,150,1",
        ]
    );
    assert!(!march.lines.concat().contains("Smith"));
    let control: Vec<(&str, i128)> = march.control.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        control,
        [
            ("admitted_claims", 2),
            ("admitted_minor", 150),
            ("claims", 12),
            ("declined", 1),
            ("min_cell", 5),
            ("registered", 12),
            ("withheld_claims", 2)
        ]
    );
    // April has one claim only, which is withheld; asked through the host as any extract is.
    let april = ic_finance::host::answer(
        &state,
        serde_json::json!({ "q": "extract", "kind": "claims_by_cause", "period": period(2028, 4) }),
    )
    .unwrap();
    assert_eq!(april["total_lines"], 1);
    assert_eq!(april["control"]["withheld_claims"], 1);
    assert_eq!(april["control"]["claims"], 0);
    assert!(state.mart.claims_by_cause(day(3, 31), day(3, 1)).is_err());
}

/// Purpose: one policy is issued as new business and another is issued and then
/// announced as migrated from the old system on the same day. The movement extract shows
/// the first as `new_business` and the second as `migrated`, and both are in force.
/// Why it matters: new business figures drive commission reviews, sales reporting and the
/// actuary's new business strain. A book loaded from the old system on cutover day would
/// otherwise look like a month of record sales.
#[test]
fn a_migrated_policy_is_not_new_business_in_the_movements() {
    let day = |n: u32| d(2028, 3, n);
    let mut s = Stream::new(day(1));
    for policy in ["pol-new", "pol-old"] {
        s.push(Event::PolicyIssued {
            policy: PolicyId::from(policy),
            number: policy.to_string(),
            group: "grp".into(),
            policyholder: "party-x".into(),
            product_code: "term-life".to_string(),
            product_hash: "h".to_string(),
            inception: d(2020, 6, 1),
            lives: vec![LifeInfo::new("life-1".into(), None)],
            layers: vec![layer("life", 1, "life-1", 100_000_00, d(2020, 6, 1))],
            members: vec![],
        });
    }
    s.push(Event::PolicyMigrated {
        policy: PolicyId::from("pol-old"),
        cohort: "c1".to_string(),
        cutover: day(1),
        periods_billed: 93,
        standing: None,
        outstanding_minor: 0,
        oldest_unpaid_due: None,
        assumptions: vec![],
    });
    s.close(day(2));
    s.close(day(3));
    let mart = mart_of(1, &s.events);
    let movement = mart.movement(day(1), day(1)).unwrap();
    let kinds: BTreeSet<(String, String)> = movement.lines[1..]
        .iter()
        .map(|l| {
            let cols: Vec<&str> = l.split(',').collect();
            (cols[2].to_string(), cols[4].to_string())
        })
        .collect();
    assert!(kinds.contains(&("pol-new".to_string(), "new_business".to_string())), "{kinds:?}");
    assert!(kinds.contains(&("pol-old".to_string(), "migrated".to_string())), "{kinds:?}");
    assert!(!kinds.contains(&("pol-old".to_string(), "new_business".to_string())), "{kinds:?}");
    assert!(movement.reconciled);
    assert_eq!(mart.in_force(day(1)).unwrap().control["policies"], 2);
}
