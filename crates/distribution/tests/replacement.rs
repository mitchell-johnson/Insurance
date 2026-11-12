//! Replacements nobody declared: new cover written on a life soon after
//! cover on that life was given up, matched by rule when it is written.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::{Command as CoreCommand, PolicyId, PolicyStatus};
use ic_distribution::config::Change;
use ic_distribution::host::{ask, Question};
use ic_distribution::model::MatchReason;
use ic_distribution::rules::{HeldReason, Share};
use ic_distribution::state::Settings;
use ic_distribution::structure::{PartyKind, Relation, RelationKind};
use ic_distribution::{BookId, Command, EngagementId, Event};

const PERSON: &str = "party-00000001";

/// The worked example with two more advisers: B at FAP Two, which sits in
/// the same group as A's FAP One, and C at FAP Three, which is on its own.
fn scene(settings: Settings) -> World {
    let mut w = worked_example_in(World::with(1, settings));
    for (id, kind) in [
        ("adviser-b", PartyKind::Adviser),
        ("adviser-c", PartyKind::Adviser),
        ("fap-three", PartyKind::Fap),
    ] {
        w.add_party(id, kind);
    }
    w.payee("fap-three");
    w.account("fap-three", "acct-fap-three");
    w.engage("eng-b-two", "adviser-b", "fap-two", d(2020, 1, 1));
    w.engage("eng-c-three", "adviser-c", "fap-three", d(2020, 1, 1));
    for fap in ["fap-one", "fap-two"] {
        let under = Relation {
            child: party(fap),
            parent: party("group-old"),
            kind: RelationKind::Under,
            from: d(2020, 1, 1),
            to: None,
        };
        w.send(admin(), Command::Relate(under)).unwrap();
    }
    let start = d(START.0, START.1, START.2);
    let book = |id: &str, engagement: &str, fap: &str| {
        vec![
            Change::CreateBook {
                id: BookId::from(id),
                name: id.into(),
                engagement: Some(EngagementId::from(engagement)),
                owner: Some(party(fap)),
                house: false,
            },
            Change::ActivateRule(rule(id, 1, start, vec![base(fap, Share::OfRemainder(pct(100.0)), true)], vec![liable(fap)])),
        ]
    };
    let mut changes = book("b-at-fap-two", "eng-b-two", "fap-two");
    changes.extend(book("c-at-fap-three", "eng-c-three", "fap-three"));
    w.activate("more-advisers", changes).unwrap();
    w
}

fn layer_of(policy: &PolicyId) -> String {
    format!("{policy}/life/1")
}

fn cancel(w: &mut World, policy: &PolicyId) {
    w.core(policy, "policy-servicing", CoreCommand::CancelPolicy { policy: policy.clone() })
        .unwrap();
}

/// The match the rule announced in these events, if it made one.
fn matched(events: &[Event]) -> Option<(i64, MatchReason)> {
    events.iter().find_map(|e| match e {
        Event::ReplacementMatched { matched_minor, reason, .. } => Some((*matched_minor, *reason)),
        _ => None,
    })
}

/// What an allocation paid to each payee, by commission type.
fn paid(w: &World, reference: &str) -> BTreeMap<(String, String), i64> {
    let mut out = BTreeMap::new();
    for l in &w.dist.allocations[reference].lines {
        let kind = format!("{:?}", l.commission_type.unwrap());
        *out.entry((l.payee.0.clone(), kind)).or_insert(0) += l.amount_minor;
    }
    out
}

fn earned(w: &World, from: usize) -> String {
    w.events[from..]
        .iter()
        .find_map(|e| match e {
            Event::CommissionEarned { reference, .. } => Some(reference.clone()),
            _ => None,
        })
        .unwrap()
}

