//! End to end: the core's events in, what each kind of person sees out.
//! Each test drives a simulated core cluster, relays its event stream into
//! the channel state, and looks at device feeds and portal views.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_channels::directory::{AdviserStanding, DelegateAccess, DirectoryCommand, LicenceStatus, StructureLink};
use ic_channels::feed::{Op, Row, Sync};
use ic_channels::state::{Approver, RequestStatus, EVENT_RELAY, GATEWAY};
use ic_channels::{Command, Effect};
use ic_core::command::MemberChange;
use ic_core::policy::LifeKind;
use ic_core::{LifeId, PolicyId};
use ic_product::{Frequency, Sex};
use ic_readmodel::scope::Relationship;
use ic_sim::persona::{group_scheme, health_family, member, term_life};
use serde_json::json;

const KIRI: &str = "party-00000001";

fn retail(w: &mut World) -> PolicyId {
    w.issue(term_life(
        1,
        open(),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        Some(100_000_00),
        Frequency::Monthly,
        None,
    ))
}

fn linked_member(reference: &str, party: &str) -> ic_core::command::LifeSpec {
    let mut m = member(reference, d(1980, 1, 1), "staff", 80_000_00);
    m.kind = LifeKind::UnnamedMember {
        member_ref: reference.to_string(),
        linked_party: Some(p(party)),
    };
    m
}

fn submit(id: &str, policy: Option<&PolicyId>, kind: &str, module: &str) -> Command {
    Command::SubmitRequest {
        id: id.to_string(),
        policy: policy.cloned(),
        kind: kind.to_string(),
        module: module.to_string(),
        payload: json!({ "choice": "yes" }),
        signed: false,
        route_to_person: false,
    }
}

fn moved(effects: &[Effect]) -> BTreeSet<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::FeedMoved { party, .. } => Some(party.0.clone()),
            _ => None,
        })
        .collect()
}

fn revoked(effects: &[Effect]) -> BTreeSet<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Revoked { party } => Some(party.0.clone()),
            _ => None,
        })
        .collect()
}

/// Purpose: a retail customer's device receives their policy, cover, billing and notices from the core's events, and follows later changes by version.
/// Why it matters: this is the customer app's whole offline view; it must be complete, belong only to the customer, and move when the core does.
#[test]
fn a_retail_customer_follows_their_policy() {
    let mut w = World::new();
    let pol = retail(&mut w);
    assert_eq!(
        w.held(KIRI),
        ["billing", "cover/life/1", "cover/trauma/1", "notice/1", "policy"].map(|s| format!("{pol}/{s}"))
    );
    assert_eq!(w.row(KIRI, &format!("{pol}/policy")).unwrap()["number"], "KTL00000001");
    assert_eq!(w.row(KIRI, &format!("{pol}/cover/life/1")).unwrap()["sum_insured_minor"], 500_000_00);
    assert_eq!(w.ch.data.feeds.len(), 1, "nobody else holds anything");
    assert!(w.sees("stranger", &pol).is_empty());
    assert_eq!(w.ch.data.audiences[&pol], [p(KIRI)].into());

    // The device takes a snapshot, then follows.
    let Sync::Snapshot { version, rows } = w.feed(KIRI).unwrap().sync(None) else {
        panic!("expected a snapshot")
    };
    assert_eq!(rows.len(), 5);

    // The customer declines indexation in the core; the policy row moves.
    w.core(
        &pol,
        "policy-servicing",
        ic_core::Command::DeclineIndexation {
            policy: pol.clone(),
            declined: true,
        },
    );
    let effects = w.relay();
    assert_eq!(moved(&effects), [KIRI.to_string()].into());
    let Sync::Changes { changes, .. } = w.feed(KIRI).unwrap().sync(Some(version)) else {
        panic!("expected changes")
    };
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, format!("{pol}/policy"));
    assert_eq!(w.row(KIRI, &format!("{pol}/policy")).unwrap()["indexation_declined"], true);
    assert_eq!(w.ch.today(0), open(), "the business date comes from the stream");
}

