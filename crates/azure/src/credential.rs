//! Microsoft Entra ID tokens without secrets.
//!
//! On AKS the pod's service account is federated to a user-assigned managed
//! identity (Entra Workload ID): the kubelet projects a short-lived token
//! into the pod, and it is exchanged for an access token. Outside AKS the
//! instance metadata service provides a managed identity token.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::{check, AzureError};

pub const SCOPE_STORAGE: &str = "https://storage.azure.com/.default";
pub const SCOPE_KEY_VAULT: &str = "https://vault.azure.net/.default";
pub const SCOPE_MANAGED_HSM: &str = "https://managedhsm.azure.net/.default";
pub const SCOPE_EVENT_HUBS: &str = "https://eventhubs.azure.net/.default";
pub const SCOPE_POSTGRES: &str = "https://ossrdbms-aad.database.windows.net/.default";

#[derive(Debug, Clone)]
enum Source {
    /// Entra Workload ID: a federated token file exchanged at the authority.
    WorkloadIdentity {
        authority: String,
        tenant: String,
        client_id: String,
        token_file: String,
    },
    /// The instance metadata service.
    ManagedIdentity { endpoint: String, client_id: Option<String> },
    /// A fixed token, for tests and local emulators.
    Static(String),
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(deserialize_with = "seconds")]
    expires_in: u64,
}

// The token endpoint returns a number; the metadata service returns a string.
fn seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        Num(u64),
        Str(String),
    }
    match N::deserialize(d)? {
        N::Num(n) => Ok(n),
        N::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

/// A credential that caches tokens per scope and refreshes them before expiry.
#[derive(Clone)]
pub struct AzureCredential {
    source: Source,
    http: reqwest::Client,
    cache: Arc<Mutex<BTreeMap<String, (String, Instant)>>>,
}

impl AzureCredential {
    /// Workload identity if the AKS webhook has injected its variables,
    /// otherwise the managed identity endpoint.
    pub fn from_env() -> Result<Self, AzureError> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let source = match (var("AZURE_FEDERATED_TOKEN_FILE"), var("AZURE_TENANT_ID"), var("AZURE_CLIENT_ID")) {
            (Some(token_file), Some(tenant), Some(client_id)) => Source::WorkloadIdentity {
                authority: var("AZURE_AUTHORITY_HOST").unwrap_or_else(|| "https://login.microsoftonline.com/".to_string()),
                tenant,
                client_id,
                token_file,
            },
            (None, _, client_id) => Source::ManagedIdentity {
                endpoint: "http://169.254.169.254/metadata/identity/oauth2/token".to_string(),
                client_id,
            },
            _ => {
                return Err(AzureError::NoCredential(
                    "AZURE_FEDERATED_TOKEN_FILE is set but AZURE_TENANT_ID or AZURE_CLIENT_ID is missing".to_string(),
                ))
            }
        };
        Ok(Self::new(source))
    }

    pub fn workload_identity(authority: &str, tenant: &str, client_id: &str, token_file: &str) -> Self {
        Self::new(Source::WorkloadIdentity {
            authority: authority.to_string(),
            tenant: tenant.to_string(),
            client_id: client_id.to_string(),
            token_file: token_file.to_string(),
        })
    }

    pub fn static_token(token: &str) -> Self {
        Self::new(Source::Static(token.to_string()))
    }

    fn new(source: Source) -> Self {
        Self {
            source,
            http: reqwest::Client::new(),
            cache: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// A bearer token for a scope such as [`SCOPE_STORAGE`].
    pub async fn token(&self, scope: &str) -> Result<String, AzureError> {
        let mut cache = self.cache.lock().await;
        if let Some((token, expires)) = cache.get(scope) {
            if Instant::now() + Duration::from_secs(300) < *expires {
                return Ok(token.clone());
            }
        }
        let t = self.fetch(scope).await?;
        cache.insert(scope.to_string(), (t.access_token.clone(), Instant::now() + Duration::from_secs(t.expires_in)));
        Ok(t.access_token)
    }

    async fn fetch(&self, scope: &str) -> Result<TokenResponse, AzureError> {
        match &self.source {
            Source::Static(t) => Ok(TokenResponse {
                access_token: t.clone(),
                expires_in: 3600,
            }),
            Source::WorkloadIdentity {
                authority,
                tenant,
                client_id,
                token_file,
            } => {
                // Read the projected token every time: the kubelet rotates it.
                let assertion = tokio::fs::read_to_string(token_file).await?;
                let url = format!("{}/{tenant}/oauth2/v2.0/token", authority.trim_end_matches('/'));
                let form = [
                    ("grant_type", "client_credentials"),
                    ("client_id", client_id.as_str()),
                    ("scope", scope),
                    ("client_assertion_type", "urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
                    ("client_assertion", assertion.trim()),
                ];
                let body: String = form.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect::<Vec<_>>().join("&");
                let resp = self
                    .http
                    .post(url)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(body)
                    .send()
                    .await?;
                Ok(check("Entra ID", resp).await?.json().await?)
            }
            Source::ManagedIdentity { endpoint, client_id } => {
                let resource = scope.trim_end_matches("/.default");
                let mut url = format!("{endpoint}?api-version=2018-02-01&resource={}", urlencode(resource));
                if let Some(id) = client_id {
                    url.push_str(&format!("&client_id={}", urlencode(id)));
                }
                let resp = self.http.get(url).header("Metadata", "true").send().await?;
                Ok(check("managed identity", resp).await?.json().await?)
            }
        }
    }
}

pub(crate) fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}
