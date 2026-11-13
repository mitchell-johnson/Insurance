//! Standing controls: the exclusion-match control, peer review by sample,
//! reinsurer consent, and who may send what.

use ic_claims::command::ReinsuranceNotice;
use ic_claims::model::{HealthItem, OverpaymentCause, RunStatus};
use ic_claims::state::ClaimsState;
use ic_claims::{ClaimType, Command, CoverFacts, DecisionKind, Queue, RunKind, Status, TaskKind};

use crate::world::*;

fn control(after: Option<String>) -> Command {
    Command::ExclusionControl { after }
}

/// Purpose: the standing control finds a claim in payment whose recorded cause matches an
/// exclusion on the paying cover as the policy record has it now, sends it to senior review
/// once, and publishes what the pass looked at (design addendum, entry 5).
/// Why it matters: the addendum's reversal case was found by a person by accident. The control
/// is what finds the next one in days: an exclusion keyed late, or missed at admission, is
/// matched against every claim being paid on every pass.
#[test]
fn the_exclusion_control_finds_a_claim_paid_against_an_exclusion() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.benefit_in_payment("ip-2");
    w.ok(Command::RecordCause {
        claim: "ip-2".into(),
        cause_code: "CC-0001".into(),
        actor: actor("ana"),
    });
    w.ok(control(None));
    let run = w.core.noticed("exclusion_control_run")[0].clone();
    assert_eq!((run["scanned"].as_str(), run["hits"].as_str(), run["complete"].as_str()), ("2", "0", "true"));

    // The exclusion reaches the policy record after the admission.
    w.core.cover(
        "pol-2",
        "ip",
        CoverFacts {
            sum_insured_minor: 4_000,
            exclusions: vec!["EX-0001".into()],
            ..facts("income_protection")
        },
    );
    w.on(d(2027, 3, 2));
    w.ok(control(None));
    let run = w.core.noticed("exclusion_control_run")[1].clone();
    assert_eq!((run["scanned"].as_str(), run["hits"].as_str()), ("2", "1"));
    let c = w.claim("ip-2");
    assert_eq!(c.queue, Queue::ComplexLarge);
    assert!(c
        .tasks
        .iter()
        .any(|t| t.id == "exclusion-match/CC-0001/1" && t.kind == TaskKind::ExclusionMatch));
    assert_eq!(c.status, Status::InPayment);
    assert_eq!(w.claim("ip-1").queue, Queue::IncomeProtectionOngoing);
    assert_eq!(w.core.noticed("exclusion_match").len(), 1);
    assert_eq!(w.state.control.last_run, Some(d(2027, 3, 2)));
    // The notice names the claim and nothing clinical.
    assert!(w
        .core
        .notices
        .iter()
        .filter(|(k, _, _)| k == "exclusion_match")
        .all(|(_, claim, detail)| claim == "ip-2" && detail.is_empty()));

    // A hit already in front of a senior is not raised again by the next pass.
    w.ok(control(None));
    assert_eq!(w.core.noticed("exclusion_control_run")[2]["hits"], "0");
    assert_eq!(w.core.noticed("exclusion_match").len(), 1);
    assert_eq!(
        w.complete("ip-2", "exclusion-match/CC-0001/1", "reviewed", "ana")
            .expect_err("a senior's task")
            .code,
        "not_authorised"
    );
    w.complete("ip-2", "exclusion-match/CC-0001/1", "reviewed", "sue")
        .expect("a senior reviews the hit");

    // If the policy has left the partition the control falls back on the recorded cover check.
    w.core.covers.remove("pol-2/ip");
    w.ok(control(None));
    assert_eq!(w.core.noticed("exclusion_control_run")[3]["hits"], "0");

    let mut bare = World::new();
    bare.state = ClaimsState::default();
    assert_eq!(bare.refused(control(None)), "not_configured");
}

/// Purpose: a proposed rules version can be run against every admitted claim before it is
/// activated, listing the claims it would match to an exclusion.
/// Why it matters: the addendum asks that a rules change mapping a cause to an exclusion shows
/// which claims in payment it catches before it goes live, so the reviews are planned and
/// nobody learns of them from the first control pass.
#[test]
fn proposed_rules_can_be_dry_run_against_admitted_claims() {
    let mut w = World::new();
    w.core.cover(
        "pol-2",
        "ip",
        CoverFacts {
            sum_insured_minor: 4_000,
            exclusions: vec!["EX-0001".into()],
            ..facts("income_protection")
        },
    );
    w.benefit_in_payment("ip-1");
    w.open_claim("c-open");
    assert!(w.state.exclusion_hits(&w.state.rules).is_empty());
    let mut proposed = rules();
    proposed.version = 2;
    proposed.exclusions.get_mut("EX-0001").expect("in the rules").cause_tags.push("mental".into());
    assert_eq!(w.state.exclusion_hits(&proposed), vec!["ip-1".to_string()]);
}

