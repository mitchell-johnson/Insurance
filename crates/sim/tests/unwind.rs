//! An insured event notified late, and an insurer's error found late: the
//! policy is put back to where it stood on a date without rewriting anything.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::event::Event;
use ic_core::{Command, CoverStatus, PolicyId, PolicyStatus, Reject, UnwindCause};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money, FINE_PER_MINOR};
use ic_product::{load_product, Frequency, Sex};
use ic_readmodel::Projections;
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member, term_life};
use ic_sim::year::audit;
use ic_sim::Cluster;

const MONTHLY: i64 = 61_84;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

/// The day the insured event happened: the last day of October 2028, before the first anniversary.
fn event_day() -> NaiveDate {
    d(2028, 10, 31)
}

fn cluster_with(indexation_pays_after_event: bool) -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.keep_journal = true;
    c.open(start()).unwrap();
    for command in shipped_activations().unwrap() {
        let command = match command {
            Command::ActivateProduct { product } if indexation_pays_after_event && product.code == "term-life" => {
                let source = include_str!("../../../products/term_life.star").replace(
                    "indexation(rate_code = \"cpi\")",
                    "indexation(rate_code = \"cpi\", pays_step_applied_after_event = True)",
                );
                let changed = load_product("term_life.star", &source).unwrap();
                assert!(changed.pays_step_applied_after_event());
                Command::ActivateProduct { product: Box::new(changed) }
            }
            other => other,
        };
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

/// A 37-year-old with 500,000 of life cover, paid fourteen months ahead, taken to 16 December
/// 2028: the anniversary on 15 November indexed the cover and two periods were billed since
/// the event day.
fn six_weeks_on(c: &mut Cluster) -> (PartitionId, PolicyId) {
    let issue = term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    let paid = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: "ahead".to_string(),
        amount: Money::new(14 * MONTHLY + 50_00, NZD),
    };
    c.submit(p, "cash-matching", paid).unwrap();
    days(c, 397);
    assert_eq!(c.business_date(), Some(d(2028, 12, 16)));
    let policy = &c.state(p).policies[&id];
    assert_eq!((policy.periods_billed, policy.anniversaries_processed), (14, 1));
    assert_eq!(policy.covers["life"].sum_insured_on(d(2028, 12, 16)), 512_500_00);
    (p, id)
}

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn events(c: &Cluster, p: PartitionId) -> Vec<Event> {
    c.nodes[&p].outbox.iter().map(|e| e.event.clone()).collect()
}

