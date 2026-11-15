//! Cover above the automatic amount: asking underwriting, writing the
//! decision to the core as a layer, keeping it in line with the formula as
//! salaries move, and the ceiling reinsurance can ask for.

mod common;

use common::*;
use ic_core::Reject;
use ic_group::command::{caller, ExcessDecision, GroupCommand};
use ic_group::event::{ExceptionKind, GroupEvent};
use ic_group::mapping::FileKind;
use ic_group::record::RowChange;
use ic_group::state::CaseStatus;
use ic_group::terms::ExcessLoading;
use ic_money::DecStr;

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

/// The founder, one staff member and an executive on 240,000, whose formula
/// cover of 960,000 is 210,000 above the automatic 750,000.
fn with_executive(t: ic_group::terms::SchemeTerms) -> (Harness, u64) {
    let mut h = Harness::new(t);
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E003", attrs("staff", 90_000_00, "akl")),
            row(3, "E004", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    let case = h.requests[0].case;
    (h, case)
}

fn accept(minor: i64, loading: Option<&str>, consent: bool) -> ExcessDecision {
    ExcessDecision::Accepted {
        sum_insured_minor: minor,
        loadings: loading.map(|l| DecStr::parse(l).unwrap()).into_iter().collect(),
        exclusions: vec![],
        member_consents_to_itemising: consent,
        member_pays_loading: false,
    }
}

fn status(h: &Harness, case: u64) -> CaseStatus {
    h.scheme().cases[&case].status
}

/// Purpose: an accepted decision becomes a layer in the core for exactly the accepted
/// amount from the day the member joined, the employer learns only that it is in place,
/// and when the member's salary falls the layer ends from that day.
/// Why it matters: until the layer is in the core the member is not covered above the
/// automatic amount whatever underwriting said; and a layer that outlives the salary that
/// justified it is cover nobody is entitled to and the employer is billed for.
#[test]
fn an_accepted_decision_becomes_a_layer_and_ends_when_the_formula_no_longer_supports_it() {
    let (mut h, case) = with_executive(terms());
    assert_eq!(status(&h, case), CaseStatus::Requested);
    assert!(h.excess_layers("E004").is_empty());
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessUnderwritingRequested { excess_minor: 210_000_00, .. })));

    // Only underwriting decides, and only within what was asked.
    let decide = GroupCommand::RecordExcessDecision {
        scheme: SCHEME.into(),
        case,
        decision: accept(210_000_00, None, false),
    };
    assert_eq!(code(h.send_as(caller::STAFF, "staff-ana", decide)), "caller_not_allowed");
    assert_eq!(code(h.underwriting(case, accept(0, None, false))), "invalid_amount");
    assert_eq!(code(h.underwriting(case, accept(210_000_01, None, false))), "invalid_amount");
    assert_eq!(code(h.underwriting(999, accept(1, None, false))), "case_closed");

    h.underwriting(case, accept(210_000_00, Some("0.5"), false)).unwrap();
    assert_eq!(status(&h, case), CaseStatus::Accepted);
    assert_eq!(h.excess_layers("E004"), vec![(210_000_00, None)]);
    assert!(
        h.scheme().cases[&case].layer.is_some(),
        "the core's number for the layer is learned from its event"
    );
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessCoverDecided { in_place: true, .. })));
    assert_eq!(code(h.underwriting(case, ExcessDecision::Declined)), "case_closed");
    // Nothing the module publishes says what the terms were.
    let published = serde_json::to_string(&h.published).unwrap();
    assert!(!published.contains("loading") && !published.contains("0.5"), "{published}");

    // Salary falls to 180,000: four times is 720,000, inside the automatic amount.
    h.days(40);
    let today = h.today();
    h.salary("f2", "E004", "executive", 180_000_00);
    assert_eq!(status(&h, case), CaseStatus::Ended);
    assert_eq!(h.excess_layers("E004"), vec![(210_000_00, Some(today))]);
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessCoverEnded { .. })));
    assert_eq!(h.requests.len(), 1, "nothing new to underwrite");
}

/// Purpose: a declined or abandoned case leaves the member on the automatic amount with
/// no layer, and a partial acceptance is topped up or cut back as the salary moves, newest
/// request first.
/// Why it matters: the member's cover above the automatic amount must always be what
/// underwriting accepted and no more than the formula gives. Each of these paths is a
/// place where the two can drift apart.
#[test]
fn declined_abandoned_and_partial_decisions_keep_cover_within_the_formula() {
    let (mut h, case) = with_executive(terms());
    h.underwriting(case, ExcessDecision::Declined).unwrap();
    assert_eq!(status(&h, case), CaseStatus::Declined);
    assert!(h.excess_layers("E004").is_empty());
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessCoverDecided { in_place: false, .. })));

    // A pay rise asks again for the whole excess, now 250,000; underwriting accepts 100,000.
    h.salary("f2", "E004", "executive", 250_000_00);
    let second = h.requests[1].case;
    assert_eq!(h.requests[1].excess_minor, 250_000_00);
    h.underwriting(second, accept(100_000_00, None, false)).unwrap();
    assert_eq!(h.excess_layers("E004"), vec![(100_000_00, None)]);

    // Another rise: only the part not already held is asked for.
    h.salary("f3", "E004", "executive", 300_000_00);
    let third = h.requests[2].case;
    assert_eq!(h.requests[2].excess_minor, 350_000_00);
    assert_eq!(status(&h, third), CaseStatus::Requested);

    // A cut to 200,000 leaves 50,000 above automatic: the open request is withdrawn, the
    // 100,000 layer ends, and 50,000 is asked for afresh.
    h.salary("f4", "E004", "executive", 200_000_00);
    assert_eq!(status(&h, third), CaseStatus::NotProceeded);
    assert_eq!(status(&h, second), CaseStatus::Ended);
    assert_eq!(h.requests[3].excess_minor, 50_000_00);
    let fourth = h.requests[3].case;
    h.underwriting(fourth, ExcessDecision::NotProceeded).unwrap();
    assert_eq!(status(&h, fourth), CaseStatus::NotProceeded);
    assert!(h.excess_layers("E004").iter().all(|(_, end)| end.is_some()));
}