/// Purpose: the relay delivers at least once and in order; a repeat changes nothing, a hole is reported and nothing after it is applied, and redelivery heals it.
/// Why it matters: the event stream is the only source of the views; applying across a gap would leave them silently wrong.
#[test]
fn duplicates_are_ignored_and_gaps_are_reported() {
    let mut w = World::new();
    let issue = term_life(1, open(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let (partition, pol) = (w.cluster.partition_of(&issue.group), issue.policy.clone());
    w.cluster.submit(partition, "new-business", ic_sim::persona::issue_command(issue)).unwrap();
    let records = w.cluster.take_events();
    assert!(records.len() >= 3);

    // The first record is lost on the way.
    let effects = w
        .send(
            module(EVENT_RELAY),
            Command::CoreEvents {
                records: records[1..].to_vec(),
            },
        )
        .unwrap();
    assert_eq!(
        effects,
        vec![Effect::Gap {
            partition,
            expected: records[0].seq
        }]
    );
    assert!(w.feed(KIRI).is_none(), "nothing after the hole was applied");

    // Redelivered whole, then delivered again.
    let effects = w.send(module(EVENT_RELAY), Command::CoreEvents { records: records.clone() }).unwrap();
    assert_eq!(moved(&effects), [KIRI.to_string()].into());
    let version = w.feed(KIRI).unwrap().version;
    let effects = w.send(module(EVENT_RELAY), Command::CoreEvents { records }).unwrap();
    assert!(effects.is_empty());
    assert_eq!(w.feed(KIRI).unwrap().version, version);
    assert!(w.sees(KIRI, &pol).len() >= 3);

    // Only the relay may deliver events or say the stream is current.
    assert_eq!(code(w.send(module("claims"), Command::CoreEvents { records: vec![] })), "forbidden");
    assert_eq!(code(w.send(party(KIRI), Command::Heartbeat)), "forbidden");
}

/// Purpose: relationship data goes stale when the stream is silent, and a heartbeat from the relay keeps it fresh; a directory command does not.
/// Why it matters: sensitive rows are never shown on stale relationships, so freshness must reflect the stream and nothing else.
#[test]
fn freshness_follows_the_stream() {
    let mut w = World::new();
    let pol = retail(&mut w);
    let heard = w.ch.data.directory.heard;
    assert!(w.ch.data.directory.fresh(heard + 30, 30));
    assert!(!w.ch.data.directory.fresh(heard + 31, 30));
    w.at += 100;
    w.engage("e1", "adv-1", "fap-1", None).unwrap();
    assert_eq!(w.ch.data.directory.heard, heard, "a command is not news from the stream");
    w.send(module(EVENT_RELAY), Command::Heartbeat).unwrap();
    assert_eq!(w.ch.data.directory.heard, w.at);

    // On stale data a sensitive row is withheld, even from the person it is about.
    w.send(
        module("claims"),
        Command::PublishRows {
            rows: vec![Row {
                key: format!("{pol}/claims/c1"),
                policy: pol.clone(),
                kind: "claim".into(),
                subject: Some(p(KIRI)),
                sensitive: true,
                body: json!({ "state": "received" }),
            }],
            remove: vec![],
        },
    )
    .unwrap();
    let on = w.ch.today(w.at);
    let keys = |fresh| w.ch.visible_rows(&p(KIRI), &pol, on, fresh).1.into_iter().map(|r| r.key).collect::<Vec<_>>();
    assert!(keys(true).contains(&format!("{pol}/claims/c1")));
    assert!(!keys(false).contains(&format!("{pol}/claims/c1")));
    assert!(keys(false).contains(&format!("{pol}/policy")));
}

/// Purpose: on a family health policy each adult sees their own cover and their own health-implying rows; the policyholder sees another adult's only while that adult consents, and loses them the moment consent is withdrawn.
/// Why it matters: family privacy is a conduct and privacy obligation; a claim's existence implies health and must not reach the person who pays unless the claimant agrees.
#[test]
fn family_privacy_and_consent() {
    let mut w = World::new();
    let pol = w.issue(health_family(2, open(), &[d(1985, 1, 1), d(1986, 2, 2), d(2015, 3, 3)], Frequency::Monthly));
    let (holder, partner) = ("party-00000002-1", "party-00000002-2");
    assert_eq!(
        w.held(partner),
        [format!("{pol}/cover/hospital/2")],
        "an insured adult holds their own cover only"
    );
    assert_eq!(w.held(holder).len(), 6);

    // Claims publishes the partner's claim: a sensitive row about the partner.
    let claim = format!("{pol}/claims/c1");
    let row = Row {
        key: claim.clone(),
        policy: pol.clone(),
        kind: "claim".into(),
        subject: Some(p(partner)),
        sensitive: true,
        body: json!({ "state": "received" }),
    };
    let effects = w
        .send(
            module("claims"),
            Command::PublishRows {
                rows: vec![row.clone()],
                remove: vec![],
            },
        )
        .unwrap();
    assert_eq!(moved(&effects), [partner.to_string()].into(), "only the claimant's device moves");
    assert!(w.held(partner).contains(&claim));
    assert!(!w.held(holder).contains(&claim));
    assert!(!w.sees(holder, &pol).contains(&claim));

    // Only a person insured on the policy can consent, and only for themselves.
    let consent = |yes| DirectoryCommand::SetFamilyConsent {
        policy: pol.clone(),
        consent: yes,
    };
    assert_eq!(code(w.dir(party("stranger"), consent(true))), "not_insured");
    assert_eq!(code(w.dir(module("claims"), consent(true))), "forbidden");
    assert_eq!(code(w.dir(staff("ana"), consent(true))), "forbidden");

    let effects = w.dir(party(partner), consent(true)).unwrap();
    assert_eq!(moved(&effects), [holder.to_string()].into());
    assert_eq!(w.row(holder, &claim).unwrap()["state"], "received");

    // Withdrawn: the policyholder's device is told to purge it.
    w.dir(party(partner), consent(false)).unwrap();
    assert!(!w.held(holder).contains(&claim));
    assert_eq!(w.last_op(holder, &claim), Some(Op::Purge));
    assert!(w.held(partner).contains(&claim));

    // The claim is closed and removed: the claimant's device is told it is gone.
    w.send(
        module("claims"),
        Command::PublishRows {
            rows: vec![],
            remove: vec![claim.clone()],
        },
    )
    .unwrap();
    assert_eq!(w.last_op(partner, &claim), Some(Op::Delete));
    assert!(!w.ch.data.published.contains_key(&pol));
}

/// Purpose: a parent sees their child's health-implying rows until the child reaches the age the product sets; from the next
/// business date those rows are purged from the parent's device and the handover is announced; an adult child can still consent.
/// Why it matters: a parent has to manage a young child's claims, and a seventeen-year-old's are their own. Getting the date wrong
/// in either direction is a privacy breach or a parent locked out of their child's care.
#[test]
fn a_childs_health_information_becomes_their_own() {
    let mut w = World::new();
    let pol = w.issue(health_family(2, open(), &[d(1985, 1, 1), d(1986, 2, 2), d(2011, 11, 16)], Frequency::Monthly));
    let (holder, partner, child) = ("party-00000002-1", "party-00000002-2", "party-00000002-3");
    let claim = format!("{pol}/claims/c9");
    let row = Row {
        key: claim.clone(),
        policy: pol.clone(),
        kind: "claim".into(),
        subject: Some(p(child)),
        sensitive: true,
        body: json!({ "state": "received" }),
    };
    w.send(
        module("claims"),
        Command::PublishRows {
            rows: vec![row],
            remove: vec![],
        },
    )
    .unwrap();
    // Until the customer master says who is a child, nobody's health information is shown to anyone else.
    assert!(!w.held(holder).contains(&claim));

    let birthday = open().succ_opt().unwrap();
    let dependant = |who: &str, from| DirectoryCommand::RecordDependant {
        policy: pol.clone(),
        party: p(who),
        controls_from: from,
    };
    assert_eq!(code(w.dir(party(holder), dependant(child, birthday))), "forbidden");
    assert_eq!(code(w.dir(module("customer-master"), dependant("stranger", birthday))), "not_insured");
    assert_eq!(code(w.dir(module("customer-master"), dependant(holder, birthday))), "policyholder");
    let effects = w.dir(module("customer-master"), dependant(child, birthday)).unwrap();
    assert!(moved(&effects).contains(holder));
    assert_eq!(w.row(holder, &claim).unwrap()["state"], "received");
    // The other parent is insured, not the policyholder: the child's claim is not theirs to see.
    assert!(!w.held(partner).contains(&claim));

    // The day closes and the next opens: the child is of age.
    w.cluster.end_of_day().unwrap();
    let effects = w.relay();
    assert_eq!(w.ch.today(0), birthday);
    assert!(effects.contains(&Effect::ControlPassed {
        policy: pol.clone(),
        party: p(child),
        policyholder: Some(p(holder)),
    }));
    assert_eq!(w.last_op(holder, &claim), Some(Op::Purge));
    assert!(w.held(child).contains(&claim), "it is still theirs");
    // The handover is announced once, not on every later day.
    w.cluster.end_of_day().unwrap();
    assert!(!w.relay().iter().any(|e| matches!(e, Effect::ControlPassed { .. })));

    // Now an adult for this purpose, they may let the parent see it again.
    w.dir(
        party(child),
        DirectoryCommand::SetFamilyConsent {
            policy: pol.clone(),
            consent: true,
        },
    )
    .unwrap();
    assert!(w.held(holder).contains(&claim));
}

/// Purpose: the policyholder lets a relative look, an accountant see the money side, and a support person help; each sees what that
/// access is for, none can change anything, a support person's prepared request waits for the policyholder's own signature, and the
/// access ends when the policyholder says so or the policy changes hands.
/// Why it matters: people share sign-ins when there is no proper way to let someone help. This is the proper way, and it must not
/// become a way round the policyholder: no health information, no commands, no approving on their behalf.
#[test]
fn people_the_policyholder_lets_in() {
    let mut w = World::new();
    let pol = retail(&mut w);
    let let_in = |who: &str, access| DirectoryCommand::SetDelegate {
        policy: pol.clone(),
        party: p(who),
        access,
    };
    assert_eq!(code(w.dir(party("daughter"), let_in("daughter", Some(DelegateAccess::Viewer)))), "forbidden");
    assert_eq!(
        code(w.dir(module("customer-master"), let_in("daughter", Some(DelegateAccess::Viewer)))),
        "forbidden"
    );
    assert_eq!(code(w.dir(party(KIRI), let_in(KIRI, Some(DelegateAccess::Viewer)))), "policyholder");
    assert_eq!(code(w.dir(party(KIRI), let_in("daughter", None))), "not_authorised");

    // A sensitive row about Kiri, to show nobody let in sees it.
    let loading = format!("{pol}/underwriting/terms");
    w.send(
        module("underwriting"),
        Command::PublishRows {
            rows: vec![Row {
                key: loading.clone(),
                policy: pol.clone(),
                kind: "underwriting_terms".into(),
                subject: Some(p(KIRI)),
                sensitive: true,
                body: json!({ "loading": 50 }),
            }],
            remove: vec![],
        },
    )
    .unwrap();
    let all = w.sees(KIRI, &pol);
    assert!(all.contains(&loading));

    let effects = w.dir(party(KIRI), let_in("daughter", Some(DelegateAccess::Viewer))).unwrap();
    assert!(moved(&effects).contains("daughter"), "her device receives the policy");
    w.dir(party(KIRI), let_in("accountant", Some(DelegateAccess::Financial))).unwrap();
    w.dir(party(KIRI), let_in("moko", Some(DelegateAccess::Support))).unwrap();
    let on = w.ch.today(w.at);
    let relationship = |w: &World, who: &str| w.ch.data.directory.relationships(&w.ch.data.view, &p(who), &pol, on);
    assert_eq!(relationship(&w, "daughter"), [Relationship::Viewer]);
    assert_eq!(relationship(&w, "accountant"), [Relationship::FinancialViewer]);
    assert_eq!(relationship(&w, "moko"), [Relationship::SupportPerson]);

    // A viewer and a support person see everything but the sensitive row; the accountant sees the policy and the money only.
    let expected: Vec<String> = all.iter().filter(|k| **k != loading).cloned().collect();
    assert_eq!(w.sees("daughter", &pol), expected);
    assert_eq!(w.sees("moko", &pol), expected);
    assert_eq!(w.held("daughter"), expected);
    let (_, rows) = w.ch.visible_rows(&p("accountant"), &pol, on, true);
    let kinds: BTreeSet<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    assert!(
        !kinds.is_empty() && kinds.iter().all(|k| ["policy", "billing", "notice", "document"].contains(k)),
        "{kinds:?}"
    );

    // None of them can change anything.
    for who in ["daughter", "accountant"] {
        assert_eq!(
            code(w.send(party(who), submit("r-x", Some(&pol), "cancel_policy", "policy-servicing"))),
            "forbidden"
        );
    }
    assert_eq!(code(w.dir(party("moko"), let_in("friend", Some(DelegateAccess::Viewer)))), "forbidden");
    // The support person prepares; the step waits for Kiri's own signature and nobody else's.
    w.send(party("moko"), submit("r-help", Some(&pol), "change_frequency", "policy-servicing"))
        .unwrap();
    assert_eq!(
        w.ch.data.requests["r-help"].status,
        RequestStatus::AwaitingApproval { from: Approver::Customer }
    );
    let approve = |signed| Command::ApproveRequest {
        id: "r-help".into(),
        signed,
        route_to_person: false,
    };
    assert_eq!(code(w.send(party("moko"), approve(true))), "not_found");
    assert_eq!(code(w.send(party(KIRI), approve(false))), "step_up_required");
    w.send(party(KIRI), approve(true)).unwrap();

    // Access is changed and ended by the policyholder: the device is told to purge.
    w.dir(party(KIRI), let_in("daughter", Some(DelegateAccess::Financial))).unwrap();
    assert_eq!(relationship(&w, "daughter"), [Relationship::FinancialViewer]);
    let effects = w.dir(party(KIRI), let_in("daughter", None)).unwrap();
    assert_eq!(revoked(&effects), ["daughter".to_string()].into());
    assert!(w.held("daughter").is_empty() && w.sees("daughter", &pol).is_empty());

    // The policy changes hands: everyone the old owner let in is out.
    let effects = w
        .dir(
            module("customer-master"),
            DirectoryCommand::RecordOwnership {
                policy: pol.clone(),
                policyholder: p("new-owner"),
            },
        )
        .unwrap();
    assert!(revoked(&effects).contains("moko") && revoked(&effects).contains("accountant"));
    assert!(w.sees("moko", &pol).is_empty() && w.held("accountant").is_empty());
}

/// Purpose: a module publishes rows only under its own name, and only modules publish.
/// Why it matters: published rows reach customers' devices; one module overwriting another's rows, or the channel's own, would show customers facts nobody owns.
#[test]
fn modules_publish_only_their_own_rows() {
    let mut w = World::new();
    let pol = retail(&mut w);
    let row = |key: String| Row {
        key,
        policy: pol.clone(),
        kind: "claim".into(),
        subject: None,
        sensitive: false,
        body: json!({ "n": 1 }),
    };
    let publish = |rows, remove| Command::PublishRows { rows, remove };
    assert_eq!(code(w.send(party(KIRI), publish(vec![row(format!("{pol}/claims/c1"))], vec![]))), "forbidden");
    assert_eq!(code(w.send(module("claims"), publish(vec![row(format!("{pol}/policy"))], vec![]))), "bad_key");
    assert_eq!(
        code(w.send(module("claims"), publish(vec![row(format!("{pol}/commission/h1"))], vec![]))),
        "bad_key"
    );
    assert_eq!(
        code(w.send(module("claims"), publish(vec![row("pol-other/claims/c1".into())], vec![]))),
        "bad_key"
    );
    assert_eq!(code(w.send(module("claims"), publish(vec![], vec![format!("{pol}/commission/h1")]))), "bad_key");
    assert_eq!(code(w.send(module("claims"), publish(vec![], vec!["nokey".into()]))), "bad_key");
    // One bad key refuses the whole command.
    assert_eq!(
        code(w.send(
            module("claims"),
            publish(vec![row(format!("{pol}/claims/ok")), row(format!("{pol}/billing"))], vec![])
        )),
        "bad_key"
    );
    assert!(w.ch.data.published.is_empty());

    w.send(
        module("claims"),
        publish(vec![row(format!("{pol}/claims/c1")), row(format!("{pol}/claims/c2"))], vec![]),
    )
    .unwrap();
    assert!(w.held(KIRI).contains(&format!("{pol}/claims/c1")));
    // Removing a row that is not there is harmless; removing one of two keeps the other.
    w.send(module("claims"), publish(vec![], vec![format!("{pol}/claims/c1"), format!("{pol}/claims/zz")]))
        .unwrap();
    assert_eq!(w.ch.data.published[&pol].len(), 1);
    // Rows for a policy the channel has not heard of are kept until it is.
    let early = Row {
        policy: PolicyId::from("pol-later"),
        key: "pol-later/claims/c9".into(),
        ..row(String::new())
    };
    assert!(w.send(module("claims"), publish(vec![early], vec![])).unwrap().is_empty());
}

/// Purpose: on a group scheme a member's phone holds only that member's cover, the employer is served by the portal with scheme-level rows, and a change to one member touches one feed.
/// Why it matters: a scheme can hold tens of thousands of members; nobody may see a colleague's cover, and one member's change must not rebuild every feed.
#[test]
fn scheme_members_and_the_employer() {
    let mut w = World::new();
    let pol = w.issue(group_scheme(
        3,
        open(),
        vec![linked_member("m-1", "worker-1"), member("m-2", d(1981, 1, 1), "staff", 90_000_00)],
    ));
    let employer = "employer-00000003";
    assert_eq!(w.held("worker-1"), [format!("{pol}/member/m-1/group-life")]);
    assert!(w.feed(employer).is_none(), "the employer uses the portal, not a device feed");
    assert_eq!(w.ch.data.audiences[&pol], [p("worker-1")].into());

    // Group administration names the first administrator.
    let admin = |who: &str, yes| DirectoryCommand::SetEmployerAdmin {
        policy: pol.clone(),
        party: p(who),
        admin: yes,
    };
    assert_eq!(code(w.dir(party("hr-1"), admin("hr-1", true))), "forbidden");
    assert_eq!(
        code(w.dir(
            module("group-admin"),
            DirectoryCommand::SetEmployerAdmin {
                policy: PolicyId::from("nope"),
                party: p("hr-1"),
                admin: true
            }
        )),
        "unknown_policy"
    );
    w.dir(module("group-admin"), admin("hr-1", true)).unwrap();
    let on = w.ch.today(w.at);
    let (relationships, rows) = w.ch.visible_rows(&p("hr-1"), &pol, on, true);
    assert_eq!(relationships, [Relationship::EmployerAdmin]);
    let kinds: BTreeSet<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["billing", "member", "scheme"].into());
    let scheme = rows.iter().find(|r| r.kind == "scheme").unwrap();
    assert_eq!(scheme.body["members"], 2);
    assert_eq!(rows.iter().filter(|r| r.kind == "member").count(), 2);
    assert!(w.feed("hr-1").is_none());

    // Administrators manage their own list, and a scheme keeps at least one.
    assert_eq!(code(w.dir(party("hr-1"), admin("hr-1", false))), "last_admin");
    w.dir(party("hr-1"), admin("hr-2", true)).unwrap();
    let effects = w.dir(party("hr-2"), admin("hr-1", false)).unwrap();
    assert_eq!(revoked(&effects), ["hr-1".to_string()].into());
    assert!(w.sees("hr-1", &pol).is_empty());
    assert!(!w.ch.data.directory.admin_policies[&p("hr-1")].contains(&pol));
    // Group administration can remove even the last one, when an employer leaves.
    w.dir(party("hr-2"), admin("hr-1", true)).unwrap();

    // A new member joins from the membership file. Members are unnamed until
    // identified, so no device moves; the employer's portal shows three.
    w.core(
        &pol,
        "group-admin",
        ic_core::Command::ApplyMembership {
            policy: pol.clone(),
            changes: vec![MemberChange::Joined {
                life: member("m-3", d(1990, 5, 5), "staff", 70_000_00),
                effective: open(),
            }],
        },
    );
    let effects = w.relay();
    assert!(moved(&effects).is_empty());
    let (_, rows) = w.ch.visible_rows(&p("hr-2"), &pol, on, true);
    assert_eq!(rows.iter().find(|r| r.kind == "scheme").unwrap().body["members"], 3);
    w.core(
        &pol,
        "group-admin",
        ic_core::Command::IdentifyMember {
            policy: pol.clone(),
            life: LifeId::from("m-3"),
            party: p("worker-3"),
        },
    );
    let effects = w.relay();
    assert_eq!(moved(&effects), ["worker-3".to_string()].into(), "only that person's feed moves");
    assert_eq!(w.held("worker-3"), [format!("{pol}/member/m-3/group-life")]);
    assert_eq!(w.held("worker-1").len(), 1);

    // An unnamed member is identified at claim: their own feed starts.
    w.core(
        &pol,
        "claims",
        ic_core::Command::IdentifyMember {
            policy: pol.clone(),
            life: LifeId::from("m-2"),
            party: p("worker-2"),
        },
    );
    let effects = w.relay();
    assert_eq!(moved(&effects), ["worker-2".to_string()].into());
    assert_eq!(w.held("worker-2"), [format!("{pol}/member/m-2/group-life")]);
    assert_eq!(
        w.ch.data.directory.relationships(&w.ch.data.view, &p("worker-2"), &pol, on),
        [Relationship::Member]
    );

    // A member leaves: their cover goes from their device.
    w.core(
        &pol,
        "group-admin",
        ic_core::Command::ApplyMembership {
            policy: pol.clone(),
            changes: vec![MemberChange::Left {
                life: LifeId::from("m-3"),
                effective: open(),
            }],
        },
    );
    w.relay();
    assert!(w
        .row("worker-3", &format!("{pol}/member/m-3/group-life"))
        .is_none_or(|b| b["in_force"] == false));
}

/// Purpose: a scheme can require a second administrator for named kinds of change; the one who asked cannot be the one who approves.
/// Why it matters: removing members or changing salaries in bulk alters cover and premium for other people; the employer's own two-person control must hold in the platform.
#[test]
fn employer_changes_can_need_a_second_administrator() {
    let mut w = World::new();
    let pol = w.issue(group_scheme(3, open(), vec![linked_member("m-1", "worker-1")]));
    for who in ["hr-1", "hr-2"] {
        w.dir(
            module("group-admin"),
            DirectoryCommand::SetEmployerAdmin {
                policy: pol.clone(),
                party: p(who),
                admin: true,
            },
        )
        .unwrap();
    }
    let need = DirectoryCommand::RequireSecondAdmin {
        policy: pol.clone(),
        changes: ["remove_member".to_string()].into(),
    };
    assert_eq!(code(w.dir(party("worker-1"), need.clone())), "forbidden");
    w.dir(party("hr-1"), need.clone()).unwrap();
    w.dir(module("group-admin"), need).unwrap();

    // An ordinary change goes straight to the owning module.
    let effects = w.send(party("hr-1"), submit("r1", Some(&pol), "add_member", "group-admin")).unwrap();
    assert!(effects.contains(&Effect::RequestReady {
        id: "r1".into(),
        module: "group-admin".into()
    }));

    // A controlled one waits for the second administrator.
    let effects = w.send(party("hr-1"), submit("r2", Some(&pol), "remove_member", "group-admin")).unwrap();
    assert!(!effects.iter().any(|e| matches!(e, Effect::RequestReady { .. })));
    assert_eq!(
        w.ch.data.requests["r2"].status,
        RequestStatus::AwaitingApproval {
            from: Approver::SecondAdministrator
        }
    );
    // The second administrator's own circumstances do not change how a scheme's request is handled.
    let approve = |id: &str| Command::ApproveRequest {
        id: id.to_string(),
        signed: false,
        route_to_person: true,
    };
    assert_eq!(
        code(w.send(party("hr-1"), approve("r2"))),
        "not_found",
        "the administrator who asked cannot approve"
    );
    assert_eq!(code(w.send(party("worker-1"), approve("r2"))), "not_found");
    assert_eq!(code(w.send(module("group-admin"), approve("r2"))), "forbidden");
    let effects = w.send(party("hr-2"), approve("r2")).unwrap();
    assert!(effects.contains(&Effect::RequestReady {
        id: "r2".into(),
        module: "group-admin".into()
    }));
    assert_eq!(w.ch.data.requests["r2"].approved_by, Some(p("hr-2")));
    assert!(!w.ch.data.requests["r2"].route_to_person);
    assert_eq!(code(w.send(party("hr-2"), approve("r2"))), "not_awaiting");
    assert_eq!(code(w.send(party("hr-2"), approve("zz"))), "not_found");

    // A member acts on their own matters without approval.
    let effects = w.send(party("worker-1"), submit("r3", Some(&pol), "notify_claim", "claims")).unwrap();
    assert!(effects.contains(&Effect::RequestReady {
        id: "r3".into(),
        module: "claims".into()
    }));
    assert_eq!(w.ch.pending_for("group-admin").iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["r1", "r2"]);
    assert_eq!(w.ch.pending_for("claims").len(), 1);
}

/// Purpose: an adviser the customer authorised sees the policy and prepares changes the customer approves with a signature; the customer can end the access at once.
/// Why it matters: adviser access exists only by the customer's authority, and an adviser must never be able to change a policy without the customer's own signed approval.
/// A request an adviser prepares is routed by the customer who approves it, so a vulnerable customer reaches a person whoever typed it.
#[test]
fn an_adviser_acts_by_the_customers_authority() {
    let mut w = World::new();
    let pol = retail(&mut w);
    w.engage("e1", "adv-1", "fap-1", None).unwrap();
    let grant = |adviser: &str, engagement: &str| DirectoryCommand::GrantAdviserAuthority {
        policy: pol.clone(),
        adviser: p(adviser),
        engagement: engagement.to_string(),
    };
    assert_eq!(
        code(w.dir(party("adv-1"), grant("adv-1", "e1"))),
        "forbidden",
        "an adviser cannot authorise themselves"
    );
    assert_eq!(code(w.dir(module("distribution"), grant("adv-1", "e1"))), "forbidden");
    assert_eq!(code(w.dir(party(KIRI), grant("adv-1", "e9"))), "unknown_engagement");
    assert_eq!(
        code(w.dir(party(KIRI), grant("adv-2", "e1"))),
        "unknown_engagement",
        "access belongs to the engagement's adviser"
    );
    assert!(w.sees("adv-1", &pol).is_empty());

    let effects = w.dir(party(KIRI), grant("adv-1", "e1")).unwrap();
    assert_eq!(moved(&effects), [KIRI.to_string()].into());
    assert_eq!(
        w.row(KIRI, &format!("{pol}/adviser/adv-1")).unwrap(),
        json!({ "adviser": "adv-1", "state": "available" })
    );
    assert_eq!(w.sees("adv-1", &pol).len(), 6);
    assert!(w.feed("adv-1").is_none(), "advisers use the portal");
    assert!(w.ch.data.directory.adviser_policies[&p("adv-1")].contains(&pol));

    // The adviser prepares; the customer approves with their device's signature.
    w.send(party("adv-1"), submit("r1", Some(&pol), "decline_indexation", "policy-servicing"))
        .unwrap();
    assert_eq!(w.ch.data.requests["r1"].status, RequestStatus::AwaitingApproval { from: Approver::Customer });
    assert_eq!(w.row(KIRI, &format!("{pol}/request/r1")).unwrap()["state"]["status"], "awaiting_approval");
    // Kiri needs a person to handle her changes: the adviser's preparing it does not change that.
    let approve = |signed| Command::ApproveRequest {
        id: "r1".into(),
        signed,
        route_to_person: true,
    };
    assert!(!w.ch.data.requests["r1"].route_to_person);
    assert_eq!(code(w.send(party("adv-1"), approve(true))), "not_found");
    assert_eq!(code(w.send(party(KIRI), approve(false))), "step_up_required");
    let effects = w.send(party(KIRI), approve(true)).unwrap();
    assert!(effects.contains(&Effect::RequestReady {
        id: "r1".into(),
        module: "policy-servicing".into()
    }));
    assert!(w.ch.data.requests["r1"].signed);
    assert!(w.ch.data.requests["r1"].route_to_person, "routed as the customer's");

    // The owning module resolves it; the customer's device shows the outcome.
    let resolve = |id: &str, rejected: Option<(&str, &str)>| Command::ResolveRequest {
        id: id.to_string(),
        reference: Some("core-123".into()),
        rejected: rejected.map(|(c, m)| (c.to_string(), m.to_string())),
    };
    assert_eq!(code(w.send(module("claims"), resolve("r1", None))), "forbidden");
    assert_eq!(code(w.send(party(KIRI), resolve("r1", None))), "forbidden");
    assert_eq!(code(w.send(module("policy-servicing"), resolve("zz", None))), "not_found");
    w.send(module("policy-servicing"), resolve("r1", None)).unwrap();
    assert_eq!(
        w.row(KIRI, &format!("{pol}/request/r1")).unwrap()["state"],
        json!({ "status": "applied", "reference": "core-123" })
    );
    assert_eq!(code(w.send(module("policy-servicing"), resolve("r1", None))), "not_pending");

    // The customer ends the adviser's access. It takes effect at once.
    let withdraw = DirectoryCommand::WithdrawAdviserAuthority {
        policy: pol.clone(),
        adviser: p("adv-1"),
    };
    assert_eq!(code(w.dir(party("adv-1"), withdraw.clone())), "forbidden");
    let effects = w.dir(party(KIRI), withdraw.clone()).unwrap();
    assert_eq!(revoked(&effects), ["adv-1".to_string()].into());
    assert!(w.sees("adv-1", &pol).is_empty());
    assert_eq!(w.last_op(KIRI, &format!("{pol}/adviser/adv-1")), Some(Op::Delete));
    assert_eq!(code(w.dir(party(KIRI), withdraw)), "not_authorised");
    assert_eq!(code(w.send(party("adv-1"), submit("r2", Some(&pol), "x", "policy-servicing"))), "not_found");
}

/// Purpose: when a licence is lost anywhere above an adviser, the adviser becomes read-only that instant, the customer is told the adviser is unavailable with a route to the insurer, and restoring the licence restores service.
/// Why it matters: an unlicensed adviser must not transact (addendum entry 4), customers must not be left without a route to service, and the cascade must reach every adviser under the body that lost its licence.
#[test]
fn a_lost_licence_makes_the_adviser_read_only() {
    let mut w = World::new();
    let pol = retail(&mut w);
    w.engage("e1", "adv-1", "fap-1", None).unwrap();
    w.engage("e2", "adv-2", "fap-2", None).unwrap();
    // fap-1 operates under group-1, which operates under holding-1.
    for (child, parent) in [("fap-1", "group-1"), ("group-1", "holding-1")] {
        w.dir(
            module("distribution"),
            DirectoryCommand::RecordStructure {
                child: p(child),
                link: StructureLink {
                    parent: p(parent),
                    from: d(2025, 1, 1),
                    to: None,
                },
            },
        )
        .unwrap();
    }
    w.dir(
        party(KIRI),
        DirectoryCommand::GrantAdviserAuthority {
            policy: pol.clone(),
            adviser: p("adv-1"),
            engagement: "e1".into(),
        },
    )
    .unwrap();
    let licence = |who: &str, status, effective| DirectoryCommand::RecordLicence {
        party: p(who),
        status,
        effective,
        source: "FMA notice 2027/118".into(),
    };
    assert_eq!(code(w.dir(party("adv-1"), licence("holding-1", LicenceStatus::Active, open()))), "forbidden");
    let unsourced = DirectoryCommand::RecordLicence {
        party: p("holding-1"),
        status: LicenceStatus::Revoked,
        effective: open(),
        source: " ".into(),
    };
    assert_eq!(code(w.dir(module("compliance"), unsourced)), "no_source");

    // The top of the structure loses its licence.
    let effects = w.dir(module("compliance"), licence("holding-1", LicenceStatus::Revoked, open())).unwrap();
    assert_eq!(revoked(&effects), ["adv-1".to_string()].into(), "adv-2 is under another provider");
    assert_eq!(moved(&effects), [KIRI.to_string()].into());
    assert_eq!(
        w.ch.data.directory.standing("e1", open()),
        AdviserStanding::ReadOnly {
            since: open(),
            licence_of: p("holding-1"),
            status: LicenceStatus::Revoked
        }
    );
    assert_eq!(w.ch.data.directory.standing("e2", open()), AdviserStanding::Active);
    assert_eq!(
        w.ch.data.directory.standing("e1", d(2027, 11, 14)),
        AdviserStanding::Active,
        "before the effective date"
    );

    // The customer is told, with a direct route to the insurer.
    assert_eq!(
        w.row(KIRI, &format!("{pol}/adviser/adv-1")).unwrap(),
        json!({ "adviser": "adv-1", "state": "unavailable", "since": "2027-11-15", "contact": "insurer" })
    );
    // The adviser still sees the book and can change nothing.
    assert_eq!(w.sees("adv-1", &pol).len(), 6);
    assert_eq!(
        code(w.send(party("adv-1"), submit("r1", Some(&pol), "decline_indexation", "policy-servicing"))),
        "adviser_read_only"
    );
    // The customer can still act for themselves.
    w.send(party(KIRI), submit("r2", Some(&pol), "decline_indexation", "policy-servicing")).unwrap();

    // The licence is restored from tomorrow: read-only today, active then.
    w.dir(module("compliance"), licence("holding-1", LicenceStatus::Active, d(2027, 11, 16)))
        .unwrap();
    assert!(matches!(w.ch.data.directory.standing("e1", open()), AdviserStanding::ReadOnly { .. }));
    assert_eq!(w.ch.data.directory.standing("e1", d(2027, 11, 16)), AdviserStanding::Active);

    // The adviser's own licence is suspended earlier than a later lapse above: the earliest loss is reported.
    w.dir(module("distribution"), licence("adv-1", LicenceStatus::Suspended, d(2027, 12, 1)))
        .unwrap();
    w.dir(module("compliance"), licence("fap-1", LicenceStatus::Lapsed, d(2027, 12, 10))).unwrap();
    assert_eq!(
        w.ch.data.directory.standing("e1", d(2027, 12, 20)),
        AdviserStanding::ReadOnly {
            since: d(2027, 12, 1),
            licence_of: p("adv-1"),
            status: LicenceStatus::Suspended
        }
    );
    assert!(w.ch.data.directory.licence_on(&p("fap-1"), d(2027, 12, 9)).is_none());
    assert!(w.ch.data.directory.licence_on(&p("nobody"), open()).is_none(), "no record means licensed");
}

/// Purpose: an engagement's end ends the adviser's access with it; the structure above an adviser is dated; and bad distribution data is refused.
/// Why it matters: access belongs to the engagement, not the adviser or the advice business, so a move between providers must not carry the old book along.
#[test]
fn access_ends_with_the_engagement() {
    let mut w = World::new();
    let pol = retail(&mut w);
    w.engage("e1", "adv-1", "fap-1", None).unwrap();
    assert_eq!(code(w.engage("e1", "adv-2", "fap-1", None)), "engagement_reused");
    assert_eq!(code(w.engage("e3", "adv-3", "fap-1", Some(d(2024, 1, 1)))), "bad_period");
    assert_eq!(
        code(w.dir(
            party("adv-1"),
            DirectoryCommand::RecordEngagement {
                engagement: w.ch.data.directory.engagements["e1"].clone()
            }
        )),
        "forbidden"
    );
    let link = |child: &str, parent: &str, to| DirectoryCommand::RecordStructure {
        child: p(child),
        link: StructureLink {
            parent: p(parent),
            from: d(2025, 1, 1),
            to,
        },
    };
    assert_eq!(code(w.dir(module("distribution"), link("fap-1", "fap-1", None))), "bad_structure");
    assert_eq!(code(w.dir(module("compliance"), link("fap-1", "group-1", None))), "forbidden");
    // A cycle entered in error does not hang the walk.
    w.dir(module("distribution"), link("fap-1", "group-1", None)).unwrap();
    w.dir(module("distribution"), link("group-1", "fap-1", None)).unwrap();
    assert_eq!(w.ch.data.directory.standing("e1", open()), AdviserStanding::Active);
    // The link to group-1 ended last year: a loss there no longer cascades.
    w.dir(module("distribution"), link("fap-1", "group-1", Some(d(2026, 1, 1)))).unwrap();
    assert_eq!(w.ch.data.directory.structure[&p("fap-1")].len(), 1, "the same link recorded again replaces it");
    w.dir(
        module("compliance"),
        DirectoryCommand::RecordLicence {
            party: p("group-1"),
            status: LicenceStatus::Revoked,
            effective: d(2027, 1, 1),
            source: "FMA".into(),
        },
    )
    .unwrap();
    assert_eq!(w.ch.data.directory.standing("e1", open()), AdviserStanding::Active);
    assert!(matches!(w.ch.data.directory.standing("e1", d(2025, 6, 1)), AdviserStanding::Active));

    w.dir(
        party(KIRI),
        DirectoryCommand::GrantAdviserAuthority {
            policy: pol.clone(),
            adviser: p("adv-1"),
            engagement: "e1".into(),
        },
    )
    .unwrap();
    assert_eq!(w.ch.data.directory.advisers_on(&pol, open()).len(), 1);

    // The engagement ends today.
    let effects = w.engage("e1", "adv-1", "fap-1", Some(open())).unwrap();
    assert_eq!(revoked(&effects), ["adv-1".to_string()].into());
    assert_eq!(w.ch.data.directory.standing("e1", open()), AdviserStanding::Ended);
    assert_eq!(w.ch.data.directory.standing("nope", open()), AdviserStanding::Ended);
    assert!(w.sees("adv-1", &pol).is_empty());
    assert!(w.row(KIRI, &format!("{pol}/adviser/adv-1")).is_none());
    assert_eq!(code(w.send(party("adv-1"), submit("r1", Some(&pol), "x", "policy-servicing"))), "not_found");
}

/// Purpose: when ownership changes, the previous owner's device is purged, their adviser's authority ends, and a lender with an assignment sees only that the policy exists.
/// Why it matters: authority was the previous owner's to give; leaving it or their data in place after a transfer of ownership discloses the new owner's affairs.
#[test]
fn ownership_change_and_assignment() {
    let mut w = World::new();
    let pol = retail(&mut w);
    w.engage("e1", "adv-1", "fap-1", None).unwrap();
    w.dir(
        party(KIRI),
        DirectoryCommand::GrantAdviserAuthority {
            policy: pol.clone(),
            adviser: p("adv-1"),
            engagement: "e1".into(),
        },
    )
    .unwrap();

    // A lender's interest is recorded.
    let assign = |yes| DirectoryCommand::RecordAssignment {
        policy: pol.clone(),
        party: p("bank-1"),
        active: yes,
    };
    assert_eq!(code(w.dir(party(KIRI), assign(true))), "forbidden");
    w.dir(module("policy-servicing"), assign(true)).unwrap();
    assert_eq!(w.held("bank-1"), [format!("{pol}/policy")]);
    assert_eq!(code(w.send(party("bank-1"), submit("r1", Some(&pol), "x", "policy-servicing"))), "forbidden");

    // Ownership moves to a trust.
    let own = |policy: &PolicyId, to: &str| DirectoryCommand::RecordOwnership {
        policy: policy.clone(),
        policyholder: p(to),
    };
    assert_eq!(code(w.dir(party(KIRI), own(&pol, "trust-1"))), "forbidden");
    assert_eq!(
        code(w.dir(module("customer-master"), own(&PolicyId::from("nope"), "trust-1"))),
        "unknown_policy"
    );
    let effects = w.dir(module("customer-master"), own(&pol, "trust-1")).unwrap();
    assert_eq!(revoked(&effects), [KIRI.to_string(), "adv-1".to_string()].into());
    assert!(w.sees("adv-1", &pol).is_empty());
    assert!(w.ch.data.directory.adviser_policies[&p("adv-1")].is_empty());
    // Kiri is still the life insured: she keeps her own cover and loses the rest.
    assert_eq!(w.held(KIRI), [format!("{pol}/cover/life/1"), format!("{pol}/cover/trauma/1")]);
    assert_eq!(w.last_op(KIRI, &format!("{pol}/billing")), Some(Op::Purge));
    assert_eq!(w.held("trust-1").len(), 5);

    // The assignment is released: the lender's device is purged.
    let effects = w.dir(module("customer-master"), assign(false)).unwrap();
    assert_eq!(revoked(&effects), ["bank-1".to_string()].into());
    assert!(w.held("bank-1").is_empty());
    assert_eq!(w.last_op("bank-1", &format!("{pol}/policy")), Some(Op::Purge));
    assert_eq!(w.ch.data.audiences[&pol], [p(KIRI), p("trust-1")].into());
}

/// Purpose: a sign-in subject is bound to one party by the identity service, and unbinding closes that party's sessions.
/// Why it matters: this binding is what turns a token into a customer; rebinding a subject to another party would hand over that party's policies.
#[test]
fn subjects_bind_to_one_party() {
    let mut w = World::new();
    let bind = |subject: &str, who: &str| DirectoryCommand::BindSubject {
        subject: subject.to_string(),
        party: p(who),
    };
    assert_eq!(code(w.dir(party(KIRI), bind("sub-1", KIRI))), "forbidden");
    w.dir(module("identity"), bind("sub-1", KIRI)).unwrap();
    w.dir(module("customer-master"), bind("sub-1", KIRI)).unwrap();
    assert_eq!(code(w.dir(module("identity"), bind("sub-1", "mallory"))), "already_bound");
    assert_eq!(w.ch.data.directory.party_of_subject("sub-1"), Some(&p(KIRI)));
    assert_eq!(
        code(w.dir(staff("ana"), DirectoryCommand::UnbindSubject { subject: "sub-1".into() })),
        "forbidden"
    );
    let effects = w.dir(module("identity"), DirectoryCommand::UnbindSubject { subject: "sub-1".into() }).unwrap();
    assert_eq!(revoked(&effects), [KIRI.to_string()].into());
    assert!(w
        .dir(module("identity"), DirectoryCommand::UnbindSubject { subject: "sub-1".into() })
        .unwrap()
        .is_empty());
    assert!(w.ch.data.directory.party_of_subject("sub-1").is_none());
}

/// Purpose: requests are recorded, shown as pending and resolved later; who may submit, cancel and resolve is enforced, and a refusal reads the same as "no such policy".
/// Why it matters: writes are asynchronous by contract, so the request record is the customer's only view of what they asked for; and an error that distinguishes "exists but not yours" leaks who is insured.
#[test]
fn requests_are_asynchronous_and_owned() {
    let mut w = World::new();
    let pol = retail(&mut w);

    assert_eq!(code(w.send(module("claims"), submit("r0", Some(&pol), "x", "claims"))), "forbidden");
    assert_eq!(code(w.send(party("stranger"), submit("r0", Some(&pol), "x", "claims"))), "not_found");
    assert_eq!(
        code(w.send(party(KIRI), submit("r0", Some(&PolicyId::from("nope")), "x", "claims"))),
        "not_found"
    );
    assert_eq!(code(w.send(party(KIRI), submit("r0", Some(&pol), " ", "claims"))), "bad_request");
    assert_eq!(code(w.send(party(KIRI), submit("r0", Some(&pol), "x", ""))), "bad_request");
    assert_eq!(code(w.send(staff("ana"), submit("r0", None, "x", "claims"))), "bad_request");
    assert!(w.ch.data.requests.is_empty());

    // The policyholder's own request goes straight to the module.
    let effects = w.send(party(KIRI), submit("r1", Some(&pol), "cancel_policy", "policy-servicing")).unwrap();
    assert!(effects.contains(&Effect::RequestReady {
        id: "r1".into(),
        module: "policy-servicing".into()
    }));
    assert_eq!(w.row(KIRI, &format!("{pol}/request/r1")).unwrap()["state"]["status"], "pending");
    assert_eq!(w.ch.data.requests["r1"].submitted, w.at);
    assert_eq!(
        code(w.send(party(KIRI), submit("r1", Some(&pol), "cancel_policy", "policy-servicing"))),
        "duplicate"
    );

    // The module refuses it, with a reason the customer sees.
    w.send(
        module("policy-servicing"),
        Command::ResolveRequest {
            id: "r1".into(),
            reference: None,
            rejected: Some(("claim_open".into(), "a claim is open on this policy".into())),
        },
    )
    .unwrap();
    assert_eq!(
        w.row(KIRI, &format!("{pol}/request/r1")).unwrap()["state"],
        json!({ "status": "rejected", "code": "claim_open", "message": "a claim is open on this policy" })
    );
    let cancel = |id: &str| Command::CancelRequest { id: id.to_string() };
    assert_eq!(code(w.send(party(KIRI), cancel("r1"))), "already_resolved");

    // A request about no policy (a new quote, say) is the requester's alone.
    w.send(party("prospect"), submit("r2", None, "quote", "new-business")).unwrap();
    assert_eq!(code(w.send(party(KIRI), cancel("r2"))), "not_found");
    assert_eq!(code(w.send(party(KIRI), cancel("zz"))), "not_found");
    assert!(w.send(party("prospect"), cancel("r2")).unwrap().is_empty());
    assert_eq!(w.ch.data.requests["r2"].status, RequestStatus::Cancelled);
    // The gateway resolves what it dispatched itself.
    w.send(party("prospect"), submit("r3", None, "quote", "new-business")).unwrap();
    w.send(
        module(GATEWAY),
        Command::ResolveRequest {
            id: "r3".into(),
            reference: None,
            rejected: None,
        },
    )
    .unwrap();
    assert_eq!(w.ch.data.requests["r3"].status, RequestStatus::Applied { reference: None });

    // Staff act for a customer on the phone; the customer approves in the app, or cancels.
    w.send(staff("ana"), submit("r4", Some(&pol), "change_frequency", "policy-servicing")).unwrap();
    assert_eq!(w.ch.data.requests["r4"].status, RequestStatus::AwaitingApproval { from: Approver::Customer });
    assert_eq!(code(w.send(staff("ben"), cancel("r4"))), "not_found");
    w.send(party(KIRI), cancel("r4")).unwrap();
    assert_eq!(w.row(KIRI, &format!("{pol}/request/r4")).unwrap()["state"]["status"], "cancelled");
    w.send(staff("ana"), submit("r5", Some(&pol), "change_frequency", "policy-servicing")).unwrap();
    w.send(staff("ana"), cancel("r5")).unwrap();
    assert!(w.ch.pending_for("policy-servicing").is_empty());
}

/// Purpose: device and staff commands reach their state machines only from the right caller, and their revocations reach open sessions.
/// Why it matters: the envelope's actor is the only identity the state machine has; a customer able to send identity-service or entitlement commands could enrol a device for someone else or grant themselves staff access.
#[test]
fn identity_and_staff_commands_are_gated_by_caller() {
    use ic_channels::identity::{action_hash, challenge_message, Enrolment, IdentityCommand, IdentityEvent};
    use ic_channels::staff::{Profile, StaffCommand, StaffEvent, Template};
    let mut w = World::new();
    let phone = Key::new();
    let enrol = |device: &str, who: &str, key: &Key, enrolment| {
        Command::Identity(IdentityCommand::EnrolDevice {
            device: device.to_string(),
            party: p(who),
            public_key: key.public.clone(),
            hardware_backed: true,
            enrolment,
        })
    };
    let verified = || Enrolment::Verification { reference: "idv-1".into() };
    // A person cannot enrol their own first device: that follows identity verification.
    assert_eq!(code(w.send(party(KIRI), enrol("phone", KIRI, &phone, verified()))), "forbidden");
    assert_eq!(code(w.send(module("claims"), enrol("phone", KIRI, &phone, verified()))), "forbidden");
    w.send(module("identity"), enrol("phone", KIRI, &phone, verified())).unwrap();

    // They add a second from the first, for themselves only.
    let laptop = Key::new();
    let by_phone = || Enrolment::ExistingDevice {
        device: "phone".into(),
        signature: phone.sign(format!("enrol\nlaptop\n{}", laptop.public).as_bytes()),
    };
    assert_eq!(code(w.send(party("mallory"), enrol("laptop", KIRI, &laptop, by_phone()))), "forbidden");
    w.send(party(KIRI), enrol("laptop", KIRI, &laptop, by_phone())).unwrap();

    // Challenges are issued and redeemed for one's own device.
    let hash = action_hash(&json!({ "kind": "approve", "request": "r1" }));
    let issue = Command::Identity(IdentityCommand::IssueChallenge {
        id: "c1".into(),
        device: "phone".into(),
        action_hash: hash.clone(),
        nonce: "0123456789abcdef".into(),
        ttl_secs: 60,
    });
    assert_eq!(code(w.send(party("mallory"), issue.clone())), "forbidden");
    let effects = w.send(party(KIRI), issue).unwrap();
    let Effect::Identity(IdentityEvent::ChallengeIssued { challenge }) = &effects[0] else {
        panic!("expected a challenge")
    };
    let redeem = Command::Identity(IdentityCommand::RedeemChallenge {
        id: "c1".into(),
        action_hash: hash,
        signature: phone.sign(&challenge_message(challenge)),
    });
    assert_eq!(code(w.send(party("mallory"), redeem.clone())), "forbidden");
    let effects = w.send(party(KIRI), redeem.clone()).unwrap();
    assert!(matches!(&effects[0], Effect::Identity(IdentityEvent::ActionSigned { .. })));
    assert_eq!(code(w.send(party(KIRI), redeem)), "forbidden", "a used challenge is nobody's");

    // Revocation: one's own device, or everything by the identity service.
    let revoke = |device: &str| Command::Identity(IdentityCommand::RevokeDevice { device: device.to_string() });
    assert_eq!(code(w.send(party("mallory"), revoke("phone"))), "forbidden");
    let effects = w.send(party(KIRI), revoke("laptop")).unwrap();
    assert_eq!(revoked(&effects), [KIRI.to_string()].into());
    let all = Command::Identity(IdentityCommand::RevokeSessions { party: p(KIRI) });
    assert_eq!(code(w.send(party(KIRI), all.clone())), "forbidden");
    assert_eq!(code(w.send(staff("ana"), all.clone())), "forbidden");
    assert_eq!(revoked(&w.send(module("identity"), all).unwrap()), [KIRI.to_string()].into());
    // The identity service's own rejections pass through.
    assert_eq!(code(w.send(module("identity"), revoke("laptop"))), "unknown_device");

    // Staff: set up by the entitlements job, then run by staff.
    let bootstrap = Command::Staff(StaffCommand::Bootstrap {
        templates: vec![Template {
            id: "administrator".into(),
            actions: [ic_channels::staff::actions::MANAGE_STAFF.to_string()].into(),
            classes: Default::default(),
            default_profile: "none".into(),
            ceilings: Default::default(),
            break_glass: false,
        }],
        profiles: vec![Profile {
            id: "none".into(),
            version: 1,
            limits: Default::default(),
        }],
        admin: p("alice"),
        admin_templates: vec!["administrator".into()],
    });
    assert_eq!(code(w.send(party(KIRI), bootstrap.clone())), "forbidden");
    assert_eq!(code(w.send(module("claims"), bootstrap.clone())), "forbidden");
    w.send(module("entitlements"), bootstrap.clone()).unwrap();
    assert_eq!(code(w.send(module("entitlements"), bootstrap)), "already_bootstrapped");
    let employ = Command::Staff(StaffCommand::Employ {
        party: p("ana"),
        team: "claims".into(),
        manager: None,
        templates: vec![],
    });
    assert_eq!(
        code(w.send(module("entitlements"), employ.clone())),
        "forbidden",
        "the module sends only what an entitlement source sends"
    );
    w.send(staff("alice"), employ).unwrap();
    let effects = w.send(staff("alice"), Command::Staff(StaffCommand::EndEmployment { party: p("ana") })).unwrap();
    assert_eq!(revoked(&effects), ["ana".to_string()].into());
    assert!(effects.iter().any(|e| matches!(e, Effect::Staff(StaffEvent::SessionsRevoked { .. }))));
}

/// Purpose: the state is a pure function of its log: two replicas agree on the hash, a saved state restores to the same hash, a repeated key changes nothing, and a rejected command is not remembered.
/// Why it matters: the module runs as a replicated group; any divergence between members, or a retry applied twice, corrupts what customers see and cannot be reconciled afterwards.
#[test]
fn replicas_agree_and_retries_are_safe() {
    use ic_channels::state::Output;
    use ic_channels::{ChannelState, Envelope};
    use ic_log::{MemoryLog, Replica};

    // The whole stream from the cluster's first event, for replicas that start empty.
    let mut cluster = ic_sim::Cluster::in_memory(2, 7);
    cluster.open(open()).unwrap();
    for activation in ic_sim::config::shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let issue = term_life(1, open(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let (partition, pol) = (cluster.partition_of(&issue.group), issue.policy.clone());
    cluster.submit(partition, "new-business", ic_sim::persona::issue_command(issue)).unwrap();
    let records = cluster.take_events();
    let envelope = |key: &str, at, actor, command| Envelope {
        key: key.to_string(),
        at,
        actor,
        command,
    };
    let log = [
        envelope("a", t0(), module(EVENT_RELAY), Command::CoreEvents { records }),
        envelope("b", t0() + 1, party("stranger"), submit("r1", Some(&pol), "x", "claims")),
        envelope("b", t0() + 2, party(KIRI), submit("r1", Some(&pol), "x", "claims")),
        envelope("b", t0() + 3, party(KIRI), submit("r1", Some(&pol), "x", "claims")),
        envelope(
            "c",
            t0() + 4,
            module("claims"),
            Command::ResolveRequest {
                id: "r1".into(),
                reference: None,
                rejected: None,
            },
        ),
    ];
    let run = || {
        let mut replica = Replica::recover(MemoryLog::new(), ChannelState::shipped().unwrap()).unwrap();
        let outputs: Vec<Output> = log.iter().cloned().map(|e| replica.propose(e).unwrap().1).collect();
        (replica, outputs)
    };
    let (one, outputs) = run();
    let (two, _) = run();
    assert_eq!(code(outputs[1].result.clone()), "not_found");
    assert!(
        outputs[2].result.is_ok() && !outputs[2].duplicate,
        "the corrected retry under the same key is applied"
    );
    assert!(
        outputs[3].duplicate && outputs[3].result == Ok(vec![]),
        "a repeat is acknowledged and changes nothing"
    );
    assert_eq!(one.applied(), 5);

    let hash = one.state.state_hash();
    assert_eq!(hash, two.state.state_hash());
    assert_ne!(hash, ChannelState::shipped().unwrap().state_hash());

    // Saved and loaded, as a snapshot is: the same state, and it keeps working.
    let saved = serde_json::to_value(&one.state).unwrap();
    let mut restored: ChannelState = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.state_hash(), hash);
    assert_eq!(
        restored
            .scope()
            .may_view(
                "x",
                &[],
                &ic_readmodel::scope::RowScope {
                    kind: "policy",
                    subject: None,
                    sensitive: false
                },
                true
            )
            .ok(),
        Some(false)
    );
    use ic_log::StateMachine;
    assert!(restored.apply(6, log[3].clone()).duplicate, "idempotency keys survive a snapshot");
    // Outputs travel as JSON through the module host.
    let wire = serde_json::to_value(&outputs[1]).unwrap();
    assert_eq!(serde_json::from_value::<Output>(wire).unwrap(), outputs[1]);
}

/// Purpose: before any event has arrived the date is taken from the command's own time, never from a clock.
/// Why it matters: licences and engagements are judged on a date; a replica reading its own clock would disagree with its peers.
#[test]
fn the_date_falls_back_to_the_commands_time() {
    let ch = ic_channels::ChannelState::shipped().unwrap();
    assert_eq!(ch.today(t0()), open());
    assert_eq!(ch.today(i64::MAX), chrono::NaiveDate::default(), "an impossible time does not panic");
    assert!(ch.rows(&PolicyId::from("nope"), open(), None).is_empty());
    assert!(ch.pending_for("claims").is_empty());
}

/// Purpose: one person on one phone is a member of their company's scheme, the policyholder of a family health plan with their
/// partner on it, the holder of their own term life policy, and the scheme's administrator. Their phone holds their own scheme cover
/// and nothing else of the scheme, the family plan as its policyholder without the partner's claim, and their own policy; as the
/// scheme's administrator they see the scheme's members and billing and no other member's claim, not even a fellow director's.
/// Why it matters: persona row AUD-006. A small employer's director holds every role at once; a slip in any one of them would show
/// a colleague's or a partner's health to the person who employs or pays for them.
#[test]
fn one_person_in_three_roles_sees_each_slice() {
    const HONE: &str = "party-00000040";
    let mut w = World::new();
    let mut own = term_life(40, open(), d(1981, 4, 4), Sex::Male, false, 300_000_00, None, Frequency::Monthly, None);
    own.policyholder = p(HONE);
    let own = w.issue(own);
    let mut family = health_family(41, open(), &[d(1981, 4, 4), d(1983, 6, 6)], Frequency::Monthly);
    family.policyholder = p(HONE);
    family.lives[0].kind = LifeKind::Named { party: p(HONE) };
    let partner = "party-00000041-2";
    let family = w.issue(family);
    let scheme = w.issue(group_scheme(42, open(), vec![linked_member("m-1", HONE), linked_member("m-2", "director-2")]));
    w.dir(
        module("group-admin"),
        DirectoryCommand::SetEmployerAdmin {
            policy: scheme.clone(),
            party: p(HONE),
            admin: true,
        },
    )
    .unwrap();

    // Claims publishes a claim for the other director and one for the partner.
    let claim = |policy: &PolicyId, id: &str, subject: &str| Row {
        key: format!("{policy}/claims/{id}"),
        policy: policy.clone(),
        kind: "claim".into(),
        subject: Some(p(subject)),
        sensitive: true,
        body: json!({ "state": "received" }),
    };
    let rows = vec![claim(&scheme, "c-dir", "director-2"), claim(&family, "c-partner", partner)];
    w.send(module("claims"), Command::PublishRows { rows, remove: vec![] }).unwrap();

    let held = w.held(HONE);
    let of = |policy: &PolicyId| held.iter().filter(|k| k.starts_with(&format!("{policy}/"))).cloned().collect::<Vec<_>>();
    // The scheme: their own cover as a member, nothing the employer sees.
    assert_eq!(of(&scheme), [format!("{scheme}/member/m-1/group-life")], "{held:?}");
    // The family plan: as policyholder, the plan, both covers and the billing, never the partner's claim.
    assert!(of(&family).iter().any(|k| k.ends_with("/policy")), "{held:?}");
    assert!(of(&family).len() > 2, "{held:?}");
    assert!(!held.iter().any(|k| k.contains("/claims/")), "{held:?}");
    // Their own policy, whole.
    assert!(of(&own).iter().any(|k| k.ends_with("/policy")), "{held:?}");
    assert!(
        w.held(partner).contains(&format!("{family}/claims/c-partner")),
        "the claimant keeps their own claim"
    );

    // As administrator, through the portal: the scheme's members and billing, no claim of anyone's.
    let on = w.ch.today(w.at);
    let (relationships, rows) = w.ch.visible_rows(&p(HONE), &scheme, on, true);
    assert_eq!(relationships, [Relationship::Member, Relationship::EmployerAdmin]);
    assert!(rows.iter().all(|r| !r.sensitive && r.kind != "claim"), "{rows:?}");
    assert_eq!(rows.iter().filter(|r| r.kind == "member").count(), 2);
    assert!(w.sees("director-2", &scheme).contains(&format!("{scheme}/claims/c-dir")));
}
