//! Staff authorisation: templates, profiles, grants, the four questions,
//! and the seam to an external entitlement source.

use std::collections::{BTreeMap, BTreeSet};

use ic_channels::classes::DataClass;
use ic_channels::staff::*;
use ic_core::PartyId;

const DAY: At = 86_400;
const T0: At = 1_800_000_000;

fn p(id: &str) -> PartyId {
    PartyId::from(id)
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn limits(items: &[(&str, i64)]) -> BTreeMap<String, i64> {
    items.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn template(id: &str, acts: &[&str], classes: &[DataClass], profile: &str, ceilings: &[(&str, i64)]) -> Template {
    Template {
        id: id.to_string(),
        actions: set(acts),
        classes: classes.iter().copied().collect(),
        default_profile: profile.to_string(),
        ceilings: limits(ceilings),
        break_glass: false,
    }
}

/// An insurer's staff: an administrator, a security approver, a claims
/// manager, two assessors and a collections officer.
fn staffed() -> StaffState {
    let mut s = StaffState::new();
    let admin = template(
        "administrator",
        &[actions::MANAGE_STAFF, actions::CONFIGURE, actions::APPROVE_GRANT, actions::REVIEW_BREAK_GLASS],
        &[DataClass::Ordinary],
        "none",
        &[],
    );
    let security = template(
        "security",
        &[actions::APPROVE_CONFIGURATION, actions::APPROVE_PROFILE_UPLIFT, actions::APPROVE_GRANT],
        &[DataClass::Ordinary],
        "none",
        &[],
    );
    let assessor = template(
        "claims-assessor",
        &["claim.view", "claim.admit", "claim.approve_payment", "claim.view_clinical"],
        &[
            DataClass::Ordinary,
            DataClass::Personal,
            DataClass::Financial,
            DataClass::InsuranceSensitive,
            DataClass::Clinical,
        ],
        "assessor-1",
        &[("admit", 500_000_00)],
    );
    let collections = template(
        "collections-officer",
        &["policy.view", "suspense.allocate"],
        &[DataClass::Ordinary, DataClass::Financial],
        "collections-1",
        &[("suspense", 10_000_00)],
    );
    let mut senior = template(
        "senior-claims",
        &["claim.view", "claim.admit", "claim.decline"],
        &[DataClass::Ordinary, DataClass::Personal, DataClass::Clinical],
        "assessor-1",
        &[("admit", 2_000_000_00)],
    );
    senior.break_glass = true;
    s.apply(
        &p("module:entitlements"),
        T0,
        StaffCommand::Bootstrap {
            templates: vec![admin, security, assessor, collections, senior],
            profiles: vec![
                Profile {
                    id: "none".into(),
                    version: 1,
                    limits: BTreeMap::new(),
                },
                Profile {
                    id: "assessor-1".into(),
                    version: 1,
                    limits: limits(&[("admit", 100_000_00)]),
                },
                Profile {
                    id: "collections-1".into(),
                    version: 1,
                    limits: limits(&[("suspense", 1_000_00)]),
                },
            ],
            admin: p("alice"),
            admin_templates: vec!["administrator".into()],
        },
    )
    .unwrap();
    for (who, team, manager, templates) in [
        ("sam", "security", None, vec!["security"]),
        ("mia", "claims", None, vec!["administrator", "senior-claims"]),
        ("ana", "claims", Some("mia"), vec!["claims-assessor"]),
        ("ben", "claims", Some("mia"), vec!["claims-assessor"]),
        ("col", "collections", Some("mia"), vec!["collections-officer"]),
    ] {
        s.apply(
            &p("alice"),
            T0,
            StaffCommand::Employ {
                party: p(who),
                team: team.into(),
                manager: manager.map(p),
                templates: templates.into_iter().map(String::from).collect(),
            },
        )
        .unwrap();
    }
    for (who, scope) in [("ana", "queue:claims-life"), ("ben", "queue:claims-health"), ("col", "queue:collections")] {
        s.apply(
            &p("alice"),
            T0,
            StaffCommand::AssignPath {
                party: p(who),
                scope: scope.into(),
            },
        )
        .unwrap();
    }
    s
}

fn request(staff: &str, action: &str, class: DataClass) -> AccessRequest {
    AccessRequest {
        staff: p(staff),
        action: action.into(),
        class,
        scopes: vec![],
        record: None,
        amount: None,
        purpose: None,
        decides: false,
        conflict_subject: None,
        maker: None,
        fresh: true,
        at: T0 + 60,
    }
}

fn case(reference: &str) -> Option<Purpose> {
    Some(Purpose {
        kind: PurposeKind::Case,
        reference: reference.into(),
    })
}

fn policy() -> StaffPolicy {
    StaffPolicy::shipped().unwrap()
}

fn code<T: std::fmt::Debug>(r: Result<T, ic_core::Reject>) -> String {
    r.unwrap_err().code
}

/// Purpose: each of the four questions (what, whose, how much, why) denies on its own when it is
/// the one that fails, and the action is allowed when all four are answered.
/// Why it matters: a role alone must never be enough. An assessor may admit claims, but only on a
/// claim in their queue, only up to their authority, and only with the case named. Each denial
/// reason is what tells the person which gap to take to their manager.
#[test]
fn the_four_questions_each_deny_on_their_own() {
    let s = staffed();
    let pol = policy();
    let mut ok = request("ana", "claim.admit", DataClass::Financial);
    ok.scopes = vec!["queue:claims-life".into()];
    ok.record = Some("claim-1".into());
    ok.amount = Some(Amount {
        limit: "admit".into(),
        value: 80_000_00,
    });
    ok.purpose = case("claim-1");
    ok.decides = true;
    let d = s.authorise(&pol, &ok);
    assert!(d.allowed, "{d:?}");
    assert_eq!(d.template.as_deref(), Some("claims-assessor"));
    assert_eq!(d.profile, Some(("assessor-1".to_string(), 1)));
    assert_eq!(d.grant, None);
    assert!(d.classes.contains(&DataClass::Clinical));

    let mut what = ok.clone();
    what.action = "claim.decline".into();
    assert_eq!(s.authorise(&pol, &what).denied, Some(Denial::What));

    let mut whose = ok.clone();
    whose.scopes = vec!["queue:claims-health".into()];
    assert_eq!(s.authorise(&pol, &whose).denied, Some(Denial::Whose));

    let mut how_much = ok.clone();
    how_much.amount = Some(Amount {
        limit: "admit".into(),
        value: 100_000_01,
    });
    assert_eq!(s.authorise(&pol, &how_much).denied, Some(Denial::HowMuch));

    let mut why = ok.clone();
    why.purpose = None;
    assert_eq!(s.authorise(&pol, &why).denied, Some(Denial::Why));
    why.purpose = Some(Purpose {
        kind: PurposeKind::Case,
        reference: "  ".into(),
    });
    assert_eq!(s.authorise(&pol, &why).denied, Some(Denial::Why));

    let stranger = request("nobody", "claim.view", DataClass::Ordinary);
    let d = s.authorise(&pol, &stranger);
    assert_eq!(d.denied, Some(Denial::NotStaff));
    assert!(d.classes.is_empty());
    for denial in [
        Denial::NotStaff,
        Denial::What,
        Denial::Whose,
        Denial::HowMuch,
        Denial::Why,
        Denial::Class,
        Denial::Conflict,
        Denial::MakerChecker,
        Denial::Stale,
        Denial::Policy,
    ] {
        assert!(!denial.explain().is_empty());
    }
}

/// Purpose: data classes limit what a role sees whatever the action, clinical information is
/// reached only through a task on one record (never an ad hoc search or a bulk export), and stale
/// entitlement data blocks decisions and sensitive reads while still allowing an ordinary read.
/// Why it matters: a collections officer must never reach a loading or a diagnosis, and an
/// assessor must not browse clinical files by ad hoc lookup. If the platform cannot confirm a
/// person's entitlements are current, the safe failure is to refuse what could do harm.
#[test]
fn classes_clinical_purpose_and_freshness() {
    let s = staffed();
    let pol = policy();
    let mut col = request("col", "policy.view", DataClass::InsuranceSensitive);
    col.scopes = vec!["queue:collections".into()];
    col.purpose = case("case-9");
    assert_eq!(s.authorise(&pol, &col).denied, Some(Denial::Class));
    col.class = DataClass::Financial;
    assert!(s.authorise(&pol, &col).allowed);

    let mut clinical = request("ana", "claim.view_clinical", DataClass::Clinical);
    clinical.scopes = vec!["queue:claims-life".into()];
    clinical.purpose = Some(Purpose {
        kind: PurposeKind::Reason,
        reference: "curious".into(),
    });
    assert_eq!(s.authorise(&pol, &clinical).denied, Some(Denial::Why));
    clinical.purpose = Some(Purpose {
        kind: PurposeKind::Task,
        reference: "task-4".into(),
    });
    // A search or a bulk export names no one record: refused even under a task.
    assert_eq!(s.authorise(&pol, &clinical).denied, Some(Denial::Why));
    clinical.record = Some("claim-1".into());
    assert!(s.authorise(&pol, &clinical).allowed);
    clinical.fresh = false;
    assert_eq!(s.authorise(&pol, &clinical).denied, Some(Denial::Stale));

    let mut ordinary = request("ana", "claim.view", DataClass::Ordinary);
    ordinary.fresh = false;
    assert!(s.authorise(&pol, &ordinary).allowed);
    ordinary.decides = true;
    ordinary.purpose = case("c");
    assert_eq!(s.authorise(&pol, &ordinary).denied, Some(Denial::Stale));
}

/// Purpose: a policy that refuses for a reason the facts do not name is reported as a policy
/// denial, a policy that does not parse is an error, and an action name Cedar cannot hold is denied.
/// Why it matters: operators tighten Cedar policy without a code change. Whatever they write, the
/// outcome must be a clean denial, never an allow by accident or a crash.
#[test]
fn policy_is_the_final_word() {
    let s = staffed();
    let strict = StaffPolicy::new("forbid (principal, action, resource);").unwrap();
    let d = s.authorise(&strict, &request("ana", "claim.view", DataClass::Ordinary));
    assert_eq!(d.denied, Some(Denial::Policy));
    assert!(StaffPolicy::new("permit (").is_err());
    assert!(StaffPolicy::new("permit (").unwrap_err().to_string().contains("do not parse"));
}

/// Purpose: maker-checker and declared separation of duties are refused whatever the role, and an
/// act is remembered only when it was allowed and decided something.
/// Why it matters: the person who prepares a payment approving it, or the person who underwrote a
/// life assessing its claim, are the frauds and errors these controls exist to stop. They have to
/// hold by policy, not by people remembering.
#[test]
fn maker_checker_and_separation_of_duties() {
    let mut s = staffed();
    let pol = policy();
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::DeclareConflict {
            conflict: Conflict {
                a: "claim.admit".into(),
                b: "claim.approve_payment".into(),
            },
            approved_by: p("sam"),
        },
    )
    .unwrap();
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::AssignPath {
            party: p("ben"),
            scope: "queue:claims-life".into(),
        },
    )
    .unwrap();

    let mut admit = request("ana", "claim.admit", DataClass::Financial);
    admit.scopes = vec!["queue:claims-life".into()];
    admit.purpose = case("claim-1");
    admit.decides = true;
    admit.conflict_subject = Some("claim:claim-1".into());
    let d = s.authorise(&pol, &admit);
    assert!(d.allowed);
    // Recording for someone else is refused.
    assert_eq!(
        code(s.apply(
            &p("ben"),
            T0 + 61,
            StaffCommand::Record {
                request: admit.clone(),
                decision: d.clone()
            }
        )),
        "forbidden"
    );
    s.apply(
        &p("ana"),
        T0 + 61,
        StaffCommand::Record {
            request: admit.clone(),
            decision: d,
        },
    )
    .unwrap();

    let mut pay = admit.clone();
    pay.action = "claim.approve_payment".into();
    assert_eq!(s.authorise(&pol, &pay).denied, Some(Denial::Conflict));
    // The conflict runs both ways, and only for the person who acted.
    pay.staff = p("ben");
    let d = s.authorise(&pol, &pay);
    assert!(d.allowed);
    s.apply(
        &p("ben"),
        T0 + 62,
        StaffCommand::Record {
            request: pay.clone(),
            decision: d,
        },
    )
    .unwrap();
    let mut admit_by_ben = admit.clone();
    admit_by_ben.staff = p("ben");
    assert_eq!(s.authorise(&pol, &admit_by_ben).denied, Some(Denial::Conflict));
    // An action in no declared conflict is unaffected.
    let mut view = admit.clone();
    view.action = "claim.view".into();
    assert!(s.authorise(&pol, &view).allowed);

    let mut approve = pay.clone();
    approve.conflict_subject = None;
    approve.maker = Some(p("ben"));
    assert_eq!(s.authorise(&pol, &approve).denied, Some(Denial::MakerChecker));
    approve.maker = Some(p("ana"));
    assert!(s.authorise(&pol, &approve).allowed);

    // A denied decision is logged and leaves no act behind.
    let mut denied = admit.clone();
    denied.conflict_subject = Some("claim:claim-2".into());
    denied.purpose = None;
    let d = s.authorise(&pol, &denied);
    s.apply(&p("ana"), T0 + 63, StaffCommand::Record { request: denied, decision: d }).unwrap();
    assert!(!s.acts.contains_key("claim:claim-2"));
    let own = s.own_access(&p("ana"), T0 + 64);
    assert_eq!(own["recent"].as_array().unwrap().len(), 2);
    assert_eq!(own["recent"][0]["decision"]["denied"], "why");
}