/// Purpose: on a scheme that itemises loadings, a loaded decision is refused without the
/// member's recorded consent, and with it the core's layer is marked for itemising; on a
/// scheme that bills loadings as one total, consent is not asked and nothing is itemised.
/// Why it matters: a loaded premium on a member's own invoice line tells the employer
/// that the member has a health condition. That may happen only with the member's consent.
#[test]
fn a_loading_is_itemised_to_the_employer_only_with_the_members_consent() {
    let mut t = terms();
    t.excess_loading = ExcessLoading::EmployerItemised;
    let (mut h, case) = with_executive(t);
    assert_eq!(code(h.underwriting(case, accept(210_000_00, Some("0.5"), false))), "consent_required");
    assert_eq!(status(&h, case), CaseStatus::Requested);
    h.underwriting(case, accept(210_000_00, Some("0.5"), true)).unwrap();
    let itemised = |h: &Harness| {
        h.c.state(h.p).policies[&h.policy]
            .covers
            .values()
            .flat_map(|c| c.layers.iter())
            .filter(|l| l.life == life("E004"))
            .any(|l| l.loading_itemised)
    };
    assert!(itemised(&h));

    let (mut h, case) = with_executive(terms());
    h.underwriting(case, accept(210_000_00, Some("0.5"), true)).unwrap();
    assert!(!itemised(&h), "consent alone does not itemise on a scheme that bills one total");
}

/// Purpose: when reinsurance says part of a member's automatic cover must be underwritten,
/// the member's automatic cover is capped in the core from that day and the part above
/// the cap goes to underwriting under reinsurance's reference.
/// Why it matters: this is the group half of one life on two policies. Without the cap
/// the insurer carries more on one life than its retention the moment a member turns out
/// to hold a retail policy too.
#[test]
fn reinsurance_can_require_part_of_automatic_cover_to_be_underwritten() {
    let (mut h, _) = with_executive(terms());
    let cover = h.requests[0].cover.clone();
    let require = |h: &mut Harness, policy: &ic_core::PolicyId, who: &str, cover: &str, excess_minor: i64| {
        let r = h.send_as(
            caller::REINSURANCE,
            "reinsurance",
            GroupCommand::RequireUnderwrittenExcess {
                policy: policy.clone(),
                life: life(who),
                cover: cover.to_string(),
                excess_minor,
                reference: "ri-accum-7".into(),
            },
        );
        h.settle();
        r
    };
    let policy = h.policy.clone();
    assert_eq!(code(require(&mut h, &ic_core::PolicyId("nope".into()), "E003", &cover, 1)), "unknown_policy");
    assert_eq!(code(require(&mut h, &policy, "E999", &cover, 1)), "unknown_member");
    assert_eq!(code(require(&mut h, &policy, "E003", "dental", 1)), "unknown_cover");
    assert_eq!(code(require(&mut h, &policy, "E003", &cover, 0)), "invalid_amount");
    assert_eq!(code(require(&mut h, &policy, "E003", &cover, 180_000_01)), "invalid_amount");

    // E003 has 180,000 automatic. 80,000 of it must be underwritten.
    require(&mut h, &policy, "E003", &cover, 80_000_00).unwrap();
    assert_eq!(h.scheme().membership.active("E003").unwrap().auto_cap_minor, Some(100_000_00));
    let cap_in_core = |h: &Harness| {
        let core = &h.c.state(h.p).policies[&h.policy].lives[&life("E003")];
        core.factors.as_of(h.today()).unwrap().auto_cover_cap_minor
    };
    assert_eq!(cap_in_core(&h), Some(100_000_00));
    let request = h.requests.last().unwrap();
    assert_eq!((request.life.clone(), request.excess_minor), (life("E003"), 80_000_00));
    assert_eq!(request.reference.as_deref(), Some("ri-accum-7"));
    h.underwriting(request.case, accept(80_000_00, None, false)).unwrap();
    assert_eq!(h.excess_layers("E003"), vec![(80_000_00, None)]);
    // A later salary change keeps the cap.
    h.salary("f2", "E003", "staff", 95_000_00);
    assert_eq!(cap_in_core(&h), Some(100_000_00));
    assert_eq!(h.requests.last().unwrap().excess_minor, 10_000_00);
}

