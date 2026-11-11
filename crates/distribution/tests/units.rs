//! The parts that stand alone: allocation arithmetic, the dated structure,
//! the authority decision, schedule terms, and the core's own checks on a
//! counterparty posting.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use ic_core::counterparty::{CounterpartyAccount, CounterpartyLine, CounterpartyPosting, InternalLedger, PartyLedger};
use ic_core::Command as CoreCommand;
use ic_distribution::authority::{Ask, Authority, AuthorityError, Grant, GrantKind, LimitKind, Role};
use ic_distribution::model::{clawback_ppm, months_between, ClawbackStep, LayerTerms, Schedule};
use ic_distribution::rules::{portion, CommissionType, Component, Composed, Fragment, LiabilityLine, Line, Rule, Share, WHOLE};
use ic_distribution::structure::{Engagement, LicenceRecord, LicenceState, PartyKind, Relation, RelationKind, Structure, StructureError};
use ic_distribution::{EngagementId, FragmentId};
use ic_money::Currency::NZD;
use proptest::prelude::*;

fn day() -> chrono::NaiveDate {
    d(2027, 6, 1)
}

fn tail(payee: &str, ppm: u32, until: Option<chrono::NaiveDate>) -> Line {
    Line {
        commission_type: Some(CommissionType::Renewal),
        until,
        ..base(payee, Share::OfComponent(ppm), false)
    }
}

fn composed_of(rule: &Rule) -> Composed<'_> {
    Composed::of(rule, &[])
}

/// Purpose: a share of an amount is rounded half to even, exactly, for positive and negative
/// amounts.
/// Why it matters: commission is split millions of times. A rounding rule with a bias, or one
/// that treats a clawback differently from a payment, leaks cents in one direction for ever.
#[test]
fn shares_round_half_to_even_in_both_directions() {
    assert_eq!(portion(100, pct(25.0)), 25);
    assert_eq!(portion(1, pct(50.0)), 0, "0.5 rounds to the even 0");
    assert_eq!(portion(3, pct(50.0)), 2, "1.5 rounds to the even 2");
    assert_eq!(portion(5, pct(50.0)), 2, "2.5 rounds to the even 2");
    assert_eq!(portion(-3, pct(50.0)), -2);
    assert_eq!(portion(-1, pct(50.0)), 0);
    assert_eq!(portion(7, pct(33.3333)), 2);
    assert_eq!(portion(i64::MAX / 2, WHOLE), i64::MAX / 2);
    assert_eq!(portion(123_456_789, 0), 0);
}

proptest! {
    /// Purpose: whatever the amount and however the remainder is shared, the pieces a rule
    /// allocates sum to the amount exactly, for payments and for clawbacks.
    /// Why it matters: this is the invariant every reconciliation in the module rests on. A
    /// property test finds the awkward amounts a hand-picked example never would.
    #[test]
    fn a_rule_allocates_every_cent_of_any_amount(gross in -10_000_000_00i64..10_000_000_00, tail_ppm in 0u32..900_000, first in 1u32..WHOLE) {
        let mut lines = vec![base("a", Share::OfRemainder(first), true)];
        if first < WHOLE {
            lines.push(base("b", Share::OfRemainder(WHOLE - first), false));
        }
        if tail_ppm > 0 {
            lines.push(tail("t", tail_ppm, None));
        }
        let rule = rule("book", 1, day(), lines, vec![]);
        let composed = composed_of(&rule);
        prop_assert!(composed.validate(day()).is_ok());
        for ctype in [CommissionType::Initial, CommissionType::Renewal] {
            let out = composed.allocate_base(gross, ctype, day()).unwrap();
            prop_assert_eq!(out.iter().map(|a| a.amount_minor).sum::<i64>(), gross);
        }
    }
}

