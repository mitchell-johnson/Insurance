//! Cover reduced or given up on a retail policy at the owner's request.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::command::ReduceCover;
use ic_core::event::Event;
use ic_core::interest::{InterestCommand, InterestEvent, RestrictedAction};
use ic_core::{Command, LifeId, PartyId, PolicyId, ReceivableKind, Reject};
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn code(r: Result<(), Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

fn policy(c: &mut Cluster, n: u64, trauma: Option<i64>) -> PolicyId {
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, trauma, Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    id
}

fn pay(c: &mut Cluster, policy: &PolicyId, receipt: &str, minor: i64) {
    let cmd = Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: receipt.into(),
        amount: Money::new(minor, NZD),
    };
    c.submit(0, "cash-matching", cmd).unwrap();
}

fn reduce(policy: &PolicyId, cover: &str, life: &str, keep: i64) -> Command {
    Command::ReduceCover(Box::new(ReduceCover {
        policy: policy.clone(),
        cover: cover.into(),
        life: LifeId(life.into()),
        sum_insured_minor: keep,
    }))
}

fn servicing(c: &mut Cluster, cmd: Command) -> Result<(), Reject> {
    c.submit(0, "policy-servicing", cmd)
}

fn suspense(c: &Cluster, id: &PolicyId) -> i128 {
    c.state(0).ledger.natural_balance(&accounts::policy_suspense(id, NZD).id)
}

/// The total of the bill for the period that starts on a date.
fn bill(c: &Cluster, id: &PolicyId, period_start: NaiveDate) -> i64 {
    let policy = &c.state(0).policies[id];
    let mut bills = policy
        .receivables
        .iter()
        .filter(|r| r.kind == ReceivableKind::Period && r.period_start == period_start);
    bills.next().unwrap().total_minor
}

