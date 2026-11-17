//! Decisions: authority, second signatures, reasons and what each decision does.

use ic_claims::command::{NonDisclosureOutcome, ReinsuranceNotice, ReopenBasis};
use ic_claims::config::Match;
use ic_claims::model::{EndReason, HealthItem, OverpaymentCause, TimerKind};
use ic_claims::{ClaimType, Command, CoverFacts, DecisionKind, Instruction, Queue, Request, Status, TaskKind};

use crate::world::*;

/// Purpose: an admission inside the assessor's own authority takes effect at once, posts through
/// the core, and leaves a decision record that can reproduce it.
/// Why it matters: the decision record (who, which profile, which rule and authority versions,
/// the cause, the definitions and facts relied on) is what the insurer shows a regulator or a
/// dispute scheme years later. An admission without it cannot be defended or reviewed.
#[test]
fn an_admission_within_authority_takes_effect_and_is_recorded() {
    let mut w = World::new();
    w.open_claim("c-1");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK"), "cover_not_checked");
    w.ok(Command::CoverCheck {
        claim: "c-1".into(),
        relied_on: vec!["in_force".into()],
        actor: actor("ana"),
    });
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK"), "cause_required");
    w.assess("c-1", "CC-0003");
    assert_eq!(w.undecided("c-1", "ben", World::admit_lump(Some(200_000)), "R-OK"), "not_owner");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(200_000)), "R-NO"), "unknown_reason");
    let no_reason = Command::Decide {
        claim: "c-1".into(),
        request: World::admit_lump(Some(200_000)),
        reasons: vec![],
        actor: actor("ana"),
    };
    assert_eq!(w.refused(no_reason), "reason_required");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(500_001)), "R-OK"), "amount_exceeds_cover");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(0)), "R-OK"), "amount_exceeds_cover");

    w.decided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK");
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.admitted_minor, c.payable_minor, c.reserve_minor),
        (Status::Admitted, 200_000, 200_000, 0)
    );
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Admit {
            amount_minor: 200_000,
            release_reserve: true,
            deduct_premium_minor: 0,
            terminate: None,
            ..
        })
    ));
    assert_eq!(w.core.noticed("claim_admitted")[0]["admitted_minor"], "200000");
    // The core takes the lump sum off this life's cover and the covers linked to it.
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Admit { life: Some(l), .. }) if l == "life-1"));
    let decision = c.decisions.last().expect("the admission is recorded");
    assert_eq!(decision.kind, DecisionKind::Admit);
    assert_eq!(decision.signers, vec!["ana".to_string()]);
    assert_eq!(
        (decision.rules_version, decision.authority_version, decision.profile.as_str()),
        (1, 1, "assessor")
    );
    assert_eq!(decision.cause_code.as_deref(), Some("CC-0003"));
    assert_eq!(decision.definitions, vec!["trauma-def@3".to_string()]);
    assert_eq!(decision.relied_on, vec!["in_force".to_string(), "sum_insured".to_string()]);
    assert!(!decision.straight_through);
    // The decision timer is met and the payment timer runs from admission.
    assert_eq!(c.timers[&TimerKind::Decision].met_on, Some(d(2027, 3, 1)));
    assert!(c.timers.contains_key(&TimerKind::Payment));
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(1)), "R-OK"), "not_open");
}

/// Purpose: an admission at or above the two-signature amount waits for a second person whose
/// own authority covers it, who is not the proposer and has no conflict of interest.
/// Why it matters: four eyes on large payments is the main control against both error and
/// internal fraud. If the proposer, an unqualified colleague or the life's underwriter could
/// give the second signature, the control would exist only on paper.
#[test]
fn a_large_admission_waits_for_a_second_signature() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    let instructions = w.core.instructions.len();
    w.decided("c-1", "ana", World::admit_lump(None), "R-OK");
    assert_eq!(w.claim("c-1").status, Status::Open);
    assert_eq!(w.core.instructions.len(), instructions);
    assert_eq!(w.core.noticed("decision_pending")[0]["kind"], "Admit");

    assert_eq!(w.undecided("c-1", "ana", Request::Withdraw, "R-OK"), "decision_pending");
    assert_eq!(w.unapproved("c-1", "ana"), "second_signer");
    assert_eq!(w.unapproved("c-1", "jon"), "authority");
    assert_eq!(w.unapproved("c-1", "rel"), "conflict_of_interest");
    assert_eq!(w.unapproved("c-1", "una"), "conflict_of_interest");
    assert_eq!(w.unapproved("c-1", "nobody"), "unknown_staff");
    w.approved("c-1", "ben");
    let c = w.claim("c-1");
    assert_eq!((c.status, c.admitted_minor), (Status::Admitted, 500_000));
    assert_eq!(c.decisions.last().expect("recorded").signers, vec!["ana".to_string(), "ben".to_string()]);
    assert_eq!(w.unapproved("c-1", "sam"), "nothing_pending");
}

