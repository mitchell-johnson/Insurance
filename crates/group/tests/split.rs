//! Splitting a scheme: part of an employer is sold or demerged and its
//! members move to a new scheme with their history and underwritten cover.

mod common;

use common::*;
use ic_core::{PolicyId, Reject};
use ic_group::command::{caller, ExcessDecision, GroupCommand};
use ic_group::event::{ExceptionKind, GroupEvent, ReinsuranceNotice};
use ic_group::mapping::FileKind;
use ic_group::record::MemberRow;
use ic_group::state::{CaseStatus, FileStatus};

const NEW: &str = "harbour-north";

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn new_policy() -> PolicyId {
    PolicyId("pol-north".to_string())
}

fn split(employee_ids: &[&str], effective: chrono::NaiveDate) -> GroupCommand {
    GroupCommand::SplitScheme {
        scheme: SCHEME.to_string(),
        into: NEW.to_string(),
        new_policy: new_policy(),
        new_number: "WPLNORTH01".to_string(),
        employer: party("employer-north"),
        effective,
        employee_ids: employee_ids.iter().map(|s| s.to_string()).collect(),
        terms: Box::new(terms()),
    }
}

/// Apply a file, confirming it if the safety check holds it.
fn apply(h: &mut Harness, file: &str, rows: Vec<MemberRow>) {
    h.upload(file, FileKind::Full, rows).unwrap();
    if h.scheme().files[file].status == FileStatus::Held {
        h.staff(GroupCommand::ConfirmHeld {
            scheme: SCHEME.to_string(),
            file: file.to_string(),
        })
        .unwrap();
    }
    h.approve(file).unwrap();
    h.resolve(file, |_| None);
}

/// The founder, a staff member, an executive whose cover above the automatic
/// amount underwriting accepted, and `more` further staff.
fn scheme_with(more: usize) -> Harness {
    let mut h = Harness::new(terms());
    let mut rows = vec![
        row(1, "E001", attrs("staff", 60_000_00, "akl")),
        row(2, "E003", attrs("staff", 90_000_00, "akl")),
        row(3, "E004", attrs("executive", 240_000_00, "wlg")),
    ];
    rows.extend((0..more).map(|n| row(4 + n as u32, &format!("M{n:03}"), attrs("staff", 50_000_00, "wlg"))));
    apply(&mut h, "f1", rows);
    let case = h.requests[0].case;
    h.underwriting(
        case,
        ExcessDecision::Accepted {
            sum_insured_minor: 210_000_00,
            loadings: vec![],
            exclusions: vec![],
            member_consents_to_itemising: false,
            member_pays_loading: false,
        },
    )
    .unwrap();
    assert_eq!(h.scheme().cases[&case].status, CaseStatus::Accepted);
    h
}

