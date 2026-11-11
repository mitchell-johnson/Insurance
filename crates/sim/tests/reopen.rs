//! Choices made on a defective notice: the policyholder makes the choice
//! again in a fresh period, and it applies as if made correctly the first time.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::event::Event;
use ic_core::{Command, CoverStatus, PolicyId, PolicyStatus, Reject, ReopenedChoice, Standing, UnwindCause};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_readmodel::Projections;
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::year::audit;
use ic_sim::Cluster;

const MONTHLY: i64 = 61_84;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.keep_journal = true;
    c.open(start()).unwrap();
    for command in shipped_activations().unwrap() {
        c.activate_everywhere(command).unwrap();
    }
    c
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

/// A 37-year-old with 500,000 of life cover, monthly, with `paid` received on the first day.
fn issued(c: &mut Cluster, n: u64, paid: i64) -> (PartitionId, PolicyId) {
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    if paid > 0 {
        let receipt = Command::ApplyReceipt {
            policy: id.clone(),
            receipt: format!("first-{n}"),
            amount: Money::new(paid, NZD),
        };
        c.submit(p, "cash-matching", receipt).unwrap();
    }
    (p, id)
}

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn events(c: &Cluster, p: PartitionId) -> Vec<Event> {
    c.nodes[&p].outbox.iter().map(|e| e.event.clone()).collect()
}

fn window(id: &PolicyId, reference: &str, from: NaiveDate, until: NaiveDate) -> Command {
    Command::OpenReopenWindow {
        policy: id.clone(),
        reopen_reference: reference.to_string(),
        purpose: "renewal".to_string(),
        from,
        until,
    }
}

fn again(id: &PolicyId, reference: &str, choice: ReopenedChoice) -> Command {
    Command::ReopenChoice {
        policy: id.clone(),
        reopen_reference: reference.to_string(),
        choice,
    }
}

/// The whole history replays to the same state, the journal verifies and the read model follows.
fn sound(c: &mut Cluster, policies: &[PolicyId]) {
    let mut view = Projections::new();
    for record in c.take_events() {
        view.apply(&record).unwrap();
    }
    audit(c, &view, policies, true).unwrap();
}