/// Purpose: reducing a life's cover part way through a paid month ends the layer today and
/// puts a smaller one in its place on the same rating date; the premium billed for the days
/// left is credited in proportion and, the month being paid, lands in policy suspense; the
/// next month is billed on the smaller cover; giving a cover up for the life ends its layer
/// with nothing in its place; requests that are not a reduction are refused; and the books
/// reconcile every day.
/// Why it matters: the owner is owed back the premium for cover they no longer have, from
/// the day they gave it up and not from the next bill. A reduction that kept charging, that
/// refunded days already covered, or that lost the layer's entry age would each put a wrong
/// amount on a customer's account.
#[test]
fn a_reduction_ends_layers_from_today_and_credits_the_days_left() {
    let mut c = cluster();
    let id = policy(&mut c, 1, Some(100_000_00));
    let first = bill(&c, &id, start());
    pay(&mut c, &id, "first", first);
    days(&mut c, 10);
    let today = c.business_date().unwrap();
    assert_eq!(today, d(2027, 11, 25));
    assert_eq!(suspense(&c, &id), 0);

    // What is not a reduction, and who may ask.
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", 500_000_00))), "not_a_reduction");
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", -1))), "not_a_reduction");
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", 10_000_00))), "sum_insured");
    assert_eq!(code(servicing(&mut c, reduce(&id, "funeral", "life-1", 0))), "unknown_cover");
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-9", 0))), "unknown_life");
    assert_eq!(
        code(c.submit(0, "underwriting", reduce(&id, "life", "life-1", 300_000_00))),
        "caller_not_allowed"
    );
    let unknown = reduce(&PolicyId::from("nobody"), "life", "life-1", 0);
    assert_eq!(code(servicing(&mut c, unknown)), "unknown_policy");
    assert_eq!(c.state(0).policies[&id].covers["life"].layers.len(), 1, "a refusal changes nothing");
    c.take_events();

    servicing(&mut c, reduce(&id, "life", "life-1", 300_000_00)).unwrap();
    let layers = c.state(0).policies[&id].covers["life"].layers.clone();
    assert_eq!(layers.len(), 2);
    assert_eq!((layers[0].end, layers[0].sum_insured_minor), (Some(today), 500_000_00));
    assert_eq!(
        (
            layers[1].start,
            layers[1].end,
            layers[1].sum_insured_minor,
            layers[1].rated_from,
            layers[1].origin
        ),
        (today, None, 300_000_00, Some(start()), layers[0].origin)
    );
    assert_eq!(c.state(0).policies[&id].covers["life"].sum_insured_on(today), 300_000_00);
    let events = c.take_events();
    assert!(events.iter().any(|e| matches!(&e.event,
        Event::LayerEnded { layer: 1, effective, sum_insured_minor: 500_000_00, .. } if *effective == today)));
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, Event::LayerAdded { layer, .. } if layer.layer == 2 && layer.sum_insured_minor == 300_000_00)));
    let credit = c.state(0).policies[&id].receivables.last().unwrap().clone();
    assert_eq!(
        (credit.kind, credit.period_start, credit.period_end, credit.outstanding_minor),
        (ReceivableKind::Adjustment, today, d(2027, 12, 15), 0)
    );
    assert!(credit.total_minor < 0);
    assert!(events
        .iter()
        .any(|e| matches!(&e.event, Event::PremiumAdjusted { total_minor, .. } if *total_minor == credit.total_minor)));
    assert_eq!(
        suspense(&c, &id),
        -credit.total_minor as i128,
        "the month was paid, so the credit is the customer's money"
    );

    // The next month is billed on the smaller cover, and the credit was that difference
    // for the 20 days left of 30.
    days(&mut c, 21);
    let second = bill(&c, &id, d(2027, 12, 15));
    assert!(second < first);
    let expected = (second - first) * 20 / 30;
    assert!((credit.total_minor - expected).abs() <= 2, "{} against {expected}", credit.total_minor);

    // Giving up trauma cover altogether: the layer ends and nothing replaces it.
    let today = c.business_date().unwrap();
    servicing(&mut c, reduce(&id, "trauma", "life-1", 0)).unwrap();
    let trauma = c.state(0).policies[&id].covers["trauma"].clone();
    assert_eq!((trauma.layers.len(), trauma.layers[0].end), (1, Some(today)));
    assert_eq!(trauma.sum_insured_on(today), 0);
    assert_eq!(code(servicing(&mut c, reduce(&id, "trauma", "life-1", 0))), "unknown_life");
    // The life cover is now all there is: it can be reduced but not given up this way.
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", 0))), "last_cover");
    days(&mut c, 40);
    assert!(bill(&c, &id, d(2028, 1, 15)) < second);
}

/// Purpose: with several layers on a cover, a reduction takes the newest first: an
/// indexation layer goes whole before the original layer is cut, and the cut layer's
/// replacement keeps the original rating date.
/// Why it matters: the design reduces newest first because the oldest layers carry the
/// oldest terms, which the customer cannot get back. Cutting the wrong layer would also
/// claw back commission and recalculate reinsurance on the wrong tranche.
#[test]
fn a_reduction_takes_the_newest_layer_first() {
    let mut c = cluster();
    let id = policy(&mut c, 2, None);
    pay(&mut c, &id, "ahead", 2_000_00);
    days(&mut c, 400);
    let today = c.business_date().unwrap();
    let before = c.state(0).policies[&id].covers["life"].layers.clone();
    assert_eq!(before.len(), 2);
    let indexed = before[1].sum_insured_minor;
    c.take_events();

    servicing(&mut c, reduce(&id, "life", "life-1", 495_000_00)).unwrap();
    let after = c.state(0).policies[&id].covers["life"].layers.clone();
    assert_eq!(after.len(), 3);
    assert_eq!((after[0].end, after[1].end), (Some(today), Some(today)));
    assert_eq!(
        (after[2].sum_insured_minor, after[2].start, after[2].rated_from, after[2].end),
        (495_000_00, today, Some(start()), None)
    );
    let ended: Vec<(u32, i64)> = c
        .take_events()
        .iter()
        .filter_map(|e| match &e.event {
            Event::LayerEnded { layer, sum_insured_minor, .. } => Some((*layer, *sum_insured_minor)),
            _ => None,
        })
        .collect();
    assert_eq!(ended, vec![(2, indexed), (1, 500_000_00)]);
    days(&mut c, 40);
}

