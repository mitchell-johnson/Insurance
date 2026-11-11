//! Issue, delivery, proof of delivery and the notice clock.

mod support;

use ic_documents::command::{Command, NoticeSpec};
use ic_documents::model::{AttemptOutcome, Channel, CoreInstruction, DeliveryStatus, RecipientRole};
use ic_documents::{DocumentKind, Event, Gate, Waiting};
use support::*;

fn bounced() -> AttemptOutcome {
    AttemptOutcome::Bounced { reason: "mailbox unknown".into() }
}

/// Purpose: a renewal notice issued, sent by email and confirmed starts its clock: the notice
/// is delivered on the confirmation date, the earliest effect is thirty days later, the gate
/// opens on that day and not before, and the core is to be told.
/// Why it matters: a change without a valid notice can be unwound. The clock must run from
/// proven delivery and for the template's full minimum period, to the day.
#[test]
fn proven_delivery_starts_the_notice_clock() {
    let mut kit = Kit::ready();
    let events = kit.issue(kit.renewal_spec("n1"));
    assert!(matches!(&events[0], Event::DocumentIssued { document, kind: DocumentKind::RenewalNotice, .. } if document == "n1"));
    assert_eq!(events[1], Event::DeliveryRequested { document: "n1".into(), party: party("aroha"), channel: Channel::Email });
    let pol = policy("pol-1");
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2028-06-01")), Gate::Waiting(Waiting::NotYetDelivered));
    assert_eq!(kit.state.notice_gate(&pol, "lapse", date("2028-06-01")), Gate::NoNotice);
    assert_eq!(kit.state.pending_sends(10).len(), 1);

    kit.advance_days(1);
    let events = kit.deliver("n1", "aroha", Channel::Email);
    let delivered = date("2027-11-16");
    let effect = date("2027-12-16");
    assert_eq!(events[0], Event::DeliveryConfirmed { document: "n1".into(), party: party("aroha"), channel: Channel::Email, on: delivered });
    assert_eq!(
        events[1],
        Event::NoticeDelivered { notice: "n1".into(), policy: Some(pol.clone()), purpose: "renewal".into(), delivered_on: delivered, earliest_effect: effect }
    );
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2027-12-15")), Gate::Waiting(Waiting::PeriodRunning { until: effect }));
    assert_eq!(kit.state.notice_gate(&pol, "renewal", effect), Gate::Clear { since: effect });
    assert_eq!(
        kit.state.core_outbox["notice-delivery:n1:pol-1"],
        CoreInstruction::RecordNoticeDelivery { policy: pol, purpose: "renewal".into(), notice: "n1".into(), delivered_on: delivered, earliest_effect: effect }
    );
    assert!(kit.state.pending_sends(10).is_empty());
    // The record keeps the masked destination and the provider's reference, nothing more.
    let attempt = &kit.state.documents["n1"].deliveries[0].attempts[0];
    assert_eq!(attempt.masked_destination.as_deref(), Some("a***@example.test"));
    assert_eq!(attempt.provider_reference.as_deref(), Some("ref-1"));

    kit.ok("relay", "svc", Command::AckCoreInstructions { ids: vec!["notice-delivery:n1:pol-1".into()] });
    assert!(kit.state.core_outbox.is_empty());
}

/// Purpose: when email bounces, a mandatory notice falls back to post without being asked;
/// post lodged is deemed delivered after the configured days, and the clock runs from then.
/// The bounce is reported so the contact point can be marked unverified.
/// Why it matters: "if electronic delivery fails for a mandatory notice, it falls back to
/// post". Without the automatic fallback a bounced email would leave a lapse waiting forever.
#[test]
fn a_bounced_notice_falls_back_to_post_and_is_deemed_delivered() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    kit.sent("n1", "aroha", Channel::Email);
    let events = kit.outcome("n1", "aroha", Channel::Email, bounced());
    assert_eq!(
        events,
        vec![
            Event::ContactPointBounced { party: party("aroha"), channel: Channel::Email },
            Event::DeliveryFailed { document: "n1".into(), party: party("aroha"), channel: Channel::Email },
            Event::DeliveryRequested { document: "n1".into(), party: party("aroha"), channel: Channel::Post },
        ]
    );
    assert_eq!(kit.state.pending_sends(10)[0].channel, Channel::Post);
    kit.sent("n1", "aroha", Channel::Post);
    kit.advance_days(4);
    assert!(kit.tick().is_empty());
    kit.advance_days(1);
    let events = kit.tick();
    let on = date("2027-11-20");
    assert_eq!(events[0], Event::DeliveryConfirmed { document: "n1".into(), party: party("aroha"), channel: Channel::Post, on });
    assert!(matches!(&events[1], Event::NoticeDelivered { delivered_on, earliest_effect, .. } if *delivered_on == on && *earliest_effect == date("2027-12-20")));
    assert_eq!(kit.state.documents["n1"].deliveries[0].status, DeliveryStatus::Delivered { on, channel: Channel::Post });
}

