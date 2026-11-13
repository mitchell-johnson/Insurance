//! Views, reports and the state as data.

use ic_claims::model::{HealthItem, TimerKind};
use ic_claims::state::ClaimsState;
use ic_claims::{ClaimType, Command, DecisionKind, Queue, Request, Status, TaskKind};

use crate::world::*;

/// Purpose: an adviser or employer sees that a claim exists and where it is; the claimant sees
/// their own claim, its owner, what is awaited and what is paid; neither view carries the cause
/// of claim or a reason code.
/// Why it matters: health information stays inside the claims module. These views are what the
/// portals read, so a field added here is a field shown to an employer about an employee.
#[test]
fn the_views_show_each_audience_only_what_it_may_see() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    w.ok(Command::RequestEvidence {
        claim: "c-1".into(),
        evidence: "e-1".into(),
        kind: "medical_report".into(),
        from: "treating-doctor".into(),
        actor: actor("ana"),
    });
    let status = w.state.status_view("c-1").expect("the claim exists");
    assert_eq!((status.status, status.notified_on, status.closed_on), (Status::Open, d(2027, 3, 1), None));
    let json = serde_json::to_string(&status).expect("serialises");
    assert!(!json.contains("CC-0003") && !json.contains("cause"));

    let view = w.state.claimant_view("c-1").expect("the claim exists");
    assert_eq!(
        (view.owner.as_deref(), view.claim_type, view.admitted_minor, view.paid_minor),
        (Some("ana"), ClaimType::Trauma, 0, 0)
    );
    assert_eq!(view.decision_due, Some(w.claim("c-1").timers[&TimerKind::Decision].due));
    assert_eq!(view.evidence_outstanding.len(), 1);
    assert_eq!(view.next_benefit, None);
    let json = serde_json::to_string(&view).expect("serialises");
    assert!(!json.contains("CC-0003") && !json.contains("cause") && !json.contains("R-OK"));

    w.benefit_in_payment("ip-1");
    assert_eq!(w.state.claimant_view("ip-1").expect("the claim exists").next_benefit, Some(d(2027, 3, 1)));
    assert!(w.state.status_view("nope").is_none() && w.state.claimant_view("nope").is_none());
}

/// Purpose: a queue lists its open claims most urgent first and then oldest first, and leaves
/// out closed claims.
/// Why it matters: the oldest claim must always be visible at the top of its priority, or an
/// awkward claim can sit unworked for ever behind newer ones.
#[test]
fn a_queue_shows_the_most_urgent_and_then_the_oldest_first() {
    let mut w = World::new();
    w.open_claim("c-old");
    w.advance(3);
    w.open_claim("c-new");
    let mut urgent = World::notification("c-urgent", "pol-1", "trauma", ClaimType::Trauma);
    urgent.flags.hardship = true;
    w.ok(Command::Notify(Box::new(urgent)));
    w.open_claim("c-gone");
    w.decided("c-gone", "ana", Request::Withdraw, "R-OK");
    let rows = w.state.queue(Queue::LumpSum, d(2027, 3, 10));
    assert_eq!(rows.iter().map(|r| r.claim.as_str()).collect::<Vec<_>>(), vec!["c-urgent", "c-old", "c-new"]);
    assert_eq!((rows[1].age_days, rows[1].owner.as_deref(), rows[0].owner.as_deref()), (9, Some("ana"), None));
    assert!(w.state.queue(Queue::Recoveries, d(2027, 3, 10)).is_empty());
}

