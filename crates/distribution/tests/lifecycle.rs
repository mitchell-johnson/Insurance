//! Commission through the rest of a policy's life: indexation, merged and
//! unwound layers, a move between partitions, and replacement business.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::event::{Event as CoreEvent, LayerInfo};
use ic_core::{Command as CoreCommand, EventRecord, LayerOrigin, PolicyId, PolicyStatus, UnwindCause};
use ic_distribution::config::Change;
use ic_distribution::model::Schedule;
use ic_distribution::rules::{Share, WHOLE};
use ic_distribution::state::Settings;
use ic_distribution::structure::PartyKind;
use ic_distribution::{BookId, Command, EngagementId, Event};
use ic_sim::Delivery;

const ANNUAL: i64 = 1_200_00;

fn layer_of(policy: &PolicyId, n: u32) -> String {
    format!("{policy}/life/{n}")
}

/// Deliver an event the core in a box has no command to produce, as the next
/// in the partition's sequence. Only at the end of a test.
fn inject(w: &mut World, partition: u16, event: CoreEvent) -> Vec<Event> {
    let seq = w.dist.checkpoints[&partition] + 1;
    let record = EventRecord {
        id: format!("{partition}-{seq}"),
        partition,
        seq,
        business_date: w.today(),
        log_index: 0,
        traceparent: None,
        event,
    };
    w.send(module("event-relay"), Command::Ingest(Box::new(record))).unwrap()
}

/// Purpose: an indexation step inherits the cover's book, writing engagement and terms scaled
/// to its size; it earns initial commission only where the schedule says indexation does; a
/// merge of old layers ends the merged layer; and unwinding a layer reverses the initial
/// commission paid on that layer alone.
/// Why it matters: every policy with indexation gains a layer a year for decades. If a step
/// earned nothing, or earned initial commission it should not, the error repeats annually
/// across the whole book.
#[test]
fn indexation_steps_inherit_their_terms_and_an_unwound_step_gives_its_initial_back() {
    let mut w = worked_example();
    w.activate(
        "indexed",
        vec![Change::ActivateSchedule(Schedule {
            id: "indexed".into(),
            initial_on_indexation: true,
            ..schedule()
        })],
    )
    .unwrap();
    let (plain, _) = w.issue(1, 500_000_00, None);
    let (indexed, _) = w.issue(2, 500_000_00, None);
    w.write(&plain, "life", 1, "eng-a-one", "a-at-fap-one", ANNUAL).unwrap();
    let mut written = ic_distribution::state::WriteLayer {
        policy: indexed.clone(),
        cover: "life".into(),
        layer: 1,
        writing: vec![("eng-a-one".into(), WHOLE)],
        books: vec![("a-at-fap-one".into(), WHOLE)],
        schedule: "indexed".into(),
        option: "upfront".into(),
        annual_premium_minor: ANNUAL,
        replacement: false,
    };
    w.send(module("new-business"), Command::WriteLayer(Box::new(written.clone()))).unwrap();
    for policy in [&plain, &indexed] {
        let ahead = w.owing(policy) * 60;
        w.pay_amount(policy, "ahead", ahead);
    }
    w.days(800);

    let step = &w.dist.layers[&layer_of(&plain, 2)];
    assert!(step.indexation);
    assert_eq!(step.sum_insured_minor, 12_500_00);
    let terms = step.terms.as_ref().unwrap();
    assert_eq!(
        (terms.annual_premium_minor, terms.rates.initial_ppm),
        (30_00, 0),
        "2.5% of the cover, 2.5% of the premium, no initial"
    );
    assert_eq!(step.writing, w.dist.layers[&layer_of(&plain, 1)].writing);
    assert_eq!(step.books_on(w.today()), w.dist.layers[&layer_of(&plain, 1)].books_on(w.today()));
    let exposures = |w: &World, policy: &PolicyId| w.dist.exposures.values().filter(|e| &e.policy == policy && e.open).count();
    assert_eq!(exposures(&w, &plain), 2, "only the original layer paid initial commission");
    assert_eq!(exposures(&w, &indexed), 6, "each step paid initial commission under this schedule");
    assert!(w.dist.layers[&layer_of(&indexed, 3)].initial_paid.is_some());
    w.assert_reconciled();

    // Merge the old layers of the plain policy: the merged layer ends, the first carries its cover.
    w.core(
        &plain,
        "policy-servicing",
        CoreCommand::MergeLayers {
            policy: plain.clone(),
            cover: "life".into(),
        },
    )
    .unwrap();
    assert!(w.dist.layers[&layer_of(&plain, 2)].ended.is_some());
    assert_eq!(w.dist.layers[&layer_of(&plain, 1)].sum_insured_minor, 512_500_00);

    // Unwind the indexed policy to before its latest step.
    let third = w.dist.layers[&layer_of(&indexed, 3)].clone();
    let before = w.payable("a-co") + w.payable("fap-one");
    let initial_on_third = ic_distribution::rules::portion(third.terms.as_ref().unwrap().annual_premium_minor, WHOLE);
    w.core(
        &indexed,
        "remediation",
        CoreCommand::UnwindAfter {
            policy: indexed.clone(),
            after: third.start.pred_opt().unwrap(),
            cause: UnwindCause::Remediation,
            reference: "rem-1".into(),
        },
    )
    .unwrap();
    let layer = &w.dist.layers[&layer_of(&indexed, 3)];
    assert_eq!((layer.ended, layer.initial_paid.clone()), (Some(third.start), None));
    assert_eq!(exposures(&w, &indexed), 4);
    let taken_back = before - (w.payable("a-co") + w.payable("fap-one"));
    assert!(
        taken_back >= initial_on_third,
        "the step's initial commission, and what the step earned beside it: {taken_back}"
    );
    assert!(w.events.iter().any(|e| matches!(e, Event::CommissionReversed { .. })));
    w.assert_reconciled();

    // An increase arrives with no terms: it earns nothing until someone writes it, and says so.
    written.layer = 9;
    let today = w.today();
    let events = inject(
        &mut w,
        0,
        CoreEvent::LayerAdded {
            policy: indexed.clone(),
            layer: LayerInfo {
                cover: "life".into(),
                cover_type: "life".into(),
                layer: 9,
                start: today,
                sum_insured_minor: 50_000_00,
                life: ic_core::LifeId("life-1".into()),
            },
            origin: LayerOrigin::Increase,
            continues: None,
        },
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::TaskCreated { kind, .. } if kind == "layer_needs_terms")));
    assert!(w.dist.layers[&layer_of(&indexed, 9)].terms.is_none());
    w.send(module("policy-servicing"), Command::WriteLayer(Box::new(written))).unwrap();
    assert!(w.dist.layers[&layer_of(&indexed, 9)].terms.is_some());
}