/// Purpose: a tail is a share of the whole component until its end date; the day after, the
/// ongoing parties share everything with no new rule.
/// Why it matters: tails that end are the reason shares are of the remainder. If the rule had
/// to be rewritten when a tail expired, every expiry would be a manual change someone forgets.
#[test]
fn a_tail_that_ends_flows_to_the_ongoing_parties_without_a_new_rule() {
    let end = d(2028, 5, 31);
    let lines = vec![
        base("adviser", Share::OfRemainder(pct(75.0)), true),
        base("new-fap", Share::OfRemainder(pct(25.0)), false),
        tail("old-fap", pct(25.0), Some(end)),
    ];
    let rule = rule("book", 1, day(), lines, vec![]);
    let composed = composed_of(&rule);
    composed.validate(day()).unwrap();
    let amounts = |on| -> BTreeMap<String, i64> {
        composed
            .allocate_base(100_00, CommissionType::Renewal, on)
            .unwrap()
            .into_iter()
            .map(|a| (a.payee.0, a.amount_minor))
            .collect()
    };
    assert_eq!(amounts(end), BTreeMap::from([("adviser".into(), 56_25), ("new-fap".into(), 18_75), ("old-fap".into(), 25_00)]));
    assert_eq!(amounts(end.succ_opt().unwrap()), BTreeMap::from([("adviser".into(), 75_00), ("new-fap".into(), 25_00)]));
    // The tail is renewal only: initial commission never saw it.
    let initial = composed.allocate_base(100_00, CommissionType::Initial, day()).unwrap();
    assert!(initial.iter().all(|a| a.payee.0 != "old-fap"));
    assert_eq!(composed.payees().len(), 3);
}

/// Purpose: each way a rule can fail to add up is caught by validation, on every date its
/// lines change, with a message that says what is wrong.
/// Why it matters: validation is the gate between a typed rule and money. Anything it lets
/// through that does not allocate in full becomes an exception on every premium in the book.
#[test]
fn rule_validation_catches_every_way_a_rule_fails_to_add_up() {
    let whole = || base("a", Share::OfRemainder(WHOLE), true);
    let check = |lines: Vec<Line>, liability: Vec<LiabilityLine>| {
        let rule = rule("book", 1, day(), lines, liability);
        composed_of(&rule).validate(day()).map_err(|e| e.to_string())
    };
    assert!(check(vec![whole()], vec![]).is_ok());
    let err = |lines, liability| check(lines, liability).unwrap_err();
    assert!(err(vec![], vec![]).contains("paid to nobody"));
    assert!(err(vec![base("a", Share::OfRemainder(pct(90.0)), true)], vec![]).contains("not allocated in full"));
    assert!(err(vec![base("a", Share::OfComponent(pct(90.0)), true)], vec![]).contains("not allocated in full"));
    assert!(err(vec![whole(), tail("t", WHOLE, None)], vec![]).contains("not allocated in full"));
    assert!(err(vec![base("a", Share::OfRemainder(WHOLE), false)], vec![]).contains("absorb rounding"));
    assert!(err(vec![whole(), base("b", Share::RateOnPremium(5), false)], vec![]).contains("does not fit"));
    assert!(err(vec![whole(), base("b", Share::OfRemainder(0), false)], vec![]).contains("does not fit"));
    let fee = |share| Line {
        component: Component::FixedFee("referral".into()),
        share,
        ..base("b", Share::OfRemainder(1), false)
    };
    assert!(err(vec![whole(), fee(Share::Fixed(0))], vec![]).contains("fee:referral"));
    assert!(check(vec![whole(), fee(Share::Fixed(10))], vec![]).is_ok());
    let backwards = Line {
        until: Some(d(2020, 1, 1)),
        from: d(2021, 1, 1),
        ..whole()
    };
    assert!(err(vec![backwards], vec![]).contains("ends before it starts"));
    // A line that ends leaves base unallocated from the next day.
    let ending = Line {
        until: Some(d(2027, 12, 31)),
        ..whole()
    };
    assert!(err(vec![ending], vec![]).contains("from 2028-01-01"));
    // A line that starts later leaves a gap until it does.
    let later = Line { from: d(2028, 1, 1), ..whole() };
    assert!(err(vec![later], vec![]).contains("from 2027-06-01"));
    let half = LiabilityLine { share: pct(50.0), ..liable("a") };
    assert!(err(vec![whole()], vec![half.clone()]).contains("clawback liability for base sums to 500000"));
    let other_half = LiabilityLine {
        liable: party("b"),
        from: d(2028, 1, 1),
        until: Some(d(2028, 6, 30)),
        ..half.clone()
    };
    let first_half = LiabilityLine {
        until: Some(d(2027, 12, 31)),
        share: WHOLE,
        ..half.clone()
    };
    assert!(err(vec![whole()], vec![first_half, other_half]).contains("from 2028-01-01"));
    // No absorber is an error at allocation too, when rounding actually leaves a cent.
    let no_absorber = rule("book", 1, day(), vec![base("a", Share::OfRemainder(pct(50.0)), false), base("b", Share::OfRemainder(pct(50.0)), false)], vec![]);
    assert!(composed_of(&no_absorber).allocate_base(1, CommissionType::Initial, day()).is_err());
    assert!(composed_of(&no_absorber).allocate_base(2, CommissionType::Initial, day()).is_ok());
    assert!(composed_of(&no_absorber).allocate_base(2, CommissionType::Initial, d(2019, 1, 1)).is_err());
}

