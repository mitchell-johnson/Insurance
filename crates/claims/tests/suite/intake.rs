//! Intake, triage, service timers and assessment.

use ic_claims::model::{HealthItem, TimerKind};
use ic_claims::{ClaimType, ClaimsState, Command, CoverFacts, Flags, Instruction, Queue, Status, TaskKind};

use crate::world::*;

fn notify(w: &mut World, id: &str, policy: &str, cover: &str, claim_type: ClaimType) {
    w.ok(Command::Notify(Box::new(World::notification(id, policy, cover, claim_type))));
}

fn dental(id: &str, invoice: i64) -> ic_claims::command::Notification {
    let mut n = World::notification(id, "pol-3", "health", ClaimType::Health);
    n.health = Some(HealthItem {
        category: "dental".into(),
        invoice_minor: invoice,
        provider: Some("provider-9".into()),
        pre_approval_request: false,
        settles_pre_approval: None,
        service_code: None,
        service_date: None,
    });
    n
}

/// Purpose: rules and authority profiles are activated only with a preparer and two other
/// approvers, only as a later version, and only if they are well formed.
/// Why it matters: these two documents decide who may admit how much and which claims are paid
/// without a person. A version one person could activate alone, or one that replaced a live
/// version in place, would make every decision recorded against it unexplainable.
#[test]
fn configuration_needs_dual_approval_and_a_new_version() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.approved_by = vec!["config-ann".into(), "config-bob".into()];
    assert_eq!(w.refused(Command::ActivateRules { rules: Box::new(r.clone()) }), "dual_approval");
    r.approved_by = vec!["config-bob".into(), "config-bob".into()];
    assert_eq!(w.refused(Command::ActivateRules { rules: Box::new(r.clone()) }), "dual_approval");
    r.approved_by = vec!["config-bob".into(), "config-cat".into()];
    r.max_offset_pct = 101;
    assert_eq!(w.refused(Command::ActivateRules { rules: Box::new(r.clone()) }), "invalid_rules");
    r.max_offset_pct = 25;
    r.cause_codes.insert("CC-0009".into(), vec!["astrology.mercury".into()]);
    assert_eq!(w.refused(Command::ActivateRules { rules: Box::new(r.clone()) }), "invalid_rules");
    r.cause_codes.remove("CC-0009");
    r.version = 1;
    assert_eq!(w.refused(Command::ActivateRules { rules: Box::new(r.clone()) }), "config_conflict");
    r.version = 2;
    w.ok(Command::ActivateRules { rules: Box::new(r) });

    let mut book = authority();
    assert_eq!(w.refused(Command::ActivateAuthority { book: Box::new(book.clone()) }), "config_conflict");
    book.version = 2;
    book.approved_by = vec!["config-bob".into()];
    assert_eq!(w.refused(Command::ActivateAuthority { book: Box::new(book.clone()) }), "dual_approval");
    book.approved_by = vec!["config-bob".into(), "config-cat".into()];
    book.staff.get_mut("ana").expect("ana is in the book").profile = "nobody".into();
    assert_eq!(w.refused(Command::ActivateAuthority { book: Box::new(book.clone()) }), "invalid_authority");
    book.staff.get_mut("ana").expect("ana is in the book").profile = "assessor".into();
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    assert_eq!(w.state.activations.len(), 4);
    assert_eq!(w.state.activations[3].version, 2);
    assert_eq!(w.state.activations[3].content_hash, w.state.authority.content_hash());
}

/// Purpose: a partition with no rules or authority activated refuses claims work.
/// Why it matters: a claim registered with no rules would be triaged, timed and reserved by
/// defaults nobody approved.
#[test]
fn nothing_is_worked_before_configuration() {
    let mut w = World::new();
    w.state = ClaimsState::default();
    let n = World::notification("c-1", "pol-1", "trauma", ClaimType::Trauma);
    assert_eq!(w.refused(Command::Notify(Box::new(n))), "not_configured");
}

/// Purpose: a notification opens a claim, posts the first reserve, routes it to a queue by skill
/// and starts the acknowledgement timer; a duplicate, a future event and an unknown policy are
/// refused.
/// Why it matters: the books must show the liability from the day of notification, and the
/// channel retrying a notification must not open a second claim.
#[test]
fn notification_opens_a_claim_with_a_reserve() {
    let mut w = World::new();
    notify(&mut w, "c-1", "pol-1", "trauma", ClaimType::Trauma);
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.queue, c.reserve_minor, c.claimed_minor),
        (Status::Open, Queue::LumpSum, 500_000, 500_000)
    );
    assert_eq!(c.policyholder, "party-holder");
    assert!(c.timers.contains_key(&TimerKind::Acknowledgement));
    assert!(matches!(&w.core.instructions[0], Instruction::SetReserve { amount_minor: 500_000, reason, .. } if reason == "notification"));
    let reserved = w.core.noticed("claim_reserved");
    assert_eq!(reserved[0]["gross_reserve_minor"], "500000");
    assert_eq!(reserved[0]["periodic"], "false");
    // Nothing clinical leaves the module.
    assert!(w.core.notices.iter().all(|(_, _, detail)| !detail.contains_key("cause")));

    let again = World::notification("c-1", "pol-1", "trauma", ClaimType::Trauma);
    assert_eq!(w.refused(Command::Notify(Box::new(again))), "claim_exists");
    let mut future = World::notification("c-2", "pol-1", "trauma", ClaimType::Trauma);
    future.event_date = d(2027, 3, 2);
    assert_eq!(w.refused(Command::Notify(Box::new(future))), "event_in_future");
    let elsewhere = World::notification("c-3", "pol-9", "trauma", ClaimType::Trauma);
    assert_eq!(w.refused(Command::Notify(Box::new(elsewhere))), "unknown_policy");
}

/// Purpose: a claim is registered even where the cover check says cover was not in force, and a
/// core that refuses the reserve leaves no claim behind.
/// Why it matters: refusing to register a claim is never a system behaviour, a decline is a
/// decision with reasons; and a claim with no reserve in the ledger would understate the books.
#[test]
fn doubtful_cover_still_registers_and_a_refused_reserve_leaves_nothing() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            in_force: false,
            ..facts("trauma")
        },
    );
    w.core.refuse_next = 1;
    let n = World::notification("c-1", "pol-1", "trauma", ClaimType::Trauma);
    assert_eq!(w.refused(Command::Notify(Box::new(n.clone()))), "ledger");
    w.ok(Command::Notify(Box::new(n)));
    assert_eq!(w.claim("c-1").status, Status::Open);
}