/// Purpose: post that was deemed delivered and then comes back stops the clock: the notice
/// becomes undeliverable, the gate closes again, the instruction to the core is withdrawn and
/// an exception event is raised. A retry with corrected details starts delivery again.
/// Why it matters: an undeliverable mandatory notice is an exception item and whatever
/// depends on it waits. Returned mail arriving late must not leave a lapse cleared to run.
#[test]
fn returned_post_makes_the_notice_undeliverable_until_retried() {
    let mut kit = Kit::ready();
    let mut spec = kit.renewal_spec("n1");
    spec.recipients[0].channels = vec![Channel::Post];
    kit.issue(spec);
    kit.sent("n1", "aroha", Channel::Post);
    kit.advance_days(5);
    kit.tick();
    let pol = policy("pol-1");
    assert!(matches!(kit.state.notice_gate(&pol, "renewal", date("2028-06-01")), Gate::Clear { .. }));
    assert_eq!(kit.state.core_outbox.len(), 1);

    let events = kit.outcome("n1", "aroha", Channel::Post, AttemptOutcome::Returned);
    assert!(events.contains(&Event::DocumentUndeliverable { document: "n1".into(), party: party("aroha") }));
    assert!(events.contains(&Event::NoticeUndeliverable { notice: "n1".into(), policy: Some(pol.clone()), purpose: "renewal".into() }));
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2028-06-01")), Gate::Waiting(Waiting::Undeliverable));
    assert!(kit.state.core_outbox.is_empty());
    assert_eq!(kit.state.report(date("2027-11-15"), kit.today()).undeliverable, 1);
    // Nothing more is outstanding, so a second return has nothing to act on.
    assert_eq!(
        kit.err("delivery", "svc", Command::RecordOutcome { document: "n1".into(), party: party("aroha"), channel: Channel::Post, outcome: AttemptOutcome::Returned }),
        "no_attempt"
    );

    let retry = |channels| Command::RetryDelivery { document: "n1".into(), party: party("aroha"), channels };
    assert_eq!(kit.err("servicing", "csr", retry(vec![])), "invalid");
    let events = kit.ok("servicing", "csr", retry(vec![Channel::Sms]));
    assert_eq!(events, vec![Event::DeliveryRequested { document: "n1".into(), party: party("aroha"), channel: Channel::Sms }]);
    assert_eq!(kit.err("servicing", "csr", retry(vec![Channel::Sms])), "not_undeliverable");
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2028-06-01")), Gate::Waiting(Waiting::NotYetDelivered));
    // The retried chain still ends in post, and every earlier attempt stays on the record.
    let delivery = &kit.state.documents["n1"].deliveries[0];
    assert_eq!(delivery.chain, vec![Channel::Post, Channel::Sms, Channel::Post]);
    kit.advance_days(1);
    let events = kit.deliver("n1", "aroha", Channel::Sms);
    assert!(matches!(&events[1], Event::NoticeDelivered { delivered_on, .. } if *delivered_on == date("2027-11-21")));
    assert_eq!(kit.state.documents["n1"].deliveries[0].attempts.len(), 2);
}

