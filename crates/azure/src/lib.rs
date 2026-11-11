//! Azure adapters for the parts of the platform that sit outside the core.
//!
//! | Design element | Azure capability |
//! | --- | --- |
//! | Short-lived workload identities | Microsoft Entra Workload ID on AKS, managed identities |
//! | Write-once storage for Merkle roots and the cold tier | Blob Storage with version-level immutability (WORM) |
//! | Hardware-backed signing keys | Key Vault Premium or Managed HSM |
//! | Event stream (one topic partition per ledger partition) | Event Hubs, 64 partitions, Kafka-compatible |
//!
//! Each adapter speaks the service's REST API with a bearer token, so there
//! are no keys or connection strings anywhere. Every adapter sits behind a
//! small trait with an in-memory implementation for the insurance in a box.

pub mod blob;
pub mod cold_tier;
pub mod credential;
pub mod eventhubs;
pub mod keyvault;
pub mod merkle;

#[derive(Debug, thiserror::Error)]
pub enum AzureError {
    #[error("no Azure credential is configured: {0}")]
    NoCredential(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{service} returned {status}: {body}")]
    Status { service: &'static str, status: u16, body: String },
    #[error("the blob already exists and is immutable: {0}")]
    AlreadyExists(String),
    #[error("unexpected response: {0}")]
    Response(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// The HTTP client every adapter uses. No call to an Azure service may wait
/// for ever: a connection that is not made in five seconds, or a call not
/// answered in thirty, fails and is retried by whoever asked.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

pub(crate) async fn check(service: &'static str, resp: reqwest::Response) -> Result<reqwest::Response, AzureError> {
    if resp.status().is_success() {
        Ok(resp)
    } else {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Err(AzureError::Status {
            service,
            status,
            body: body.chars().take(500).collect(),
        })
    }
}
