//! A lender's interest in a policy: recording it, ranking it, the
//! restriction it puts on the owner, consent and release.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::event::Event;
use ic_core::interest::{ranked, InterestCommand, InterestEvent, RestrictedAction};
use ic_core::invest::{InvestCommand, PayoutReason};
use ic_core::{Command, PartyId, PolicyId, Reject};
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

fn code(r: Result<(), Reject>) -> String {
    r.err().map(|e| e.code).unwrap_or_else(|| "accepted".to_string())
}

fn interest(c: &mut Cluster, command: InterestCommand) -> Result<(), Reject> {
    c.submit(0, "policy-servicing", Command::Interest(Box::new(command)))
}

fn record(policy: &PolicyId, id: &str, party: &str, notice_received: NaiveDate, extent_minor: Option<i64>) -> InterestCommand {
    InterestCommand::Record {
        policy: policy.clone(),
        interest: id.to_string(),
        party: PartyId(party.to_string()),
        notice_received,
        extent_minor,
        effective_from: start(),
        notice: format!("vault/notice-{id}"),
    }
}

fn consent(c: &mut Cluster, policy: &PolicyId, id: &str, action: RestrictedAction, reference: &str) -> Result<(), Reject> {
    interest(
        c,
        InterestCommand::Consent {
            policy: policy.clone(),
            interest: id.to_string(),
            action,
            reference: reference.to_string(),
        },
    )
}

fn release(c: &mut Cluster, policy: &PolicyId, id: &str, effective: NaiveDate, discharge: &str) -> Result<(), Reject> {
    interest(
        c,
        InterestCommand::Release {
            policy: policy.clone(),
            interest: id.to_string(),
            effective,
            discharge: discharge.to_string(),
        },
    )
}

fn payout(c: &mut Cluster, policy: &PolicyId, id: &str, reason: PayoutReason) -> Result<(), Reject> {
    let command = InvestCommand::RaisePayout {
        policy: policy.clone(),
        payout: id.to_string(),
        amount_minor: 100_00,
        reason,
    };
    c.submit(0, "policy-servicing", Command::Investment(Box::new(command)))
}

fn cancel(c: &mut Cluster, caller: &str, policy: &PolicyId) -> Result<(), Reject> {
    c.submit(0, caller, Command::CancelPolicy { policy: policy.clone() })
}

/// One partition, two term life policies; the first holds NZD 1,000 in
/// pending investment cash to withdraw.
fn book() -> (Cluster, PolicyId, PolicyId) {
    let mut c = Cluster::in_memory(1, 23);
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let mut ids = Vec::new();
    for n in 1..=2 {
        let issue = term_life(n, start(), d(1988, 2, 20), Sex::Female, false, 400_000_00, None, Frequency::Monthly, None);
        ids.push(issue.policy.clone());
        c.submit(0, "new-business", issue_command(issue)).unwrap();
    }
    let receipt = Command::ApplyReceipt {
        policy: ids[0].clone(),
        receipt: "r1".to_string(),
        amount: Money::new(2_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    let allocate = InvestCommand::AllocateCash {
        policy: ids[0].clone(),
        reference: "alloc-1".to_string(),
        amount_minor: 1_000_00,
    };
    c.submit(0, "policy-servicing", Command::Investment(Box::new(allocate))).unwrap();
    (c, ids[0].clone(), ids[1].clone())
}

fn interest_events(c: &mut Cluster) -> Vec<InterestEvent> {
    c.take_events()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::Interest(event) => Some(event),
            _ => None,
        })
        .collect()
}

