//! Setting a scheme up and looking after it: registration, terms, the file
//! mapping, freezing, stale files, housekeeping, and what the host asks.

mod common;

use chrono::Days;
use common::*;
use ic_core::{PolicyId, Reject};
use ic_group::command::{caller, Envelope, GroupCommand};
use ic_group::event::{ExceptionKind, GroupEvent, ReinsuranceNotice};
use ic_group::mapping::FileKind;
use ic_group::model::Member;
use ic_group::state::{FileStatus, GroupState};
use ic_group::terms::MemberBasis;
use ic_log::StateMachine;
use serde_json::json;

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn member(id: &str) -> Member {
    Member {
        employee_id: id.to_string(),
        life: life(id),
        attrs: attrs("staff", 60_000_00, "akl"),
        joined: d(2027, 11, 1),
        left: None,
        absence_since: None,
        party: None,
        fingerprint: None,
        auto_cap_minor: None,
        health: None,
    }
}

/// Purpose: a scheme is registered once, against a product that has member-driven cover,
/// with terms that hold together and the members or pools its basis calls for; anything
/// else is refused and leaves nothing behind.
/// Why it matters: everything the module does for a scheme rests on this record. A second
/// registration over a live scheme would wipe its membership; a scheme on a retail product
/// would send membership changes the core can only refuse.
#[test]
fn a_scheme_is_registered_once_and_only_if_it_holds_together() {
    let mut h = Harness::new(terms());
    let register = |h: &mut Harness, scheme: &str, policy: &str, product, terms, founding: Vec<Member>, pools: Vec<String>| {
        h.staff(GroupCommand::RegisterScheme {
            scheme: scheme.into(),
            policy: PolicyId(policy.into()),
            group: h.group.clone(),
            employer: party("employer-2"),
            inception: d(2027, 11, 1),
            product: Box::new(product),
            terms: Box::new(terms),
            founding,
            pools,
        })
    };
    let before = h.g.schemes.clone();
    let existing = h.policy.0.clone();
    let retail = ic_product::load_product("term_life.star", include_str!("../../../products/term_life.star")).unwrap();
    let mut pooled = terms();
    pooled.basis = MemberBasis::Pool;
    let mut broken = terms();
    broken.billing_groups.clear();
    assert_eq!(
        code(register(&mut h, "", "p2", product(), terms(), vec![member("A1")], vec![])),
        "invalid_scheme"
    );
    assert_eq!(
        code(register(&mut h, "a:b", "p2", product(), terms(), vec![member("A1")], vec![])),
        "invalid_scheme"
    );
    assert_eq!(
        code(register(&mut h, SCHEME, "p2", product(), terms(), vec![member("A1")], vec![])),
        "scheme_exists"
    );
    assert_eq!(
        code(register(&mut h, "second", &existing, product(), terms(), vec![member("A1")], vec![])),
        "scheme_exists"
    );
    assert_eq!(
        code(register(&mut h, "second", "p2", product(), broken.clone(), vec![member("A1")], vec![])),
        "invalid_terms"
    );
    assert_eq!(
        code(register(&mut h, "second", "p2", retail, terms(), vec![member("A1")], vec![])),
        "not_a_scheme"
    );
    assert_eq!(
        code(register(&mut h, "second", "p2", product(), terms(), vec![member("A1"), member("A1")], vec![])),
        "duplicate_member"
    );
    assert_eq!(code(register(&mut h, "second", "p2", product(), terms(), vec![], vec![])), "invalid_scheme");
    assert_eq!(
        code(register(&mut h, "second", "p2", product(), terms(), vec![member("A1")], vec!["x".into()])),
        "invalid_scheme"
    );
    assert_eq!(
        code(register(&mut h, "second", "p2", product(), pooled, vec![member("A1")], vec!["x".into()])),
        "invalid_scheme"
    );
    assert_eq!(h.g.schemes, before);
    let only_staff = GroupCommand::Freeze {
        scheme: SCHEME.into(),
        reason: "x".into(),
    };
    assert_eq!(code(h.send_as(caller::EMPLOYER, "admin-bo", only_staff)), "caller_not_allowed");
    assert_eq!(code(h.staff(GroupCommand::Unfreeze { scheme: "nowhere".into() })), "unknown_scheme");

    // Two schemes side by side keep to themselves.
    register(&mut h, "second", "p2", product(), terms(), vec![member("A1")], vec![]).unwrap();
    assert_eq!(h.g.schemes["second"].membership.active_count(), 1);
    assert_eq!(h.scheme().membership.active_count(), 1);
    assert_eq!(ic_group::work::scheme_of(&h.g, &PolicyId("p2".into())).unwrap().id, "second");
    assert!(ic_group::work::scheme_of(&h.g, &PolicyId("p3".into())).is_none());

    // Terms are amended whole, checked, versioned, and reinsurance is told.
    let amend = |h: &mut Harness, t| {
        h.staff(GroupCommand::AmendTerms {
            scheme: SCHEME.into(),
            terms: Box::new(t),
        })
    };
    assert_eq!(code(amend(&mut h, broken)), "invalid_terms");
    let mut wider = terms();
    wider.sites.push(("wlg-depot".into(), 40));
    amend(&mut h, wider.clone()).unwrap();
    assert_eq!((h.scheme().terms_version, &h.scheme().terms), (2, &wider));
    assert!(h.saw(|e| matches!(e, GroupEvent::TermsAmended { terms_version: 2, .. })));
    assert!(matches!(h.notices.last().unwrap(), ReinsuranceNotice::SetSchemeTerms { sites, .. } if sites.len() == 2));

    // A mapping is replaced only by a later, sealed version.
    let set = |h: &mut Harness, m| {
        h.staff(GroupCommand::SetMapping {
            scheme: SCHEME.into(),
            mapping: Box::new(m),
        })
    };
    assert_eq!(code(set(&mut h, mapping(1))), "mapping_version");
    let mut tampered = mapping(2);
    tampered.date_format = "%m/%d/%Y".into();
    assert_eq!(code(set(&mut h, tampered)), "mapping_not_sealed");
    set(&mut h, mapping(2)).unwrap();
    assert_eq!(h.scheme().mapping.as_ref().unwrap().version, 2);
}

