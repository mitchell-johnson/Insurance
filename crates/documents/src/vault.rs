//! The document vault: every document exactly as issued, with the data it
//! was rendered from, encrypted under its own key.
//!
//! An object is written once under a name that includes a random part, so a
//! retry after a crash writes a new object and the one left behind has no
//! key anywhere: it is unreadable and is never referred to.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use ic_azure::credential::{AzureCredential, SCOPE_STORAGE};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::crypto::{self, CryptoError, Key};
use crate::template::sha256_hex;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("the object already exists: {0}")]
    AlreadyExists(String),
    /// Storage still holds the object under a retention lock.
    #[error("the object may not be deleted yet: {0}")]
    Locked(String),
    #[error("storage failed: {0}")]
    Storage(String),
    #[error("the document is not in the vault: {0}")]
    Missing(String),
    #[error("the document has been destroyed")]
    Destroyed,
    #[error("the stored document does not match its recorded hash")]
    Tampered,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// Where the vault keeps its objects.
pub trait ObjectStore: Send + Sync {
    /// Write a new object. `keep_until`, where the retention period is
    /// already known, asks storage itself to refuse deletion before then.
    fn put_new(&self, name: &str, body: Vec<u8>, keep_until: Option<NaiveDate>) -> impl Future<Output = Result<(), VaultError>> + Send;
    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, VaultError>> + Send;
    /// Delete an object. Deleting one that is already gone succeeds.
    fn delete(&self, name: &str) -> impl Future<Output = Result<(), VaultError>> + Send;
}

/// An Azure Blob Storage container with version-level immutability enabled.
#[derive(Clone)]
pub struct BlobVault {
    endpoint: String,
    container: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

const API_VERSION: &str = "2023-11-03";

impl BlobVault {
    pub fn new(endpoint: &str, container: &str, credential: AzureCredential) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            container: container.to_string(),
            credential,
            http: ic_azure::http_client(),
        }
    }

    async fn request(&self, method: reqwest::Method, name: &str) -> Result<reqwest::RequestBuilder, VaultError> {
        let token = self.credential.token(SCOPE_STORAGE).await.map_err(|e| VaultError::Storage(e.to_string()))?;
        Ok(self
            .http
            .request(method, format!("{}/{}/{}", self.endpoint, self.container, name))
            .bearer_auth(token)
            .header("x-ms-version", API_VERSION)
            .header("x-ms-date", chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()))
    }
}

fn storage(e: reqwest::Error) -> VaultError {
    VaultError::Storage(e.to_string())
}

fn refused(resp: &reqwest::Response) -> VaultError {
    VaultError::Storage(format!("Blob Storage returned {}", resp.status().as_u16()))
}

impl ObjectStore for BlobVault {
    async fn put_new(&self, name: &str, body: Vec<u8>, keep_until: Option<NaiveDate>) -> Result<(), VaultError> {
        let mut req = self
            .request(reqwest::Method::PUT, name)
            .await?
            .header("x-ms-blob-type", "BlockBlob")
            .header("if-none-match", "*")
            .header("content-type", "application/octet-stream");
        if let Some(date) = keep_until {
            req = req
                .header("x-ms-immutability-policy-until-date", date.format("%a, %d %b %Y 00:00:00 GMT").to_string())
                .header("x-ms-immutability-policy-mode", "Locked");
        }
        let resp = req.body(body).send().await.map_err(storage)?;
        match resp.status().as_u16() {
            409 | 412 => Err(VaultError::AlreadyExists(name.to_string())),
            s if (200..300).contains(&s) => Ok(()),
            _ => Err(refused(&resp)),
        }
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, VaultError> {
        let resp = self.request(reqwest::Method::GET, name).await?.send().await.map_err(storage)?;
        match resp.status().as_u16() {
            404 => Ok(None),
            s if (200..300).contains(&s) => Ok(Some(resp.bytes().await.map_err(storage)?.to_vec())),
            _ => Err(refused(&resp)),
        }
    }

    async fn delete(&self, name: &str) -> Result<(), VaultError> {
        let resp = self.request(reqwest::Method::DELETE, name).await?.send().await.map_err(storage)?;
        match resp.status().as_u16() {
            404 => Ok(()),
            409 => Err(VaultError::Locked(name.to_string())),
            s if (200..300).contains(&s) => Ok(()),
            _ => Err(refused(&resp)),
        }
    }
}

/// An in-memory store for the insurance in a box and tests.
#[derive(Clone, Default)]
pub struct MemoryStore {
    objects: Arc<Mutex<BTreeMap<String, (Vec<u8>, Option<NaiveDate>)>>>,
    down: Arc<Mutex<bool>>,
}

impl MemoryStore {
    fn objects(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, (Vec<u8>, Option<NaiveDate>)>> {
        self.objects.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn names(&self) -> Vec<String> {
        self.objects().keys().cloned().collect()
    }

    /// The date storage keeps an object until, if one was asked for.
    pub fn kept_until(&self, name: &str) -> Option<NaiveDate> {
        self.objects().get(name).and_then(|o| o.1)
    }

    /// Change stored bytes behind the vault's back, as a fault would.
    pub fn corrupt(&self, name: &str) {
        if let Some(object) = self.objects().get_mut(name) {
            object.0.iter_mut().for_each(|b| *b ^= 0x55);
        }
    }

    /// While down, every call fails as an outage would.
    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }

    fn check(&self) -> Result<(), VaultError> {
        if *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err(VaultError::Storage("storage is unavailable".into()));
        }
        Ok(())
    }
}

impl ObjectStore for MemoryStore {
    async fn put_new(&self, name: &str, body: Vec<u8>, keep_until: Option<NaiveDate>) -> Result<(), VaultError> {
        self.check()?;
        let mut objects = self.objects();
        if objects.contains_key(name) {
            return Err(VaultError::AlreadyExists(name.to_string()));
        }
        objects.insert(name.to_string(), (body, keep_until));
        Ok(())
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, VaultError> {
        self.check()?;
        Ok(self.objects().get(name).map(|o| o.0.clone()))
    }

    async fn delete(&self, name: &str) -> Result<(), VaultError> {
        self.check()?;
        self.objects().remove(name);
        Ok(())
    }
}

/// What the vault holds for one document: the text exactly as issued and
/// the data snapshot it was rendered from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contents {
    pub text: String,
    pub data: Value,
    /// The PDF an external composer set, base64, exactly as issued. None
    /// when the PDF is set from the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<String>,
}

impl Contents {
    /// The issued PDF, if one was stored with the text.
    pub fn issued_pdf(&self) -> Result<Option<Vec<u8>>, VaultError> {
        use base64::Engine;
        self.pdf
            .as_deref()
            .map(|encoded| base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| VaultError::Tampered))
            .transpose()
    }
}