/// Purpose: a cover grant gives the coverer the covered person's queues and not their authority;
/// a profile uplift raises a limit only to the template's ceiling; both need a second person, end
/// on their own, and are named on the decisions that relied on them. An uplift lasts two weeks at
/// most and raises amounts only, never the data classes seen; covering never opens the covered
/// person's clinical information.
/// Why it matters: leave cover and short staffing are when controls usually get bypassed with
/// shared logins. Grants make the exception explicit, time-boxed and auditable, and the ceiling
/// means no approval can hand out more authority than the role was ever meant to carry.
#[test]
fn cover_and_uplift_grants() {
    let mut s = staffed();
    let pol = policy();
    let cover = StaffCommand::RequestGrant {
        id: "g-cover".into(),
        holder: p("ben"),
        kind: GrantKind::Cover { covered: p("ana") },
        reason: "Ana on leave".into(),
        from: T0,
        to: T0 + 7 * DAY,
    };
    let events = s.apply(&p("ben"), T0, cover.clone()).unwrap();
    assert_eq!(
        events,
        vec![StaffEvent::GrantRequested {
            grant: "g-cover".into(),
            approver_action: actions::APPROVE_GRANT.into()
        }]
    );
    assert_eq!(code(s.apply(&p("ben"), T0, cover)), "duplicate");
    // Neither the holder nor someone without the action approves.
    assert_eq!(
        code(s.apply(&p("ben"), T0, StaffCommand::ApproveGrant { id: "g-cover".into() })),
        "maker_checker"
    );
    assert_eq!(code(s.apply(&p("col"), T0, StaffCommand::ApproveGrant { id: "g-cover".into() })), "forbidden");
    assert_eq!(code(s.apply(&p("mia"), T0, StaffCommand::ApproveGrant { id: "nope".into() })), "not_requested");

    let mut on_anas_queue = request("ben", "claim.admit", DataClass::Financial);
    on_anas_queue.scopes = vec!["queue:claims-life".into()];
    on_anas_queue.purpose = case("claim-1");
    on_anas_queue.decides = true;
    on_anas_queue.amount = Some(Amount {
        limit: "admit".into(),
        value: 50_000_00,
    });
    assert_eq!(s.authorise(&pol, &on_anas_queue).denied, Some(Denial::Whose));
    s.apply(&p("mia"), T0, StaffCommand::ApproveGrant { id: "g-cover".into() }).unwrap();
    let d = s.authorise(&pol, &on_anas_queue);
    assert!(d.allowed);
    assert_eq!(d.grant.as_deref(), Some("g-cover"));
    // Covering a colleague's queue never reaches clinical information, even with a task on one record (STF-018).
    let mut clinical = request("ben", "claim.view_clinical", DataClass::Clinical);
    clinical.scopes = vec!["queue:claims-life".into()];
    clinical.record = Some("claim-1".into());
    clinical.purpose = Some(Purpose {
        kind: PurposeKind::Task,
        reference: "task-4".into(),
    });
    assert_eq!(s.authorise(&pol, &clinical).denied, Some(Denial::Why));
    clinical.scopes = vec!["queue:claims-health".into()];
    assert!(s.authorise(&pol, &clinical).allowed, "on ben's own queue it is his own task");
    // The grant ends on its own.
    on_anas_queue.at = T0 + 7 * DAY;
    assert_eq!(s.authorise(&pol, &on_anas_queue).denied, Some(Denial::Whose));
    on_anas_queue.at = T0 + 60;

    // Uplift: above the ceiling is refused at request; within it, needs the function head.
    let uplift = |id: &str, value: i64| StaffCommand::RequestGrant {
        id: id.into(),
        holder: p("ben"),
        kind: GrantKind::ProfileUplift {
            limits: limits(&[("admit", value)]),
        },
        reason: "short staffed".into(),
        from: T0,
        to: T0 + 7 * DAY,
    };
    assert_eq!(code(s.apply(&p("ben"), T0, uplift("g-up", 600_000_00))), "above_ceiling");
    // An uplift is for a short-staffed week or two, never longer (STF-006).
    let long = StaffCommand::RequestGrant {
        id: "g-long".into(),
        holder: p("ben"),
        kind: GrantKind::ProfileUplift {
            limits: limits(&[("admit", 300_000_00)]),
        },
        reason: "short staffed".into(),
        from: T0,
        to: T0 + 15 * DAY,
    };
    assert_eq!(code(s.apply(&p("ben"), T0, long)), "too_long");
    s.apply(&p("ben"), T0, uplift("g-up", 300_000_00)).unwrap();
    assert_eq!(code(s.apply(&p("mia"), T0, StaffCommand::ApproveGrant { id: "g-up".into() })), "forbidden");
    s.apply(&p("sam"), T0, StaffCommand::ApproveGrant { id: "g-up".into() }).unwrap();
    on_anas_queue.amount = Some(Amount {
        limit: "admit".into(),
        value: 250_000_00,
    });
    let d = s.authorise(&pol, &on_anas_queue);
    assert!(d.allowed);
    assert_eq!(d.grant.as_deref(), Some("g-up"));
    on_anas_queue.amount = Some(Amount {
        limit: "admit".into(),
        value: 300_000_01,
    });
    assert_eq!(s.authorise(&pol, &on_anas_queue).denied, Some(Denial::HowMuch));
    // An uplift raises amounts only: it never widens the data classes the holder's roles see.
    let col_up = StaffCommand::RequestGrant {
        id: "g-col".into(),
        holder: p("col"),
        kind: GrantKind::ProfileUplift {
            limits: limits(&[("suspense", 5_000_00)]),
        },
        reason: "month end".into(),
        from: T0,
        to: T0 + 3 * DAY,
    };
    s.apply(&p("col"), T0, col_up).unwrap();
    s.apply(&p("sam"), T0, StaffCommand::ApproveGrant { id: "g-col".into() }).unwrap();
    let mut allocate = request("col", "suspense.allocate", DataClass::Financial);
    allocate.scopes = vec!["queue:collections".into()];
    allocate.purpose = case("case-9");
    allocate.amount = Some(Amount {
        limit: "suspense".into(),
        value: 5_000_00,
    });
    assert!(s.authorise(&pol, &allocate).allowed);
    allocate.class = DataClass::InsuranceSensitive;
    assert_eq!(s.authorise(&pol, &allocate).denied, Some(Denial::Class));
    // A limit the profile and templates do not know is zero.
    on_anas_queue.amount = Some(Amount {
        limit: "write_off".into(),
        value: 1,
    });
    assert_eq!(s.authorise(&pol, &on_anas_queue).denied, Some(Denial::HowMuch));

    // Revocation: by the holder's manager, at once, with sessions closed.
    assert_eq!(code(s.apply(&p("col"), T0 + 1, StaffCommand::RevokeGrant { id: "g-up".into() })), "forbidden");
    let events = s.apply(&p("mia"), T0 + 1, StaffCommand::RevokeGrant { id: "g-up".into() }).unwrap();
    assert!(events.contains(&StaffEvent::SessionsRevoked { party: p("ben") }));
    assert_eq!(code(s.apply(&p("mia"), T0 + 1, StaffCommand::RevokeGrant { id: "g-up".into() })), "not_active");
    s.apply(&p("ben"), T0 + 1, StaffCommand::RevokeGrant { id: "g-cover".into() }).unwrap();
    assert_eq!(GrantKind::Cover { covered: p("ana") }.key(), "cover");
}

