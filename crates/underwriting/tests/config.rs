//! Activation of configuration: versions, approvals and what each artefact
//! must agree with before it can be used.

use chrono::NaiveDate;
use ic_underwriting::authority::DEFAULT_POLICIES;
use ic_underwriting::command::Approval;
use ic_underwriting::config::{Config, DelegatedAuthority, InterimSettings, Settings};
use ic_underwriting::model::VersionRef;
use ic_underwriting::questions::DisclosureRegime;
use ic_underwriting::shipped;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn approval() -> Approval {
    Approval {
        author: "ann".into(),
        approvers: vec!["bob".into(), "cy".into()],
        change_ref: "CHG-1".into(),
    }
}

fn code(r: Result<impl std::fmt::Debug, ic_underwriting::command::Reject>) -> String {
    r.unwrap_err().code
}

fn loaded() -> Config {
    let s = shipped::load().unwrap();
    let mut c = Config::default();
    let today = d(2026, 11, 11);
    c.activate_exclusions(s.exclusions, approval(), today).unwrap();
    c.activate_reasons(s.reasons, approval(), today).unwrap();
    for q in s.question_sets {
        c.activate_question_set(q, approval(), today).unwrap();
    }
    for r in s.rule_sets {
        c.activate_rule_set(r, None, approval(), today).unwrap();
    }
    c
}

fn delegated() -> DelegatedAuthority {
    DelegatedAuthority {
        id: "acme-scheme".into(),
        version: 1,
        delegate: "acme-tpa".into(),
        products: ["group-life".to_string()].into(),
        per_life_minor: [("group_life".to_string(), 500_000_00)].into(),
        aggregate_minor: 50_000_000_00,
        max_loading_percent: 50,
        may_exclude: true,
        may_decline: true,
        rule_set: None,
        effective_from: d(2026, 1, 1),
        effective_to: Some(d(2027, 1, 1)),
        reinsurer_agreement_ref: Some("RE-AGR-7".into()),
    }
}

/// Purpose: a configuration change needs an author, two other distinct approvers and a change
/// record. The author approving their own change, one approver, the same approver twice, an
/// empty name or no change record are all refused.
/// Why it matters: rules, question sets and authority limits decide who is insured and on what
/// terms; the design requires dual approval on activation so no one person can change them.
#[test]
fn activation_needs_two_other_approvers_and_a_change_record() {
    let s = shipped::load().unwrap();
    let today = d(2026, 11, 11);
    let try_with = |a: Approval| code(Config::default().activate_exclusions(s.exclusions.clone(), a, today));
    assert_eq!(
        try_with(Approval {
            approvers: vec!["bob".into()],
            ..approval()
        }),
        "approval"
    );
    assert_eq!(
        try_with(Approval {
            approvers: vec!["bob".into(), "ann".into()],
            ..approval()
        }),
        "approval"
    );
    assert_eq!(
        try_with(Approval {
            approvers: vec!["bob".into(), "bob".into()],
            ..approval()
        }),
        "approval"
    );
    assert_eq!(
        try_with(Approval {
            approvers: vec!["bob".into(), "".into()],
            ..approval()
        }),
        "approval"
    );
    assert_eq!(
        try_with(Approval {
            author: "".into(),
            ..approval()
        }),
        "approval"
    );
    assert_eq!(
        try_with(Approval {
            change_ref: "".into(),
            ..approval()
        }),
        "approval"
    );
    let mut c = Config::default();
    let a = c.activate_exclusions(s.exclusions.clone(), approval(), today).unwrap();
    assert_eq!(
        (a.kind.as_str(), a.version, a.on, a.approval.change_ref.as_str()),
        ("exclusions", 1, today, "CHG-1")
    );
    assert_eq!(c.activations.len(), 1);
}