/// Purpose: a policy is cancelled and five days later an adviser in the same group writes a
/// larger policy on the same life. The rule matches the amount given up: the adviser is told
/// with the reason; the clawback on the old cover is reversed and its exposure carries on
/// under the new layer on its original clock; the new adviser earns initial commission on the
/// increase alone; trail on the matched amount goes to the original book; a later
/// cancellation claws each adviser back for their own business only; and oversight can list
/// the match.
/// Why it matters: this is churn. Without the rule the group is paid initial commission
/// twice on the same cover, and the customer is moved for the adviser's benefit. With a rule
/// that is wrong, an honest adviser loses commission on real new business.
#[test]
fn cover_rewritten_inside_the_group_after_a_cancellation_is_treated_as_a_replacement() {
    let (mut w, old, _) = advised_in(scene(Settings::default()));
    w.days(40);
    let fap_before = w.payable("fap-one");
    cancel(&mut w, &old);
    assert_eq!(w.payable("fap-one"), fap_before - 1_200_00, "clawed back in full inside the first year");
    w.days(5);

    let new = w.issue_for(2, PERSON, 600_000_00);
    let before = w.events.len();
    let events = w.write(&new, "life", 1, "eng-b-two", "b-at-fap-two", 1_440_00).unwrap();
    w.sync();
    assert_eq!(matched(&events), Some((500_000_00, MatchReason::RelatedAdviser)));
    assert!(w.events[before..].iter().any(|e| matches!(e, Event::CommissionReversed { .. })));
    assert_eq!(w.payable("fap-one"), fap_before, "the clawback is undone: the cover was not lost");
    let carried: Vec<_> = w.dist.exposures.values().filter(|e| e.open).collect();
    assert_eq!(carried.iter().map(|e| e.amount_minor).sum::<i64>(), 1_200_00);
    assert!(carried.iter().all(|e| e.layer == layer_of(&new) && e.policy == new));
    let original = w.dist.layers[&layer_of(&old)].start;
    assert!(carried.iter().all(|e| e.layer_start == original), "on its original clock");
    assert_eq!(w.dist.layers[&layer_of(&old)].matched_away_minor, 500_000_00);

    // The first premium: initial on the 100,000 increase only, trail split five to one.
    let before = w.events.len();
    let premium = w.pay(&new, "n1");
    let lines = paid(&w, &earned(&w, before));
    let get = |who: &str, kind: &str| lines.get(&(who.to_string(), kind.to_string())).copied().unwrap_or(0);
    assert_eq!(get("fap-two", "Initial"), 240_00, "one sixth of 1,440.00");
    assert_eq!(get("a-co", "Initial") + get("fap-one", "Initial"), 0);
    let trail_old = get("a-co", "Renewal") + get("fap-one", "Renewal");
    let trail_new = get("fap-two", "Renewal");
    assert!(
        trail_new > 0 && (trail_old - 5 * trail_new).abs() <= 5,
        "{trail_old} to {trail_new} of {premium}"
    );
    w.assert_reconciled();

    let rows = ask(&w.dist, Question::ReplacementMatches).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["layer"], layer_of(&new));
    assert_eq!(
        (rows[0]["reason"].as_str(), rows[0]["matched_minor"].as_i64()),
        (Some("related_adviser"), Some(500_000_00))
    );
    assert_eq!(rows[0]["writing"][0], "eng-b-two");

    // The new policy is cancelled too: each provider bears its own commission.
    w.days(30);
    let before = w.events.len();
    cancel(&mut w, &new);
    let reference = w.events[before..]
        .iter()
        .find_map(|e| match e {
            Event::ClawbackRaised { reference, .. } => Some(reference.clone()),
            _ => None,
        })
        .unwrap();
    let lines = paid(&w, &reference);
    assert_eq!(lines[&("fap-one".to_string(), "Initial".to_string())], -1_200_00);
    assert_eq!(lines[&("fap-two".to_string(), "Initial".to_string())], -240_00);
    w.assert_reconciled();
}