/// Purpose: a lender's interest recorded on a policy ranks by the date its notice arrived,
/// can be asked about as at any date, and stops the owner taking value out or giving the
/// cover up until every interested party has consented to that one action. A consent is
/// used up by the action it allowed. A release, on the party's discharge, ends the
/// restriction from its effective date and leaves the past as it was. The insurer's own
/// cancellation and money the insurer owes for a pricing error are not the owner's acts
/// and are not restricted. Sending any of the commands again changes nothing.
/// Why it matters: a bank lends against a policy on the promise that the insurer will not
/// let the borrower cash it in or cancel it behind the bank's back, and will pay the bank
/// first on a claim. If a withdrawal slipped past a standing interest, or one consent could
/// be used twice, the insurer would have broken that promise and would owe the lender.
#[test]
fn an_interest_restricts_the_owner_until_its_holder_consents_or_releases() {
    let (mut c, policy, other) = book();
    let today = start();
    let yesterday = today.pred_opt().unwrap();

    // Recording: what is refused, and that a repeat is accepted once.
    let mut bad_id = record(&policy, "int 1", "lender-1", today, None);
    assert_eq!(code(interest(&mut c, bad_id.clone())), "invalid_interest");
    if let InterestCommand::Record { interest, notice, .. } = &mut bad_id {
        *interest = "int-1".to_string();
        *notice = String::new();
    }
    assert_eq!(code(interest(&mut c, bad_id)), "invalid_interest");
    assert_eq!(code(interest(&mut c, record(&policy, "int-1", "", today, None))), "invalid_interest");
    assert_eq!(code(interest(&mut c, record(&policy, "int-1", "lender-1", today, Some(0)))), "invalid_interest");
    let tomorrow = today.succ_opt().unwrap();
    assert_eq!(code(interest(&mut c, record(&policy, "int-1", "lender-1", tomorrow, None))), "invalid_interest");
    let by_claims = Command::Interest(Box::new(record(&policy, "int-1", "lender-1", today, None)));
    assert_eq!(code(c.submit(0, "claims", by_claims)), "caller_not_allowed");
    let unknown = record(&PolicyId::from("nobody"), "int-1", "lender-1", today, None);
    assert_eq!(code(interest(&mut c, unknown)), "unknown_policy");

    interest(&mut c, record(&policy, "int-1", "lender-1", today, Some(300_000_00))).unwrap();
    interest(&mut c, record(&policy, "int-1", "lender-1", today, Some(300_000_00))).unwrap();
    assert_eq!(
        code(interest(&mut c, record(&policy, "int-1", "lender-1", today, Some(1)))),
        "duplicate_interest"
    );
    // A second lender whose notice arrived a day earlier ranks first.
    interest(&mut c, record(&policy, "int-2", "lender-2", yesterday, None)).unwrap();
    let rank = |c: &Cluster, on: NaiveDate| -> Vec<(String, String, Option<i64>, u32)> {
        ranked(&c.state(0).policies[&policy], on)
            .into_iter()
            .map(|r| (r.interest, r.party.0, r.extent_minor, r.priority))
            .collect()
    };
    assert_eq!(
        rank(&c, today),
        [
            ("int-2".to_string(), "lender-2".to_string(), None, 1),
            ("int-1".to_string(), "lender-1".to_string(), Some(300_000_00), 2)
        ]
    );
    // Neither stood the day before they took effect.
    assert!(rank(&c, yesterday).is_empty());

    // The owner cannot withdraw, surrender or cancel. The insurer's own payment is not held.
    assert_eq!(code(payout(&mut c, &policy, "wd-1", PayoutReason::Withdrawal)), "interest_consent_required");
    assert_eq!(code(payout(&mut c, &policy, "sur-1", PayoutReason::Surrender)), "interest_consent_required");
    let refused = cancel(&mut c, "policy-servicing", &policy).unwrap_err();
    assert_eq!(refused.code, "interest_consent_required");
    assert!(refused.message.contains("reduce_cover"), "{}", refused.message);
    payout(&mut c, &policy, "rem-1", PayoutReason::PriceRemediation).unwrap();

    // Consent: both holders, for the one action.
    assert_eq!(
        code(consent(&mut c, &policy, "int-9", RestrictedAction::Withdrawal, "vault/c-1")),
        "unknown_interest"
    );
    assert_eq!(code(consent(&mut c, &policy, "int-1", RestrictedAction::Withdrawal, "c 1")), "invalid_interest");
    consent(&mut c, &policy, "int-1", RestrictedAction::Withdrawal, "vault/c-1").unwrap();
    consent(&mut c, &policy, "int-1", RestrictedAction::Withdrawal, "vault/c-1").unwrap();
    assert_eq!(code(payout(&mut c, &policy, "wd-1", PayoutReason::Withdrawal)), "interest_consent_required");
    consent(&mut c, &policy, "int-2", RestrictedAction::Withdrawal, "vault/c-2").unwrap();
    // Consent to a withdrawal is not consent to a surrender.
    assert_eq!(code(payout(&mut c, &policy, "sur-1", PayoutReason::Surrender)), "interest_consent_required");
    interest_events(&mut c);
    payout(&mut c, &policy, "wd-1", PayoutReason::Withdrawal).unwrap();
    let used = interest_events(&mut c);
    assert_eq!(used.len(), 2);
    assert!(used.iter().all(|e| matches!(
        e,
        InterestEvent::ConsentUsed {
            action: RestrictedAction::Withdrawal,
            ..
        }
    )));
    // The consents are spent: a second withdrawal needs new ones. The first, sent again, is
    // the same withdrawal and is accepted without them.
    assert_eq!(code(payout(&mut c, &policy, "wd-2", PayoutReason::Withdrawal)), "interest_consent_required");
    payout(&mut c, &policy, "wd-1", PayoutReason::Withdrawal).unwrap();
    let pending = c.state(0).ledger.natural_balance(&accounts::pending_investment_cash(&policy, NZD).id);
    assert_eq!(pending, 1_000_00 - 100_00 - 100_00);

    // The next day the first-ranked lender releases. Today it no longer stands; yesterday it did.
    c.end_of_day().unwrap();
    let next = c.business_date().unwrap();
    assert_eq!(code(release(&mut c, &policy, "int-9", next, "vault/d-2")), "unknown_interest");
    assert_eq!(code(release(&mut c, &policy, "int-2", yesterday, "vault/d-2")), "invalid_interest");
    assert_eq!(code(release(&mut c, &policy, "int-2", next, "")), "invalid_interest");
    release(&mut c, &policy, "int-2", next, "vault/d-2").unwrap();
    release(&mut c, &policy, "int-2", next, "vault/d-2").unwrap();
    assert_eq!(code(release(&mut c, &policy, "int-2", next, "vault/d-other")), "interest_released");
    assert_eq!(
        code(consent(&mut c, &policy, "int-2", RestrictedAction::Surrender, "vault/c-3")),
        "interest_released"
    );
    assert_eq!(rank(&c, next), [("int-1".to_string(), "lender-1".to_string(), Some(300_000_00), 1)]);
    assert_eq!(rank(&c, today).len(), 2);
    let released = &c.state(0).policies[&policy].interests["int-2"];
    assert_eq!(released.released.as_ref().map(|r| (r.effective, r.recorded_on)), Some((next, next)));

    // One interest still stands: the owner's cancellation needs that holder's consent, once.
    assert_eq!(code(cancel(&mut c, "policy-servicing", &policy)), "interest_consent_required");
    consent(&mut c, &policy, "int-1", RestrictedAction::ReduceCover, "vault/c-4").unwrap();
    cancel(&mut c, "policy-servicing", &policy).unwrap();
    assert!(c.state(0).policies[&policy].interests["int-1"].consents.is_empty());

    // The insurer cancelling a policy is not the owner's act: a standing interest does not stop it.
    interest(&mut c, record(&other, "int-3", "lender-1", today, None)).unwrap();
    cancel(&mut c, "underwriting", &other).unwrap();

    let seen = interest_events(&mut c);
    let count = |want: &dyn Fn(&InterestEvent) -> bool| seen.iter().filter(|e| want(e)).count();
    assert_eq!(
        count(&|e| matches!(e, InterestEvent::Released { interest, effective, .. } if interest == "int-2" && *effective == next)),
        1
    );
    assert_eq!(
        count(&|e| matches!(
            e,
            InterestEvent::ConsentUsed {
                action: RestrictedAction::ReduceCover,
                ..
            }
        )),
        1
    );
    assert_eq!(count(&|e| matches!(e, InterestEvent::Recorded { interest, .. } if interest == "int-3")), 1);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}