/// Purpose: a member who leaves loses the layer with the membership and any open request
/// is dropped; put back after a removal in error, the layer is back as it was with no gap.
/// Why it matters: runbook 3 (an employer file removed people in error) promises cover
/// "as if they had never left". Underwritten cover is the part a member cannot get back
/// by rejoining, because rejoining would mean underwriting again.
#[test]
fn a_layer_follows_the_member_out_and_back() {
    let (mut h, case) = with_executive(terms());
    h.underwriting(case, accept(210_000_00, None, false)).unwrap();
    h.salary("f2", "E003", "executive", 200_000_00);
    let open_request = h.requests.last().unwrap().case;
    h.days(5);
    let today = h.today();
    h.apply(
        "f3",
        FileKind::Changes,
        vec![
            change(1, "E004", RowChange::Leave, today, attrs("executive", 240_000_00, "wlg")),
            change(2, "E003", RowChange::Leave, today, attrs("executive", 200_000_00, "wlg")),
        ],
    );
    assert_eq!(status(&h, case), CaseStatus::Ended);
    assert_eq!(status(&h, open_request), CaseStatus::NotProceeded);
    assert_eq!(h.excess_layers("E004"), vec![(210_000_00, Some(today))]);
    assert!(h.scheme().offers.values().any(|o| o.employee_id == "E004"));

    h.days(3);
    let reinstate = |h: &mut Harness, ids: &[&str]| {
        h.staff(GroupCommand::Reinstate {
            scheme: SCHEME.into(),
            employee_ids: ids.iter().map(|s| s.to_string()).collect(),
            reason: "file f3 removed them in error".into(),
        })
    };
    assert_eq!(code(reinstate(&mut h, &[])), "nothing_to_do");
    assert_eq!(code(reinstate(&mut h, &["E004", "E001"])), "not_left");
    reinstate(&mut h, &["E004"]).unwrap();
    assert!(h.scheme().membership.active("E004").is_some());
    assert_eq!(status(&h, case), CaseStatus::Accepted);
    assert_eq!(h.excess_layers("E004"), vec![(210_000_00, None)]);
    assert!(h.core_lives().contains(&"E004".to_string()));
    assert!(h.saw(|e| matches!(e, GroupEvent::MembersReinstated { count: 1, .. })));
    assert!(h.saw(|e| matches!(e, GroupEvent::ContinuationExpired { .. })), "there is nothing to continue");
    assert!(h.scheme().membership.active("E003").is_none());
}

/// Purpose: if the core refuses to write an accepted layer the case is closed as not in
/// place and raised to staff; and a layer whose end is already due when the core accepts
/// it is ended whichever of the core's answer and the core's event arrives first.
/// Why it matters: "accepted" on the underwriter's screen with no layer in the core is a
/// member who believes they are covered and is not, so it must be loud. And the order of
/// answer and event is not guaranteed between two replicated groups.
#[test]
fn a_refused_layer_is_loud_and_a_due_ending_survives_either_order() {
    let (mut h, case) = with_executive(terms());
    h.core_faults.push_back(Some("exceeds_maximum".into()));
    h.underwriting(case, accept(210_000_00, None, false)).unwrap();
    assert_eq!(status(&h, case), CaseStatus::NotProceeded);
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessCoverDecided { in_place: false, .. })));
    assert!(h
        .g
        .exceptions
        .values()
        .any(|e| e.kind == ExceptionKind::RowRefused && e.subject == format!("case:{case}")));
    // Underwriting accepted these terms and is told the core would not write them.
    assert_eq!(
        h.outcomes,
        vec![(
            format!("grp-uw:{SCHEME}:{case}"),
            ic_group::event::IssueOutcome::Rejected {
                code: "exceeds_maximum".into()
            }
        )]
    );

    for events_first in [false, true] {
        let (mut h, case) = with_executive(terms());
        h.days(10);
        let today = h.today();
        // A salary cut is on its way to the core when underwriting's acceptance arrives.
        h.upload(
            "f2",
            FileKind::Changes,
            vec![change(1, "E004", RowChange::Change, today, attrs("executive", 180_000_00, "wlg"))],
        )
        .unwrap();
        let approve = GroupCommand::ApproveFile {
            scheme: SCHEME.into(),
            file: "f2".into(),
        };
        h.send_as(caller::EMPLOYER, "admin-bo", approve).unwrap();
        h.send_as(
            caller::UNDERWRITING,
            "underwriter-di",
            GroupCommand::RecordExcessDecision {
                scheme: SCHEME.into(),
                case,
                decision: accept(210_000_00, None, false),
            },
        )
        .unwrap();
        assert_eq!(status(&h, case), CaseStatus::Writing);
        if events_first {
            h.core_events_before_answer();
            assert_eq!(h.scheme().cases[&case].end_from, Some(today));
            h.core_events_before_answer();
        }
        h.settle();
        assert_eq!(status(&h, case), CaseStatus::Ended, "events first: {events_first}");
        assert_eq!(h.excess_layers("E004"), vec![(210_000_00, Some(today))]);
    }

    // Ending a layer the core no longer has closes the case all the same.
    let (mut h, case) = with_executive(terms());
    h.underwriting(case, accept(210_000_00, None, false)).unwrap();
    h.core_faults.push_back(None);
    h.core_faults.push_back(Some("unknown_layer".into()));
    h.salary("f2", "E004", "executive", 180_000_00);
    assert_eq!(status(&h, case), CaseStatus::Ended);
}