/// Purpose: when a policy moves to another partition its commission follows: later premiums
/// post on the new partition, and reversing what was earned before the move posts there too,
/// without a reference the new partition has never seen.
/// Why it matters: transfers happen when a member leaves a scheme. A reversal that still
/// pointed at the old partition would be refused by the core and the commission never taken back.
#[test]
fn commission_follows_a_policy_to_another_partition() {
    let mut w = worked_example_in(World::with(2, Settings::default()));
    let (policy, from) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", ANNUAL).unwrap();
    w.pay(&policy, "r1");
    let to = 1 - from;
    let target = (0..)
        .map(|i| ic_core::GroupId(format!("grp-move-{i}")))
        .find(|g| w.cluster.partition_of(g) == to)
        .unwrap();
    w.cluster
        .submit(
            from,
            "transfer",
            CoreCommand::BeginTransfer {
                policy: policy.clone(),
                transfer_id: "t1".into(),
                target_group: target,
                target_partition: to,
            },
        )
        .unwrap();
    while w.cluster.messages_in_flight() > 0 {
        w.cluster.relay_messages(|_| Delivery::Deliver);
    }
    w.sync();
    assert_eq!(w.cluster.locate(&policy), Some(to));
    assert_eq!(w.dist.policies[&policy].partition, to);
    let earned = w.payable("a-co") + w.payable("fap-one");
    assert!(earned > ANNUAL);

    w.days(2);
    w.core(&policy, "policy-servicing", CoreCommand::CancelPolicy { policy: policy.clone() })
        .unwrap();
    assert_eq!(
        w.payable("a-co") + w.payable("fap-one"),
        0,
        "cooling-off reverses everything, across the two partitions"
    );
    let reversal = w.dist.allocations.values().find(|a| a.kind == "commission_reversed").unwrap();
    assert_eq!((reversal.partition, reversal.reverses.clone()), (to, None));
    assert_eq!(
        w.dist.by_partition(&party("a-co"), ic_core::counterparty::PartyLedger::CommissionPayable).len(),
        2
    );
    assert!(w.refused.is_empty(), "{:?}", w.refused);
    w.assert_reconciled();
}

/// Purpose: where the schedule says a replaced policy's unexpired clawback carries over, ending
/// the old policy claws nothing back and the exposure moves to the replacement, where a later
/// lapse does claw it back.
/// Why it matters: replacing a policy with the same insurer is not a lapse, and clawing back on
/// it punishes good advice. But the exposure must not vanish, or replacing a policy becomes the
/// way to escape clawback.
#[test]
fn a_replaced_policys_clawback_carries_to_the_replacement() {
    let mut w = worked_example();
    w.activate(
        "carry",
        vec![Change::ActivateSchedule(Schedule {
            id: "carry".into(),
            replacement_carries_clawback: true,
            ..schedule()
        })],
    )
    .unwrap();
    let (old, _) = w.issue(1, 500_000_00, None);
    let write = |w: &mut World, policy: &PolicyId, replacement| {
        w.send(
            module("new-business"),
            Command::WriteLayer(Box::new(ic_distribution::state::WriteLayer {
                policy: policy.clone(),
                cover: "life".into(),
                layer: 1,
                writing: vec![("eng-a-one".into(), WHOLE)],
                books: vec![("a-at-fap-one".into(), WHOLE)],
                schedule: "carry".into(),
                option: "upfront".into(),
                annual_premium_minor: ANNUAL,
                replacement,
            })),
        )
        .unwrap();
    };
    write(&mut w, &old, false);
    w.pay(&old, "r1");
    w.days(20);
    let (new, _) = w.issue(2, 600_000_00, None);
    write(&mut w, &new, true);
    w.send(
        module("new-business"),
        Command::LinkReplacement {
            policy: new.clone(),
            replaces: old.clone(),
        },
    )
    .unwrap();
    w.pay(&new, "r1");
    let fap = w.payable("fap-one");
    w.core(&old, "policy-servicing", CoreCommand::CancelPolicy { policy: old.clone() }).unwrap();
    assert_eq!(w.payable("fap-one"), fap, "nothing is clawed back on the replaced policy");
    assert!(w.dist.exposures.values().all(|e| e.policy == new && e.open));
    assert_eq!(w.dist.clawback_exposure(w.today())[&party("fap-one")], ANNUAL);
    w.days(20);
    w.core(&new, "policy-servicing", CoreCommand::CancelPolicy { policy: new.clone() }).unwrap();
    assert_eq!(
        w.payable("fap-one"),
        fap - ANNUAL,
        "the carried exposure is clawed back when the replacement ends"
    );
    w.assert_reconciled();
}

