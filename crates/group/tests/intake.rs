//! Membership files from receipt to the core: quarantine, comparison, the
//! safety hold, approval, matching people, and applying in order.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::Reject;
use ic_group::command::{caller, GroupCommand, Resolution};
use ic_group::event::{ExceptionKind, GroupEvent, ReinsuranceNotice};
use ic_group::mapping::{FileKind, RowReject};
use ic_group::record::RowChange;
use ic_group::state::FileStatus;

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn file_cmd(f: fn(String, String) -> GroupCommand, file: &str) -> GroupCommand {
    f(SCHEME.to_string(), file.to_string())
}

/// Three joiners beside the founding member: two staff and an executive
/// whose formula cover is above the automatic limit.
fn first_file() -> Vec<ic_group::record::MemberRow> {
    vec![
        row(1, "E001", attrs("staff", 60_000_00, "akl")),
        row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        row(3, "E003", attrs("staff", 90_000_00, "akl")),
        row(4, "E004", attrs("executive", 240_000_00, "wlg")),
    ]
}

/// Purpose: a full file goes from quarantine through scan, comparison, the employer's
/// approval and the customer master to the core, and the module, the core and the
/// reinsurance notices all agree on who is a member at the end.
/// Why it matters: this is the whole reason the module exists. A member the module shows
/// and the core does not hold has no cover; one the core holds and the module does not is
/// billed without the employer ever having approved it.
#[test]
fn a_full_file_reaches_the_core_as_the_employer_approved_it() {
    let mut h = Harness::new(terms());
    h.upload("f1", FileKind::Full, first_file()).unwrap();
    let f = &h.scheme().files["f1"];
    assert_eq!(f.status, FileStatus::AwaitingApproval);
    assert_eq!((f.summary.joiners, f.summary.leavers, f.summary.changes, f.summary.rejected), (3, 0, 0, 0));
    assert_eq!((f.summary.members_before, f.summary.above_automatic_cover), (1, 1));
    assert_eq!(h.core_lives(), vec!["E001"], "nothing reaches the core before approval");

    h.approve("f1").unwrap();
    assert_eq!(h.scheme().files["f1"].status, FileStatus::ResolvingParties);
    assert_eq!(
        h.to_resolve,
        vec![("f1".to_string(), vec!["E002".to_string(), "E003".to_string(), "E004".to_string()])]
    );
    h.resolve("f1", |id| (id == "E003").then(|| Resolution::Linked { party: party("party-known") }));

    assert_eq!(h.scheme().files["f1"].status, FileStatus::Applied);
    assert_eq!(h.scheme().open_file, None);
    assert_eq!(h.core_lives(), vec!["E001", "E002", "E003", "E004"]);
    assert_eq!(h.scheme().membership.active_count(), 4);
    assert_eq!(h.scheme().membership.active("E003").unwrap().party, Some(party("party-known")));
    assert!(h.saw(|e| matches!(
        e,
        GroupEvent::FileApplied {
            applied: 3,
            refused: 0,
            scheduled: 0,
            ..
        }
    )));
    assert!(h.saw(|e| matches!(e, GroupEvent::MemberLinked { how, .. } if how == "matched")));
    assert!(h.saw(|e| matches!(e, GroupEvent::MemberLinked { how, .. } if how == "created")));
    // Reinsurance is told the risk factors of each joiner and, at registration, the scheme's terms.
    assert!(matches!(h.notices[0], ReinsuranceNotice::SetSchemeTerms { .. }));
    assert_eq!(h.notices.iter().filter(|n| matches!(n, ReinsuranceNotice::RecordRiskFactors { .. })).count(), 3);
    // The executive's formula gives 960,000; 750,000 is automatic and the rest goes to underwriting.
    assert_eq!(h.requests.len(), 1);
    assert_eq!((h.requests[0].life.clone(), h.requests[0].excess_minor), (life("E004"), 210_000_00));
    // One command carried all three joiners, keyed from the module's own state.
    assert_eq!(h.sent.len(), 1);
    assert_eq!(h.sent[0].0, "grp:harbour:1");
    assert!(h.g.outbox.is_empty() && h.g.core_keys.is_empty());

    // The same content again is recognised, whatever it is called.
    let again = h.employer(GroupCommand::ReceiveFile {
        scheme: SCHEME.into(),
        file: "f1-again".into(),
        content_hash: "hash-f1".into(),
    });
    assert_eq!(code(again), "duplicate_file");
}