/// Purpose: when the core refuses a cover ceiling that reinsurance asked for, the refusal
/// is reported against the member as a ceiling, raised as an exception, and the module
/// does not show a ceiling the core does not hold.
/// Why it matters: staff clear these from the exception list. A refusal filed under an
/// empty or wrong subject cannot be found, and reinsurance would go on believing the
/// member's accumulation had been brought back under the retention.
#[test]
fn refused_work_is_reported_by_what_it_was_about() {
    let (mut h, _) = with_executive(terms());
    let refused = |h: &Harness, subject: &str, kind: &str| {
        h.saw(|e| matches!(e, GroupEvent::RowRefused { employee_id, change, code, .. } if employee_id == subject && change == kind && code == "refused_here"))
    };
    let cover = h.requests[0].cover.clone();

    h.core_faults.push_back(Some("refused_here".into()));
    h.send_as(
        caller::REINSURANCE,
        "reinsurance",
        GroupCommand::RequireUnderwrittenExcess {
            policy: h.policy.clone(),
            life: life("E003"),
            cover,
            excess_minor: 100_000_00,
            reference: "ri-accum-9".into(),
        },
    )
    .unwrap();
    h.settle();
    assert!(refused(&h, "E003", "cover_ceiling"), "{:?}", h.published);
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::RowRefused && e.subject == "E003"));
    assert_eq!(
        h.scheme().membership.active("E003").unwrap().auto_cap_minor,
        None,
        "no ceiling the core does not hold"
    );
}

/// The payer the core holds on a member's underwritten layers.
fn payers(h: &Harness, id: &str) -> Vec<ic_core::policy::MemberPays> {
    h.c.state(h.p).policies[&h.policy]
        .covers
        .values()
        .flat_map(|c| c.layers.iter())
        .filter(|l| l.life == life(id) && l.origin == ic_core::LayerOrigin::UnderwrittenExcess)
        .map(|l| l.member_pays)
        .collect()
}

/// Purpose: on a scheme whose terms bill loadings to the member, a loaded layer is written
/// with the member paying the loading and an unloaded one with the employer paying all of
/// it; the member's part is not on the scheme's invoice; and the count that protects a
/// total is refused on terms that have no total.
/// Why it matters: the setting decides who is asked for money that reveals a health
/// loading. Billing the employer where the member was told they would pay discloses the
/// loading to the employer; billing nobody is cover given away.
#[test]
fn a_loading_is_billed_to_the_member_where_the_terms_say_so() {
    use ic_core::policy::MemberPays;

    let mut t = terms();
    t.excess_loading = ExcessLoading::MemberPays;
    let mut bad = t.clone();
    bad.excess_loading_min_count = 2;
    assert!(bad.validate().unwrap_err().contains("minimum count"));

    let (mut h, case) = with_executive(t.clone());
    let owed_before = h.scheme().outstanding_minor;
    h.underwriting(case, accept(210_000_00, Some("0.5"), false)).unwrap();
    assert_eq!(status(&h, case), CaseStatus::Accepted);
    assert_eq!(payers(&h, "E004"), vec![MemberPays::Loading]);
    let with_loading = h.scheme().outstanding_minor - owed_before;

    let (mut h, case) = with_executive(t);
    let owed_before = h.scheme().outstanding_minor;
    h.underwriting(case, accept(210_000_00, None, false)).unwrap();
    assert_eq!(payers(&h, "E004"), vec![MemberPays::Nothing]);
    assert_eq!(
        h.scheme().outstanding_minor - owed_before,
        with_loading,
        "the employer owes the standard rate either way"
    );
    assert!(with_loading > 0);
    // Nothing the module publishes says who has a loading.
    assert!(!format!("{:?}", h.published).contains("loading"));
}

