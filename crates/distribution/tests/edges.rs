//! The less travelled paths: structure changes inside a change set, limits
//! on what a rule may carry, money committed but not yet posted, and
//! positions that span partitions.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_core::counterparty::{InternalLedger, PartyLedger};
use ic_core::PolicyStatus;
use ic_distribution::authority::{Grant, GrantKind, LimitKind, Role};
use ic_distribution::config::{AdjustmentKind, Change};
use ic_distribution::rules::{Component, Fragment, Line, Rule, Share, WHOLE};
use ic_distribution::state::Settings;
use ic_distribution::structure::{Engagement, LicenceState, PartyKind, Relation, RelationKind};
use ic_distribution::workbench::{Action, Debt};
use ic_distribution::{Actor, BookId, Command, EngagementId, Event, FragmentId};
use ic_money::Currency::NZD;

fn start() -> chrono::NaiveDate {
    d(START.0, START.1, START.2)
}

/// Purpose: a change set can restructure a provider in one approved step: relations added and
/// ended and an engagement started, all or nothing; and it refuses a rule that follows a
/// fragment not yet in effect, a rule with more lines than one posting can carry, a layer move
/// dated before an assignment the layer already has, and servicing by an unknown engagement.
/// Why it matters: restructures are done in bulk under one set of signed instructions. If half
/// of one could activate, the licence cascade would run over a structure nobody approved.
#[test]
fn a_change_set_restructures_in_one_step_and_refuses_what_cannot_be_carried_out() {
    let (mut w, policy, _) = advised_in(worked_example());
    let today = w.today();
    w.add_party("ab-1", PartyKind::AuthorisedBody);
    w.add_party("adviser-b", PartyKind::Adviser);
    let relation = |kind| Relation {
        child: party("ab-1"),
        parent: party("fap-one"),
        kind,
        from: d(2020, 1, 1),
        to: None,
    };
    let engagement = Change::StartEngagement {
        id: EngagementId::from("eng-b"),
        engagement: Engagement {
            adviser: party("adviser-b"),
            with: party("ab-1"),
            from: d(2020, 1, 1),
            to: None,
            accreditations: BTreeSet::new(),
        },
    };
    let events = {
        w.propose("restructure", vec![Change::Relate(relation(RelationKind::Under)), Change::Relate(relation(RelationKind::LicensedBy)), engagement.clone()])
            .unwrap();
        w.send(approver(), Command::ApproveChange { id: "restructure".into(), compliance: false }).unwrap();
        w.send(compliance(), Command::ApproveChange { id: "restructure".into(), compliance: true }).unwrap()
    };
    assert!(events.iter().any(|e| matches!(e, Event::EngagementStarted { .. })));
    assert!(w.dist.config.structure.below(&party("fap-one"), today).contains(&party("adviser-b")));
    // The whole set fails together: the engagement already exists, so the relation is not ended either.
    let end = Change::EndRelation {
        child: party("ab-1"),
        parent: party("fap-one"),
        kind: RelationKind::LicensedBy,
        on: today,
    };
    assert_eq!(code(w.propose("again", vec![end.clone(), engagement])), "structure");
    assert_eq!(w.dist.config.structure.relations.iter().filter(|r| r.to.is_none()).count(), 2);
    w.activate("end", vec![end]).unwrap();
    assert_eq!(w.dist.config.structure.relations.iter().filter(|r| r.to.is_none()).count(), 1);

    let later = today.checked_add_days(chrono::Days::new(30)).unwrap();
    let fragment = Change::ActivateFragment(Fragment {
        id: FragmentId::from("later"),
        version: 1,
        effective_from: later,
        lines: vec![],
        liability: vec![],
    });
    let split = || vec![base("a-co", Share::OfRemainder(pct(75.0)), true), base("fap-one", Share::OfRemainder(pct(25.0)), false)];
    let following = Change::ActivateRule(Rule {
        fragments: vec![FragmentId::from("later")],
        ..rule("a-at-fap-one", 2, today, split(), vec![])
    });
    assert_eq!(code(w.propose("early", vec![fragment, following])), "unknown_fragment");
    let mut many = split();
    for n in 0..60 {
        many.push(Line {
            component: Component::Override(format!("level-{n}")),
            share: Share::RateOnPremium(1),
            ..base("fap-one", Share::OfRemainder(1), false)
        });
    }
    assert_eq!(code(w.propose("many", vec![Change::ActivateRule(rule("a-at-fap-one", 2, today, many, vec![]))])), "invalid_rule");

    let layer = format!("{policy}/life/1");
    let to = vec![(BookId::from("a-at-fap-one"), WHOLE)];
    w.activate("move", vec![Change::MoveLayers { layers: vec![layer.clone()], to: to.clone(), from: later }]).unwrap();
    assert_eq!(code(w.propose("before", vec![Change::MoveLayers { layers: vec![layer.clone()], to: to.clone(), from: today }])), "assignment_order");
    // The same date again replaces that assignment rather than stacking a second one.
    w.activate("same-day", vec![Change::MoveLayers { layers: vec![layer.clone()], to, from: later }]).unwrap();
    assert_eq!(w.dist.layers[&layer].assignments.len(), 2);
    let servicing = Change::SetServicing {
        policy,
        engagement: Some(EngagementId::from("ghost")),
    };
    assert_eq!(code(w.propose("ghost", vec![servicing])), "unknown_engagement");
}

