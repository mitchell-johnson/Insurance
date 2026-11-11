//! The per-party change feed a device syncs from.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::p;
use ic_channels::feed::*;
use ic_core::PolicyId;
use ic_readmodel::scope::{AudienceScope, Relationship};
use serde_json::{json, Value};

fn rows(items: &[(&str, Value)]) -> BTreeMap<String, Value> {
    items.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn row(key: &str, kind: &str, subject: Option<&str>, sensitive: bool) -> Row {
    Row {
        key: key.to_string(),
        policy: PolicyId::from("pol-1"),
        kind: kind.to_string(),
        subject: subject.map(p),
        sensitive,
        body: json!({}),
    }
}

/// Purpose: a device with nothing takes a snapshot, then receives exactly the changes after the version it holds.
/// Why it matters: this is the sync contract the app is built on; a missed or repeated change shows a customer the wrong cover or balance offline.
#[test]
fn snapshot_then_changes() {
    let pol = PolicyId::from("pol-1");
    let mut feed = PartyFeed::default();
    assert_eq!(feed.sync(None), Sync::Snapshot { version: 0, rows: vec![] });
    assert_eq!(feed.sync(Some(0)), Sync::Changes { version: 0, changes: vec![] });

    let made = feed.reconcile(
        &pol,
        &rows(&[("pol-1/policy", json!({ "status": "in_force" })), ("pol-1/billing", json!({ "due": 10 }))]),
        |_| true,
    );
    assert_eq!(made, 2);
    let Sync::Snapshot { version, rows: snapshot } = feed.sync(None) else {
        panic!("expected a snapshot")
    };
    assert_eq!(version, 2);
    assert_eq!(snapshot.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["pol-1/billing", "pol-1/policy"]);

    // Nothing changed: nothing is sent, and the version does not move.
    assert_eq!(
        feed.reconcile(
            &pol,
            &rows(&[("pol-1/policy", json!({ "status": "in_force" })), ("pol-1/billing", json!({ "due": 10 }))]),
            |_| true
        ),
        0
    );
    assert_eq!(feed.sync(Some(2)), Sync::Changes { version: 2, changes: vec![] });

    // One row changes.
    feed.reconcile(
        &pol,
        &rows(&[("pol-1/policy", json!({ "status": "lapsed" })), ("pol-1/billing", json!({ "due": 10 }))]),
        |_| true,
    );
    let Sync::Changes { version, changes } = feed.sync(Some(2)) else {
        panic!("expected changes")
    };
    assert_eq!(version, 3);
    assert_eq!(
        changes,
        vec![Change {
            version: 3,
            key: "pol-1/policy".into(),
            op: Op::Upsert {
                body: json!({ "status": "lapsed" })
            }
        }]
    );
    // A device one behind the start gets everything in order.
    let Sync::Changes { changes, .. } = feed.sync(Some(0)) else {
        panic!("expected changes")
    };
    assert_eq!(changes.iter().map(|c| c.version).collect::<Vec<_>>(), [1, 2, 3]);
}

/// Purpose: a row the viewer can no longer see is purged if it still exists and deleted if it is gone, and other policies' rows are left alone.
/// Why it matters: purge is how personal data leaves a device when a relationship ends; confusing it with delete, or touching another policy, either leaks data or destroys it.
#[test]
fn lost_access_purges_and_a_gone_row_deletes() {
    let (one, two) = (PolicyId::from("pol-1"), PolicyId::from("pol-10"));
    let mut feed = PartyFeed::default();
    feed.reconcile(
        &one,
        &rows(&[("pol-1/policy", json!(1)), ("pol-1/claim/c1", json!(2)), ("pol-1/notice/3", json!(3))]),
        |_| true,
    );
    feed.reconcile(&two, &rows(&[("pol-10/policy", json!(9))]), |_| true);

    let made = feed.reconcile(&one, &rows(&[("pol-1/policy", json!(1))]), |key| key == "pol-1/claim/c1");
    assert_eq!(made, 2);
    let Sync::Changes { changes, .. } = feed.sync(Some(4)) else {
        panic!("expected changes")
    };
    let ops: BTreeMap<&str, &Op> = changes.iter().map(|c| (c.key.as_str(), &c.op)).collect();
    assert_eq!(ops["pol-1/claim/c1"], &Op::Purge);
    assert_eq!(ops["pol-1/notice/3"], &Op::Delete);
    assert_eq!(
        feed.rows.keys().map(String::as_str).collect::<Vec<_>>(),
        ["pol-1/policy", "pol-10/policy"],
        "pol-10 is not under pol-1"
    );

    // The whole relationship ends.
    feed.reconcile(&one, &BTreeMap::new(), |_| true);
    assert_eq!(feed.rows.len(), 1);
}

/// Purpose: the change log is bounded, and a device too far behind or ahead of the feed starts again from a snapshot.
/// Why it matters: an unbounded log grows without limit in replicated state; a device that silently skipped trimmed changes would hold stale rows forever.
#[test]
fn a_device_too_far_behind_or_ahead_takes_a_snapshot() {
    let pol = PolicyId::from("pol-1");
    let mut feed = PartyFeed::default();
    for i in 0..(CHANGE_LOG_LEN as u64 + 10) {
        feed.reconcile(&pol, &rows(&[("pol-1/policy", json!(i))]), |_| true);
    }
    assert_eq!(feed.changes.len(), CHANGE_LOG_LEN);
    assert_eq!(feed.version, CHANGE_LOG_LEN as u64 + 10);
    // The oldest change kept is version 11, so a device at 10 can still follow.
    assert!(matches!(feed.sync(Some(10)), Sync::Changes { changes, .. } if changes.len() == CHANGE_LOG_LEN));
    assert!(matches!(feed.sync(Some(9)), Sync::Snapshot { .. }));
    assert!(
        matches!(feed.sync(Some(feed.version + 1)), Sync::Snapshot { .. }),
        "a device ahead of the feed restarts"
    );
    let Sync::Snapshot { rows: snapshot, version } = feed.sync(Some(0)) else {
        panic!("expected a snapshot")
    };
    assert_eq!((snapshot.len(), snapshot[0].version, version), (1, feed.version, feed.version));
}

/// Purpose: the wire form of a sync answer is stable: a mode tag, and each change flattened with its operation.
/// Why it matters: the app and the portals parse this JSON; a silent change of shape breaks every installed client.
#[test]
fn the_wire_form_is_stable() {
    let pol = PolicyId::from("pol-1");
    let mut feed = PartyFeed::default();
    feed.reconcile(&pol, &rows(&[("pol-1/policy", json!({ "a": 1 }))]), |_| true);
    assert_eq!(
        serde_json::to_value(feed.sync(None)).unwrap(),
        json!({ "mode": "snapshot", "version": 1, "rows": [{ "key": "pol-1/policy", "version": 1, "body": { "a": 1 } }] })
    );
    feed.reconcile(&pol, &BTreeMap::new(), |_| true);
    assert_eq!(
        serde_json::to_value(feed.sync(Some(0))).unwrap(),
        json!({ "mode": "changes", "version": 2, "changes": [
            { "version": 1, "key": "pol-1/policy", "op": "upsert", "body": { "a": 1 } },
            { "version": 2, "key": "pol-1/policy", "op": "purge" },
        ] })
    );
    let change: Change = serde_json::from_value(json!({ "version": 3, "key": "k", "op": "delete" })).unwrap();
    assert_eq!(change.op, Op::Delete);
}

/// Purpose: visibility follows the shipped audience policy, with family consent layered on top and never on stale data.
/// Why it matters: this function decides whether health-implying facts about one adult reach another person's screen.
#[test]
fn visibility_respects_relationships_consent_and_freshness() {
    let scope = AudienceScope::shipped().unwrap();
    let (holder, partner) = (p("holder"), p("partner"));
    let none = BTreeSet::new();
    let consented: BTreeSet<_> = [partner.clone()].into();
    let own_claim = row("pol-1/claims/c1", "claim", Some("partner"), true);
    let cover = row("pol-1/cover/x/1", "cover", Some("partner"), false);
    let policy = row("pol-1/policy", "policy", None, false);
    let see = |viewer, rels: &[Relationship], row: &Row, consent: &BTreeSet<_>, fresh| visible(&scope, viewer, rels, row, consent, fresh).unwrap();

    // No relationship: nothing, whatever the row.
    assert!(!see(&holder, &[], &policy, &none, true));
    // The policyholder sees the policy and others' ordinary rows, not another adult's sensitive row.
    assert!(see(&holder, &[Relationship::Policyholder], &policy, &none, true));
    assert!(see(&holder, &[Relationship::Policyholder], &cover, &none, true));
    assert!(!see(&holder, &[Relationship::Policyholder], &own_claim, &none, true));
    // With that adult's consent they do, but never on stale data and never without being the policyholder.
    assert!(see(&holder, &[Relationship::Policyholder], &own_claim, &consented, true));
    assert!(!see(&holder, &[Relationship::Policyholder], &own_claim, &consented, false));
    assert!(!see(&holder, &[Relationship::Adviser], &own_claim, &consented, true));
    // Consent covers the person who gave it only.
    let childs = row("pol-1/claims/c2", "claim", Some("other-adult"), true);
    assert!(!see(&holder, &[Relationship::Policyholder], &childs, &consented, true));
    // The person themselves sees their own rows and nothing else.
    assert!(see(&partner, &[Relationship::LifeInsured], &own_claim, &none, true));
    assert!(see(&partner, &[Relationship::LifeInsured], &cover, &none, true));
    assert!(!see(&partner, &[Relationship::LifeInsured], &policy, &none, true));
    assert!(
        !see(&partner, &[Relationship::LifeInsured], &own_claim, &none, false),
        "sensitive rows need fresh data"
    );
    // An employer administrator sees scheme, billing and member rows only; a lender the policy only.
    let member = row("pol-1/member/m1/group-life", "member", Some("m1"), false);
    assert!(see(&holder, &[Relationship::EmployerAdmin], &member, &none, true));
    assert!(!see(&holder, &[Relationship::EmployerAdmin], &policy, &none, true));
    assert!(see(&holder, &[Relationship::Assignee], &policy, &none, true));
    assert!(!see(&holder, &[Relationship::Assignee], &cover, &none, true));
    assert_eq!(own_claim.scope().subject, Some("partner"));
}