/// Purpose: a policyholder who declined indexation on a renewal notice that quoted a wrong
/// premium accepts it a month after the anniversary, inside the reopen window. The step the
/// anniversary would have added is added from the anniversary, the two periods billed since
/// are reversed by entries with their own value dates and billed again at the premium that
/// includes the step, the difference is due at the end of the window and not before, and the
/// decline no longer counts against the policy. Outside a window, under an unknown reference,
/// or a second time, the choice is refused; so is opening a window by anyone but documents.
/// Why it matters: this is the notice defect of the design addendum. The customer must end
/// up exactly where a correct notice would have left them: cover indexed from the
/// anniversary, charged for it from the anniversary, and not chased as a late payer for
/// money that only became due because the insurer's notice was wrong.
#[test]
fn a_decline_of_indexation_made_on_a_wrong_notice_is_set_aside_from_the_anniversary() {
    let mut c = cluster();
    let (p, id) = issued(&mut c, 1, 15 * MONTHLY);
    let decline = Command::DeclineIndexation {
        policy: id.clone(),
        declined: true,
    };
    c.submit(p, "policy-servicing", decline).unwrap();
    days(&mut c, 397);
    let today = d(2028, 12, 16);
    assert_eq!(c.business_date(), Some(today));
    let anniversary = d(2028, 11, 15);
    {
        let policy = &c.state(p).policies[&id];
        assert_eq!((policy.periods_billed, policy.anniversaries_processed), (14, 1));
        assert_eq!(policy.declined_anniversaries, vec![1]);
        assert_eq!(policy.covers["life"].sum_insured_on(today), 500_000_00);
        assert_eq!(policy.outstanding_minor(), 0);
    }
    // Whatever was paid beyond the fourteen periods billed goes back, so the sums below are exact.
    let spare = c.state(p).ledger.available(&accounts::policy_suspense(&id, NZD).id) as i64;
    let give_back = Command::RefundSuspense {
        policy: id.clone(),
        amount: Money::new(spare, NZD),
        with_claim: None,
    };
    c.submit(p, "policy-servicing", give_back).unwrap();
    let accept = again(&id, "def-7:ntc-1", ReopenedChoice::IndexationAccepted);
    assert_eq!(code(c.submit(p, "policy-servicing", accept.clone())), "unknown_reopen_reference");

    // The corrected notice was delivered on the 10th; the fresh period runs thirty days.
    let (from, until) = (d(2028, 12, 10), d(2029, 1, 9));
    let open = window(&id, "def-7:ntc-1", from, until);
    assert_eq!(code(c.submit(p, "policy-servicing", open.clone())), "caller_not_allowed");
    assert_eq!(code(c.submit(p, "documents", window(&id, " ", from, until))), "reference_required");
    assert_eq!(code(c.submit(p, "documents", window(&id, "def-7:ntc-1", until, from))), "date");
    c.submit(p, "documents", open.clone()).unwrap();
    // Sent again until acknowledged: the same window is the same window.
    c.submit(p, "documents", open).unwrap();
    assert_eq!(
        code(c.submit(p, "documents", window(&id, "def-7:ntc-1", from, d(2029, 2, 1)))),
        "reopen_conflict"
    );
    let opened = events(&c, p).iter().filter(|e| matches!(e, Event::ReopenWindowOpened { .. })).count();
    assert_eq!(opened, 1);
    // A window that has not started, or has run out, lets nothing through.
    c.submit(p, "documents", window(&id, "later", d(2029, 3, 1), d(2029, 3, 31))).unwrap();
    assert_eq!(
        code(c.submit(p, "policy-servicing", again(&id, "later", ReopenedChoice::IndexationAccepted))),
        "reopen_window_closed"
    );
    assert_eq!(
        code(c.submit(p, "policy-servicing", again(&id, "def-7:ntc-1", ReopenedChoice::CancellationSetAside))),
        "not_cancelled"
    );

    let journal_before = c.nodes[&p].journal.clone();
    c.submit(p, "policy-servicing", accept.clone()).unwrap();
    assert_eq!(code(c.submit(p, "remediation", accept)), "reopen_used");

    let state = c.state(p);
    let policy = &state.policies[&id];
    assert!(!policy.indexation_declined);
    assert!(policy.declined_anniversaries.is_empty(), "the decline no longer counts");
    let life = &policy.covers["life"];
    assert_eq!(life.sum_insured_on(anniversary), 512_500_00, "the step applies from the anniversary");
    assert_eq!(life.sum_insured_on(d(2028, 11, 14)), 500_000_00);
    assert_eq!((life.layers[1].start, life.layers[1].end), (anniversary, None));
    assert_eq!(policy.periods_billed, 14);

    // Nothing was rewritten. Each of the two periods since the anniversary was reversed by
    // an entry that names the one it reverses, and billed again for more.
    let journal = &c.nodes[&p].journal;
    assert_eq!(&journal[..journal_before.len()], &journal_before[..]);
    let added = &journal[journal_before.len()..];
    let reversals: Vec<_> = added.iter().filter(|e| e.body.kind == "premium_due_unwound").collect();
    let rebilled: Vec<_> = added.iter().filter(|e| e.body.kind == "premium_due").collect();
    assert_eq!((reversals.len(), rebilled.len()), (2, 2));
    for ((reversal, bill), period) in reversals.iter().zip(&rebilled).zip([anniversary, d(2028, 12, 15)]) {
        assert_eq!(reversal.body.value_date, period);
        assert_eq!(reversal.body.detail["reference"], "def-7:ntc-1");
        let original = journal_before
            .iter()
            .find(|e| e.id.to_string() == reversal.body.detail["reverses"])
            .expect("the reversal names an entry in the journal");
        assert_eq!(original.body.detail["period_start"], period.to_string());
        assert_eq!(bill.body.detail["period_start"], period.to_string());
    }
    // What was paid for the two periods was put back against them; the difference is owed,
    // and due on the last day of the window.
    let owed = policy.outstanding_minor();
    assert!(
        owed > 0 && owed < 2 * MONTHLY / 10,
        "the difference for two periods on 12,500 more cover, not {owed}"
    );
    assert_eq!(policy.oldest_unpaid_due(), Some(until));
    assert_eq!(policy.standing, Some(Standing::Paid));
    assert_eq!(state.ledger.available(&accounts::policy_suspense(&id, NZD).id), 0);
    assert_eq!(state.ledger.balance(&accounts::premium_receivable(&id, NZD).id), owed as i128);
    assert!(events(&c, p).iter().any(|e| matches!(
        e,
        Event::ChoiceReopened { choice: ReopenedChoice::IndexationAccepted, effective, reopen_reference, .. }
            if *effective == anniversary && reopen_reference == "def-7:ntc-1"
    )));

    // The window runs out with the difference unpaid: only then does the policy fall behind.
    days(&mut c, 24);
    assert_eq!(c.business_date(), Some(until));
    assert_eq!(c.state(p).policies[&id].standing, Some(Standing::Paid));
    days(&mut c, 3);
    assert_eq!(c.state(p).policies[&id].standing, Some(Standing::InGrace));
    sound(&mut c, &[id]);
}