/// Purpose: the book view describes claims in payment as a portfolio: count, monthly cost,
/// reserve, duration bands, cause roots, expected ends, reviews, return-to-work outcomes and
/// caseload per case manager.
/// Why it matters: benefits in payment are the insurer's longest liability. The book view is
/// how claims management sees it drifting, and cause analysis exists only here, inside the
/// module, never in what other modules read.
#[test]
fn the_book_describes_claims_in_payment_as_a_portfolio() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.benefit_in_payment("ip-2");
    w.task("ip-1", "rtw-1", TaskKind::ReturnToWork, "ana");
    w.task("ip-2", "rtw-2", TaskKind::ReturnToWork, "ana");
    w.complete("ip-2", "rtw-2", "partial_return", "ana").expect("the case manager records it");
    let book = w.state.book(d(2027, 3, 1));
    assert_eq!((book.in_payment, book.monthly_minor, book.reserve_minor), (2, 6_000, 96_000));
    assert_eq!(book.by_duration["0-6"], 2);
    assert_eq!(book.by_cause["mental"], 2);
    assert_eq!(book.expected_end_by_year[&2028], 2);
    assert_eq!(book.caseload["ana"], 2);
    assert_eq!((book.return_to_work["open"], book.return_to_work["partial_return"]), (1, 1));
    assert_eq!(book.reviews_open, 1);
    assert_eq!(w.state.book(d(2027, 10, 1)).by_duration["6-12"], 2);
    assert_eq!(w.state.book(d(2028, 6, 1)).by_duration["12-24"], 2);
    assert_eq!(w.state.book(d(2029, 6, 1)).by_duration["24+"], 2);
}

/// Purpose: the management report counts notifications, decisions by kind and by assessor,
/// straight-through and partial admissions, time to decision, timers met and missed, evidence
/// cost and workload.
/// Why it matters: these are the figures the conduct regulator asks for and the ones that show
/// an assessor or a rule version deciding differently from the rest.
#[test]
fn the_report_counts_decisions_and_service() {
    let mut w = World::new();
    let mut n = World::notification("h-1", "pol-3", "health", ClaimType::Health);
    n.health = Some(HealthItem {
        category: "dental".into(),
        invoice_minor: 600,
        provider: None,
        pre_approval_request: false,
        settles_pre_approval: None,
        service_code: None,
        service_date: None,
    });
    w.ok(Command::Notify(Box::new(n)));
    w.open_claim("c-1");
    w.ok(Command::RequestEvidence {
        claim: "c-1".into(),
        evidence: "e-1".into(),
        kind: "medical_report".into(),
        from: "treating-doctor".into(),
        actor: actor("ana"),
    });
    w.ok(Command::ReceiveEvidence {
        claim: "c-1".into(),
        evidence: "e-1".into(),
        vault_ref: "vault-9".into(),
        cost_minor: 250,
    });
    w.advance(4);
    w.assess("c-1", "CC-0003");
    w.decided("c-1", "ana", World::admit_lump(Some(100_000)), "R-OK");
    w.open_claim("c-2");
    // Left alone long enough for its first-contact timer to escalate.
    w.advance(60);
    w.ok(Command::DrainDue);

    let r = w.state.report();
    assert_eq!((r.notified[&ClaimType::Trauma], r.notified[&ClaimType::Health]), (2, 1));
    assert_eq!(r.straight_through["dental"], 1);
    assert_eq!(r.decisions[&DecisionKind::Admit], 2);
    assert_eq!(r.by_assessor["ana"][&DecisionKind::Admit], 1);
    assert_eq!(r.partial_admissions, 1);
    assert_eq!((r.decisions_timed, r.days_to_decision), (2, 4));
    assert_eq!(r.evidence_cost_minor, 250);
    assert_eq!(r.workload["ana"], 2);
    let (met, missed) = r.timers[&TimerKind::Decision];
    assert_eq!((met, missed), (1, 1));
    assert!(r.timers[&TimerKind::FirstContact].1 >= 1);
}