/// Purpose: nothing reads a file that has not been scanned, a file that fails the scan or
/// cannot be mapped is refused, and a refused or withdrawn file frees the scheme for the
/// next one.
/// Why it matters: employer files arrive from outside. Parsing one before it is scanned is
/// how malware gets in, and a refused file that kept the scheme locked would stop every
/// later file for that employer.
#[test]
fn a_file_is_quarantined_until_scanned_and_a_bad_one_is_refused() {
    let mut h = Harness::new(terms());
    let receive = |h: &mut Harness, file: &str| {
        h.employer(GroupCommand::ReceiveFile {
            scheme: SCHEME.into(),
            file: file.into(),
            content_hash: format!("hash-{file}"),
        })
    };
    let submit = |h: &mut Harness, file: &str, hash: String, rows: Vec<ic_group::record::MemberRow>, last: bool| {
        let today = h.today();
        h.driver(GroupCommand::SubmitRows {
            scheme: SCHEME.into(),
            file: file.into(),
            mapping_hash: hash,
            kind: FileKind::Full,
            file_date: today,
            rows,
            rejected: vec![RowReject::new(9, Some("E009"), "invalid_date", Some("date_of_birth"))],
            last,
        })
    };
    receive(&mut h, "f1").unwrap();
    assert_eq!(h.scheme().files["f1"].status, FileStatus::Quarantined);
    assert_eq!(code(submit(&mut h, "f1", mapping(1).content_hash, first_file(), true)), "wrong_file_status");
    assert_eq!(code(receive(&mut h, "f1")), "file_exists");
    assert_eq!(code(receive(&mut h, "f2")), "file_in_progress");

    let scan = |h: &mut Harness, file: &str, clean: bool| {
        h.driver(GroupCommand::RecordScan {
            scheme: SCHEME.into(),
            file: file.into(),
            clean,
        })
    };
    scan(&mut h, "f1", false).unwrap();
    h.settle();
    assert_eq!(h.scheme().files["f1"].status, FileStatus::Refused);
    assert!(h.saw(|e| matches!(e, GroupEvent::FileRefused { code, .. } if code == "malware")));
    assert_eq!(code(scan(&mut h, "f1", true)), "wrong_file_status");
    assert_eq!(code(scan(&mut h, "nope", true)), "unknown_file");

    // The next file can start. It cannot be read with another mapping, for a future date,
    // or in parts that are too large.
    receive(&mut h, "f2").unwrap();
    scan(&mut h, "f2", true).unwrap();
    assert_eq!(code(submit(&mut h, "f2", "another-mapping".into(), first_file(), true)), "mapping_mismatch");
    let many = (0..2_001).map(|i| row(i, &format!("X{i}"), attrs("staff", 1, "akl"))).collect();
    assert_eq!(code(submit(&mut h, "f2", mapping(1).content_hash, many, true)), "too_many_rows");
    let tomorrow = h.today().succ_opt().unwrap();
    let future = h.driver(GroupCommand::SubmitRows {
        scheme: SCHEME.into(),
        file: "f2".into(),
        mapping_hash: mapping(1).content_hash,
        kind: FileKind::Full,
        file_date: tomorrow,
        rows: vec![],
        rejected: vec![],
        last: true,
    });
    assert_eq!(code(future), "file_date");
    // The mapper could not read it at all.
    h.driver(GroupCommand::RefuseFile {
        scheme: SCHEME.into(),
        file: "f2".into(),
        code: "not_text".into(),
    })
    .unwrap();
    assert_eq!(h.scheme().files["f2"].code.as_deref(), Some("not_text"));

    // Rows may arrive in parts; rows the mapper rejected are kept with the file's own.
    receive(&mut h, "f3").unwrap();
    scan(&mut h, "f3", true).unwrap();
    let mut rows = first_file();
    let tail = rows.split_off(2);
    submit(&mut h, "f3", mapping(1).content_hash, rows, false).unwrap();
    assert_eq!(h.scheme().files["f3"].status, FileStatus::Scanned);
    submit(&mut h, "f3", mapping(1).content_hash, tail, true).unwrap();
    let f = &h.scheme().files["f3"];
    assert_eq!(
        (f.status, f.summary.joiners, f.summary.rejected, f.summary.rows),
        (FileStatus::AwaitingApproval, 3, 2, 6)
    );
    assert_eq!(f.rejected[0].code, "invalid_date");

    // Withdrawn before approval, it frees the scheme and can be sent again under a new name.
    h.employer(file_cmd(|scheme, file| GroupCommand::WithdrawFile { scheme, file }, "f3")).unwrap();
    assert_eq!(h.scheme().files["f3"].status, FileStatus::Withdrawn);
    assert_eq!(code(h.approve("f3")), "wrong_file_status");
    receive(&mut h, "f4").unwrap();
    assert_eq!(h.core_lives(), vec!["E001"]);
}

