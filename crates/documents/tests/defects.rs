//! The notice defect: addendum entry 3, the document side.

mod support;

use ic_documents::command::{Command, NoticeSpec};
use ic_documents::model::{AttemptOutcome, Channel, CoreInstruction, Correction, DefectCause, DefectItem};
use ic_documents::{DocumentKind, Event, Gate, Waiting};
use support::*;

fn raise(kit: &mut Kit, id: &str) {
    kit.ok(
        "remediation",
        "commander",
        Command::RaiseNoticeDefect {
            defect: id.into(),
            case: "REM-1".into(),
            description: "renewal notices quoted a premium from a wrong rate cell".into(),
            cause: DefectCause::WrongRate,
            from: date("2027-11-01"),
            to: date("2027-11-30"),
            grace_days: None,
        },
    );
}

fn attach(id: &str, notices: &[&str]) -> Command {
    Command::AttachDefectNotices { defect: id.into(), notices: notices.iter().map(|s| s.to_string()).collect() }
}

fn corrected(kit: &Kit, id: &str, defect: &str, original: &str) -> ic_documents::command::Issue {
    let mut spec = kit.renewal_spec(id);
    spec.reason = Some("corrected premium; any earlier decision is set aside".into());
    spec.notice = Some(NoticeSpec {
        purpose: "renewal".into(),
        corrects: Some(Correction { defect: defect.into(), original: original.into() }),
    });
    spec
}

/// Purpose: the whole remediation path for a defective renewal notice. Raising the defect
/// voids the delivered notice's clock; the corrected notice supersedes it; on proof of
/// delivery the clock restarts for the full minimum period, a reopen period opens for the
/// grace days with a stable reference, and the core is told both.
/// Why it matters: a choice made in reliance on a wrong mandatory notice is not a valid
/// choice. Every date the core's reopen path depends on is produced here.
#[test]
fn a_defective_notice_is_voided_corrected_and_its_choices_reopened() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    kit.deliver("n1", "aroha", Channel::Email);
    let pol = policy("pol-1");
    assert!(matches!(kit.state.notice_gate(&pol, "renewal", date("2028-01-01")), Gate::Clear { .. }));

    kit.advance_days(20);
    raise(&mut kit, "D1");
    let events = kit.ok("remediation", "commander", attach("D1", &["n1"]));
    assert_eq!(
        events,
        vec![Event::NoticeDefectRaised { defect: "D1".into(), case: "REM-1".into(), notice: "n1".into(), policy: Some(pol.clone()), purpose: "renewal".into() }]
    );
    // The original is treated as not given.
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2028-01-01")), Gate::Waiting(Waiting::Defective));
    assert!(kit.state.core_outbox.is_empty());
    assert!(kit.ok("remediation", "commander", attach("D1", &["n1"])).is_empty(), "attaching twice raises nothing new");
    let report = kit.state.report(date("2027-11-01"), kit.today());
    assert_eq!((report.open_defects, report.defective_notices_outstanding), (1, 1));

    let events = kit.issue(corrected(&kit, "n1c", "D1", "n1"));
    assert!(events.contains(&Event::DocumentSuperseded { document: "n1".into(), by: "n1c".into() }));
    assert_eq!(kit.state.defects["D1"].notices["n1"], DefectItem::Corrected { by: "n1c".into() });
    assert_eq!(kit.issue_err(corrected(&kit, "n1d", "D1", "n1")), "already_corrected");
    // Issued, not yet proven: the clock has not restarted.
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2028-06-01")), Gate::Waiting(Waiting::NotYetDelivered));

    kit.advance_days(1);
    let events = kit.deliver("n1c", "aroha", Channel::Email);
    let delivered = date("2027-12-06");
    assert_eq!(
        events[1],
        Event::NoticeClockRestarted {
            notice: "n1c".into(),
            policy: Some(pol.clone()),
            purpose: "renewal".into(),
            delivered_on: delivered,
            earliest_effect: date("2028-01-05"),
        }
    );
    assert_eq!(
        events[2],
        Event::ReopenPeriodOpened {
            reopen_reference: "D1:n1".into(),
            defect: "D1".into(),
            policy: Some(pol.clone()),
            purpose: "renewal".into(),
            corrected_notice: "n1c".into(),
            from: delivered,
            until: date("2028-01-05"),
        }
    );
    assert_eq!(
        kit.state.core_outbox["reopen:D1:n1:pol-1"],
        CoreInstruction::OpenReopenWindow { policy: pol.clone(), reopen_reference: "D1:n1".into(), purpose: "renewal".into(), from: delivered, until: date("2028-01-05") }
    );
    assert!(kit.state.core_outbox.contains_key("notice-delivery:n1c:pol-1"));
    // The window extends to give the full minimum period from the corrected notice.
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2028-01-04")), Gate::Waiting(Waiting::PeriodRunning { until: date("2028-01-05") }));
    // The defective original and its correction are both still in the customer's inbox.
    assert_eq!(kit.state.inbox(&party("aroha")).len(), 2);
}