/// Purpose: a health claim notified without its invoice, as by phone, takes the invoice from its
/// owner, in a category the cover pays, and a wrong amount is put right the same way; nothing
/// changes once the claim is decided, and only the owner records it.
/// Why it matters: found by the scenario run (SCN-11, F-11-2 and F-11-3). A phone notification
/// had no way to take the invoice, so "admit" was refused for an amount the cover would pay, and
/// an invoice typed wrong could only be withdrawn and notified again; "surgery" was taken for a
/// plan whose category is "surgical".
#[test]
fn a_health_invoice_is_recorded_on_the_claim() {
    let mut w = World::new();
    w.ok(Command::Notify(Box::new(World::notification("h-phone", "pol-3", "health", ClaimType::Health))));
    w.assign("h-phone", "sue");
    let invoice = |category: &str, amount: i64, by: &str| Command::RecordInvoice {
        claim: "h-phone".into(),
        category: category.into(),
        invoice_minor: amount,
        provider: Some("provider-1".into()),
        actor: actor(by),
    };
    assert_eq!(w.refused(invoice("dental", 700, "ana")), "not_owner");
    assert_eq!(w.refused(invoice("dental", 0, "sue")), "invalid_invoice");
    assert_eq!(w.refused(invoice("surgery", 700, "sue")), "unknown_category");
    assert_eq!(
        w.told(invoice("surgery", 700, "sue")),
        "surgery is not a benefit category this cover pays: the categories are dental"
    );
    w.ok(invoice("dental", 7_850, "sue"));
    w.ok(invoice("dental", 785, "sue"));
    let claim = w.claim("h-phone");
    assert_eq!(
        (claim.claimed_minor, claim.health.as_ref().map(|h| (h.category.as_str(), h.invoice_minor))),
        (785, Some(("dental", 785)))
    );
    assert_eq!(claim.health.as_ref().and_then(|h| h.provider.clone()), Some("provider-1".to_string()));

    // Not a health claim; and a claim already decided keeps its invoice.
    w.open_claim("c-1");
    let mut other = invoice("dental", 700, "ana");
    if let Command::RecordInvoice { claim, .. } = &mut other {
        *claim = "c-1".into();
    }
    assert_eq!(w.refused(other), "not_health");
    w.ok(Command::Notify(Box::new(dental("h-auto", 600))));
    let mut decided = invoice("dental", 700, "sue");
    if let Command::RecordInvoice { claim, .. } = &mut decided {
        *claim = "h-auto".into();
    }
    assert_eq!(w.refused(decided), "invoice_after_decision");
}

/// Purpose: triage sends complex and large claims to the senior queue with a case conference,
/// group claims to the group queue, and the rest by line; priority rises with vulnerability,
/// hardship and a waiting provider; a family member may be the one who notifies (found by the
/// scenario run, SCN-12, F-12-5).
/// Why it matters: routing by skill is what stops a contested or very large claim being decided
/// by whoever picked it up, and priority is what gets a funeral paid for before a routine claim.
#[test]
fn triage_routes_by_skill_and_ranks_by_urgency() {
    let mut w = World::new();
    let mut contested = World::notification("c-contested", "pol-1", "trauma", ClaimType::Trauma);
    contested.flags.contested_cause = true;
    w.ok(Command::Notify(Box::new(contested)));
    assert_eq!(w.claim("c-contested").queue, Queue::ComplexLarge);
    assert!(w.claim("c-contested").has_open_task(TaskKind::CaseConference));

    let mut large = World::notification("c-large", "pol-1", "life", ClaimType::Death);
    large.claimed_minor = Some(2_000_000);
    w.ok(Command::Notify(Box::new(large)));
    assert_eq!(w.claim("c-large").queue, Queue::ComplexLarge);

    let mut group = World::notification("c-group", "pol-1", "life", ClaimType::Death);
    group.flags.group = true;
    w.ok(Command::Notify(Box::new(group)));
    assert_eq!(w.claim("c-group").queue, Queue::Group);

    notify(&mut w, "c-ip", "pol-2", "ip", ClaimType::IncomeProtection);
    assert_eq!(w.claim("c-ip").queue, Queue::IncomeProtectionNew);
    // The first reserve for a benefit is twelve months of it, by the rules.
    assert_eq!(w.claim("c-ip").reserve_minor, 48_000);
    assert_eq!(w.core.noticed("claim_reserved").last().expect("a notice")["periodic"], "true");

    let mut health = dental("c-health", 100_000);
    // A relative may ring for the family (found by the scenario run, SCN-12, F-12-5).
    health.notifier = ic_claims::model::Notifier::FamilyMember;
    health.flags = Flags {
        vulnerable: true,
        hardship: true,
        provider_waiting: true,
        ..Flags::default()
    };
    w.ok(Command::Notify(Box::new(health)));
    assert_eq!(w.claim("c-health").notifier, ic_claims::model::Notifier::FamilyMember);
    assert_eq!(w.claim("c-health").queue, Queue::Health);
    // Three urgency flags, and a large amount.
    assert_eq!(w.claim("c-health").priority, 90);
    assert_eq!(w.claim("c-large").priority, 20);
    let rows = w.state.queue(Queue::ComplexLarge, d(2027, 3, 4));
    assert_eq!(rows.iter().map(|r| r.claim.as_str()).collect::<Vec<_>>(), ["c-large", "c-contested"]);
    // The days since the event keep counting while the claim waits: twelve by now.
    assert_eq!(rows[0].priority, 20 + 12);
    assert_eq!(rows[0].age_days, 3);
}

/// Purpose: a provider's invoice inside the limits is priced and admitted by the rules with no
/// person involved, counted against the limits, recorded as a straight-through decision and
/// queued for the next provider run; an invoice beyond the limits goes to an assessor.
/// Why it matters: most health claims must not need a person, but each one has to be explainable
/// afterwards from the rule version, and the limit counters are what stop a policy paying more
/// than it promises.
#[test]
fn health_invoice_goes_straight_through_inside_the_limits() {
    let mut w = World::new();
    w.ok(Command::Notify(Box::new(dental("h-1", 600))));
    // 600 less the 50 excess, less the member's 20% share.
    let c = w.claim("h-1");
    assert_eq!((c.status, c.admitted_minor, c.payable_minor, c.reserve_minor), (Status::Admitted, 440, 440, 0));
    assert!(c.decisions[0].straight_through);
    assert_eq!((c.decisions[0].rules_version, c.decisions[0].authority_version), (1, 1));
    assert!(matches!(
        &w.core.instructions[0],
        Instruction::Admit {
            amount_minor: 440,
            release_reserve: true,
            ..
        }
    ));
    assert_eq!(w.core.noticed("claim_admitted")[0]["admitted_minor"], "440");
    assert!(w.state.provider_payable.contains("h-1"));
    assert_eq!(w.state.report().straight_through["dental"], 1);

    // The per-event limit caps a large invoice, and the year's limit caps the next.
    w.ok(Command::Notify(Box::new(dental("h-2", 5_000))));
    assert_eq!(w.claim("h-2").admitted_minor, 1_000);
    w.ok(Command::Notify(Box::new(dental("h-3", 5_000))));
    assert_eq!(w.claim("h-3").admitted_minor, 560);
    assert_eq!(w.state.health_used["pol-3/life-1/health/dental/2027-01-01"], 2_000);
    // Nothing left this year: the rules do not admit zero, a person looks at it.
    w.ok(Command::Notify(Box::new(dental("h-4", 700))));
    assert_eq!(w.claim("h-4").status, Status::Open);
    assert_eq!(w.claim("h-4").reserve_minor, 700);

    // A category the rules do not know is never paid by the rules.
    let mut unknown = dental("h-5", 300);
    unknown.health.as_mut().expect("a health item").category = "optical".into();
    w.ok(Command::Notify(Box::new(unknown)));
    assert_eq!(w.claim("h-5").status, Status::Open);
}