/// Purpose: a decision beyond the proposer's own limit is held for someone whose profile covers
/// all of it, and a reserve that moves sharply goes to senior review.
/// Why it matters: authority limits are how the insurer caps the damage one person can do, and
/// the reserve drives the financial statements, so a large unexplained move must be looked at.
#[test]
fn a_reserve_beyond_authority_is_borrowed_and_a_sharp_move_is_reviewed() {
    let mut w = World::new();
    w.open_claim("c-1");
    assert_eq!(w.undecided("c-1", "ana", Request::Reserve { amount_minor: -1 }, "R-OK"), "invalid_reserve");
    w.decided("c-1", "ana", Request::Reserve { amount_minor: 800_000 }, "R-OK");
    assert_eq!(w.claim("c-1").reserve_minor, 500_000);
    assert_eq!(w.unapproved("c-1", "ben"), "authority");
    w.approved("c-1", "sam");
    let c = w.claim("c-1");
    assert_eq!(c.reserve_minor, 800_000);
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::SetReserve { amount_minor: 800_000, .. })
    ));
    assert_eq!(
        w.core.noticed("claim_reserve_changed").last().expect("published")["gross_reserve_minor"],
        "800000"
    );
    // 500,000 to 800,000 is a 60% move: a senior looks at it.
    assert!(c.has_open_task(TaskKind::SeniorReview));
    let reviews = c.task(TaskKind::SeniorReview).count();

    // A small move inside the assessor's limit is theirs alone and raises nothing.
    w.decided("c-1", "ana", Request::Reserve { amount_minor: 700_000 }, "R-OK");
    assert_eq!(w.claim("c-1").reserve_minor, 700_000);
    assert_eq!(w.claim("c-1").task(TaskKind::SeniorReview).count(), reviews);

    // The ledger refusing the posting leaves the claim's reserve where it was.
    w.core.refuse_next = 1;
    assert_eq!(w.undecided("c-1", "ana", Request::Reserve { amount_minor: 650_000 }, "R-OK"), "ledger");
}

/// Purpose: a decline needs a recorded conversation with the claimant, a decline reason code, a
/// proposer who may decline and always a second signature; a withdrawal needs none of that.
/// Why it matters: a decline is the decision most likely to be wrong against the customer and
/// to end up at a dispute scheme. The conversation and the second signature are the conduct
/// controls the design notes require before any decline.
#[test]
fn a_decline_needs_a_conversation_and_two_signatures() {
    let mut w = World::new();
    w.open_claim("c-1");
    assert_eq!(w.undecided("c-1", "ana", Request::Decline, "R-NO"), "conversation_required");
    w.conversation("c-1", "talk-1");
    assert_eq!(w.undecided("c-1", "ana", Request::Decline, "R-OK"), "unknown_reason");
    w.decided("c-1", "ana", Request::Decline, "R-NO");
    assert_eq!(w.claim("c-1").status, Status::Open);
    w.approved("c-1", "ben");
    let c = w.claim("c-1");
    assert_eq!((c.status, c.reserve_minor, c.closed_on), (Status::Declined, 0, Some(d(2027, 3, 1))));
    assert!(c.due.is_empty());
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Close { outcome, overpayment: None, .. }) if outcome == "declined"));
    assert_eq!(w.core.noticed("claim_closed")[0]["outcome"], "declined");
    // The reason code is clinical and stays inside the module.
    assert!(w.core.notices.iter().all(|(_, _, detail)| detail.values().all(|v| v != "R-NO")));

    w.open_claim("c-2");
    w.decided("c-2", "ana", Request::Withdraw, "R-OK");
    assert_eq!(w.claim("c-2").status, Status::Withdrawn);
    assert_eq!(w.undecided("c-2", "ana", Request::Decline, "R-NO"), "not_open");
    assert_eq!(w.undecided("c-2", "ana", Request::Withdraw, "R-OK"), "not_open");
    assert_eq!(w.undecided("c-2", "ana", Request::Reserve { amount_minor: 1 }, "R-OK"), "invalid_reserve");
}

/// Purpose: a health claim can be declined without the conversation task, and someone whose
/// profile does not let them decline cannot even propose it.
/// Why it matters: health invoices are high volume and are declined on the benefit schedule,
/// not after an interview; but the power to decline is never borrowed from the second signer.
#[test]
fn declining_is_a_power_the_proposer_must_hold() {
    let mut w = World::new();
    let mut n = World::notification("h-1", "pol-3", "health", ClaimType::Health);
    n.claimed_minor = Some(400);
    w.ok(Command::Notify(Box::new(n)));
    w.assign("h-1", "jon");
    assert_eq!(w.undecided("h-1", "jon", Request::Decline, "R-NO"), "authority");
    w.assign("h-1", "ana");
    w.decided("h-1", "ana", Request::Decline, "R-NO");
    w.approved("h-1", "ben");
    assert_eq!(w.claim("h-1").status, Status::Declined);
}