/// Purpose: code lists move forward only: a version must follow the active one, cannot take
/// effect earlier, and cannot drop a code that is on file; the reason list must keep the codes
/// the module itself gives; the list in force is chosen by date.
/// Why it matters: an exclusion code sits on policies for decades. Removing a code, or
/// reusing a version number, would leave in-force exclusions that nothing can interpret.
#[test]
fn code_lists_only_move_forward_and_never_lose_a_code() {
    let s = shipped::load().unwrap();
    let today = d(2026, 11, 11);
    let mut c = loaded();
    assert_eq!(code(c.activate_exclusions(s.exclusions.clone(), approval(), today)), "stale_version");
    assert_eq!(code(c.activate_reasons(s.reasons.clone(), approval(), today)), "stale_version");

    let mut next = s.exclusions.clone();
    next.version = 2;
    next.effective_from = d(2025, 1, 1);
    assert_eq!(code(c.activate_exclusions(next.clone(), approval(), today)), "effective_date");
    next.effective_from = d(2027, 1, 1);
    let mut dropped = next.clone();
    dropped.entries.remove("EX-1001");
    assert_eq!(code(c.activate_exclusions(dropped, approval(), today)), "code_removed");
    let mut odd = next.clone();
    odd.entries.get_mut("EX-1001").unwrap().applies_to.insert("pets".into());
    assert_eq!(code(c.activate_exclusions(odd, approval(), today)), "cover_type");
    c.activate_exclusions(next, approval(), today).unwrap();
    assert_eq!(c.exclusions_on(d(2026, 12, 31)).unwrap().version, 1);
    assert_eq!(c.exclusions_on(d(2027, 1, 1)).unwrap().version, 2);
    assert!(c.exclusions_on(d(2025, 1, 1)).is_none());
    assert_eq!(c.exclusions_ref(d(2027, 1, 1)).unwrap().version, 2);

    let mut reasons = s.reasons.clone();
    reasons.version = 2;
    reasons.effective_from = d(2025, 1, 1);
    assert_eq!(code(c.activate_reasons(reasons.clone(), approval(), today)), "effective_date");
    reasons.effective_from = d(2027, 1, 1);
    let mut dropped = reasons.clone();
    dropped.entries.remove("RC-0101");
    assert_eq!(code(c.activate_reasons(dropped, approval(), today)), "code_removed");
    c.activate_reasons(reasons, approval(), today).unwrap();
    assert_eq!(c.reasons_ref(d(2027, 6, 1)).unwrap().version, 2);
    let mut first = s.reasons.clone();
    first.entries.remove("RC-0910");
    assert_eq!(code(Config::default().activate_reasons(first, approval(), today)), "code_removed");
}

/// Purpose: a question set must sit wholly under the legal basis of its dates, and the set and
/// rules used for a contract are chosen by the contract date: duty of disclosure before
/// 15 November 2027, reasonable care from then.
/// Why it matters: the Contracts of Insurance Act 2024 changes what an applicant must tell the
/// insurer and the remedies for getting it wrong. Both regimes run side by side for years, and a
/// contract judged under the wrong one is a legal failure.
#[test]
fn question_sets_follow_the_disclosure_regime_of_the_contract_date() {
    let s = shipped::load().unwrap();
    let today = d(2026, 11, 11);
    let c = loaded();
    assert_eq!(c.question_set_on("personal-statement", d(2027, 11, 14)).unwrap().version, 1);
    assert_eq!(c.question_set_on("personal-statement", d(2027, 11, 15)).unwrap().version, 2);
    assert!(c.question_set_on("personal-statement", d(2025, 1, 1)).is_none());
    assert!(c.question_set_on("other", today).is_none());
    assert_eq!(c.regime_on(d(2027, 11, 14)), DisclosureRegime::DutyOfDisclosure);
    assert_eq!(c.regime_on(d(2027, 11, 15)), DisclosureRegime::ReasonableCare);
    let known = VersionRef {
        id: "personal-statement".into(),
        version: 2,
        content_hash: String::new(),
    };
    assert_eq!(c.question_set(&known).unwrap().regime, DisclosureRegime::ReasonableCare);
    assert!(c.question_set(&VersionRef { version: 9, ..known }).is_none());

    let mut c = loaded();
    let mut wrong = s.question_sets[0].clone();
    wrong.version = 3;
    wrong.effective_to = Some(d(2028, 1, 1));
    assert_eq!(
        code(c.activate_question_set(wrong.clone(), approval(), today)),
        "regime",
        "runs past the change of law"
    );
    wrong.effective_from = d(2027, 12, 1);
    assert_eq!(
        code(c.activate_question_set(wrong.clone(), approval(), today)),
        "regime",
        "starts under the new law"
    );
    wrong.version = 2;
    assert_eq!(code(c.activate_question_set(wrong, approval(), today)), "stale_version");
}