/// Purpose: a death on 31 October notified on 16 December ends the policy as at 31 October.
/// The indexation layer the anniversary added on 15 November ends on the day it started, the
/// two periods billed since are each reversed by a new entry that names the entry it reverses
/// and carries the period's own value date, premium is earned to the day of the event and no
/// further, and every later cent is back in the policyholder's name. Because the payer was
/// the deceased, the refund can only be paid with the claim. The journal still verifies, the
/// log replays to the same state and the read model follows.
/// Why it matters: this is the late-notified death of the design addendum. The claim must be
/// paid on the sum insured at death, the estate must get back premium collected from a dead
/// person's account, and none of it may be done by editing history: an auditor has to be able
/// to walk from each reversal to the posting it reverses, and every root already published
/// has to stay true.
#[test]
fn a_death_notified_six_weeks_late_ends_the_policy_as_at_the_day_it_happened() {
    let mut c = cluster_with(false);
    let (p, id) = six_weeks_on(&mut c);
    let journal_before = c.nodes[&p].journal.clone();
    let terminate = Command::TerminateOnClaim {
        policy: id.clone(),
        event_date: event_day(),
        claim: "clm-1".to_string(),
        payer_deceased: true,
    };
    assert_eq!(code(c.submit(p, "policy-servicing", terminate.clone())), "caller_not_allowed");
    c.submit(p, "claims", terminate).unwrap();

    let state = c.state(p);
    let policy = &state.policies[&id];
    assert_eq!(policy.status, PolicyStatus::Terminated);
    assert_eq!(policy.standing, None);
    assert_eq!(policy.unwind, None);
    let life = &policy.covers["life"];
    assert_eq!(life.status, CoverStatus::Ended);
    assert_eq!(life.sum_insured_on(event_day()), 500_000_00, "the claim is on the sum insured at the event");
    assert_eq!(life.sum_insured_on(d(2028, 11, 1)), 0);
    assert_eq!(
        (life.layers[1].start, life.layers[1].end),
        (d(2028, 11, 15), Some(d(2028, 11, 15))),
        "the layer existed for no days"
    );
    assert_eq!(life.layers[0].end, Some(d(2028, 11, 1)));

    // Twelve periods were billed up to the event. Eleven are earned in full and the twelfth,
    // 15 October to 15 November, for the 17 days up to and including the 31st.
    let fine = |minor: i64| minor as i128 * FINE_PER_MINOR;
    let part = |minor: i64| (fine(minor) * 17 + 15) / 31;
    let earned = -state.ledger.role_total(accounts::role::EARNED_PREMIUM);
    assert_eq!(earned, 11 * fine(MONTHLY) + part(55_84) + part(6_00));
    assert_eq!(state.ledger.balance(&accounts::premium_receivable(&id, NZD).id), 0);
    assert_eq!(state.ledger.natural_balance(&accounts::unearned_premium(&id, NZD).id), 0);
    let held = state.ledger.available(&accounts::policy_suspense(&id, NZD).id);
    let exact = fine(14 * MONTHLY + 50_00) - earned;
    assert!((held * FINE_PER_MINOR - exact).abs() < FINE_PER_MINOR, "held {held}, exact {exact}");

    // Nothing was rewritten: the old journal is a prefix of the new one, and each reversal
    // points at the entry that billed the period.
    let journal = &c.nodes[&p].journal;
    assert_eq!(&journal[..journal_before.len()], &journal_before[..]);
    let unwound: Vec<_> = journal.iter().filter(|e| e.body.kind == "premium_due_unwound").collect();
    assert_eq!(unwound.len(), 2);
    for (reversal, period) in unwound.iter().zip([d(2028, 11, 15), d(2028, 12, 15)]) {
        assert_eq!(reversal.body.value_date, period);
        assert_eq!(reversal.business_date, d(2028, 12, 16));
        assert_eq!(reversal.body.detail["reference"], "clm-1");
        let original = journal_before
            .iter()
            .find(|e| e.id.to_string() == reversal.body.detail["reverses"])
            .expect("the reversal names an entry in the journal");
        assert_eq!(original.body.kind, "premium_due");
        assert_eq!(original.body.detail["period_start"], period.to_string());
    }
    let closing = journal.iter().find(|e| e.body.kind == "terminated_on_claim").unwrap();
    assert_eq!((closing.body.value_date, closing.body.detail["claim"].as_str()), (event_day(), "clm-1"));

    let seen = events(&c, p);
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::LayerUnwound { layer: 2, reference, .. } if reference == "clm-1")));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::PaymentAuthorityCancelled { claim, .. } if claim == "clm-1")));
    let payable = seen
        .iter()
        .find_map(|e| match e {
            Event::PolicyTerminated { effective, payable, .. } if *effective == event_day() => Some(payable.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(payable.len(), 1);
    assert_eq!((payable[0].cover.as_str(), payable[0].sum_insured_minor), ("life", 500_000_00));

    // The refund goes with the claim, to the estate, and nowhere else.
    let refund = |with_claim: Option<&str>| Command::RefundSuspense {
        policy: id.clone(),
        amount: Money::new(held as i64, NZD),
        with_claim: with_claim.map(str::to_string),
    };
    assert_eq!(code(c.submit(p, "policy-servicing", refund(None))), "refund_with_claim");
    assert_eq!(code(c.submit(p, "claims", refund(Some("clm-2")))), "refund_with_claim");
    c.submit(p, "claims", refund(Some("clm-1"))).unwrap();
    assert!(events(&c, p)
        .iter()
        .any(|e| matches!(e, Event::SuspenseRefunded { with_claim: Some(claim), .. } if claim == "clm-1")));
    assert_eq!(c.state(p).ledger.available(&accounts::policy_suspense(&id, NZD).id), 0);

    // The books reconcile the next night, nothing more is billed, and the whole history
    // replays and verifies.
    days(&mut c, 2);
    assert!(!events(&c, p).iter().any(|e| matches!(e, Event::PolicyException { .. })));
    assert_eq!(c.state(p).policies[&id].periods_billed, 12);
    let mut view = Projections::new();
    for event in c.take_events() {
        view.apply(&event).unwrap();
    }
    audit(&c, &view, std::slice::from_ref(&id), true).unwrap();
    assert_eq!(view.policy(&id).unwrap().status, PolicyStatus::Terminated);
    assert!(view.layers_on_policy(&id).iter().all(|row| !row.in_force && row.layer == 1));
}

/// Purpose: where a product's wording pays an indexation step applied after an event nobody
/// yet knew of, the step stands: it is not unwound and the claim is assessed on the indexed
/// sum insured. Premium after the event is still returned.
/// Why it matters: the default (the step never started) is a product rule, not a law. A
/// product that promises the indexed amount must pay it, and the system must make that a
/// setting on the product rather than a manual override on each claim.
#[test]
fn a_product_can_pay_the_indexation_step_applied_after_the_event() {
    let mut c = cluster_with(true);
    let (p, id) = six_weeks_on(&mut c);
    let terminate = Command::TerminateOnClaim {
        policy: id.clone(),
        event_date: event_day(),
        claim: "clm-1".to_string(),
        payer_deceased: false,
    };
    c.submit(p, "claims", terminate).unwrap();
    let seen = events(&c, p);
    assert!(!seen.iter().any(|e| matches!(e, Event::LayerUnwound { .. })));
    assert!(!seen.iter().any(|e| matches!(e, Event::PaymentAuthorityCancelled { .. })));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::PolicyTerminated { payable, .. } if payable[0].sum_insured_minor == 512_500_00)));
    let policy = &c.state(p).policies[&id];
    assert_eq!(policy.refund_with_claim, None, "a living payer is refunded to the source account");
    assert_eq!(policy.periods_billed, 12);
    let held = c.state(p).ledger.available(&accounts::policy_suspense(&id, NZD).id);
    let refund = Command::RefundSuspense {
        policy: id.clone(),
        amount: Money::new(held as i64, NZD),
        with_claim: Some("clm-1".to_string()),
    };
    assert_eq!(code(c.submit(p, "claims", refund)), "refund_to_source");
    days(&mut c, 1);
}