/// Purpose: the control and a benefit run both work through a large book in bounded pieces: 200
/// claims per control command with a cursor to carry on from, and 250 payments per run.
/// Why it matters: every command runs inside the partition's apply budget. A pass or a run
/// that grew with the book would one day be too large to apply, and the book would stop
/// being paid.
#[test]
fn a_large_book_is_worked_in_bounded_pieces() {
    let mut w = World::new();
    for n in 0..251 {
        w.benefit_in_payment(&format!("ip-{n:03}"));
    }
    w.ok(control(None));
    assert_eq!(w.state.control.cursor.as_deref(), Some("ip-199"));
    assert_eq!(w.core.noticed("exclusion_control_run")[0]["complete"], "false");
    let cursor = w.state.control.cursor.clone();
    w.ok(control(cursor));
    assert_eq!((w.state.control.scanned, w.state.control.cursor.clone()), (251, None));
    assert_eq!(w.core.noticed("exclusion_control_run")[1]["complete"], "true");

    w.pay_run("run-1", d(2027, 3, 1));
    assert_eq!((w.state.runs["run-1"].count, w.state.runs["run-1"].status), (250, RunStatus::Submitted));
    w.pay_run("run-2", d(2027, 3, 1));
    assert_eq!(w.state.runs["run-2"].count, 1);
    assert_eq!(w.state.book(d(2027, 3, 1)).in_payment, 251);
}

/// Purpose: decisions are sampled for peer review at the rate the rules set, by a draw every
/// replica makes the same way, and the review is done by a quality reviewer who has not acted
/// on the claim.
/// Why it matters: peer review is how wrong decisions that nobody complained about are found.
/// A sample that differed between replicas would fork the state machine, and a review by
/// someone who worked the claim would be marking their own homework.
#[test]
fn decisions_are_sampled_for_independent_peer_review() {
    let sampled = |pct: u32| {
        let mut w = World::new();
        let mut r = rules();
        r.version = 2;
        r.sample_pct.insert(DecisionKind::Admit, pct);
        w.ok(Command::ActivateRules { rules: Box::new(r) });
        w.open_claim("c-1");
        w.assess("c-1", "CC-0003");
        w.decided("c-1", "ana", World::admit_lump(Some(100_000)), "R-OK");
        w
    };
    assert!(!sampled(0).claim("c-1").has_open_task(TaskKind::QualityReview));
    let mut w = sampled(100);
    let task = w.claim("c-1").task(TaskKind::QualityReview).next().expect("sampled").id.clone();
    assert_eq!(
        w.complete("c-1", &task, "sound", "ana").expect_err("ana made the decision").code,
        "not_authorised"
    );
    assert_eq!(w.complete("c-1", &task, "sound", "sam").expect_err("not a reviewer").code, "not_authorised");
    w.complete("c-1", &task, "sound", "quin").expect("an independent reviewer");
    assert!(!w.claim("c-1").touched_by.contains("quin"));
    // The same history gives the same sample.
    assert_eq!(sampled(37).state, sampled(37).state);
}

