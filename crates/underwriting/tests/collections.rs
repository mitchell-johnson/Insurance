//! Collections asking for a reinstatement to be underwritten.

mod common;

use common::*;
use ic_underwriting::command::Command;
use ic_underwriting::model::{Entry, Stage};

/// Purpose: collections may open a proposal, but only to reinstate a lapsed policy; anything
/// else it sends is refused `entry`. The reinstatement then waits for the life's disclosure
/// like any other.
/// Why it matters: when the core refuses a reinstatement with `underwriting_required`, the
/// customer has paid their arrears and is waiting; collections opening the proposal itself means
/// nobody has to notice the refusal and re-key it. But collections has no business starting new
/// business or changing cover, so the door it uses opens on reinstatement alone.
#[test]
fn collections_opens_a_proposal_only_to_reinstate() {
    let mut w = World::new();
    let ordinary = w.open_command("P1", &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    assert_eq!(w.err("collections", Command::OpenProposal(Box::new(ordinary))), "entry");

    let mut open = w.open_command("P2", &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    open.entry = Entry::Reinstatement { policy: open.policy.clone() };
    w.ok("collections", Command::OpenProposal(Box::new(open)));
    assert_eq!(w.stage("P2", "L1"), Stage::AwaitingDisclosure);
}