/// Purpose: straight-through admission is refused whenever anything is out of the ordinary: an
/// exclusion that matches or cannot be decided, premium owing, a claim above the threshold, a
/// life not on the policy or a core that refuses the posting.
/// Why it matters: straight-through is safe only if every doubt falls to a person. A cause the
/// rules cannot read must never clear an exclusion.
#[test]
fn straight_through_stops_at_any_doubt() {
    let mut w = World::new();
    let small = |id: &str, cause: Option<&str>| {
        let mut n = World::notification(id, "pol-1", "trauma", ClaimType::Trauma);
        n.claimed_minor = Some(4_000);
        n.cause_code = cause.map(str::to_string);
        n
    };
    // Clean: admitted by the rules.
    w.ok(Command::Notify(Box::new(small("t-1", Some("CC-0003")))));
    assert_eq!(w.claim("t-1").status, Status::Admitted);
    assert_eq!(w.state.report().straight_through["trauma"], 1);
    // The cover carries an exclusion the cause matches.
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            exclusions: vec!["EX-0001".into()],
            ..facts("trauma")
        },
    );
    w.ok(Command::Notify(Box::new(small("t-2", Some("CC-0001")))));
    assert_eq!(w.claim("t-2").status, Status::Open);
    // No cause, or a cause the rules do not know: the exclusion cannot be cleared.
    w.ok(Command::Notify(Box::new(small("t-3", None))));
    assert_eq!(w.claim("t-3").status, Status::Open);
    w.ok(Command::Notify(Box::new(small("t-4", Some("back pain")))));
    assert_eq!(w.claim("t-4").status, Status::Open);
    assert_eq!(w.claim("t-4").cause_code, None);
    assert!(w.claim("t-4").history.iter().any(|h| h.action == "cause_not_recognised"));
    // A cause that does not match the exclusion is clean again.
    w.ok(Command::Notify(Box::new(small("t-5", Some("CC-0003")))));
    assert_eq!(w.claim("t-5").status, Status::Admitted);

    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            standing: Some("in_grace".into()),
            ..facts("trauma")
        },
    );
    w.ok(Command::Notify(Box::new(small("t-6", Some("CC-0003")))));
    assert_eq!(w.claim("t-6").status, Status::Open);
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            life_on_policy: false,
            ..facts("trauma")
        },
    );
    w.ok(Command::Notify(Box::new(small("t-7", Some("CC-0003")))));
    assert_eq!(w.claim("t-7").status, Status::Open);
    w.core.cover("pol-1", "trauma", facts("trauma"));
    let mut big = small("t-8", Some("CC-0003"));
    big.claimed_minor = Some(5_001);
    w.ok(Command::Notify(Box::new(big)));
    assert_eq!(w.claim("t-8").status, Status::Open);
    let mut over = small("t-9", Some("CC-0003"));
    over.claimed_minor = Some(600_000);
    w.ok(Command::Notify(Box::new(over)));
    assert_eq!(w.claim("t-9").status, Status::Open);
    // Death is never paid by the rules.
    let mut death = small("t-10", Some("CC-0003"));
    death.claim_type = ClaimType::Death;
    w.ok(Command::Notify(Box::new(death)));
    assert_eq!(w.claim("t-10").status, Status::Open);

    w.core.refuse_next = 1;
    assert_eq!(w.refused(Command::Notify(Box::new(small("t-11", Some("CC-0003"))))), "ledger");
}

/// Purpose: a pre-approval holds an amount against the limits without posting anything; the
/// invoice that settles it releases the hold and posts the real amount; an unused hold expires.
/// Why it matters: a pre-approval is a promise to pay. If it did not count against the limits the
/// same benefit could be promised twice, and if it never expired the limit would be lost for good.
#[test]
fn pre_approval_is_a_hold_not_a_posting() {
    let mut w = World::new();
    let mut request = dental("pre-1", 1_300);
    request.health.as_mut().expect("a health item").pre_approval_request = true;
    w.ok(Command::Notify(Box::new(request)));
    assert!(w.core.instructions.is_empty());
    assert_eq!(w.state.pre_approvals["pre-1"].amount_minor, 1_000);
    assert_eq!(w.core.noticed("pre_approval_given")[0]["amount_minor"], "1000");
    assert_eq!(w.claim("pre-1").status, Status::Open);

    // The hold counts: another invoice this year can only have what is left.
    w.ok(Command::Notify(Box::new(dental("h-1", 5_000))));
    assert_eq!(w.claim("h-1").admitted_minor, 1_000);
    w.ok(Command::Notify(Box::new(dental("h-2", 5_000))));
    assert_eq!(w.claim("h-2").status, Status::Open, "the year's limit is used and held");

    // The invoice for the approved procedure does not compete with its own hold.
    let mut invoice = dental("h-3", 1_250);
    invoice.health.as_mut().expect("a health item").settles_pre_approval = Some("pre-1".into());
    w.ok(Command::Notify(Box::new(invoice)));
    assert_eq!(w.claim("h-3").admitted_minor, 960);
    assert!(w.state.pre_approvals.is_empty());
    assert_eq!(w.claim("pre-1").status, Status::Withdrawn);
    assert_eq!(w.core.noticed("pre_approval_released")[0]["reason"], "settled");

    // A hold nobody uses expires on its date.
    let mut w = World::new();
    let mut request = dental("pre-2", 500);
    request.health.as_mut().expect("a health item").pre_approval_request = true;
    w.ok(Command::Notify(Box::new(request)));
    w.advance(29);
    w.ok(Command::DrainDue);
    assert_eq!(w.state.pre_approvals.len(), 1);
    w.advance(1);
    assert_eq!(w.state.due_backlog(w.core.today), 1);
    w.ok(Command::DrainDue);
    assert!(w.state.pre_approvals.is_empty());
    assert_eq!(w.claim("pre-2").status, Status::Withdrawn);
    assert_eq!(w.core.noticed("pre_approval_released")[0]["reason"], "expired");
    // The limit the hold took is the member's again.
    w.ok(Command::Notify(Box::new(dental("h-9", 5_000))));
    assert_eq!(w.claim("h-9").admitted_minor, 1_000);
}

/// Purpose: the acknowledgement needs proof of delivery, happens once, and starts the first
/// contact and decision timers; the decision target doubles for a vulnerable claimant.
/// Why it matters: the service standards are measured from acknowledgement, and the process must
/// slow rather than fail for a claimant who cannot respond quickly.
#[test]
fn acknowledgement_starts_the_service_timers() {
    let mut w = World::new();
    notify(&mut w, "c-1", "pol-1", "trauma", ClaimType::Trauma);
    let ack = |proof: &str| Command::Acknowledge {
        claim: "c-1".into(),
        delivery_ref: proof.into(),
    };
    assert_eq!(w.refused(ack("")), "acknowledgement");
    w.advance(1);
    w.ok(ack("delivered-1"));
    let c = w.claim("c-1");
    assert_eq!(c.timers[&TimerKind::Acknowledgement].met_on, Some(d(2027, 3, 2)));
    assert_eq!(c.timers[&TimerKind::FirstContact].due, d(2027, 3, 7));
    assert_eq!(c.timers[&TimerKind::Decision].due, d(2027, 3, 22));
    assert_eq!(w.refused(ack("delivered-2")), "acknowledgement");
    assert_eq!(
        w.refused(Command::Acknowledge {
            claim: "nope".into(),
            delivery_ref: "x".into()
        }),
        "unknown_claim"
    );

    let mut vulnerable = World::notification("c-2", "pol-1", "trauma", ClaimType::Trauma);
    vulnerable.flags.vulnerable = true;
    w.ok(Command::Notify(Box::new(vulnerable)));
    w.ok(Command::Acknowledge {
        claim: "c-2".into(),
        delivery_ref: "delivered-3".into(),
    });
    assert_eq!(w.claim("c-2").timers[&TimerKind::Decision].due, d(2027, 4, 11));
}