/// Purpose: a full file that would remove more than the scheme's safety share of its
/// members is held until a member of staff confirms it, and applies as approved once they do.
/// Why it matters: a truncated export looks exactly like a mass resignation. Without the
/// hold, half a workforce loses cover because a payroll job failed half way.
#[test]
fn a_file_that_removes_too_many_members_is_held_for_a_person() {
    let mut h = Harness::new(terms());
    h.apply("f1", FileKind::Full, first_file());
    // The next export has only one of the four.
    h.upload("f2", FileKind::Full, vec![row(1, "E001", attrs("staff", 60_000_00, "akl"))]).unwrap();
    assert_eq!(h.scheme().files["f2"].status, FileStatus::Held);
    assert!(h.saw(|e| matches!(e, GroupEvent::FileHeld { leavers: 3, members: 4, .. })));
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::FileHeld && e.subject == "f2"));
    assert_eq!(code(h.approve("f2")), "wrong_file_status");
    // Only staff may confirm, and the employer still has to approve afterwards.
    let confirm = file_cmd(|scheme, file| GroupCommand::ConfirmHeld { scheme, file }, "f2");
    assert_eq!(code(h.send_as(caller::EMPLOYER, "admin-bo", confirm.clone())), "caller_not_allowed");
    h.staff(confirm).unwrap();
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::FileHeld));
    assert_eq!(h.core_lives().len(), 4);
    h.approve("f2").unwrap();
    assert_eq!(h.scheme().files["f2"].status, FileStatus::Applied, "no joiners, so nobody to ask about");
    assert_eq!(h.core_lives(), vec!["E001"]);
    assert_eq!(h.scheme().membership.active_count(), 1);
}

