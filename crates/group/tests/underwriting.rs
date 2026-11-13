//! A whole underwriting decision with the real underwriting module: the
//! group module and the core on one side, the underwriting state machine
//! with its shipped rules on the other, and between them only the messages
//! the two drivers carry.

mod common;
/// The underwriting module's own test world: its state machine with the
/// shipped question sets, rules and authority profiles active.
#[path = "../../underwriting/tests/common/mod.rs"]
mod uw;

use common::*;
use ic_group::adapters::{excess_answer, underwriting_envelope, underwriting_outcome, underwriting_withdrawal};
use ic_group::command::{caller, GroupCommand};
use ic_group::event::{GroupEvent, IssueOutcome, UnderwritingRequest};
use ic_group::mapping::FileKind;
use ic_group::state::CaseStatus;
use ic_underwriting::command::{Command as Uw, Effect, Envelope as UwEnvelope};

/// A scheme with an executive whose formula cover is 210,000 above the
/// automatic amount, and the request the module raised for it.
fn scheme_with_a_request() -> (Harness, UnderwritingRequest) {
    let mut h = Harness::new(terms());
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E003", attrs("staff", 90_000_00, "akl")),
            row(3, "E004", attrs("executive", 240_000_00, "wlg")),
        ],
    );
    let request = h.requests[0].clone();
    (h, request)
}

/// Deliver one of this module's messages to underwriting, as its JSON.
fn deliver(
    w: &mut uw::World,
    message: impl FnOnce(chrono::DateTime<chrono::Utc>, chrono::NaiveDate) -> serde_json::Value,
) -> Result<ic_underwriting::command::Applied, ic_underwriting::command::Reject> {
    let envelope: UwEnvelope = serde_json::from_value(message(w.now, w.today())).unwrap();
    w.send_keyed(&envelope.key, &envelope.caller, envelope.command)
}

/// Carry every answer in underwriting's outbox to the group module, as
/// either driver does, and say how many there were.
fn carry_answers(w: &uw::World, h: &mut Harness) -> usize {
    let answers: Vec<_> =
        w.s.outbox(0, 10_000)
            .iter()
            .filter_map(|(_, effect)| excess_answer(&serde_json::to_value(effect).unwrap()))
            .collect();
    for answer in &answers {
        // An answer for a case already closed is refused, which is settled.
        let _ = h.send_as(
            caller::DRIVER,
            "group-driver",
            GroupCommand::RecordExcessDecision {
                scheme: answer.scheme.clone(),
                case: answer.case,
                decision: answer.decision.clone(),
            },
        );
        h.settle();
    }
    answers.len()
}

/// Purpose: a member's cover above the automatic amount goes the whole way with the real
/// underwriting module: the request opens a proposal there, the member's disclosure is
/// assessed by the shipped rules, the accepted terms come back and become a layer in the
/// core for the amount asked, and underwriting is told which layer so that it confirms
/// the reinsurance capacity it reserved.
/// Why it matters: each half was tested against the other's types, never against the
/// other's behaviour. A proposal the rules cannot assess, or terms the core will not
/// write, would leave a member believing they are covered above the automatic amount
/// when they are not.
#[test]
fn accepted_excess_goes_from_request_to_layer_and_back() {
    let (mut h, request) = scheme_with_a_request();
    assert!(request.party.is_some(), "the member is a known person before underwriting is asked");
    let key = format!("grp-uw:{SCHEME}:{}", request.case);
    let mut w = uw::World::new();

    let opened = deliver(&mut w, |at, today| underwriting_envelope(&key, at, today, &request).unwrap()).unwrap();
    assert!(!opened.duplicate);
    assert_eq!(carry_answers(&w, &mut h), 0, "nothing to carry until the member has disclosed");

    let disclosed = w.disclose(&key, "E004", &uw::clean());
    w.automatic(&disclosed);
    assert_eq!(carry_answers(&w, &mut h), 1);
    assert_eq!(h.scheme().cases[&request.case].status, CaseStatus::Accepted);
    assert_eq!(h.excess_layers("E004"), vec![(request.excess_minor, None)]);
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessCoverDecided { in_place: true, .. })));

    // The core's layer goes back, and underwriting confirms capacity against it.
    assert_eq!(h.outcomes.len(), 1);
    let (request_key, outcome) = h.outcomes[0].clone();
    assert_eq!(request_key, key);
    let IssueOutcome::Issued { layer, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    let issued = deliver(&mut w, |at, today| {
        underwriting_outcome(&format!("{key}/excess-issued"), at, today, &request_key, &outcome)
    })
    .unwrap();
    assert!(
        issued
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ConfirmCapacity { policy, cover, .. } if *policy == request.policy && *cover == request.cover)),
        "{:?}",
        issued.effects
    );
    assert!(*layer > 0);
    // Told twice, underwriting says it is already recorded, which the driver treats as settled.
    let again = deliver(&mut w, |at, today| {
        underwriting_outcome(&format!("{key}/excess-issued-2"), at, today, &request_key, &outcome)
    });
    assert_eq!(again.unwrap_err().code, "not_issuing");
    // Carried again, the answer changes nothing.
    carry_answers(&w, &mut h);
    assert_eq!(h.excess_layers("E004").len(), 1);
}