/// Purpose: a service timer that is missed warns the owner, then the queue lead, then the claims
/// manager, on the configured days; a timer that was met warns nobody.
/// Why it matters: escalation that depends on someone remembering is not escalation. The business
/// date reaching the day is what raises it, on every replica alike.
#[test]
fn a_missed_timer_escalates_in_three_steps() {
    let mut w = World::new();
    notify(&mut w, "c-1", "pol-1", "trauma", ClaimType::Trauma);
    notify(&mut w, "c-2", "pol-1", "trauma", ClaimType::Trauma);
    w.ok(Command::Acknowledge {
        claim: "c-2".into(),
        delivery_ref: "delivered".into(),
    });
    // Acknowledgement is due in 2 days; the steps follow 1, 3 and 7 days later.
    w.advance(3);
    w.ok(Command::DrainDue);
    assert_eq!(w.claim("c-1").timers[&TimerKind::Acknowledgement].escalated, 1);
    assert_eq!(w.core.noticed("timer_escalated").len(), 1);
    w.advance(6);
    w.ok(Command::DrainDue);
    assert_eq!(w.claim("c-1").timers[&TimerKind::Acknowledgement].escalated, 3);
    let levels: Vec<&str> = w
        .core
        .noticed("timer_escalated")
        .iter()
        .filter(|n| n["timer"] == "Acknowledgement")
        .map(|n| n["level"].as_str())
        .collect();
    assert_eq!(levels, ["1", "2", "3"]);
    assert_eq!(w.claim("c-2").timers[&TimerKind::Acknowledgement].escalated, 0);
    assert_eq!(w.state.report().timers[&TimerKind::Acknowledgement], (1, 1));
    // First contact on the second claim was due on day 5 and is now late too.
    assert_eq!(w.claim("c-2").timers[&TimerKind::FirstContact].escalated, 2);
    w.assign("c-2", "ana");
    w.ok(Command::RecordContact {
        claim: "c-2".into(),
        actor: actor("ana"),
    });
    assert_eq!(w.claim("c-2").timers[&TimerKind::FirstContact].met_on, Some(d(2027, 3, 10)));
    assert_eq!(w.state.report().timers[&TimerKind::FirstContact], (0, 1));
    w.advance(30);
    w.ok(Command::DrainDue);
    assert_eq!(w.claim("c-2").timers[&TimerKind::FirstContact].escalated, 2, "a met timer goes no further");
}

/// Purpose: a claim is offered only to someone accredited for its line and queue, senior where
/// the queue is, with authority for its type and amount, trained where the claimant is
/// vulnerable, under their work limit, and with no conflict; only a lead assigns to others.
/// Why it matters: routing by authority and by conflict is a control. Someone who underwrote the
/// life, or is related to the claimant, deciding the claim is exactly what it exists to stop. A
/// refusal for authority names the limit and who may take the claim (found by the scenario run,
/// SCN-11, F-11-5), so the claim does not sit unassigned; so does a refusal for the line, naming the
/// person's own lines (SCN2-09, F2-09-9).
#[test]
fn a_claim_is_offered_only_to_someone_who_may_work_it() {
    let mut w = World::new();
    notify(&mut w, "c-1", "pol-1", "trauma", ClaimType::Trauma);
    let assign = |to: &str, by: &str| Command::Assign {
        claim: "c-1".into(),
        to: to.into(),
        actor: actor(by),
    };
    assert_eq!(w.refused(assign("ben", "ana")), "not_authorised");
    assert_eq!(w.refused(assign("nobody", "sam")), "unknown_staff");
    assert_eq!(w.refused(assign("jon", "sam")), "not_accredited");
    // The refusal names the person's lines and who could take it (SCN2-09, F2-09-9).
    let told = w.told(assign("jon", "sam"));
    assert!(
        told.starts_with("jon is not accredited for lump sum claims in this queue (their lines: health): "),
        "{told}"
    );
    assert!(told.contains("ben") && told.ends_with("can take it"), "{told}");
    assert_eq!(w.refused(assign("mo", "sam")), "not_accredited");
    assert_eq!(w.refused(assign("una", "sam")), "conflict_of_interest");
    assert_eq!(w.refused(assign("rel", "sam")), "conflict_of_interest");
    let mut no_purpose = actor("sam");
    no_purpose.purpose = " ".into();
    assert_eq!(
        w.refused(Command::Assign {
            claim: "c-1".into(),
            to: "ana".into(),
            actor: no_purpose
        }),
        "purpose_required"
    );
    // Taking a claim from the queue yourself is allowed.
    w.ok(assign("ana", "ana"));
    assert_eq!(w.core.noticed("claim_assigned")[0]["handover"], "false");
    w.ok(assign("ben", "sam"));
    assert_eq!(w.core.noticed("claim_assigned")[1]["handover"], "true");
    assert_eq!(w.state.report().workload["ben"], 1);
    assert!(!w.state.report().workload.contains_key("ana"));

    let mut vulnerable = World::notification("c-2", "pol-1", "trauma", ClaimType::Trauma);
    vulnerable.flags.vulnerable = true;
    w.ok(Command::Notify(Box::new(vulnerable)));
    let to = |claim: &str, to: &str| Command::Assign {
        claim: claim.into(),
        to: to.into(),
        actor: actor("sam"),
    };
    assert_eq!(w.refused(to("c-2", "ana")), "not_trained");
    w.ok(to("c-2", "ben"));

    let mut contested = World::notification("c-3", "pol-1", "trauma", ClaimType::Trauma);
    contested.flags.overseas_event = true;
    w.ok(Command::Notify(Box::new(contested)));
    assert_eq!(w.refused(to("c-3", "ana")), "not_accredited");
    w.ok(to("c-3", "sue"));

    // A health claim above the junior's limit.
    w.ok(Command::Notify(Box::new(dental("h-1", 100_000))));
    assert_eq!(w.refused(to("h-1", "jon")), "authority");
    // The refusal names the limit and who can take the claim instead (found by the scenario
    // run, F-11-5).
    let told = w.told(to("h-1", "jon"));
    assert!(
        told.starts_with("jon may admit up to $") && told.contains("this one is $1,000.00: ") && told.contains(" can take it, given it by a queue lead"),
        "{told}"
    );

    // Work in progress limits.
    let mut book = authority();
    book.version = 2;
    book.staff.get_mut("ben").expect("ben is in the book").wip_limit = 2;
    book.staff.get_mut("ben").expect("ben is in the book").queues.insert(Queue::ComplexLarge);
    w.ok(Command::ActivateAuthority { book: Box::new(book) });
    notify(&mut w, "c-4", "pol-1", "trauma", ClaimType::Trauma);
    assert_eq!(w.refused(to("c-4", "ben")), "wip_limit");
    w.ok(to("c-2", "ben"));

    // Sitting on the senior queue's list does not make someone a senior, and a closed claim
    // is given to nobody.
    assert_eq!(w.refused(to("c-3", "ben")), "not_senior");
    w.ok(Command::Decide {
        claim: "c-3".into(),
        request: ic_claims::Request::Withdraw,
        reasons: vec!["R-OK".into()],
        actor: actor("sue"),
    });
    assert_eq!(w.refused(to("c-3", "sam")), "claim_closed");
}

