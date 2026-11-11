//! Write-once object storage on Azure Blob Storage.
//!
//! The containers are created (see `infra/`) with version-level immutability
//! and a locked time-based retention policy, so a blob written here cannot be
//! changed or deleted by anyone, including an account owner, until its
//! retention ends. On top of that every write uses `If-None-Match: *`, so a
//! name is only ever written once.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::credential::{AzureCredential, SCOPE_STORAGE};
use crate::{check, AzureError};

/// Write-once storage outside the core.
pub trait WormStore: Send + Sync {
    /// Write a new object. Fails with [`AzureError::AlreadyExists`] if the
    /// name has been written before.
    fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> impl Future<Output = Result<(), AzureError>> + Send;
    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, AzureError>> + Send;
}

#[derive(Clone)]
pub struct BlobContainer {
    /// For example `https://stinscoreledger.blob.core.windows.net`.
    endpoint: String,
    container: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

const API_VERSION: &str = "2023-11-03";

impl BlobContainer {
    pub fn new(endpoint: &str, container: &str, credential: AzureCredential) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            container: container.to_string(),
            credential,
            http: reqwest::Client::new(),
        }
    }

    fn url(&self, name: &str) -> String {
        format!("{}/{}/{}", self.endpoint, self.container, name)
    }

    fn date() -> String {
        chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }
}

impl WormStore for BlobContainer {
    async fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        let token = self.credential.token(SCOPE_STORAGE).await?;
        let resp = self
            .http
            .put(self.url(name))
            .bearer_auth(token)
            .header("x-ms-version", API_VERSION)
            .header("x-ms-date", Self::date())
            .header("x-ms-blob-type", "BlockBlob")
            .header("if-none-match", "*")
            .header("content-type", content_type)
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
}

/// In-memory write-once store for the insurance in a box and tests.
#[derive(Clone, Default)]
pub struct MemoryWormStore {
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl MemoryWormStore {
    pub fn names(&self) -> Vec<String> {
        self.objects.lock().map(|o| o.keys().cloned().collect()).unwrap_or_default()
    }
}

impl WormStore for MemoryWormStore {
    async fn put_new(&self, name: &str, _content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        let mut o = self.objects.lock().map_err(|_| AzureError::Response("store poisoned".to_string()))?;
        if o.contains_key(name) {
            return Err(AzureError::AlreadyExists(name.to_string()));
        }
        o.insert(name.to_string(), body);
        Ok(())
    }
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        Ok(self
            .objects
            .lock()
            .map_err(|_| AzureError::Response("store poisoned".to_string()))?
            .get(name)
            .cloned())
    }
}

/// A write-once store in a directory. An object, once written, is never
/// replaced: the second writer of a name is refused.
#[derive(Debug, Clone)]
pub struct DirWormStore {
    root: std::path::PathBuf,
}

impl DirWormStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, name: &str) -> Result<std::path::PathBuf, AzureError> {
        let relative = std::path::Path::new(name);
        // Names come from the code, not from callers; refuse anything that
        // could climb out of the directory all the same.
        if relative.is_absolute() || relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return Err(AzureError::Response(format!("not a store name: {name}")));
        }
        Ok(self.root.join(relative))
    }
}

impl WormStore for DirWormStore {
    async fn put_new(&self, name: &str, _content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        let path = self.path(name)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write the whole object beside its name, then link it into place.
        // The link fails if the name exists, so a reader never sees half an
        // object and a second writer never replaces the first.
        let staged = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = std::fs::File::create(&staged)?;
        std::io::Write::write_all(&mut file, &body)?;
        file.sync_all()?;
        let linked = std::fs::hard_link(&staged, &path);
        let _ = std::fs::remove_file(&staged);
        match linked {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(AzureError::AlreadyExists(name.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        match std::fs::read(self.path(name)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