/// Purpose: a defect closes only when every attached notice has a corrected notice proven
/// delivered or an exclusion with a reason, by someone other than whoever raised it, with the
/// control that should have caught it recorded. After closing nothing more can be attached.
/// Why it matters: the remediation case's last step is reconciliation that every affected
/// policy was corrected. A defect that could close with a customer still uncorrected would
/// hide exactly the people the process exists for.
#[test]
fn a_defect_closes_only_when_every_notice_is_corrected_or_excluded() {
    let mut kit = Kit::ready();
    for id in ["n1", "n2", "n3"] {
        let mut spec = kit.renewal_spec(id);
        spec.policy = Some(policy(&format!("pol-{id}")));
        kit.issue(spec);
        kit.deliver(id, "aroha", Channel::Email);
    }
    raise(&mut kit, "D1");
    let close = || Command::CloseNoticeDefect { defect: "D1".into(), control: "adjacent-age monotonicity check on rate tables".into() };
    assert_eq!(kit.err("remediation", "reviewer", close()), "empty");
    kit.ok("remediation", "commander", attach("D1", &["n1", "n2", "n3"]));
    assert_eq!(kit.err("remediation", "reviewer", close()), "not_reconciled");

    let mut c1 = corrected(&kit, "c1", "D1", "n1");
    c1.policy = Some(policy("pol-n1"));
    kit.issue(c1);
    kit.deliver("c1", "aroha", Channel::Email);
    let exclude = |notice: &str, reason: &str| Command::ExcludeFromDefect { defect: "D1".into(), notice: notice.into(), reason: reason.into() };
    assert_eq!(kit.err("remediation", "commander", exclude("n2", "")), "no_reason");
    assert_eq!(kit.err("remediation", "commander", exclude("n9", "x")), "not_in_defect");
    assert_eq!(kit.err("remediation", "commander", exclude("n1", "x")), "already_corrected");
    let events = kit.ok("remediation", "commander", exclude("n2", "policy cancelled before the anniversary"));
    assert_eq!(events, vec![Event::NoticeExcludedFromDefect { defect: "D1".into(), notice: "n2".into() }]);
    // n3 has a corrected notice issued but not yet proven: still not reconciled.
    let mut c3 = corrected(&kit, "c3", "D1", "n3");
    c3.policy = Some(policy("pol-n3"));
    kit.issue(c3);
    assert_eq!(kit.err("remediation", "reviewer", close()), "not_reconciled");
    kit.deliver("c3", "aroha", Channel::Email);

    assert_eq!(kit.err("remediation", "commander", close()), "maker_checker");
    assert_eq!(kit.err("remediation", "reviewer", Command::CloseNoticeDefect { defect: "D1".into(), control: String::new() }), "no_control");
    let events = kit.ok("remediation", "reviewer", close());
    assert_eq!(events, vec![Event::NoticeDefectClosed { defect: "D1".into(), notices: 3, excluded: 1 }]);
    assert_eq!(kit.state.defects["D1"].closed.as_ref().unwrap().by, "reviewer");
    assert_eq!(kit.err("remediation", "commander", attach("D1", &["n1"])), "defect_closed");
    assert_eq!(kit.issue_err(corrected(&kit, "late", "D1", "n2")), "defect_closed");
    assert_eq!(kit.state.report(date("2027-11-01"), kit.today()).open_defects, 0);
}