/// Purpose: only the owner records the cause and the cover check; the cause must be on the
/// controlled list; the cover check stores the facts and what was relied on.
/// Why it matters: every decision must be reproducible from the cover-check facts and the cause
/// as recorded. A free-text cause is one the exclusion rules cannot read.
#[test]
fn the_owner_records_cause_and_cover_check() {
    let mut w = World::new();
    w.open_claim("c-1");
    let cause = |code: &str, who: &str| Command::RecordCause {
        claim: "c-1".into(),
        cause_code: code.into(),
        actor: actor(who),
    };
    assert_eq!(w.refused(cause("CC-0001", "ben")), "not_owner");
    assert_eq!(w.refused(cause("a sore back", "ana")), "unknown_cause");
    // The refusal lists the codes there are (found by the scenario run, F-11-9).
    let told = w.told(cause("a sore back", "ana"));
    assert!(
        told.starts_with("a sore back is not on the controlled list of cause codes: the codes are CC-"),
        "{told}"
    );
    w.ok(cause("CC-0001", "ana"));
    w.ok(cause("CC-0003", "ana"));
    assert_eq!(w.claim("c-1").cause_history.len(), 2);
    let check = |who: &str| Command::CoverCheck {
        claim: "c-1".into(),
        relied_on: vec!["in_force".into()],
        actor: actor(who),
    };
    assert_eq!(w.refused(check("ben")), "not_owner");
    w.ok(check("ana"));
    let recorded = w.claim("c-1").cover_check.as_ref().expect("the check is recorded");
    assert_eq!((recorded.facts.sum_insured_minor, recorded.by.as_str()), (500_000, "ana"));
    assert_eq!(recorded.relied_on, ["in_force"]);
    assert!(w.claim("c-1").touched_by.contains("ana"));
}

/// Purpose: an evidence request has a due date, shows as overdue when the date passes, and is
/// closed by the evidence arriving with its place in the vault and its cost.
/// Why it matters: claims stall on evidence. A request with no timer is one nobody chases, and
/// the cost of evidence per claim is a reported figure.
#[test]
fn evidence_is_requested_chased_and_received() {
    let mut w = World::new();
    w.open_claim("c-1");
    let request = |id: &str, who: &str| Command::RequestEvidence {
        claim: "c-1".into(),
        evidence: id.into(),
        kind: "medical_report".into(),
        from: "treating-doctor".into(),
        actor: actor(who),
    };
    assert_eq!(w.refused(request("e-1", "ben")), "not_owner");
    w.ok(request("e-1", "ana"));
    w.ok(request("e-2", "ana"));
    assert_eq!(w.refused(request("e-1", "ana")), "evidence_exists");
    assert_eq!(w.state.claimant_view("c-1").expect("a view").evidence_outstanding.len(), 2);
    let receive = |id: &str, vault: &str| Command::ReceiveEvidence {
        claim: "c-1".into(),
        evidence: id.into(),
        vault_ref: vault.into(),
        cost_minor: 150,
    };
    assert_eq!(w.refused(receive("e-1", "")), "evidence");
    assert_eq!(w.refused(receive("e-9", "vault-9")), "evidence");
    w.ok(receive("e-1", "vault-1"));
    assert_eq!(w.refused(receive("e-1", "vault-1")), "evidence");
    w.advance(21);
    w.ok(Command::DrainDue);
    let overdue = w.core.noticed("evidence_overdue");
    assert_eq!(overdue.len(), 1);
    assert_eq!(overdue[0]["evidence"], "e-2");
    assert_eq!(w.state.report().evidence_cost_minor, 150);
    assert_eq!(w.state.claimant_view("c-1").expect("a view").evidence_outstanding[0].0, "e-2");
}

/// Purpose: people open only the kinds of task that are theirs to open, and each kind is
/// completed only by the right person: a medical officer for an opinion, a reviewer who did not
/// touch the case for a quality review, a senior for a senior review.
/// Why it matters: a task is how the module proves a step happened. A case conference its owner
/// signs off alone, or a peer review by the person who made the decision, proves nothing.
#[test]
fn tasks_are_completed_by_the_right_person() {
    let mut w = World::new();
    w.open_claim("c-1");
    let open = |task: &str, kind: TaskKind, who: &str| Command::OpenTask {
        claim: "c-1".into(),
        task: task.into(),
        kind,
        due: None,
        actor: actor(who),
    };
    assert_eq!(w.refused(open("t-1", TaskKind::MedicalOpinion, "ben")), "not_owner");
    assert_eq!(w.refused(open("t-1", TaskKind::QualityReview, "ana")), "system_task");
    w.ok(open("opinion", TaskKind::MedicalOpinion, "ana"));
    assert_eq!(w.refused(open("opinion", TaskKind::MedicalOpinion, "ana")), "task_exists");
    // A lead may open a task on a claim they do not own.
    w.ok(open("conference", TaskKind::CaseConference, "sam"));
    w.ok(open("senior", TaskKind::SeniorReview, "ana"));
    w.ok(open("talk", TaskKind::ClaimantConversation, "ana"));
    w.ok(open("rehab", TaskKind::Rehabilitation, "ana"));
    w.ok(open("arrangement", TaskKind::RepaymentArrangement, "ana"));
    w.ok(open("ndr-7", TaskKind::NonDisclosureReview, "ana"));
    assert_eq!(w.core.noticed("non_disclosure_review_requested")[0]["review_ref"], "ndr-7");

    let done = |task: &str, who: &str, attendees: usize, vault: bool, spend: i64| Command::CompleteTask {
        claim: "c-1".into(),
        task: task.into(),
        outcome: "done".into(),
        attendees: ["sam", "sue", "mo"].iter().take(attendees).map(|s| s.to_string()).collect(),
        vault_ref: vault.then(|| "vault-7".to_string()),
        spend_minor: spend,
        actor: actor(who),
    };
    assert_eq!(w.refused(done("missing", "ana", 0, false, 0)), "unknown_task");
    assert_eq!(w.refused(done("opinion", "ana", 0, true, 0)), "not_authorised");
    assert_eq!(w.refused(done("opinion", "mo", 0, false, 0)), "not_authorised");
    w.ok(done("opinion", "mo", 0, true, 0));
    assert_eq!(w.refused(done("opinion", "mo", 0, true, 0)), "unknown_task");
    assert_eq!(w.refused(done("conference", "ana", 1, false, 0)), "not_authorised");
    assert_eq!(w.refused(done("conference", "ben", 2, false, 0)), "not_authorised");
    w.ok(done("conference", "sue", 2, false, 0));
    assert_eq!(w.refused(done("senior", "ana", 0, false, 0)), "not_authorised");
    w.ok(done("senior", "sam", 0, false, 0));
    assert_eq!(w.refused(done("talk", "ben", 0, false, 0)), "not_authorised");
    w.ok(done("talk", "ana", 0, false, 0));
    assert_eq!(w.refused(done("rehab", "ana", 0, false, 600_001)), "not_authorised");
    assert_eq!(w.refused(done("rehab", "ana", 0, false, -1)), "not_authorised");
    w.ok(done("rehab", "ana", 0, false, 2_000));
    assert_eq!(w.claim("c-1").task(TaskKind::Rehabilitation).next().expect("the task").spend_minor, 2_000);
    assert_eq!(w.refused(done("arrangement", "ben", 0, false, 0)), "not_authorised");
    w.ok(done("arrangement", "rec", 0, false, 0));
    assert_eq!(w.refused(done("ndr-7", "ana", 0, false, 0)), "system_task");
    assert_eq!(w.refused(done("ndr-7", "una", 0, false, 0)), "conflict_of_interest");

    // Underwriting answers the non-disclosure review; nobody in claims does.
    let outcome = |review: &str| Command::NonDisclosureOutcome {
        claim: "c-1".into(),
        review_ref: review.into(),
        outcome: ic_claims::command::NonDisclosureOutcome::NoAction,
    };
    assert_eq!(w.refused(outcome("ndr-8")), "unknown_task");
    w.ok(outcome("ndr-7"));
    assert!(!w.claim("c-1").has_open_task(TaskKind::NonDisclosureReview));
    assert_eq!(w.refused(outcome("ndr-7")), "unknown_task");
}