/// Purpose: grant requests are validated (staff only, a reason, a sane period inside the maximum
/// life, a real person to cover, a complete one-off), can be declined by an approver or withdrawn
/// by the requester, and break-glass cannot be requested like an ordinary grant.
/// Why it matters: every one of these is a way a grant could otherwise become standing access:
/// no end date, a year-long "temporary" uplift, cover for nobody in particular.
#[test]
fn grant_requests_are_validated() {
    let mut s = staffed();
    let req = |id: &str, holder: &str, kind: GrantKind, reason: &str, from: At, to: At| StaffCommand::RequestGrant {
        id: id.into(),
        holder: p(holder),
        kind,
        reason: reason.into(),
        from,
        to,
    };
    let cover = GrantKind::Cover { covered: p("ana") };
    assert_eq!(code(s.apply(&p("nobody"), T0, req("g", "ben", cover.clone(), "r", T0, T0 + DAY))), "not_staff");
    assert_eq!(code(s.apply(&p("ben"), T0, req("g", "ben", cover.clone(), " ", T0, T0 + DAY))), "no_reason");
    assert_eq!(code(s.apply(&p("ben"), T0, req("g", "nobody", cover.clone(), "r", T0, T0 + DAY))), "not_staff");
    assert_eq!(code(s.apply(&p("ben"), T0, req("g", "ben", cover.clone(), "r", T0, T0))), "bad_period");
    assert_eq!(
        code(s.apply(&p("ben"), T0, req("g", "ben", cover.clone(), "r", T0 - 2 * DAY, T0 - DAY))),
        "bad_period"
    );
    assert_eq!(code(s.apply(&p("ben"), T0, req("g", "ben", cover, "r", T0, T0 + 31 * DAY))), "too_long");
    assert_eq!(
        code(s.apply(&p("ben"), T0, req("g", "ben", GrantKind::Cover { covered: p("ben") }, "r", T0, T0 + DAY))),
        "bad_cover"
    );
    assert_eq!(
        code(s.apply(&p("ben"), T0, req("g", "ben", GrantKind::Cover { covered: p("ghost") }, "r", T0, T0 + DAY))),
        "bad_cover"
    );
    let empty = GrantKind::OneOff {
        action: "".into(),
        record: "x".into(),
    };
    assert_eq!(code(s.apply(&p("ben"), T0, req("g", "ben", empty, "r", T0, T0 + 3600))), "bad_one_off");
    let glass = GrantKind::BreakGlass {
        template: "senior-claims".into(),
    };
    assert_eq!(glass.key(), "break_glass");
    assert_eq!(code(s.apply(&p("ben"), T0, req("g", "ben", glass, "r", T0, T0 + 3600))), "bad_kind");

    let one = GrantKind::OneOff {
        action: "claim.decline".into(),
        record: "claim-7".into(),
    };
    s.apply(&p("ben"), T0, req("g1", "ben", one.clone(), "r", T0, T0 + 3600)).unwrap();
    s.apply(&p("ben"), T0, req("g2", "ben", one, "r", T0, T0 + 3600)).unwrap();
    assert_eq!(code(s.apply(&p("col"), T0, StaffCommand::DeclineGrant { id: "g1".into() })), "forbidden");
    let events = s.apply(&p("mia"), T0, StaffCommand::DeclineGrant { id: "g1".into() }).unwrap();
    assert_eq!(
        events,
        vec![StaffEvent::GrantChanged {
            grant: "g1".into(),
            state: GrantState::Declined
        }]
    );
    s.apply(&p("ben"), T0, StaffCommand::DeclineGrant { id: "g2".into() }).unwrap();
    assert_eq!(code(s.apply(&p("mia"), T0, StaffCommand::DeclineGrant { id: "g2".into() })), "not_requested");
    // Uplift declines need the uplift approver.
    let up = GrantKind::ProfileUplift {
        limits: limits(&[("admit", 200_000_00)]),
    };
    assert_eq!(up.key(), "profile_uplift");
    s.apply(&p("ben"), T0, req("g3", "ben", up, "r", T0, T0 + DAY)).unwrap();
    assert_eq!(code(s.apply(&p("mia"), T0, StaffCommand::DeclineGrant { id: "g3".into() })), "forbidden");
    s.apply(&p("sam"), T0, StaffCommand::DeclineGrant { id: "g3".into() }).unwrap();
}

