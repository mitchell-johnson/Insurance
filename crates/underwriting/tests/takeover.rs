//! A scheme member's excess that a previous insurer had accepted, and one
//! reinsurance asked to have underwritten.

mod common;

use common::*;
use ic_underwriting::command::{Command, Effect, Event};
use ic_underwriting::model::{Entry, Stage};

fn excess(w: &World, id: &str, takeover: Option<i64>, reference: Option<&str>) -> Command {
    let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    open.product_code = "group-life".into();
    open.entry = Entry::GroupExcess {
        policy: open.policy.clone(),
        takeover_accepted_minor: takeover,
        reinsurance_reference: reference.map(Into::into),
        delegated_authority: None,
    };
    Command::OpenProposal(Box::new(open))
}

/// Purpose: an excess no larger than what a previous insurer had accepted is taken on standard
/// terms when the proposal opens, with the takeover reason and no disclosure, and goes to the
/// scheme policy once reinsurance capacity is in hand; a larger excess, or one reinsurance asked
/// to have underwritten, waits for the member's disclosure as any other; and a takeover amount
/// of nothing or a blank reference is refused.
/// Why it matters: a scheme's takeover terms promise members the cover they already had with
/// the previous insurer without asking about their health again; asking anyway breaks that
/// promise. But the promise covers only what was accepted before, and when reinsurance needs the
/// cover underwritten because of the member's other policies, skipping it would cede a risk the
/// treaty has not agreed to; its reference goes back on the reservation so reinsurance can tie
/// its accumulation case to the answer.
#[test]
fn a_taken_over_excess_is_accepted_without_evidence_up_to_what_was_accepted() {
    let mut w = World::new();
    let opened = w.ok("group-admin", excess(&w, "grp-uw:S1:1", Some(300_000_00), None));
    assert!(
        opened.events.iter().any(|e| matches!(e, Event::CapacityRequested { .. })),
        "{:?}",
        opened.events
    );
    let decision = w.p("grp-uw:S1:1").lives[&"L1".into()].decisions[0].clone();
    assert_eq!(decision.reasons, set(&["RC-0975"]));
    assert!(decision.all_standard());
    let applied = w.automatic(&opened);
    assert!(applied.effects.iter().any(|e| matches!(
        e,
        Effect::ApplyTerms { covers, .. } if covers[0].sum_insured_minor == 300_000_00 && covers[0].loadings.is_empty()
    )));
    assert!(w.p("grp-uw:S1:1").lives[&"L1".into()].disclosures.is_empty());

    for (id, takeover, reference) in [("grp-uw:S1:2", Some(299_999_99), None), ("grp-uw:S1:3", Some(300_000_00), Some("UWX/k/1"))] {
        w.ok("group-admin", excess(&w, id, takeover, reference));
        assert_eq!(w.stage(id, "L1"), Stage::AwaitingDisclosure, "{id}");
    }
    let held = w.p("grp-uw:S1:3").entry.clone();
    assert!(matches!(held, Entry::GroupExcess { reinsurance_reference: Some(r), .. } if r == "UWX/k/1"));
    // The reference goes back to reinsurance on the reservation, so its
    // accumulation case can be tied to the underwritten excess.
    let disclosed = w.disclose("grp-uw:S1:3", "L1", &clean());
    assert!(disclosed.effects.iter().any(|e| matches!(
        e,
        Effect::ReserveCapacity { excess_reference: Some(r), .. } if r == "UWX/k/1"
    )));

    let zero = excess(&w, "grp-uw:S1:4", Some(0), None);
    assert_eq!(w.err("group-admin", zero), "takeover");
    let blank = excess(&w, "grp-uw:S1:5", None, Some(" "));
    assert_eq!(w.err("group-admin", blank), "takeover");
}