/// Purpose: the clinical file opens only for the claim's owner or for someone reviewing it
/// through an open task or a pending signature, never for someone with a conflict, and every
/// view is logged with its purpose.
/// Why it matters: this is health information. Access by role alone would let any manager read
/// any file; the rule is access by what you are doing, on the record.
#[test]
fn the_file_opens_only_to_the_owner_or_a_reviewer() {
    let mut w = World::new();
    w.open_claim("c-1");
    let open = |who: &str| Command::OpenFile {
        claim: "c-1".into(),
        actor: actor(who),
    };
    w.ok(open("ana"));
    for stranger in ["ben", "sam", "sue", "mo", "quin"] {
        assert_eq!(w.refused(open(stranger)), "no_access", "{stranger}");
    }
    assert_eq!(w.refused(open("una")), "conflict_of_interest");
    w.task("c-1", "opinion", TaskKind::MedicalOpinion, "ana");
    w.ok(open("mo"));
    assert_eq!(w.refused(open("sue")), "no_access");
    w.task("c-1", "senior", TaskKind::SeniorReview, "ana");
    w.ok(open("sue"));
    w.ok(open("sam"));
    let views: Vec<_> = w.claim("c-1").history.iter().filter(|h| h.action == "file_opened").collect();
    assert_eq!(views.len(), 4);
    assert!(views.iter().all(|h| h.purpose.as_deref() == Some("assessing the claim")));
    // Looking is not acting: a reader has not touched the claim.
    assert!(!w.claim("c-1").touched_by.contains("mo"));
}

/// Purpose: escalation moves a claim to the senior queue, opens a case conference and takes the
/// claim off an owner who is not senior; identification at claim links an unnamed member through
/// the core.
/// Why it matters: the senior queue is where reversals and contested claims are decided, and a
/// group claim for an unnamed member cannot be paid until the member is a person.
#[test]
fn escalation_and_identification() {
    let mut w = World::new();
    w.open_claim("c-1");
    let escalate = |who: &str| Command::Escalate {
        claim: "c-1".into(),
        actor: actor(who),
    };
    assert_eq!(w.refused(escalate("ben")), "not_owner");
    let identify = |party: &str, who: &str| Command::IdentifyLife {
        claim: "c-1".into(),
        party: party.into(),
        actor: actor(who),
    };
    assert_eq!(w.refused(identify("party-new", "ben")), "not_owner");
    assert_eq!(w.refused(identify("", "ana")), "no_life");
    w.core.refuse_next = 1;
    assert_eq!(w.refused(identify("party-new", "ana")), "ledger");
    w.ok(identify("party-new", "ana"));
    assert_eq!(w.claim("c-1").life_party.as_deref(), Some("party-new"));
    assert!(matches!(w.core.instructions.last(), Some(Instruction::IdentifyMember { life, party, .. }) if life == "life-1" && party == "party-new"));
    assert!(w.state.by_party["party-new"].contains("c-1"));
    assert!(!w.state.by_party["party-life"].contains("c-1"));

    w.ok(escalate("ana"));
    let c = w.claim("c-1");
    assert_eq!((c.queue, c.owner.clone()), (Queue::ComplexLarge, None));
    assert!(c.has_open_task(TaskKind::CaseConference));
    w.assign("c-1", "sam");
    // A lead may escalate again; a senior owner keeps the claim.
    w.ok(escalate("sue"));
    assert_eq!(w.claim("c-1").owner.as_deref(), Some("sam"));
    assert_eq!(w.claim("c-1").task(TaskKind::CaseConference).count(), 2);
}

/// Purpose: a death claim tells the customer master the life has died, and a death notified long
/// after it happened raises a check of the death register feed.
/// Why it matters: design addendum, entry 1. An earlier match on the register would have
/// shortened the window in which premiums were collected and benefits paid after the death.
#[test]
fn a_late_notified_death_prompts_a_register_check() {
    let mut w = World::new();
    let mut prompt = World::notification("c-prompt", "pol-1", "life", ClaimType::Death);
    prompt.event_date = d(2027, 2, 1);
    w.ok(Command::Notify(Box::new(prompt)));
    assert!(!w.claim("c-prompt").has_open_task(TaskKind::DeathRegisterCadence));
    assert_eq!(w.core.noticed("death_notified")[0]["date_of_death"], "2027-02-01");

    let mut late = World::notification("c-late", "pol-1", "life", ClaimType::Death);
    late.event_date = d(2027, 1, 15);
    w.ok(Command::Notify(Box::new(late)));
    assert!(w.claim("c-late").has_open_task(TaskKind::DeathRegisterCadence));
    assert_eq!(
        w.core
            .notices
            .iter()
            .filter(|(kind, claim, _)| kind == "death_register_cadence_check" && claim == "c-late")
            .count(),
        1
    );
    // The check is a lead's to close.
    assert!(w.complete("c-late", "death-register-cadence", "feed_daily", "ana").is_err());
    w.complete("c-late", "death-register-cadence", "feed_daily", "sam")
        .expect("a lead closes the check");
}

fn load(claim: &ic_claims::Claim) -> Command {
    Command::LoadMigrated {
        claim: Box::new(claim.clone()),
        health_used: Default::default(),
    }
}

/// Purpose: a claim in payment loaded from another system arrives at the step it had reached:
/// its reserve is opened in the core in the same command, it is marked as migrated, its reviews
/// are on the calendar, and the next run pays the period after the last one the old system paid.
/// Why it matters: the design promises that the first payment from the new platform matches the
/// last from the old. A loader that reopened the claim as new, or lost the review dates, would
/// pay a period twice or let a claim run unreviewed.
#[test]
fn a_migrated_claim_in_payment_carries_on_where_it_was() {
    // The old system: a benefit in payment with March paid.
    let mut old = World::new();
    old.benefit_in_payment("ip-1");
    old.pay_run("run-old", d(2027, 3, 1));
    let mut legacy = old.claim("ip-1").clone();
    legacy.reserve_minor = 50_000;

    let mut w = World::new();
    w.core.refuse_next = 1;
    assert_eq!(w.refused(load(&legacy)), "ledger");
    w.ok(load(&legacy));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::OpenMigrated {
            reserve_minor: 50_000,
            payable_minor: 0,
            receivable_minor: 0,
            open: true,
            ..
        })
    ));
    let c = w.claim("ip-1");
    assert!(c.migrated && w.state.in_payment.contains("ip-1"));
    assert!(c.history.iter().any(|h| h.action == "migrated"));
    assert_eq!(w.core.noticed("claim_migrated")[0]["status"], "in_payment");
    assert!(c
        .due
        .iter()
        .any(|(on, due)| *on == d(2027, 6, 1) && matches!(due, ic_claims::model::Due::Review(_))));
    assert_eq!(w.refused(load(&legacy)), "claim_exists");

    w.on(d(2027, 4, 1));
    w.pay_run("run-1", d(2027, 4, 1));
    assert_eq!(w.state.runs["run-1"].count, 1, "April only: March was paid before the move");
    assert_eq!(w.claim("ip-1").schedule.as_ref().expect("in payment").periods.len(), 2);
}