/// Purpose: a one-off grant allows one named action on one named record, shows on the decision,
/// and cannot be used a second time or on another record.
/// Why it matters: this is the controlled form of a planned exception. If it could be reused it
/// would be a standing permission that nobody recertifies.
#[test]
fn one_off_grant_is_consumed_on_use() {
    let mut s = staffed();
    let pol = policy();
    let kind = GrantKind::OneOff {
        action: "claim.decline".into(),
        record: "claim-7".into(),
    };
    assert_eq!(kind.key(), "one_off");
    s.apply(
        &p("mia"),
        T0,
        StaffCommand::RequestGrant {
            id: "g-one".into(),
            holder: p("ana"),
            kind,
            reason: "second signature needed today".into(),
            from: T0,
            to: T0 + 3600,
        },
    )
    .unwrap();
    // The requester is not the approver either.
    assert_eq!(code(s.apply(&p("mia"), T0, StaffCommand::ApproveGrant { id: "g-one".into() })), "maker_checker");
    s.apply(&p("sam"), T0, StaffCommand::ApproveGrant { id: "g-one".into() }).unwrap();

    let mut decline = request("ana", "claim.decline", DataClass::Personal);
    decline.scopes = vec!["queue:somewhere-else".into()];
    decline.record = Some("claim-8".into());
    decline.purpose = case("claim-8");
    decline.decides = true;
    assert_eq!(s.authorise(&pol, &decline).denied, Some(Denial::What));
    decline.record = Some("claim-7".into());
    let d = s.authorise(&pol, &decline);
    assert!(d.allowed);
    assert_eq!(d.grant.as_deref(), Some("g-one"));
    assert_eq!(d.template, None);
    let events = s
        .apply(
            &p("ana"),
            T0 + 61,
            StaffCommand::Record {
                request: decline.clone(),
                decision: d.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        events,
        vec![StaffEvent::GrantChanged {
            grant: "g-one".into(),
            state: GrantState::Consumed
        }]
    );
    assert_eq!(s.authorise(&pol, &decline).denied, Some(Denial::What));
    assert_eq!(
        code(s.apply(&p("ana"), T0 + 62, StaffCommand::Record { request: decline, decision: d })),
        "consumed"
    );
}

/// Purpose: break-glass gives a wider role at once for a short time, raises an alert, reaches
/// records without a path, never reaches clinical data, ends on its own and must be reviewed by
/// someone else.
/// Why it matters: an emergency path that needs an approval is useless at 2 a.m., and one with no
/// alert or review is a back door. This is the balance: immediate, loud, short and checked.
#[test]
fn break_glass_is_immediate_loud_short_and_reviewed() {
    let mut s = staffed();
    let pol = policy();
    assert_eq!(
        code(s.apply(
            &p("ana"),
            T0,
            StaffCommand::BreakGlass {
                id: "bg".into(),
                template: "senior-claims".into(),
                reason: "".into()
            }
        )),
        "no_reason"
    );
    assert_eq!(
        code(s.apply(
            &p("ana"),
            T0,
            StaffCommand::BreakGlass {
                id: "bg".into(),
                template: "administrator".into(),
                reason: "r".into()
            }
        )),
        "not_break_glass"
    );
    let events = s
        .apply(
            &p("col"),
            T0,
            StaffCommand::BreakGlass {
                id: "bg".into(),
                template: "senior-claims".into(),
                reason: "claims system incident".into(),
            },
        )
        .unwrap();
    assert!(matches!(&events[0], StaffEvent::BreakGlassUsed { party, template, .. } if party == &p("col") && template == "senior-claims"));
    assert_eq!(
        code(s.apply(
            &p("col"),
            T0,
            StaffCommand::BreakGlass {
                id: "bg".into(),
                template: "senior-claims".into(),
                reason: "again".into()
            }
        )),
        "duplicate"
    );

    let mut decline = request("col", "claim.decline", DataClass::Personal);
    decline.scopes = vec!["queue:claims-life".into()];
    decline.purpose = case("incident-1");
    decline.decides = true;
    let d = s.authorise(&pol, &decline);
    assert!(d.allowed, "{d:?}");
    assert_eq!(d.grant.as_deref(), Some("bg"));
    assert!(d.classes.contains(&DataClass::Personal) && !d.classes.contains(&DataClass::Clinical));
    let mut clinical = decline.clone();
    clinical.class = DataClass::Clinical;
    clinical.purpose = Some(Purpose {
        kind: PurposeKind::Task,
        reference: "t".into(),
    });
    assert!(!s.authorise(&pol, &clinical).allowed);
    decline.at = T0 + 4 * 3600;
    assert_eq!(s.authorise(&pol, &decline).denied, Some(Denial::What));

    assert_eq!(
        code(s.apply(&p("ana"), T0 + 1, StaffCommand::ReviewBreakGlass { id: "bg".into() })),
        "forbidden"
    );
    s.apply(
        &p("alice"),
        T0 + 1,
        StaffCommand::BreakGlass {
            id: "bg2".into(),
            template: "senior-claims".into(),
            reason: "test".into(),
        },
    )
    .unwrap();
    assert_eq!(
        code(s.apply(&p("alice"), T0 + 2, StaffCommand::ReviewBreakGlass { id: "bg2".into() })),
        "maker_checker"
    );
    s.apply(&p("alice"), T0 + 2, StaffCommand::ReviewBreakGlass { id: "bg".into() }).unwrap();
    assert_eq!(s.grants["bg"].reviewed, Some((p("alice"), T0 + 2)));
    assert_eq!(
        code(s.apply(&p("alice"), T0 + 3, StaffCommand::ReviewBreakGlass { id: "bg".into() })),
        "not_reviewable"
    );
}

/// Purpose: leaving ends every role, path and grant at once and closes sessions; roles lapse
/// unless the manager recertifies them; recertification drops what the manager does not confirm.
/// Why it matters: access that outlives the job is the most common finding in any access audit.
/// Nothing here depends on someone remembering to remove it.
#[test]
fn leaving_and_recertification() {
    let mut s = staffed();
    let pol = policy();
    s.apply(
        &p("ana"),
        T0,
        StaffCommand::RequestGrant {
            id: "g".into(),
            holder: p("ana"),
            kind: GrantKind::Cover { covered: p("ben") },
            reason: "r".into(),
            from: T0,
            to: T0 + DAY,
        },
    )
    .unwrap();
    let view = request("ana", "claim.view", DataClass::Ordinary);
    assert!(s.authorise(&pol, &view).allowed);

    // Unconfirmed roles lapse after the period.
    let mut later = view.clone();
    later.at = T0 + 181 * DAY;
    assert_eq!(s.authorise(&pol, &later).denied, Some(Denial::What));
    assert_eq!(
        code(s.apply(
            &p("alice"),
            T0 + 181 * DAY,
            StaffCommand::Recertify {
                party: p("ana"),
                confirmed: vec![]
            }
        )),
        "forbidden"
    );
    assert_eq!(
        code(s.apply(
            &p("mia"),
            T0 + 181 * DAY,
            StaffCommand::Recertify {
                party: p("ghost"),
                confirmed: vec![]
            }
        )),
        "not_staff"
    );
    // Mia's own roles have lapsed too, but recertifying her team is her duty as manager, not a role.
    s.apply(
        &p("mia"),
        T0 + 181 * DAY,
        StaffCommand::Recertify {
            party: p("ana"),
            confirmed: vec!["claims-assessor".into()],
        },
    )
    .unwrap();
    assert!(s.authorise(&pol, &later).allowed);
    let own = s.own_access(&p("ana"), T0 + 181 * DAY);
    assert_eq!(own["recertification_due"], T0 + 361 * DAY);
    // Confirming nothing removes the role.
    s.apply(
        &p("mia"),
        T0 + 182 * DAY,
        StaffCommand::Recertify {
            party: p("ana"),
            confirmed: vec![],
        },
    )
    .unwrap();
    later.at = T0 + 182 * DAY + 1;
    assert_eq!(s.authorise(&pol, &later).denied, Some(Denial::What));

    let events = s.apply(&p("alice"), T0 + 10, StaffCommand::EndEmployment { party: p("ana") }).unwrap();
    assert!(events.contains(&StaffEvent::SessionsRevoked { party: p("ana") }));
    assert!(events.contains(&StaffEvent::GrantChanged {
        grant: "g".into(),
        state: GrantState::Revoked
    }));
    let mut after = view.clone();
    after.at = T0 + 11;
    assert_eq!(s.authorise(&pol, &after).denied, Some(Denial::NotStaff));
    assert!(!s.paths.contains_key(&p("ana")));
    assert_eq!(
        code(s.apply(&p("alice"), T0 + 10, StaffCommand::EndEmployment { party: p("ghost") })),
        "not_staff"
    );
    assert_eq!(s.own_access(&p("ana"), T0 + 11)["staff"], false);
}

/// Purpose: staff administration is itself controlled: only people with the action manage staff,
/// nobody gives themselves a role, unknown templates and people are refused, and paths and
/// templates can be removed.
/// Why it matters: the commands that hand out access are the most valuable ones to attack. A
/// manager who can assign themselves the payments role has every role.
#[test]
fn staff_administration_is_controlled() {
    let mut s = staffed();
    let employ = |party: &str, templates: &[&str]| StaffCommand::Employ {
        party: p(party),
        team: "t".into(),
        manager: None,
        templates: templates.iter().map(|t| t.to_string()).collect(),
    };
    assert_eq!(code(s.apply(&p("ana"), T0, employ("new", &["claims-assessor"]))), "forbidden");
    assert_eq!(code(s.apply(&p("alice"), T0, employ("new", &["wizard"]))), "unknown_template");
    assert_eq!(code(s.apply(&p("alice"), T0, employ("ana", &["claims-assessor"]))), "already_staff");

    let assign = |party: &str, template: &str| StaffCommand::AssignTemplate {
        party: p(party),
        template: template.into(),
    };
    assert_eq!(code(s.apply(&p("ana"), T0, assign("ben", "security"))), "forbidden");
    assert_eq!(code(s.apply(&p("alice"), T0, assign("ben", "wizard"))), "unknown_template");
    assert_eq!(code(s.apply(&p("alice"), T0, assign("alice", "security"))), "self_service");
    assert_eq!(code(s.apply(&p("alice"), T0, assign("ghost", "security"))), "not_staff");
    s.apply(&p("alice"), T0, assign("ben", "collections-officer")).unwrap();
    assert!(s.holds_action(&p("ben"), "suspense.allocate", T0));

    let remove = |party: &str, template: &str| StaffCommand::RemoveTemplate {
        party: p(party),
        template: template.into(),
    };
    assert_eq!(code(s.apply(&p("ana"), T0, remove("ben", "collections-officer"))), "forbidden");
    assert_eq!(code(s.apply(&p("alice"), T0, remove("ben", "security"))), "not_held");
    let events = s.apply(&p("alice"), T0 + 5, remove("ben", "collections-officer")).unwrap();
    assert_eq!(events, vec![StaffEvent::SessionsRevoked { party: p("ben") }]);
    assert!(!s.holds_action(&p("ben"), "suspense.allocate", T0 + 5));

    assert_eq!(
        code(s.apply(
            &p("ana"),
            T0,
            StaffCommand::AssignPath {
                party: p("ben"),
                scope: "q".into()
            }
        )),
        "forbidden"
    );
    assert_eq!(
        code(s.apply(
            &p("alice"),
            T0,
            StaffCommand::AssignPath {
                party: p("ghost"),
                scope: "q".into()
            }
        )),
        "not_staff"
    );
    assert_eq!(
        code(s.apply(
            &p("ana"),
            T0,
            StaffCommand::RemovePath {
                party: p("ben"),
                scope: "q".into()
            }
        )),
        "forbidden"
    );
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::RemovePath {
            party: p("ben"),
            scope: "queue:claims-health".into(),
        },
    )
    .unwrap();
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::RemovePath {
            party: p("ghost"),
            scope: "q".into(),
        },
    )
    .unwrap();
    assert!(s.paths[&p("ben")].is_empty());

    // Bootstrap happens once, and not with templates that do not exist.
    assert_eq!(
        code(s.apply(
            &p("x"),
            T0,
            StaffCommand::Bootstrap {
                templates: vec![],
                profiles: vec![],
                admin: p("x"),
                admin_templates: vec![]
            }
        )),
        "already_bootstrapped"
    );
    let mut empty = StaffState::new();
    let bad = StaffCommand::Bootstrap {
        templates: vec![],
        profiles: vec![],
        admin: p("x"),
        admin_templates: vec!["wizard".into()],
    };
    assert_eq!(code(empty.apply(&p("x"), T0, bad)), "unknown_template");
    assert_eq!(empty, StaffState::new());
}

