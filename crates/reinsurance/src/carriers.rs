//! The carriers for notices that leave the cluster: records, filed in
//! write-once Blob Storage, and notices for staff, published to the module's
//! event hub. Notices for other modules go through whatever carrier is given
//! as `modules`.
//!
//! Both are safe to repeat. A record's name is fixed by the module, so a
//! second try finds the first and compares it. An event carries the notice's
//! number as its ID, and a consumer that sees the number twice drops the
//! second.

use std::future::Future;

use chrono::NaiveDate;
use ic_azure::blob::WormStore;
use ic_azure::eventhubs::EventHubsSink;
use ic_azure::AzureError;
use ic_core::Reject;
use serde_json::{json, Value};

use crate::driver::{Audiences, DriverError, Handed};
use crate::machine::{Audience, Notice};

/// Where notices for staff are published.
pub trait StaffFeed: Send + Sync {
    /// Publish one notice. `id` is the same on every try.
    fn publish(&self, id: &str, kind: &str, body: Value) -> impl Future<Output = Result<(), AzureError>> + Send;
}

/// The module's hub has one partition: its notices are one ordered sequence.
impl StaffFeed for EventHubsSink {
    async fn publish(&self, id: &str, kind: &str, body: Value) -> Result<(), AzureError> {
        self.send_json(0, &[(body, json!({"event-id": id, "kind": kind}))]).await
    }
}

/// Every audience: other modules through `modules`, records in `records`,
/// staff through `staff`.
#[derive(Clone)]
pub struct Carriers<A, W, S> {
    pub modules: A,
    pub records: W,
    pub staff: S,
}

/// What a notice is called on the wire: `exception_opened`, `document`, …
pub fn kind(notice: &Notice) -> String {
    let named = serde_json::to_value(notice).unwrap_or_default();
    named.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default()
}

/// The name of the record of a release of information to a reinsurer.
pub fn release_record(seq: u64) -> String {
    format!("reinsurance/releases/{seq:012}.json")
}

fn away(e: AzureError) -> DriverError {
    DriverError::Unavailable(e.to_string())
}

/// File one record. A name already written is fine when it holds the same
/// thing, and is refused, for a person, when it holds something else: the
/// store never replaces what it has.
async fn file<W: WormStore>(records: &W, seq: u64, notice: &Notice, on: NaiveDate) -> Result<Handed, DriverError> {
    let (name, body, same): (String, Vec<u8>, Box<dyn Fn(&[u8]) -> bool + Send>) = match notice {
        Notice::Document { name, sha256, body } => {
            if crate::account::sha256_hex(body) != *sha256 {
                return Ok(Handed::Refused(Reject::new(
                    "record_damaged",
                    format!("{name} does not have the hash it was issued with"),
                )));
            }
            let bytes = body.clone().into_bytes();
            let ours = bytes.clone();
            (name.clone(), bytes, Box::new(move |kept| kept == ours))
        }
        release => {
            // The date it was filed is of the first try; the release itself is what must agree.
            let ours = json!(release);
            let body = json!({"seq": seq, "filed_on": on, "release": ours});
            (
                release_record(seq),
                body.to_string().into_bytes(),
                Box::new(move |kept| serde_json::from_slice::<Value>(kept).is_ok_and(|kept| kept["release"] == ours)),
            )
        }
    };
    match records.put_new(&name, "application/json", body).await {
        Ok(()) => Ok(Handed::Taken),
        Err(AzureError::AlreadyExists(_)) => match records.get(&name).await.map_err(away)? {
            Some(kept) if same(&kept) => Ok(Handed::Taken),
            Some(_) => Ok(Handed::Refused(Reject::new(
                "record_conflict",
                format!("{name} is already filed with other contents"),
            ))),
            None => Err(DriverError::Unavailable(format!("{name} exists but cannot be read"))),
        },
        Err(e) => Err(away(e)),
    }
}

impl<A: Audiences, W: WormStore, S: StaffFeed> Audiences for Carriers<A, W, S> {
    fn reached(&self) -> Vec<Audience> {
        let mut reached = self.modules.reached();
        reached.extend([Audience::Records, Audience::Staff]);
        reached
    }

    async fn hand_on(&self, seq: u64, notice: &Notice, on: NaiveDate) -> Result<Handed, DriverError> {
        match notice.audience() {
            Audience::Records => file(&self.records, seq, notice, on).await,
            Audience::Staff => {
                let kind = kind(notice);
                let body = json!({"seq": seq, "on": on, "notice": notice});
                self.staff.publish(&format!("reinsurance-notice-{seq}"), &kind, body).await.map_err(away)?;
                // The same line in the log, where the alerting rules read it.
                tracing::warn!(target: "reinsurance.notice", seq, kind, notice = %json!(notice), "for reinsurance operations");
                Ok(Handed::Taken)
            }
            _ => self.modules.hand_on(seq, notice, on).await,
        }
    }
}