/// Purpose: a closed claim loads with the health limits it used, and a record that cannot be
/// carried consistently is refused whole: money below zero, more payable than admitted, a
/// claim in a run or with a decision waiting, a status that does not fit its money or its
/// schedule, limits keyed by another policy, or a policy this partition does not hold.
/// Why it matters: earlier claims limit later ones, so closed claims must come across. And a
/// loader that accepted a half-consistent record would put a wrong opening entry in the ledger
/// that nobody could trace back to the old system's total.
#[test]
fn a_migration_record_is_loaded_whole_or_refused() {
    let mut old = World::new();
    old.open_claim("c-1");
    old.assess("c-1", "CC-0003");
    old.decided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK");
    let admitted = old.claim("c-1").clone();
    old.verify_payee("c-1", "ana");
    old.ok(Command::Pay {
        claim: "c-1".into(),
        payment_ref: "p-1".into(),
        amount_minor: 200_000,
        reissue_of: None,
        actor: actor("ana"),
    });
    let paid = old.claim("c-1").clone();

    let mut w = World::new();
    let broken = |change: &dyn Fn(&mut ic_claims::Claim)| {
        let mut c = admitted.clone();
        change(&mut c);
        load(&c)
    };
    for command in [
        broken(&|c| c.reserve_minor = -1),
        broken(&|c| c.payable_minor = c.admitted_minor + 1),
        broken(&|c| c.cost_payable_minor = 5),
        broken(&|c| c.in_run = Some("run-9".into())),
        // Closed, yet money still owed on it.
        broken(&|c| c.status = Status::Paid),
        // In payment with no schedule.
        broken(&|c| c.status = Status::InPayment),
    ] {
        assert_eq!(w.refused(command), "invalid_migration");
    }
    assert_eq!(w.refused(broken(&|c| c.policy = "pol-elsewhere".into())), "unknown_policy");
    let other_policy = Command::LoadMigrated {
        claim: Box::new(paid.clone()),
        health_used: [("pol-9/life-1/dental".to_string(), 100)].into(),
    };
    assert_eq!(w.refused(other_policy), "invalid_migration");

    // Admitted and unpaid: the money owed is opened as payable.
    w.ok(load(&admitted));
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::OpenMigrated {
            admitted_minor: 200_000,
            payable_minor: 200_000,
            open: true,
            ..
        })
    ));
    w.verify_payee("c-1", "ana");
    w.ok(Command::Pay {
        claim: "c-1".into(),
        payment_ref: "p-new".into(),
        amount_minor: 200_000,
        reissue_of: None,
        actor: actor("ana"),
    });
    assert_eq!(w.claim("c-1").status, Status::Paid);

    // Closed and paid, with the limits it used.
    let mut closed = paid;
    closed.id = "c-2".into();
    w.ok(Command::LoadMigrated {
        claim: Box::new(closed),
        health_used: [("pol-1/life-1/dental".to_string(), 100)].into(),
    });
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::OpenMigrated {
            reserve_minor: 0,
            payable_minor: 0,
            open: false,
            ..
        })
    ));
    assert_eq!(w.state.health_used["pol-1/life-1/dental"], 100);
    assert!(w.claim("c-2").migrated);
}