/// Purpose: a rule set is activated only when its question set and code lists are active and
/// agree with it, and a change to an existing rule set must carry the digest of its backtest.
/// Why it matters: rules change the straight-through rate and who is declined. The design
/// requires a changed rule set to be tested against history before activation, with the evidence
/// tied to the approval.
#[test]
fn a_changed_rule_set_needs_its_backtest_and_its_references() {
    let s = shipped::load().unwrap();
    let today = d(2026, 11, 11);
    let rules = s.rule_sets[0].clone();
    assert_eq!(code(Config::default().activate_rule_set(rules.clone(), None, approval(), today)), "references");

    let mut c = loaded();
    assert_eq!(c.rule_set_on("term-life", today).unwrap().version, 1);
    assert!(c.rule_set_on("funeral", today).is_none());
    assert!(c.rule_set_on("term-life", d(2025, 1, 1)).is_none());
    let mut next = rules.clone();
    next.version = 2;
    assert_eq!(code(c.activate_rule_set(next.clone(), None, approval(), today)), "backtest");
    assert_eq!(code(c.activate_rule_set(next.clone(), Some(String::new()), approval(), today)), "backtest");
    assert_eq!(code(c.activate_rule_set(rules, Some("abc".into()), approval(), today)), "stale_version");
    let mut broken = next.clone();
    broken.rules[0].reasons = vec!["RC-7777".into()];
    assert_eq!(code(c.activate_rule_set(broken, Some("abc".into()), approval(), today)), "references");
    let a = c.activate_rule_set(next, Some("abc".into()), approval(), today).unwrap();
    assert_eq!(a.backtest_digest.as_deref(), Some("abc"));
    assert_eq!(c.rule_set_on("term-life", today).unwrap().version, 2);
}