/// Purpose: a split moves the named members to a new scheme in the core and here, in parts
/// of at most one membership chunk, with their life records, joining dates and the
/// executive's underwritten cover under the same case and layer; the new scheme is opened
/// with its own employer and terms, announced to reinsurance, and takes files; the members
/// are not leavers, so nobody is offered a continuation.
/// Why it matters: a sale or demerger must not interrupt anyone's cover or send members back
/// through underwriting, and the two schemes must agree with the core about who is on which,
/// or the next file on either scheme would join or remove the wrong people.
#[test]
fn a_split_moves_members_with_their_cover_to_a_new_scheme() {
    let mut h = scheme_with(100);
    let case = *h.scheme().cases.keys().next().unwrap();
    let effective = d(2027, 12, 1);
    let mut moving = vec!["E003", "E004"];
    let extra: Vec<String> = (0..100).map(|n| format!("M{n:03}")).collect();
    moving.extend(extra.iter().map(String::as_str));

    // Staff only, and files wait until the split is done.
    assert_eq!(code(h.send_as(caller::EMPLOYER, "admin-bo", split(&moving, effective))), "caller_not_allowed");
    h.send_as(caller::STAFF, "staff-ana", split(&moving, effective)).unwrap();
    let busy = GroupCommand::ReceiveFile {
        scheme: SCHEME.to_string(),
        file: "f2".to_string(),
        content_hash: "hash-f2".to_string(),
    };
    assert_eq!(code(h.send_as(caller::EMPLOYER, "admin-bo", busy)), "scheme_splitting");
    assert_eq!(code(h.send_as(caller::STAFF, "staff-ana", split(&["E001"], effective))), "scheme_busy");
    h.settle();

    // Two parts: the core takes at most a chunk of members per command.
    let parts = h.sent.iter().filter(|(_, c)| matches!(c, ic_core::Command::SplitScheme(_))).count();
    assert_eq!(parts, 2);
    assert!(h.saw(|e| matches!(e, GroupEvent::SplitStarted { members: 102, .. })));
    assert!(h.saw(|e| matches!(e, GroupEvent::MembersMoved { moved: 100, .. })));
    assert!(h.saw(|e| matches!(e, GroupEvent::SplitCompleted { members: 102, .. })));
    assert!(h.saw(|e| matches!(e, GroupEvent::SchemeRegistered { scheme, .. } if scheme == NEW)));
    assert!(!h.saw(|e| matches!(e, GroupEvent::ContinuationOffered { .. })), "nobody left");
    assert!(h
        .notices
        .iter()
        .any(|n| matches!(n, ReinsuranceNotice::SetSchemeTerms { policy, .. } if *policy == new_policy())));

    // Here: the old scheme keeps the founder; the moved members' records end on the date.
    let old = h.scheme();
    assert!(old.splitting.is_none());
    assert_eq!(old.membership.active_count(), 1);
    assert_eq!(old.membership.members["E004"].left, Some(effective));
    assert!(!old.cases.contains_key(&case));
    let new = &h.g.schemes[NEW];
    assert_eq!(new.employer, party("employer-north"));
    assert_eq!(new.policy, new_policy());
    assert_eq!(new.mapping, old.mapping, "the employer's layout comes with the members");
    assert_eq!(new.membership.active_count(), 102);
    let e004 = &new.membership.members["E004"];
    assert_eq!((e004.life.clone(), e004.joined), (life("E004"), d(2027, 11, 1)));
    assert_eq!(new.cases[&case].status, CaseStatus::Accepted);
    assert_eq!(h.g.by_policy[&new_policy()], NEW);

    // The core agrees: the lives and the layer are on the new policy.
    let state = h.c.state(h.p);
    let moved = &state.policies[&new_policy()];
    assert_eq!(moved.lives.len(), 102);
    let layer = new.cases[&case].layer.unwrap();
    assert!(moved
        .covers
        .values()
        .flat_map(|c| c.layers.iter())
        .any(|l| l.life == life("E004") && l.id == layer));
    assert_eq!(h.core_lives(), vec!["E001".to_string()]);

    // The old scheme takes files again, and a full file of just the founder changes nothing.
    h.apply("f2", FileKind::Full, vec![row(1, "E001", attrs("staff", 60_000_00, "akl"))]);
    assert_eq!(h.scheme().membership.active_count(), 1);
    assert!(!h.saw(|e| matches!(e, GroupEvent::RowRefused { .. })));

    // The new scheme is invoiced for its own members from the date.
    h.days(35);
    assert!(h.saw(|e| matches!(e, GroupEvent::InvoiceIssued { scheme, members: 102, .. } if scheme == NEW)));
}

