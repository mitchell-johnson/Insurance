//! A scheme whose broker underwrites its members under a delegated authority.

mod common;

use common::*;
use ic_core::{LifeId, PolicyId};
use ic_underwriting::command::{Command, DelegatedDecision, Effect, View};
use ic_underwriting::config::DelegatedAuthority;
use ic_underwriting::model::Entry;

fn authority() -> DelegatedAuthority {
    DelegatedAuthority {
        id: "broker-1".into(),
        version: 1,
        delegate: "broker-tpa".into(),
        products: ["group-life".to_string()].into(),
        per_life_minor: [("group_life".to_string(), 400_000_00)].into(),
        aggregate_minor: 2_000_000_00,
        max_loading_percent: 50,
        may_exclude: false,
        may_decline: false,
        rule_set: None,
        effective_from: d(2026, 1, 1),
        effective_to: None,
        reinsurer_agreement_ref: Some("RE-9".into()),
    }
}

fn world() -> World {
    let mut w = World::new();
    w.ok(
        "config",
        Command::ActivateDelegatedAuthority {
            authority: Box::new(authority()),
            approval: approval(),
        },
    );
    w
}

/// A member's excess on a scheme naming `authority`.
fn excess(w: &World, id: &str, sum: i64, authority: Option<&str>) -> Command {
    let mut open = w.open_command(id, &["L1"], &[("L1", "life", "group_life", sum)], false);
    open.product_code = "group-life".into();
    open.policy = PolicyId::from("SCHEME-1");
    open.entry = Entry::GroupExcess {
        policy: open.policy.clone(),
        takeover_accepted_minor: None,
        reinsurance_reference: None,
        delegated_authority: authority.map(Into::into),
    };
    Command::OpenProposal(Box::new(open))
}

fn decision(w: &World, id: &str, loading: u32) -> DelegatedDecision {
    DelegatedDecision {
        proposal: id.into(),
        life: "L1".into(),
        authority: "broker-1".into(),
        terms: vec![accept("life", loading, None)],
        effective: w.today(),
        rule_version: None,
        evidence: Default::default(),
        decided_by: "broker-underwriter-2".into(),
        delegate_ref: format!("BRK-{id}"),
        consent_ref: "CONS-9".into(),
    }
}

/// Purpose: a member's excess on a scheme whose broker holds a delegated authority is put to
/// the broker: the delegate is asked through an effect naming the proposal, life, party,
/// cover, amount and authority; the member is not asked to disclose to us; and the broker's
/// decision within the authority binds. Above the per-life ceiling the excess runs as any
/// other, through our own disclosure. A decision outside the authority comes back to our own
/// queue and the proposal is no longer the delegate's. A scheme naming an authority not in force is refused.
/// Why it matters: a broker that underwrites under authority must not have its members asked
/// the same questions twice, while an amount or a decision beyond what the authority allows
/// must still reach the insurer's own underwriters rather than bind it.
#[test]
fn a_delegate_decides_a_members_excess_within_its_authority() {
    let mut w = world();

    // Not in force: refused, with nothing opened.
    assert_eq!(w.err("group-admin", excess(&w, "X0", 300_000_00, Some("broker-0"))), "authority");

    // Within the authority: the delegate is asked and the member does not disclose to us.
    let out = w.ok("group-admin", excess(&w, "X1", 300_000_00, Some("broker-1")));
    let asked: Vec<_> = out
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::RequestDelegatedDecision {
                proposal,
                life,
                cover,
                cover_type,
                sum_insured_minor,
                authority,
                ..
            } => Some((
                proposal.0.as_str(),
                life.clone(),
                cover.as_str(),
                cover_type.as_str(),
                *sum_insured_minor,
                authority.as_str(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(asked, vec![("X1", LifeId::from("L1"), "life", "group_life", 300_000_00, "broker-1")]);
    assert_eq!(w.p("X1").delegated_to.as_deref(), Some("broker-1"));
    let d = w.disclosure("X1", "L1", &clean());
    assert_eq!(w.err("new-business", Command::SubmitDisclosure(Box::new(d))), "delegated");
    let bound = w.send("delegate-api", Command::DelegatedDecision(Box::new(decision(&w, "X1", 25)))).unwrap();
    assert!(matches!(bound.view, Some(View::Delegated { accepted: true, .. })), "{:?}", bound.view);
    assert!(!w.p("X1").lives[&LifeId::from("L1")].decisions.is_empty());

    // Above the ceiling: no delegate, our own disclosure.
    let out = w.ok("group-admin", excess(&w, "X2", 400_000_01, Some("broker-1")));
    assert!(!out.effects.iter().any(|e| matches!(e, Effect::RequestDelegatedDecision { .. })));
    assert_eq!(w.p("X2").delegated_to, None);
    w.disclose("X2", "L1", &clean());

    // Outside the authority: comes back to our queue, and the member may disclose to us.
    w.ok("group-admin", excess(&w, "X3", 300_000_00, Some("broker-1")));
    let back = w.send("delegate-api", Command::DelegatedDecision(Box::new(decision(&w, "X3", 75)))).unwrap();
    assert!(matches!(back.view, Some(View::Delegated { accepted: false, .. })));
    assert_eq!(w.p("X3").delegated_to, None);
    assert!(w.p("X3").lives[&LifeId::from("L1")].open_case().is_some(), "on our own queue");
}
