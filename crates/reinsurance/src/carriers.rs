//! The carriers for notices that leave the cluster: records, filed in
//! write-once Blob Storage, and notices for staff, published to the module's
//! event hub. Notices for other modules go through whatever carrier is given
//! as `modules`.
//!
//! Both are safe to repeat. A record's name is fixed by the module, so a
//! second try finds the first and compares it. An event carries the notice's
//! number as its ID, and a consumer that sees the number twice drops the
//! second.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use ic_azure::blob::{BlobContainer, MemoryWormStore, WormStore};
use ic_azure::credential::AzureCredential;
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

/// Where each reinsurer collects its statements and listings: a container
/// of its own, which it reads over SFTP and nobody else's can.
pub trait Outbound: Send + Sync {
    type Store: WormStore;
    /// The reinsurer's container, or `None` when statements are not left
    /// for reinsurers at all.
    fn drop_for(&self, container: &str) -> Option<Self::Store>;
}

/// The reinsurers' containers in the SFTP storage account, or none.
#[derive(Clone)]
pub enum ReinsurerDrops {
    Off,
    Azure { endpoint: String, credential: AzureCredential },
}

impl Outbound for ReinsurerDrops {
    type Store = BlobContainer;
    fn drop_for(&self, container: &str) -> Option<BlobContainer> {
        match self {
            ReinsurerDrops::Off => None,
            ReinsurerDrops::Azure { endpoint, credential } => Some(BlobContainer::new(endpoint, container, credential.clone())),
        }
    }
}

/// Reinsurers' containers in memory, for tests and the insurance in a box.
#[derive(Clone, Default)]
pub struct MemoryDrops {
    stores: Arc<Mutex<BTreeMap<String, MemoryWormStore>>>,
}

impl MemoryDrops {
    /// The container, created empty when first asked for.
    pub fn store(&self, container: &str) -> MemoryWormStore {
        self.stores
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(container.to_string())
            .or_default()
            .clone()
    }
}

impl Outbound for MemoryDrops {
    type Store = MemoryWormStore;
    fn drop_for(&self, container: &str) -> Option<MemoryWormStore> {
        Some(self.store(container))
    }
}

/// The container a reinsurer collects from: `ri-` and its ID in lower case,
/// with underscores as hyphens. `None` when that is not a valid container
/// name, which the reinsurer's ID must then be changed to allow.
pub fn drop_container(reinsurer: &str) -> Option<String> {
    let name = format!("ri-{}", reinsurer.to_ascii_lowercase().replace('_', "-"));
    let ok = name.len() <= 63 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !name.contains("--") && !name.ends_with('-');
    ok.then_some(name)
}

/// Every audience: other modules through `modules`, records in `records`,
/// staff through `staff`, and each statement and listing also left for its
/// reinsurer in `outbound`.
#[derive(Clone)]
pub struct Carriers<A, W, S, O = ReinsurerDrops> {
    pub modules: A,
    pub records: W,
    pub staff: S,
    pub outbound: O,
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
    let (name, body, same): (String, Vec<u8>, Box<dyn Fn(&[u8]) -> bool + Send + Sync>) = match notice {
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
    put_once(records, &name, body, same.as_ref()).await
}

/// Write one object that must never change once written.
async fn put_once<W: WormStore>(store: &W, name: &str, body: Vec<u8>, same: &(dyn Fn(&[u8]) -> bool + Send + Sync)) -> Result<Handed, DriverError> {
    match store.put_new(name, "application/json", body).await {
        Ok(()) => Ok(Handed::Taken),
        Err(AzureError::AlreadyExists(_)) => match store.get(name).await.map_err(away)? {
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

/// Leave a filed statement or listing (`reinsurance/<treaty>/<reinsurer>/
/// statement-…`) in its reinsurer's container as `<treaty>/<file>`. Other
/// records stay with the insurer.
async fn leave_for_reinsurer<O: Outbound>(outbound: &O, notice: &Notice) -> Result<Handed, DriverError> {
    let Notice::Document { name, body, .. } = notice else {
        return Ok(Handed::Taken);
    };
    let parts: Vec<&str> = name.split('/').collect();
    let [_, treaty, reinsurer, file] = parts[..] else { return Ok(Handed::Taken) };
    if !file.starts_with("statement-") {
        return Ok(Handed::Taken);
    }
    let Some(container) = drop_container(reinsurer) else {
        return Ok(Handed::Refused(Reject::new(
            "no_drop_for_reinsurer",
            format!("reinsurer {reinsurer} has no valid container name for collecting statements"),
        )));
    };
    let Some(store) = outbound.drop_for(&container) else {
        return Ok(Handed::Taken);
    };
    let bytes = body.clone().into_bytes();
    let ours = bytes.clone();
    put_once(&store, &format!("{treaty}/{file}"), bytes, &move |kept: &[u8]| kept == ours).await
}

impl<A: Audiences, W: WormStore, S: StaffFeed, O: Outbound> Audiences for Carriers<A, W, S, O> {
    fn reached(&self) -> Vec<Audience> {
        let mut reached = self.modules.reached();
        reached.extend([Audience::Records, Audience::Staff]);
        reached
    }

    async fn hand_on(&self, seq: u64, notice: &Notice, on: NaiveDate) -> Result<Handed, DriverError> {
        match notice.audience() {
            Audience::Records => match file(&self.records, seq, notice, on).await? {
                Handed::Taken => leave_for_reinsurer(&self.outbound, notice).await,
                refused => Ok(refused),
            },
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