/// Purpose: a decline set aside before the anniversary it was made for costs nothing and
/// reverses nothing: the choice is recorded, and the anniversary then indexes the cover in
/// the ordinary way. A policy that never declined has nothing to make again, a policy held
/// by an unwind is left alone, and a policy that is not in force cannot accept indexation.
/// Why it matters: most corrected notices arrive before the anniversary. The reopen path
/// must not manufacture reversals where nothing was billed wrongly, and must not disturb a
/// policy that another correction already holds.
#[test]
fn a_decline_set_aside_before_the_anniversary_just_applies() {
    let mut c = cluster();
    let (p, id) = issued(&mut c, 1, 14 * MONTHLY + 50_00);
    let (_, never) = issued(&mut c, 2, 14 * MONTHLY);
    let decline = Command::DeclineIndexation {
        policy: id.clone(),
        declined: true,
    };
    c.submit(p, "policy-servicing", decline).unwrap();
    days(&mut c, 350);
    let today = c.business_date().unwrap();
    for policy in [&id, &never] {
        c.submit(p, "documents", window(policy, "def-9", today, d(2028, 11, 30))).unwrap();
    }
    assert_eq!(
        code(c.submit(p, "policy-servicing", again(&never, "def-9", ReopenedChoice::IndexationAccepted))),
        "nothing_to_reopen"
    );
    // A remediation case holds the policy as at an earlier date: no other correction until it lets go.
    let hold = Command::UnwindAfter {
        policy: id.clone(),
        after: d(2028, 10, 1),
        cause: UnwindCause::Remediation,
        reference: "rem-1".to_string(),
    };
    c.submit(p, "remediation", hold).unwrap();
    let accept = again(&id, "def-9", ReopenedChoice::IndexationAccepted);
    assert_eq!(code(c.submit(p, "policy-servicing", accept.clone())), "already_unwound");
    let resume = Command::ResumeAfterUnwind {
        policy: id.clone(),
        reference: "rem-1".to_string(),
    };
    c.submit(p, "remediation", resume).unwrap();
    days(&mut c, 1);

    let entries = c.nodes[&p].journal.len();
    c.submit(p, "policy-servicing", accept).unwrap();
    assert_eq!(c.nodes[&p].journal.len(), entries, "nothing had been billed wrongly, so nothing is posted");
    let today = c.business_date().unwrap();
    assert!(events(&c, p).iter().any(|e| matches!(
        e,
        Event::ChoiceReopened { effective, .. } if *effective == today
    )));
    days(&mut c, 20);
    let policy = &c.state(p).policies[&id];
    assert_eq!(policy.anniversaries_processed, 1);
    assert!(policy.declined_anniversaries.is_empty());
    assert_eq!(policy.covers["life"].sum_insured_on(d(2028, 11, 15)), 512_500_00);

    // A cancelled policy has no indexation to accept.
    c.submit(p, "policy-servicing", Command::CancelPolicy { policy: never.clone() }).unwrap();
    assert_eq!(
        code(c.submit(p, "policy-servicing", again(&never, "def-9", ReopenedChoice::IndexationAccepted))),
        "not_in_force"
    );
    sound(&mut c, &[id, never]);
}