/// Purpose: a split that does not fit the scheme is refused before anything is sent: the
/// new scheme exists already or has a bad ID, a member is named twice or is not a member,
/// every member would move, a member's cover above the automatic amount is still with
/// underwriting, the scheme is not active, or the terms are not valid.
/// Why it matters: the core refuses some of these too, but only part by part; a split
/// half done on a mistake staff could have been told about at once leaves members on two
/// schemes until someone untangles them.
#[test]
fn a_split_that_does_not_fit_is_refused_before_anything_moves() {
    let mut h = Harness::new(terms());
    apply(
        &mut h,
        "f1",
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E003", attrs("staff", 90_000_00, "akl")),
            row(3, "E004", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    let on = d(2027, 12, 1);
    let with = |f: &dyn Fn(&mut GroupCommand)| {
        let mut c = split(&["E003"], on);
        f(&mut c);
        c
    };
    let existing = h.policy.clone();
    let refused = |h: &mut Harness, c: GroupCommand| code(h.staff(c));
    assert_eq!(
        refused(
            &mut h,
            with(&|c| if let GroupCommand::SplitScheme { into, .. } = c {
                *into = SCHEME.into()
            })
        ),
        "scheme_exists"
    );
    assert_eq!(
        refused(
            &mut h,
            with(&|c| if let GroupCommand::SplitScheme { into, .. } = c {
                *into = "a:b".into()
            })
        ),
        "invalid_scheme"
    );
    assert_eq!(
        refused(
            &mut h,
            with(&|c| if let GroupCommand::SplitScheme { new_policy, .. } = c {
                *new_policy = existing.clone()
            })
        ),
        "scheme_exists"
    );
    assert_eq!(refused(&mut h, split(&[], on)), "invalid_split");
    assert_eq!(refused(&mut h, split(&["E003", "E003"], on)), "invalid_split");
    assert_eq!(refused(&mut h, split(&["E009"], on)), "unknown_member");
    assert_eq!(refused(&mut h, split(&["E001", "E003", "E004"], on)), "not_a_split");
    assert_eq!(refused(&mut h, split(&["E004"], on)), "excess_open", "underwriting is still deciding");
    let mut bad = terms();
    bad.safety_share_percent = 0;
    assert_eq!(
        refused(
            &mut h,
            with(&|c| if let GroupCommand::SplitScheme { terms, .. } = c {
                **terms = bad.clone()
            })
        ),
        "invalid_terms"
    );
    let pool = ic_group::terms::MemberBasis::Pool;
    assert_eq!(
        refused(
            &mut h,
            with(&|c| if let GroupCommand::SplitScheme { terms, .. } = c {
                terms.basis = pool
            })
        ),
        "pool_scheme"
    );
    h.staff(GroupCommand::Freeze {
        scheme: SCHEME.into(),
        reason: "checking".into(),
    })
    .unwrap();
    assert_eq!(refused(&mut h, split(&["E003"], on)), "scheme_not_ready");
    assert!(!h.g.schemes.contains_key(NEW));
    assert!(!h.sent.iter().any(|(_, c)| matches!(c, ic_core::Command::SplitScheme(_))));
}

/// Purpose: when the core refuses a part of a split, staff are told how many members had
/// already moved, the parts not yet sent are dropped, the old scheme takes files again,
/// and a split on a date that is not the start of a billing period moves nobody.
/// Why it matters: the members who moved are covered on the new scheme and the rest on the
/// old one; carrying on with later parts after a refusal, or keeping the old scheme closed
/// to files, would leave the employer's changes unapplied with nobody knowing why.
#[test]
fn a_refused_part_stops_the_split_and_says_how_far_it_got() {
    let mut h = scheme_with(100);
    let mut moving: Vec<String> = (0..100).map(|n| format!("M{n:03}")).collect();
    moving.push("E003".to_string());
    let ids: Vec<&str> = moving.iter().map(String::as_str).collect();

    // A date inside a period: the core refuses the first part and nothing moves.
    h.staff(split(&ids, d(2027, 12, 15))).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::SplitRefused { moved: 0, code, .. } if code == "effective")));
    assert!(!h.g.schemes.contains_key(NEW));
    assert!(h.scheme().splitting.is_none());
    assert!(h.g.exceptions.values().any(|x| x.kind == ExceptionKind::RowRefused));

    // The first part moves; the core refuses the second.
    h.core_faults.extend([None, Some("member_money".to_string())]);
    h.staff(split(&ids, d(2027, 12, 1))).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::SplitRefused { moved: 100, code, .. } if code == "member_money")));
    assert_eq!(h.g.schemes[NEW].membership.active_count(), 100);
    assert!(h.scheme().membership.active("E003").is_some(), "the second part stayed");
    assert!(h.scheme().queue.is_empty());
    h.apply("f2", FileKind::Changes, vec![]);
}
