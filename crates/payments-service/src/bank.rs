//! The seam to a bank: submit a batch, ask whether the bank holds it, and
//! collect what the bank has to say. A file channel and an API channel are
//! the same seam with different wire forms.
//!
//! The wire form here is this system's own, over HTTPS. A real bank's
//! adapter implements [`Bank`] against that bank's formats and transport;
//! nothing above the trait changes.

use std::collections::BTreeMap;
use std::future::Future;

use base64::Engine;
use ic_payments::config::{ChannelKind, WireFormat};
use ic_payments::file::{BankMessage, Batch, Manifest, Recall};
use serde::{Deserialize, Serialize};

/// One region's way to one channel, with the secret already fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub kind: ChannelKind,
    pub format: WireFormat,
    pub url: String,
    pub token: String,
    /// The module's code for each of our account numbers at this bank, to
    /// read the bank's statements by.
    pub accounts: BTreeMap<String, String>,
    /// The client certificate and its key, in PEM, presented to the bank.
    pub identity: Option<String>,
    /// The bank's private certificate authority, in PEM, if it has one.
    pub authority: Option<String>,
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
    /// Which of the messages read from one of the bank's documents this is.
    /// A document in the system's own format is one message.
    #[serde(default, skip_serializing_if = "is_first")]
    pub part: u32,
    pub message: BankMessage,
}

fn is_first(part: &u32) -> bool {
    *part == 0
}

/// One document from a bank that speaks ISO 20022, with its place in the
/// bank's sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub seq: u64,
    pub document: String,
}

/// The messages in a bank's documents, in order. A document that cannot be
/// read stops the reading there: what came before it is returned, and if it
/// is the first, the reason is. Nothing after it is read until it can be.
pub fn read_documents(documents: Vec<Document>, accounts: &BTreeMap<String, String>) -> Result<Vec<Numbered>, ChannelError> {
    let mut out = Vec::new();
    for d in documents {
        match ic_payments::iso20022::read_messages(&d.document, accounts) {
            Ok(messages) => out.extend(messages.into_iter().zip(0..).map(|(message, part)| Numbered { seq: d.seq, part, message })),
            Err(e) if out.is_empty() => return Err(ChannelError::Refused(format!("document {} cannot be read: {e}", d.seq))),
            Err(_) => break,
        }
    }
    Ok(out)
}

pub trait Bank: Send + Sync {
    /// Send a batch. Sending the same batch again under the same ID is
    /// answered as the first time.
    fn submit(&self, to: &Target, batch: &Batch, manifest: &Manifest) -> impl Future<Output = Result<Receipt, ChannelError>> + Send;
    /// The content hash of the batch the bank holds under this ID, if any.
    fn holds(&self, to: &Target, id: &str) -> impl Future<Output = Result<Option<String>, ChannelError>> + Send;
    /// The bank's messages after `after`, in order.
    fn messages(&self, to: &Target, after: u64) -> impl Future<Output = Result<Vec<Numbered>, ChannelError>> + Send;
    /// Ask the bank to recall a payment. The same recall sent again under
    /// its ID is answered as the first time.
    fn recall(&self, to: &Target, recall: &Recall) -> impl Future<Output = Result<(), ChannelError>> + Send;
}

/// A bank reached over HTTPS, presenting a client certificate where the
/// endpoint has one.
#[derive(Clone)]
pub struct HttpBank {
    http: reqwest::Client,
    /// A client per certificate and authority, by the hash of both, so a
    /// renewed certificate takes effect on the next request.
    tls: std::sync::Arc<std::sync::Mutex<BTreeMap<String, reqwest::Client>>>,
}

impl Default for HttpBank {
    fn default() -> Self {
        Self {
            http: ic_azure::http_client(),
            tls: Default::default(),
        }
    }
}

/// Why the certificates cannot be used. Nothing was sent.
fn unusable(e: reqwest::Error) -> ChannelError {
    ChannelError::NotSent(format!("the certificates cannot be used: {e}"))
}