/// Purpose: a book's own lines for a component replace the shared fragment's lines for that
/// component and leave the fragment's other components in place; rule and fragment hashes
/// change when their content does.
/// Why it matters: negotiated exceptions are written as an override on one book. The fragment
/// must keep applying to everything else, and the content hash is how an activated rule is
/// proved to be the one that was approved.
#[test]
fn a_books_own_lines_replace_the_fragments_lines_for_the_same_component() {
    let over = |payee: &str, rate: f64| Line {
        component: Component::Override("group".into()),
        share: Share::RateOnPremium(pct(rate)),
        ..base(payee, Share::OfRemainder(1), false)
    };
    let fee = Line {
        component: Component::FixedFee("referral".into()),
        share: Share::Fixed(25_00),
        ..base("referrer", Share::OfRemainder(1), false)
    };
    let fragment = Fragment {
        id: FragmentId::from("std"),
        version: 1,
        effective_from: day(),
        lines: vec![over("group", 2.0), fee],
        liability: vec![liable("group")],
    };
    let plain = rule("book", 1, day(), vec![base("a", Share::OfRemainder(WHOLE), true)], vec![]);
    let composed = Composed::of(&plain, &[&fragment]);
    assert_eq!(composed.extra_components(CommissionType::Initial, day()).len(), 2);
    assert_eq!(composed.allocate_extra(&Component::Override("group".into()), 1_000_00, false, CommissionType::Renewal, day())[0].amount_minor, 20_00);
    assert!(composed.allocate_extra(&Component::FixedFee("referral".into()), 1_000_00, false, CommissionType::Renewal, day()).is_empty());
    assert_eq!(composed.allocate_extra(&Component::FixedFee("referral".into()), 1_000_00, true, CommissionType::Initial, day())[0].amount_minor, 25_00);
    assert_eq!(composed.liable(&Component::Base, day()), vec![(party("group"), WHOLE, None)]);

    let mut negotiated = plain.clone();
    negotiated.lines.push(over("group", 1.0));
    negotiated.liability = vec![liable("a")];
    let composed = Composed::of(&negotiated, &[&fragment]);
    let paid = composed.allocate_extra(&Component::Override("group".into()), 1_000_00, false, CommissionType::Renewal, day());
    assert_eq!(paid.len(), 1);
    assert_eq!(paid[0].amount_minor, 10_00);
    assert_eq!(composed.liable(&Component::Base, day()), vec![(party("a"), WHOLE, None)]);
    assert_eq!(composed.extra_components(CommissionType::Initial, day()).len(), 2, "the fee still comes from the fragment");

    assert_ne!(plain.content_hash(), negotiated.content_hash());
    assert_eq!(plain.content_hash(), plain.clone().content_hash());
    let mut changed = fragment.clone();
    changed.version = 2;
    assert_ne!(fragment.content_hash(), changed.content_hash());
    assert_eq!(Component::Base.label(), "base");
}