/// Purpose: the module's totals by policy are the open reserves, the admitted and unpaid
/// amounts and the overpayments outstanding, and nothing for a policy with none.
/// Why it matters: the core compares these with the ledger every day. They are the proof that
/// the claim records and the money agree.
#[test]
fn totals_by_policy_are_what_the_ledger_must_agree_with() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.open_claim("c-2");
    w.assess("c-2", "CC-0003");
    w.decided("c-2", "ana", World::admit_lump(Some(100_000)), "R-OK");
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.ok(Command::DeclareIncome {
        claim: "ip-1".into(),
        from: d(2027, 3, 1),
        other_income_minor: 700,
        actor: actor("ana"),
    });
    w.open_claim("c-3");
    w.decided("c-3", "ana", Request::Withdraw, "R-OK");
    let totals = w.state.totals_by_policy();
    assert_eq!(
        (totals["pol-1"].reserve_minor, totals["pol-1"].payable_minor, totals["pol-1"].receivable_minor),
        (500_000, 100_000, 0)
    );
    assert_eq!(
        (totals["pol-2"].reserve_minor, totals["pol-2"].payable_minor, totals["pol-2"].receivable_minor),
        (45_000, 0, 700)
    );
    assert!(!totals.contains_key("pol-3"));
}

/// Purpose: the whole claims state, and a command, survive being written as JSON and read back.
/// Why it matters: the state is snapshotted and shipped between replicas and regions, and
/// commands are the replicated log. A field that does not round-trip is state lost at the next
/// restore.
#[test]
fn state_and_commands_survive_a_round_trip() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.decided("ip-1", "ana", Request::Reserve { amount_minor: 800_000 }, "R-OK");
    let json = serde_json::to_string(&w.state).expect("serialises");
    let back: ClaimsState = serde_json::from_str(&json).expect("reads back");
    assert_eq!(back, w.state);

    let command = World::decide_command("c-1", "ana", World::admit_lump(None), "R-OK");
    let json = serde_json::to_string(&command).expect("serialises");
    assert!(json.contains("\"decide\"") && json.contains("\"admit\""));
    assert_eq!(serde_json::from_str::<Command>(&json).expect("reads back"), command);
    // Optional fields may be left out by a caller.
    let short = r#"{"decide":{"claim":"c-1","request":{"admit":{}},"actor":{"staff":"ana","purpose":"assessing"}}}"#;
    assert!(serde_json::from_str::<Command>(short).is_ok());
}

/// Purpose: the code a claim type or a status is published under is the same word its JSON
/// form uses, for every type and status, apart from the two closing outcomes that are named
/// for what happened (`benefit_ended`, `admission_reversed`).
/// Why it matters: other modules match on these strings in events while the API carries the
/// JSON form. If the two drifted apart, a reader would silently stop recognising a claim type.
#[test]
fn published_codes_match_the_json_form() {
    for t in [
        ClaimType::Death,
        ClaimType::TerminalIllness,
        ClaimType::Trauma,
        ClaimType::TotalPermanentDisability,
        ClaimType::IncomeProtection,
        ClaimType::Health,
        ClaimType::PremiumWaiver,
    ] {
        assert_eq!(serde_json::to_value(t).expect("a claim type is data"), t.code());
    }
    for s in [
        Status::Open,
        Status::Admitted,
        Status::InPayment,
        Status::Paid,
        Status::Declined,
        Status::Withdrawn,
    ] {
        assert_eq!(serde_json::to_value(s).expect("a status is data"), s.code());
    }
    assert_eq!((Status::Ended.code(), Status::Reversed.code()), ("benefit_ended", "admission_reversed"));
}

