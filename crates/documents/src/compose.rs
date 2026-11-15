//! Composing a document: the seam for an external document service.
//!
//! The documents module decides what may be issued. A template is
//! versioned by its content hash, approved by legal and product (and by
//! compliance for a notice), tested and activated there, and placeholder
//! wording can never be activated. Whatever composes the document, the
//! driver first fills the approved template with the letter's data, so a
//! missing field still stops the letter. It then stores what was composed,
//! encrypted, and records the issue with the template version, the content
//! and data hashes and, for an external composer, what composed it. None of
//! that moves to the composer.
//!
//! Two composers, chosen in configuration:
//!
//! - [`Composer::BuiltIn`], the default: the filled template's text is the
//!   document, and its PDF is set from that text when it is sent or
//!   downloaded.
//! - [`Composer::External`]: a document generation or customer
//!   communications platform under its own Entra identity. It is sent the
//!   template's id, version and content hash, the approved wording in the
//!   document's language, the data and the filled text. It answers the
//!   document's text and, optionally, its PDF, which is then the document
//!   exactly as issued: stored, downloaded, attached to email and put in
//!   the print outbox (the print instruction still carries the address).

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ic_azure::credential::AzureCredential;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::kinds::DocumentKind;
use crate::template::{sha256_hex, AlternateFormat, TemplateRef};

/// What is composed: the approved template version and the filled text.
#[derive(Debug, Clone, PartialEq)]
pub struct Composition<'a> {
    pub document: &'a str,
    pub template: &'a TemplateRef,
    /// The template version's content hash, as the module approved it.
    pub template_hash: &'a str,
    pub kind: DocumentKind,
    pub title: &'a str,
    pub language: &'a str,
    pub format: Option<AlternateFormat>,
    /// The approved wording in `language`, as the template holds it.
    pub source: &'a str,
    pub data: &'a Value,
    /// The template filled with the data by the built-in renderer.
    pub filled: &'a str,
}

/// What was composed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    /// The document's text: the body of an email or SMS, and what is read
    /// to verify it.
    pub text: String,
    /// The document as a PDF, when the composer sets it.
    pub pdf: Option<Vec<u8>>,
    /// Who composed it, for the record. `None` for the built-in renderer.
    pub rendition: Option<Rendition>,
}

/// The record of an external composition, kept with the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rendition {
    /// The composer's name and version, as it reports them.
    pub composer: String,
    pub version: String,
    /// The hash of the PDF it set, if it set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    /// The composer could not be reached or failed: try again.
    #[error("the document composer is unavailable: {0}")]
    Unavailable(String),
    /// The composer refused this document, or answered something that is
    /// not a document. Trying again will not help until someone looks.
    #[error("the document composer refused the document: {0}")]
    Refused(String),
}

/// The external composer's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSettings {
    /// The service's base URL; documents are composed at `{base}/v1/compose`.
    pub base: String,
    /// The Entra ID scope of the service's API.
    pub scope: String,
}

/// An external document service over HTTPS.
#[derive(Clone)]
pub struct ExternalComposer {
    base: String,
    scope: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

impl ExternalComposer {
    pub fn new(settings: &ExternalSettings, credential: AzureCredential) -> Self {
        Self {
            base: settings.base.trim_end_matches('/').to_string(),
            scope: settings.scope.clone(),
            credential,
            http: ic_azure::http_client(),
        }
    }

    async fn compose(&self, c: &Composition<'_>) -> Result<Composed, ComposeError> {
        let down = |e: String| ComposeError::Unavailable(e);
        let token = self.credential.token(&self.scope).await.map_err(|e| down(e.to_string()))?;
        let body = json!({
            "document": c.document,
            "template": { "id": c.template.id, "version": c.template.version, "content_hash": c.template_hash },
            "kind": c.kind,
            "title": c.title,
            "language": c.language,
            "format": c.format,
            "source": c.source,
            "data": c.data,
            "filled": c.filled,
        });
        let resp = self
            .http
            .post(format!("{}/v1/compose", self.base))
            .bearer_auth(token)
            // The same document is composed once however often it is asked.
            .header(
                "Idempotency-Key",
                sha256_hex(format!("{}|{}|{}", c.document, c.template_hash, c.language).as_bytes()),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| down(e.to_string()))?;
        let status = resp.status().as_u16();
        let answer: Value = match status {
            200..=299 => resp.json().await.map_err(|e| ComposeError::Refused(format!("the answer is not JSON: {e}")))?,
            408 | 429 | 500..=599 => return Err(down(format!("the composer returned {status}"))),
            _ => {
                let why = resp.text().await.unwrap_or_default();
                return Err(ComposeError::Refused(format!("{status}: {why}")));
            }
        };
        composed_from(&answer)
    }
}

/// The external composer's answer: `{text, pdf?: base64, composer:
/// {name, version}}`. A document with no text, a PDF that is not one, or
/// a composer that does not say who it is, is refused.
pub fn composed_from(answer: &Value) -> Result<Composed, ComposeError> {
    let refused = |why: &str| Err(ComposeError::Refused(why.to_string()));
    let Some(text) = answer["text"].as_str().filter(|t| !t.trim().is_empty()) else {
        return refused("the answer has no text");
    };
    let pdf = match &answer["pdf"] {
        Value::Null => None,
        Value::String(encoded) => match STANDARD.decode(encoded) {
            Ok(bytes) if bytes.starts_with(b"%PDF-") => Some(bytes),
            _ => return refused("the pdf is not a PDF"),
        },
        _ => return refused("the pdf is not base64 text"),
    };
    let (Some(name), Some(version)) = (answer["composer"]["name"].as_str(), answer["composer"]["version"].as_str()) else {
        return refused("the answer does not say what composed it");
    };
    Ok(Composed {
        text: text.to_string(),
        rendition: Some(Rendition {
            composer: name.to_string(),
            version: version.to_string(),
            pdf_sha256: pdf.as_deref().map(sha256_hex),
        }),
        pdf,
    })
}

/// What composes documents.
#[derive(Clone, Default)]
pub enum Composer {
    /// The filled template is the document.
    #[default]
    BuiltIn,
    External(ExternalComposer),
}

impl Composer {
    /// The external composer when its settings are given, else the
    /// built-in renderer.
    pub fn from_settings(settings: Option<&ExternalSettings>, credential: AzureCredential) -> Self {
        match settings {
            Some(settings) => Composer::External(ExternalComposer::new(settings, credential)),
            None => Composer::BuiltIn,
        }
    }

    pub async fn compose(&self, c: &Composition<'_>) -> Result<Composed, ComposeError> {
        match self {
            Composer::BuiltIn => Ok(Composed {
                text: c.filled.to_string(),
                pdf: None,
                rendition: None,
            }),
            Composer::External(external) => external.compose(c).await,
        }
    }
}