/// Purpose: a frozen scheme takes and approves no files until staff unfreeze it, and a
/// file left unfinished longer than the terms allow is raised to staff until it is closed.
/// Why it matters: freezing is the first step of the runbook for a file that removed
/// people in error: it stops the employer's next automatic file making things worse. And
/// a file sitting unapproved is members who have joined the employer but have no cover.
#[test]
fn a_frozen_scheme_takes_no_files_and_a_stale_file_is_raised() {
    let mut h = Harness::new(terms());
    h.upload(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        ],
    )
    .unwrap();
    h.staff(GroupCommand::Freeze {
        scheme: SCHEME.into(),
        reason: "payroll export under review".into(),
    })
    .unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::SchemeFrozen { .. })));
    assert_eq!(code(h.approve("f1")), "scheme_frozen");
    let receive = |file: &str| GroupCommand::ReceiveFile {
        scheme: SCHEME.into(),
        file: file.into(),
        content_hash: format!("hash-{file}"),
    };
    assert_eq!(code(h.employer(receive("f2"))), "scheme_frozen");

    // Fourteen days are allowed; on the fifteenth morning after, it is raised, once.
    h.days(14);
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::FileStale));
    h.days(2);
    assert_eq!(
        h.g.exceptions
            .values()
            .filter(|e| e.kind == ExceptionKind::FileStale && e.subject == "f1")
            .count(),
        1
    );

    h.staff(GroupCommand::Unfreeze { scheme: SCHEME.into() }).unwrap();
    assert_eq!(code(h.staff(GroupCommand::Unfreeze { scheme: SCHEME.into() })), "not_frozen");
    h.approve("f1").unwrap();
    h.resolve("f1", |_| None);
    assert_eq!(h.scheme().files["f1"].status, FileStatus::Applied);
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::FileStale), "closed with the file");
    h.employer(receive("f2")).unwrap();
}