/// Purpose: a waiting proposal can be refused by someone who could have signed it or by a lead,
/// never by its proposer, and the checks are run again when the last signature is given.
/// Why it matters: a claim can change between proposal and signature (here a reinsurer's
/// consent becomes required). Signing must approve the claim as it is now, not as it was.
#[test]
fn a_proposal_can_be_refused_and_is_rechecked_at_signing() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    let refuse = |who: &str| Command::Refuse {
        claim: "c-1".into(),
        actor: actor(who),
    };
    assert_eq!(w.refused(refuse("sam")), "nothing_pending");
    w.decided("c-1", "ana", World::admit_lump(None), "R-OK");
    assert_eq!(w.refused(refuse("ana")), "authority");
    assert_eq!(w.refused(refuse("jon")), "authority");
    assert_eq!(w.refused(refuse("una")), "conflict_of_interest");
    w.ok(refuse("sam"));
    assert!(w.claim("c-1").pending.is_none());
    assert_eq!(w.core.noticed("decision_refused")[0]["kind"], "Admit");

    w.decided("c-1", "ana", World::admit_lump(None), "R-OK");
    w.ok(Command::Reinsurance(ReinsuranceNotice::ReinsurerConsentRequired {
        claim: "c-1".into(),
        treaty: "treaty-1".into(),
        due_by: d(2027, 3, 20),
    }));
    assert_eq!(w.unapproved("c-1", "ben"), "awaiting_reinsurer_consent");
    w.ok(Command::Reinsurance(ReinsuranceNotice::ReinsurerConsent {
        claim: "c-1".into(),
        reinsurer: "re-1".into(),
        agreed: true,
        reference: "consent-1".into(),
        treaty: Some("treaty-1".into()),
    }));
    w.approved("c-1", "ben");
    assert_eq!(w.claim("c-1").status, Status::Admitted);
}

/// Purpose: an exclusion on the cover that matches the recorded cause stops the admission; only
/// a profile with the override power can admit over it, with a reason code, and the override is
/// written on the decision. An exclusion the rules cannot read needs the assessor to say they
/// read it.
/// Why it matters: this is the control whose absence the design addendum's reversal of
/// admission entry describes: a claim admitted and paid although an exclusion applied. The
/// record of what the rules made of each exclusion is what later tells an assessor override
/// apart from a gap in the rules.
#[test]
fn an_exclusion_stops_admission_unless_overridden_with_authority() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            exclusions: vec!["EX-0001".into()],
            ..facts("trauma")
        },
    );
    w.open_claim("c-1");
    w.assess("c-1", "CC-0001");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(200_000)), "R-OK"), "exclusion_applies");
    let over = |code: &str| Request::Admit {
        amount_minor: Some(200_000),
        periodic: None,
        exclusion_override: Some(code.into()),
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    assert_eq!(w.undecided("c-1", "ana", over("R-OVR"), "R-OK"), "authority");
    w.assign("c-1", "sam");
    assert_eq!(w.undecided("c-1", "sam", over("R-NO"), "R-OK"), "unknown_reason");
    w.decided("c-1", "sam", over("R-OVR"), "R-OK");
    let decision = w.claim("c-1").decisions.last().expect("recorded").clone();
    assert_eq!(decision.exclusion_override.as_deref(), Some("R-OVR"));
    assert_eq!(decision.exclusion_outcomes["EX-0001"], Match::Applies);

    // A cause the exclusion does not reach is admitted, and the record says the rules looked.
    w.open_claim("c-2");
    w.assess("c-2", "CC-0003");
    w.decided("c-2", "ana", World::admit_lump(Some(100_000)), "R-OK");
    assert_eq!(
        w.claim("c-2").decisions.last().expect("recorded").exclusion_outcomes["EX-0001"],
        Match::DoesNotApply
    );

    // Free-text wording: the rules cannot decide, a person must read it.
    w.core.cover(
        "pol-1",
        "life",
        CoverFacts {
            exclusions: vec!["EX-9001".into()],
            ..facts("life")
        },
    );
    w.ok(Command::Notify(Box::new(World::notification(
        "c-3",
        "pol-1",
        "life",
        ClaimType::TerminalIllness,
    ))));
    w.assign("c-3", "ana");
    w.assess("c-3", "CC-0003");
    assert_eq!(w.undecided("c-3", "ana", World::admit_lump(Some(100_000)), "R-OK"), "exclusion_needs_assessor");
    let read = Request::Admit {
        amount_minor: Some(100_000),
        periodic: None,
        exclusion_override: None,
        exclusion_reviewed: true,
        payer_deceased: false,
    };
    w.decided("c-3", "ana", read, "R-OK");
    assert_eq!(
        w.claim("c-3").decisions.last().expect("recorded").exclusion_outcomes["EX-9001"],
        Match::NeedsAssessor
    );
}