/// Purpose: a scheme that needs two people cannot have a file approved by whoever uploaded
/// it, and changes dated further back than the employer may date them wait for staff, who
/// can let them through or drop only those rows.
/// Why it matters: backdated leavers are refunds and backdated joiners are cover for an
/// event that may already have happened. Both are where fraud and honest error concentrate,
/// so one person at the employer must not be able to do either alone.
#[test]
fn approval_needs_a_second_person_and_staff_for_deep_backdating() {
    let mut t = terms();
    t.second_approver = true;
    let mut h = Harness::new(t);
    h.apply_as_two("f1", FileKind::Full, first_file());
    h.days(120);
    h.pay(h.owed());
    // E002 is said to have left 100 days ago (beyond the 90 the employer may backdate) and
    // E003's salary changed last week.
    let hundred_days_ago = h.today() - chrono::Days::new(100);
    let last_week = h.today() - chrono::Days::new(7);
    let rows = vec![
        change(1, "E002", RowChange::Leave, hundred_days_ago, attrs("staff", 60_000_00, "wlg")),
        change(2, "E003", RowChange::Change, last_week, attrs("staff", 95_000_00, "akl")),
    ];
    h.upload("f2", FileKind::Changes, rows.clone()).unwrap();
    assert_eq!(h.scheme().files["f2"].summary.needing_backdating_approval, 1);
    assert_eq!(code(h.approve("f2")), "second_approver");
    h.send_as(
        caller::EMPLOYER,
        "admin-cy",
        file_cmd(|scheme, file| GroupCommand::ApproveFile { scheme, file }, "f2"),
    )
    .unwrap();
    h.settle();
    assert_eq!(h.scheme().files["f2"].status, FileStatus::AwaitingBackdatingDecision);
    assert!(h.scheme().membership.active("E002").is_some(), "nothing applied while staff decide");
    // Staff decline: only the backdated row is dropped.
    h.staff(GroupCommand::DecideBackdating {
        scheme: SCHEME.into(),
        file: "f2".into(),
        approve: false,
    })
    .unwrap();
    let f = &h.scheme().files["f2"];
    assert_eq!((f.status, f.applied, f.refused), (FileStatus::Applied, 1, 1));
    assert!(f.rejected.iter().any(|r| r.code == "backdating_declined" && r.row == 1));
    assert!(h.scheme().membership.active("E002").is_some());
    assert_eq!(h.scheme().membership.active("E003").unwrap().attrs.salary_minor, Some(95_000_00));

    // The same leaver again, and this time staff approve it.
    h.upload("f3", FileKind::Changes, vec![rows[0].clone()]).unwrap();
    h.send_as(
        caller::EMPLOYER,
        "admin-cy",
        file_cmd(|scheme, file| GroupCommand::ApproveFile { scheme, file }, "f3"),
    )
    .unwrap();
    let before = h.owed();
    h.staff(GroupCommand::DecideBackdating {
        scheme: SCHEME.into(),
        file: "f3".into(),
        approve: true,
    })
    .unwrap();
    assert!(h.scheme().membership.active("E002").is_none());
    assert!(h.owed() < before, "premium billed since the leaving date comes back");
    assert_eq!(h.core_lives(), vec!["E001", "E003", "E004"]);
}

/// Purpose: a joiner who might be a person already known joins unlinked, is raised for a
/// person to decide with reinsurance told of each candidate, and is linked only by that
/// decision or by the member's own verified registration.
/// Why it matters: linking on a guess merges two people's cover and discloses one's
/// policies to the other; not noticing the possibility lets one life exceed the insurer's
/// retention across two policies. The design allows neither.
#[test]
fn a_possible_match_is_never_linked_on_a_guess() {
    let mut h = Harness::new(terms());
    h.upload("f1", FileKind::Full, first_file()).unwrap();
    h.approve("f1").unwrap();
    h.resolve("f1", |id| {
        (id == "E002").then(|| Resolution::Possible {
            candidates: vec![party("party-a"), party("party-b")],
        })
    });
    let m = h.scheme().membership.active("E002").unwrap();
    assert_eq!(m.party, None);
    let noted: Vec<_> = h.notices.iter().filter(|n| matches!(n, ReinsuranceNotice::NotePossibleMatch { .. })).collect();
    assert_eq!(noted.len(), 2);
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::PossibleMatch && e.subject == "E002"));
    assert!(h.saw(|e| matches!(e, GroupEvent::PossibleMatchRaised { candidates: 2, .. })));

    let decide = |h: &mut Harness, id: &str, decision| {
        h.staff(GroupCommand::ResolveMatch {
            scheme: SCHEME.into(),
            employee_id: id.into(),
            decision,
        })
    };
    use ic_group::command::MatchDecision::{DifferentPerson, SamePerson};
    assert_eq!(code(decide(&mut h, "E003", SamePerson { party: party("party-a") })), "no_possible_match");
    assert_eq!(code(decide(&mut h, "E002", SamePerson { party: party("party-z") })), "not_a_candidate");
    assert_eq!(code(decide(&mut h, "E002", DifferentPerson { party: party("party-a") })), "not_a_candidate");
    decide(&mut h, "E002", SamePerson { party: party("party-b") }).unwrap();
    assert_eq!(h.scheme().membership.active("E002").unwrap().party, Some(party("party-b")));
    assert!(h.scheme().possible.is_empty());
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::PossibleMatch));
    assert_eq!(
        h.notices.iter().filter(|n| matches!(n, ReinsuranceNotice::ClearPossibleMatch { .. })).count(),
        2
    );
    assert!(h.saw(|e| matches!(e, GroupEvent::MemberLinked { how, .. } if how == "resolved_same_person")));
    // The core holds the link too.
    let core = &h.c.state(h.p).policies[&h.policy].lives[&life("E002")];
    assert!(matches!(&core.kind, ic_core::LifeKind::UnnamedMember { linked_party: Some(p), .. } if *p == party("party-b")));

    // Self-registration: the same person again is nothing; another person is refused.
    let register = |h: &mut Harness, id: &str, who: &str| {
        let r = h.send_as(
            caller::MEMBER,
            who,
            GroupCommand::SelfRegister {
                scheme: SCHEME.into(),
                employee_id: id.into(),
                party: party(who),
            },
        );
        h.settle();
        r
    };
    register(&mut h, "E002", "party-b").unwrap();
    assert_eq!(code(register(&mut h, "E002", "party-c")), "already_identified");
    assert_eq!(code(register(&mut h, "E999", "party-c")), "unknown_member");
}