/// Purpose: when the reinsurance module says a reinsurer must consent, admission waits on a
/// dated task; consent for the named treaty lifts the hold; a refusal sends the claim to a
/// senior, whose decision it remains.
/// Why it matters: admitting a claim without a consent the treaty requires can lose the
/// recovery. But the reinsurer does not decide the claim: a refusal must not leave the
/// customer's claim stuck with no one able to act.
#[test]
fn a_reinsurers_consent_holds_admission_and_a_refusal_goes_to_a_senior() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    let required = |treaty: &str| {
        Command::Reinsurance(ReinsuranceNotice::ReinsurerConsentRequired {
            claim: "c-1".into(),
            treaty: treaty.into(),
            due_by: d(2027, 3, 20),
        })
    };
    let consent = |agreed: bool, treaty: Option<&str>| {
        Command::Reinsurance(ReinsuranceNotice::ReinsurerConsent {
            claim: "c-1".into(),
            reinsurer: "re-1".into(),
            agreed,
            reference: "ref-1".into(),
            treaty: treaty.map(str::to_string),
        })
    };
    w.ok(required("treaty-1"));
    w.ok(required("treaty-2"));
    let c = w.claim("c-1");
    assert!(c.tasks.iter().any(|t| t.id == "reinsurer-consent/treaty-1" && t.due == Some(d(2027, 3, 20))));
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(1_000)), "R-OK"), "awaiting_reinsurer_consent");
    assert_eq!(
        w.complete("c-1", "reinsurer-consent/treaty-1", "agreed", "ana")
            .expect_err("not the assessor's")
            .code,
        "system_task"
    );

    w.ok(consent(true, Some("treaty-1")));
    let c = w.claim("c-1");
    assert_eq!(c.reinsurance.consents, vec![("re-1".to_string(), "ref-1".to_string())]);
    assert_eq!(c.reinsurance.consent_required.iter().collect::<Vec<_>>(), vec!["treaty-2"]);
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(1_000)), "R-OK"), "awaiting_reinsurer_consent");

    w.ok(consent(false, None));
    let c = w.claim("c-1");
    assert!(c.reinsurance.consent_refused && c.reinsurance.consent_required.is_empty());
    assert_eq!((c.queue, c.owner.clone()), (Queue::ComplexLarge, None));
    assert!(c.tasks.iter().any(|t| t.id == "senior-review/reinsurer-refused"));
    assert_eq!(
        c.task(TaskKind::ReinsurerConsent).filter(|t| t.outcome.as_deref() == Some("refused")).count(),
        1
    );

    // The senior holds the conference and decides.
    w.assign("c-1", "sam");
    w.ok(Command::Escalate {
        claim: "c-1".into(),
        actor: actor("sam"),
    });
    w.conference("c-1", "sam");
    w.decided("c-1", "sam", World::admit_lump(Some(1_000)), "R-OK");
    assert_eq!(w.claim("c-1").status, Status::Admitted);

    // On a claim already decided, a refusal changes nothing about who holds it.
    w.ok(required("treaty-3"));
    w.ok(consent(false, None));
    assert_eq!(w.claim("c-1").owner.as_deref(), Some("sam"));
}

/// Purpose: an overpayment found outside a replay, such as a provider billing twice, is raised
/// by the claim's owner or the recoveries team, for no more than was paid, under a new ID.
/// Why it matters: a receivable larger than what was paid, or raised twice, would overstate the
/// insurer's assets and chase a provider for money they never had.
#[test]
fn an_overpayment_can_be_raised_by_hand_within_what_was_paid() {
    let mut w = World::new();
    let mut n = World::notification("h-1", "pol-3", "health", ClaimType::Health);
    n.health = Some(HealthItem {
        category: "dental".into(),
        invoice_minor: 600,
        provider: Some("provider-9".into()),
        pre_approval_request: false,
        settles_pre_approval: None,
        service_code: None,
        service_date: None,
    });
    w.ok(Command::Notify(Box::new(n)));
    let raise = |id: &str, amount: i64, who: &str| Command::RaiseOverpayment {
        claim: "h-1".into(),
        overpayment: id.into(),
        amount_minor: amount,
        cause: OverpaymentCause::ProviderBilling,
        good_faith: false,
        actor: actor(who),
    };
    assert_eq!(w.refused(raise("op-1", 100, "rec")), "invalid_overpayment");

    // The provider run pays the invoice.
    w.ok(Command::PrepareRun {
        run: "prov-1".into(),
        kind: RunKind::Provider,
        benefit_date: d(2027, 3, 1),
        actor: actor("ana"),
    });
    assert!(w.state.runs["prov-1"].items.iter().all(|i| i.payee == "provider-9" && i.gross_minor == 440));
    w.ok(Command::ApproveRun {
        run: "prov-1".into(),
        actor: actor("sam"),
    });
    assert_eq!(w.claim("h-1").status, Status::Paid);
    assert_eq!(w.state.remittance("prov-1")["provider-9"], (1, 440));
    assert!(!w.state.provider_payable.contains("h-1"));

    assert_eq!(w.refused(raise("op-1", 100, "ben")), "not_owner");
    assert_eq!(w.refused(raise("op-1", 0, "rec")), "invalid_overpayment");
    assert_eq!(w.refused(raise("op-1", 441, "rec")), "invalid_overpayment");
    w.core.refuse_next = 1;
    assert_eq!(w.refused(raise("op-1", 440, "rec")), "ledger");
    w.ok(raise("op-1", 300, "rec"));
    assert_eq!(w.refused(raise("op-1", 100, "rec")), "invalid_overpayment");
    assert_eq!(w.refused(raise("op-2", 141, "rec")), "invalid_overpayment");
    let c = w.claim("h-1");
    assert_eq!(
        (c.overpayments[0].id.as_str(), c.overpayments[0].amount_minor, c.overpayments[0].protected()),
        ("op-1", 300, false)
    );
    assert_eq!(w.core.noticed("overpayment_raised")[0]["cause"], "ProviderBilling");
}