/// Purpose: a corrected notice that cannot be delivered does not reopen anything; a further
/// correction can then be issued and, once proven, opens the period.
/// Why it matters: the fresh period runs from the corrected notice reaching the customer. An
/// undeliverable correction must not start a period the customer never knew of, and must not
/// leave the defect stuck.
#[test]
fn an_undeliverable_correction_can_itself_be_replaced() {
    let mut kit = Kit::ready();
    let mut spec = kit.renewal_spec("n1");
    spec.recipients[0].channels = vec![Channel::Post];
    kit.issue(spec);
    raise(&mut kit, "D1");
    kit.ok("remediation", "commander", attach("D1", &["n1"]));
    let mut c1 = corrected(&kit, "c1", "D1", "n1");
    c1.recipients[0].channels = vec![Channel::Post];
    kit.issue(c1);
    kit.outcome("c1", "aroha", Channel::Post, AttemptOutcome::NoContactPoint);
    assert!(kit.state.core_outbox.is_empty());
    assert_eq!(kit.state.defects["D1"].notices["n1"], DefectItem::Corrected { by: "c1".into() });

    let events = kit.issue(corrected(&kit, "c2", "D1", "n1"));
    // The second correction replaces the first, which had replaced the original.
    assert!(events.contains(&Event::DocumentSuperseded { document: "c1".into(), by: "c2".into() }));
    assert_eq!(kit.state.documents["c2"].supersedes.as_deref(), Some("c1"));
    let events = kit.deliver("c2", "aroha", Channel::Email);
    assert!(matches!(&events[2], Event::ReopenPeriodOpened { reopen_reference, corrected_notice, .. } if reopen_reference == "D1:n1" && corrected_notice == "c2"));
    assert!(matches!(&kit.state.defects["D1"].notices["n1"], DefectItem::Reopened { by, .. } if by == "c2"));
}

/// Purpose: a defect's own grace period overrides the default, a letter with no clock (a
/// claim decision) can be defective and corrected too, and delivery of a notice that became
/// defective while in transit does not start its clock.
/// Why it matters: the mechanism is "not specific to indexation or to rate tables". It must
/// work for any proven letter, and a defective notice arriving late is still not notice.
#[test]
fn defects_cover_any_proven_letter_and_notices_still_in_transit() {
    let mut kit = Kit::ready();
    let mut letter = kit.issue_spec("d1", DocumentKind::ClaimDecisionLetter, "decision");
    letter.trigger_event = Some("9-4".into());
    kit.issue(letter);
    kit.issue(kit.renewal_spec("n1"));
    kit.sent("n1", "aroha", Channel::Email);
    kit.ok(
        "remediation",
        "commander",
        Command::RaiseNoticeDefect {
            defect: "D2".into(),
            case: "REM-2".into(),
            description: "template error".into(),
            cause: DefectCause::TemplateError,
            from: date("2027-11-15"),
            to: date("2027-11-15"),
            grace_days: Some(45),
        },
    );
    let events = kit.ok("remediation", "commander", attach("D2", &["d1", "n1"]));
    assert!(matches!(&events[0], Event::NoticeDefectRaised { purpose, .. } if purpose.is_empty()));
    // The defective notice lands after the defect was raised: no clock, no instruction.
    let events = kit.outcome("n1", "aroha", Channel::Email, AttemptOutcome::Delivered);
    assert_eq!(events.len(), 1);
    assert_eq!(kit.state.notice_gate(&policy("pol-1"), "renewal", date("2029-01-01")), Gate::Waiting(Waiting::Defective));
    assert!(kit.state.core_outbox.is_empty());

    let mut fix = kit.issue_spec("d1c", DocumentKind::ClaimDecisionLetter, "decision");
    fix.trigger_event = Some("9-4".into());
    fix.reason = Some("corrected wording".into());
    fix.notice = Some(NoticeSpec { purpose: "claim_decision".into(), corrects: Some(Correction { defect: "D2".into(), original: "d1".into() }) });
    kit.issue(fix);
    let events = kit.deliver("d1c", "aroha", Channel::Email);
    let today = kit.today();
    assert!(matches!(&events[2], Event::ReopenPeriodOpened { until, .. } if *until == today + chrono::Duration::days(45)));
}