/// Purpose: on a scheme that gives no names, members join without anyone being asked who
/// they are, and a member who registers with verified identity is linked from then on.
/// Why it matters: an unnamed scheme has no names to match, so asking the customer master
/// would stall every file. The member's own registration is the one moment the insurer
/// learns who the life is, and reinsurance needs it.
#[test]
fn an_unnamed_scheme_links_a_member_only_when_they_register() {
    let mut t = terms();
    t.basis = ic_group::terms::MemberBasis::Unnamed;
    let mut h = Harness::new(t);
    h.upload("f1", FileKind::Full, first_file()).unwrap();
    h.approve("f1").unwrap();
    assert!(h.to_resolve.is_empty());
    assert_eq!(h.scheme().files["f1"].status, FileStatus::Applied);
    assert_eq!(h.scheme().membership.active("E002").unwrap().party, None);
    h.send_as(
        caller::MEMBER,
        "party-m",
        GroupCommand::SelfRegister {
            scheme: SCHEME.into(),
            employee_id: "E002".into(),
            party: party("party-m"),
        },
    )
    .unwrap();
    h.settle();
    assert_eq!(h.scheme().membership.active("E002").unwrap().party, Some(party("party-m")));
    assert!(h.saw(|e| matches!(e, GroupEvent::MemberLinked { how, .. } if how == "self_registered")));
}