/// Purpose: configuration (templates, profiles, conflicts, settings) needs a proposer and a
/// different approver who each hold the right action; profile versions go up by one; a person's
/// profile changes with two approvals, never above the role's ceiling, and decisions carry the
/// version they were made under.
/// Why it matters: a later question about whether someone was allowed to do something is answered
/// from the record. That only works if authority changes are versioned and nobody can widen their
/// own.
#[test]
fn configuration_and_profiles_need_two_people() {
    let mut s = staffed();
    let pol = policy();
    let v2 = Profile {
        id: "assessor-1".into(),
        version: 2,
        limits: limits(&[("admit", 150_000_00)]),
    };
    let activate = |profile: &Profile, by: &str| StaffCommand::ActivateProfile {
        profile: profile.clone(),
        approved_by: p(by),
    };
    assert_eq!(code(s.apply(&p("alice"), T0, activate(&v2, "alice"))), "maker_checker");
    assert_eq!(code(s.apply(&p("ana"), T0, activate(&v2, "sam"))), "forbidden");
    assert_eq!(code(s.apply(&p("alice"), T0, activate(&v2, "mia"))), "forbidden");
    let v3 = Profile { version: 3, ..v2.clone() };
    assert_eq!(code(s.apply(&p("alice"), T0, activate(&v3, "sam"))), "bad_version");
    s.apply(&p("alice"), T0, activate(&v2, "sam")).unwrap();
    let senior = Profile {
        id: "assessor-2".into(),
        version: 1,
        limits: limits(&[("admit", 400_000_00)]),
    };
    s.apply(&p("alice"), T0, activate(&senior, "sam")).unwrap();
    let huge = Profile {
        id: "assessor-9".into(),
        version: 1,
        limits: limits(&[("admit", 900_000_00)]),
    };
    s.apply(&p("alice"), T0, activate(&huge, "sam")).unwrap();

    let mut admit = request("ana", "claim.admit", DataClass::Financial);
    admit.scopes = vec!["queue:claims-life".into()];
    admit.purpose = case("c");
    admit.decides = true;
    admit.amount = Some(Amount {
        limit: "admit".into(),
        value: 140_000_00,
    });
    assert_eq!(s.authorise(&pol, &admit).profile, Some(("assessor-1".to_string(), 2)));
    assert!(s.authorise(&pol, &admit).allowed);

    let set_profile = |party: &str, profile: Option<&str>, by: &str| StaffCommand::SetProfile {
        party: p(party),
        profile: profile.map(String::from),
        approved_by: p(by),
    };
    assert_eq!(code(s.apply(&p("ana"), T0, set_profile("ben", Some("assessor-2"), "sam"))), "forbidden");
    assert_eq!(code(s.apply(&p("alice"), T0, set_profile("ana", Some("assessor-2"), "mia"))), "forbidden");
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::AssignTemplate {
            party: p("sam"),
            template: "administrator".into(),
        },
    )
    .unwrap();
    assert_eq!(code(s.apply(&p("sam"), T0, set_profile("ana", Some("assessor-2"), "sam"))), "maker_checker");
    assert_eq!(code(s.apply(&p("alice"), T0, set_profile("sam", Some("assessor-2"), "sam"))), "maker_checker");
    assert_eq!(code(s.apply(&p("alice"), T0, set_profile("ana", Some("nope"), "sam"))), "unknown_profile");
    assert_eq!(code(s.apply(&p("alice"), T0, set_profile("ana", Some("assessor-9"), "sam"))), "above_ceiling");
    assert_eq!(code(s.apply(&p("alice"), T0, set_profile("ghost", None, "sam"))), "not_staff");
    s.apply(&p("alice"), T0, set_profile("ana", Some("assessor-2"), "sam")).unwrap();
    admit.amount = Some(Amount {
        limit: "admit".into(),
        value: 390_000_00,
    });
    assert_eq!(s.authorise(&pol, &admit).profile, Some(("assessor-2".to_string(), 1)));
    assert!(s.authorise(&pol, &admit).allowed);
    assert_eq!(s.own_access(&p("ana"), T0 + 1)["profile"]["id"], "assessor-2");
    s.apply(&p("alice"), T0, set_profile("ana", None, "sam")).unwrap();

    let t = template("trainee", &["claim.view"], &[DataClass::Ordinary], "nope", &[]);
    assert_eq!(
        code(s.apply(
            &p("alice"),
            T0,
            StaffCommand::DefineTemplate {
                template: t.clone(),
                approved_by: p("sam")
            }
        )),
        "unknown_profile"
    );
    let t = Template {
        default_profile: "none".into(),
        ..t
    };
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::DefineTemplate {
            template: t,
            approved_by: p("sam"),
        },
    )
    .unwrap();
    assert!(s.templates.contains_key("trainee"));
    let settings = Settings {
        recertify_every: 90 * DAY,
        ..Settings::default()
    };
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::ChangeSettings {
            settings: settings.clone(),
            approved_by: p("sam"),
        },
    )
    .unwrap();
    assert_eq!(s.settings, settings);
    // A person with no template has no profile to show.
    assert!(s.own_access(&p("ghost"), T0)["profile"].is_null());
}

