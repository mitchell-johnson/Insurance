//! Commission through the rest of a policy's life: indexation, merged and
//! unwound layers, a move between partitions, and replacement business.

mod common;

use common::*;
use ic_core::event::{Event as CoreEvent, LayerInfo};
use ic_core::{Command as CoreCommand, EventRecord, LayerOrigin, PolicyId, UnwindCause};
use ic_distribution::config::Change;
use ic_distribution::model::Schedule;
use ic_distribution::rules::WHOLE;
use ic_distribution::state::Settings;
use ic_distribution::{Command, Event};
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