/// Purpose: where the employer pays loadings as one total with a minimum count, a member
/// whose terms are accepted while fewer members than that hold underwritten cover pays
/// their own loading, and one accepted once the count is reached is in the employer's
/// total.
/// Why it matters: a total of loadings over one or two members is those members'
/// health loading, disclosed to their employer. The count is the only thing that stops
/// the default billing option doing that on a small scheme.
#[test]
fn too_few_members_for_a_total_means_the_member_pays_the_loading() {
    use ic_core::policy::MemberPays;

    let mut t = terms();
    t.excess_loading_min_count = 2;
    let mut h = Harness::new(t);
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E004", attrs("executive", 240_000_00, "wlg")),
            row(3, "E005", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    let case_of = |h: &Harness, id: &str| h.requests.iter().find(|r| r.life == life(id)).unwrap().case;
    let (first, second) = (case_of(&h, "E004"), case_of(&h, "E005"));

    h.underwriting(first, accept(210_000_00, Some("0.5"), false)).unwrap();
    assert_eq!(payers(&h, "E004"), vec![MemberPays::Loading], "one member alone would be the total");
    h.underwriting(second, accept(210_000_00, Some("0.25"), false)).unwrap();
    assert_eq!(payers(&h, "E005"), vec![MemberPays::Nothing], "two members make a total");
    // What the first member was told at their offer stands.
    assert_eq!(payers(&h, "E004"), vec![MemberPays::Loading]);
}

/// Purpose: when a leaver takes the members with underwritten cover below the scheme's
/// minimum count while a loading is still in the employer's total, staff are told once,
/// nobody's payer is changed, the alert names no member, and it clears by itself when
/// another member's cover brings the number back up.
/// Why it matters: who pays is settled when terms are accepted, so nothing else notices
/// that a total which hid two members' loadings now shows one member's health loading
/// to their employer. Moving the loading to the member without telling them would break
/// what their offer said, so a person has to decide; this alert is how they find out.
#[test]
fn staff_are_told_when_leavers_leave_too_few_members_in_the_total() {
    use ic_core::policy::MemberPays;

    let mut t = terms();
    t.excess_loading_min_count = 2;
    let mut h = Harness::new(t);
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E004", attrs("executive", 240_000_00, "wlg")),
            row(3, "E005", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    let case_of = |h: &Harness, id: &str| h.requests.iter().rev().find(|r| r.life == life(id)).unwrap().case;
    let alerts = |h: &Harness| h.g.exceptions.values().filter(|e| e.kind == ExceptionKind::LoadingTotalTooSmall).count();

    // The first pays their own loading; the second, with two holders, goes in the total.
    h.underwriting(case_of(&h, "E004"), accept(210_000_00, Some("0.5"), false)).unwrap();
    assert_eq!(alerts(&h), 0, "nothing is in the employer's total yet");
    h.underwriting(case_of(&h, "E005"), accept(210_000_00, Some("0.25"), false)).unwrap();
    assert_eq!(alerts(&h), 0, "two holders hide whose loading it is");

    let today = h.today();
    h.apply(
        "leave",
        FileKind::Changes,
        vec![change(1, "E004", RowChange::Leave, today, attrs("executive", 240_000_00, "wlg"))],
    );
    assert_eq!(alerts(&h), 1, "one holder is left and their loading is the whole total");
    assert!(h.saw(|e| matches!(e, GroupEvent::ExceptionRaised { kind: ExceptionKind::LoadingTotalTooSmall, subject, .. } if subject == "loadings")));
    assert_eq!(payers(&h, "E005"), vec![MemberPays::Nothing], "what the member was told stands");
    // Staff see whose loading is in the total, with the proposal to ask underwriting to re-offer.
    let scheme = h.scheme().id.clone();
    let listed = ic_group::host::answer(&h.g, serde_json::json!({ "ask": "loadings_in_total", "scheme": scheme })).unwrap();
    let cases = listed["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 1, "{listed}");
    assert_eq!(cases[0]["employee"], "E005");
    assert_eq!(cases[0]["proposal"], format!("grp-uw:{scheme}:{}", case_of(&h, "E005")));
    assert_eq!(listed["min_count"], 2);
    assert!(ic_group::host::answer(&h.g, serde_json::json!({ "ask": "loadings_in_total", "scheme": "nope" })).is_err());
    h.days(2);
    assert_eq!(alerts(&h), 1, "it is raised once and stays until it is dealt with");

    // Another executive joins and is accepted: two holders again.
    h.apply(
        "join",
        FileKind::Changes,
        vec![change(1, "E006", RowChange::Join, h.today(), attrs("executive", 240_000_00, "wlg"))],
    );
    h.underwriting(case_of(&h, "E006"), accept(210_000_00, None, false)).unwrap();
    assert_eq!(alerts(&h), 0);
    assert!(h.saw(|e| matches!(
        e,
        GroupEvent::ExceptionCleared {
            kind: ExceptionKind::LoadingTotalTooSmall,
            ..
        }
    )));
}

/// Purpose: a member who pays their own loading and stops paying loses that cover when
/// the core lapses it: the case ends, the employer hears only that the cover is no longer
/// in place, and the member's automatic cover and the scheme itself carry on. When the
/// member later asks for the cover back, staff ask underwriting again, and the cover it
/// accepts is in place from that day.
/// Why it matters: the module would otherwise go on showing underwritten cover the core
/// has ended, and a claim would be assessed against cover that is not there. And a
/// member's own arrears must never put their colleagues' cover, or their own automatic
/// cover, at risk. Without a way back, a lapsed member would wait for a pay rise to be
/// asked again.
#[test]
fn a_member_who_stops_paying_their_loading_loses_only_that_cover() {
    let mut t = terms();
    t.excess_loading = ExcessLoading::MemberPays;
    let (mut h, case) = with_executive(t);
    h.underwriting(case, accept(210_000_00, Some("0.5"), false)).unwrap();
    assert_eq!(status(&h, case), CaseStatus::Accepted);

    // The employer pays every month. The member never does.
    for _ in 0..8 {
        h.days(30);
        let owed = h.scheme().outstanding_minor;
        if owed > 0 {
            h.pay(owed);
        }
    }
    assert_eq!(status(&h, case), CaseStatus::Ended);
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessCoverEnded { case: c, .. } if *c == case)));
    let layers = h.excess_layers("E004");
    assert_eq!(layers.len(), 1);
    assert!(layers[0].1.is_some(), "the core ended the layer");
    assert!(h.scheme().membership.active("E004").is_some(), "still a member, on automatic cover");
    assert_eq!(h.scheme().status, ic_group::state::SchemeStatus::Active);
    assert!(h.core_lives().contains(&"E004".to_string()));

    // Later the member asks for the cover back. Staff ask underwriting again: a new request
    // from today, not from when the member joined; asked twice, the second is refused.
    let again = |h: &mut Harness, id: &str, reason: &str| {
        h.staff(GroupCommand::RequestExcessAgain {
            scheme: SCHEME.into(),
            employee_id: id.into(),
            reason: reason.into(),
        })
    };
    assert_eq!(code(again(&mut h, "E004", " ")), "missing_value");
    assert_eq!(code(again(&mut h, "E999", "asked by phone")), "unknown_member");
    assert_eq!(code(again(&mut h, "E001", "asked by phone")), "nothing_to_ask");
    let asked = h.today();
    let before = h.requests.len();
    again(&mut h, "E004", "member asked for the cover back and set up a direct debit").unwrap();
    let request = h.requests[before].clone();
    assert_eq!((request.excess_minor, request.effective), (210_000_00, asked));
    assert_eq!(code(again(&mut h, "E004", "asked again")), "case_open");
    h.underwriting(request.case, accept(210_000_00, Some("0.5"), false)).unwrap();
    assert_eq!(h.excess_layers("E004").last(), Some(&(210_000_00, None)));
    assert_eq!(status(&h, request.case), CaseStatus::Accepted);
}

