//! The seam to a bank: submit a batch, ask whether the bank holds it, and
//! collect what the bank has to say. A file channel and an API channel are
//! the same seam with different wire forms.
//!
//! The wire form here is this system's own, over HTTPS. A real bank's
//! adapter implements [`Bank`] against that bank's formats and transport;
//! nothing above the trait changes.

use std::future::Future;

use base64::Engine;
use ic_payments::config::ChannelKind;
use ic_payments::file::{BankMessage, Batch, Manifest};
use serde::{Deserialize, Serialize};

/// One region's way to one channel, with the secret already fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub kind: ChannelKind,
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    /// The bank answered and said no. It does not hold the batch.
    #[error("the bank refused: {0}")]
    Refused(String),
    /// Nothing was sent: the connection was never made.
    #[error("the bank could not be reached: {0}")]
    NotSent(String),
    /// Something was sent and no clear answer came back. The bank may hold
    /// the batch. Ask before doing anything else.
    #[error("no clear answer from the bank: {0}")]
    Unknown(String),
}

/// The bank's receipt for a batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub reference: String,
    /// An API channel acknowledges in its answer. A file channel sends an
    /// acknowledgement file later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledgement: Option<BankMessage>,
}

/// One message from the bank, with its place in the bank's sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbered {
    pub seq: u64,
    pub message: BankMessage,
}

pub trait Bank: Send + Sync {
    /// Send a batch. Sending the same batch again under the same ID is
    /// answered as the first time.
    fn submit(&self, to: &Target, batch: &Batch, manifest: &Manifest) -> impl Future<Output = Result<Receipt, ChannelError>> + Send;
    /// The content hash of the batch the bank holds under this ID, if any.
    fn holds(&self, to: &Target, id: &str) -> impl Future<Output = Result<Option<String>, ChannelError>> + Send;
    /// The bank's messages after `after`, in order.
    fn messages(&self, to: &Target, after: u64) -> impl Future<Output = Result<Vec<Numbered>, ChannelError>> + Send;
}

/// A bank reached over HTTPS.
#[derive(Clone)]
pub struct HttpBank {
    http: reqwest::Client,
}

impl Default for HttpBank {
    fn default() -> Self {
        Self { http: ic_azure::http_client() }
    }
}

#[derive(Deserialize)]
struct Holding {
    sha256: String,
}

fn transport(e: reqwest::Error) -> ChannelError {
    if e.is_connect() {
        ChannelError::NotSent(e.to_string())
    } else {
        ChannelError::Unknown(e.to_string())
    }
}

/// A 4xx is the bank saying no. Anything else that is not success leaves
/// the outcome unknown.
async fn checked(response: reqwest::Response) -> Result<reqwest::Response, ChannelError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    if status.is_client_error() {
        Err(ChannelError::Refused(format!("{} {body}", status.as_u16())))
    } else {
        Err(ChannelError::Unknown(format!("{} {body}", status.as_u16())))
    }
}

async fn body<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, ChannelError> {
    response.json().await.map_err(|e| ChannelError::Unknown(e.to_string()))
}

impl HttpBank {
    fn place(kind: ChannelKind) -> &'static str {
        match kind {
            ChannelKind::File => "files",
            ChannelKind::Api => "batches",
        }
    }
}

impl Bank for HttpBank {
    async fn submit(&self, to: &Target, batch: &Batch, manifest: &Manifest) -> Result<Receipt, ChannelError> {
        let request = match to.kind {
            ChannelKind::File => {
                let manifest = serde_json::to_vec(manifest).map_err(|e| ChannelError::NotSent(e.to_string()))?;
                self.http
                    .put(format!("{}/files/{}", to.url, batch.id))
                    .header("x-manifest", base64::engine::general_purpose::STANDARD.encode(manifest))
                    .body(batch.file())
            }
            ChannelKind::Api => self.http.post(format!("{}/batches", to.url)).header("idempotency-key", &batch.id).json(batch),
        };
        let response = request.bearer_auth(&to.token).send().await.map_err(transport)?;
        body(checked(response).await?).await
    }

    async fn holds(&self, to: &Target, id: &str) -> Result<Option<String>, ChannelError> {
        let url = format!("{}/{}/{id}", to.url, Self::place(to.kind));
        let response = self.http.get(url).bearer_auth(&to.token).send().await.map_err(transport)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let holding: Holding = body(checked(response).await?).await?;
        Ok(Some(holding.sha256))
    }

    async fn messages(&self, to: &Target, after: u64) -> Result<Vec<Numbered>, ChannelError> {
        let url = format!("{}/messages?after={after}", to.url);
        let response = self.http.get(url).bearer_auth(&to.token).send().await.map_err(transport)?;
        body(checked(response).await?).await
    }
}