/// Purpose: an electronic send that nobody confirms in the allowed time falls back to the
/// next channel for a notice, and every channel failing ends in an undeliverable exception.
/// Why it matters: silence from a provider is the commonest failure. A notice must not sit
/// "sent" for ever with a lapse waiting on it and nobody told.
#[test]
fn an_unconfirmed_send_times_out_and_the_chain_runs_to_its_end() {
    let mut kit = Kit::ready();
    let mut spec = kit.renewal_spec("n1");
    spec.recipients[0].channels = vec![Channel::Push, Channel::Email];
    kit.issue(spec);
    // Not yet sent: time passing does nothing, because the driver has not acted.
    kit.advance_days(10);
    assert!(kit.tick().is_empty());
    kit.sent("n1", "aroha", Channel::Push);
    // Recording the same send again changes nothing.
    kit.sent("n1", "aroha", Channel::Push);
    kit.advance_hours(71);
    assert!(kit.tick().is_empty());
    kit.advance_hours(1);
    let events = kit.tick();
    assert_eq!(
        events,
        vec![
            Event::DeliveryFailed { document: "n1".into(), party: party("aroha"), channel: Channel::Push },
            Event::DeliveryRequested { document: "n1".into(), party: party("aroha"), channel: Channel::Email },
        ]
    );
    assert_eq!(kit.state.documents["n1"].deliveries[0].attempts[0].outcome, Some(AttemptOutcome::TimedOut));
    kit.sent("n1", "aroha", Channel::Email);
    kit.outcome("n1", "aroha", Channel::Email, AttemptOutcome::Failed { reason: "provider error".into() });
    let events = kit.outcome("n1", "aroha", Channel::Post, AttemptOutcome::NoContactPoint);
    assert_eq!(
        events,
        vec![
            Event::DeliveryFailed { document: "n1".into(), party: party("aroha"), channel: Channel::Post },
            Event::DocumentUndeliverable { document: "n1".into(), party: party("aroha") },
            Event::NoticeUndeliverable { notice: "n1".into(), policy: Some(policy("pol-1")), purpose: "renewal".into() },
        ]
    );
    assert_eq!(kit.state.documents["n1"].deliveries[0].status, DeliveryStatus::Undeliverable);
}

/// Purpose: a notice to joint owners is delivered only when every addressee's delivery is
/// proven, on the later date; a lender who also receives it does not hold up the clock.
/// Why it matters: the clock must not start while one owner has not been reached, and an
/// interested party's bounced email must not block a renewal the owners were properly told of.
#[test]
fn the_clock_waits_for_every_addressee_but_not_for_interested_parties() {
    let mut kit = Kit::ready();
    let mut spec = kit.renewal_spec("n1");
    spec.recipients = vec![
        recipient("aroha", RecipientRole::Addressee, &[Channel::Email]),
        recipient("ben", RecipientRole::Addressee, &[Channel::Email]),
        recipient("lender", RecipientRole::InterestedParty, &[Channel::Email]),
    ];
    kit.issue(spec);
    let events = kit.deliver("n1", "aroha", Channel::Email);
    assert_eq!(events.len(), 1, "one owner reached is not yet notice to both");
    kit.sent("n1", "lender", Channel::Email);
    kit.outcome("n1", "lender", Channel::Email, bounced());
    kit.advance_days(2);
    let events = kit.deliver("n1", "ben", Channel::Email);
    assert!(matches!(&events[1], Event::NoticeDelivered { delivered_on, .. } if *delivered_on == date("2027-11-17")));
    // The lender's copy is still being pursued by post.
    assert_eq!(kit.state.pending_sends(10).len(), 1);
    assert_eq!(kit.state.report(date("2027-11-15"), kit.today()).deliveries_in_progress, 1);
    // Each recipient finds the notice in their own inbox.
    for p in ["aroha", "ben", "lender"] {
        assert_eq!(kit.state.inbox(&party(p)).len(), 1);
    }
}

/// Purpose: one notice covering several policies records each of them, gates each, and
/// queues one instruction to the core per policy.
/// Why it matters: policies in a group with the same anniversary are reviewed in one notice.
/// Each policy's renewal depends on it, so each must see the delivery.
#[test]
fn one_notice_can_cover_several_policies() {
    let mut kit = Kit::ready();
    let mut spec = kit.renewal_spec("n1");
    spec.other_policies = vec![policy("pol-2")];
    kit.issue(spec);
    kit.deliver("n1", "aroha", Channel::Email);
    assert!(matches!(kit.state.notice_gate(&policy("pol-2"), "renewal", date("2028-06-01")), Gate::Clear { .. }));
    assert_eq!(kit.state.core_outbox.keys().cloned().collect::<Vec<_>>(), vec!["notice-delivery:n1:pol-1", "notice-delivery:n1:pol-2"]);
    assert_eq!(kit.state.policy_documents(&policy("pol-2")).len(), 1);

    let mut bad = kit.renewal_spec("n2");
    bad.policy = None;
    bad.other_policies = vec![policy("pol-2")];
    assert_eq!(kit.issue_err(bad), "invalid");
}