/// Purpose: a member who pays the loading that is theirs to pay keeps the cover, month
/// after month: their payments settle only what they owe, the employer's balance is
/// neither reduced by them nor increased by the member's part, the case stays in place
/// and nothing is raised to staff.
/// Why it matters: the lapse test shows what happens to a member who does not pay; this
/// is the ordinary case beside it. If a member's payment were set against the scheme's
/// invoice, the employer would be undercharged and the member lapsed for money they had
/// paid.
#[test]
fn a_member_who_pays_their_loading_keeps_the_cover() {
    let mut t = terms();
    t.excess_loading = ExcessLoading::MemberPays;
    let (mut h, case) = with_executive(t);
    h.underwriting(case, accept(210_000_00, Some("0.5"), false)).unwrap();
    let member = life("E004");
    let members_debt = |h: &Harness| h.c.state(h.p).policies[&h.policy].member_outstanding_minor(&life("E004"));

    let mut paid_by_member = 0;
    for month in 0..8 {
        h.days(30);
        let schemes = h.scheme().outstanding_minor;
        let theirs = members_debt(&h);
        assert_eq!(h.owed(), schemes + theirs, "the two debts are kept apart and add up");
        if theirs > 0 {
            let command = ic_core::Command::ApplyMemberReceipt {
                policy: h.policy.clone(),
                life: member.clone(),
                receipt: format!("member-receipt-{month}"),
                amount: ic_money::Money::new(theirs, ic_money::Currency::NZD),
            };
            h.c.submit(h.p, "cash-matching", command).unwrap();
            h.settle();
            paid_by_member += theirs;
            assert_eq!(members_debt(&h), 0);
            assert_eq!(h.scheme().outstanding_minor, schemes, "the member's money is not the employer's");
        }
        if schemes > 0 {
            h.pay(schemes);
            assert_eq!(h.owed(), 0);
        }
    }
    assert!(paid_by_member > 0, "the member was billed for the loading");
    assert_eq!(status(&h, case), CaseStatus::Accepted);
    assert_eq!(h.excess_layers("E004"), vec![(210_000_00, None)]);
    assert!(h.g.exceptions.is_empty(), "{:?}", h.g.exceptions);
}