/// Purpose: a licence problem anywhere up the chain reaches every engagement and payee that
/// depends on it, on the dates it applies and no others, however many levels there are.
/// Why it matters: addendum entry 4 makes licence status a cascading dated attribute. The
/// cascade is computed, not stored, so this is the computation that decides who may be paid.
#[test]
fn a_licence_problem_cascades_through_the_dated_structure() {
    let mut s = Structure::default();
    for (id, kind) in [
        ("group", PartyKind::Group),
        ("fap", PartyKind::Fap),
        ("body", PartyKind::AuthorisedBody),
        ("adviser", PartyKind::Adviser),
        ("adviser-co", PartyKind::Company),
        ("dual", PartyKind::Adviser),
        ("other-fap", PartyKind::Fap),
        ("idle", PartyKind::Adviser),
    ] {
        s.add_party(party(id), kind).unwrap();
    }
    let relate = |s: &mut Structure, child: &str, parent: &str, kind, from, to| {
        s.relate(Relation {
            child: party(child),
            parent: party(parent),
            kind,
            from,
            to,
        })
    };
    relate(&mut s, "fap", "group", RelationKind::Under, d(2020, 1, 1), None).unwrap();
    relate(&mut s, "body", "fap", RelationKind::LicensedBy, d(2020, 1, 1), Some(d(2028, 1, 1))).unwrap();
    relate(&mut s, "adviser-co", "adviser", RelationKind::PayeeFor, d(2020, 1, 1), None).unwrap();
    assert!(relate(&mut s, "body", "fap", RelationKind::LicensedBy, d(2021, 1, 1), None).is_err(), "already in force");
    assert!(relate(&mut s, "body", "fap", RelationKind::LicensedBy, d(2029, 1, 1), Some(d(2029, 1, 1))).is_err());
    let engage = |s: &mut Structure, id: &str, adviser: &str, with: &str| {
        s.start_engagement(
            EngagementId::from(id),
            Engagement {
                adviser: party(adviser),
                with: party(with),
                from: d(2020, 1, 1),
                to: None,
                accreditations: BTreeSet::from(["term-life".to_string()]),
            },
        )
    };
    engage(&mut s, "e-adviser", "adviser", "body").unwrap();
    engage(&mut s, "e-dual-1", "dual", "fap").unwrap();
    engage(&mut s, "e-dual-2", "dual", "other-fap").unwrap();
    let bad = Engagement {
        adviser: party("idle"),
        with: party("fap"),
        from: d(2020, 1, 1),
        to: Some(d(2020, 1, 1)),
        accreditations: BTreeSet::new(),
    };
    assert!(matches!(s.start_engagement(EngagementId::from("bad"), bad), Err(StructureError::Invalid(_))));
    assert!(matches!(s.end_engagement(&EngagementId::from("ghost"), day()), Err(StructureError::UnknownEngagement(_))));

    let suspended = LicenceRecord {
        state: LicenceState::Suspended,
        effective: d(2027, 3, 1),
        source_ref: "notice-1".into(),
    };
    s.record_licence(&party("fap"), suspended.clone()).unwrap();
    s.record_licence(&party("fap"), suspended.clone()).unwrap();
    assert_eq!(s.parties[&party("fap")].licence.len(), 1);
    s.record_licence(
        &party("fap"),
        LicenceRecord {
            state: LicenceState::Active,
            effective: d(2027, 9, 1),
            source_ref: "notice-2".into(),
        },
    )
    .unwrap();

    let before = d(2027, 2, 28);
    let during = d(2027, 6, 1);
    let after = d(2027, 9, 1);
    let e = EngagementId::from("e-adviser");
    assert!(s.may_advise(&e, "term-life", before).is_ok());
    assert!(s.may_advise(&e, "term-life", during).unwrap_err().contains("fap is Suspended since 2027-03-01"));
    assert!(s.may_advise(&e, "term-life", after).is_ok());
    assert!(s.may_advise(&e, "health", before).unwrap_err().contains("not accredited"));
    assert!(s.may_advise(&e, "term-life", d(2019, 1, 1)).unwrap_err().contains("not in force"));
    assert!(s.may_advise(&EngagementId::from("ghost"), "term-life", before).is_err());
    assert_eq!(s.engagement_impairment(&e, during).unwrap().source, party("fap"));
    assert!(s.engagement_impairment(&EngagementId::from("ghost"), during).is_none());

    // Payees: the provider, the body under its licence, the adviser whose only engagement is
    // impaired, and the company paid for that adviser. Not the adviser with a second, sound
    // engagement, not the group above, not an adviser with no engagement at all.
    let (engagements, payees) = s.affected_by(&party("fap"), during);
    assert_eq!(engagements, BTreeSet::from([EngagementId::from("e-adviser"), EngagementId::from("e-dual-1")]));
    assert_eq!(payees, BTreeSet::from([party("fap"), party("body"), party("adviser"), party("adviser-co")]));
    assert!(s.payee_impairment(&party("dual"), during).is_none());
    assert!(s.payee_impairment(&party("idle"), during).is_none());
    assert!(s.payee_impairment(&party("ghost"), during).is_none());
    assert_eq!(s.affected_by(&party("fap"), after), (BTreeSet::new(), BTreeSet::new()));
    // The body left the provider's licence in 2028: a later suspension does not reach it.
    s.record_licence(
        &party("fap"),
        LicenceRecord {
            state: LicenceState::Revoked,
            effective: d(2028, 6, 1),
            source_ref: "notice-3".into(),
        },
    )
    .unwrap();
    assert!(s.payee_impairment(&party("body"), d(2028, 7, 1)).is_none());
    assert!(s.payee_impairment(&party("fap"), d(2028, 7, 1)).is_some());
    // An adviser's own record suspends them wherever they are engaged.
    s.record_licence(
        &party("dual"),
        LicenceRecord {
            state: LicenceState::Lapsed,
            effective: d(2027, 10, 1),
            source_ref: "notice-4".into(),
        },
    )
    .unwrap();
    assert_eq!(s.payee_impairment(&party("dual"), d(2027, 10, 2)).unwrap().state, LicenceState::Lapsed);
    assert!(s.engagement_impairment(&EngagementId::from("e-dual-2"), d(2027, 10, 2)).is_some());
    assert!(matches!(s.record_licence(&party("ghost"), suspended.clone()), Err(StructureError::UnknownParty(_))));
    assert!(matches!(s.mark_ceased(&party("ghost"), day(), ic_distribution::structure::CeasedReason::Deceased), Err(StructureError::UnknownParty(_))));

    // Who a group sees: everything under it on the day.
    let below = s.below(&party("group"), during);
    assert!(below.contains(&party("fap")) && below.contains(&party("body")) && below.contains(&party("adviser")) && below.contains(&party("dual")));
    assert!(!below.contains(&party("other-fap")));
    assert!(!s.below(&party("group"), d(2028, 7, 1)).contains(&party("body")));
    assert!(StructureError::DuplicateEngagement(e).to_string().contains("already exists"));
}