/// Purpose: admission needs cover in force for the life on the event date; a member not on the
/// record needs employer evidence; a claim in grace is paid less the premium owing; and a death
/// claim on a single-life policy ends the policy with the admission.
/// Why it matters: these are the cover facts that decide whether money is owed at all. Paying
/// on lapsed cover, or paying a grace claim without taking the premium, is a direct loss; and
/// a policy left in force after its only life has died keeps billing a dead customer.
#[test]
fn admission_follows_the_cover_facts() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            in_force: false,
            ..facts("trauma")
        },
    );
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    assert_eq!(w.undecided("c-1", "ana", World::admit_lump(Some(1_000)), "R-OK"), "cover_not_confirmed");

    // The scheme's records do not show the member: the employer's evidence settles it.
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            life_on_policy: false,
            ..facts("trauma")
        },
    );
    w.open_claim("c-2");
    w.assess("c-2", "CC-0003");
    assert_eq!(w.undecided("c-2", "ana", World::admit_lump(Some(1_000)), "R-OK"), "cover_not_confirmed");
    w.task("c-2", "member-1", TaskKind::MembershipEvidence, "ana");
    w.complete("c-2", "member-1", "confirmed", "ana").expect("the owner records the evidence");
    w.decided("c-2", "ana", World::admit_lump(Some(1_000)), "R-OK");

    // In grace: the claim is payable less the premium owing.
    let grace = CoverFacts {
        standing: Some("in_grace".into()),
        outstanding_premium_minor: 1_200,
        ..facts("trauma")
    };
    w.core.cover("pol-1", "trauma", grace);
    w.open_claim("c-3");
    w.assess("c-3", "CC-0003");
    w.decided("c-3", "ana", World::admit_lump(Some(100_000)), "R-OK");
    assert!(matches!(
        w.core.instructions.last(),
        Some(Instruction::Admit {
            deduct_premium_minor: 1_200,
            ..
        })
    ));
    assert_eq!((w.claim("c-3").admitted_minor, w.claim("c-3").payable_minor), (100_000, 98_800));

    // A death claim on a policy that ends on death: the admission ends the policy.
    let mut death = World::notification("c-4", "pol-1", "life", ClaimType::Death);
    death.claimed_minor = Some(250_000);
    w.ok(Command::Notify(Box::new(death)));
    w.assign("c-4", "ana");
    w.assess("c-4", "CC-0003");
    let request = Request::Admit {
        amount_minor: Some(250_000),
        periodic: None,
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: true,
    };
    w.decided("c-4", "ana", request, "R-OK");
    let Some(Instruction::Admit { terminate: Some(t), .. }) = w.core.instructions.last() else {
        panic!("the admission did not ask for the policy to end");
    };
    assert_eq!((t.event_date, t.payer_deceased), (d(2027, 2, 20), true));
}

/// Purpose: a second lump sum on the same cover and life is capped by what was already admitted,
/// and a lump sum claim cannot be given benefit-in-payment terms.
/// Why it matters: without the cap, two claims for one event (or a trauma claim followed by a
/// death claim on a cover that pays once) would pay the sum insured twice.
#[test]
fn lump_sums_on_one_cover_never_exceed_the_sum_insured() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    w.decided("c-1", "ana", World::admit_lump(Some(250_000)), "R-OK");
    w.open_claim("c-2");
    w.assess("c-2", "CC-0003");
    assert_eq!(w.undecided("c-2", "ana", World::admit_lump(Some(250_001)), "R-OK"), "amount_exceeds_cover");
    let periodic = Request::Admit {
        amount_minor: None,
        periodic: Some(ic_claims::command::Periodic {
            monthly_minor: 1_000,
            first_period: d(2027, 3, 1),
            end: d(2028, 3, 1),
            expected_end: d(2027, 9, 1),
        }),
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    assert_eq!(w.undecided("c-2", "ana", periodic, "R-OK"), "invalid_schedule");
    // Left out, the amount is what the cover still has to give.
    w.decided("c-2", "ana", World::admit_lump(None), "R-OK");
    assert_eq!(w.claim("c-2").admitted_minor, 250_000);
}