/// Purpose: held money already committed to an approved action whose posting has not landed
/// cannot be committed again.
/// Why it matters: between approval and the ledger confirming there is a window. Without
/// counting what is in flight, two approvers working the same queue could release the same
/// money twice inside it.
#[test]
fn held_money_in_flight_cannot_be_committed_twice() {
    let (mut w, _, _) = advised_in(worked_example());
    w.days(7);
    w.licence("fap-one", LicenceState::Suspended, start()).unwrap();
    let held = w.held("fap-one");
    let release = || Action::Release {
        amount_minor: held,
        account_ref: "acct-fap-one".into(),
    };
    w.act("first", "fap-one", release()).unwrap();
    w.auto_deliver = false;
    w.sign("first", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.dist.balance(&party("fap-one"), PartyLedger::Held), held, "the ledger has not confirmed yet");
    assert_eq!(code(w.act("second", "fap-one", release())), "insufficient_held");
    assert_eq!(code(w.act("second", "fap-one", Action::WriteOffHeld { amount_minor: 1 })), "insufficient_held");
    w.auto_deliver = true;
    w.sync();
    assert_eq!(w.held("fap-one"), 0);
    w.settle("pay:first");
    w.assert_reconciled();
}

/// Purpose: a debt that sits on two partitions is settled by one dividend at the same cents in
/// the dollar on each, with the last partition taking the odd cent, and an advance recorded on
/// one partition is recovered from commission earned on another.
/// Why it matters: no ledger entry can span partitions, so every counterparty action is one
/// entry per partition. The pieces must add up to exactly what was approved.
#[test]
fn a_dividend_and_an_advance_recovery_are_shared_correctly_across_partitions() {
    let mut w = worked_example_in(World::with(4, Settings::default()));
    let (first, p1) = w.issue(1, 500_000_00, None);
    let mut n = 2;
    let (second, p2) = loop {
        let (policy, p) = w.issue(n, 500_000_00, None);
        if p != p1 {
            break (policy, p);
        }
        n += 1;
    };
    for policy in [&first, &second] {
        w.write(policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
        w.pay(policy, "r1");
    }
    // An advance to the adviser's company, recorded on a partition where it has earned nothing.
    let elsewhere = (0..4).find(|p| *p != p1 && *p != p2).unwrap();
    w.activate(
        "advance",
        vec![Change::Adjustment {
            reference: "adv-1".into(),
            payee: party("a-co"),
            partition: elsewhere,
            currency: NZD,
            amount_minor: 100_00,
            kind: AdjustmentKind::Advance,
        }],
    )
    .unwrap();
    w.days(6);
    let owed = w.payable("a-co");
    w.cut("a-co").unwrap();
    let statement = w.statement("a-co", 1).clone();
    assert_eq!(statement.advance_recovered_minor, 100_00, "all of it: well inside a fifth of the statement");
    assert_eq!(statement.payments[0].amount_minor, owed - 100_00);
    assert_eq!(w.ledger("a-co", PartyLedger::Advance), 0);
    w.settle("pay:a-co.1:1");
    assert_eq!(w.dist.by_partition(&party("a-co"), PartyLedger::Clearing), vec![]);

    w.until_status(&first, PolicyStatus::Lapsed);
    w.until_status(&second, PolicyStatus::Lapsed);
    let owed = w.dist.by_partition(&party("fap-one"), PartyLedger::CommissionPayable);
    assert_eq!(owed.len(), 2);
    let total: i64 = -owed.iter().map(|(_, v)| v).sum::<i64>();
    let cash = w.internal(InternalLedger::Settlement);
    let dividend = Action::RecordDividend {
        debt: Debt::Commission,
        received_minor: 333_33,
        receipt_ref: "liquidator-receipt".into(),
    };
    w.act("div", "fap-one", dividend).unwrap();
    w.sign("div", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.payable("fap-one"), 0);
    assert_eq!(w.internal(InternalLedger::Settlement), cash + 333_33);
    assert_eq!(w.internal(InternalLedger::CommissionBadDebt), total - 333_33);
    assert_eq!(w.dist.requested.keys().filter(|k| k.starts_with("wb:div:")).count(), 2);
    w.assert_reconciled();
}

/// Purpose: grants that are not cover grants give no path, and a policy that simply runs its
/// course or is surrendered closes its exposure without a clawback.
/// Why it matters: an uplift raises how much someone may decide, never whose records they may
/// touch. And only a lapse or a cancellation is the customer walking away early.
#[test]
fn an_uplift_gives_no_path_and_an_ordinary_ending_is_not_a_clawback() {
    let (mut w, policy, _) = advised_in(worked_example());
    let today = w.today();
    let mut person = person("staff-narrow", &[Role::DistributionAdmin]);
    person.paths = BTreeSet::new();
    person.grants = vec![Grant {
        id: "uplift".into(),
        kind: GrantKind::ProfileUplift {
            limits: [(LimitKind::Release, 1)].into(),
        },
        reason: "r".into(),
        approver: party("m"),
        from: today,
        until: today,
        external_ref: None,
    }];
    let hold = Command::SetPaymentHold {
        party: party("a-co"),
        reason: Some("x".into()),
    };
    assert_eq!(code(w.send(Actor::Staff(Box::new(person)), hold)), "not_authorised");

    let before = (w.payable("a-co"), w.payable("fap-one"));
    let seq = w.dist.checkpoints[&0] + 1;
    let record = ic_core::EventRecord {
        id: format!("0-{seq}"),
        partition: 0,
        seq,
        business_date: today,
        log_index: 0,
        traceparent: None,
        event: ic_core::event::Event::PolicyStatusChanged {
            policy: policy.clone(),
            from: PolicyStatus::InForce,
            to: PolicyStatus::Surrendered,
        },
    };
    w.send(module("event-relay"), Command::Ingest(Box::new(record))).unwrap();
    w.sync();
    assert_eq!((w.payable("a-co"), w.payable("fap-one")), before);
    assert!(w.dist.exposures.values().all(|e| !e.open));
    assert!(w.dist.policies[&policy].ended.is_some());
}