/// Purpose: a cancellation made on a wrong notice is set aside twelve days later. Where the
/// refund is still held, the cancellation's own entry is reversed at its own value date and
/// the policy is in force with unbroken cover and the receivables it had. Where the refund
/// has already been paid out, the policy is in force all the same and the refund is owed
/// again, due at the end of the window; the policyholder is not in arrears for it and the
/// policy does not lapse inside the window. Both policies bill, earn and reconcile afterwards.
/// Why it matters: the addendum makes this a reinstatement with no conditions. Requiring
/// the customer to hand the refund back before cover resumes would leave someone uninsured
/// because of the insurer's own mistake; and restoring cover without accounting for the
/// refund would leave money that nobody owes and nobody holds.
#[test]
fn a_cancellation_made_on_a_wrong_notice_is_set_aside_without_conditions() {
    let mut c = cluster();
    let (p, held) = issued(&mut c, 1, 3 * MONTHLY);
    let (_, refunded) = issued(&mut c, 2, 3 * MONTHLY);
    days(&mut c, 40);
    let cancelled_on = c.business_date().unwrap();
    for id in [&held, &refunded] {
        c.submit(p, "policy-servicing", Command::CancelPolicy { policy: id.clone() }).unwrap();
        assert_eq!(c.state(p).policies[id].status, PolicyStatus::Cancelled);
    }
    let refund = c.state(p).ledger.available(&accounts::policy_suspense(&refunded, NZD).id) as i64;
    assert!(refund > MONTHLY, "more than a month's premium was returned: {refund}");
    let pay_out = Command::RefundSuspense {
        policy: refunded.clone(),
        amount: Money::new(refund, NZD),
        with_claim: None,
    };
    c.submit(p, "policy-servicing", pay_out).unwrap();
    days(&mut c, 12);
    let today = c.business_date().unwrap();
    let until = d(2028, 2, 5);
    let set_aside = |id: &PolicyId| again(id, "def-3", ReopenedChoice::CancellationSetAside);
    for id in [&held, &refunded] {
        c.submit(p, "documents", window(id, "def-3", today, until)).unwrap();
        c.submit(p, "remediation", set_aside(id)).unwrap();
        assert_eq!(code(c.submit(p, "remediation", set_aside(id))), "reopen_used");
    }

    for id in [&held, &refunded] {
        let policy = &c.state(p).policies[id];
        assert_eq!(policy.status, PolicyStatus::InForce);
        assert_eq!(policy.cancellation, None);
        let life = &policy.covers["life"];
        assert_eq!(life.status, CoverStatus::InForce);
        for day in [cancelled_on, cancelled_on.succ_opt().unwrap(), today] {
            assert_eq!(life.sum_insured_on(day), 500_000_00, "cover is unbroken on {day}");
        }
        assert_eq!(policy.periods_billed, 2);
        assert_eq!(policy.standing, Some(Standing::Paid));
    }
    // The reversal names the cancellation and carries its date.
    let journal = &c.nodes[&p].journal;
    let reversals: Vec<_> = journal.iter().filter(|e| e.body.kind == "cancellation_set_aside").collect();
    assert_eq!(reversals.len(), 2);
    for reversal in &reversals {
        assert_eq!(reversal.body.value_date, cancelled_on);
        assert_eq!(reversal.body.detail["reference"], "def-3");
        let original = journal.iter().find(|e| e.id.to_string() == reversal.body.detail["reverses"]).unwrap();
        assert_eq!(original.body.kind, "cancelled");
        assert_eq!(original.body.policy, reversal.body.policy);
    }
    // The first policy is exactly where it was: three months paid, nothing owed.
    let state = c.state(p);
    assert_eq!(state.policies[&held].outstanding_minor(), 0);
    assert_eq!(state.ledger.available(&accounts::policy_suspense(&held, NZD).id), MONTHLY as i128);
    // The second owes the refund again, and nothing else, by the end of the window.
    // (The month paid ahead was the policyholder's own money and stays paid out.)
    let returned = refund - MONTHLY;
    let policy = &state.policies[&refunded];
    assert_eq!(policy.outstanding_minor(), returned);
    assert_eq!(policy.oldest_unpaid_due(), Some(until));
    assert_eq!(state.ledger.available(&accounts::policy_suspense(&refunded, NZD).id), 0);
    assert!(events(&c, p).iter().any(|e| matches!(
        e,
        Event::ChoiceReopened { choice: ReopenedChoice::CancellationSetAside, effective, .. } if *effective == cancelled_on
    )));

    // Through the window and a little beyond: no lapse inside it, books reconciling nightly.
    while c.business_date().unwrap() < until {
        days(&mut c, 1);
        assert_eq!(c.state(p).policies[&refunded].status, PolicyStatus::InForce);
    }
    let pays_back = Command::ApplyReceipt {
        policy: refunded.clone(),
        receipt: "back".to_string(),
        amount: Money::new(returned + MONTHLY, NZD),
    };
    c.submit(p, "cash-matching", pays_back).unwrap();
    days(&mut c, 5);
    assert!(!events(&c, p).iter().any(|e| matches!(e, Event::PolicyException { .. })));
    assert_eq!(c.state(p).policies[&held].status, PolicyStatus::InForce);
    sound(&mut c, &[held, refunded]);
}