/// Purpose: a chain of licence relations longer than the depth limit is treated as a loop and
/// refused, so the cascade always terminates.
/// Why it matters: the cascade is recursive and runs inside the state machine. A structure that
/// made it run for ever would stop the module for every payee.
#[test]
fn an_impossibly_deep_structure_is_refused() {
    let mut s = Structure::default();
    for n in 0..40 {
        s.add_party(party(&format!("p{n}")), PartyKind::Team).unwrap();
    }
    let mut refused = false;
    for n in (0..39).rev() {
        let r = s.relate(Relation {
            child: party(&format!("p{n}")),
            parent: party(&format!("p{}", n + 1)),
            kind: RelationKind::LicensedBy,
            from: d(2020, 1, 1),
            to: None,
        });
        refused |= r.is_err();
    }
    assert!(refused, "a chain past the depth limit cannot be built");
    assert!(s.payee_impairment(&party("p0"), day()).is_none());
}

/// Purpose: the authority decision answers the four questions and records what it relied on:
/// limits come from the best template, an uplift raises them only for its dates and never past
/// the ceiling, and a one-off grant gives a path to one record for one action.
/// Why it matters: "was this person allowed to do that on that day" has to be answerable from
/// the record years later. Ceilings are what stop a manager granting more than the role allows.
#[test]
fn the_authority_decision_applies_limits_uplifts_ceilings_and_one_off_grants() {
    let authority = Authority::shipped().unwrap();
    let none = BTreeSet::new();
    let mut approver = person("p", &[Role::CounterpartyFinanceApprover]);
    let ask = |amount| Ask::new("workbench.approve", "fap-one").amount(LimitKind::WriteOff, amount);
    let ok = authority.decide(&approver, &ask(25_000_00), day(), &none).unwrap();
    assert_eq!((ok.limit_minor, ok.profile_version, ok.purpose.as_str()), (25_000_00, 1, "case-1"));
    assert_eq!(ok.roles, vec![Role::CounterpartyFinanceApprover]);
    assert!(matches!(authority.decide(&approver, &ask(25_000_01), day(), &none), Err(AuthorityError::Denied(m)) if m.contains("above your limit")));

    let uplift = |limit, from, until| Grant {
        id: "uplift-1".into(),
        kind: GrantKind::ProfileUplift {
            limits: BTreeMap::from([(LimitKind::WriteOff, limit)]),
        },
        reason: "year end".into(),
        approver: party("head-of-finance"),
        from,
        until,
        external_ref: Some("iga-123".into()),
    };
    approver.grants = vec![uplift(100_000_00, d(2027, 6, 1), d(2027, 6, 30))];
    let raised = authority.decide(&approver, &ask(90_000_00), day(), &none).unwrap();
    assert_eq!(raised.grants, vec!["uplift-1".to_string()]);
    assert!(authority.decide(&approver, &ask(90_000_00), d(2027, 7, 1), &none).is_err(), "the uplift has ended");
    approver.grants = vec![uplift(900_000_000_00, d(2027, 6, 1), d(2027, 6, 30))];
    assert_eq!(authority.decide(&approver, &ask(1), day(), &none).unwrap().limit_minor, 5_000_000_00, "capped at the ceiling");
    approver.grants = vec![uplift(1, d(2027, 6, 1), d(2027, 6, 30))];
    assert_eq!(authority.decide(&approver, &ask(1), day(), &none).unwrap().limit_minor, 25_000_00, "an uplift never lowers");

    let mut narrow = person("q", &[Role::Legal]);
    narrow.paths = BTreeSet::new();
    let legal = Ask::new("workbench.approve_legal", "fap-one");
    assert!(matches!(authority.decide(&narrow, &legal, day(), &none), Err(AuthorityError::Denied(m)) if m.contains("no path")));
    narrow.grants = vec![Grant {
        id: "one-off-1".into(),
        kind: GrantKind::OneOff {
            action: "workbench.approve_legal".into(),
            record: "fap-one".into(),
        },
        reason: "conflict cover".into(),
        approver: party("general-counsel"),
        from: day(),
        until: day(),
        external_ref: None,
    }];
    assert_eq!(authority.decide(&narrow, &legal, day(), &none).unwrap().grants, vec!["one-off-1".to_string()]);
    assert!(authority.decide(&narrow, &Ask::new("workbench.approve_legal", "fap-two"), day(), &none).is_err());
    assert!(authority.decide(&narrow, &Ask::new("workbench.view", "fap-one"), day(), &none).is_err(), "one action only");
    // A cover grant uses the paths its holder was given by the caller.
    narrow.grants = vec![Grant {
        id: "cover-1".into(),
        kind: GrantKind::Cover { of: party("colleague") },
        reason: "leave".into(),
        approver: party("general-counsel"),
        from: day(),
        until: day(),
        external_ref: None,
    }];
    let covered = BTreeSet::from(["*".to_string()]);
    assert_eq!(authority.decide(&narrow, &legal, day(), &covered).unwrap().grants, vec!["cover-1".to_string()]);

    assert!(matches!(Authority::new("this is not cedar"), Err(AuthorityError::Policies(_))));
    assert!(matches!(authority.decide(&narrow, &Ask::new("bad action \u{0}", "x"), day(), &covered), Err(AuthorityError::Denied(_) | AuthorityError::Request(_))));
    for role in [
        Role::DistributionAdmin,
        Role::CommissionOps,
        Role::CommissionApprover,
        Role::Compliance,
        Role::CounterpartyFinance,
        Role::CounterpartyFinanceApprover,
        Role::Legal,
        Role::ConductOfficer,
    ] {
        assert!(!role.key().is_empty());
        let classes = role.data_classes();
        assert!(classes.contains(&"ordinary") && !classes.contains(&"clinical"), "no distribution role holds clinical data");
    }
    assert!(AuthorityError::Request("x".into()).to_string().contains("could not build"));
}

