//! Downloading a document's PDF from the documents module
//! (`docs/documents.md`, "Downloading").
//!
//! A customer, adviser or employer administrator asks for one of their own
//! documents; the documents module decides by the party named, answering
//! 404 for one that is not theirs, the same as one that does not exist.
//! Staff ask with the staff action `documents.download` and a purpose, and
//! never get a clinical document. The PDF passes through and is never kept.

use ic_telemetry::Traced;
use std::future::Future;
use std::pin::Pin;

/// The staff action that downloads a document.
pub mod actions {
    pub const DOWNLOAD: &str = "documents.download";
}

/// Who is asking, as the documents module's rule needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asker {
    /// The signed-in customer, adviser or administrator.
    Party(String),
    /// A member of staff, with why they need it.
    Staff { actor: String, purpose: String },
}

/// What the documents module answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Fetched, String>> + Send + 'a>>;

/// The documents module, as the portal calls it.
pub trait Documents: Send + Sync {
    fn pdf(&self, id: &str, asker: Asker) -> Asked<'_>;
}

/// A document ID as the documents module takes it, or `None` for one that
/// could not be one.
pub fn id(raw: &str) -> Option<&str> {
    (!raw.is_empty() && raw.len() <= 128 && raw.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c)) && !raw.contains("..")).then_some(raw)
}

/// The documents service, reached with the portal's own Entra token for the
/// documents app, which carries the app roles `portal` and `staff`.
pub struct Hosted {
    pub base: String,
    pub scope: String,
    pub credential: ic_azure::credential::AzureCredential,
    pub http: reqwest::Client,
}

impl Documents for Hosted {
    fn pdf(&self, id: &str, asker: Asker) -> Asked<'_> {
        let url = format!("{}/v1/documents/{id}/pdf", self.base.trim_end_matches('/'));
        Box::pin(async move {
            let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
            let request = self.http.get(url).bearer_auth(token);
            let request = match asker {
                Asker::Party(party) => request.header("x-ic-caller", "portal").header("x-ic-party", party),
                Asker::Staff { actor, purpose } => request
                    .header("x-ic-caller", "staff")
                    .header("x-ic-actor", actor)
                    .header("x-ic-purpose", purpose),
            };
            let response = request.traced().send().await.map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            if status == 503 {
                return Err("the documents module asked us to try again".to_string());
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();
            let body = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
            Ok(Fetched { status, content_type, body })
        })
    }
}