/// Purpose: when an unwind reverses the billing of periods already paid, the renewal commission
/// those premiums earned is taken back from the payees who received it, to the cent; the
/// initial commission and the renewal on periods that stand are left alone; premium handed
/// back never takes back more commission than it earned; and once the policy is resumed and
/// billed again, the premium earns again.
/// Why it matters: a late-notified death or a remediation hands months of premium back to the
/// customer. Commission left with the adviser on premium the insurer no longer has is an
/// overpayment nobody would find until the adviser's account was audited, and taking back more
/// than was earned would be a debt the adviser never owed.
#[test]
fn premium_handed_back_by_an_unwind_takes_its_renewal_commission_with_it() {
    let (mut w, policy, first_renewal) = advised_in(worked_example());
    let total = |w: &World| w.payable("a-co") + w.payable("fap-one") + w.held("fap-one");
    let unwind_after = w.today() + chrono::Duration::days(20);
    // Two more months, each paid when billed.
    let mut earned = vec![];
    for n in 2..=3 {
        w.days(31);
        let before = total(&w);
        assert!(w.pay(&policy, &format!("r{n}")) > 0, "month {n} was not billed");
        earned.push(total(&w) - before);
    }
    assert!(earned.iter().all(|e| *e > 0), "{earned:?}");
    let (a_before, fap_before, all_before) = (w.payable("a-co"), w.payable("fap-one"), total(&w));

    w.core(
        &policy,
        "remediation",
        CoreCommand::UnwindAfter {
            policy: policy.clone(),
            after: unwind_after,
            cause: UnwindCause::Remediation,
            reference: "rem-2".into(),
        },
    )
    .unwrap();
    let taken_back = all_before - total(&w);
    assert_eq!(taken_back, earned.iter().sum::<i64>(), "exactly the renewal on the two months handed back");
    assert!(
        w.payable("a-co") < a_before && w.payable("fap-one") < fap_before,
        "from each payee who received it"
    );
    assert_eq!(
        total(&w),
        1_200_00 + first_renewal,
        "the initial commission and the first month's renewal stand"
    );
    assert!(w
        .events
        .iter()
        .any(|e| matches!(e, Event::CommissionReversed { reference, .. } if reference.starts_with("cu:"))));
    w.assert_reconciled();

    // More premium said to be handed back than is left takes back only what is left, once.
    let partition = w.cluster.locate(&policy).unwrap();
    let all = std::collections::BTreeMap::from([("life".to_string(), i64::MAX / 4)]);
    let rest = inject(
        &mut w,
        partition,
        CoreEvent::PremiumUnapplied {
            policy: policy.clone(),
            reference: "everything".into(),
            net_by_cover_type: all.clone(),
        },
    );
    let reference = rest
        .iter()
        .find_map(|e| match e {
            Event::CommissionReversed { reference, .. } => Some(reference.clone()),
            _ => None,
        })
        .expect("the first month's renewal is taken back");
    let lines = &w.dist.allocations[&reference].lines;
    assert_eq!(
        lines.iter().map(|l| l.amount_minor).sum::<i64>(),
        -first_renewal,
        "only the first month's renewal was left to take"
    );
    assert_eq!(lines.len(), 2, "one line for each payee of the rule: {lines:?}");
    let again = inject(
        &mut w,
        partition,
        CoreEvent::PremiumUnapplied {
            policy: policy.clone(),
            reference: "everything-again".into(),
            net_by_cover_type: all,
        },
    );
    assert!(again.is_empty(), "{again:?}");
    // Premium on a policy nobody advised on takes nothing from anyone.
    let unknown = inject(
        &mut w,
        partition,
        CoreEvent::PremiumUnapplied {
            policy: PolicyId("POL-NOBODY".into()),
            reference: "x".into(),
            net_by_cover_type: std::collections::BTreeMap::from([("life".to_string(), 100)]),
        },
    );
    assert!(unknown.is_empty());
}