/// Purpose: old idempotency keys and long-closed files are forgotten after the retention
/// period, and nothing still open or recent is.
/// Why it matters: the state is replicated and snapshotted whole, so it must not grow for
/// ever; but forgetting a key too early would let a late duplicate apply twice.
#[test]
fn housekeeping_forgets_only_what_is_old_and_closed() {
    let mut h = Harness::new(terms());
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        ],
    );
    h.upload("f2", FileKind::Full, vec![row(1, "E001", attrs("staff", 60_000_00, "akl"))]).unwrap();
    let old = Envelope {
        key: "old-key".into(),
        caller: caller::STAFF.into(),
        actor: "staff-ana".into(),
        today: h.today(),
        command: GroupCommand::Unfreeze { scheme: SCHEME.into() },
    };
    assert!(!h.g.apply_envelope(old.clone()).duplicate);
    let tick = |g: &mut GroupState, key: &str, today| {
        let env = Envelope {
            key: key.into(),
            caller: caller::DRIVER.into(),
            actor: "driver".into(),
            today,
            command: GroupCommand::Tick,
        };
        assert!(g.apply(0, env).result.is_ok());
    };
    let mut g = h.g.clone();
    tick(&mut g, "t1", h.today() + Days::new(400));
    assert!(g.apply_envelope(old.clone()).duplicate, "inside the retention period");
    assert!(g.schemes[SCHEME].files.contains_key("f1"));
    tick(&mut g, "t2", h.today() + Days::new(401));
    assert_eq!(g.business_date(), Some(h.today() + Days::new(401)));
    assert!(!g.apply_envelope(old).duplicate, "forgotten");
    assert!(!g.schemes[SCHEME].files.contains_key("f1"), "closed and old");
    assert!(g.schemes[SCHEME].files.contains_key("f2"), "still open, however old");
    assert_eq!(GroupState::default().business_date(), None);
}