fn mapping(version: u32) -> Mapping {
    Mapping {
        version,
        effective_from: T0,
        roles: [
            ("IGA-Claims-Assessor".to_string(), vec!["claims-assessor".to_string()]),
            ("IGA-Collections".to_string(), vec!["collections-officer".to_string()]),
        ]
        .into(),
        elevations: [(
            "pam-cover".to_string(),
            ElevationRule {
                kind: "cover".into(),
                max_secs: 2 * DAY,
            },
        )]
        .into(),
        templates_external: true,
        grants_external: set(&["cover"]),
        alignment_owner: Some(p("sam")),
    }
}

/// Purpose: with an external identity governance system as the source, role holdings follow what
/// it says: mapped entitlements become templates, unmapped ones confer nothing and are reported
/// with the member of staff the mapping names to work them,
/// a full reconciliation revokes what the source no longer grants, durations are mirrored, and
/// local assignment is switched off.
/// Why it matters: an insurer that already runs an identity governance tool will not maintain
/// roles twice. The platform must follow that tool exactly, including when it takes access away,
/// while never letting an unknown entitlement turn into access by default.
#[test]
fn external_source_drives_templates() {
    let mut s = staffed();
    let m = mapping(1);
    let activate = |m: &Mapping, by: &str| StaffCommand::ActivateMapping {
        mapping: m.clone(),
        approved_by: p(by),
    };
    let sync = |holdings: Vec<ExternalHolding>, full: bool| StaffCommand::ExternalSync { holdings, full };
    let holding = |person: &str, entitlement: &str, to: Option<At>| ExternalHolding {
        person: p(person),
        entitlement: entitlement.into(),
        reference: format!("iga-{person}"),
        to,
    };
    assert_eq!(code(s.apply(&p("m"), T0, sync(vec![], true))), "no_mapping");
    assert_eq!(s.alignment(), Alignment::default());
    assert_eq!(code(s.apply(&p("alice"), T0, activate(&m, "alice"))), "maker_checker");
    assert_eq!(code(s.apply(&p("alice"), T0, activate(&mapping(2), "sam"))), "bad_version");
    let mut unknown = m.clone();
    unknown.roles.insert("X".into(), vec!["wizard".into()]);
    assert_eq!(code(s.apply(&p("alice"), T0, activate(&unknown, "sam"))), "unknown_template");
    let mut bad = m.clone();
    bad.elevations.insert(
        "pam-root".into(),
        ElevationRule {
            kind: "break_glass".into(),
            max_secs: 10,
        },
    );
    assert_eq!(code(s.apply(&p("alice"), T0, activate(&bad, "sam"))), "bad_elevation");
    // Every exception the report lists has someone to work it: a mapping names a current member of staff.
    for owner in [None, Some(p("nobody"))] {
        let unowned = Mapping {
            alignment_owner: owner,
            ..m.clone()
        };
        assert_eq!(code(s.apply(&p("alice"), T0, activate(&unowned, "sam"))), "no_alignment_owner");
    }
    s.apply(&p("alice"), T0, activate(&m, "sam")).unwrap();
    assert_eq!(s.alignment().owner, Some(p("sam")));

    // Templates now come from the source.
    assert_eq!(
        code(s.apply(
            &p("alice"),
            T0,
            StaffCommand::AssignTemplate {
                party: p("ben"),
                template: "security".into()
            }
        )),
        "external_source"
    );
    // Before the first sync, everyone's holdings are local only.
    let a = s.alignment();
    assert!(a.local_only.contains(&(p("ana"), "claims-assessor".to_string())));
    assert!(a.templates_without_source.contains("security"));
    assert!(!a.templates_without_source.contains("claims-assessor"));

    s.apply(
        &p("m"),
        T0 + 1,
        sync(
            vec![
                holding("col", "IGA-Claims-Assessor", Some(T0 + 30 * DAY)),
                holding("col", "IGA-Collections", None),
                holding("col", "IGA-Unknown", None),
            ],
            false,
        ),
    )
    .unwrap();
    assert!(s.holds_action(&p("col"), "claim.admit", T0 + 2));
    assert!(!s.holds_action(&p("col"), "claim.admit", T0 + 30 * DAY), "the source's end date is mirrored");
    assert_eq!(s.alignment().unmapped, set(&["IGA-Unknown"]));
    assert!(matches!(&s.members[&p("col")].templates["claims-assessor"].source, Source::External { reference } if reference == "iga-col"));
    // The source also grants the collections role col was assigned here: the source takes it over.
    assert!(matches!(&s.members[&p("col")].templates["collections-officer"].source, Source::External { .. }));
    assert!(!s.alignment().local_only.contains(&(p("col"), "collections-officer".to_string())));
    // A second partial sync mirrors a changed duration and leaves what it does not mention alone.
    s.apply(
        &p("m"),
        T0 + 2,
        sync(
            vec![holding("col", "IGA-Claims-Assessor", Some(T0 + 10 * DAY)), holding("col", "IGA-Other", None)],
            false,
        ),
    )
    .unwrap();
    assert!(!s.holds_action(&p("col"), "claim.admit", T0 + 10 * DAY));
    assert_eq!(s.alignment().unmapped, set(&["IGA-Unknown", "IGA-Other"]));
    assert!(s.templates_of(&p("col"), T0 + 3).iter().any(|t| t.id == "collections-officer"));

    // The reconciliation pull no longer lists the assessor role: it is revoked, sessions closed.
    let events = s.apply(&p("m"), T0 + 3, sync(vec![holding("col", "IGA-Collections", None)], true)).unwrap();
    assert_eq!(events, vec![StaffEvent::SessionsRevoked { party: p("col") }]);
    assert!(!s.holds_action(&p("col"), "claim.admit", T0 + 4));
    assert!(s.alignment().unmapped.is_empty());
}