/// Purpose: events that arrive for things the module does not know, or a second time, change
/// nothing and post nothing: a status change or a reinstatement for a policy nobody advised on,
/// a reinstatement with no clawback behind it, a layer announced twice, a receipt reversed
/// twice, and premium handed back on a policy that earned no commission. A cancelled case marks
/// everyone it reaches as changed, and a command that only a person may send names the module
/// that tried.
/// Why it matters: the event feed is at least once and carries every policy, advised or not. A
/// module that posted on a repeat would pay or claw back twice, and one that failed on an
/// unknown policy would stop reading the feed for every adviser.
#[test]
fn stray_and_repeated_events_post_nothing() {
    let (mut w, policy, _) = advised_in(worked_example());
    let partition = w.cluster.locate(&policy).unwrap();
    // A policy nobody wrote: it earns nothing.
    let (plain, plain_partition) = w.issue(2, 300_000_00, None);
    w.pay(&plain, "p1");
    w.assert_reconciled();
    let nobody = PolicyId("POL-NOBODY".into());
    let requested = w.dist.requested.len();
    for event in [
        CoreEvent::PolicyStatusChanged {
            policy: nobody.clone(),
            from: ic_core::PolicyStatus::InForce,
            to: ic_core::PolicyStatus::Lapsed,
        },
        CoreEvent::PolicyReinstated { policy: nobody.clone() },
        CoreEvent::PolicyReinstated { policy: policy.clone() },
        CoreEvent::LayerAdded {
            policy: policy.clone(),
            layer: LayerInfo {
                cover: "life".into(),
                cover_type: "life".into(),
                layer: 1,
                start: w.today(),
                sum_insured_minor: 1,
                life: ic_core::LifeId("life-1".into()),
            },
            origin: LayerOrigin::Increase,
            continues: None,
        },
    ] {
        let events = inject(&mut w, partition, event.clone());
        assert!(events.is_empty(), "{event:?} produced {events:?}");
    }
    assert_eq!(
        w.dist.layers[&layer_of(&policy, 1)].sum_insured_minor,
        500_000_00,
        "the layer announced twice is unchanged"
    );
    assert_eq!(w.dist.requested.len(), requested);

    // A receipt reversed, and the same reversal offered again under a new event.
    let reversal = CoreEvent::ReceiptReversed {
        policy: policy.clone(),
        receipt: "r1".into(),
        amount_minor: 1,
    };
    let first = inject(&mut w, partition, reversal.clone());
    assert!(first.iter().any(|e| matches!(e, Event::CommissionReversed { .. })), "{first:?}");
    assert_eq!(w.dist.requested.len(), requested + 1, "one posting takes the receipt's commission back");
    assert!(inject(&mut w, partition, reversal).is_empty());
    assert_eq!(w.dist.requested.len(), requested + 1);

    // Premium handed back on the policy that earned nothing takes nothing.
    let back = inject(
        &mut w,
        plain_partition,
        CoreEvent::PremiumUnapplied {
            policy: plain.clone(),
            reference: "rem-9".into(),
            net_by_cover_type: std::collections::BTreeMap::from([("life".to_string(), 10_00)]),
        },
    );
    assert!(back.is_empty(), "{back:?}");
    assert_eq!(w.dist.requested.len(), requested + 1);

    // A command only a person decides, tried by a module, is refused by name.
    let refused = ic_distribution::Actor::Module("payments".into()).person().unwrap_err();
    assert_eq!(refused.code, "caller_not_allowed");
    assert!(refused.message.contains("payments"));
    assert!(admin().person().is_ok());
    // A unit command names itself as the others do.
    assert_eq!(Command::RecordDailyControl.name(), "record_daily_control");
}

/// Purpose: a claim overpayment that claims keeps in its own ledger shows on the party's
/// position as a memo line with the claim, the amount outstanding, the recovery status and
/// whether the insurer made the error; each later word from claims replaces the line; a line
/// with nothing outstanding drops off; the party is marked as changed each time; none of it
/// touches this module's balances or asks the core for a posting; and an event that names no
/// party, or is some other claims fact, is left alone.
/// Why it matters: the workbench is the one place a person sees everything a counterparty owes
/// before releasing money to them. An overpayment missing from it lets held commission be paid
/// to someone who owes the insurer; one counted in this module's balances would be counted
/// twice, because the claim ledger already holds it.
#[test]
fn a_claim_overpayment_shows_as_a_memo_line_and_moves_no_money() {
    let (mut w, policy, _) = advised_in(worked_example());
    let partition = w.cluster.locate(&policy).unwrap();
    let today = w.today();
    let (requested, payable) = (w.dist.requested.len(), w.payable("a-co"));
    let claims = |kind: &str, fields: &[(&str, &str)]| CoreEvent::Module {
        module: "claims".into(),
        kind: kind.into(),
        subject: "clm-1".into(),
        detail: fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    };
    let whole = |outstanding: &'static str, status: &'static str| {
        vec![
            ("claim", "clm-1"),
            ("policy", "POL-9"),
            ("overpayment", "clm-1/overpayment/1"),
            ("party", "a-co"),
            ("outstanding_minor", outstanding),
            ("status", status),
            ("cause", "duplicate_payment"),
            ("insurer_error", "true"),
            ("good_faith", "true"),
        ]
    };

    let cursor = w.dist.seq;
    inject(&mut w, partition, claims("overpayment_raised", &whole("250000", "held")));
    let memo = w.dist.position(&party("a-co"), today).memo;
    assert_eq!(memo.len(), 1);
    let line = &memo[0];
    assert_eq!(
        (line.owner.as_str(), line.reference.as_str(), line.case_ref.as_str()),
        ("claims", "clm-1/overpayment/1", "clm-1")
    );
    assert_eq!(
        (line.outstanding_minor, line.status.as_str(), line.cause.as_str()),
        (2_500_00, "held", "duplicate_payment")
    );
    assert!(line.insurer_error && line.good_faith);
    assert_eq!((line.policy.clone(), line.as_at), (Some(PolicyId("POL-9".into())), today));
    assert!(w.dist.changed[&party("a-co")] > cursor, "the publisher is told the party moved");
    assert!(
        w.dist.position(&party("fap-one"), today).memo.is_empty(),
        "it shows on the party that owes it and nobody else"
    );

    // Claims changes its mind about recovery, then part is repaid: the one line follows.
    inject(&mut w, partition, claims("recovery_status_changed", &whole("250000", "pursued")));
    inject(&mut w, partition, claims("overpayment_recovered", &whole("100000", "pursued")));
    let memo = w.dist.position(&party("a-co"), today).memo;
    assert_eq!((memo.len(), memo[0].outstanding_minor, memo[0].status.as_str()), (1, 1_000_00, "pursued"));

    // Events that cannot be shown, or are not about an overpayment, are left alone.
    let no_party: Vec<(&str, &str)> = whole("999", "held").into_iter().filter(|(k, _)| *k != "party").collect();
    inject(&mut w, partition, claims("overpayment_raised", &no_party));
    inject(&mut w, partition, claims("claim_paid", &whole("999", "held")));
    assert_eq!(w.dist.memo.len(), 1);
    assert_eq!(w.dist.position(&party("a-co"), today).memo[0].outstanding_minor, 1_000_00);

    // Written off: nothing is outstanding, so it leaves the position. The record of it stays.
    inject(&mut w, partition, claims("overpayment_written_off", &whole("0", "written_off")));
    assert!(w.dist.position(&party("a-co"), today).memo.is_empty());
    assert_eq!(w.dist.memo["claims:clm-1/overpayment/1"].status, "written_off");

    assert_eq!(
        (
            w.dist.requested.len(),
            w.dist.balance(&party("a-co"), ic_core::counterparty::PartyLedger::CommissionPayable)
        ),
        (requested, payable)
    );
}