impl HttpBank {
    /// The client for a target: the plain one, or one that presents the
    /// target's client certificate and trusts the bank's own authority.
    fn client(&self, to: &Target) -> Result<reqwest::Client, ChannelError> {
        if to.identity.is_none() && to.authority.is_none() {
            return Ok(self.http.clone());
        }
        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        for part in [&to.identity, &to.authority] {
            sha2::Digest::update(&mut hasher, part.as_deref().unwrap_or_default().as_bytes());
            sha2::Digest::update(&mut hasher, [0u8]);
        }
        let key = hex::encode(sha2::Digest::finalize(hasher));
        let mut cache = self
            .tls
            .lock()
            .map_err(|_| ChannelError::NotSent("the TLS client cache is poisoned".to_string()))?;
        if let Some(client) = cache.get(&key) {
            return Ok(client.clone());
        }
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30));
        if let Some(pem) = &to.identity {
            if !pem.contains("-----BEGIN CERTIFICATE-----") || !pem.contains("PRIVATE KEY-----") {
                return Err(ChannelError::NotSent(
                    "the client certificate cannot be used: it needs a certificate and its private key in PEM".to_string(),
                ));
            }
            let identity = reqwest::Identity::from_pem(pem.as_bytes()).map_err(unusable)?;
            builder = builder.identity(identity);
        }
        if let Some(pem) = &to.authority {
            if !pem.contains("-----BEGIN CERTIFICATE-----") {
                return Err(ChannelError::NotSent(
                    "the bank's certificate authority cannot be used: it needs a certificate in PEM".to_string(),
                ));
            }
            let authority = reqwest::Certificate::from_pem(pem.as_bytes()).map_err(unusable)?;
            builder = builder.add_root_certificate(authority);
        }
        let client = builder.build().map_err(unusable)?;
        cache.insert(key, client.clone());
        Ok(client)
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
    response.json().await.map_err(transport)
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
        // A batch the format cannot carry never leaves.
        let wire = batch.wire().map_err(ChannelError::NotSent)?;
        let http = self.client(to)?;
        let content_type = match to.format {
            WireFormat::Own => "text/plain",
            WireFormat::Iso20022 => "application/xml",
        };
        let request = match (to.kind, to.format) {
            (ChannelKind::File, _) => {
                let manifest = serde_json::json!(manifest).to_string();
                http.put(format!("{}/files/{}", to.url, batch.id))
                    .header("x-manifest", base64::engine::general_purpose::STANDARD.encode(manifest))
                    .header("content-type", content_type)
                    .body(wire)
            }
            (ChannelKind::Api, WireFormat::Own) => http.post(format!("{}/batches", to.url)).header("idempotency-key", &batch.id).json(batch),
            (ChannelKind::Api, WireFormat::Iso20022) => http
                .post(format!("{}/batches", to.url))
                .header("idempotency-key", &batch.id)
                .header("content-type", content_type)
                .body(wire),
        };
        let response = request.bearer_auth(&to.token).send().await.map_err(transport)?;
        body(checked(response).await?).await
    }

    async fn holds(&self, to: &Target, id: &str) -> Result<Option<String>, ChannelError> {
        let url = format!("{}/{}/{id}", to.url, Self::place(to.kind));
        let response = self.client(to)?.get(url).bearer_auth(&to.token).send().await.map_err(transport)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let holding: Holding = body(checked(response).await?).await?;
        Ok(Some(holding.sha256))
    }

    async fn recall(&self, to: &Target, recall: &Recall) -> Result<(), ChannelError> {
        let wire = recall.wire().map_err(ChannelError::NotSent)?;
        let content_type = match to.format {
            WireFormat::Own => "application/json",
            WireFormat::Iso20022 => "application/xml",
        };
        let http = self.client(to)?;
        let request = match to.kind {
            ChannelKind::File => http.put(format!("{}/recalls/{}", to.url, recall.id)),
            ChannelKind::Api => http.post(format!("{}/recalls", to.url)).header("idempotency-key", &recall.id),
        };
        let response = request
            .header("content-type", content_type)
            .body(wire)
            .bearer_auth(&to.token)
            .send()
            .await
            .map_err(transport)?;
        checked(response).await.map(|_| ())
    }

    async fn messages(&self, to: &Target, after: u64) -> Result<Vec<Numbered>, ChannelError> {
        let url = format!("{}/messages?after={after}", to.url);
        let response = self.client(to)?.get(url).bearer_auth(&to.token).send().await.map_err(transport)?;
        match to.format {
            WireFormat::Own => body(checked(response).await?).await,
            WireFormat::Iso20022 => read_documents(body(checked(response).await?).await?, &to.accounts),
        }
    }
}