/// Purpose: an informational document is sent once on its first channel: a failure makes it
/// undeliverable with no fallback, an unconfirmed send is simply treated as done, and a
/// document can ask for proof of delivery even when its kind does not require it.
/// Why it matters: fallback to post costs money and is for documents that must be proven. A
/// receipt must not generate a letter, and a claim acknowledgement must be able to.
#[test]
fn informational_documents_are_sent_once_unless_proof_is_asked_for() {
    let mut kit = Kit::ready();
    kit.issue(kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt"));
    kit.sent("r1", "aroha", Channel::Email);
    let events = kit.outcome("r1", "aroha", Channel::Email, bounced());
    assert!(events.contains(&Event::DocumentUndeliverable { document: "r1".into(), party: party("aroha") }));
    assert_eq!(kit.state.documents["r1"].deliveries[0].attempts.len(), 1);

    kit.issue(kit.issue_spec("r2", DocumentKind::PremiumReceipt, "receipt"));
    kit.sent("r2", "aroha", Channel::Email);
    kit.advance_hours(72);
    assert!(kit.tick().is_empty());
    assert!(matches!(kit.state.documents["r2"].deliveries[0].status, DeliveryStatus::Delivered { channel: Channel::Email, .. }));

    let mut spec = kit.issue_spec("r3", DocumentKind::PremiumReceipt, "receipt");
    spec.prove_delivery = true;
    kit.issue(spec);
    assert_eq!(kit.state.documents["r3"].deliveries[0].chain, vec![Channel::Email, Channel::Post]);
    let report = kit.state.report(date("2027-11-15"), kit.today());
    assert_eq!(report.issued_by_kind[&DocumentKind::PremiumReceipt], 3);
    assert_eq!(report.undeliverable_per_mille, 333);
}

/// Purpose: delivery commands that match nothing are refused: an unknown document, a party
/// who was not sent it, a channel with no send waiting, a return on a channel that is not
/// post, and a send with no masked destination.
/// Why it matters: provider callbacks arrive late, twice and for the wrong thing. Each must
/// be refused cleanly without touching the delivery record.
#[test]
fn delivery_commands_that_match_nothing_are_refused() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    let send = |document: &str, p: &str, channel, masked: &str| Command::RecordSend {
        document: document.into(),
        party: party(p),
        channel,
        masked_destination: masked.into(),
        provider_reference: "x".into(),
    };
    let outcome = |p: &str, channel, outcome| Command::RecordOutcome { document: "n1".into(), party: party(p), channel, outcome };
    assert_eq!(kit.err("delivery", "svc", send("nope", "aroha", Channel::Email, "m")), "unknown_document");
    assert_eq!(kit.err("delivery", "svc", send("n1", "ben", Channel::Email, "m")), "unknown_recipient");
    assert_eq!(kit.err("delivery", "svc", send("n1", "aroha", Channel::Sms, "m")), "no_attempt");
    assert_eq!(kit.err("delivery", "svc", send("n1", "aroha", Channel::Email, "")), "invalid");
    assert_eq!(kit.err("delivery", "svc", outcome("aroha", Channel::Sms, AttemptOutcome::Delivered)), "no_attempt");
    assert_eq!(kit.err("delivery", "svc", outcome("aroha", Channel::Email, AttemptOutcome::Returned)), "invalid");
    assert_eq!(kit.err("delivery", "svc", outcome("aroha", Channel::Post, AttemptOutcome::Returned)), "no_attempt");
    kit.deliver("n1", "aroha", Channel::Email);
    assert_eq!(kit.err("delivery", "svc", outcome("aroha", Channel::Email, AttemptOutcome::Delivered)), "no_attempt");
    assert_eq!(
        kit.err("servicing", "csr", Command::RetryDelivery { document: "nope".into(), party: party("aroha"), channels: vec![Channel::Post] }),
        "unknown_document"
    );
}

