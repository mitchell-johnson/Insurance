//! Small things that are easy to get wrong and were not pinned anywhere
//! else: the names states go by, rule conditions that the shipped rules do
//! not happen to use, a change of authority policy through the log, and how
//! long a repeated command gets its first answer in full.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_log::StateMachine;
use ic_underwriting::authority::DEFAULT_POLICIES;
use ic_underwriting::command::Command;
use ic_underwriting::machine::{KEYS_KEPT_DAYS, RESULTS_KEPT};
use ic_underwriting::model::{CoverOutcome, ProposalStatus, Stage};
use ic_underwriting::questions::Answer;
use ic_underwriting::rules::{Action, Condition, Disposition, Rule, RuleInput, RuleSet};

/// Purpose: the name a stage, a cover outcome and a proposal status go by in messages, effect
/// keys and reports is the same word as on the wire, for every variant, and no two share one.
/// Why it matters: these names are read by the portals, by group administration (the outcome
/// of an ended proposal) and by people reading a refusal. A name that drifted from the wire
/// form, or two states with one name, would report a declined proposal as something else.
#[test]
fn every_state_goes_by_the_name_it_has_on_the_wire() {
    let stages = [
        Stage::AwaitingDisclosure,
        Stage::AwaitingEvidence,
        Stage::InAssessment,
        Stage::AwaitingCountersign,
        Stage::AwaitingCapacity,
        Stage::AwaitingFacultative,
        Stage::OfferOpen,
        Stage::Accepted,
        Stage::Declined,
        Stage::OfferDeclined,
    ];
    for stage in stages {
        assert_eq!(serde_json::to_value(stage).unwrap(), stage.key());
        assert_eq!(stage.is_final(), matches!(stage, Stage::Accepted | Stage::Declined | Stage::OfferDeclined));
    }

    let at = chrono::DateTime::parse_from_rfc3339("2027-02-01T09:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let statuses = [
        (ProposalStatus::Open, "open", true),
        (ProposalStatus::Issuing, "issuing", true),
        (ProposalStatus::IssueFailed { code: "x".into() }, "issue_failed", true),
        (ProposalStatus::Issued { at }, "issued", false),
        (ProposalStatus::Lapsed { reason: "r".into(), at }, "lapsed", false),
        (ProposalStatus::Declined { at }, "declined", false),
        (ProposalStatus::Withdrawn { at }, "withdrawn", false),
    ];
    for (status, name, open) in statuses {
        assert_eq!((status.key(), status.is_open()), (name, open));
    }

    let outcomes = [
        (CoverOutcome::standard(), "standard"),
        (
            CoverOutcome::Accept {
                loading_percent: 50,
                exclusions: vec![],
                revised_sum_insured_minor: None,
            },
            "terms",
        ),
        (CoverOutcome::Decline, "decline"),
        (CoverOutcome::Defer { until: d(2028, 1, 1) }, "defer"),
        (CoverOutcome::UnknownTerms, "unknown_terms"),
    ];
    for (outcome, name) in outcomes {
        assert_eq!(outcome.key(), name);
    }
}

/// Purpose: the rule conditions the shipped rule set does not use still mean what they say: a
/// rule that always applies fires for anyone, "not" inverts its condition, and an upper age
/// bound includes the age itself and excludes the year after.
/// Why it matters: an underwriting rule change is activated by people who read the rule, not
/// the evaluator. If "age at most 17" quietly excluded seventeen-year-olds, or "not smoker"
/// meant smoker, the first rule set to use it would decide real applications wrongly.
#[test]
fn conditions_the_shipped_rules_do_not_use_still_mean_what_they_say() {
    let rule = |id: &str, when: Condition| Rule {
        id: id.into(),
        when,
        then: Action::Load { percent: 25 },
        reasons: vec![],
    };
    let rules = RuleSet {
        id: "corner".into(),
        version: 1,
        effective_from: d(2027, 1, 1),
        effective_to: None,
        question_set: "q".into(),
        products: vec!["term-life".into()],
        rules: vec![
            rule("always", Condition::Always),
            rule("minor", Condition::AgeAtMost { years: 17 }),
            rule(
                "not-smoker",
                Condition::Not {
                    cond: Box::new(Condition::Smoker { is: true }),
                },
            ),
        ],
        content_hash: String::new(),
    };
    let answers: BTreeMap<String, Answer> = BTreeMap::new();
    let fired = |age: u32, smoker: bool| {
        rules
            .assess(&RuleInput {
                answers: &answers,
                age,
                smoker: Some(smoker),
                cover_type: "life",
                sum_insured_minor: 1,
                existing_on_life_minor: 0,
            })
            .fired
    };
    assert_eq!(fired(17, true), ["always", "minor"]);
    assert_eq!(fired(18, true), ["always"]);
    assert_eq!(fired(40, false), ["always", "not-smoker"]);
    let loaded = rules.assess(&RuleInput {
        answers: &answers,
        age: 40,
        smoker: Some(true),
        cover_type: "life",
        sum_insured_minor: 1,
        existing_on_life_minor: 0,
    });
    assert_eq!((loaded.loading_percent, loaded.disposition()), (25, Disposition::Terms));
}