/// Purpose: each command names itself and the modules allowed to send it.
/// Why it matters: the core checks the caller of every command against this list. It is what
/// stops, say, the payments module deciding a claim or anyone but the configuration service
/// activating authority profiles.
#[test]
fn each_command_names_who_may_send_it() {
    let a = actor("ana");
    let cases: Vec<(Command, &str, &[&str])> = vec![
        (Command::ActivateRules { rules: Box::new(rules()) }, "activate_rules", &["config"]),
        (Command::ActivateAuthority { book: Box::new(authority()) }, "activate_authority", &["config"]),
        (
            Command::PaymentReturned {
                claim: "c".into(),
                payment_ref: "p".into(),
            },
            "payment_returned",
            &["payments", "claims"],
        ),
        (
            Command::ConfirmRun {
                run: "r".into(),
                settled_minor: 0,
            },
            "confirm_run",
            &["payments", "claims"],
        ),
        (
            Command::Approve {
                claim: "c".into(),
                actor: a.clone(),
            },
            "approve",
            &["claims", "held-balances"],
        ),
        (
            Command::Refuse {
                claim: "c".into(),
                actor: a.clone(),
            },
            "refuse",
            &["claims", "held-balances"],
        ),
        (
            Command::Reinsurance(ReinsuranceNotice::LargeClaimNotification {
                claim: "c".into(),
                treaty: "t".into(),
            }),
            "reinsurance",
            &["reinsurance"],
        ),
        (
            Command::NonDisclosureOutcome {
                claim: "c".into(),
                review_ref: "r".into(),
                outcome: ic_claims::command::NonDisclosureOutcome::Vary,
            },
            "non_disclosure_outcome",
            &["underwriting"],
        ),
        (Command::ExclusionControl { after: None }, "exclusion_control", &["claims", "end-of-day"]),
        (Command::DrainDue, "drain_due", &["claims", "end-of-day"]),
        (
            Command::RecordDeath {
                party: "p".into(),
                date_of_death: d(2027, 1, 1),
            },
            "record_death",
            &["claims", "customer-master"],
        ),
        (
            Command::OpenFile {
                claim: "c".into(),
                actor: a.clone(),
            },
            "open_file",
            &["claims"],
        ),
        (
            Command::RecordContact {
                claim: "c".into(),
                actor: a.clone(),
            },
            "record_contact",
            &["claims"],
        ),
        (
            Command::IdentifyLife {
                claim: "c".into(),
                party: "p".into(),
                actor: a.clone(),
            },
            "identify_life",
            &["claims"],
        ),
        (
            Command::ReceiveEvidence {
                claim: "c".into(),
                evidence: "e".into(),
                vault_ref: "v".into(),
                cost_minor: 0,
            },
            "receive_evidence",
            &["claims"],
        ),
        (
            Command::RequestEvidence {
                claim: "c".into(),
                evidence: "e".into(),
                kind: "k".into(),
                from: "f".into(),
                actor: a.clone(),
            },
            "request_evidence",
            &["claims"],
        ),
        (
            Command::CancelRun {
                run: "r".into(),
                actor: a.clone(),
            },
            "cancel_run",
            &["claims"],
        ),
        (
            Command::RecordRepayment {
                claim: "c".into(),
                overpayment: "o".into(),
                amount_minor: 1,
            },
            "record_repayment",
            &["payments", "claims"],
        ),
        (
            Command::ConfirmAlive {
                party: "p".into(),
                source: "s".into(),
                evidence_ref: "v".into(),
            },
            "confirm_alive",
            &["channels", "customer-master"],
        ),
        (
            Command::RecordRepaymentLetter {
                claim: "c".into(),
                overpayment: "o".into(),
                request: "r".into(),
            },
            "record_repayment_letter",
            &["documents"],
        ),
        (
            Command::ReferForInvestigation {
                case: "k".into(),
                rule: "r".into(),
                claims: vec![],
            },
            "refer_for_investigation",
            &["compliance"],
        ),
        (
            Command::OffsetReturned {
                from: 1,
                take_ref: "t".into(),
                claim: "c".into(),
                amount_minor: 1,
            },
            "offset_returned",
            &["partition"],
        ),
    ];
    for (command, name, callers) in cases {
        assert_eq!(command.name(), name);
        assert_eq!(command.allowed_callers(), callers, "{name}");
    }
}