/// Purpose: an ex gratia payment is its own decision with its own limits and second signature,
/// needs a verified payee, is marked as ex gratia on the payment, and leaves the reserve alone.
/// Why it matters: ex gratia money is paid outside the contract. It must never be recoverable
/// from a reinsurer by accident or hide inside claims paid, so it is authorised, posted and
/// reported apart.
#[test]
fn ex_gratia_is_decided_and_paid_apart_from_the_contract() {
    let mut w = World::new();
    w.open_claim("c-1");
    let gift = |amount: i64, payment: &str| Request::ExGratia {
        amount_minor: amount,
        payment_ref: payment.into(),
    };
    assert_eq!(w.undecided("c-1", "ana", gift(500, "xg-1"), "R-OK"), "payee_not_verified");
    w.verify_payee("c-1", "ana");
    assert_eq!(w.undecided("c-1", "ana", gift(0, "xg-1"), "R-OK"), "payee_not_verified");
    w.decided("c-1", "ana", gift(500, "xg-1"), "R-OK");
    let c = w.claim("c-1");
    assert_eq!(
        (c.status, c.reserve_minor, c.paid_minor(true), c.paid_minor(false)),
        (Status::Open, 500_000, 500, 0)
    );
    assert!(matches!(w.core.instructions.last(), Some(Instruction::ExGratia { amount_minor: 500, payee, .. }) if payee == "party-claimant"));
    let paid = w.core.noticed("claim_paid");
    assert_eq!(
        (paid[0]["ex_gratia"].as_str(), paid[0]["payment_ref"].as_str(), paid[0]["amount_minor"].as_str()),
        ("true", "xg-1", "500")
    );
    assert_eq!(w.undecided("c-1", "ana", gift(100, "xg-1"), "R-OK"), "payment_exists");

    // At the two-signature amount it waits; beyond the assessor's limit only a senior signs.
    w.decided("c-1", "ana", gift(900, "xg-2"), "R-OK");
    w.approved("c-1", "ben");
    w.decided("c-1", "ana", gift(1_500, "xg-3"), "R-OK");
    assert_eq!(w.unapproved("c-1", "ben"), "authority");
    w.approved("c-1", "sue");
    assert_eq!(w.claim("c-1").paid_minor(true), 2_900);
}

/// Purpose: a closed claim is reopened with a new reserve by someone with authority; an internal
/// review is worked by someone who did not sign the decision; an overturn is counted; history is
/// kept.
/// Why it matters: a review by the person who made the decision is not a review. Counting
/// overturns against the decision, rule version and assessor is how the insurer learns where
/// its declines are wrong.
#[test]
fn a_closed_claim_reopens_under_independent_review() {
    let mut w = World::new();
    w.open_claim("c-1");
    let reopen = |who: &str, basis: ReopenBasis, reserve: i64| Command::Reopen {
        claim: "c-1".into(),
        basis,
        reserve_minor: reserve,
        actor: actor(who),
    };
    assert_eq!(w.refused(reopen("sam", ReopenBasis::NewEvidence, 100_000)), "not_closed");
    w.conversation("c-1", "talk-1");
    w.decided("c-1", "ana", Request::Decline, "R-NO");
    w.approved("c-1", "ben");
    let decisions = w.claim("c-1").decisions.len();

    assert_eq!(w.refused(reopen("ben", ReopenBasis::InternalReview, 100_000)), "independent_review");
    assert_eq!(w.refused(reopen("jon", ReopenBasis::InternalReview, 100)), "authority");
    assert_eq!(w.refused(reopen("ana", ReopenBasis::NewEvidence, 900_000)), "authority");
    assert_eq!(w.refused(reopen("una", ReopenBasis::NewEvidence, 100_000)), "conflict_of_interest");
    assert_eq!(w.refused(reopen("sam", ReopenBasis::Recurrence, 100_000)), "recurrence_window");
    w.ok(reopen("sam", ReopenBasis::InternalReview, 100_000));
    let c = w.claim("c-1");
    assert_eq!((c.status, c.reserve_minor, c.closed_on, c.overturns), (Status::Open, 100_000, None, 1));
    assert_eq!(c.owner.as_deref(), Some("sam"));
    assert_eq!(c.decisions.len(), decisions);
    assert!(matches!(w.core.instructions.last(), Some(Instruction::Reopen { reserve_minor: 100_000, .. })));
    assert_eq!(w.core.noticed("claim_reopened")[0]["basis"], "InternalReview");
    assert!(c.timers[&TimerKind::Decision].met_on.is_none());

    // New evidence reopens without counting as an overturn.
    w.decided("c-1", "sam", Request::Withdraw, "R-OK");
    w.ok(reopen("ana", ReopenBasis::NewEvidence, 50_000));
    assert_eq!(w.claim("c-1").overturns, 1);
}