/// Purpose: the host's two seams behave as the host expects: the core events the module
/// cares about become keyed commands for it and every other event becomes nothing, and
/// questions about the outbox, a scheme, a file, invoices and exceptions are answered from
/// state, with a plain error for what does not exist.
/// Why it matters: the driver, the portals and operations see the module only through
/// these. An event fed twice must be recognised by its key, and a question must never
/// change anything.
#[test]
fn the_host_feeds_events_and_asks_questions() {
    let mut h = Harness::new(terms());
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    h.c.end_of_day().unwrap();
    let records = h.c.take_events();
    let fed: Vec<Envelope> = records.iter().flat_map(ic_group::react).collect();
    assert!(fed.len() < records.len(), "most events are nothing to this module");
    let tick = fed.iter().find(|e| e.command == GroupCommand::Tick).unwrap();
    assert_eq!(tick.today, h.today(), "a date change belongs to the day it opens");
    assert!(fed.iter().all(|e| e.key.starts_with("core:") && e.caller == caller::DRIVER));
    for env in &fed {
        assert!(!h.g.apply_envelope(env.clone()).duplicate);
    }
    let settled = h.g.clone();
    for env in fed {
        assert!(h.g.apply_envelope(env).duplicate);
    }
    assert_eq!(h.g, settled);

    let ask = |h: &Harness, q| ic_group::answer(&h.g, q);
    h.send_as(
        caller::EMPLOYER,
        "admin-bo",
        GroupCommand::ReceiveFile {
            scheme: SCHEME.into(),
            file: "f2".into(),
            content_hash: "h2".into(),
        },
    )
    .unwrap();
    let outbox = ask(&h, json!({"ask": "outbox", "limit": 0})).unwrap();
    assert_eq!((outbox["items"].as_array().unwrap().len(), outbox["more"].as_bool()), (0, Some(true)));
    let outbox = ask(&h, json!({"ask": "outbox", "limit": 50, "after": 0})).unwrap();
    assert_eq!((outbox["items"].as_array().unwrap().len(), outbox["more"].as_bool()), (1, Some(false)));
    let scheme = ask(&h, json!({"ask": "scheme", "scheme": SCHEME})).unwrap();
    assert_eq!(
        (scheme["members"].as_u64(), scheme["open_file"].as_str(), scheme["status"]["status"].as_str()),
        (Some(2), Some("f2"), Some("active"))
    );
    let file = ask(&h, json!({"ask": "file", "scheme": SCHEME, "file": "f1"})).unwrap();
    assert_eq!((file["status"].as_str(), file["applied"].as_u64()), (Some("applied"), Some(1)));
    assert!(ask(&h, json!({"ask": "invoices", "scheme": SCHEME})).unwrap()["invoices"].is_array());
    h.staff(GroupCommand::Freeze {
        scheme: SCHEME.into(),
        reason: "x".into(),
    })
    .unwrap();
    h.days(16);
    let all = ask(&h, json!({"ask": "exceptions", "scheme": null})).unwrap();
    assert_eq!(all["exceptions"][0]["kind"].as_str(), Some("file_stale"));
    let other = ask(&h, json!({"ask": "exceptions", "scheme": "another"})).unwrap();
    assert!(other["exceptions"].as_array().unwrap().is_empty());
    assert!(ask(&h, json!({"ask": "scheme", "scheme": "nowhere"})).unwrap_err().contains("not registered"));
    assert!(ask(&h, json!({"ask": "file", "scheme": SCHEME, "file": "f9"}))
        .unwrap_err()
        .contains("not known"));
    assert!(ask(&h, json!({"ask": "weather"})).unwrap_err().contains("not a question"));
}

/// Purpose: the module as the node hosts it takes a command as JSON, answers a question
/// as JSON, is fed the core's events, and comes back from its own snapshot unchanged.
/// Why it matters: this is the form in which the module actually runs. A command type
/// that does not survive JSON, or a snapshot that loses a field, would pass every other
/// test here and fail on the first restart of a deployed node.
#[test]
fn the_module_runs_under_the_node_host() {
    let spec = ic_group::module_spec().unwrap();
    assert_eq!(spec.name, ic_group::MODULE);
    let h = Harness::new(terms());
    // Replay the harness's first commands through the hosted form.
    let mut hosted = (spec.make)();
    let register = Envelope {
        key: "k1".into(),
        caller: caller::STAFF.into(),
        actor: "staff-ana".into(),
        today: h.today(),
        command: GroupCommand::RegisterScheme {
            scheme: SCHEME.into(),
            policy: h.policy.clone(),
            group: h.group.clone(),
            employer: party("employer-1"),
            inception: d(2027, 11, 1),
            product: Box::new(product()),
            terms: Box::new(terms()),
            founding: vec![member("E001")],
            pools: vec![],
        },
    };
    let out = hosted.apply(1, serde_json::to_value(&register).unwrap()).unwrap();
    assert_eq!(out["duplicate"].as_bool(), Some(false));
    assert!(spec.events, "the module is fed the core's events");
    let mut c = ic_sim::Cluster::in_memory(1, 3);
    c.open(h.today()).unwrap();
    c.end_of_day().unwrap();
    for (i, record) in c.take_events().iter().enumerate() {
        hosted.on_event(2 + i as u64, record);
    }
    let answer = hosted.query(json!({"ask": "scheme", "scheme": SCHEME})).unwrap();
    assert_eq!(answer["members"].as_u64(), Some(1));
    assert!(hosted.apply(9, json!({"not": "a command"})).is_err());
    let saved = hosted.save().unwrap();
    let mut restored = (spec.make)();
    restored.load(saved.clone()).unwrap();
    assert_eq!(restored.query(json!({"ask": "scheme", "scheme": SCHEME})).unwrap(), answer);
    assert_eq!(restored.save().unwrap(), saved);
}