/// Purpose: the two ways a request ends without cover both close it on each side: a
/// request withdrawn because the salary fell back ends the proposal in underwriting, and
/// a member who never discloses is lapsed by underwriting's own sweep, which closes the
/// case here and tells the employer the cover is not in place.
/// Why it matters: a proposal left open keeps asking a member for health details nobody
/// needs, and a case left open here shows the employer cover "pending" for ever.
#[test]
fn a_request_that_ends_without_cover_is_closed_on_both_sides() {
    // Withdrawn: the salary falls before the member has answered.
    let (mut h, request) = scheme_with_a_request();
    let key = format!("grp-uw:{SCHEME}:{}", request.case);
    let mut w = uw::World::new();
    deliver(&mut w, |at, today| underwriting_envelope(&key, at, today, &request).unwrap()).unwrap();
    h.salary("f2", "E004", "executive", 150_000_00);
    assert_eq!(h.withdrawals, vec![key.clone()]);
    let withdrawn = deliver(&mut w, |at, today| underwriting_withdrawal(&format!("{key}/withdraw"), at, today, &key)).unwrap();
    assert!(withdrawn
        .effects
        .iter()
        .any(|e| matches!(e, Effect::ProposalEnded { outcome, .. } if outcome == "withdrawn")));
    assert_eq!(carry_answers(&w, &mut h), 1);
    assert_eq!(h.scheme().cases[&request.case].status, CaseStatus::NotProceeded);
    // Withdrawn again: underwriting says it is closed, which is settled.
    let again = deliver(&mut w, |at, today| underwriting_withdrawal(&format!("{key}/withdraw-2"), at, today, &key));
    assert_eq!(again.unwrap_err().code, "closed");

    // Never answered: underwriting lapses it and the case closes here.
    let (mut h, request) = scheme_with_a_request();
    let key = format!("grp-uw:{SCHEME}:{}", request.case);
    let mut w = uw::World::new();
    deliver(&mut w, |at, today| underwriting_envelope(&key, at, today, &request).unwrap()).unwrap();
    w.days(91);
    w.ok("timer", Uw::Sweep { after: None, max: 100 });
    assert_eq!(carry_answers(&w, &mut h), 1);
    assert_eq!(h.scheme().cases[&request.case].status, CaseStatus::NotProceeded);
    assert!(h.saw(|e| matches!(e, GroupEvent::ExcessCoverDecided { in_place: false, .. })));
    assert!(h.excess_layers("E004").is_empty());
}

