//! The dunning sequence as configuration, and the notices it may not drop.

mod common;

use common::*;
use ic_collections::machine::Command;
use ic_collections::{Channel, Role, StepKind};

fn refused(sequence: ic_collections::Sequence) -> String {
    let mut world = World::new();
    let applied = world.staff("config-1", &[Role::Configuration], Command::SetSequence { sequence });
    applied.result.unwrap_err().code
}

/// Purpose: COL-012. A sequence that leaves out the suspension or the final
/// notice is refused, and so is one that puts the final notice closer to
/// lapse than the product's minimum or sets that minimum below the floor.
/// Why it matters: these notices are what the law and the conduct regime
/// require before cover is lost; configuration must not be able to skip or
/// shorten them, whoever edits it.
#[test]
fn mandatory_notices_cannot_be_configured_away() {
    let mut no_suspension = sequence();
    no_suspension.steps.retain(|s| s.kind != StepKind::SuspensionNotice);
    assert_eq!(refused(no_suspension), "missing_mandatory_notice");

    let mut no_final = sequence();
    no_final.steps.retain(|s| s.kind != StepKind::FinalNotice);
    assert_eq!(refused(no_final), "missing_mandatory_notice");

    let mut late = sequence();
    late.steps[4].day = 50;
    assert_eq!(refused(late), "notice_too_short");

    let mut short = sequence();
    short.minimum_notice_days = 7;
    assert_eq!(refused(short), "notice_too_short");

    let mut by_sms = sequence();
    by_sms.steps[3].channel = Channel::Sms;
    assert_eq!(refused(by_sms), "invalid_sequence");

    let mut twice = sequence();
    twice.steps.push(step(50, StepKind::FinalNotice, Channel::Post));
    twice.minimum_notice_days = 14;
    twice.lapse_day = 70;
    assert_eq!(refused(twice), "invalid_sequence");

    let mut backwards = sequence();
    backwards.steps[3].kind = StepKind::FinalNotice;
    backwards.steps[4].kind = StepKind::SuspensionNotice;
    assert_eq!(refused(backwards), "invalid_sequence");
}

/// Purpose: the shape of a sequence is checked: a product code, steps on
/// different ascending days between the due date and lapse, a template for
/// each, calls only to the queue, an early call before the call step, and
/// sensible arrangement and escalation limits.
/// Why it matters: a sequence is read the one way on every member and by
/// every officer; one that could be read two ways, or that sends a call as a
/// letter, would be found by a customer rather than by the person who set
/// it.
#[test]
fn a_sequence_is_checked_before_it_is_used() {
    let mut product = sequence();
    product.product = "bad code!".to_string();
    assert_eq!(refused(product), "invalid_sequence");

    let mut empty = sequence();
    empty.steps.clear();
    assert_eq!(refused(empty), "invalid_sequence");

    let mut order = sequence();
    order.steps.swap(0, 1);
    assert_eq!(refused(order), "invalid_sequence");

    let mut beyond = sequence();
    beyond.steps[0].day = 0;
    assert_eq!(refused(beyond), "invalid_sequence");

    let mut template = sequence();
    template.steps[1].template = " ".to_string();
    assert_eq!(refused(template), "invalid_sequence");

    let mut call = sequence();
    call.steps[2].channel = Channel::Email;
    assert_eq!(refused(call), "invalid_sequence");

    let mut early = sequence();
    early.early_call.as_mut().unwrap().day = 20;
    assert_eq!(refused(early), "invalid_sequence");

    let mut no_call = sequence();
    no_call.steps.retain(|s| s.kind != StepKind::CallTask);
    assert_eq!(refused(no_call), "invalid_sequence");

    let mut long = sequence();
    long.max_arrangement_instalments = 61;
    assert_eq!(refused(long), "invalid_sequence");

    let mut never = sequence();
    never.call_escalate_days = 0;
    assert_eq!(refused(never), "invalid_sequence");
}

/// Purpose: only configuration may set a sequence, through the staff
/// caller; a valid one replaces the product's earlier one and is evented
/// with its hash.
/// Why it matters: the sequence decides what customers are told before
/// they lose cover; who changed it and to what must be on the record, and a
/// collections officer must not be able to change it.
#[test]
fn only_configuration_sets_a_sequence() {
    let mut world = World::new();
    let applied = world.officer(Command::SetSequence { sequence: sequence() });
    assert_eq!(applied.result.unwrap_err().code, "not_allowed");
    let applied = world.send("core-events", "x", &[Role::Configuration], Command::SetSequence { sequence: sequence() });
    assert_eq!(applied.result.unwrap_err().code, "not_allowed");
    let applied = world.staff(" ", &[Role::Configuration], Command::SetSequence { sequence: sequence() });
    assert_eq!(applied.result.unwrap_err().code, "no_actor");

    let mut longer = sequence();
    longer.minimum_notice_days = 15;
    let applied = world.staff("config-2", &[Role::Configuration], Command::SetSequence { sequence: longer });
    assert_eq!(applied.result, Ok(()));
    assert!(matches!(&applied.events[0].event, ic_collections::Event::SequenceSet { product, hash } if product == PRODUCT && hash.len() == 64));
    assert_eq!(world.state.sequences[PRODUCT].minimum_notice_days, 15);
}
