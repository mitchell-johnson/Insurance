//! Erasable object storage on Azure Blob Storage.
//!
//! The write-once containers in [`crate::blob`] cannot be used for anything
//! a person has the right to have erased. This adapter is for a container
//! with no immutability policy, no soft delete and no versioning (see
//! `infra/`): a name is still written only once, but it can be deleted, and
//! a deleted blob is gone.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::credential::{AzureCredential, SCOPE_STORAGE};
use crate::{check, AzureError};

/// Storage for records that are written once and can be erased.
pub trait RecordStore: Send + Sync {
    /// Write a new object. Fails with [`AzureError::AlreadyExists`] if the
    /// name is in use.
    fn put_new(&self, name: &str, body: Vec<u8>) -> impl Future<Output = Result<(), AzureError>> + Send;
    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, AzureError>> + Send;
    /// Delete an object. Deleting one that is not there succeeds, so an
    /// erasure can be retried.
    fn delete(&self, name: &str) -> impl Future<Output = Result<(), AzureError>> + Send;
}

#[derive(Clone)]
pub struct BlobRecords {
    /// For example `https://stinscoreclinical.blob.core.windows.net`.
    endpoint: String,
    container: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

const API_VERSION: &str = "2023-11-03";

impl BlobRecords {
    pub fn new(endpoint: &str, container: &str, credential: AzureCredential) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            container: container.to_string(),
            credential,
            http: crate::http_client(),
        }
    }

    fn url(&self, name: &str) -> String {
        format!("{}/{}/{}", self.endpoint, self.container, name)
    }

    fn date() -> String {
        chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }
}

impl RecordStore for BlobRecords {
    async fn put_new(&self, name: &str, body: Vec<u8>) -> Result<(), AzureError> {
        let token = self.credential.token(SCOPE_STORAGE).await?;
        let resp = self
            .http
            .put(self.url(name))
            .bearer_auth(token)
            .header("x-ms-version", API_VERSION)
            .header("x-ms-date", Self::date())
            .header("x-ms-blob-type", "BlockBlob")
            .header("if-none-match", "*")
            .header("content-type", "application/octet-stream")
            .body(body)
            .send()
            .await?;
        if matches!(resp.status().as_u16(), 409 | 412) {
            return Err(AzureError::AlreadyExists(name.to_string()));
        }
        check("Blob Storage", resp).await?;
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        let token = self.credential.token(SCOPE_STORAGE).await?;
        let resp = self
            .http
            .get(self.url(name))
            .bearer_auth(token)
            .header("x-ms-version", API_VERSION)
            .header("x-ms-date", Self::date())
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        Ok(Some(check("Blob Storage", resp).await?.bytes().await?.to_vec()))
    }

    async fn delete(&self, name: &str) -> Result<(), AzureError> {
        let token = self.credential.token(SCOPE_STORAGE).await?;
        let resp = self
            .http
            .delete(self.url(name))
            .bearer_auth(token)
            .header("x-ms-version", API_VERSION)
            .header("x-ms-date", Self::date())
            // Snapshots, if any were ever taken, go with the blob.
            .header("x-ms-delete-snapshots", "include")
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        check("Blob Storage", resp).await?;
        Ok(())
    }
}

/// In-memory erasable store for the insurance in a box and tests. It can be
/// switched off to play an outage.
#[derive(Clone, Default)]
pub struct MemoryRecords {
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    down: Arc<std::sync::atomic::AtomicBool>,
}

impl MemoryRecords {
    pub fn names(&self) -> Vec<String> {
        self.objects.lock().map(|o| o.keys().cloned().collect()).unwrap_or_default()
    }

    /// Replace what is stored under a name, as someone with access to the
    /// storage account could.
    pub fn overwrite(&self, name: &str, body: Vec<u8>) {
        if let Ok(mut o) = self.objects.lock() {
            o.insert(name.to_string(), body);
        }
    }

    pub fn set_down(&self, down: bool) {
        self.down.store(down, std::sync::atomic::Ordering::SeqCst);
    }

    fn objects(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>>, AzureError> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AzureError::Status {
                service: "Blob Storage",
                status: 503,
                body: "unavailable".to_string(),
            });
        }
        self.objects.lock().map_err(|_| AzureError::Response("store poisoned".to_string()))
    }
}

impl RecordStore for MemoryRecords {
    async fn put_new(&self, name: &str, body: Vec<u8>) -> Result<(), AzureError> {
        let mut o = self.objects()?;
        if o.contains_key(name) {
            return Err(AzureError::AlreadyExists(name.to_string()));
        }
        o.insert(name.to_string(), body);
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        Ok(self.objects()?.get(name).cloned())
    }

    async fn delete(&self, name: &str) -> Result<(), AzureError> {
        self.objects()?.remove(name);
        Ok(())
    }
}