/// Purpose: a death claim for someone lost at sea, with no body and no death certificate, is
/// registered and reserved like any other; the assessor asks the coroner for a finding, which has
/// its own due date, and when the date arrives the overdue request is published and noted.
/// Why it matters: persona test CLM-005. A family waiting on a coroner must not be told the
/// claim cannot be lodged: refusing to register is never a behaviour. The reserve must be in the
/// books from notification, and an evidence request without a timer is one nobody chases.
#[test]
fn a_death_with_no_certificate_is_registered_and_the_coroner_is_chased() {
    let mut w = World::new();
    let mut lost = World::notification("c-sea", "pol-1", "life", ClaimType::Death);
    lost.notifier = ic_claims::model::Notifier::Beneficiary;
    lost.claimant = Some("party-widow".into());
    w.ok(Command::Notify(Box::new(lost)));
    let c = w.claim("c-sea");
    assert_eq!(c.status, Status::Open);
    assert!(c.reserve_minor > 0, "reserved from notification");
    assert!(c.evidence.is_empty(), "nothing is demanded before a claim is registered");
    w.ok(Command::Acknowledge {
        claim: "c-sea".into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign("c-sea", "ana");
    w.ok(Command::RequestEvidence {
        claim: "c-sea".into(),
        evidence: "coroner".into(),
        kind: "coroner_finding".into(),
        from: "coroner".into(),
        actor: actor("ana"),
    });
    let due = w.claim("c-sea").evidence[0].due;
    assert!(due > w.core.today);
    assert_eq!(w.core.noticed("evidence_requested")[0]["due"], due.to_string());
    w.on(due.pred_opt().expect("a real date"));
    w.ok(Command::DrainDue);
    assert!(w.core.noticed("evidence_overdue").is_empty(), "not chased before its due date");
    w.on(due);
    w.ok(Command::DrainDue);
    assert_eq!(w.core.noticed("evidence_overdue")[0]["evidence"], "coroner");
    assert_eq!(w.claim("c-sea").status, Status::Open, "the claim waits; it is not declined");
}

/// Purpose: a claim in a queue is never shown to someone with a declared relationship to anyone
/// on it, or who underwrote the life: the queue they ask for leaves it out and its working view
/// answers as if there were no such claim, while colleagues see it as usual.
/// Why it matters: persona test CLM-033. A cousin working the queue a relative's claim lands in
/// must not be offered it, and seeing that the claim exists is already a disclosure of health.
#[test]
fn a_claim_is_never_offered_to_a_relative_in_the_queue() {
    use ic_claims::report::{Question, Reply};
    let mut w = World::new();
    notify(&mut w, "c-1", "pol-1", "trauma", ClaimType::Trauma);
    let today = w.core.today;
    let queue = |w: &World, staff: Option<&str>| match w.state.answer(
        &Question::Queue {
            queue: Queue::LumpSum,
            staff: staff.map(str::to_string),
        },
        today,
    ) {
        Reply::Queue(rows) => rows.into_iter().map(|r| r.claim).collect::<Vec<_>>(),
        other => panic!("a queue question gets a queue, not {other:?}"),
    };
    let work = |w: &World, staff: &str| {
        matches!(
            w.state.answer(
                &Question::Work {
                    claim: "c-1".into(),
                    staff: Some(staff.into()),
                },
                today,
            ),
            Reply::Work(Some(_))
        )
    };
    assert_eq!(queue(&w, Some("ana")), ["c-1"]);
    assert_eq!(queue(&w, None), ["c-1"], "the whole queue for the system");
    // rel declared a relationship with the policyholder; una underwrote the life.
    for conflicted in ["rel", "una"] {
        assert!(queue(&w, Some(conflicted)).is_empty(), "{conflicted} is shown the claim");
        assert!(!work(&w, conflicted), "{conflicted} can open the claim");
    }
    assert!(work(&w, "ana"));
    assert_eq!(
        w.refused(Command::Assign {
            claim: "c-1".into(),
            to: "rel".into(),
            actor: actor("rel")
        }),
        "conflict_of_interest"
    );
}

/// Purpose: a claimant with a communication marker keeps one named owner, a change of owner is
/// recorded as a handover with a call to the claimant, and every deadline the claimant has to
/// meet (evidence asked of them, the grace on a review) moves out by the rules' extension, once,
/// while deadlines on others stay as they were.
/// Why it matters: persona test CLM-042. Someone who needs more time or a familiar voice is
/// otherwise chased on the standard clock and passed between strangers, which is how a claim is
/// lost for want of a reply rather than for want of cover.
#[test]
fn a_communication_marker_gives_one_owner_a_said_handover_and_more_time() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.communication_extra_days = 10;
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w.open_claim("c-1");
    let ask = |evidence: &str, from: &str| Command::RequestEvidence {
        claim: "c-1".into(),
        evidence: evidence.into(),
        kind: "statement".into(),
        from: from.into(),
        actor: actor("ana"),
    };
    w.ok(ask("e-1", "claimant"));
    w.ok(ask("e-2", "treating-doctor"));
    let due = |w: &World, id: &str| w.claim("c-1").evidence.iter().find(|e| e.id == id).expect("asked").due;
    let (mine, doctors) = (due(&w, "e-1"), due(&w, "e-2"));
    let marker = |claim: &str, needed: bool, who: &str| Command::RecordCommunicationNeed {
        claim: claim.into(),
        needed,
        actor: actor(who),
    };
    assert_eq!(w.refused(marker("c-1", true, "ben")), "not_owner");
    w.ok(marker("c-1", true, "ana"));
    w.ok(marker("c-1", true, "ana"));
    assert_eq!(due(&w, "e-1"), mine + chrono::Days::new(10), "moved once");
    assert_eq!(due(&w, "e-2"), doctors, "the doctor's deadline is not the claimant's");
    w.ok(ask("e-3", "party-claimant"));
    assert_eq!(due(&w, "e-3"), mine + chrono::Days::new(10));
    assert_eq!(w.core.noticed("communication_need_recorded").len(), 1);
    assert!(w.state.work_view("c-1").expect("the claim exists").communication_support);

    // A handover is said: the new owner calls the claimant.
    w.ok(Command::Assign {
        claim: "c-1".into(),
        to: "ben".into(),
        actor: actor("sam"),
    });
    let c = w.claim("c-1");
    assert!(c
        .tasks
        .iter()
        .any(|t| t.id == "handover/ben/2027-03-01" && t.kind == TaskKind::ClaimantConversation));
    let told = w.core.noticed("claim_assigned");
    let told = told.last().expect("told");
    assert_eq!(
        (told["owner"].as_str(), told["handover"].as_str(), told["communication_support"].as_str()),
        ("ben", "true", "true")
    );
    // Turned off: nothing already given is shortened.
    w.ok(marker("c-1", false, "sam"));
    assert_eq!(due(&w, "e-1"), mine + chrono::Days::new(10));

    // A review's grace: moved when the marker arrives, and longer for the next one.
    w.benefit_in_payment("ip-1");
    w.on(d(2027, 6, 1));
    w.ok(Command::DrainDue);
    w.ok(marker("ip-1", true, "ana"));
    w.on(d(2027, 6, 15));
    w.ok(Command::DrainDue);
    assert!(w.core.noticed("review_overdue").is_empty(), "the 14 days of grace became 24");
    w.on(d(2027, 6, 25));
    w.ok(Command::DrainDue);
    assert_eq!(w.core.noticed("review_overdue").len(), 1);
    w.on(d(2027, 9, 1));
    w.ok(Command::DrainDue);
    w.on(d(2027, 9, 15));
    w.ok(Command::DrainDue);
    assert_eq!(w.core.noticed("review_overdue").len(), 1, "the medical review has 24 days too");
}

/// Purpose: a trauma event overseas is registered at once, routed to the complex queue with a
/// case conference opened, cannot be admitted until the conference is held, and its evidence is
/// asked for through the evidence request that the documents adapters deliver.
/// Why it matters: persona test CLM-041. An overseas event needs foreign records and a senior's
/// eye; one left on the ordinary queue would be decided on thin evidence, and one waiting to
/// be registered until the claimant is home would miss the service timers.
#[test]
fn a_trauma_event_overseas_goes_to_the_complex_queue_with_a_case_conference() {
    let mut w = World::new();
    let mut n = World::notification("t-1", "pol-1", "trauma", ClaimType::Trauma);
    n.flags = Flags {
        overseas_event: true,
        ..Flags::default()
    };
    w.ok(Command::Notify(Box::new(n)));
    let c = w.claim("t-1");
    assert_eq!((c.status, c.queue, c.notified_on), (Status::Open, Queue::ComplexLarge, d(2027, 3, 1)));
    assert_eq!(c.task(TaskKind::CaseConference).filter(|t| t.completed_on.is_none()).count(), 1);
    w.ok(Command::Acknowledge {
        claim: "t-1".into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign("t-1", "sam");
    w.ok(Command::RequestEvidence {
        claim: "t-1".into(),
        evidence: "e-1".into(),
        kind: "hospital_record".into(),
        from: "overseas-hospital".into(),
        actor: actor("sam"),
    });
    assert_eq!(w.core.noticed("evidence_requested")[0]["evidence"], "e-1");
    w.assess("t-1", "CC-0003");
    assert_eq!(w.undecided("t-1", "sam", World::admit_lump(Some(100_000)), "R-OK"), "case_conference_required");
    w.conference("t-1", "sam");
    w.decided("t-1", "sam", World::admit_lump(Some(100_000)), "R-OK");
    assert_eq!(w.claim("t-1").status, Status::Admitted);
}

/// Purpose: a claim notified without its life takes the cover's own life, as the core reports it
/// for a policy with one life, and keeps the life the notifier named when there is one.
/// Why it matters: found by the scenario run, SCN2-08 (F2-08-3): a claim notified with "Life
/// (optional)" left blank reached reinsurance with no life, so the reinsurer's recovery on it
/// was silently wrong until someone entered the life by hand.
#[test]
fn a_claim_notified_without_its_life_takes_the_covers() {
    let mut w = World::new();
    let mut blank = World::notification("c-1", "pol-1", "trauma", ClaimType::Trauma);
    blank.life = None;
    w.ok(Command::Notify(Box::new(blank)));
    assert_eq!(w.claim("c-1").life.as_deref(), Some("life-1"));
    let mut named = World::notification("c-2", "pol-1", "life", ClaimType::Death);
    named.life = Some("life-2".into());
    w.ok(Command::Notify(Box::new(named)));
    assert_eq!(w.claim("c-2").life.as_deref(), Some("life-2"));
}