/// Purpose: defect commands refuse bad input: missing fields, a period that ends before it
/// starts, a duplicate id, an unknown defect, an empty chunk, an unknown document, a document
/// that never had to be proven, one issued outside the period, one already under another
/// defect, a correction naming an unknown defect or a notice outside it, a correction with
/// another purpose, and one that claims to supersede some other document.
/// Why it matters: a defect names a population. Attaching the wrong notices would void valid
/// notices and reopen choices nobody was misled about.
#[test]
fn defect_commands_refuse_bad_input() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    kit.issue(kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt"));
    let raise_with = |defect: &str, case: &str, from: &str, to: &str| Command::RaiseNoticeDefect {
        defect: defect.into(),
        case: case.into(),
        description: "d".into(),
        cause: DefectCause::Other,
        from: date(from),
        to: date(to),
        grace_days: None,
    };
    assert_eq!(kit.err("remediation", "c", raise_with("", "REM", "2027-11-01", "2027-11-30")), "invalid");
    assert_eq!(kit.err("remediation", "c", raise_with("D1", "REM", "2027-12-01", "2027-11-30")), "invalid");
    kit.ok("remediation", "c", raise_with("D1", "REM", "2027-11-01", "2027-11-30"));
    assert_eq!(kit.err("remediation", "c", raise_with("D1", "REM", "2027-11-01", "2027-11-30")), "defect_exists");
    kit.ok("remediation", "c", raise_with("OLD", "REM", "2026-01-01", "2026-01-31"));
    kit.ok("remediation", "c", raise_with("D3", "REM", "2027-11-01", "2027-11-30"));

    assert_eq!(kit.err("remediation", "c", attach("nope", &["n1"])), "unknown_defect");
    assert_eq!(kit.err("remediation", "c", attach("D1", &[])), "invalid");
    assert_eq!(kit.err("remediation", "c", attach("D1", &["ghost"])), "unknown_document");
    assert_eq!(kit.err("remediation", "c", attach("D1", &["r1"])), "not_a_notice");
    assert_eq!(kit.err("remediation", "c", attach("OLD", &["n1"])), "outside_period");
    kit.ok("remediation", "c", attach("D1", &["n1"]));
    assert_eq!(kit.err("remediation", "c", attach("D3", &["n1"])), "already_defective");

    assert_eq!(kit.issue_err(corrected(&kit, "c1", "nope", "n1")), "unknown_defect");
    assert_eq!(kit.issue_err(corrected(&kit, "c1", "D3", "n1")), "not_in_defect");
    let mut wrong_purpose = corrected(&kit, "c1", "D1", "n1");
    wrong_purpose.notice.as_mut().unwrap().purpose = "lapse".into();
    assert_eq!(kit.issue_err(wrong_purpose), "invalid");
    let mut wrong_target = corrected(&kit, "c1", "D1", "n1");
    wrong_target.supersedes = Some("r1".into());
    assert_eq!(kit.issue_err(wrong_target), "invalid");
    let mut no_reason = corrected(&kit, "c1", "D1", "n1");
    no_reason.reason = None;
    assert_eq!(kit.issue_err(no_reason), "no_reason");
    assert_eq!(kit.err("remediation", "c", Command::CloseNoticeDefect { defect: "nope".into(), control: "x".into() }), "unknown_defect");
}

/// Purpose: the population of a defect can be found from the template version and the period.
/// Why it matters: runbook eight identifies recipients from the notification record. The
/// query must return exactly the proven notices that version rendered in the window.
#[test]
fn the_population_is_found_by_template_version_and_period() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    kit.advance_days(3);
    kit.issue(kit.renewal_spec("n2"));
    kit.issue(kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt"));
    let found = kit.state.notices_by_template(&tref("renewal", 1), date("2027-11-15"), date("2027-11-17"));
    assert_eq!(found, vec!["n1"]);
    assert_eq!(kit.state.notices_by_template(&tref("renewal", 1), date("2027-11-01"), date("2027-11-30")).len(), 2);
    assert!(kit.state.notices_by_template(&tref("receipt", 1), date("2027-11-01"), date("2027-11-30")).is_empty());
}