/// Purpose: a year of monthly full files on a named scheme of 5,000 members, each with
/// joiners, leavers and pay changes, goes through preview, approval and the core in
/// chunks; after every month the module's membership is the core's, the invoice counts
/// every member once and its amount due is what the core says is owed, the daily
/// reconciliation is clean on every one of the year's days, and nothing is left queued
/// or raised to staff but the renewal at the year's end.
/// Why it matters: every other test here uses a handful of members. Chunking, the
/// one-command-in-flight rule and the invoice built from many chunk events only do real
/// work at this size, and a scheme this large is an ordinary customer. A count that
/// drifted by one member a month would be a wrong invoice by the end of the year.
#[test]
fn twelve_monthly_files_on_a_scheme_of_five_thousand() {
    const SIZE: u32 = 5_000;
    let mut h = Harness::new(terms());
    let id = |n: u32| format!("E{n:05}");
    let member = |n: u32, (salary, hired): (i64, Option<chrono::NaiveDate>)| {
        let mut a = attrs("staff", salary, if n.is_multiple_of(2) { "akl" } else { "wlg" });
        // Someone hired during the year starts work, and cover, on the day they were hired.
        a.employment_start = hired.or(a.employment_start);
        row(n, &id(n), a)
    };

    // Who is employed, and on what pay: E00001..E05000 to start, beside the founder (E001),
    // who is on the scheme already and in every file.
    let mut staff: BTreeMap<u32, (i64, Option<chrono::NaiveDate>)> = (1..=SIZE).map(|n| (n, (60_000_00, None))).collect();
    let mut next = SIZE + 1;
    let mut first = true;
    for month in 1..=12u32 {
        if !first {
            // Thirty leave, forty join, fifty get a rise.
            let leavers: Vec<u32> = staff.keys().copied().filter(|n| n % 167 == month % 167).take(30).collect();
            for n in leavers {
                staff.remove(&n);
            }
            for _ in 0..40 {
                staff.insert(next, (60_000_00, Some(h.today())));
                next += 1;
            }
            let raised: Vec<u32> = staff.keys().copied().filter(|n| n % 97 == month % 97).take(50).collect();
            for n in raised {
                staff.get_mut(&n).unwrap().0 += 1_000_00;
            }
        }
        let mut rows: Vec<_> = staff.iter().map(|(n, who)| member(*n, *who)).collect();
        rows.push(row(900_000, "E001", attrs("staff", 60_000_00, "akl")));
        let members = staff.len() + 1;
        let file = format!("month-{month:02}");
        first = false;
        h.apply(&file, FileKind::Full, rows);
        let s = h.scheme();
        assert!(s.files[&file].rejected.is_empty(), "{file}: {:?}", s.files[&file].rejected.first());
        assert_eq!(s.files[&file].status, FileStatus::Applied, "{file}");
        assert_eq!(s.membership.active_count(), members, "{file}");
        assert_eq!(h.core_lives().len(), members, "{file}: the core has the same members");
        assert!(s.queue.is_empty() && s.in_flight.is_none(), "{file}: nothing is left to apply");

        // The rest of the month, each day reconciled, then the invoice and the payment.
        let issued = h.scheme().invoices.back().map(|i| i.period_index);
        for _ in 0..32 {
            h.days(1);
            if h.scheme().invoices.back().map(|i| i.period_index) != issued {
                break;
            }
        }
        let s = h.scheme();
        let invoice = s.invoices.back().unwrap();
        assert_eq!(invoice.members() as usize, members, "{file}: every member is on the invoice once");
        assert_eq!(invoice.amount_due_minor(), h.owed(), "{file}");
        h.pay(h.owed());
        assert_eq!(h.scheme().outstanding_minor, 0);
    }
    assert_eq!(staff.len() as u32, SIZE + 11 * 10);
    // The twelfth invoice falls on the renewal date, and this test agrees no renewal: that
    // is the one thing staff are asked to look at after the whole year.
    let raised: Vec<_> = h.g.exceptions.values().map(|e| e.kind).collect();
    assert_eq!(raised, vec![ExceptionKind::RenewalNotAgreed]);
}

/// Purpose: when a file changes something reinsurance prices on (here a member who now
/// smokes), reinsurance is sent that member's risk factors again; a change of salary alone
/// sends nothing.
/// Why it matters: the reinsurance premium for a life is worked out from the factors last
/// sent. A member recorded as a non-smoker for ever is ceded at the wrong rate, and the
/// difference is found only when a claim is recovered.
#[test]
fn a_change_to_a_priced_detail_is_sent_to_reinsurance_again() {
    let mut h = Harness::new(terms());
    h.apply("f1", FileKind::Full, first_file());
    let factors = |h: &Harness| -> Vec<Option<bool>> {
        h.notices
            .iter()
            .filter_map(|n| match n {
                ReinsuranceNotice::RecordRiskFactors { life: l, smoker, .. } if *l == life("E002") => Some(*smoker),
                _ => None,
            })
            .collect()
    };
    assert_eq!(factors(&h), vec![None]);

    let today = h.today();
    let raise = attrs("staff", 65_000_00, "wlg");
    h.apply("f2", FileKind::Changes, vec![change(1, "E002", RowChange::Change, today, raise.clone())]);
    assert_eq!(factors(&h), vec![None], "a raise is not a risk factor");

    let smokes = ic_group::record::MemberAttrs { smoker: Some(true), ..raise };
    h.apply("f3", FileKind::Changes, vec![change(1, "E002", RowChange::Change, today, smokes)]);
    assert_eq!(factors(&h), vec![None, Some(true)]);
}