/// Purpose: when staff have underwriting offer a member their loading to pay, because too
/// few members are left for the employer's total to hide it, and the member agrees, the
/// reissued terms replace the layer: a new layer with the same cover and the member
/// paying the loading starts today, the old one ends the same day and only once the new
/// one is in the core, the alert clears, and underwriting is told of the new layer under
/// the original request. If the member declines, nothing changes and that is published.
/// Reissued terms for more cover, without a loading, or twice, are refused.
/// Why it matters: this is the only way a loading leaves the employer's invoice after the
/// fact. Ending the old layer before the new one is written would leave the member without
/// cover if the core refused it; leaving both would bill for double the cover.
#[test]
fn reissued_terms_move_a_loading_to_the_member_without_a_gap_in_cover() {
    use ic_core::policy::MemberPays;

    let mut t = terms();
    t.excess_loading_min_count = 2;
    let mut h = Harness::new(t);
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E004", attrs("executive", 240_000_00, "wlg")),
            row(3, "E005", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    let case_of = |h: &Harness, id: &str| h.requests.iter().find(|r| r.life == life(id)).unwrap().case;
    let alerts = |h: &Harness| h.g.exceptions.values().filter(|e| e.kind == ExceptionKind::LoadingTotalTooSmall).count();
    let reissue = |minor: i64, loading: Option<&str>| match accept(minor, loading, false) {
        ExcessDecision::Accepted {
            sum_insured_minor,
            loadings,
            exclusions,
            ..
        } => ExcessDecision::Accepted {
            sum_insured_minor,
            loadings,
            exclusions,
            member_consents_to_itemising: false,
            member_pays_loading: true,
        },
        other => other,
    };
    let (first, second) = (case_of(&h, "E004"), case_of(&h, "E005"));
    // Nothing is in place to reoffer yet.
    assert_eq!(code(h.underwriting(second, ExcessDecision::ReofferDeclined)), "case_closed");
    h.underwriting(first, accept(210_000_00, Some("0.5"), false)).unwrap();
    h.underwriting(second, accept(210_000_00, Some("0.25"), false)).unwrap();
    h.days(3);
    let left = h.today();
    h.apply(
        "leave",
        FileKind::Changes,
        vec![change(1, "E004", RowChange::Leave, left, attrs("executive", 240_000_00, "wlg"))],
    );
    assert_eq!(alerts(&h), 1);
    h.days(2);

    // The member says no: the terms in place stand.
    h.underwriting(second, ExcessDecision::ReofferDeclined).unwrap();
    assert!(h.saw(|e| matches!(e, GroupEvent::LoadingMoveDeclined { case, .. } if *case == second)));
    assert_eq!((payers(&h, "E005"), alerts(&h)), (vec![MemberPays::Nothing], 1));

    // Terms that are not a reissue of this cover.
    assert_eq!(code(h.underwriting(second, reissue(250_000_00, Some("0.25")))), "invalid_amount");
    assert_eq!(code(h.underwriting(second, reissue(210_000_00, None))), "no_loading");
    assert_eq!(h.excess_layers("E005"), vec![(210_000_00, None)], "refusals changed nothing");

    // The member agrees.
    let moved = h.today();
    let outcomes_before = h.outcomes.len();
    h.underwriting(second, reissue(210_000_00, Some("0.25"))).unwrap();
    assert_eq!(h.excess_layers("E005"), vec![(210_000_00, Some(moved)), (210_000_00, None)]);
    assert_eq!(payers(&h, "E005"), vec![MemberPays::Nothing, MemberPays::Loading]);
    // The new layer is written as carrying the old one on, which the core accepts and passes
    // to reinsurance so that the cession is not started afresh.
    let old_layer = h.scheme().cases[&second].layer;
    let carried: Vec<Option<u32>> = h
        .sent
        .iter()
        .filter_map(|(_, c)| match c {
            ic_core::Command::AddExcessLayer(l) => Some(l.continues),
            _ => None,
        })
        .collect();
    assert!(old_layer.is_some() && carried.last() == Some(&old_layer), "{carried:?}");
    assert!(carried[..carried.len() - 1].iter().all(Option::is_none), "a first layer continues nothing");
    assert_eq!(status(&h, second), CaseStatus::Ended);
    let new_case = h.scheme().cases.values().find(|c| c.replaces == Some(second)).unwrap();
    assert_eq!((new_case.status, new_case.effective), (CaseStatus::Accepted, moved));
    assert_eq!(alerts(&h), 0, "nothing of a member's is left in the employer's total");
    // Underwriting hears of the new layer under the request it knows.
    let (request, outcome) = &h.outcomes[outcomes_before];
    assert_eq!(request, &format!("grp-uw:{SCHEME}:{second}"));
    assert!(matches!(outcome, ic_group::event::IssueOutcome::Issued { .. }));
    // The same reissue again finds nothing in place to replace.
    assert_eq!(code(h.underwriting(second, reissue(210_000_00, Some("0.25")))), "case_closed");

    // From the next billing the loading is the member's and the employer's balance is its own.
    let debt = |h: &Harness, id: &str| h.c.state(h.p).policies[&h.policy].member_outstanding_minor(&life(id));
    h.days(31);
    assert!(debt(&h, "E005") > 0);
    // What is owed is the employer's balance and each member's own, the leaver's included.
    assert_eq!(h.owed(), h.scheme().outstanding_minor + debt(&h, "E005") + debt(&h, "E004"));
}

/// Purpose: the employer's view of one member gives what the employer told us and what the
/// member is covered for (the formula amount, the automatic amount, and cover above it as
/// pending, in place or not in place), and nothing of the member's health: no loading, no
/// exclusion, no date of birth or smoker status, and a decline looks the same as a request
/// not proceeded with. A leaver shows as no longer in force with no cover, someone who is
/// not a member is not found, and a policy answers with its scheme.
/// Why it matters: the employer's administrator opens these records every month. A loading
/// or a decline shown here tells an employer about an employee's health, which is the one
/// thing a scheme must never do.
#[test]
fn the_employers_view_of_a_member_shows_cover_and_never_health() {
    let mut t = terms();
    t.excess_loading = ExcessLoading::EmployerItemised;
    let mut h = Harness::new(t);
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E004", attrs("executive", 240_000_00, "wlg")),
            row(3, "E005", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    let ask = |h: &Harness, q: serde_json::Value| ic_group::answer(&h.g, q);
    let member = |h: &Harness, id: &str| ask(h, serde_json::json!({"ask": "member", "scheme": SCHEME, "employee": id})).unwrap();
    let case_of = |h: &Harness, id: &str| h.requests.iter().find(|r| r.life == life(id)).unwrap().case;

    let pending = member(&h, "E004");
    assert_eq!(
        (pending["category"].as_str(), pending["salary_minor"].as_i64(), pending["in_force"].as_bool()),
        (Some("executive"), Some(240_000_00), Some(true))
    );
    assert_eq!(
        (pending["covers"][0]["formula_minor"].as_i64(), pending["covers"][0]["automatic_minor"].as_i64()),
        (Some(960_000_00), Some(750_000_00))
    );
    assert_eq!(
        pending["above_automatic"],
        serde_json::json!([{ "cover": pending["covers"][0]["cover"], "asked_minor": 210_000_00, "state": "pending", "in_place_minor": 0, "from": pending["joined"] }])
    );

    // One accepted with a loading and an exclusion, the other declined.
    let loaded = ExcessDecision::Accepted {
        sum_insured_minor: 210_000_00,
        loadings: vec![DecStr::parse("0.5").unwrap()],
        exclusions: vec!["cardiac".into()],
        member_consents_to_itemising: true,
        member_pays_loading: false,
    };
    h.underwriting(case_of(&h, "E004"), loaded).unwrap();
    h.underwriting(case_of(&h, "E005"), ExcessDecision::Declined).unwrap();
    let in_place = member(&h, "E004");
    assert_eq!(
        (
            in_place["above_automatic"][0]["state"].as_str(),
            in_place["above_automatic"][0]["in_place_minor"].as_i64()
        ),
        (Some("in_place"), Some(210_000_00))
    );
    let declined = member(&h, "E005");
    assert_eq!(
        (
            declined["above_automatic"][0]["state"].as_str(),
            declined["above_automatic"][0]["in_place_minor"].as_i64()
        ),
        (Some("not_in_place"), Some(0))
    );
    for answer in [&in_place, &declined, &member(&h, "E001")] {
        let text = answer.to_string();
        for word in ["loading", "exclusion", "cardiac", "0.5", "declin", "date_of_birth", "smoker", "sex", "party"] {
            assert!(!text.contains(word), "{word} in {text}");
        }
    }
    assert_eq!(member(&h, "E001")["above_automatic"], serde_json::json!([]));

    // A leaver, someone unknown, and the scheme of a policy.
    h.days(3);
    let left = h.today();
    h.apply(
        "leave",
        FileKind::Changes,
        vec![change(1, "E004", RowChange::Leave, left, attrs("executive", 240_000_00, "wlg"))],
    );
    let gone = member(&h, "E004");
    assert_eq!(
        (gone["in_force"].as_bool(), gone["left"].as_str()),
        (Some(false), Some(left.to_string().as_str()))
    );
    assert_eq!(
        (gone["covers"].clone(), gone["above_automatic"].clone()),
        (serde_json::json!([]), serde_json::json!([]))
    );
    assert!(ask(&h, serde_json::json!({"ask": "member", "scheme": SCHEME, "employee": "E999"})).is_err());
    assert!(ask(&h, serde_json::json!({"ask": "member", "scheme": "nobody", "employee": "E001"})).is_err());
    assert_eq!(
        ask(&h, serde_json::json!({"ask": "scheme_of", "policy": h.policy})).unwrap(),
        serde_json::json!({ "schemes": [SCHEME] })
    );
    assert_eq!(
        ask(&h, serde_json::json!({"ask": "scheme_of", "policy": "pol-unknown"})).unwrap(),
        serde_json::json!({ "schemes": [] })
    );
}

/// Purpose: on a scheme whose broker underwrites under a delegated authority, a member's
/// cover above the automatic limit goes to underwriting naming that authority, so the
/// delegate decides; but a member reinsurance asked us to underwrite, and any scheme that
/// cedes its excess facultatively, goes to our own underwriters; and an authority with no id
/// is refused in the terms.
/// Why it matters: Mitchell's decision of 14 Nov 2026. The delegate may bind only what we can
/// retain: a delegate deciding a facultative case or an accumulation reinsurance flagged would
/// bind risk the treaty does not cover.
#[test]
fn a_brokers_delegated_authority_decides_only_what_we_retain() {
    let mut t = terms();
    t.delegated_underwriting = Some(ic_group::terms::DelegatedUnderwriting {
        authority: "auth-harbour-broker".into(),
    });
    let mut blank = t.clone();
    blank.delegated_underwriting = Some(ic_group::terms::DelegatedUnderwriting { authority: " ".into() });
    assert!(blank.validate().is_err());

    let (mut h, _) = with_executive(t.clone());
    assert_eq!(h.requests[0].delegated_authority.as_deref(), Some("auth-harbour-broker"));
    let cover = h.requests[0].cover.clone();
    let policy = h.policy.clone();
    h.send_as(
        caller::REINSURANCE,
        "reinsurance",
        GroupCommand::RequireUnderwrittenExcess {
            policy,
            life: life("E003"),
            cover,
            excess_minor: 80_000_00,
            reference: "ri-accum-7".into(),
        },
    )
    .unwrap();
    h.settle();
    let flagged = h.requests.last().unwrap();
    assert_eq!((flagged.life.clone(), flagged.delegated_authority.clone()), (life("E003"), None));

    let mut ceded = t;
    ceded.excess_path = ic_group::terms::ExcessPath::Facultative;
    let (h, _) = with_executive(ceded);
    assert!(h.requests.iter().all(|r| r.delegated_authority.is_none()));
}
