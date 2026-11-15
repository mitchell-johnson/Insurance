//! First notice of a claim from the customer's own channel.

use ic_claims::channel::{claim_id, notification, resolution, MODULE, REQUEST_KIND};
use ic_claims::model::{Channel, Notifier};
use ic_claims::{ClaimType, Command, Queue, Status};
use serde_json::json;

use crate::world::*;

fn asked(claim_type: &str, event_date: &str) -> serde_json::Value {
    json!({ "cover": "trauma", "life": "life-1", "event_date": event_date, "claim_type": claim_type, "layer": 0 })
}

/// Purpose: a customer's request becomes a notification from the app by the policyholder, for
/// the cover, life and event date they chose, with the person who asked as claimant and a claim
/// ID derived from the request; registered, it lands in the assessors' queue like any claim.
/// Why it matters: first notice from the customer page has to reach a person's queue without
/// anyone retyping it, and the same request carried twice must not open two claims.
#[test]
fn a_customers_request_becomes_a_claim_in_the_queue() {
    assert_eq!((REQUEST_KIND, MODULE), ("notify_claim", "claims"));
    let n = notification("r-1", "party-holder", Some("pol-1"), &asked("trauma", "2027-02-20")).expect("taken");
    assert_eq!(
        (
            n.claim.as_str(),
            n.policy.as_str(),
            n.cover.as_str(),
            n.life.as_deref(),
            n.claim_type,
            n.event_date
        ),
        ("app-r-1", "pol-1", "trauma", Some("life-1"), ClaimType::Trauma, d(2027, 2, 20))
    );
    assert_eq!(
        (n.channel, n.notifier, n.claimant.as_deref()),
        (Channel::App, Notifier::Policyholder, Some("party-holder"))
    );
    assert!(n.health.is_none() && n.cause_code.is_none() && n.claimed_minor.is_none());

    let mut w = World::new();
    w.ok(Command::Notify(Box::new(n.clone())));
    let c = w.claim("app-r-1");
    assert_eq!((c.status, c.queue), (Status::Open, Queue::LumpSum));
    assert_eq!(resolution("r-1", None), Ok(claim_id("r-1")));
    // Carried again: refused as already registered, and the request is still applied.
    let again = w.state.apply(&mut w.core, 99, Command::Notify(Box::new(n))).expect_err("one claim");
    assert_eq!(resolution("r-1", Some((again.code, again.message))), Ok("app-r-1".to_string()));
    assert_eq!(
        resolution("r-2", Some(("cover_not_found".into(), "no".into()))),
        Err(("cover_not_found".into(), "no".into()))
    );
}

/// Purpose: a request is refused, with a reason the customer can be shown, when it has no ID or
/// requester, an over-long ID, no policy, a payload that is not a claim request, a blank cover
/// or life, a health claim (made with the invoice) or a premium waiver
/// (which follows a benefit claim); an event after the partition's date is refused on registration.
/// Why it matters: whatever the page sends is untrusted. A claim registered on a guessed cover,
/// with a future event or without an invoice would put work in the queue nobody can decide.
#[test]
fn a_request_that_is_not_a_claim_is_refused_with_a_reason() {
    let code = |request: &str, party: &str, policy: Option<&str>, payload: serde_json::Value| {
        notification(request, party, policy, &payload).expect_err("refused").code
    };
    let good = asked("death", "2027-02-20");
    assert_eq!(code("", "p", Some("pol-1"), good.clone()), "invalid_request");
    assert_eq!(code("r", " ", Some("pol-1"), good.clone()), "invalid_request");
    assert_eq!(code(&"x".repeat(65), "p", Some("pol-1"), good.clone()), "invalid_request");
    assert_eq!(code("r", "p", None, good.clone()), "policy_required");
    assert_eq!(code("r", "p", Some(" "), good.clone()), "policy_required");
    assert_eq!(code("r", "p", Some("pol-1"), json!({ "cover": "life" })), "invalid_claim_request");
    assert_eq!(
        code(
            "r",
            "p",
            Some("pol-1"),
            json!({ "cover": "life", "life": "l", "event_date": "2027-02-20", "claim_type": "death", "cause": "x" })
        ),
        "invalid_claim_request"
    );
    assert_eq!(
        code(
            "r",
            "p",
            Some("pol-1"),
            json!({ "cover": " ", "life": "l", "event_date": "2027-02-20", "claim_type": "death" })
        ),
        "invalid_claim_request"
    );
    assert_eq!(code("r", "p", Some("pol-1"), asked("health", "2027-02-20")), "health_by_invoice");
    assert_eq!(code("r", "p", Some("pol-1"), asked("premium_waiver", "2027-02-20")), "waiver_follows_claim");
    // An event after the partition's date is refused when it is registered.
    let mut w = World::new();
    let later = notification("r", "p", Some("pol-1"), &asked("death", "2027-03-02")).expect("the payload is a claim request");
    assert_eq!(w.refused(Command::Notify(Box::new(later))), "event_in_future");
}
