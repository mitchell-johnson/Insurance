//! Encryption of documents at rest.
//!
//! Every document has its own key. That key is wrapped by its owner's key,
//! and the owner's key is wrapped by a key that never leaves Azure Key
//! Vault. Destroying a document is dropping its wrapped key; erasing a
//! person is dropping theirs. What is left in storage cannot be read by
//! anyone.

use std::future::Future;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ic_azure::credential::{AzureCredential, SCOPE_KEY_VAULT, SCOPE_MANAGED_HSM};
use rand::RngCore;
use serde::Deserialize;

pub const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The key is wrong, or the bytes were changed after they were sealed.
    #[error("the data could not be opened with this key")]
    Unreadable,
    #[error("a key is {KEY_BYTES} bytes")]
    BadKey,
    #[error("the key service failed: {0}")]
    KeyService(String),
}

pub type Key = [u8; KEY_BYTES];

/// A new random key.
pub fn new_key() -> Key {
    let mut key = [0u8; KEY_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

fn cipher(key: &[u8]) -> Result<Aes256Gcm, CryptoError> {
    Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::BadKey)
}

/// Encrypt with AES-256-GCM under a fresh nonce. `context` is bound to the
/// result: bytes sealed for one document cannot be opened as another's.
pub fn seal(key: &[u8], context: &[u8], plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let sealed = cipher(key)?
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad: context })
        .map_err(|_| CryptoError::Unreadable)?;
    Ok([nonce.as_slice(), &sealed].concat())
}

pub fn open(key: &[u8], context: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < NONCE_BYTES {
        return Err(CryptoError::Unreadable);
    }
    let (nonce, body) = sealed.split_at(NONCE_BYTES);
    cipher(key)?
        .decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad: context })
        .map_err(|_| CryptoError::Unreadable)
}

/// Wrap a document's key under its owner's key, as text for the state.
pub fn wrap_under(owner: &[u8], context: &str, key: &Key) -> Result<String, CryptoError> {
    Ok(STANDARD.encode(seal(owner, context.as_bytes(), key)?))
}

pub fn unwrap_under(owner: &[u8], context: &str, wrapped: &str) -> Result<Key, CryptoError> {
    let sealed = STANDARD.decode(wrapped).map_err(|_| CryptoError::Unreadable)?;
    open(owner, context.as_bytes(), &sealed)?.try_into().map_err(|_| CryptoError::BadKey)
}

/// The key that wraps every owner's key. In production it is in Key Vault
/// or Managed HSM and never leaves it.
pub trait KeyWrap: Send + Sync {
    fn wrap(&self, key: &Key) -> impl Future<Output = Result<String, CryptoError>> + Send;
    fn unwrap(&self, wrapped: &str) -> impl Future<Output = Result<Key, CryptoError>> + Send;
}

/// Azure Key Vault `wrapKey` and `unwrapKey` with RSA-OAEP-256.
#[derive(Clone)]
pub struct KeyVaultWrap {
    vault: String,
    key: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct KeyOperation {
    #[serde(default)]
    kid: String,
    value: String,
}

impl KeyVaultWrap {
    /// `vault` is `https://<name>.vault.azure.net` or a Managed HSM address.
    pub fn new(vault: &str, key: &str, credential: AzureCredential) -> Self {
        Self {
            vault: vault.trim_end_matches('/').to_string(),
            key: key.to_string(),
            credential,
            http: ic_azure::http_client(),
        }
    }

    fn scope(&self) -> &'static str {
        if self.vault.contains(".managedhsm.") {
            SCOPE_MANAGED_HSM
        } else {
            SCOPE_KEY_VAULT
        }
    }

    async fn call(&self, url: String, value: String) -> Result<KeyOperation, CryptoError> {
        let failed = |e: String| CryptoError::KeyService(e);
        let token = self.credential.token(self.scope()).await.map_err(|e| failed(e.to_string()))?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "alg": "RSA-OAEP-256", "value": value }))
            .send()
            .await
            .map_err(|e| failed(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(failed(format!("Key Vault returned {}", resp.status().as_u16())));
        }
        resp.json().await.map_err(|e| failed(e.to_string()))
    }
}

impl KeyWrap for KeyVaultWrap {
    /// The result names the key version that wrapped it, so it can still be
    /// unwrapped after the key is rotated.
    async fn wrap(&self, key: &Key) -> Result<String, CryptoError> {
        let url = format!("{}/keys/{}/wrapkey?api-version=7.4", self.vault, self.key);
        let done = self.call(url, URL_SAFE_NO_PAD.encode(key)).await?;
        if done.kid.is_empty() || done.kid.contains(' ') {
            return Err(CryptoError::KeyService("the response named no key version".into()));
        }
        Ok(format!("{} {}", done.kid, done.value))
    }

    async fn unwrap(&self, wrapped: &str) -> Result<Key, CryptoError> {
        let (kid, value) = wrapped.split_once(' ').ok_or(CryptoError::Unreadable)?;
        // Only ever call the vault this service is configured for.
        if !kid.starts_with(&format!("{}/keys/", self.vault)) {
            return Err(CryptoError::Unreadable);
        }
        let done = self.call(format!("{kid}/unwrapkey?api-version=7.4"), value.to_string()).await?;
        URL_SAFE_NO_PAD
            .decode(done.value)
            .map_err(|_| CryptoError::Unreadable)?
            .try_into()
            .map_err(|_| CryptoError::BadKey)
    }
}

/// A local wrapping key for the insurance in a box and tests. The key is
/// in this process's memory, which is exactly what production must not do.
#[derive(Clone)]
pub struct LocalWrap {
    key: Key,
}

impl LocalWrap {
    pub fn new(key: Key) -> Self {
        Self { key }
    }
}

impl KeyWrap for LocalWrap {
    async fn wrap(&self, key: &Key) -> Result<String, CryptoError> {
        wrap_under(&self.key, "owner-key", key)
    }

    async fn unwrap(&self, wrapped: &str) -> Result<Key, CryptoError> {
        unwrap_under(&self.key, "owner-key", wrapped)
    }
}
