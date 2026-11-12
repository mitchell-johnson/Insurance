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
//! module renders and delivers; until it has a door for them they are parked
//! with the reason, as are evidence requests and delegate files.

use std::future::Future;

use ic_azure::eventhubs::EventHubsSink;
use ic_azure::AzureError;
use serde_json::{json, Value};

use crate::command::{Effect, Recipient};
use crate::driver::{DriverError, Forward, NothingConnected};

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

/// Staff notices to `staff`; everything else as with nothing connected.
#[derive(Clone)]
pub struct StaffNotices<S> {
    pub staff: S,
}

impl<S: StaffFeed> Forward for StaffNotices<S> {
    async fn forward(&self, seq: u64, effect: &Effect) -> Result<Option<String>, DriverError> {
        let Effect::Notify { to, kind, proposal, life, .. } = effect else {
            return NothingConnected.forward(seq, effect).await;
        };
        let to = match to {
            Recipient::Underwriter { underwriter } => json!({"underwriter": underwriter}),
            Recipient::QueueLead { queue } => json!({"queue_lead": queue}),
            Recipient::UnderwritingManager => json!("underwriting_manager"),
            Recipient::Party { .. } | Recipient::Adviser { .. } => return NothingConnected.forward(seq, effect).await,
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