/// Purpose: a member chooses a plan and tier from the scheme's health menu, from a date no
/// further back than the menu allows; a plan or tier not on the menu, a scheme with no menu
/// and a menu naming a cover the scheme's product lacks are refused; and the employer's view
/// shows the plan and tier (the default plan, single, until the member chooses) and nothing
/// of who the dependants are.
/// Why it matters: Mitchell's decision of 14 Nov 2026. The tier sets the premium the employer
/// pays, so the employer must see it; who a member's partner or children are is not the
/// employer's business.
#[test]
fn a_member_chooses_a_health_plan_from_the_schemes_menu() {
    use ic_group::terms::{HealthMenu, HealthPlan, HealthTier, MenuCover};
    let mut h = Harness::new(terms());
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        ],
    );
    let today = h.today();
    let choose = |h: &mut Harness, caller: &str, plan: &str, tier: HealthTier, effective: chrono::NaiveDate| {
        let r = h.send_as(
            caller,
            "someone",
            GroupCommand::ChooseHealthPlan {
                scheme: SCHEME.into(),
                employee_id: "E002".into(),
                plan: plan.into(),
                tier,
                effective,
            },
        );
        h.settle();
        r
    };
    assert_eq!(code(choose(&mut h, caller::MEMBER, "standard", HealthTier::Single, today)), "no_menu");

    let cover = |c: &str| MenuCover {
        cover: c.into(),
        excess_option: None,
    };
    let menu = HealthMenu {
        plans: vec![HealthPlan {
            code: "standard".into(),
            name: "Standard".into(),
            covers: vec![cover("group-life")],
        }],
        default_plan: "standard".into(),
        tiers: vec![HealthTier::Single, HealthTier::Family],
        choice_backdating_days: 30,
    };
    let amend = |h: &mut Harness, menu: HealthMenu| {
        let mut t = h.scheme().terms.clone();
        t.health_menu = Some(menu);
        h.staff(GroupCommand::AmendTerms {
            scheme: SCHEME.into(),
            terms: Box::new(t),
        })
    };
    let mut lacking = menu.clone();
    lacking.plans[0].covers = vec![cover("hospital")];
    assert_eq!(code(amend(&mut h, lacking)), "invalid_terms");
    amend(&mut h, menu).unwrap();

    let view = |h: &Harness| ic_group::answer(&h.g, json!({"ask": "member", "scheme": SCHEME, "employee": "E002"})).unwrap()["health"].clone();
    assert_eq!(view(&h), json!({"plan": "standard", "tier": "single", "since": null}));
    assert_eq!(code(choose(&mut h, caller::MEMBER, "gold", HealthTier::Single, today)), "unknown_plan");
    assert_eq!(code(choose(&mut h, caller::MEMBER, "standard", HealthTier::Couple, today)), "tier_not_offered");
    let long_ago = today.checked_sub_days(Days::new(31)).unwrap();
    assert_eq!(
        code(choose(&mut h, caller::MEMBER, "standard", HealthTier::Family, long_ago)),
        "backdating_limit"
    );
    assert_eq!(
        code(choose(&mut h, caller::REINSURANCE, "standard", HealthTier::Family, today)),
        "caller_not_allowed"
    );
    choose(&mut h, caller::EMPLOYER, "standard", HealthTier::Family, today).unwrap();
    assert_eq!(view(&h), json!({"plan": "standard", "tier": "family", "since": today}));
    assert!(h.saw(|e| matches!(e, GroupEvent::HealthPlanChosen { tier: HealthTier::Family, .. })));
}
