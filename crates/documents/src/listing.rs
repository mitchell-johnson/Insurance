//! Each document on a policy as a row in the channels' read model, so that
//! the customer app and the adviser and employer portals can list it.
//!
//! A module cannot write to another, so the documents module keeps a list
//! of documents whose row is to be written or taken down, and this step
//! carries each to the channel module as `publish_rows` under module name
//! `documents` (keys `{policy}/documents/{document}`), then marks it done.
//! Who sees a row is the channels' audience policy; downloading the PDF is
//! still decided here, by the owner rule (`service.rs`).

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ic_raft::ClusterClient;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::channels::{Channels, ContactDirectory};
use crate::command::Command;
use crate::crypto::KeyWrap;
use crate::driver::{Driver, DriverError, Module};
use crate::kinds::DataClass;
use crate::model::Document;
use crate::module::Query;
use crate::vault::ObjectStore;

/// The name the rows are written under.
pub const MODULE: &str = "documents";

/// The channel module's refusal.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RowsRefused {
    pub code: String,
    pub message: String,
}

/// The channel module, wherever it runs.
pub trait RowSink: Send + Sync {
    /// Apply one `publish_rows` envelope; the inner result is the module's
    /// own answer.
    fn publish(&self, envelope: Value) -> impl Future<Output = Result<Result<(), RowsRefused>, DriverError>> + Send;
}

/// The channel module on the cluster.
#[derive(Clone)]
pub struct ClusterRows {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterRows {
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

#[derive(Deserialize)]
struct Answer {
    result: Result<serde::de::IgnoredAny, RowsRefused>,
}

impl RowSink for ClusterRows {
    async fn publish(&self, envelope: Value) -> Result<Result<(), RowsRefused>, DriverError> {
        let reply = self
            .client
            .propose_module_until("channels", &envelope, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let output = reply.result.map_err(DriverError::Answer)?;
        let answer: Answer = serde_json::from_value(output).map_err(|e| DriverError::Answer(e.to_string()))?;
        Ok(answer.result.map(|_| ()))
    }
}

/// The key of a document's row on one of its policies.
pub fn row_key(policy: &str, document: &str) -> String {
    format!("{policy}/{MODULE}/{document}")
}

/// The rows a document is shown as, one for each policy it covers.
pub fn rows(document: &Document, title: &str) -> Vec<Value> {
    let sensitive = matches!(document.data_class, DataClass::InsuranceSensitive | DataClass::Clinical);
    document
        .policies()
        .map(|policy| {
            json!({
                "key": row_key(&policy.0, &document.id),
                "policy": policy,
                "kind": "document",
                "subject": document.owner,
                "sensitive": sensitive,
                "body": {
                    "document": document.id,
                    "title": title,
                    "kind": document.kind,
                    "issued_on": document.issued_on,
                    "language": document.language,
                    "supersedes": document.supersedes,
                },
            })
        })
        .collect()
}

/// The envelope the channel module reads: rows written and keys removed by
/// module `documents`.
pub fn envelope(key: &str, at: DateTime<Utc>, rows: Vec<Value>, remove: Vec<String>) -> Value {
    json!({
        "key": key,
        "at": at.timestamp(),
        "actor": { "kind": "module", "id": MODULE },
        "command": { "publish_rows": { "rows": rows, "remove": remove } },
    })
}

impl<M: Module, S: ObjectStore, W: KeyWrap, C: Channels, D: ContactDirectory> Driver<M, S, W, C, D> {
    async fn listed(&self, sink: &impl RowSink, id: &str, listed: bool) -> Result<(), DriverError> {
        let document = self.document(id).await?;
        let (rows, remove) = if listed {
            let title = match &document.template {
                Some(r) => self.template(r).await?.0.template.title,
                None => "Your document".to_string(),
            };
            (rows(&document, &title), vec![])
        } else {
            (vec![], document.policies().map(|p| row_key(&p.0, id)).collect())
        };
        let key = format!("documents-rows:{id}:{}", if listed { "publish" } else { "remove" });
        sink.publish(envelope(&key, (self.clock)(), rows, remove))
            .await?
            .map_err(|r| DriverError::Refused(ic_core::Reject::new(&r.code, r.message)))
    }

    /// Carry the waiting row changes to the channel module. Returns how
    /// many were carried; one that fails is reported and tried next time.
    pub async fn list_documents(&self, sink: &impl RowSink) -> Result<u32, DriverError> {
        let due: Vec<(String, bool)> =
            serde_json::from_value(self.module.ask(Query::Listing { limit: self.batch }).await?).map_err(|e| DriverError::Answer(e.to_string()))?;
        let mut done = Vec::new();
        let mut first_error = None;
        for (id, listed) in due {
            match self.listed(sink, &id, listed).await {
                Ok(()) => done.push((id, listed)),
                Err(e) => {
                    tracing::warn!(document = %id, error = %e, "a document's channel row could not be written");
                    first_error.get_or_insert(e);
                }
            }
        }
        let carried = done.len() as u32;
        if !done.is_empty() {
            let key = format!("listing:{}", (self.clock)().timestamp_nanos_opt().unwrap_or_default());
            self.send(key, "relay", Command::AckListing { documents: done }).await?;
        }
        match first_error {
            Some(e) if carried == 0 => Err(e),
            _ => Ok(carried),
        }
    }
}

/// Run the step every `every` until `stop` completes.
pub async fn run_listing<M, S, W, C, D, R>(
    driver: std::sync::Arc<Driver<M, S, W, C, D>>,
    sink: R,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&Result<u32, DriverError>),
) where
    M: Module,
    S: ObjectStore,
    W: KeyWrap,
    C: Channels,
    D: ContactDirectory,
    R: RowSink,
{
    tokio::pin!(stop);
    loop {
        let carried = driver.list_documents(&sink).await;
        if let Err(e) = &carried {
            tracing::warn!(error = %e, "the documents' channel rows could not be carried");
        }
        seen(&carried);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}