/// Purpose: a claim that raised a non-disclosure question waits for underwriting's review; only
/// underwriting's answer closes the task.
/// Why it matters: non-disclosure is decided by underwriting with its own approvals. If the
/// claims assessor could close the review, or admit around it, the separation between the
/// person paying the claim and the person judging the disclosure would be gone.
#[test]
fn a_non_disclosure_review_holds_the_admission() {
    let mut w = World::new();
    w.open_claim("c-1");
    w.assess("c-1", "CC-0003");
    w.task("c-1", "ndr-1", TaskKind::NonDisclosureReview, "ana");
    let asked = w.core.noticed("non_disclosure_review_requested");
    assert_eq!((asked[0]["review_ref"].as_str(), asked[0]["policy"].as_str()), ("ndr-1", "pol-1"));
    assert_eq!(
        w.undecided("c-1", "ana", World::admit_lump(Some(1_000)), "R-OK"),
        "awaiting_non_disclosure_review"
    );
    assert_eq!(
        w.complete("c-1", "ndr-1", "fine", "ana").expect_err("not the assessor's to close").code,
        "system_task"
    );
    let outcome = |review: &str| Command::NonDisclosureOutcome {
        claim: "c-1".into(),
        review_ref: review.into(),
        outcome: NonDisclosureOutcome::NoAction,
    };
    assert_eq!(w.refused(outcome("ndr-9")), "unknown_task");
    w.ok(outcome("ndr-1"));
    assert_eq!(w.refused(outcome("ndr-1")), "unknown_task");
    w.decided("c-1", "ana", World::admit_lump(Some(1_000)), "R-OK");
}

/// Purpose: a claim on the senior queue is worked by a senior and cannot be admitted until its
/// case conference has been held with at least two people.
/// Why it matters: contested and very large claims are where one person's judgement is least
/// safe. The conference is the design's guard against a single assessor deciding them alone.
#[test]
fn the_senior_queue_needs_a_case_conference() {
    let mut w = World::new();
    let mut n = World::notification("c-1", "pol-1", "trauma", ClaimType::Trauma);
    n.flags.contested_cause = true;
    w.ok(Command::Notify(Box::new(n)));
    assert_eq!(w.claim("c-1").queue, Queue::ComplexLarge);
    w.assign("c-1", "sam");
    w.assess("c-1", "CC-0003");
    assert_eq!(w.undecided("c-1", "sam", World::admit_lump(Some(200_000)), "R-OK"), "case_conference_required");
    let task = w.claim("c-1").task(TaskKind::CaseConference).next().expect("opened at triage").id.clone();
    let alone = Command::CompleteTask {
        claim: "c-1".into(),
        task,
        outcome: "held".into(),
        attendees: vec!["sam".into()],
        vault_ref: None,
        spend_minor: 0,
        actor: actor("sam"),
    };
    assert_eq!(w.refused(alone), "not_authorised");
    w.conference("c-1", "sam");
    w.decided("c-1", "sam", World::admit_lump(Some(200_000)), "R-OK");
    assert_eq!(w.claim("c-1").status, Status::Admitted);
}

fn dental(id: &str, invoice: i64, pre_approval: bool) -> ic_claims::command::Notification {
    let mut n = World::notification(id, "pol-3", "health", ClaimType::Health);
    n.health = Some(HealthItem {
        category: "dental".into(),
        invoice_minor: invoice,
        provider: None,
        pre_approval_request: pre_approval,
        settles_pre_approval: None,
        service_code: None,
        service_date: None,
    });
    n
}

/// Purpose: a health invoice or pre-approval request the rules would not decide alone is priced
/// by the same benefit limits when a person decides it, counts against the limits, and a
/// person's pre-approval is a hold with an expiry and no posting.
/// Why it matters: the limits are the contract. They must hold whether the rules or a person
/// admits the claim, or the manual path becomes the way round the annual limit.
#[test]
fn a_person_admitting_a_health_claim_is_bound_by_the_same_limits() {
    let mut w = World::new();
    let grace = CoverFacts {
        sum_insured_minor: 0,
        standing: Some("in_grace".into()),
        outstanding_premium_minor: 40,
        ..facts("health")
    };
    w.core.cover("pol-3", "health", grace);
    w.ok(Command::Notify(Box::new(dental("h-1", 600, false))));
    assert_eq!(w.claim("h-1").status, Status::Open);
    w.assign("h-1", "ana");
    w.assess("h-1", "CC-0003");
    assert_eq!(w.undecided("h-1", "ana", World::admit_lump(Some(441)), "R-OK"), "amount_exceeds_cover");
    w.decided("h-1", "ana", World::admit_lump(None), "R-OK");
    let c = w.claim("h-1");
    assert_eq!((c.admitted_minor, c.payable_minor), (440, 400));
    assert_eq!(w.state.health_used["pol-3/life-1/health/dental/2027-01-01"], 440);

    w.ok(Command::Notify(Box::new(dental("pre-1", 900, true))));
    w.assign("pre-1", "ana");
    w.assess("pre-1", "CC-0003");
    let instructions = w.core.instructions.len();
    w.decided("pre-1", "ana", World::admit_lump(None), "R-OK");
    assert_eq!(w.core.instructions.len(), instructions);
    let hold = w.state.pre_approvals["pre-1"].clone();
    assert_eq!((hold.amount_minor, hold.expires), (680, d(2027, 3, 31)));
    assert_eq!(w.core.noticed("pre_approval_given").last().expect("published")["expires"], "2027-03-31");
    assert_eq!(w.claim("pre-1").status, Status::Open);
}

