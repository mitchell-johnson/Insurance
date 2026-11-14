//! A future insurability option exercised on a life event.

mod common;

use common::*;
use ic_core::LifeId;
use ic_underwriting::case::{CaseType, Queue};
use ic_underwriting::command::{Command, Effect, Event, NoticeKind};
use ic_underwriting::model::{DecisionSource, Entry, ProposalStatus, Stage};

/// A request to exercise the option on `P`'s policy, on a marriage `days` ago.
fn request(w: &World, id: &str, sum: i64, days: i64) -> ic_underwriting::command::OpenProposal {
    let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", sum)], false);
    open.entry = Entry::InsurabilityOption {
        policy: open.policy.clone(),
        event: "marriage".into(),
        event_date: w.today() - chrono::Duration::days(days),
        evidence: "vault://PTY-L1/certificate/1".into(),
    };
    open
}

fn check(w: &mut World, case: &str, by: &str, accepted: bool, note: &str) -> Result<ic_underwriting::command::Applied, ic_underwriting::command::Reject> {
    w.send(
        "underwriting-console",
        Command::CheckOptionEvidence {
            case: case.into(),
            underwriter: by.into(),
            accepted,
            note: note.into(),
        },
    )
}

/// Purpose: a future insurability option is a request carrying the life event, its date and
/// where its proof is kept; it opens with a case on the financial queue for a person to check
/// the proof, and the life is asked no health questions. Accepted by the person holding the
/// case, the extra cover is decided with no underwriting (source `insurability_option`,
/// reason RC-0977), is not offered back to the customer, and reaches the core as terms for
/// an option on the policy. Not accepted, with a reason, the request ends withdrawn and the
/// policyholder is told why (RC-0978). A malformed request, one dated before its event, a
/// check by someone not holding the case, a refusal with no reason, and a check on anything
/// but an option are refused.
/// Why it matters: the option's promise is more cover with no underwriting after a life
/// event, so asking health questions breaks it; but only a person can tell a marriage
/// certificate from something else, and the core does not look at the proof. Without the
/// check, anyone could add a quarter of their cover by saying they married.
#[test]
fn an_option_is_exercised_once_a_person_accepts_the_proof_of_the_event() {
    let mut w = World::new();

    let mut two = request(&w, "O0", 100_000_00, 10);
    two.covers.push(two.covers[0].clone());
    two.covers[1].code = "trauma".into();
    two.covers[1].cover_type = "trauma".into();
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(two))), "option");
    let mut blank = request(&w, "O0", 100_000_00, 10);
    if let Entry::InsurabilityOption { evidence, .. } = &mut blank.entry {
        *evidence = " ".into();
    }
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(blank))), "option");
    assert_eq!(
        w.err("new-business", Command::OpenProposal(Box::new(request(&w, "O0", 100_000_00, -1)))),
        "option"
    );
    let mut elsewhere = request(&w, "O0", 100_000_00, 10);
    elsewhere.policy = "POL-OTHER".into();
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(elsewhere))), "option");

    let opened = w.ok("new-business", Command::OpenProposal(Box::new(request(&w, "O1", 100_000_00, 10))));
    assert!(opened.events.iter().any(|e| matches!(
        e,
        Event::Referred {
            case_type: CaseType::Referral,
            queue: Queue::Financial,
            ..
        }
    )));
    let f = &w.p("O1").lives[&LifeId::from("L1")];
    assert_eq!(f.stage, Stage::InAssessment);
    assert!(f.open_case().unwrap().reasons.contains("RC-0977"));
    let d = w.disclosure("O1", "L1", &clean());
    assert_eq!(w.err("new-business", Command::SubmitDisclosure(Box::new(d))), "not_awaiting_disclosure");

    let case = w.case_of("O1", "L1");
    assert_eq!(check(&mut w, &case, "amy", true, "").unwrap_err().code, "not_owner");
    let case = w.take("O1", "L1", "amy");
    assert_eq!(check(&mut w, &case, "amy", false, " ").unwrap_err().code, "note");
    let checked = check(&mut w, &case, "amy", true, "").unwrap();
    assert!(checked
        .events
        .iter()
        .any(|e| matches!(e, Event::OptionEvidenceChecked { accepted: true, by, .. } if by == "amy")));
    let f = &w.p("O1").lives[&LifeId::from("L1")];
    assert!(f.open_case().is_none());
    let d = &f.decisions[0];
    assert!(matches!(&d.source, DecisionSource::InsurabilityOption { event, checked_by } if event == "marriage" && checked_by == "amy"));
    assert_eq!(d.reasons, set(&["RC-0977"]));
    let applied = w.automatic(&checked);
    assert_eq!(w.stage("O1", "L1"), Stage::Accepted, "nothing to offer back");
    let Some(Effect::ApplyTerms { covers, entry, .. }) = applied.effects.iter().find(|e| matches!(e, Effect::ApplyTerms { .. })) else {
        panic!("no terms: {:?}", applied.effects)
    };
    assert!(matches!(entry, Entry::InsurabilityOption { event, .. } if event == "marriage"));
    assert_eq!((covers[0].sum_insured_minor, covers[0].loadings.len()), (100_000_00, 0));
    assert_eq!(check(&mut w, &case, "amy", true, "").unwrap_err().code, "closed");

    w.ok("new-business", Command::OpenProposal(Box::new(request(&w, "O2", 100_000_00, 10))));
    let case = w.take("O2", "L1", "amy");
    let refused = check(&mut w, &case, "amy", false, "the certificate names someone else").unwrap();
    assert!(matches!(w.p("O2").status, ProposalStatus::Withdrawn { .. }));
    assert!(refused.effects.iter().any(|e| matches!(
        e,
        Effect::Notify { kind: NoticeKind::ProposalWithdrawn, reasons, .. } if reasons == &vec!["RC-0978".to_string()]
    )));

    // Proof of an event is checked only on an option.
    w.ok(
        "new-business",
        Command::OpenProposal(Box::new(w.open_command("P1", &["L1"], &[("L1", "life", "life", 100_000_00)], false))),
    );
    w.disclose("P1", "L1", &yes("heart_condition"));
    let case = w.take("P1", "L1", "amy");
    assert_eq!(check(&mut w, &case, "amy", true, "").unwrap_err().code, "entry");
}