/// Purpose: just-in-time elevation approved in the external tool becomes a grant carrying the
/// external reference, capped by the platform's maximum, revoked when the tool deactivates it,
/// and refused (fail closed) while the source is unavailable; grants of an externally managed kind
/// cannot be requested locally; existing grants keep running through an outage.
/// Why it matters: the external tool owns the approval, the platform owns the ceilings. An outage
/// of the tool must neither lock staff out of work already approved nor open a path around it.
#[test]
fn external_elevation_is_capped_and_fails_closed() {
    let mut s = staffed();
    let elevate = |reference: &str, person: &str, elevation: &str, kind: GrantKind, to: At| StaffCommand::ExternalElevation {
        reference: reference.into(),
        person: p(person),
        elevation: elevation.into(),
        kind,
        to,
    };
    let cover = GrantKind::Cover { covered: p("ana") };
    assert_eq!(
        code(s.apply(&p("m"), T0, elevate("r1", "ben", "pam-cover", cover.clone(), T0 + DAY))),
        "no_mapping"
    );
    s.apply(
        &p("alice"),
        T0,
        StaffCommand::ActivateMapping {
            mapping: mapping(1),
            approved_by: p("sam"),
        },
    )
    .unwrap();
    assert_eq!(
        code(s.apply(&p("m"), T0, elevate("r1", "ben", "pam-root", cover.clone(), T0 + DAY))),
        "unmapped"
    );
    let one = GrantKind::OneOff {
        action: "a".into(),
        record: "r".into(),
    };
    assert_eq!(code(s.apply(&p("m"), T0, elevate("r1", "ben", "pam-cover", one, T0 + DAY))), "bad_kind");
    assert_eq!(
        code(s.apply(
            &p("ben"),
            T0,
            StaffCommand::RequestGrant {
                id: "g".into(),
                holder: p("ben"),
                kind: cover.clone(),
                reason: "r".into(),
                from: T0,
                to: T0 + DAY
            }
        )),
        "external_source"
    );

    // The tool approved ten days; the mapping caps this elevation at two.
    let events = s.apply(&p("m"), T0, elevate("r1", "ben", "pam-cover", cover.clone(), T0 + 10 * DAY)).unwrap();
    assert_eq!(
        events,
        vec![StaffEvent::GrantChanged {
            grant: "ext:r1".into(),
            state: GrantState::Active
        }]
    );
    let g = &s.grants["ext:r1"];
    assert_eq!(g.to, T0 + 2 * DAY);
    assert_eq!(g.source, Source::External { reference: "r1".into() });
    assert_eq!(
        code(s.apply(&p("m"), T0, elevate("r1", "ben", "pam-cover", cover.clone(), T0 + DAY))),
        "duplicate"
    );

    // Outage: nothing new, existing grants continue.
    let events = s.apply(&p("m"), T0 + 5, StaffCommand::ExternalSourceAvailable { available: false }).unwrap();
    assert_eq!(events, vec![StaffEvent::ExternalSource { available: false }]);
    assert!(s
        .apply(&p("m"), T0 + 5, StaffCommand::ExternalSourceAvailable { available: false })
        .unwrap()
        .is_empty());
    assert_eq!(
        code(s.apply(&p("m"), T0 + 6, elevate("r2", "ben", "pam-cover", cover.clone(), T0 + DAY))),
        "external_source"
    );
    assert_eq!(s.active_grants(&p("ben"), T0 + 6).len(), 1);
    s.apply(&p("m"), T0 + 7, StaffCommand::ExternalSourceAvailable { available: true }).unwrap();
    // A shorter external duration is kept as it is.
    s.apply(&p("m"), T0 + 8, elevate("r2", "col", "pam-cover", cover, T0 + 3600)).unwrap();
    assert_eq!(s.grants["ext:r2"].to, T0 + 3600);

    let events = s
        .apply(&p("m"), T0 + 9, StaffCommand::ExternalElevationEnded { reference: "r1".into() })
        .unwrap();
    assert!(events.contains(&StaffEvent::SessionsRevoked { party: p("ben") }));
    assert!(s.active_grants(&p("ben"), T0 + 10).is_empty());
    assert_eq!(
        code(s.apply(&p("m"), T0 + 9, StaffCommand::ExternalElevationEnded { reference: "r1".into() })),
        "not_active"
    );
    // Own access lists the active external grant for its holder.
    assert_eq!(s.own_access(&p("col"), T0 + 10)["grants"].as_array().unwrap().len(), 1);
}