/// Purpose: a loading moves from the employer to the member with the real underwriting
/// module: cover accepted with a loading is in place with the employer paying; staff ask
/// underwriting to offer the member the loading; a member who declines keeps the cover as
/// it was and the scheme publishes that; a member who then accepts gets a replacement
/// layer on which they pay the loading, the old layer ends the same day, and underwriting,
/// told of the new layer under the original request, is issued again without confirming
/// reinsurance capacity a second time.
/// Why it matters: each side was built against the other's description of its messages. A
/// key or field one side reads differently would leave the member's signed agreement
/// unanswered, with the loading still on the employer's invoice where a small scheme lets
/// the employer work out whose health it reflects.
#[test]
fn a_loading_moves_to_the_member_through_underwritings_reoffer() {
    use ic_core::policy::MemberPays;
    use ic_underwriting::command::OfferResponse;
    use ic_underwriting::model::ProposalStatus;

    let (mut h, request) = scheme_with_a_request();
    let key = format!("grp-uw:{SCHEME}:{}", request.case);
    let cover = request.cover.to_string();
    let mut w = uw::World::new();
    let payers = |h: &Harness| -> Vec<MemberPays> {
        h.c.state(h.p).policies[&h.policy]
            .covers
            .values()
            .flat_map(|c| c.layers.iter())
            .filter(|l| l.life == life("E004") && l.origin == ic_core::LayerOrigin::UnderwrittenExcess)
            .map(|l| l.member_pays)
            .collect()
    };
    let reoffer = |w: &mut uw::World, reason: &str| {
        w.ok(
            "group-admin",
            Uw::ReofferExcessTerms {
                proposal: key.as_str().into(),
                reason: reason.into(),
            },
        )
    };
    let answer = |w: &mut uw::World, response: OfferResponse| {
        let offer = w.p(&key).open_reoffer().expect("an open offer").id.clone();
        let signed = uw::signed("offer", &[&key, "E004", &offer, response.key()]);
        w.ok(
            "new-business",
            Uw::RespondToOffer {
                proposal: key.as_str().into(),
                life: "E004".into(),
                offer,
                response,
                signed,
            },
        )
    };

    // Accepted with a loading by an underwriter, agreed by the member, written by the core.
    deliver(&mut w, |at, today| underwriting_envelope(&key, at, today, &request).unwrap()).unwrap();
    w.disclose(&key, "E004", &uw::yes("heart_condition"));
    let case = w.take(&key, "E004", "amy");
    let proposed = w.propose(&case, "amy", vec![uw::accept(&cover, 50, None)]).unwrap();
    w.automatic(&proposed);
    w.respond(&key, "E004", OfferResponse::Accept);
    assert_eq!(carry_answers(&w, &mut h), 1);
    assert_eq!(payers(&h), vec![MemberPays::Nothing], "the employer pays the loading in its total");
    let (request_key, outcome) = h.outcomes[0].clone();
    deliver(&mut w, |at, today| {
        underwriting_outcome(&format!("{key}/excess-issued"), at, today, &request_key, &outcome)
    })
    .unwrap();
    assert!(matches!(w.p(&key).status, ProposalStatus::Issued { .. }));

    // Offered and declined: the cover stands and the scheme says so.
    reoffer(&mut w, "too few members with underwritten cover for a total");
    assert!(answer(&mut w, OfferResponse::Decline)
        .effects
        .iter()
        .any(|e| matches!(e, Effect::ReofferEnded { .. })));
    carry_answers(&w, &mut h);
    assert!(h.saw(|e| matches!(e, GroupEvent::LoadingMoveDeclined { case, .. } if *case == request.case)));
    assert_eq!(payers(&h), vec![MemberPays::Nothing]);
    assert_eq!(h.outcomes.len(), 1);

    // Offered again and accepted: a replacement layer, the member paying.
    reoffer(&mut w, "asked again after a call with the member");
    answer(&mut w, OfferResponse::Accept);
    assert_eq!(w.p(&key).status, ProposalStatus::Issuing);
    let moved = h.today();
    carry_answers(&w, &mut h);
    assert_eq!(payers(&h), vec![MemberPays::Nothing, MemberPays::Loading]);
    assert_eq!(h.excess_layers("E004"), vec![(request.excess_minor, Some(moved)), (request.excess_minor, None)]);
    assert_eq!(h.scheme().cases[&request.case].status, CaseStatus::Ended);

    // Underwriting hears of the new layer under the request it knows.
    assert_eq!(h.outcomes.len(), 2);
    let (request_key, outcome) = h.outcomes[1].clone();
    assert_eq!(request_key, key);
    let written = deliver(&mut w, |at, today| {
        underwriting_outcome(&format!("{key}/excess-issued/2"), at, today, &request_key, &outcome)
    })
    .unwrap();
    assert!(matches!(w.p(&key).status, ProposalStatus::Issued { .. }));
    assert!(
        !written.effects.iter().any(|e| matches!(e, Effect::ConfirmCapacity { .. })),
        "{:?}",
        written.effects
    );
    // Carried again, every answer is settled and nothing more is written.
    carry_answers(&w, &mut h);
    assert_eq!(h.excess_layers("E004").len(), 2);
    assert_eq!(h.outcomes.len(), 2);
}

