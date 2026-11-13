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
//! nothing clinical. A refusal parks the letter for a person. Evidence
//! requests and delegate files have no receiver yet and are parked.

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

/// Staff notices to `staff`, letters to `letters`; everything else as with
/// nothing connected.
#[derive(Clone)]
pub struct Notices<S, L> {
    pub staff: S,
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

impl<S: StaffFeed, L: Letters> Forward for Notices<S, L> {
    async fn forward(&self, seq: u64, effect: &Effect) -> Result<Option<String>, DriverError> {
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
        match self.staff.publish(&notice_id(seq), name, body).await {
            Ok(()) => Ok(None),
            // The hub is missing or the driver may not send to it: trying
            // again will not help, and every effect behind would wait.
            Err(AzureError::Status { status, body, .. }) if (400..500).contains(&status) && status != 408 && status != 429 => {
                Ok(Some(format!("the staff notices hub refused the notice: {status}: {body}")))
            }
            Err(e) => Err(DriverError::Unavailable(e.to_string())),
        }
    }
}
