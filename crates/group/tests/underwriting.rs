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