/// Purpose: the rule matches only what it should. The same adviser rewriting cover that was
/// reduced is matched for the amount given up; an unrelated adviser is matched only where the
/// amount is the amount given up, within the tolerance; cover already matched is not matched
/// twice; another person's policy, cover given up in cooling-off (where all commission was
/// reversed), a declared replacement and a rule switched off match nothing.
/// Why it matters: every false match takes initial commission from an adviser who earned it
/// and every miss pays it twice, so the edges of the rule are where the money is.
#[test]
fn the_rule_matches_the_same_life_the_same_cover_and_a_related_adviser_or_the_same_amount() {
    let (mut w, old, _) = advised_in(scene(Settings::default()));
    w.days(40);
    let life = w.cluster.state(0).policies[&old].covers["life"].layers[0].life.clone();
    let reduce = CoreCommand::ReduceCover(Box::new(ic_core::command::ReduceCover {
        policy: old.clone(),
        cover: "life".into(),
        life,
        sum_insured_minor: 300_000_00,
    }));
    w.core(&old, "policy-servicing", reduce).unwrap();

    // Someone else's policy, written by the same adviser: not this life.
    let other = w.issue_for(2, "party-00000002", 200_000_00);
    assert_eq!(matched(&w.write(&other, "life", 1, "eng-a-one", "a-at-fap-one", 480_00).unwrap()), None);
    // An unrelated adviser writes 100,000 on the life: not the 200,000 given up.
    let small = w.issue_for(3, PERSON, 100_000_00);
    assert_eq!(matched(&w.write(&small, "life", 1, "eng-c-three", "c-at-fap-three", 240_00).unwrap()), None);
    // The adviser who reduced it writes 150,000 back: all of it replaces cover given up.
    let back = w.issue_for(4, PERSON, 150_000_00);
    let events = w.write(&back, "life", 1, "eng-a-one", "a-at-fap-one", 360_00).unwrap();
    assert_eq!(matched(&events), Some((150_000_00, MatchReason::SameAdviser)));
    assert!(
        w.dist.layers[&layer_of(&back)].replacement.as_ref().unwrap().continued.is_empty(),
        "a part cut keeps its clawback"
    );
    // 50,000 of the 200,000 is left to match: an unrelated adviser writing 51,000 matches it.
    let rest = w.issue_for(5, PERSON, 51_000_00);
    let events = w.write(&rest, "life", 1, "eng-c-three", "c-at-fap-three", 122_40).unwrap();
    assert_eq!(matched(&events), Some((50_000_00, MatchReason::AmountsMatch)));
    // Nothing is left: the same adviser writing again is new business.
    let again = w.issue_for(6, PERSON, 50_000_00);
    assert_eq!(matched(&w.write(&again, "life", 1, "eng-a-one", "a-at-fap-one", 120_00).unwrap()), None);
    // A fully matched layer earns trail and no initial commission.
    let before = w.events.len();
    w.pay(&back, "b1");
    let lines = paid(&w, &earned(&w, before));
    assert!(lines.keys().all(|(_, kind)| kind == "Renewal"), "{lines:?}");

    // Cancelled in cooling-off, everything was reversed: writing it again is new business.
    let (mut w, old, _) = advised_in(scene(Settings::default()));
    w.days(3);
    cancel(&mut w, &old);
    let new = w.issue_for(2, PERSON, 500_000_00);
    assert_eq!(matched(&w.write(&new, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap()), None);

    // A declared replacement is already one, and a window of zero months turns the rule off.
    for (declared, window) in [(true, 12), (false, 0)] {
        let settings = Settings {
            replacement_window_months: window,
            ..Settings::default()
        };
        let (mut w, old, _) = advised_in(scene(settings));
        w.days(40);
        cancel(&mut w, &old);
        let new = w.issue_for(2, PERSON, 500_000_00);
        let command = Command::WriteLayer(Box::new(ic_distribution::state::WriteLayer {
            policy: new.clone(),
            cover: "life".into(),
            layer: 1,
            writing: vec![(EngagementId::from("eng-a-one"), ic_distribution::rules::WHOLE)],
            books: vec![(BookId::from("a-at-fap-one"), ic_distribution::rules::WHOLE)],
            schedule: "standard".into(),
            option: "upfront".into(),
            annual_premium_minor: 1_200_00,
            replacement: declared,
        }));
        assert_eq!(matched(&w.send(module("new-business"), command).unwrap()), None);
        assert!(w.dist.layers[&layer_of(&new)].replacement.is_none());
    }
}

/// Purpose: the writing adviser disputes a match. From then the initial commission on the
/// matched amount is earned into held under the dispute. Decided as a replacement, it is
/// taken back out of held and nothing is owed. Decided for the adviser, the held money stays
/// to be released, the layer is new business from then on, the clawback on the cover given
/// up is raised again, and the cover given up can be matched to something else. A dispute or
/// a decision that makes no sense is refused.
/// Why it matters: the rule is automatic and will sometimes be wrong. The adviser needs a
/// way to contest it that neither pays disputed money out nor loses it, and the decision has
/// to put every party where they would have been.
#[test]
fn a_disputed_match_holds_the_initial_commission_until_it_is_decided() {
    let dispute = |w: &mut World, policy: &PolicyId, reference: &str| {
        let command = Command::DisputeReplacement {
            policy: policy.clone(),
            cover: "life".into(),
            layer: 1,
            reference: reference.into(),
        };
        w.send(module("new-business"), command)
    };
    let decide = |w: &mut World, policy: &PolicyId, replacement: bool| {
        let command = Command::DecideReplacement {
            policy: policy.clone(),
            cover: "life".into(),
            layer: 1,
            replacement,
        };
        let r = w.send(compliance(), command);
        w.sync();
        r
    };
    // A cancels a policy and B, in the same group, writes the same cover five days later.
    let rewritten = || {
        let (mut w, old, _) = advised_in(scene(Settings::default()));
        w.days(40);
        cancel(&mut w, &old);
        w.days(5);
        let new = w.issue_for(2, PERSON, 500_000_00);
        w.write(&new, "life", 1, "eng-b-two", "b-at-fap-two", 1_200_00).unwrap();
        w.sync();
        (w, old, new)
    };
    let held_in_dispute = |w: &World| -> i64 {
        w.dist
            .held
            .iter()
            .filter(|h| h.reason == Some(HeldReason::Dispute))
            .map(|h| h.remaining_minor)
            .sum()
    };

    // Decided as a replacement.
    let (mut w, old, new) = rewritten();
    assert_eq!(code(dispute(&mut w, &PolicyId("nobody".into()), "d-1")), "unknown_layer");
    assert_eq!(code(dispute(&mut w, &old, "d-1")), "not_matched");
    assert_eq!(code(dispute(&mut w, &new, " ")), "invalid_dispute");
    assert_eq!(code(decide(&mut w, &new, true)), "not_disputed");
    assert_eq!(
        code(w.send(
            module("new-business"),
            Command::DecideReplacement {
                policy: new.clone(),
                cover: "life".into(),
                layer: 1,
                replacement: false
            }
        )),
        "caller_not_allowed"
    );
    let events = dispute(&mut w, &new, "d-1").unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::TaskCreated { kind, .. } if kind == "replacement_disputed")));
    assert_eq!(code(dispute(&mut w, &new, "d-2")), "already_disputed");
    let payable = w.payable("fap-two");
    w.pay(&new, "n1");
    assert_eq!(held_in_dispute(&w), 1_200_00, "the initial commission on the matched amount, held");
    assert_eq!(w.payable("fap-two"), payable, "and none of it is payable");
    w.assert_reconciled();
    decide(&mut w, &new, true).unwrap();
    assert_eq!(w.held("fap-two"), 0);
    assert_eq!(w.dist.exposures.values().filter(|e| e.open && e.paid_to == party("fap-two")).count(), 0);
    let m = w.dist.layers[&layer_of(&new)].replacement.clone().unwrap();
    assert!(m.disputed.is_none() && !m.overturned);
    // It is paid once: later premium earns trail only, to the original book.
    let before = w.events.len();
    w.days(31);
    w.pay(&new, "n2");
    let lines = paid(&w, &earned(&w, before));
    assert!(lines.keys().all(|(who, kind)| kind == "Renewal" && who != "fap-two"), "{lines:?}");
    w.assert_reconciled();

    // Decided for the adviser, after the commission went to held.
    let (mut w, old, new) = rewritten();
    dispute(&mut w, &new, "d-1").unwrap();
    w.pay(&new, "n1");
    let fap_one = w.payable("fap-one");
    decide(&mut w, &new, false).unwrap();
    assert_eq!(held_in_dispute(&w), 1_200_00, "left in held, to be released by the workbench");
    assert_eq!(w.payable("fap-one"), fap_one - 1_200_00, "the cover given up was given up: clawed back again");
    assert_eq!(w.dist.layers[&layer_of(&old)].matched_away_minor, 0);
    assert!(w.dist.layers[&layer_of(&new)].replacement.as_ref().unwrap().overturned);
    assert_eq!(code(dispute(&mut w, &new, "d-3")), "not_matched");
    let before = w.events.len();
    w.days(31);
    w.pay(&new, "n2");
    let lines = paid(&w, &earned(&w, before));
    assert_eq!(lines.keys().cloned().collect::<Vec<_>>(), vec![("fap-two".to_string(), "Renewal".to_string())]);
    w.assert_reconciled();

    // Decided for the adviser before any premium: the first premium pays it in the ordinary way, once.
    let (mut w, _, new) = rewritten();
    dispute(&mut w, &new, "d-1").unwrap();
    decide(&mut w, &new, false).unwrap();
    let before = w.events.len();
    w.pay(&new, "n1");
    let lines = paid(&w, &earned(&w, before));
    assert_eq!(lines[&("fap-two".to_string(), "Initial".to_string())], 1_200_00);
    assert_eq!(held_in_dispute(&w), 0);
    let before = w.events.len();
    w.days(31);
    w.pay(&new, "n2");
    assert!(paid(&w, &earned(&w, before)).keys().all(|(_, kind)| kind == "Renewal"));
    w.assert_reconciled();

    // Decided as a replacement before any premium: there is nothing to take back.
    let (mut w, _, new) = rewritten();
    dispute(&mut w, &new, "d-1").unwrap();
    let before = w.events.len();
    decide(&mut w, &new, true).unwrap();
    assert!(!w.events[before..].iter().any(|e| matches!(e, Event::CommissionReversed { .. })));
}