/// Purpose: premium the insurer pays for the customer during a claim earns renewal commission
/// for the payees of the rule as premium received does, and no initial commission; a layer
/// written under a schedule that says waived premium earns nothing earns nothing, while a layer
/// written under the earlier version still earns; a waiver that names no policy this module
/// knows, or carries no premium for cover, posts nothing; and a schedule stored before the
/// setting existed reads as earning.
/// Why it matters: the design says a waived premium earns unless the schedule says otherwise.
/// An adviser whose client is on claim for years would otherwise lose renewal income the
/// agreement gives them, and a schedule that excludes it would be overpaid for as long as the
/// claim runs.
#[test]
fn waived_premium_earns_renewal_commission_unless_the_schedule_says_otherwise() {
    let (mut w, earning, _) = advised_in(worked_example());
    // The next version of the schedule stops paying on waived premium; a new policy is written under it.
    w.activate(
        "no-waiver",
        vec![Change::ActivateSchedule(Schedule {
            version: 2,
            effective_from: w.today(),
            earns_on_waived_premium: false,
            ..schedule()
        })],
    )
    .unwrap();
    let (excluded, _) = w.issue(2, 400_000_00, None);
    w.write(&excluded, "life", 1, "eng-a-one", "a-at-fap-one", 900_00).unwrap();
    w.pay(&excluded, "x1");
    w.assert_reconciled();
    let partition = w.cluster.locate(&earning).unwrap();
    let waived = |policy: &PolicyId, fields: &[(&str, &str)]| CoreEvent::Module {
        module: "claims".into(),
        kind: "premium_waived".into(),
        subject: "clm-7".into(),
        detail: std::iter::once(("policy", policy.0.as_str()))
            .chain(fields.iter().copied())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };

    let requested = w.dist.requested.len();
    let events = inject(&mut w, partition, waived(&earning, &[("amount_minor", "11500"), ("net:life", "10000")]));
    let reference = events
        .iter()
        .find_map(|e| match e {
            Event::CommissionEarned { reference, total_minor, .. } => Some((reference.clone(), *total_minor)),
            _ => None,
        })
        .expect("waived premium earns");
    assert_eq!(reference.1, 10_00, "the renewal rate on the premium for cover, not on the tax and fee");
    let lines = &w.dist.allocations[&reference.0].lines;
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().all(|l| l.commission_type == Some(ic_distribution::rules::CommissionType::Renewal)));
    assert_eq!(w.dist.requested.len(), requested + 1);

    for stray in [
        waived(&excluded, &[("amount_minor", "11500"), ("net:life", "10000")]),
        waived(&PolicyId("POL-NOBODY".into()), &[("net:life", "10000")]),
        waived(&earning, &[("amount_minor", "11500")]),
        waived(&earning, &[("net:life", "not a number")]),
    ] {
        let events = inject(&mut w, partition, stray.clone());
        assert!(events.is_empty(), "{stray:?} produced {events:?}");
    }
    assert_eq!(w.dist.requested.len(), requested + 1);

    let mut stored = serde_json::to_value(schedule()).unwrap();
    stored.as_object_mut().unwrap().remove("earns_on_waived_premium");
    assert!(serde_json::from_value::<Schedule>(stored).unwrap().earns_on_waived_premium);
}