/// Purpose: benefit terms that do not fit the cover are refused; an exclusion code the rules do
/// not know needs a person; an exclusion scoped to another cover type does not apply; and a
/// benefit ended by decision for a death records the overpayment as late death notification.
/// Why it matters: each is a quiet way to pay wrongly: a monthly benefit above the insured
/// amount, an unknown exclusion treated as no exclusion, and an estate recovery miscoded as
/// the claimant's late information.
#[test]
fn benefit_terms_and_exclusion_scope_are_checked() {
    let mut w = World::new();
    let scoped = ic_claims::config::Exclusion {
        applies_to: ["trauma".to_string()].into(),
        cause_tags: vec!["mental".into()],
        verbatim: false,
    };
    let mut r = rules();
    r.version = 2;
    r.exclusions.insert("EX-0002".into(), scoped);
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w.core.cover(
        "pol-2",
        "ip",
        CoverFacts {
            sum_insured_minor: 4_000,
            exclusions: vec!["EX-0002".into(), "EX-7777".into()],
            ..facts("income_protection")
        },
    );
    w.ok(Command::Notify(Box::new(World::notification(
        "ip-1",
        "pol-2",
        "ip",
        ClaimType::IncomeProtection,
    ))));
    w.assign("ip-1", "ana");
    w.assess("ip-1", "CC-0002");
    let terms = |monthly: i64, reviewed: bool| Request::Admit {
        amount_minor: None,
        periodic: Some(ic_claims::command::Periodic {
            monthly_minor: monthly,
            first_period: d(2027, 3, 1),
            end: d(2027, 5, 1),
            expected_end: d(2027, 4, 1),
        }),
        exclusion_override: None,
        exclusion_reviewed: reviewed,
        payer_deceased: false,
    };
    assert_eq!(w.undecided("ip-1", "ana", terms(3_000, false), "R-OK"), "exclusion_needs_assessor");
    assert_eq!(w.undecided("ip-1", "ana", terms(4_001, true), "R-OK"), "invalid_schedule");
    let lump = Request::Admit {
        amount_minor: None,
        periodic: None,
        exclusion_override: None,
        exclusion_reviewed: true,
        payer_deceased: false,
    };
    assert_eq!(w.undecided("ip-1", "ana", lump, "R-OK"), "invalid_schedule");
    w.decided("ip-1", "ana", terms(3_000, true), "R-OK");
    let outcomes = w.claim("ip-1").decisions.last().expect("recorded").exclusion_outcomes.clone();
    assert_eq!((outcomes["EX-0002"], outcomes["EX-7777"]), (Match::DoesNotApply, Match::NeedsAssessor));
    // A two month benefit is too short for any review to fall due inside it.
    assert!(w.claim("ip-1").schedule.as_ref().expect("in payment").reviews.is_empty());

    w.verify_payee("ip-1", "ana");
    w.pay_run("run-1", d(2027, 3, 1));
    let end = Request::EndBenefit {
        end: d(2027, 3, 1),
        reason: EndReason::Death,
    };
    w.decided("ip-1", "ana", end, "R-OK");
    let c = w.claim("ip-1");
    assert_eq!(
        (c.status, c.overpayments[0].cause, c.overpayments[0].amount_minor),
        (Status::Ended, OverpaymentCause::LateDeathNotification, 3_000)
    );
}