/// Purpose: an event notified on the day it happens needs no unwind: the policy ends with
/// that day, premium is earned through it and the rest of the period is returned.
/// Why it matters: most deaths are notified promptly. The command claims uses for the late
/// case must be the same one that works for the ordinary case, with the same arithmetic.
#[test]
fn an_event_notified_the_same_day_ends_the_policy_without_an_unwind() {
    let mut c = cluster_with(false);
    let issue = term_life(1, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    c.submit(
        p,
        "cash-matching",
        Command::ApplyReceipt {
            policy: id.clone(),
            receipt: "r1".to_string(),
            amount: Money::new(MONTHLY, NZD),
        },
    )
    .unwrap();
    days(&mut c, 9);
    let today = c.business_date().unwrap();
    let terminate = |event_date, claim: &str| Command::TerminateOnClaim {
        policy: id.clone(),
        event_date,
        claim: claim.to_string(),
        payer_deceased: false,
    };
    assert_eq!(code(c.submit(p, "claims", terminate(today.succ_opt().unwrap(), "clm-9"))), "date");
    assert_eq!(code(c.submit(p, "claims", terminate(start().pred_opt().unwrap(), "clm-9"))), "date");
    assert_eq!(code(c.submit(p, "claims", terminate(today, " "))), "reference_required");
    c.submit(p, "claims", terminate(today, "clm-9")).unwrap();
    assert!(!events(&c, p).iter().any(|e| matches!(e, Event::PolicyUnwound { .. })));
    // Ten of the thirty days were days of cover.
    let fine = |minor: i64| minor as i128 * FINE_PER_MINOR;
    let earned = -c.state(p).ledger.role_total(accounts::role::EARNED_PREMIUM);
    assert_eq!(earned, (fine(55_84) + 1) / 3 + fine(6_00) / 3);
    assert_eq!(code(c.submit(p, "claims", terminate(today, "clm-9"))), "not_in_force");
    days(&mut c, 1);
}

/// Purpose: a remediation case unwinds a policy to a date and holds it there: the layer
/// added since ends on its start date, the periods billed since are reversed, and
/// end-of-day neither re-applies the anniversary nor bills while the hold lasts. When the
/// case resumes the policy, end-of-day replays the anniversary and the billing on the terms
/// then in force, here with indexation declined in the meantime, so no step is added. A
/// cover that had expired by age after the date is restored by the unwind and expires again
/// on replay.
/// Why it matters: this is how the insurer corrects its own error (a wrong table, a missed
/// notice) for a policy that carried on under it. The policy must stand still while the
/// error is corrected, or the nightly run would repeat it, and it must then catch up by the
/// ordinary anniversary and billing code rather than by hand.
#[test]
fn a_remediation_unwind_holds_the_policy_and_replays_when_resumed() {
    let mut c = cluster_with(false);
    // Aged 69 at issue: trauma cover expires at the first anniversary; yearly premium.
    let issue = term_life(
        2,
        start(),
        d(1958, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        Some(100_000_00),
        Frequency::Yearly,
        None,
    );
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    let first = c.state(p).policies[&id].outstanding_minor();
    let pay = |c: &mut Cluster, receipt: &str, minor: i64| {
        let command = Command::ApplyReceipt {
            policy: id.clone(),
            receipt: receipt.to_string(),
            amount: Money::new(minor, NZD),
        };
        c.submit(p, "cash-matching", command).unwrap();
    };
    pay(&mut c, "year-1", first);
    days(&mut c, 370);
    let second = c.state(p).policies[&id].outstanding_minor();
    assert!(second > 0);
    pay(&mut c, "year-2", second);
    let before = c.state(p).policies[&id].clone();
    assert_eq!(before.covers["trauma"].status, CoverStatus::Expired);
    assert_eq!(before.covers["life"].layers.len(), 2);

    let unwind = |after, cause, reference: &str| Command::UnwindAfter {
        policy: id.clone(),
        after,
        cause,
        reference: reference.to_string(),
    };
    let resume = |reference: &str| Command::ResumeAfterUnwind {
        policy: id.clone(),
        reference: reference.to_string(),
    };
    let today = c.business_date().unwrap();
    assert_eq!(
        code(c.submit(p, "policy-servicing", unwind(event_day(), UnwindCause::Remediation, "rem-7"))),
        "caller_not_allowed"
    );
    assert_eq!(
        code(c.submit(p, "remediation", unwind(event_day(), UnwindCause::Remediation, ""))),
        "reference_required"
    );
    assert_eq!(code(c.submit(p, "remediation", unwind(today, UnwindCause::Remediation, "rem-7"))), "date");
    assert_eq!(
        code(c.submit(p, "remediation", unwind(d(2027, 11, 14), UnwindCause::Remediation, "rem-7"))),
        "date"
    );
    assert_eq!(code(c.submit(p, "remediation", unwind(start(), UnwindCause::Remediation, "rem-7"))), "too_old");
    assert_eq!(code(c.submit(p, "remediation", resume("rem-7"))), "not_unwound");

    c.submit(p, "remediation", unwind(event_day(), UnwindCause::Remediation, "rem-7")).unwrap();
    let held = c.state(p).policies[&id].clone();
    assert_eq!(held.status, PolicyStatus::InForce);
    assert_eq!((held.periods_billed, held.anniversaries_processed), (1, 0));
    assert_eq!(held.unwind.as_ref().map(|h| (h.after, h.reference.as_str())), Some((event_day(), "rem-7")));
    assert_eq!(held.covers["trauma"].status, CoverStatus::InForce);
    assert_eq!(held.covers["trauma"].layers[0].end, None);
    assert_eq!(held.covers["life"].layers[1].end, Some(d(2028, 11, 15)));
    assert_eq!(held.outstanding_minor(), 0);
    // The second year's premium is the policyholder's money again.
    assert_eq!(c.state(p).ledger.available(&accounts::policy_suspense(&id, NZD).id), second as i128);
    assert!(events(&c, p)
        .iter()
        .any(|e| matches!(e, Event::CoverExpiryUnwound { cover, layers, .. } if cover == "trauma" && layers.len() == 1)));
    assert!(events(&c, p)
        .iter()
        .any(|e| matches!(e, Event::PolicyUnwound { receivables, cause: UnwindCause::Remediation, .. } if receivables.len() == 1)));

    // Held: a second unwind, a claim termination on another date, and the nightly run all
    // leave it where it is.
    assert_eq!(
        code(c.submit(p, "remediation", unwind(d(2028, 10, 1), UnwindCause::Remediation, "rem-8"))),
        "already_unwound"
    );
    let terminate = Command::TerminateOnClaim {
        policy: id.clone(),
        event_date: d(2028, 10, 1),
        claim: "clm-3".to_string(),
        payer_deceased: false,
    };
    assert_eq!(code(c.submit(p, "claims", terminate)), "already_unwound");
    days(&mut c, 2);
    let still = &c.state(p).policies[&id];
    assert_eq!((still.periods_billed, still.anniversaries_processed), (1, 0));
    assert!(c.state(p).end_of_day_unfinished().is_empty());

    // The case declines indexation for the customer, then lets the policy carry on.
    let decline = Command::DeclineIndexation {
        policy: id.clone(),
        declined: true,
    };
    c.submit(p, "policy-servicing", decline).unwrap();
    assert_eq!(code(c.submit(p, "remediation", resume("rem-8"))), "wrong_reference");
    c.submit(p, "claims", resume("rem-7")).unwrap();
    days(&mut c, 1);
    let after = &c.state(p).policies[&id];
    assert_eq!(after.unwind, None);
    assert_eq!((after.periods_billed, after.anniversaries_processed), (2, 1));
    assert_eq!(after.covers["trauma"].status, CoverStatus::Expired);
    assert_eq!(
        after.covers["life"].layers.iter().filter(|l| l.end.is_none()).count(),
        1,
        "no step: indexation was declined"
    );
    assert_eq!(after.covers["life"].sum_insured_on(c.business_date().unwrap()), 500_000_00);
    assert!(after.outstanding_minor() == 0, "the money returned by the unwind paid the replayed bill");
    assert!(!events(&c, p).iter().any(|e| matches!(e, Event::PolicyException { .. })));
    let mut view = Projections::new();
    for event in c.take_events() {
        view.apply(&event).unwrap();
    }
    audit(&c, &view, std::slice::from_ref(&id), true).unwrap();
    let rows = view.layers_on_policy(&id);
    assert_eq!(rows.len(), 1, "{rows:?}");
}

/// Purpose: a scheme policy and a policy that is no longer in force cannot be unwound.
/// Why it matters: a scheme's history is its membership file, corrected member by member
/// with effective dates; unwinding the whole policy would reverse a thousand members'
/// premium for one member's event. A cancelled policy has already returned its premium.
#[test]
fn schemes_and_ended_policies_are_not_unwound() {
    let mut c = cluster_with(false);
    let scheme = group_scheme(1, start(), vec![member("m1", d(1980, 1, 1), "staff", 80_000_00)]);
    let (p, scheme_id) = (c.partition_of(&scheme.group), scheme.policy.clone());
    c.submit(p, "new-business", issue_command(scheme)).unwrap();
    let retail = term_life(5, start(), d(1990, 3, 10), Sex::Male, false, 300_000_00, None, Frequency::Monthly, None);
    let (q, retail_id) = (c.partition_of(&retail.group), retail.policy.clone());
    c.submit(q, "new-business", issue_command(retail)).unwrap();
    days(&mut c, 40);
    c.submit(q, "policy-servicing", Command::CancelPolicy { policy: retail_id.clone() }).unwrap();
    let unwind = |policy: &PolicyId| Command::UnwindAfter {
        policy: policy.clone(),
        after: d(2027, 12, 1),
        cause: UnwindCause::Claim,
        reference: "clm-4".to_string(),
    };
    assert_eq!(code(c.submit(p, "claims", unwind(&scheme_id))), "scheme_policy");
    assert_eq!(code(c.submit(q, "claims", unwind(&retail_id))), "not_in_force");
}