/// Purpose: the staff working view of a claim shows who has it, the tasks, the evidence, the
/// decision waiting for signatures, the payee, the interested parties, the payments and the
/// benefit, and is absent for a claim that does not exist.
/// Why it matters: the claims screen is built from this view. If it left out the waiting
/// decision or the unpaid lender, staff would work from memory; if it carried the cause or a
/// vault reference, the clinical file would be readable without the logged step of opening it.
#[test]
fn the_working_view_shows_the_work_and_not_the_clinical_file() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    w.pay_run("run-1", d(2027, 3, 1));
    w.ok(Command::RequestEvidence {
        claim: "ip-1".into(),
        evidence: "e-1".into(),
        kind: "medical_report".into(),
        from: "treating-doctor".into(),
        actor: actor("ana"),
    });
    let view = w.state.work_view("ip-1").expect("the claim exists");
    assert_eq!(
        (view.owner.as_deref(), view.status, view.policy.as_str()),
        (Some("ana"), Status::InPayment, "pol-2")
    );
    assert_eq!(view.payee.as_ref().map(|p| p.0.as_str()), Some("party-claimant"));
    assert_eq!(
        (view.payments.len(), view.paid_minor),
        (0, 3_000),
        "a benefit period is on the schedule, not a lump payment"
    );
    let benefit = view.benefit.as_ref().expect("in payment");
    assert_eq!(
        (benefit.monthly_minor, benefit.next_period, benefit.held_for_proof_of_life),
        (3_000, d(2027, 4, 1), false)
    );
    assert_eq!(view.evidence[0].0, "e-1");
    assert!(view.waiting.is_none() && view.interests.is_empty());
    let json = serde_json::to_string(&view).expect("it serialises");
    assert!(!json.contains("CC-0002") && !json.contains("R-OK") && !json.contains("vault"), "{json}");

    // A decision above the proposer's authority waits, and the view says for whom.
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    let _ = w.decide("c-1", "ana", World::admit_lump(Some(500_000)), "R-OK");
    let waiting = w
        .state
        .work_view("c-1")
        .expect("the claim exists")
        .waiting
        .expect("a second signature is needed");
    assert_eq!((waiting.proposer.as_str(), waiting.still_needed, waiting.signed.len()), ("ana", 1, 0));
    assert!(w.state.work_view("nope").is_none());
    assert!(matches!(
        w.state.answer(&ic_claims::report::Question::Work { claim: "ip-1".into() }, d(2027, 3, 1)),
        ic_claims::report::Reply::Work(Some(_))
    ));
}

/// Purpose: the policy status question lists every claim on one policy, oldest first, with
/// only its ID, status and dates, and nothing for a policy with no claims.
/// Why it matters: this is what an adviser or an employer is shown (Mitchell's decision of
/// 13 November 2026: status, never cause). A type, reason or amount here would tell an employer
/// about an employee's health.
#[test]
fn the_policy_status_shows_an_adviser_each_claim_without_its_cause() {
    use ic_claims::report::{Question, Reply};
    let mut w = World::new();
    w.open_claim("c-2");
    w.assess("c-2", "CC-0003");
    w.advance(2);
    w.open_claim("c-1");
    w.benefit_in_payment("ip-1");
    let Reply::PolicyStatus(rows) = w.state.answer(&Question::PolicyStatus { policy: "pol-1".into() }, d(2027, 3, 5)) else {
        panic!("a policy status question gets a policy status reply");
    };
    let ids: Vec<&str> = rows.iter().map(|r| r.claim.as_str()).collect();
    assert_eq!(ids, ["c-2", "c-1"], "oldest first, and only this policy's claims");
    let json = serde_json::to_string(&rows).expect("serialises");
    for hidden in ["CC-0003", "cause", "trauma", "Trauma", "minor", "party"] {
        assert!(!json.contains(hidden), "{hidden} leaked: {json}");
    }
    assert_eq!(w.state.policy_status("pol-none"), vec![]);
}