/// Purpose: a policy that has gone unpaid long enough to lapse does not lapse before the
/// earliest effect of its lapse notice, as last proven delivered. A corrected notice with a
/// later date moves the day; once that day comes the policy lapses in the ordinary way. A
/// delivery dated in the future, or with an effect before its delivery, or unnamed, is
/// refused, and only the documents module may record one.
/// Why it matters: a lapse on a notice the customer never validly received is the insurer
/// ending cover without the warning the law requires. The clock has to restart from the
/// corrected notice, and it has to be the core that waits, because the core is what lapses.
#[test]
fn a_policy_does_not_lapse_before_its_lapse_notice_allows() {
    // What happens with no notice on record: the day the unpaid policy lapses.
    let mut control = cluster();
    let (p, id) = issued(&mut control, 1, 0);
    let mut natural = None;
    for _ in 0..150 {
        days(&mut control, 1);
        if control.state(p).policies[&id].status == PolicyStatus::Lapsed {
            natural = control.business_date();
            break;
        }
    }
    let natural = natural.expect("an unpaid policy lapses");

    let mut c = cluster();
    let (p, id) = issued(&mut c, 1, 0);
    let delivery = |notice: &str, delivered_on: NaiveDate, earliest_effect: NaiveDate| Command::RecordNoticeDelivery {
        policy: id.clone(),
        purpose: "lapse".to_string(),
        notice: notice.to_string(),
        delivered_on,
        earliest_effect,
    };
    days(&mut c, 60);
    let today = c.business_date().unwrap();
    let later = natural + chrono::Days::new(6);
    let first = delivery("ntc-1", today, natural);
    assert_eq!(code(c.submit(p, "policy-servicing", first.clone())), "caller_not_allowed");
    assert_eq!(code(c.submit(p, "documents", delivery("", today, natural))), "reference_required");
    assert_eq!(code(c.submit(p, "documents", delivery("ntc-1", natural, later))), "date");
    assert_eq!(code(c.submit(p, "documents", delivery("ntc-1", today, start()))), "date");
    c.submit(p, "documents", first.clone()).unwrap();
    c.submit(p, "documents", first).unwrap();
    // The first notice was defective; the corrected one restarts the clock.
    c.submit(p, "documents", delivery("ntc-2", today, later)).unwrap();
    let recorded = events(&c, p).iter().filter(|e| matches!(e, Event::NoticeDeliveryRecorded { .. })).count();
    assert_eq!(recorded, 2);
    assert_eq!(c.state(p).policies[&id].notices["lapse"].notice, "ntc-2");

    let day_before = later.pred_opt().unwrap();
    while c.business_date().unwrap() < day_before {
        days(&mut c, 1);
        let on = c.business_date().unwrap();
        assert_eq!(
            c.state(p).policies[&id].status,
            PolicyStatus::InForce,
            "lapsed on {on}, before the notice allows"
        );
    }
    assert!(c.business_date().unwrap() > natural);
    // The day the notice allows it, the policy lapses.
    days(&mut c, 1);
    assert_eq!(c.business_date(), Some(later));
    assert_eq!(c.state(p).policies[&id].status, PolicyStatus::Lapsed);
    sound(&mut c, &[id]);
}
