//! Applying approved changes to the core: retries, finding the one row the
//! core refuses, stopping when the policy itself is refused, changes dated
//! ahead, and answers that arrive twice.

mod common;

use common::*;
use ic_group::command::{caller, Envelope, GroupCommand};
use ic_group::event::{ExceptionKind, GroupEvent, Outbound};
use ic_group::mapping::FileKind;
use ic_group::record::RowChange;
use ic_group::state::FileStatus;
use ic_group::terms::CoverEnd;

fn joiners() -> Vec<ic_group::record::MemberRow> {
    vec![
        row(1, "E001", attrs("staff", 60_000_00, "akl")),
        row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        row(3, "E003", attrs("staff", 90_000_00, "akl")),
        row(4, "E004", attrs("staff", 70_000_00, "wlg")),
        row(5, "E005", attrs("staff", 80_000_00, "wlg")),
    ]
}

/// Purpose: when the core says it is busy with the scheme's invoice, the changes wait and
/// go again the next morning under a new key, and nothing is lost or applied twice.
/// Why it matters: an invoice run on a large scheme spans many commands, and a file
/// approved in the middle of one is ordinary. The core stores a refusal under its key, so
/// a retry that reused the key would be refused for ever.
#[test]
fn a_busy_core_is_retried_the_next_morning_under_a_new_key() {
    let mut h = Harness::new(terms());
    h.core_faults.push_back(Some("invoice_in_progress".into()));
    h.apply("f1", FileKind::Full, joiners());
    assert_eq!(h.scheme().files["f1"].status, FileStatus::Applying);
    assert!(h.scheme().waiting && h.scheme().in_flight.is_none());
    assert_eq!(h.scheme().queue.len(), 4);
    assert_eq!(h.core_lives(), vec!["E001"]);

    h.days(1);
    assert_eq!(h.scheme().files["f1"].status, FileStatus::Applied);
    assert_eq!(h.core_lives().len(), 5);
    let keys: Vec<&str> = h.sent.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["grp:harbour:1", "grp:harbour:2"]);
}

/// Purpose: when the core refuses a chunk, the module narrows the chunk until the refused
/// row is alone, applies every other row, and reports that row to the employer and to staff.
/// Why it matters: one bad row in a file of thousands must not stop the other members'
/// cover, and it must not vanish either: the refusal is on the file and on the work queue.
#[test]
fn one_refused_row_does_not_stop_the_rest_of_the_file() {
    let mut h = Harness::new(terms());
    // The chunk of four is refused, then its first half; E002 alone is accepted, E003 and
    // E004 together are refused, and then E003 alone.
    for fault in [Some("ineligible"), Some("ineligible"), None, Some("ineligible"), Some("ineligible")] {
        h.core_faults.push_back(fault.map(String::from));
    }
    h.apply("f1", FileKind::Full, joiners());
    let f = &h.scheme().files["f1"];
    assert_eq!((f.status, f.applied, f.refused), (FileStatus::Applied, 3, 1));
    assert!(f.rejected.iter().any(|r| r.row == 3 && r.code == "ineligible"));
    assert_eq!(h.core_lives(), vec!["E001", "E002", "E004", "E005"]);
    assert!(h.scheme().membership.active("E003").is_none());
    assert!(h.saw(|e| matches!(e, GroupEvent::RowRefused { employee_id, change, code, .. }
        if employee_id == "E003" && change == "join" && code == "ineligible")));
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::RowRefused && e.subject == "E003"));
    assert!(h.scheme().chunk_limit > 1, "the chunk grows again once the core accepts");
}

/// Purpose: a refusal that is about the policy and not about any row stops the scheme's
/// queue with everything still in it, raises it to staff, and carries on from the same
/// place when they resume.
/// Why it matters: searching a file for the bad row when the policy is, say, lapsed would
/// refuse every member one by one, and the employer would have to send the file again.
#[test]
fn a_refusal_about_the_policy_blocks_the_scheme_until_staff_resume_it() {
    let mut h = Harness::new(terms());
    h.core_faults.push_back(Some("not_in_force".into()));
    h.apply("f1", FileKind::Full, joiners());
    assert_eq!(h.scheme().blocked.as_deref(), Some("not_in_force"));
    assert_eq!(h.scheme().queue.len(), 4);
    assert_eq!(h.sent.len(), 1, "no search for a bad row");
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::SchemeBlocked));
    assert!(h.saw(|e| matches!(e, GroupEvent::SchemeBlocked { code, .. } if code == "not_in_force")));
    // Morning comes and goes: still blocked.
    h.days(1);
    assert_eq!(h.core_lives(), vec!["E001"]);

    let resume = GroupCommand::Resume { scheme: SCHEME.into() };
    assert_eq!(h.send_as(caller::EMPLOYER, "admin-bo", resume.clone()).unwrap_err().code, "caller_not_allowed");
    h.staff(resume.clone()).unwrap();
    assert_eq!(h.scheme().files["f1"].status, FileStatus::Applied);
    assert_eq!(h.core_lives().len(), 5);
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::SchemeBlocked));
    assert_eq!(h.staff(resume).unwrap_err().code, "not_blocked");
}