/// Purpose: while a lender's interest stands, the owner cannot reduce cover until the lender
/// has consented to it, and the consent is used up by the reduction it allowed.
/// Why it matters: the cover is the lender's security. An owner who could quietly cut it
/// would leave the lender with less than the loan was made against.
#[test]
fn a_reduction_needs_the_consent_of_a_standing_interest() {
    let mut c = cluster();
    let id = policy(&mut c, 3, None);
    days(&mut c, 5);
    let today = c.business_date().unwrap();
    let interest = |cmd: InterestCommand| Command::Interest(Box::new(cmd));
    let record = InterestCommand::Record {
        kind: Default::default(),
        policy: id.clone(),
        interest: "int-1".into(),
        party: PartyId("lender-1".into()),
        notice_received: today,
        extent_minor: None,
        effective_from: start(),
        notice: "vault/notice-1".into(),
    };
    servicing(&mut c, interest(record)).unwrap();
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", 400_000_00))), "interest_consent_required");
    let consent = InterestCommand::Consent {
        policy: id.clone(),
        interest: "int-1".into(),
        action: RestrictedAction::ReduceCover,
        reference: "vault/consent-1".into(),
    };
    servicing(&mut c, interest(consent)).unwrap();
    // A refused request does not use the consent up.
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", 600_000_00))), "not_a_reduction");
    c.take_events();
    servicing(&mut c, reduce(&id, "life", "life-1", 400_000_00)).unwrap();
    assert!(c.take_events().iter().any(|e| matches!(
        &e.event,
        Event::Interest(InterestEvent::ConsentUsed {
            action: RestrictedAction::ReduceCover,
            ..
        })
    )));
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", 300_000_00))), "interest_consent_required");
    days(&mut c, 3);
}

/// Purpose: a reduction on the first day of a period not yet billed raises no credit, since
/// that period will be billed on the smaller cover; and when billing has fallen a day behind
/// the reduction is refused until the end of day has caught up.
/// Why it matters: a credit for a period that was never billed would be money handed out
/// for nothing, and a reduction priced across days that have no bill yet could not be worked
/// out correctly at all.
#[test]
fn a_reduction_waits_for_billing_and_credits_nothing_for_an_unbilled_period() {
    let mut c = cluster();
    let id = policy(&mut c, 4, None);
    let mut date = c.business_date().unwrap();
    while date < d(2027, 12, 15) {
        date = date.succ_opt().unwrap();
        c.submit(0, "leader", Command::AdvanceBusinessDate { to: date }).unwrap();
    }
    let billed = c.state(0).policies[&id].receivables.len();
    servicing(&mut c, reduce(&id, "life", "life-1", 450_000_00)).unwrap();
    assert_eq!(
        c.state(0).policies[&id].receivables.len(),
        billed,
        "nothing is credited for a period not yet billed"
    );
    c.submit(0, "leader", Command::AdvanceBusinessDate { to: d(2027, 12, 16) }).unwrap();
    assert_eq!(code(servicing(&mut c, reduce(&id, "life", "life-1", 400_000_00))), "billing_behind");
    c.submit(0, "end-of-day", Command::RunEndOfDay { policies: vec![id.clone()] }).unwrap();
    servicing(&mut c, reduce(&id, "life", "life-1", 400_000_00)).unwrap();
    assert_eq!(c.state(0).policies[&id].covers["life"].sum_insured_on(d(2027, 12, 16)), 400_000_00);
    assert!(bill(&c, &id, d(2027, 12, 15)) < bill(&c, &id, start()));
}