/// Purpose: when one layer of a cover ends while the policy carries on, the initial commission
/// still at risk on that layer is clawed back as at the day the reduction took effect, from
/// the party the rule names as liable and not from whoever was paid; the layer earns nothing
/// afterwards; the same ending offered again, or the policy lapsing later, claws nothing more;
/// and a layer removed inside its cooling-off period has its initial commission reversed in
/// full from the payees who received it.
/// Why it matters: the design claws commission back on the layers reduced. Without it an
/// adviser keeps a full year's initial commission on cover the customer removed a month in,
/// and a later lapse that clawed the same layer again would take the money twice.
#[test]
fn a_layer_removed_while_the_policy_continues_is_clawed_back_once() {
    let ended = |policy: &PolicyId, effective| CoreEvent::LayerEnded {
        policy: policy.clone(),
        cover: "life".into(),
        layer: 1,
        life: ic_core::LifeId("life-1".into()),
        effective,
        sum_insured_minor: 500_000_00,
    };

    // Three weeks in, past cooling-off, all of the initial commission is still at risk.
    let (mut w, policy, _) = advised_in(worked_example());
    let start = w.today();
    let partition = w.cluster.locate(&policy).unwrap();
    let effective = start + chrono::Duration::days(20);
    w.run_until(effective + chrono::Duration::days(3));
    let requested = w.dist.requested.len();
    let events = inject(&mut w, partition, ended(&policy, effective));
    let (reference, total) = events
        .iter()
        .find_map(|e| match e {
            Event::ClawbackRaised { reference, total_minor, .. } => Some((reference.clone(), *total_minor)),
            _ => None,
        })
        .expect("the layer's commission is clawed back");
    assert_eq!(total, 1_200_00, "the whole initial commission is inside the first year of the scale");
    let lines = &w.dist.allocations[&reference].lines;
    assert!(
        lines.iter().all(|l| l.payee == party("fap-one") && l.amount_minor < 0),
        "from the liable party: {lines:?}"
    );
    let layer = &w.dist.layers[&layer_of(&policy, 1)];
    assert_eq!(layer.ended, Some(effective));
    assert!(w.dist.exposures.values().filter(|e| e.policy == policy).all(|e| !e.open));
    assert_eq!(w.dist.requested.len(), requested + 1);

    // Nothing more: not on a repeat, not on premium for the cover, not when the policy lapses.
    for later in [
        ended(&policy, effective),
        CoreEvent::ReceiptApplied {
            policy: policy.clone(),
            receipt: "late".into(),
            allocated_minor: 100_00,
            to_suspense_minor: 0,
            net_by_cover_type: std::collections::BTreeMap::from([("life".to_string(), 90_00)]),
            fund: None,
        },
        CoreEvent::PolicyStatusChanged {
            policy: policy.clone(),
            from: ic_core::PolicyStatus::InForce,
            to: ic_core::PolicyStatus::Lapsed,
        },
        ended(&PolicyId("POL-NOBODY".into()), effective),
    ] {
        let events = inject(&mut w, partition, later.clone());
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::ClawbackRaised { .. } | Event::CommissionEarned { .. })),
            "{later:?} produced {events:?}"
        );
    }
    assert_eq!(w.dist.requested.len(), requested + 1);

    // Removed inside cooling-off: the initial commission goes back from those who received it.
    let (mut w, policy, _) = advised_in(worked_example());
    let partition = w.cluster.locate(&policy).unwrap();
    let today = w.today();
    let events = inject(&mut w, partition, ended(&policy, today));
    let reference = events
        .iter()
        .find_map(|e| match e {
            Event::CommissionReversed { reference, .. } => Some(reference.clone()),
            _ => None,
        })
        .expect("the layer's initial commission is reversed");
    let lines = &w.dist.allocations[&reference].lines;
    assert_eq!(lines.iter().map(|l| l.amount_minor).sum::<i64>(), -1_200_00);
    let from: std::collections::BTreeSet<&str> = lines.iter().map(|l| l.payee.0.as_str()).collect();
    assert_eq!(from, ["a-co", "fap-one"].into());
    assert!(lines.iter().all(|l| l.commission_type == Some(ic_distribution::rules::CommissionType::Initial)));
}