/// Purpose: a change dated ahead waits in the module and goes to the core on its day; if
/// the member has gone by then, it is refused and raised, not sent.
/// Why it matters: employers send next month's leavers early. Applying them early ends
/// cover while the person is still employed; forgetting them bills the employer for ever.
#[test]
fn a_change_dated_ahead_is_applied_on_its_day() {
    let mut t = terms();
    t.cover_end = CoverEnd::LeavingDate;
    let mut h = Harness::new(t);
    h.apply("f1", FileKind::Full, joiners());
    let in_ten = h.today() + chrono::Days::new(10);
    let in_twenty = h.today() + chrono::Days::new(20);
    let rows = vec![
        change(1, "E002", RowChange::Leave, in_ten, attrs("staff", 60_000_00, "wlg")),
        change(2, "E003", RowChange::Leave, in_twenty, attrs("staff", 90_000_00, "akl")),
    ];
    h.apply("f2", FileKind::Changes, rows);
    let f = &h.scheme().files["f2"];
    assert_eq!((f.status, f.applied, f.scheduled), (FileStatus::Applied, 0, 2));
    assert_eq!(h.scheme().open_file, None, "the next file need not wait for the day");
    assert_eq!(h.scheme().scheduled.len(), 2);

    // E003 leaves sooner after all, by a later file.
    let today = h.today();
    h.apply(
        "f3",
        FileKind::Changes,
        vec![change(1, "E003", RowChange::Leave, today, attrs("staff", 90_000_00, "akl"))],
    );
    assert!(h.scheme().membership.active("E003").is_none());

    h.days(9);
    assert!(h.scheme().membership.active("E002").is_some());
    h.days(1);
    assert!(h.scheme().membership.active("E002").is_none());
    assert_eq!(h.scheme().membership.members["E002"].left, Some(in_ten));
    assert_eq!(h.scheme().files["f2"].applied, 1);

    h.days(10);
    let f = &h.scheme().files["f2"];
    assert_eq!((f.status, f.applied, f.refused, f.scheduled), (FileStatus::Applied, 1, 1, 0));
    assert!(f
        .rejected
        .iter()
        .any(|r| r.employee_id.as_deref() == Some("E003") && r.code == "unknown_member"));
    assert_eq!(h.core_lives(), vec!["E001", "E004", "E005"]);
}

/// Purpose: an answer from the core delivered twice, an answer nobody is waiting for, a
/// command repeated under its key, and an acknowledgement of a command the core has not
/// answered all leave the state exactly as it was.
/// Why it matters: delivery between the module and the core is at least once. Each of
/// these happens after any crash of the driver, and any of them changing state would
/// apply a member twice or drop a command the core never saw.
#[test]
fn repeats_and_strays_change_nothing() {
    let mut h = Harness::new(terms());
    h.upload("f1", FileKind::Full, joiners()).unwrap();
    // Approve without carrying anything, so a command for the core sits in the outbox.
    let approve = Envelope {
        key: "approve-1".into(),
        caller: caller::EMPLOYER.into(),
        actor: "admin-bo".into(),
        today: h.today(),
        command: GroupCommand::ApproveFile {
            scheme: SCHEME.into(),
            file: "f1".into(),
        },
    };
    assert!(!h.g.apply_envelope(approve.clone()).duplicate);
    let before = h.g.clone();
    let again = h.g.apply_envelope(approve);
    assert!(again.duplicate && again.result.is_ok());
    assert_eq!(h.g, before);
    h.settle();
    h.resolve("f1", |_| None);

    // The command is answered; a second copy of the answer and a stray one do nothing.
    let before = h.g.clone();
    for key in ["grp:harbour:1", "grp:harbour:99", "something-else"] {
        h.driver(GroupCommand::CoreResult {
            key: key.into(),
            error: Some("ineligible".into()),
        })
        .unwrap();
    }
    assert_eq!(h.g.schemes, before.schemes);
    assert_eq!(h.g.outbox, before.outbox);

    // A core command leaves the outbox only on the core's answer, never on an acknowledgement.
    let today = h.today();
    h.upload(
        "f2",
        FileKind::Changes,
        vec![change(1, "E002", RowChange::Leave, today, attrs("staff", 60_000_00, "wlg"))],
    )
    .unwrap();
    h.send_as(
        caller::EMPLOYER,
        "admin-bo",
        GroupCommand::ApproveFile {
            scheme: SCHEME.into(),
            file: "f2".into(),
        },
    )
    .unwrap();
    let (id, _) =
        h.g.outbox
            .iter()
            .find(|(_, o)| matches!(o, Outbound::Core { .. }))
            .map(|(id, o)| (*id, o.clone()))
            .unwrap();
    let all: Vec<u64> = h.g.outbox.keys().copied().collect();
    h.driver(GroupCommand::Ack { ids: all }).unwrap();
    assert_eq!(h.g.outbox.keys().copied().collect::<Vec<_>>(), vec![id]);
    h.settle();
    assert!(h.scheme().membership.active("E002").is_none());

    // A refused command is refused again under the same key, and changed nothing.
    let bad = Envelope {
        key: "bad-1".into(),
        caller: caller::STAFF.into(),
        actor: "staff-ana".into(),
        today: h.today(),
        command: GroupCommand::Unfreeze { scheme: SCHEME.into() },
    };
    let before = h.g.schemes.clone();
    assert_eq!(h.g.apply_envelope(bad.clone()).result.unwrap_err().code, "not_frozen");
    let again = h.g.apply_envelope(bad);
    assert!(again.duplicate);
    assert_eq!(again.result.unwrap_err().code, "not_frozen");
    assert_eq!(h.g.schemes, before);
}