/// Purpose: a partly matched layer whose own initial commission was already paid is disputed
/// and decided for the adviser between two premiums: the next premium pays the initial
/// commission on the matched amount once, in the ordinary way. And a lapsed policy whose
/// cover was matched to a replacement and is then reinstated is put to a person, not
/// reversed a second time.
/// Why it matters: these are the two orders of events where an automatic rule pays twice or
/// not at all: a decision that lands between premiums, and a reinstatement after the
/// clawback it would reverse has already been reversed.
#[test]
fn a_decision_between_premiums_and_a_reinstatement_after_a_match_pay_nobody_twice() {
    let (mut w, old, _) = advised_in(scene(Settings::default()));
    w.until_status(&old, PolicyStatus::Lapsed);
    let new = w.issue_for(2, PERSON, 600_000_00);
    assert_eq!(
        matched(&w.write(&new, "life", 1, "eng-b-two", "b-at-fap-two", 1_440_00).unwrap()),
        Some((500_000_00, MatchReason::RelatedAdviser))
    );
    w.sync();
    w.pay(&new, "n1");
    let command = Command::DisputeReplacement {
        policy: new.clone(),
        cover: "life".into(),
        layer: 1,
        reference: "d-1".into(),
    };
    w.send(module("new-business"), command).unwrap();
    let command = Command::DecideReplacement {
        policy: new.clone(),
        cover: "life".into(),
        layer: 1,
        replacement: false,
    };
    w.send(compliance(), command).unwrap();
    w.sync();
    let before = w.events.len();
    w.days(31);
    w.pay(&new, "n2");
    let lines = paid(&w, &earned(&w, before));
    assert_eq!(lines[&("fap-two".to_string(), "Initial".to_string())], 1_200_00, "five sixths of 1,440.00");
    let before = w.events.len();
    w.days(31);
    w.pay(&new, "n3");
    assert!(paid(&w, &earned(&w, before)).keys().all(|(_, kind)| kind == "Renewal"));
    w.assert_reconciled();

    // The old policy comes back after its cover was matched again by someone else.
    let third = w.issue_for(3, PERSON, 500_000_00);
    assert!(matched(&w.write(&third, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap()).is_some());
    w.sync();
    let fap_one = w.payable("fap-one");
    let arrears = w.cluster.state(0).policies[&old].lapse.as_ref().unwrap().arrears_minor;
    w.pay_amount(&old, "catch-up", arrears * 2);
    let before = w.events.len();
    w.core(&old, "collections", CoreCommand::Reinstate { policy: old.clone() }).unwrap();
    assert!(w.events[before..]
        .iter()
        .any(|e| matches!(e, Event::TaskCreated { kind, .. } if kind == "reinstated_after_replacement")));
    assert!(!w.events[before..].iter().any(|e| matches!(e, Event::CommissionReversed { .. })));
    assert!(w.payable("fap-one") >= fap_one, "renewal on the arrears, and no second reversal");
    w.assert_reconciled();
}