/// The record of a stored document, for the module's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub blob: String,
    /// The document's key, wrapped by its owner's key.
    pub wrapped_key: String,
    pub content_sha256: String,
    pub data_sha256: String,
    pub bytes: u64,
}

/// The canonical bytes of a data snapshot, which its hash is taken over.
pub fn data_bytes(data: &Value) -> Vec<u8> {
    // serde_json keeps object keys sorted, so equal data gives equal bytes.
    serde_json::to_vec(data).unwrap_or_default()
}

/// Encrypt a document under a new key and store it.
pub async fn store<S: ObjectStore>(
    objects: &S,
    owner_key: &Key,
    document: &str,
    contents: &Contents,
    keep_until: Option<NaiveDate>,
) -> Result<Stored, VaultError> {
    let key = crypto::new_key();
    let plain = serde_json::to_vec(contents).map_err(|e| VaultError::Storage(e.to_string()))?;
    let sealed = crypto::seal(&key, document.as_bytes(), &plain)?;
    let blob = format!("documents/{document}/{}", hex::encode(&crypto::new_key()[..8]));
    objects.put_new(&blob, sealed, keep_until).await?;
    Ok(Stored {
        blob,
        wrapped_key: crypto::wrap_under(owner_key, document, &key)?,
        content_sha256: sha256_hex(contents.text.as_bytes()),
        data_sha256: sha256_hex(&data_bytes(&contents.data)),
        bytes: contents.text.len() as u64,
    })
}

/// Fetch and decrypt a document, and check it is what was issued.
pub async fn fetch<S: ObjectStore>(objects: &S, owner_key: &Key, document: &crate::model::Document) -> Result<Contents, VaultError> {
    let wrapped = document.wrapped_key.as_deref().ok_or(VaultError::Destroyed)?;
    let key = crypto::unwrap_under(owner_key, &document.id, wrapped)?;
    let sealed = objects.get(&document.blob).await?.ok_or_else(|| VaultError::Missing(document.id.clone()))?;
    let plain = crypto::open(&key, document.id.as_bytes(), &sealed)?;
    let contents: Contents = serde_json::from_slice(&plain).map_err(|_| VaultError::Tampered)?;
    let data_ok = document.data_sha256.as_ref().is_none_or(|h| *h == sha256_hex(&data_bytes(&contents.data)));
    // A PDF an external composer set is the document as issued too: it
    // must be the one it answered, and there is none where it set none.
    let pdf_ok = match &document.rendition {
        Some(rendition) => contents.issued_pdf()?.map(|pdf| sha256_hex(&pdf)) == rendition.pdf_sha256,
        None => contents.pdf.is_none(),
    };
    if sha256_hex(contents.text.as_bytes()) != document.content_sha256 || !data_ok || !pdf_ok {
        return Err(VaultError::Tampered);
    }
    Ok(contents)
}
