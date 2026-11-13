//! A replacement advised by a customer's new adviser, with a top-up.

mod common;

use common::*;
use ic_core::PolicyId;
use ic_underwriting::command::Command;
use ic_underwriting::model::{ConsentKind, Entry, Stage};

fn confirm(id: &str, granted: bool, signed: Option<ic_underwriting::signing::SignedStep>) -> Command {
    Command::RecordConsent {
        proposal: id.into(),
        life: "L1".into(),
        kind: ConsentKind::ReplacementDisclosure,
        granted,
        signed,
    }
}

/// Purpose: an application advised as a replacement is taken only as more cover on a policy in
/// force from an adviser, and nothing is underwritten until the life confirms the replacement
/// disclosure with its own signed step: a disclosure sent first is refused
/// `replacement_disclosure`, an unsigned or mis-signed confirmation is refused `signature`, a
/// confirmation on an application that is not a replacement is refused, and once the
/// disclosure has been taken on it the confirmation cannot be withdrawn.
/// Why it matters: the replacement disclosure is where the customer is told what they keep and
/// what they gain, the premiums compared, and that the adviser earns nothing on the amount
/// replaced. Conduct rules expect it before the customer commits; an application that could
/// run ahead of it would let a replacement be confirmed on advice the customer never saw
/// (ADV-028).
#[test]
fn a_replacement_waits_for_the_signed_replacement_disclosure() {
    let mut w = World::new();
    let increase = |w: &World, id: &str, adviser: bool| {
        let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 200_000_00)], adviser);
        open.entry = Entry::Increase {
            policy: PolicyId::from("POL-P0"),
        };
        open.replacement = true;
        open
    };
    let mut new_business = increase(&w, "P1", true);
    new_business.entry = Entry::NewBusiness;
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(new_business))), "replacement");
    let unadvised = increase(&w, "P1", false);
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(unadvised))), "replacement");

    let open = increase(&w, "P1", true);
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    assert_eq!(
        w.err("new-business", Command::SubmitDisclosure(Box::new(w.disclosure("P1", "L1", &clean())))),
        "replacement_disclosure"
    );
    assert_eq!(w.err("new-business", confirm("P1", true, None)), "signature");
    assert_eq!(
        w.err("new-business", confirm("P1", true, Some(signed("consent", &["P1", "L1", "adviser_access"])))),
        "signature"
    );
    w.ok(
        "new-business",
        confirm("P1", true, Some(signed("consent", &["P1", "L1", "replacement_disclosure"]))),
    );
    // Withdrawn before the application is made, it is needed again.
    w.ok("new-business", confirm("P1", false, None));
    assert_eq!(
        w.err("new-business", Command::SubmitDisclosure(Box::new(w.disclosure("P1", "L1", &clean())))),
        "replacement_disclosure"
    );
    w.ok(
        "new-business",
        confirm("P1", true, Some(signed("consent", &["P1", "L1", "replacement_disclosure"]))),
    );
    w.disclose("P1", "L1", &clean());
    assert_ne!(w.stage("P1", "L1"), Stage::AwaitingDisclosure);
    assert_eq!(w.err("new-business", confirm("P1", false, None)), "too_late");

    // An application that is not a replacement has no replacement disclosure to confirm.
    w.open("P2", 300_000_00);
    assert_eq!(
        w.err(
            "new-business",
            confirm("P2", true, Some(signed("consent", &["P2", "L1", "replacement_disclosure"])))
        ),
        "replacement"
    );
}
