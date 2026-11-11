//! Signing with a key that never leaves Azure Key Vault or Managed HSM.
//!
//! The day's Merkle root is signed with an HSM-backed elliptic curve key
//! (`EC-HSM`, P-256). The service signs a digest; the private key cannot be
//! exported. The response names the exact key version, which is recorded
//! with the signature so rotation never orphans an old root.

use std::future::Future;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::credential::{AzureCredential, SCOPE_KEY_VAULT, SCOPE_MANAGED_HSM};
use crate::{check, AzureError};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct Signature {
    pub algorithm: String,
    /// The key and version that signed, for example
    /// `https://kv-inscore.vault.azure.net/keys/merkle-root/0123abcd`.
    pub key_id: String,
    /// Base64url signature bytes.
    pub value: String,
}

pub trait RootSigner: Send + Sync {
    /// Sign the SHA-256 digest of `message`.
    fn sign(&self, message: &[u8]) -> impl Future<Output = Result<Signature, AzureError>> + Send;
}

#[derive(Clone)]
pub struct KeyVaultSigner {
    /// `https://<name>.vault.azure.net` or `https://<name>.managedhsm.azure.net`.
    vault: String,
    key: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct SignResponse {
    kid: String,
    value: String,
}

impl KeyVaultSigner {
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
}

impl RootSigner for KeyVaultSigner {
    async fn sign(&self, message: &[u8]) -> Result<Signature, AzureError> {
        let token = self.credential.token(self.scope()).await?;
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(message));
        // No version in the path: the current version signs, and the response says which.
        let url = format!("{}/keys/{}/sign?api-version=7.4", self.vault, self.key);
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "alg": "ES256", "value": digest }))
            .send()
            .await?;
        let body: SignResponse = check("Key Vault", resp).await?.json().await?;
        Ok(Signature {
            algorithm: "ES256".to_string(),
            key_id: body.kid,
            value: body.value,
        })
    }
}

/// A signer for the insurance in a box. It proves nothing: it only fills the
/// signature field so the rest of the path can be exercised.
#[derive(Clone, Default)]
pub struct DevSigner;

impl RootSigner for DevSigner {
    async fn sign(&self, message: &[u8]) -> Result<Signature, AzureError> {
        Ok(Signature {
            algorithm: "none-dev".to_string(),
            key_id: "dev".to_string(),
            value: URL_SAFE_NO_PAD.encode(Sha256::digest(message)),
        })
    }
}