/// Purpose: a claim on pool cover, for a member the record does not name and who has no party
/// yet, is admitted only once the owner records the employer's evidence of eligible membership
/// on the event date, which must be kept with the claim; the claimant is the new party the
/// customer master created.
/// Why it matters: persona test CLM-024. Pool cover names nobody, so the employer's evidence is
/// the only proof the person was covered. A confirmation with nothing on file could not be
/// shown to an auditor or a reinsurer.
#[test]
fn a_pool_cover_claim_is_admitted_on_the_employers_evidence_kept_with_it() {
    let mut w = World::new();
    w.core.cover(
        "pol-1",
        "trauma",
        CoverFacts {
            life_on_policy: false,
            life_party: None,
            ..facts("trauma")
        },
    );
    let mut n = World::notification("p-1", "pol-1", "trauma", ClaimType::Trauma);
    n.claimant = Some("party-new".into());
    w.ok(Command::Notify(Box::new(n)));
    w.ok(Command::Acknowledge {
        claim: "p-1".into(),
        delivery_ref: "delivered-1".into(),
    });
    w.assign("p-1", "ana");
    w.assess("p-1", "CC-0003");
    assert_eq!(w.undecided("p-1", "ana", World::admit_lump(Some(1_000)), "R-OK"), "cover_not_confirmed");
    w.task("p-1", "member-1", TaskKind::MembershipEvidence, "ana");
    let without = Command::CompleteTask {
        claim: "p-1".into(),
        task: "member-1".into(),
        outcome: "confirmed".into(),
        attendees: vec![],
        vault_ref: None,
        spend_minor: 0,
        actor: actor("ana"),
    };
    assert_eq!(w.refused(without), "not_authorised");
    w.complete("p-1", "member-1", "confirmed", "ana").expect("the evidence is on file");
    let c = w.claim("p-1");
    let t = c.task(TaskKind::MembershipEvidence).next().expect("recorded");
    assert_eq!((t.outcome.as_deref(), t.vault_ref.as_deref()), (Some("confirmed"), Some("vault-1")));
    assert_eq!(c.claimant.as_deref(), Some("party-new"));
    w.decided("p-1", "ana", World::admit_lump(Some(1_000)), "R-OK");
    assert_eq!(w.claim("p-1").status, Status::Admitted);
}

/// Purpose: a reason code that does not fit the decision is refused with the
/// codes that do; where the rules in force have none for that kind of
/// decision, the refusal says so and who adds one.
/// Why it matters: found by the scenario run (SCN-12, F-12-1). The assessor
/// tried R-OK and R-EVID for a funeral advance and was told only "is not a
/// reason code for this decision", with no list and no way forward, because
/// the rules held no code for an advance.
#[test]
fn a_wrong_reason_code_is_answered_with_the_codes_that_fit() {
    let mut w = World::new();
    w.open_claim("c-1");
    assert_eq!(
        w.told(World::decide_command("c-1", "ana", Request::Decline, "R-REV")),
        "R-REV is not a reason code for this decision; a decline takes R-NO"
    );
    let mut r = rules();
    r.version = 2;
    r.reason_codes.retain(|code, _| code == "R-NO");
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    assert_eq!(
        w.told(World::decide_command("c-1", "ana", Request::FuneralAdvance { amount_minor: 100 }, "R-NO")),
        "R-NO is not a reason code for this decision, and the claims rules in force have none for a funeral advance: the claims rules owner adds one"
    );
}

/// Purpose: a claim is not declined while a non-disclosure review is open, and a decline giving
/// a reason the rules name as non-disclosure stands only once underwriting's review has avoided
/// the policy; a review that varied the terms, or found nothing, does not support it.
/// Why it matters: found by the scenario run (SCN-24, F-24-3). A claim was declined for
/// non-disclosure with the policy's terms unchanged and no underwriting remedy in place, so the
/// claimant lost the claim while the insurer kept the cover on its books.
#[test]
fn a_non_disclosure_decline_stands_on_underwritings_remedy() {
    let mut w = World::new();
    let mut r = rules();
    r.version = 2;
    r.reason_codes.insert("R-NDISC".to_string(), [DecisionKind::Decline].into());
    r.non_disclosure_reasons = ["R-NDISC".to_string()].into();
    w.ok(Command::ActivateRules { rules: Box::new(r) });
    w.open_claim("c-1");
    w.conversation("c-1", "talk-1");
    assert_eq!(w.undecided("c-1", "ana", Request::Decline, "R-NDISC"), "no_non_disclosure_remedy");
    w.task("c-1", "ndr-1", TaskKind::NonDisclosureReview, "ana");
    assert_eq!(w.undecided("c-1", "ana", Request::Decline, "R-NO"), "awaiting_non_disclosure_review");
    let outcome = |review: &str, outcome| Command::NonDisclosureOutcome {
        claim: "c-1".into(),
        review_ref: review.into(),
        outcome,
    };
    w.ok(outcome("ndr-1", NonDisclosureOutcome::Vary));
    assert_eq!(w.undecided("c-1", "ana", Request::Decline, "R-NDISC"), "no_non_disclosure_remedy");
    w.task("c-1", "ndr-2", TaskKind::NonDisclosureReview, "ana");
    w.ok(outcome("ndr-2", NonDisclosureOutcome::Avoid));
    w.decided("c-1", "ana", Request::Decline, "R-NDISC");
}