/// Purpose: staff declare their own personal connections, only a staff administrator who is not the person concerned can remove one,
/// and an attempt on a record with no path or on a personal record is flagged in the log and raised as an event.
/// Why it matters: looking up a relative, an ex-partner or oneself is the classic misuse of an insurer's systems. A declaration the
/// person could quietly withdraw, or a refused attempt nobody is told about, would not deter it.
#[test]
fn personal_connections_and_flagged_attempts() {
    let mut s = staffed();
    assert_eq!(
        code(s.apply(&p("stranger"), T0, StaffCommand::DeclareConnection { party: p("cousin") })),
        "forbidden"
    );
    s.apply(&p("ana"), T0, StaffCommand::DeclareConnection { party: p("cousin") }).unwrap();
    assert_eq!(s.own_access(&p("ana"), T0)["connections"], serde_json::json!(["cousin"]));
    let end = |staff: &str, party: &str| StaffCommand::EndConnection {
        staff: p(staff),
        party: p(party),
    };
    assert_eq!(code(s.apply(&p("ana"), T0, end("ana", "cousin"))), "forbidden");
    assert_eq!(code(s.apply(&p("ben"), T0, end("ana", "cousin"))), "forbidden");
    assert_eq!(code(s.apply(&p("alice"), T0, end("ana", "nobody"))), "unknown_connection");
    assert_eq!(code(s.apply(&p("alice"), T0, end("ben", "cousin"))), "unknown_connection");

    // A refusal for want of a path, and one for a personal record, are each flagged; an ordinary refusal is not.
    let record = |s: &mut StaffState, denial: Denial| {
        let request = request("ana", "claim.view", DataClass::Ordinary);
        let decision = Decision {
            allowed: false,
            denied: Some(denial),
            template: None,
            profile: None,
            grant: None,
            classes: BTreeSet::new(),
        };
        s.apply(&p("ana"), T0, StaffCommand::Record { request, decision }).unwrap()
    };
    let flagged = |events: &[StaffEvent]| {
        events
            .iter()
            .filter_map(|e| match e {
                StaffEvent::AccessFlagged { staff, reason, .. } => Some((staff.to_string(), reason.clone())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(flagged(&record(&mut s, Denial::Whose)), [("ana".to_string(), "no_path".to_string())]);
    assert_eq!(flagged(&record(&mut s, Denial::Personal)), [("ana".to_string(), "personal".to_string())]);
    assert!(flagged(&record(&mut s, Denial::Why)).is_empty());
    let flags: Vec<Option<String>> = s.recent[&p("ana")].iter().map(|r| r.flag.clone()).collect();
    assert_eq!(flags, [Some("no_path".to_string()), Some("personal".to_string()), None]);
    assert!(Denial::Personal.explain().contains("colleague"));

    s.apply(&p("alice"), T0, end("ana", "cousin")).unwrap();
    assert!(s.connections[&p("ana")].is_empty());
}

/// Purpose: a product owner and a platform engineer, whose templates hold no personal data class, are refused customer data
/// whichever action they ask for: a policy's personal detail and a claim's insurance-sensitive detail are refused for the class
/// even where the template holds the action, and an action the template does not hold is refused for that.
/// Why it matters: persona row STF-017. People who build and run the system need to see it working, not who the customers are;
/// a role that drifted into a personal class would let them read every policy without a case.
#[test]
fn builders_and_operators_hold_no_personal_class() {
    let mut s = StaffState::new();
    let owner = template("product-owner", &["policy.view", "product.configure"], &[DataClass::Ordinary], "none", &[]);
    let engineer = template("platform-engineer", &["platform.operate", "claim.view"], &[DataClass::Ordinary], "none", &[]);
    let admin = template("administrator", &[actions::MANAGE_STAFF], &[DataClass::Ordinary], "none", &[]);
    s.apply(
        &p("module:entitlements"),
        T0,
        StaffCommand::Bootstrap {
            templates: vec![admin, owner, engineer],
            profiles: vec![Profile {
                id: "none".into(),
                version: 1,
                limits: BTreeMap::new(),
            }],
            admin: p("alice"),
            admin_templates: vec!["administrator".into()],
        },
    )
    .unwrap();
    for (who, templates) in [("po", "product-owner"), ("pe", "platform-engineer")] {
        s.apply(
            &p("alice"),
            T0,
            StaffCommand::Employ {
                party: p(who),
                team: "platform".into(),
                manager: None,
                templates: vec![templates.into()],
            },
        )
        .unwrap();
    }
    let pol = policy();
    for (who, action, class, denied) in [
        ("po", "policy.view", DataClass::Personal, Denial::Class),
        ("po", "claim.view", DataClass::InsuranceSensitive, Denial::What),
        ("pe", "claim.view", DataClass::InsuranceSensitive, Denial::Class),
        ("pe", "policy.view", DataClass::Personal, Denial::What),
        ("pe", "claim.view_clinical", DataClass::Clinical, Denial::What),
    ] {
        let mut r = request(who, action, class);
        r.purpose = case("case-1");
        assert_eq!(s.authorise(&pol, &r).denied, Some(denied), "{who} {action}");
    }
}
