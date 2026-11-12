//! Wrapping data keys with a key that never leaves Azure Key Vault or
//! Managed HSM.
//!
//! Records that must be erasable (clinical records, for one) are sealed
//! under a data key per person. The data key is wrapped here by a
//! key-encryption key held in the vault, and only the wrapped form is
//! stored. Deleting a person's wrapped key makes everything sealed under it
//! unreadable, wherever copies of the ciphertext survive.
//!
//! The vault names the exact key version that wrapped, and that name is
//! kept with the wrapped key, so rotating the key-encryption key never
//! orphans a key wrapped under an older version.

use std::future::Future;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::credential::{AzureCredential, SCOPE_KEY_VAULT, SCOPE_MANAGED_HSM};
use crate::{check, AzureError};

/// A data key as it is stored: wrapped, with the key version that wrapped it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKey {
    /// For example `https://kv-inscore.vault.azure.net/keys/clinical-kek/0123abcd`.
    pub key_id: String,
    /// Base64url of the wrapped key.
    pub value: String,
}

pub trait KeyWrapper: Send + Sync {
    fn wrap(&self, key: &[u8]) -> impl Future<Output = Result<WrappedKey, AzureError>> + Send;
    fn unwrap(&self, wrapped: &WrappedKey) -> impl Future<Output = Result<Vec<u8>, AzureError>> + Send;
}

#[derive(Clone)]
pub struct KeyVaultWrapper {
    /// `https://<name>.vault.azure.net` or `https://<name>.managedhsm.azure.net`.
    vault: String,
    key: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct KeyOperationResponse {
    kid: String,
    value: String,
}

const ALGORITHM: &str = "RSA-OAEP-256";

impl KeyVaultWrapper {
    pub fn new(vault: &str, key: &str, credential: AzureCredential) -> Self {
        Self {
            vault: vault.trim_end_matches('/').to_string(),
            key: key.to_string(),
            credential,
            http: crate::http_client(),
        }
    }

    fn scope(&self) -> &'static str {
        if self.vault.contains(".managedhsm.") {
            SCOPE_MANAGED_HSM
        } else {
            SCOPE_KEY_VAULT
        }
    }

    async fn operate(&self, url: String, value: String) -> Result<KeyOperationResponse, AzureError> {
        let token = self.credential.token(self.scope()).await?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "alg": ALGORITHM, "value": value }))
            .send()
            .await?;
        Ok(check("Key Vault", resp).await?.json().await?)
    }
}

impl KeyWrapper for KeyVaultWrapper {
    async fn wrap(&self, key: &[u8]) -> Result<WrappedKey, AzureError> {
        // No version in the path: the current version wraps, and the response says which.
        let url = format!("{}/keys/{}/wrapkey?api-version=7.4", self.vault, self.key);
        let body = self.operate(url, URL_SAFE_NO_PAD.encode(key)).await?;
        Ok(WrappedKey {
            key_id: body.kid,
            value: body.value,
        })
    }

    async fn unwrap(&self, wrapped: &WrappedKey) -> Result<Vec<u8>, AzureError> {
        // The key name came back from storage. A token is only ever sent to
        // this vault and this key, whatever the stored name says.
        let ours = format!("{}/keys/{}/", self.vault, self.key);
        let version = wrapped.key_id.strip_prefix(&ours).unwrap_or_default();
        if version.is_empty() || !version.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(AzureError::Response(format!("{} is not a version of this vault's key", wrapped.key_id)));
        }
        let url = format!("{ours}{version}/unwrapkey?api-version=7.4");
        let body = self.operate(url, wrapped.value.clone()).await?;
        URL_SAFE_NO_PAD
            .decode(body.value)
            .map_err(|e| AzureError::Response(format!("the unwrapped key is not base64url: {e}")))
    }
}

/// A wrapper for the insurance in a box. It protects nothing: the "wrapped"
/// key is the key, so the rest of the path can be exercised.
#[derive(Clone, Default)]
pub struct DevWrapper;

impl KeyWrapper for DevWrapper {
    async fn wrap(&self, key: &[u8]) -> Result<WrappedKey, AzureError> {
        Ok(WrappedKey {
            key_id: "dev".to_string(),
            value: URL_SAFE_NO_PAD.encode(key),
        })
    }

    async fn unwrap(&self, wrapped: &WrappedKey) -> Result<Vec<u8>, AzureError> {
        URL_SAFE_NO_PAD
            .decode(&wrapped.value)
            .map_err(|e| AzureError::Response(format!("not a wrapped key: {e}")))
    }
}