/// Purpose: a scheme taken over from a previous insurer brings its members across on the
/// takeover terms: someone employed before the takeover joins although their waiting period
/// has not run, someone who started on the day waits theirs out, someone not actively at work (who
/// stays with the previous insurer) is refused by name of the rule, and an executive's cover
/// above the automatic amount, no more than the previous insurer had accepted, is put in
/// place by the real underwriting module on standard terms under the takeover reason code
/// without the member being asked a single health question.
/// Why it matters: an employer changes insurer on the promise that nobody loses cover they
/// had. Asking a long-covered member for medical evidence breaks that promise, and taking
/// on a member already off sick takes on a claim that belongs to the previous insurer.
#[test]
fn a_scheme_taken_over_keeps_members_cover_without_new_evidence() {
    use ic_group::terms::TakeoverTerms;

    let mut t = terms();
    t.eligibility.waiting_days = 90;
    t.eligibility.actively_at_work = true;
    t.takeover = Some(TakeoverTerms {
        previous_insurer: "Southern Mutual".into(),
        effective: d(2027, 11, 1),
        waive_waiting_period: true,
        honour_excess_up_to_minor: Some(250_000_00),
        reason_code: "takeover-2027".into(),
    });
    let mut h = Harness::new(t);
    let with = |category: &str, salary, change: &dyn Fn(&mut ic_group::record::MemberAttrs)| {
        let mut a = attrs(category, salary, "akl");
        change(&mut a);
        a
    };
    h.apply(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", with("staff", 60_000_00, &|a| a.employment_start = Some(d(2027, 10, 20)))),
            row(3, "E003", with("staff", 60_000_00, &|a| a.employment_start = Some(d(2027, 11, 1)))),
            row(4, "E004", with("executive", 240_000_00, &|a| a.employment_start = Some(d(2027, 10, 20)))),
            row(5, "E005", with("staff", 60_000_00, &|a| a.actively_at_work = Some(false))),
        ],
    );
    assert_eq!(h.core_lives(), vec!["E001", "E002", "E004"]);
    let refused: Vec<(Option<&str>, &str)> = h.scheme().files["f1"]
        .rejected
        .iter()
        .map(|r| (r.employee_id.as_deref(), r.code.as_str()))
        .collect();
    assert_eq!(refused, vec![(Some("E005"), "not_actively_at_work")]);
    // The new starter joins when their own waiting period has run.
    let waiting: Vec<_> = h.scheme().scheduled.keys().map(|(date, _)| *date).collect();
    assert_eq!(waiting, vec![d(2028, 1, 30)]);

    // The executive's excess is within what the previous insurer had accepted.
    let request = h.requests[0].clone();
    assert_eq!((request.excess_minor, request.honour_without_evidence_minor), (210_000_00, Some(250_000_00)));
    let key = format!("grp-uw:{SCHEME}:{}", request.case);
    let mut w = uw::World::new();
    let opened = deliver(&mut w, |at, today| underwriting_envelope(&key, at, today, &request).unwrap()).unwrap();
    let life = w.p(&key).lives.values().next().unwrap();
    assert!(life.disclosures.is_empty(), "the member is asked nothing");
    assert!(life.decisions[0].reasons.contains("RC-0975") && life.decisions[0].all_standard());
    w.automatic(&opened);
    assert_eq!(carry_answers(&w, &mut h), 1);
    assert_eq!(h.scheme().cases[&request.case].status, CaseStatus::Accepted);
    assert_eq!(h.excess_layers("E004"), vec![(210_000_00, None)]);
}