/// Purpose: issue refuses each malformed request with its own code and leaves no document:
/// duplicate id, bad hashes, an owner with no key, an unknown template, the wrong kind,
/// language or format, no recipients, no addressee, a repeated party, a notice with no
/// trigger event or purpose, a clock on a document that is not proven, and a schedule with no
/// policy or no activated wording.
/// Why it matters: these are the invariants the vault and the notice clock rest on. A notice
/// with no trigger event could say something the ledger does not.
#[test]
fn issue_refuses_malformed_requests() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    let base = kit.renewal_spec("n2");
    let mut cases: Vec<(&str, ic_documents::command::Issue)> = vec![];
    let mut with = |code: &'static str, change: &dyn Fn(&mut ic_documents::command::Issue)| {
        let mut spec = base.clone();
        change(&mut spec);
        cases.push((code, spec));
    };
    with("document_exists", &|s| s.document = "n1".into());
    with("document_exists", &|s| s.document.clear());
    with("invalid", &|s| s.content_sha256 = "abc".into());
    with("invalid", &|s| s.data_sha256 = "abc".into());
    with("invalid", &|s| s.blob.clear());
    with("no_party_key", &|s| s.owner = party("stranger"));
    with("unknown_template", &|s| s.template = tref("nothing", 1));
    with("wrong_kind", &|s| s.kind = DocumentKind::LapseNotice);
    with("no_language", &|s| s.language = "mi".into());
    with("no_format", &|s| s.format = Some(ic_documents::template::AlternateFormat::Braille));
    with("invalid", &|s| s.recipients.clear());
    with("invalid", &|s| s.recipients[0].role = RecipientRole::Adviser);
    with("invalid", &|s| s.recipients.push(s.recipients[0].clone()));
    with("no_trigger", &|s| s.trigger_event = None);
    with("no_purpose", &|s| s.notice = None);
    with("invalid", &|s| s.notice = Some(NoticeSpec { purpose: String::new(), corrects: None }));
    with("invalid", &|s| s.policy = None);
    with("unknown_document", &|s| {
        s.supersedes = Some("ghost".into());
        s.reason = Some("fix".into());
    });
    for (code, spec) in cases {
        assert_eq!(kit.issue_err(spec.clone()), code, "{spec:?}");
    }

    let mut receipt = kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt");
    receipt.notice = Some(NoticeSpec { purpose: "renewal".into(), corrects: None });
    assert_eq!(kit.issue_err(receipt.clone()), "invalid");
    receipt.notice = None;
    receipt.recipients[0].channels.clear();
    assert_eq!(kit.issue_err(receipt), "invalid");

    let mut schedule = kit.issue_spec("s1", DocumentKind::PolicySchedule, "schedule");
    assert_eq!(kit.issue_err(schedule.clone()), "no_wording");
    schedule.wordings = vec![tref("receipt", 1)];
    assert_eq!(kit.issue_err(schedule.clone()), "no_wording");
    schedule.wordings = vec![tref("wording", 1)];
    schedule.policy = None;
    assert_eq!(kit.issue_err(schedule), "invalid");
    assert_eq!(kit.state.documents.len(), 1);

    // A notice may be addressed with no channel preference: post is always there.
    let mut spec = kit.renewal_spec("n3");
    spec.recipients[0].channels.clear();
    kit.issue(spec);
    assert_eq!(kit.state.documents["n3"].deliveries[0].chain, vec![Channel::Post]);
}

