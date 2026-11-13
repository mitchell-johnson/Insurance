//! Notices for staff: an underwriter, a queue lead or the underwriting
//! manager. They are published to the module's event hub, which the staff
//! portal and the alerts read; a consumer drops a notice whose ID it has
//! already seen, so two drivers, or one driver trying again, publish it
//! harmlessly twice.
//!
//! A notice carries what it is, who it is for, the proposal and the life,
//! and nothing else. Reasons and answers stay in the module, where the
//! console shows them to whoever the authority policy lets see them; the hub
//! is read more widely than that.
//!
//! Letters to a customer or an adviser are documents, which the documents
//! module renders and delivers: each is a `request_letter` to it under the
//! request `uw:{outbox number}`, carrying the kind, the recipient, the
//! proposal, the life and the reason codes the person may be told, and
//! nothing clinical. A refusal parks the letter for a person.
//!
//! Orders for medical evidence (request, chase, cancel) go to the evidence
//! hub, which the provider's adapter reads, and requests for a delegate's
//! file to the delegates hub. Each carries the request and, for a new order,
//! the kind and the person's opaque party ID: who the person is, and who
//! their doctor is, the adapter reads from the customer master under its own
//! identity. Answers come back through the intake service.

use std::future::Future;

use chrono::{DateTime, Utc};

use ic_azure::eventhubs::EventHubsSink;
use ic_azure::AzureError;
use serde_json::{json, Value};

use crate::command::{Effect, Recipient};
use crate::driver::{DriverError, Forward, NothingConnected, Refusal};

/// Where notices for staff are published.
pub trait StaffFeed: Send + Sync {
    /// Publish one notice. `id` is the same on every try.
    fn publish(&self, id: &str, kind: &str, body: Value) -> impl Future<Output = Result<(), AzureError>> + Send;
}

/// The hub has one partition, so the notices are one ordered sequence.
impl StaffFeed for EventHubsSink {
    async fn publish(&self, id: &str, kind: &str, body: Value) -> Result<(), AzureError> {
        self.send_json(0, &[(body, json!({"event-id": id, "kind": kind}))]).await
    }
}

/// The ID a notice is published under: its number in the outbox.
pub fn notice_id(seq: u64) -> String {
    format!("underwriting-notice:{seq}")
}

/// The documents module, which renders and delivers letters.
pub trait Letters: Send + Sync {
    /// Send one `request_letter` command. `key` is the same on every try.
    fn request(&self, key: &str, at: DateTime<Utc>, command: Value) -> impl Future<Output = Result<Result<(), Refusal>, DriverError>> + Send;
}

/// No documents module: letters are parked with the reason.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLetters;

impl Letters for NoLetters {
    async fn request(&self, _key: &str, _at: DateTime<Utc>, _command: Value) -> Result<Result<(), Refusal>, DriverError> {
        Ok(Err(Refusal::new("not_connected", "correspondence is not connected to underwriting")))
    }
}

/// The request a letter goes under at the documents module.
pub fn letter_request(seq: u64) -> String {
    format!("uw:{seq}")
}

/// The ID an evidence order or a delegate file request is published under.
pub fn order_id(seq: u64) -> String {
    format!("underwriting-order:{seq}")
}

/// Staff notices to `staff`, letters to `letters`, evidence orders to
/// `evidence` and delegate file requests to `delegates`.
#[derive(Clone)]
pub struct Notices<S, L> {
    pub staff: S,
    pub evidence: S,
    pub delegates: S,
    pub letters: L,
    pub clock: fn() -> DateTime<Utc>,
}

impl<S: StaffFeed, L: Letters> Notices<S, L> {
    /// `recipient` is in the documents module's shape, which names the variant outside.
    async fn letter(&self, seq: u64, recipient: Value, effect: &Effect) -> Result<Option<String>, DriverError> {
        let Effect::Notify {
            kind, proposal, life, reasons, ..
        } = effect
        else {
            return NothingConnected.forward(seq, effect).await;
        };
        let at = (self.clock)();
        let request = letter_request(seq);
        let command = json!({"request_letter": {
            "request": request,
            "kind": kind,
            "recipient": recipient,
            "policy": null,
            "subject": proposal,
            "life": life,
            "reasons": reasons,
            "detail": {},
            "business_date": at.date_naive(),
        }});
        Ok(match self.letters.request(&request, at, command).await? {
            Ok(()) => None,
            Err(r) if r.code == "not_connected" => Some(r.message),
            Err(r) => Some(format!("documents refused the letter: {}: {}", r.code, r.message)),
        })
    }
}

/// Published, parked with the hub's refusal, or unavailable to try again.
async fn publish<S: StaffFeed>(feed: &S, hub: &str, what: &str, id: &str, kind: &str, body: Value) -> Result<Option<String>, DriverError> {
    match feed.publish(id, kind, body).await {
        Ok(()) => Ok(None),
        // The hub is missing or the driver may not send to it: trying
        // again will not help, and every effect behind would wait.
        Err(AzureError::Status { status, body, .. }) if (400..500).contains(&status) && status != 408 && status != 429 => {
            Ok(Some(format!("the {hub} hub refused the {what}: {status}: {body}")))
        }
        Err(e) => Err(DriverError::Unavailable(e.to_string())),
    }
}

impl<S: StaffFeed, L: Letters> Forward for Notices<S, L> {
    async fn forward(&self, seq: u64, effect: &Effect) -> Result<Option<String>, DriverError> {
        let order = |kind: &str, body: Value| (kind.to_string(), body);
        let (kind, body) = match effect {
            Effect::RequestEvidence { request, kind, party } => order(
                "evidence_request",
                json!({"order": "request", "request": request, "kind": kind, "party": party}),
            ),
            Effect::ChaseEvidence { request } => order("evidence_chase", json!({"order": "chase", "request": request})),
            Effect::CancelEvidence { request } => order("evidence_cancel", json!({"order": "cancel", "request": request})),
            Effect::RequestDelegateFile { proposal, life, delegate_ref } => {
                let body = json!({"proposal": proposal, "life": life, "delegate_ref": delegate_ref});
                return publish(&self.delegates, "delegates", "request", &order_id(seq), "delegate_file_request", body).await;
            }
            _ => (String::new(), Value::Null),
        };
        if !kind.is_empty() {
            return publish(&self.evidence, "evidence", "order", &order_id(seq), &kind, body).await;
        }
        let Effect::Notify { to, kind, proposal, life, .. } = effect else {
            return NothingConnected.forward(seq, effect).await;
        };
        let to = match to {
            Recipient::Underwriter { underwriter } => json!({"underwriter": underwriter}),
            Recipient::QueueLead { queue } => json!({"queue_lead": queue}),
            Recipient::UnderwritingManager => json!("underwriting_manager"),
            Recipient::Party { party } => return self.letter(seq, json!({"party": {"party": party}}), effect).await,
            Recipient::Adviser { adviser } => return self.letter(seq, json!({"adviser": {"adviser": adviser}}), effect).await,
        };
        let kind = json!(kind);
        let name = kind.as_str().unwrap_or_default();
        let body = json!({"notice": name, "to": to, "proposal": proposal, "life": life});
        publish(&self.staff, "staff notices", "notice", &notice_id(seq), name, body).await
    }
}