/// Purpose: on income protection a company owns, the benefit is paid to the company as
/// policyholder, while the claim is about the life insured: the view other modules use to decide
/// who may see it names the life, even when the company is recorded as the claimant. A death
/// claim is still about its claimant.
/// Why it matters: persona test CLM-027. An employer that owns key person cover is entitled to
/// the money but not to its director's medical file; naming the company as the person the claim
/// is about would show it the claim's detail.
#[test]
fn a_company_owned_income_claim_pays_the_company_and_is_about_the_life() {
    use ic_claims::report::{Question, Reply};
    let mut w = World::new();
    let mut owned = World::notification("ip-co", "pol-2", "ip", ClaimType::IncomeProtection);
    owned.claimant = Some("party-holder".into());
    owned.notifier = ic_claims::model::Notifier::Employer;
    w.ok(Command::Notify(Box::new(owned)));
    w.ok(Command::Acknowledge {
        claim: "ip-co".into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign("ip-co", "ana");
    w.assess("ip-co", "CC-0002");
    let admit = Request::Admit {
        amount_minor: None,
        periodic: Some(ic_claims::command::Periodic {
            monthly_minor: 3_000,
            first_period: d(2027, 3, 1),
            end: d(2029, 3, 1),
            expected_end: d(2028, 3, 1),
        }),
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    w.decide("ip-co", "ana", admit, "R-OK").expect("within ana's authority");
    w.ok(Command::VerifyPayee {
        claim: "ip-co".into(),
        party: "party-holder".into(),
        kind: ic_claims::model::PayeeKind::Policyholder,
        account_ref: "acct-co".into(),
        name_check_ref: "name-co".into(),
        screening_ref: "screen-co".into(),
        authority_doc: None,
        actor: actor("ana"),
    });
    w.pay_run("run-1", d(2027, 3, 1));
    assert!(matches!(
        w.core.instructions.last(),
        Some(ic_claims::Instruction::PayBenefit { payee, .. }) if payee == "party-holder"
    ));
    let Reply::Claim(Some(row)) = w.state.answer(&Question::Claim { claim: "ip-co".into() }, d(2027, 3, 1)) else {
        panic!("the claim is there");
    };
    assert_eq!((row.party.as_str(), row.policy.as_str()), ("party-life", "pol-2"));

    let mut death = World::notification("c-death", "pol-1", "life", ClaimType::Death);
    death.claimant = Some("party-estate".into());
    w.ok(Command::Notify(Box::new(death)));
    assert_eq!(w.claim("c-death").subject_party(), "party-estate");
}

/// Purpose: a claim on a policy issued under a leaver's continuation option is assessed on that
/// policy, and carries the scheme policy and member it continues: in the reserve event that
/// reinsurance reads, and in the assessor's view with the member's claims on the scheme.
/// Why it matters: persona test CLM-026. The continuation cover was underwritten as the scheme's,
/// so its claims history and its reinsurance belong with the scheme; assessed as a new policy, an
/// earlier scheme claim for the same condition is invisible and the claim is ceded to no treaty.
#[test]
fn a_continuation_policy_claim_carries_the_scheme_history() {
    use ic_claims::port::{Continuation, CoverFacts};
    let mut w = World::new();
    // An earlier claim on the scheme, for the member who has since left.
    w.ok(Command::Notify(Box::new(World::notification("c-scheme", "pol-1", "trauma", ClaimType::Trauma))));
    let from = Continuation {
        policy: "pol-1".into(),
        life: "life-1".into(),
    };
    w.core.cover(
        "pol-2",
        "trauma",
        CoverFacts {
            continuation_of: Some(from.clone()),
            ..facts("trauma")
        },
    );
    w.ok(Command::Notify(Box::new(World::notification("c-cont", "pol-2", "trauma", ClaimType::Trauma))));
    let c = w.claim("c-cont");
    assert_eq!((c.policy.as_str(), c.continued_from.as_ref()), ("pol-2", Some(&from)));
    let reserved = w.core.noticed("claim_reserved");
    let last = reserved.last().expect("reserved");
    assert_eq!(
        (last["continued_from_policy"].as_str(), last["continued_from_life"].as_str()),
        ("pol-1", "life-1")
    );
    assert_eq!(reserved[0]["continued_from_policy"], "", "an ordinary claim names none");
    let view = w.state.work_view("c-cont").expect("a view");
    assert_eq!(view.continued_from, Some(from));
    assert_eq!(
        view.scheme_claims,
        vec![("c-scheme".to_string(), ClaimType::Trauma, Status::Open, d(2027, 2, 20))]
    );
    assert!(w.state.work_view("c-scheme").expect("a view").scheme_claims.is_empty());
}