/// Purpose: a schedule is reissued as a new version with a reason; the earlier one is kept and
/// marked superseded, both are in the customer's inbox, and the schedule history is the
/// sequence. A version cannot be superseded twice, by another kind, or without a reason.
/// Why it matters: "a document is never edited. A correction is a new document version with a
/// reason, the earlier one kept and marked superseded, and both visible to the customer."
#[test]
fn a_correction_is_a_new_version_and_the_earlier_one_is_kept() {
    let mut kit = Kit::ready();
    let schedule = |kit: &Kit, id: &str, supersedes: Option<&str>, reason: Option<&str>| {
        let mut spec = kit.issue_spec(id, DocumentKind::PolicySchedule, "schedule");
        spec.wordings = vec![tref("wording", 1)];
        spec.components = vec!["life".into(), "trauma".into()];
        spec.lives = vec!["life-1".into()];
        spec.supersedes = supersedes.map(str::to_string);
        spec.reason = reason.map(str::to_string);
        spec
    };
    kit.issue(schedule(&kit, "s1", None, None));
    kit.advance_days(1);
    assert_eq!(kit.issue_err(schedule(&kit, "s2", Some("s1"), None)), "no_reason");
    let events = kit.issue(schedule(&kit, "s2", Some("s1"), Some("sum insured increased")));
    assert!(events.contains(&Event::DocumentSuperseded { document: "s1".into(), by: "s2".into() }));
    assert_eq!(kit.issue_err(schedule(&kit, "s3", Some("s1"), Some("again"))), "already_superseded");
    let mut receipt = kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt");
    receipt.supersedes = Some("s2".into());
    receipt.reason = Some("wrong".into());
    assert_eq!(kit.issue_err(receipt), "invalid");

    let history = kit.state.schedule_history(&policy("pol-1"));
    assert_eq!(history.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), vec!["s1", "s2"]);
    assert_eq!(history[0].superseded_by.as_deref(), Some("s2"));
    assert_eq!(history[1].wordings, vec![tref("wording", 1)]);
    assert_eq!(history[1].components, vec!["life", "trauma"]);
    // Newest first, and the superseded version is still there.
    let inbox = kit.state.inbox(&party("aroha"));
    assert_eq!(inbox.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), vec!["s2", "s1"]);
    let report = kit.state.report(date("2027-11-15"), kit.today());
    assert_eq!(report.reissues_by_reason["sum insured increased"], 1);
}

/// Purpose: a superseded notice no longer gates; the notice that replaced it does.
/// Why it matters: when a mid-term change recomputes the renewal offer, the customer is
/// re-notified. The renewal must wait for the new notice's clock, not the old one's.
#[test]
fn a_superseded_notice_hands_the_gate_to_its_replacement() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    kit.deliver("n1", "aroha", Channel::Email);
    let pol = policy("pol-1");
    assert!(matches!(kit.state.notice_gate(&pol, "renewal", date("2028-01-01")), Gate::Clear { .. }));
    let mut spec = kit.renewal_spec("n2");
    spec.supersedes = Some("n1".into());
    spec.reason = Some("offer recomputed after a reduction".into());
    kit.issue(spec);
    assert_eq!(kit.state.notice_gate(&pol, "renewal", date("2028-01-01")), Gate::Waiting(Waiting::NotYetDelivered));
}

/// Purpose: legacy documents are imported as received, with their original dates, no
/// template and no delivery; a future date or a duplicate id is refused.
/// Why it matters: issued legacy documents must stay available after migration with their
/// retention clocks running from the original dates, and must never be re-sent.
#[test]
fn legacy_documents_are_imported_as_received() {
    use ic_documents::command::LegacyImport;
    let mut kit = Kit::ready();
    let import = |id: &str, on: &str| {
        Command::ImportLegacy(Box::new(LegacyImport {
            document: id.into(),
            kind: DocumentKind::PolicySchedule,
            policy: Some(policy("pol-1")),
            data_class: Default::default(),
            owner: party("aroha"),
            issued_on: date(on),
            language: "en".into(),
            content_sha256: "a".repeat(64),
            blob: format!("legacy/{id}"),
            wrapped_key: "dek".into(),
            bytes: 2048,
        }))
    };
    let events = kit.ok("migration", "loader", import("old-1", "2019-03-01"));
    assert!(matches!(&events[0], Event::DocumentIssued { template: None, .. }));
    assert_eq!(kit.err("migration", "loader", import("old-1", "2019-03-01")), "document_exists");
    assert_eq!(kit.err("migration", "loader", import("old-2", "2030-01-01")), "invalid");
    let doc = &kit.state.documents["old-1"];
    assert!(doc.legacy && doc.deliveries.is_empty() && !doc.requires_proof());
    assert_eq!(doc.issued_on, date("2019-03-01"));
    assert!(kit.state.pending_sends(10).is_empty());
    assert_eq!(kit.state.schedule_history(&policy("pol-1")).len(), 1);
    // Legacy documents are not this platform's issue volume.
    assert!(kit.state.report(date("2019-01-01"), kit.today()).issued_by_kind.is_empty());
}