/// Purpose: months in force, the clawback scale and a layer's terms are computed the way the
/// schedule says, at each boundary.
/// Why it matters: the scale decides whether a lapse on the anniversary costs the provider
/// all, half or none of the initial commission. Off by a day is off by half the commission.
#[test]
fn the_clawback_scale_steps_on_whole_months_in_force() {
    assert_eq!(months_between(d(2027, 1, 31), d(2027, 2, 28)), 0);
    assert_eq!(months_between(d(2027, 1, 31), d(2027, 3, 31)), 2);
    assert_eq!(months_between(d(2027, 1, 15), d(2028, 1, 14)), 11);
    assert_eq!(months_between(d(2027, 1, 15), d(2028, 1, 15)), 12);
    assert_eq!(months_between(d(2027, 1, 15), d(2027, 1, 15)), 0);
    assert_eq!(months_between(d(2027, 1, 15), d(2026, 1, 15)), 0);
    let s = schedule();
    assert_eq!(clawback_ppm(&s.clawback, 11), WHOLE);
    assert_eq!(clawback_ppm(&s.clawback, 12), pct(50.0));
    assert_eq!(clawback_ppm(&s.clawback, 23), pct(50.0));
    assert_eq!(clawback_ppm(&s.clawback, 24), 0);
    let terms = LayerTerms::from_schedule(&s, "upfront", 1_200_00).unwrap();
    assert_eq!((terms.schedule_version, terms.rates.initial_ppm, terms.cooling_off_days), (1, WHOLE, 14));
    assert!(LayerTerms::from_schedule(&s, "ghost", 1).is_none());
    assert_eq!(s.content_hash(), schedule().content_hash());
    let bad_scale = Schedule {
        clawback: vec![ClawbackStep { months_under: 24, ppm: WHOLE }, ClawbackStep { months_under: 12, ppm: 1 }],
        ..schedule()
    };
    assert!(bad_scale.validate().unwrap_err().contains("clawback scale"));
    let mut too_much = schedule();
    too_much.options.get_mut("level").unwrap().renewal_ppm = WHOLE + 1;
    assert!(too_much.validate().unwrap_err().contains("cannot exceed"));
}