/// Purpose: the authority policy is replaced by a command through the log, from the config
/// caller with its approvals; a policy text that does not parse is refused and the policy in
/// force stays.
/// Why it matters: the policy is what stops an underwriter deciding beyond their grade or
/// countersigning their own work. It must change only by an approved, replicated command, and
/// a broken text must never leave the module with no policy at all.
#[test]
fn the_authority_policy_changes_only_by_an_approved_command() {
    let mut w = World::new();
    let policies = |cedar: &str| Command::ActivatePolicies {
        cedar: cedar.into(),
        approval: approval(),
    };
    assert_eq!(w.err("new-business", policies(DEFAULT_POLICIES)), "caller");
    assert_eq!(w.err("config", policies("permit (")), "policies");
    let applied = w.ok("config", policies(DEFAULT_POLICIES));
    assert_eq!(applied.events.len(), 1);
}

/// Purpose: a repeated command is answered with its first result in full while that result is
/// among the most recent kept; once it is older than that, the repeat is still recognised,
/// still changes nothing, and is answered as a duplicate with nothing in it.
/// Why it matters: drivers repeat commands after every lost reply. A repeat that was applied a
/// second time once its result had aged out would open a second proposal or issue a second
/// policy. The limit on kept results bounds memory, and this is what makes that limit safe.
#[test]
fn a_repeat_older_than_the_results_kept_still_changes_nothing() {
    let mut w = World::new();
    let open = Command::OpenProposal(Box::new(w.open_command("P1", &["L1"], &[("L1", "life", "life", 500_000_00)], false)));
    let first = w.send_keyed("first", "new-business", open.clone()).unwrap();
    assert!(!first.duplicate && !first.events.is_empty());

    let again = w.send_keyed("first", "new-business", open.clone()).unwrap();
    assert!(again.duplicate);
    assert_eq!(again.events, first.events);

    for _ in 0..RESULTS_KEPT {
        w.ok("effects-driver", Command::AcknowledgeEffects { through: 0 });
    }
    let before = w.s.clone();
    let late = w.send_keyed("first", "new-business", open).unwrap();
    assert!(late.duplicate && late.events.is_empty() && late.effects.is_empty());
    assert!(w.s == before, "the repeat changed nothing");
}

/// Purpose: keys are forgotten once they are older than the horizon, measured in business days
/// from the day each was first used. A key a day inside the horizon is still a repeat; past it
/// the same key is taken as new. A command forgets at most a hundred old keys, so a backlog is
/// cleared over the commands that follow, and a state restored from JSON forgets the same keys.
/// Why it matters: without a horizon every key ever sent is held for ever, and the module's
/// snapshot grows with every command. Forgetting must be bounded per command so no single
/// command stalls the group, and deterministic so every replica forgets the same keys.
#[test]
fn keys_older_than_the_horizon_are_forgotten_a_bounded_number_at_a_time() {
    let mut w = World::new();
    for _ in 0..250 {
        w.ok("effects-driver", Command::AcknowledgeEffects { through: 0 });
    }
    let open = Command::OpenProposal(Box::new(w.open_command("P1", &["L1"], &[("L1", "life", "life", 500_000_00)], false)));
    w.send_keyed("open-p1", "new-business", open.clone()).unwrap();
    let tick = |w: &mut World, key: &str| w.send_keyed(key, "effects-driver", Command::AcknowledgeEffects { through: 0 }).unwrap();

    w.days(KEYS_KEPT_DAYS);
    tick(&mut w, "tick-1");
    assert!(
        w.send_keyed("open-p1", "new-business", open.clone()).unwrap().duplicate,
        "the last day inside the horizon"
    );

    w.days(1);
    let restored: ic_underwriting::UnderwritingState = serde_json::from_value(serde_json::to_value(&w.s).unwrap()).unwrap();
    tick(&mut w, "tick-2");
    tick(&mut w, "tick-3");
    // Over 250 keys are a day too old: two commands forget a hundred each, and the rest wait.
    let mut replica = restored;
    for key in ["tick-2", "tick-3"] {
        let at = w.log.iter().rposition(|e| e.key == key).unwrap();
        replica.apply(at as u64 + 1, w.log[at].clone()).unwrap();
    }
    assert!(replica == w.s, "a restored replica forgets the same keys");
    assert!(
        w.send_keyed("open-p1", "new-business", Command::Withdraw { proposal: "P1".into() })
            .unwrap()
            .duplicate,
        "two commands have not yet reached the key used last of the old day"
    );
    tick(&mut w, "tick-4");
    let again = w.send_keyed("open-p1", "new-business", open).unwrap_err();
    assert_eq!(
        again.code, "duplicate_proposal",
        "forgotten, the key is new, and the proposal it opened refuses a second"
    );
}
