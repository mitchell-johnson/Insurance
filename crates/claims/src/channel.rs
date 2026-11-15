//! First notice of a claim from a customer's own channel.
//!
//! A policyholder tells the insurer about a claim on the customer page or in
//! the app. The portal records that as a request of kind [`REQUEST_KIND`] for
//! this module, with IDs and choices only: the cover, the life, the event
//! date and what happened. Claims has no process of its own, so the portal
//! carries the request to the policy's partition. This module owns what the
//! request means: [`notification`] turns it into the [`Notification`] the
//! claims module registers, or refuses it with a reason the customer can be
//! shown, and [`resolution`] says how the request is closed once the
//! partition has answered.
//!
//! The claim ID is derived from the request ID ([`claim_id`]), so a request
//! carried twice registers one claim: the second `Notify` is refused
//! `claim_exists`, and that refusal closes the request as applied.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command::Notification;
use crate::model::{Channel, ClaimType, Flags, Notifier};
use crate::port::Reject;

/// The request kind the portal records for a claim notification.
pub const REQUEST_KIND: &str = "notify_claim";

/// The module the portal records the request for.
pub const MODULE: &str = "claims";

/// The longest request ID taken, so a claim ID stays a sensible key.
pub const MAX_REQUEST_ID: usize = 64;

/// What the customer told us, as the portal records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    /// The cover code on the policy.
    pub cover: String,
    /// The life insured the claim is for, as the policy names it.
    pub life: String,
    pub event_date: NaiveDate,
    /// What happened, as the customer chose it.
    pub claim_type: ClaimType,
    /// The portal's index of the cover row the customer picked. Not used:
    /// the cover and the life say which cover it is.
    #[serde(default)]
    pub layer: Option<u32>,
}

/// The claim a request registers.
pub fn claim_id(request: &str) -> String {
    format!("app-{request}")
}

fn refuse<T>(code: &str, message: &str) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

/// The notification for a customer's request, or why it cannot be taken.
/// `party` is the person who asked, who is the claimant. An event after the
/// partition's business date is refused by `Notify` itself
/// (`event_in_future`), against the partition's own date.
pub fn notification(request: &str, party: &str, policy: Option<&str>, payload: &Value) -> Result<Notification, Reject> {
    if request.trim().is_empty() || request.len() > MAX_REQUEST_ID || party.trim().is_empty() {
        return refuse("invalid_request", "the request needs its ID and the person who made it");
    }
    let Some(policy) = policy.filter(|p| !p.trim().is_empty()) else {
        return refuse("policy_required", "say which policy the claim is on");
    };
    let asked: ClaimRequest = match serde_json::from_value(payload.clone()) {
        Ok(asked) => asked,
        Err(_) => {
            return refuse(
                "invalid_claim_request",
                "say which cover and life the claim is for, when it happened and what happened",
            )
        }
    };
    if asked.cover.trim().is_empty() || asked.life.trim().is_empty() {
        return refuse("invalid_claim_request", "say which cover and life the claim is for");
    }
    match asked.claim_type {
        // A health claim is an invoice, sent by the provider or with the receipt.
        ClaimType::Health => return refuse("health_by_invoice", "a health claim is made with the invoice, not here"),
        // A waiver follows a benefit claim: the assessor starts it.
        ClaimType::PremiumWaiver => return refuse("waiver_follows_claim", "tell us about the illness or injury and we will look at the premiums"),
        _ => {}
    }
    Ok(Notification {
        claim: claim_id(request),
        policy: policy.to_string(),
        cover: asked.cover,
        life: Some(asked.life),
        claim_type: asked.claim_type,
        event_date: asked.event_date,
        channel: Channel::App,
        notifier: Notifier::Policyholder,
        claimant: Some(party.to_string()),
        claimed_minor: None,
        flags: Flags::default(),
        health: None,
        underwriters: vec![],
        catastrophe_event: None,
        cause_code: None,
    })
}

/// How the portal closes the request once the partition has answered the
/// `Notify`: `Ok(reference)` is applied with the claim as its reference, and
/// `Err((code, message))` is rejected with the module's reason. A request
/// already registered is applied: it was carried before.
pub fn resolution(request: &str, refused: Option<(String, String)>) -> Result<String, (String, String)> {
    match refused {
        None => Ok(claim_id(request)),
        Some((code, _)) if code == "claim_exists" => Ok(claim_id(request)),
        Some(refusal) => Err(refusal),
    }
}