fn posting(reference: &str, lines: Vec<(CounterpartyAccount, i64)>) -> CounterpartyPosting {
    CounterpartyPosting {
        reference: reference.into(),
        kind: "test_posting".into(),
        currency: NZD,
        value_date: None,
        policy: None,
        reverses: None,
        lines: lines.into_iter().map(|(account, amount_minor)| CounterpartyLine { account, amount_minor }).collect(),
        detail: BTreeMap::new(),
    }
}

/// Purpose: the core itself refuses a counterparty posting that is malformed, unbalanced,
/// future dated, repeated, in the wrong currency for its policy, or sent by a module that may
/// not post, and a refusal leaves the ledger untouched.
/// Why it matters: the core is the last line. Whatever a module above it gets wrong, the ledger
/// must stay balanced and each reference must post once, or no reconciliation above it holds.
#[test]
fn the_core_refuses_a_counterparty_posting_that_is_not_well_formed() {
    let mut w = World::new();
    let (policy, _) = w.issue(1, 500_000_00, None);
    let cash = || CounterpartyAccount::Internal(InternalLedger::Settlement);
    let held = |who: &str| acct(who, PartyLedger::Held);
    let good = || vec![(cash(), 10_00), (held("p-1"), -10_00)];
    let submit = |w: &mut World, caller: &str, p: CounterpartyPosting| w.cluster.submit(0, caller, CoreCommand::PostCounterparty(Box::new(p)));
    let entries = w.cluster.state(0).ledger.entry_count();
    let bad = "invalid_counterparty_posting";
    let expense = |product: &str, cover: &str| {
        CounterpartyAccount::Internal(InternalLedger::CommissionExpense {
            product: product.into(),
            cover_type: cover.into(),
        })
    };
    let cases: Vec<(&str, CounterpartyPosting)> = vec![
        (bad, posting("has space", good())),
        (bad, posting("", good())),
        (bad, posting(&"x".repeat(121), good())),
        (bad, CounterpartyPosting { kind: "bad kind".into(), ..posting("r", good()) }),
        (bad, posting("r", vec![])),
        (bad, posting("r", (0..201).map(|n| (held("p-1"), if n % 2 == 0 { 1 } else { -1 })).collect())),
        (bad, posting("r", vec![(cash(), 10_00), (held("p-1"), -9_99)])),
        (bad, posting("r", vec![(cash(), 0), (held("p-1"), 0)])),
        (bad, posting("r", vec![(cash(), 10_00), (held("p 1"), -10_00)])),
        (bad, posting("r", vec![(expense("term life", "life"), 10_00), (held("p-1"), -10_00)])),
        (bad, posting("r", vec![(CounterpartyAccount::Internal(InternalLedger::WriteOff { class: "".into() }), 10_00), (held("p-1"), -10_00)])),
        (bad, CounterpartyPosting { value_date: w.today().succ_opt(), ..posting("r", good()) }),
        (bad, CounterpartyPosting { policy: Some(policy.clone()), currency: ic_money::Currency::AUD, ..posting("r", good()) }),
        ("unknown_reference", CounterpartyPosting { reverses: Some("never-posted".into()), ..posting("r", good()) }),
    ];
    for (n, (expected, p)) in cases.into_iter().enumerate() {
        assert_eq!(submit(&mut w, "distribution", p).unwrap_err().code, expected, "case {n}");
    }
    assert_eq!(submit(&mut w, "claims", posting("r", good())).unwrap_err().code, "caller_not_allowed");
    let ghost = CounterpartyPosting {
        policy: Some(ic_core::PolicyId("ghost".into())),
        ..posting("r", good())
    };
    assert!(submit(&mut w, "distribution", ghost).is_err());
    assert_eq!(w.cluster.state(0).ledger.entry_count(), entries, "a refusal writes nothing");
    assert!(w.cluster.state(0).counterparty.is_empty());

    // A good one posts once: the reference is spent whatever key carries it.
    let with_policy = CounterpartyPosting {
        policy: Some(policy),
        value_date: Some(w.today()),
        ..posting("r", vec![(expense("term-life", "life"), 10_00), (held("p-1"), -10_00)])
    };
    submit(&mut w, "distribution", with_policy).unwrap();
    assert!(w.cluster.state(0).counterparty.has("r"));
    assert_eq!(submit(&mut w, "counterparty-finance", posting("r", good())).unwrap_err().code, "duplicate_reference");
    let reversal = CounterpartyPosting {
        reverses: Some("r".into()),
        ..posting("r-reversed", vec![(held("p-1"), 10_00), (expense("term-life", "life"), -10_00)])
    };
    submit(&mut w, "distribution", reversal).unwrap();
    assert_eq!(w.cluster.state(0).ledger.entry_count(), entries + 2);
    assert!(w.cluster.reconcile().is_empty());
    // The module learns of postings it did not ask for and keeps the party's position.
    w.sync();
    assert_eq!(w.dist.balance(&party("p-1"), PartyLedger::Held), 0);
    assert_eq!(w.dist.movements.len(), 2);
    assert_eq!(w.held("p-1"), 0);
}