/// Purpose: profiles, settings, Cedar policy and delegated authorities are versioned and
/// checked: nobody authors or approves their own authority, settings must be in range, policy
/// must parse, and a delegated authority's per-life limits must fit inside its aggregate and its
/// dates must run forwards. A delegated authority applies only between its dates, and narrowing
/// it is a new version from a date.
/// Why it matters: each of these is a control on who can bind the insurer. A settings change
/// that turned off countersigning, or a policy file that failed to parse and so denied or
/// allowed everything, must be stopped at activation.
#[test]
fn authority_settings_and_policies_are_checked_at_activation() {
    let s = shipped::load().unwrap();
    let today = d(2026, 11, 11);
    let mut c = Config::default();
    let mut amy = s.grades[1].clone();
    amy.underwriter = "amy".into();
    assert_eq!(
        code(c.activate_profile(
            amy.clone(),
            Approval {
                author: "amy".into(),
                ..approval()
            },
            today
        )),
        "approval"
    );
    assert_eq!(
        code(c.activate_profile(
            amy.clone(),
            Approval {
                approvers: vec!["amy".into(), "bob".into()],
                ..approval()
            },
            today
        )),
        "approval"
    );
    c.activate_profile(amy.clone(), approval(), today).unwrap();
    assert_eq!(code(c.activate_profile(amy.clone(), approval(), today)), "stale_version");
    amy.version = 2;
    amy.may_decline = true;
    c.activate_profile(amy, approval(), today).unwrap();
    assert!(c.profiles["amy"].may_decline);

    assert_eq!(code(c.activate_settings(Settings::default(), approval(), today)), "stale_version");
    let v2 = || Settings {
        version: 2,
        ..Settings::default()
    };
    let bad: Vec<Settings> = vec![
        Settings { proposal_days: 0, ..v2() },
        Settings { offer_days: 0, ..v2() },
        Settings { evidence_days: 0, ..v2() },
        Settings { chase_every_days: 0, ..v2() },
        Settings {
            straight_through_limit_minor: -1,
            ..v2()
        },
        Settings {
            countersign_above_minor: -1,
            ..v2()
        },
        Settings {
            delegated_tolerance_days: -1,
            ..v2()
        },
        Settings {
            timers: ic_underwriting::case::Timers {
                first_touch_hours: 0,
                ..Default::default()
            },
            ..v2()
        },
        Settings {
            timers: ic_underwriting::case::Timers {
                manager_hours: 1,
                ..Default::default()
            },
            ..v2()
        },
        Settings {
            timers: ic_underwriting::case::Timers {
                work_in_progress_limit: 0,
                ..Default::default()
            },
            ..v2()
        },
        Settings {
            sample: ic_underwriting::config::SampleRates {
                standard_per_mille: 1001,
                non_standard_per_mille: 0,
                decline_per_mille: 0,
            },
            ..v2()
        },
        Settings {
            interim: Some(InterimSettings {
                product_code: "".into(),
                cover_code: "ad".into(),
                max_sum_insured_minor: 1,
            }),
            ..v2()
        },
        Settings {
            interim: Some(InterimSettings {
                product_code: "p".into(),
                cover_code: "".into(),
                max_sum_insured_minor: 1,
            }),
            ..v2()
        },
        Settings {
            interim: Some(InterimSettings {
                product_code: "p".into(),
                cover_code: "ad".into(),
                max_sum_insured_minor: 0,
            }),
            ..v2()
        },
    ];
    for s in bad {
        assert_eq!(code(c.activate_settings(s, approval(), today)), "settings");
    }
    c.activate_settings(Settings { offer_days: 14, ..v2() }, approval(), today).unwrap();
    assert_eq!(c.settings.offer_days, 14);

    assert_eq!(code(c.activate_policies("permit (".into(), approval(), today)), "policies");
    let a = c.activate_policies(DEFAULT_POLICIES.to_string(), approval(), today).unwrap();
    assert_eq!((a.version, a.content_hash.len()), (2, 64));
    assert!(c.authority().is_ok());
    c.cedar = "permit (".into();
    assert_eq!(c.authority().err().unwrap().code, "policies");

    for bad in [
        DelegatedAuthority {
            delegate: "".into(),
            ..delegated()
        },
        DelegatedAuthority {
            aggregate_minor: -1,
            ..delegated()
        },
        DelegatedAuthority {
            aggregate_minor: 1,
            ..delegated()
        },
        DelegatedAuthority {
            per_life_minor: [("pets".to_string(), 1)].into(),
            ..delegated()
        },
        DelegatedAuthority {
            per_life_minor: [("life".to_string(), -1)].into(),
            ..delegated()
        },
        DelegatedAuthority {
            effective_to: Some(d(2026, 1, 1)),
            ..delegated()
        },
    ] {
        assert_eq!(code(c.activate_delegated(bad, approval(), today)), "delegated_authority");
    }
    c.activate_delegated(delegated(), approval(), today).unwrap();
    assert_eq!(code(c.activate_delegated(delegated(), approval(), today)), "stale_version");
    assert!(c.delegated_on("acme-scheme", d(2026, 6, 1)).is_some());
    assert!(c.delegated_on("acme-scheme", d(2027, 1, 1)).is_none(), "ended");
    assert!(c.delegated_on("acme-scheme", d(2025, 12, 31)).is_none(), "not started");
    assert!(c.delegated_on("other", d(2026, 6, 1)).is_none());
    // Suspending is a new version from a date: decisions before it stand under version 1.
    c.activate_delegated(
        DelegatedAuthority {
            version: 2,
            effective_from: d(2026, 7, 1),
            effective_to: Some(d(2026, 7, 2)),
            ..delegated()
        },
        approval(),
        today,
    )
    .unwrap();
    assert_eq!(c.delegated_on("acme-scheme", d(2026, 6, 30)).unwrap().version, 1);
    assert_eq!(c.delegated_on("acme-scheme", d(2026, 7, 1)).unwrap().version, 2);
    assert!(c.delegated_on("acme-scheme", d(2026, 8, 1)).is_none(), "the newest version by then has ended");
}