/// Purpose: when the core reduces a cover and a layer is cut in part, the share of the initial
/// commission on the part given up is clawed back from the liable party (or, inside
/// cooling-off, reversed from the payees who received it); the part kept carries on with its
/// terms, its share of the premium and its share of the clawback exposure; it is not treated
/// as new business, so it needs no new terms and earns no second initial commission; the
/// renewal commission on premium the core hands back for the rest of the paid month goes back
/// too; later premium earns renewal on the kept layer; and the module's balances agree with
/// the core's ledgers.
/// Why it matters: the core records a partial cut as one layer ending and a smaller one
/// starting. Read naively that claws back the whole layer and then leaves the kept cover
/// with no commission terms at all, so the adviser loses commission on cover the customer
/// still holds, or is paid initial commission twice on the same cover.
#[test]
fn a_cover_reduced_in_part_is_clawed_back_in_proportion_and_carries_on() {
    let reduce = |w: &mut World, policy: &PolicyId, keep: i64| {
        let p = w.cluster.locate(policy).unwrap();
        let life = w.cluster.state(p).policies[policy].covers["life"].layers[0].life.clone();
        let command = CoreCommand::ReduceCover(Box::new(ic_core::command::ReduceCover {
            policy: policy.clone(),
            cover: "life".into(),
            life,
            sum_insured_minor: keep,
        }));
        w.core(policy, "policy-servicing", command).unwrap();
    };
    let initial = |w: &World, policy: &PolicyId| {
        w.dist
            .exposures
            .values()
            .filter(|e| &e.policy == policy && e.open)
            .map(|e| e.amount_minor)
            .sum::<i64>()
    };

    // Three weeks in, past cooling-off: 500,000 of cover reduced to 300,000.
    let (mut w, policy, first_renewal) = advised_in(worked_example());
    w.days(20);
    let (a_before, fap_before) = (w.payable("a-co"), w.payable("fap-one"));
    let before = w.events.len();
    reduce(&mut w, &policy, 300_000_00);
    let events = w.events[before..].to_vec();
    let clawed: i64 = events
        .iter()
        .filter_map(|e| match e {
            Event::ClawbackRaised { total_minor, .. } => Some(*total_minor),
            _ => None,
        })
        .sum();
    assert_eq!(clawed, 480_00, "two fifths of the 1,200.00 initial commission");
    // The core also hands back the premium for the cover given up over the rest of the paid
    // month, and the renewal commission it earned goes back from both payees.
    let renewal_back: i64 = events
        .iter()
        .filter_map(|e| match e {
            Event::CommissionReversed { reference, .. } if reference.starts_with("cu:") => {
                Some(-w.dist.allocations[reference].lines.iter().map(|l| l.amount_minor).sum::<i64>())
            }
            _ => None,
        })
        .sum();
    assert!(renewal_back > 0 && renewal_back < first_renewal * 2 / 5, "{renewal_back} of {first_renewal}");
    let (a_lost, fap_lost) = (a_before - w.payable("a-co"), fap_before - w.payable("fap-one"));
    assert_eq!(a_lost + fap_lost, 480_00 + renewal_back);
    assert!(a_lost > 0 && a_lost < renewal_back, "the clawback is from the liable party alone: {a_lost}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::TaskCreated { kind, .. } if kind == "layer_needs_terms")),
        "{events:?}"
    );
    let (old, new) = (&w.dist.layers[&layer_of(&policy, 1)], &w.dist.layers[&layer_of(&policy, 2)]);
    assert!(old.ended.is_some() && new.ended.is_none());
    assert_eq!((new.sum_insured_minor, new.start, new.initial_paid.is_some()), (300_000_00, old.start, true));
    assert_eq!(new.terms.as_ref().unwrap().annual_premium_minor, 720_00);
    assert_eq!(initial(&w, &policy), 720_00, "the kept share is still at risk, on the new layer");
    assert!(w
        .dist
        .exposures
        .values()
        .filter(|e| e.policy == policy && e.open)
        .all(|e| e.layer == layer_of(&policy, 2)));
    w.assert_reconciled();

    // The next premium earns renewal on the kept layer and no initial commission.
    w.days(15);
    let before = w.events.len();
    assert!(w.pay(&policy, "r2") > 0);
    let earned: Vec<String> = w.events[before..]
        .iter()
        .filter_map(|e| match e {
            Event::CommissionEarned { reference, .. } => Some(reference.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(earned.len(), 1);
    let lines = &w.dist.allocations[&earned[0]].lines;
    assert!(
        lines
            .iter()
            .all(|l| l.commission_type == Some(ic_distribution::rules::CommissionType::Renewal) && l.layer == Some(layer_of(&policy, 2))),
        "{lines:?}"
    );
    w.assert_reconciled();

    // The same reduction on the day the policy started is inside cooling-off.
    let (mut w, policy, _) = advised_in(worked_example());
    let (a_before, fap_before) = (w.payable("a-co"), w.payable("fap-one") + w.held("fap-one"));
    reduce(&mut w, &policy, 300_000_00);
    let back = (a_before - w.payable("a-co"), fap_before - w.payable("fap-one") - w.held("fap-one"));
    let both = back.0 + back.1;
    assert!(
        both > 480_00 && both < 480_00 + 558,
        "two fifths of the initial commission, and the renewal on the premium handed back: {back:?}"
    );
    assert!(back.0 > 0 && back.1 > 0, "from each payee who received it: {back:?}");
    assert_eq!(initial(&w, &policy), 720_00);
    w.assert_reconciled();
}

/// Purpose: with clawback by cause switched on in the rule the commission was paid under, a
/// clawback set off by an alteration is borne by the payees of the engagement that submitted
/// it; an alteration by the engagement that wrote the layer, or one attributed on an earlier
/// day, or with the switch off, falls on the original liable party as before; and the
/// attribution is refused for a policy or an engagement nobody knows.
/// Why it matters: a new adviser who cuts cover another adviser wrote sets off a clawback on
/// that adviser's commission. Where an agency agreement says the one who caused it pays, a
/// debit on the wrong party is a dispute with a provider every time it happens.
#[test]
fn clawback_by_cause_falls_on_the_engagement_that_submitted_the_alteration() {
    let mut w = worked_example();
    w.add_party("adviser-b", PartyKind::Adviser);
    w.engage("eng-b-two", "adviser-b", "fap-two", d(2020, 1, 1));
    let start = d(START.0, START.1, START.2);
    let by_cause = |on: bool, version: u32| {
        let lines = vec![
            base("a-co", Share::OfRemainder(pct(75.0)), true),
            base("fap-one", Share::OfRemainder(pct(25.0)), false),
        ];
        Change::ActivateRule(ic_distribution::rules::Rule {
            clawback_by_cause: on,
            ..rule("a-at-fap-one", version, start, lines, vec![liable("fap-one")])
        })
    };
    w.activate(
        "by-cause",
        vec![
            Change::CreateBook {
                id: BookId::from("b-at-fap-two"),
                name: "B at FAP Two".into(),
                engagement: Some(EngagementId::from("eng-b-two")),
                owner: Some(party("fap-two")),
                house: false,
            },
            // The new adviser's book names nobody as liable: its payees bear it in their shares.
            Change::ActivateRule(rule(
                "b-at-fap-two",
                1,
                start,
                vec![
                    base("fap-two", Share::OfRemainder(pct(60.0)), true),
                    base("group-old", Share::OfRemainder(pct(40.0)), false),
                ],
                vec![],
            )),
            by_cause(true, 2),
        ],
    )
    .unwrap();
    let (mut w, policy, _) = advised_in(w);
    w.days(20);

    let attribute = |w: &mut World, policy: &PolicyId, engagement: &str| {
        let command = Command::AttributeAlteration {
            policy: policy.clone(),
            engagement: EngagementId::from(engagement),
        };
        w.send(module("policy-servicing"), command)
    };
    // Cut 100,000 of the 500,000 and say who bore the 240.00 clawed back.
    let cut = |w: &mut World, policy: &PolicyId, keep: i64| -> BTreeMap<String, i64> {
        let p = w.cluster.locate(policy).unwrap();
        let life = w.cluster.state(p).policies[policy].covers["life"].layers[0].life.clone();
        let command = CoreCommand::ReduceCover(Box::new(ic_core::command::ReduceCover {
            policy: policy.clone(),
            cover: "life".into(),
            life,
            sum_insured_minor: keep,
        }));
        let before = w.events.len();
        w.core(policy, "policy-servicing", command).unwrap();
        let mut bore = BTreeMap::new();
        for e in &w.events[before..] {
            if let Event::ClawbackRaised { reference, .. } = e {
                for l in &w.dist.allocations[reference].lines {
                    *bore.entry(l.payee.0.clone()).or_insert(0) -= l.amount_minor;
                }
            }
        }
        bore
    };
    let only = |who: &str, amount: i64| BTreeMap::from([(who.to_string(), amount)]);

    assert_eq!(code(attribute(&mut w, &PolicyId("nobody".into()), "eng-b-two")), "unknown_policy");
    assert_eq!(code(attribute(&mut w, &policy, "eng-nobody")), "unknown_engagement");

    // Attributed yesterday: it says nothing about today's alteration.
    attribute(&mut w, &policy, "eng-b-two").unwrap();
    w.days(1);
    assert_eq!(cut(&mut w, &policy, 400_000_00), only("fap-one", 240_00));

    // The adviser who wrote the cover reduces it: the original liable party, as always.
    attribute(&mut w, &policy, "eng-a-one").unwrap();
    assert_eq!(cut(&mut w, &policy, 300_000_00), only("fap-one", 240_00));

    // The new adviser reduces it: the new adviser's payees bear it, 60 to 40.
    attribute(&mut w, &policy, "eng-b-two").unwrap();
    let bore = cut(&mut w, &policy, 200_000_00);
    assert_eq!(bore, BTreeMap::from([("fap-two".to_string(), 144_00), ("group-old".to_string(), 96_00)]));
    w.assert_reconciled();

    // The switch goes off again: the same alteration falls where it used to.
    w.activate("by-cause-off", vec![by_cause(false, 3)]).unwrap();
    assert_eq!(cut(&mut w, &policy, 100_000_00), only("fap-one", 240_00));
    w.assert_reconciled();
}

/// Purpose: a policy avoided for non-disclosure is treated as never having been in force:
/// every line of commission on it is reversed, initial and renewal, from whoever received it,
/// and a clawback already raised on it is given back to whoever bore it, so each party nets
/// to nothing on the policy; nothing is left at risk; a later premium event earns nothing;
/// and the result is the same whether the premium is refunded or kept.
/// Why it matters: avoidance unwinds the contract from the start. Commission left on it is
/// paid for business that legally never existed, and a clawback left standing charges the
/// liable provider for commission that has already been taken back from the adviser.
#[test]
fn an_avoided_policy_earns_nobody_anything() {
    for premium in [ic_core::PremiumOnAvoidance::Refund, ic_core::PremiumOnAvoidance::Retain] {
        let (mut w, policy, _) = advised_in(worked_example());
        // A month of renewal, and a cut that claws back part of the initial commission.
        w.days(31);
        w.pay(&policy, "r2");
        let p = w.cluster.locate(&policy).unwrap();
        let life = w.cluster.state(p).policies[&policy].covers["life"].layers[0].life.clone();
        let reduce = CoreCommand::ReduceCover(Box::new(ic_core::command::ReduceCover {
            policy: policy.clone(),
            cover: "life".into(),
            life,
            sum_insured_minor: 400_000_00,
        }));
        w.core(&policy, "policy-servicing", reduce).unwrap();
        assert!(w.events.iter().any(|e| matches!(e, Event::ClawbackRaised { .. })));
        assert!(w.payable("a-co") > 0 && w.payable("fap-one") > 0);

        let avoid = CoreCommand::AvoidPolicy {
            policy: policy.clone(),
            case: "nd-1".into(),
            premium,
        };
        w.core(&policy, "underwriting", avoid).unwrap();
        assert_eq!((w.payable("a-co"), w.held("a-co")), (0, 0), "{premium:?}");
        assert_eq!((w.payable("fap-one"), w.held("fap-one")), (0, 0), "{premium:?}");
        assert!(w.dist.clawback_exposure(w.today()).is_empty());
        assert!(w.dist.layers.values().all(|l| l.given_up.is_none()));
        w.assert_reconciled();

        // The same status again, or anything later, changes nothing.
        let count = w.dist.allocations.len();
        inject(
            &mut w,
            p,
            CoreEvent::PolicyStatusChanged {
                policy: policy.clone(),
                from: PolicyStatus::InForce,
                to: PolicyStatus::Avoided,
            },
        );
        assert_eq!(w.dist.allocations.len(), count);
    }
}

/// Purpose: the person behind a scheme member's life is recorded when the member joins as a
/// named life or is identified later, and only on a policy the module knows.
/// Why it matters: the replacement rule matches cover by the person, not by the core's life
/// ID, which is only unique within a policy. A member whose person is unknown can never be
/// matched, so cover moved off a scheme onto a retail policy would earn initial commission
/// twice.
#[test]
fn the_person_behind_a_scheme_members_life_is_recorded() {
    let (mut w, policy, _) = advised_in(worked_example());
    let partition = w.cluster.locate(&policy).unwrap();
    let today = w.today();
    inject(
        &mut w,
        partition,
        CoreEvent::MemberJoined {
            policy: policy.clone(),
            life: ic_core::LifeId("m-7".into()),
            effective: today,
            covers: vec![],
            party: Some(ic_core::PartyId("person-7".into())),
            info: None,
        },
    );
    inject(
        &mut w,
        partition,
        CoreEvent::MemberIdentified {
            policy: policy.clone(),
            life: ic_core::LifeId("m-8".into()),
            party: ic_core::PartyId("person-8".into()),
        },
    );
    inject(
        &mut w,
        partition,
        CoreEvent::MemberIdentified {
            policy: PolicyId("nobody".into()),
            life: ic_core::LifeId("m-9".into()),
            party: ic_core::PartyId("person-9".into()),
        },
    );
    let lives = &w.dist.policies[&policy].lives;
    assert_eq!(lives["m-7"].0, "person-7");
    assert_eq!(lives["m-8"].0, "person-8");
    assert!(!w.dist.policies.contains_key(&PolicyId("nobody".into())));
}
