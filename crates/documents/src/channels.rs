//! The delivery channels, and where a person can be reached.
//!
//! Email and SMS go through Azure Communication Services, which reports
//! delivery back. Post is a print file in a storage container the print
//! house collects from. The in-app inbox needs no sending: a document is
//! there as soon as it is issued, and is proven when it is opened.
//!
//! Contact details are never kept by this module. They are fetched from the
//! customer master at the moment of sending, for the relationship the
//! document is sent under, and only a masked form is recorded.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ic_azure::credential::AzureCredential;
use ic_core::PartyId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::kinds::{DataClass, DocumentKind};
use crate::model::Channel;
use crate::pdf::{Fonts, Letter, PdfError};
use crate::template::{sha256_hex, AlternateFormat};
use crate::vault::{ObjectStore, VaultError};

pub const SCOPE_COMMUNICATION: &str = "https://communication.azure.com/.default";

/// Where to reach a person on one channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Contact {
    pub name: String,
    /// An email address, a phone number in international form, or a postal
    /// address with one line per line.
    pub destination: String,
    /// The language the person prefers, where the customer master knows it.
    #[serde(default)]
    pub language: Option<String>,
    /// The format the person asked for: `standard`, `large_print`, `audio`
    /// or `braille`.
    #[serde(default)]
    pub format: Option<String>,
    /// The person has died. Nothing is sent in their name: what is owed
    /// goes to whoever represents the estate, as a recipient in their own right.
    #[serde(default)]
    pub deceased: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    #[error("the customer master could not be reached: {0}")]
    Unavailable(String),
}

/// The customer master, as far as delivery needs it.
pub trait ContactDirectory: Send + Sync {
    /// The contact point for notices to a party under a relationship, on a
    /// channel. `None` when the party has none.
    fn contact(&self, party: &PartyId, relationship: Option<&str>, channel: Channel) -> impl Future<Output = Result<Option<Contact>, DirectoryError>> + Send;
}

/// The customer master over HTTP:
/// `GET {base}/v1/parties/{party}/contact-points?purpose=notices&channel=email&relationship=policyholder`
/// answers `{"name": "...", "destination": "..."}`, or 404 when there is none.
#[derive(Clone)]
pub struct HttpDirectory {
    base: String,
    scope: String,
    credential: AzureCredential,
    http: reqwest::Client,
}

impl HttpDirectory {
    /// `scope` is the Entra ID scope of the customer master's API.
    pub fn new(base: &str, scope: &str, credential: AzureCredential) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            scope: scope.to_string(),
            credential,
            http: ic_azure::http_client(),
        }
    }
}

pub fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::InApp => "in_app",
        Channel::Push => "push",
        Channel::Email => "email",
        Channel::Sms => "sms",
        Channel::Post => "post",
    }
}

impl ContactDirectory for HttpDirectory {
    async fn contact(&self, party: &PartyId, relationship: Option<&str>, channel: Channel) -> Result<Option<Contact>, DirectoryError> {
        let down = |e: String| DirectoryError::Unavailable(e);
        let token = self.credential.token(&self.scope).await.map_err(|e| down(e.to_string()))?;
        let mut query = vec![("purpose", "notices"), ("channel", channel_name(channel))];
        if let Some(r) = relationship {
            query.push(("relationship", r));
        }
        let resp = self
            .http
            .get(format!("{}/v1/parties/{}/contact-points", self.base, party))
            .query(&query)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| down(e.to_string()))?;
        match resp.status().as_u16() {
            404 => Ok(None),
            s if (200..300).contains(&s) => resp.json().await.map(Some).map_err(|e| down(e.to_string())),
            s => Err(down(format!("the customer master returned {s}"))),
        }
    }
}

/// A directory held in memory, for the insurance in a box and tests. Like
/// the customer master, a contact point chosen for a relationship is used
/// for it, and the party's own otherwise.
#[derive(Clone, Default)]
pub struct MemoryDirectory {
    contacts: Arc<Mutex<BTreeMap<(String, Channel), Contact>>>,
    for_relationship: Arc<Mutex<BTreeMap<(String, String, Channel), Contact>>>,
    down: Arc<Mutex<bool>>,
}

impl MemoryDirectory {
    pub fn set_contact(&self, party: &str, channel: Channel, contact: Contact) {
        self.contacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((party.to_string(), channel), contact);
    }

    pub fn set(&self, party: &str, channel: Channel, name: &str, destination: &str) {
        let contact = Contact {
            name: name.to_string(),
            destination: destination.to_string(),
            ..Contact::default()
        };
        self.set_contact(party, channel, contact);
    }

    /// The contact point the party chose for one relationship.
    pub fn set_for(&self, party: &str, relationship: &str, channel: Channel, contact: Contact) {
        self.for_relationship
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((party.to_string(), relationship.to_string(), channel), contact);
    }

    pub fn remove(&self, party: &str, channel: Channel) {
        self.contacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(party.to_string(), channel));
    }

    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }
}

impl ContactDirectory for MemoryDirectory {
    async fn contact(&self, party: &PartyId, relationship: Option<&str>, channel: Channel) -> Result<Option<Contact>, DirectoryError> {
        if *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err(DirectoryError::Unavailable("the directory is down".into()));
        }
        if let Some(relationship) = relationship {
            let chosen = self
                .for_relationship
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&(party.to_string(), relationship.to_string(), channel))
                .cloned();
            if chosen.is_some() {
                return Ok(chosen);
            }
        }
        Ok(self
            .contacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(party.to_string(), channel))
            .cloned())
    }
}

/// A destination with most of it hidden, for the delivery record.
pub fn mask(channel: Channel, destination: &str) -> String {
    let tail = |n: usize| destination.chars().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect::<String>();
    match channel {
        Channel::Email => match destination.split_once('@') {
            Some((local, domain)) => format!("{}***@{domain}", local.chars().next().map(String::from).unwrap_or_default()),
            None => "***".to_string(),
        },
        Channel::Sms => format!("***{}", tail(3)),
        // The last line of an address is the town and postcode.
        Channel::Post => format!("*** {}", destination.lines().last().unwrap_or_default().trim()),
        Channel::InApp | Channel::Push => "app".to_string(),
    }
}

/// One message to one person on one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    pub document: String,
    pub party: PartyId,
    pub channel: Channel,
    pub kind: DocumentKind,
    pub data_class: DataClass,
    pub title: String,
    /// The document exactly as issued.
    pub text: String,
    pub format: Option<AlternateFormat>,
    /// The language of the text, as the document records it.
    pub language: String,
    pub contact: Contact,
    /// Which attempt on this channel this is, from one.
    pub attempt: u32,
}

impl Outbound {
    /// The same for every retry of the same attempt, so a provider that is
    /// asked twice sends once.
    pub fn idempotency_id(&self) -> String {
        let h = sha256_hex(format!("{}|{}|{}|{}", self.document, self.party, channel_name(self.channel), self.attempt).as_bytes());
        // The shape of a UUID, which the providers require.
        format!("{}-{}-4{}-8{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
    }

    /// Whether the text itself may travel on this channel. Health and
    /// other sensitive content stays in the secure inbox and on paper; an
    /// email then only says that a document is waiting.
    pub fn carries_text(&self) -> bool {
        match self.channel {
            Channel::Post | Channel::InApp => true,
            Channel::Email => self.data_class < DataClass::InsuranceSensitive,
            Channel::Sms | Channel::Push => false,
        }
    }

    /// The document as a PDF for this recipient: addressed for the
    /// envelope when it is posted, and in large print when that was asked for.
    pub fn pdf(&self, fonts: &Fonts) -> Result<Vec<u8>, PdfError> {
        let address: Vec<String> = match self.channel {
            Channel::Post => self.contact.destination.lines().map(str::to_string).collect(),
            _ => vec![],
        };
        let letter = Letter {
            document: &self.document,
            title: &self.title,
            name: if address.is_empty() { "" } else { &self.contact.name },
            address: &address,
            text: &self.text,
            language: &self.language,
            large_print: self.format == Some(AlternateFormat::LargePrint),
        };
        crate::pdf::pdf(&letter, fonts)
    }

    /// The PDF, or nothing when the text has a character the font cannot
    /// draw. The text still goes, so nothing is held up; the gap is logged.
    fn pdf_or_none(&self, fonts: &Fonts) -> Option<Vec<u8>> {
        match self.pdf(fonts) {
            Ok(file) => Some(file),
            Err(e) => {
                tracing::warn!(document = %self.document, error = %e, "the document goes without a PDF");
                None
            }
        }
    }

    pub fn pointer(&self) -> String {
        format!("You have a new document from us: {}. Sign in to your secure inbox to read it.", self.title)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// This channel will not work for this message. Move to the next.
    #[error("the message cannot be sent on this channel: {0}")]
    Permanent(String),
    /// Try again later.
    #[error("the channel is unavailable: {0}")]
    Temporary(String),
}

/// The channels as the delivery driver uses them. `send` returns the
/// provider's reference for the message, which its delivery report quotes.
pub trait Channels: Send + Sync {
    fn send(&self, message: &Outbound) -> impl Future<Output = Result<String, SendError>> + Send;
}

/// Azure Communication Services for email and SMS, and a storage container
/// as the print house's outbox.
#[derive(Clone)]
pub struct AzureChannels<P: ObjectStore> {
    /// `https://<resource>.communication.azure.com`.
    endpoint: String,
    /// The verified sender address of the email domain.
    email_from: String,
    /// The number or sender ID SMS is sent from.
    sms_from: String,
    credential: AzureCredential,
    print: P,
    fonts: Fonts,
    http: reqwest::Client,
}

impl<P: ObjectStore> AzureChannels<P> {
    pub fn new(endpoint: &str, email_from: &str, sms_from: &str, credential: AzureCredential, print: P) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            email_from: email_from.to_string(),
            sms_from: sms_from.to_string(),
            credential,
            print,
            fonts: Fonts::bundled(),
            http: ic_azure::http_client(),
        }
    }

    /// Set documents in the insurer's own faces instead of the bundled ones.
    pub fn with_fonts(mut self, fonts: Fonts) -> Self {
        self.fonts = fonts;
        self
    }

    async fn post(&self, path: &str, id: &str, body: Value) -> Result<Value, SendError> {
        let later = |e: String| SendError::Temporary(e);
        let token = self.credential.token(SCOPE_COMMUNICATION).await.map_err(|e| later(e.to_string()))?;
        let resp = self
            .http
            .post(format!("{}{path}", self.endpoint))
            .bearer_auth(token)
            .header("operation-id", id)
            .header("repeatability-request-id", id)
            .json(&body)
            .send()
            .await
            .map_err(|e| later(e.to_string()))?;
        let status = resp.status().as_u16();
        match status {
            200..=299 => resp.json().await.map_err(|e| later(e.to_string())),
            // Throttled, or the service's own fault: the message is fine.
            408 | 429 | 500..=599 => Err(later(format!("Communication Services returned {status}"))),
            _ => Err(SendError::Permanent(format!("Communication Services refused the message with {status}"))),
        }
    }

    async fn email(&self, m: &Outbound) -> Result<String, SendError> {
        let text = if m.carries_text() { m.text.clone() } else { m.pointer() };
        let mut body = json!({
            "senderAddress": self.email_from,
            "content": { "subject": m.title, "plainText": text },
            "recipients": { "to": [{ "address": m.contact.destination, "displayName": m.contact.name }] },
            "userEngagementTrackingDisabled": true,
        });
        // The same document to keep or print, when it may travel by email.
        if let Some(file) = m.carries_text().then(|| m.pdf_or_none(&self.fonts)).flatten() {
            body["attachments"] = json!([{
                "name": format!("{}.pdf", m.document),
                "contentType": "application/pdf",
                "contentInBase64": STANDARD.encode(file),
            }]);
        }
        let id = m.idempotency_id();
        let accepted = self.post("/emails:send?api-version=2023-03-31", &id, body).await?;
        Ok(accepted["id"].as_str().map_or(id, str::to_string))
    }

    async fn sms(&self, m: &Outbound) -> Result<String, SendError> {
        let id = m.idempotency_id();
        let body = json!({
            "from": self.sms_from,
            "smsRecipients": [{ "to": m.contact.destination, "repeatabilityRequestId": id }],
            "message": m.pointer(),
            "smsSendOptions": { "enableDeliveryReport": true, "tag": m.document },
        });
        let accepted = self.post("/sms?api-version=2021-03-07", &id, body).await?;
        let result = &accepted["value"][0];
        if result["successful"] != json!(true) {
            let reason = result["errorMessage"].as_str().unwrap_or("the number was refused");
            return Err(SendError::Permanent(reason.to_string()));
        }
        result["messageId"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| SendError::Temporary("the response carried no message id".into()))
    }

    async fn print(&self, m: &Outbound) -> Result<String, SendError> {
        let name = format!("print/{}/{}-{}.json", m.document, m.party, m.attempt);
        // The pages first, so that an instruction never names a file that
        // is not there. Written once: a retry finds it and carries on.
        let pages = match m.pdf_or_none(&self.fonts) {
            Some(file) => {
                let pages = format!("print/{}/{}-{}.pdf", m.document, m.party, m.attempt);
                match self.print.put_new(&pages, file, None).await {
                    Ok(()) | Err(VaultError::AlreadyExists(_)) => Some(pages),
                    Err(e) => return Err(SendError::Temporary(e.to_string())),
                }
            }
            None => None,
        };
        let file = json!({
            "pdf": pages,
            "document": m.document,
            "name": m.contact.name,
            "address": m.contact.destination.lines().collect::<Vec<_>>(),
            "title": m.title,
            "text": m.text,
            "format": m.format,
        });
        match self.print.put_new(&name, serde_json::to_vec(&file).unwrap_or_default(), None).await {
            // Already in the outbox from an earlier try: it is printed once.
            Ok(()) | Err(VaultError::AlreadyExists(_)) => Ok(name),
            Err(e) => Err(SendError::Temporary(e.to_string())),
        }
    }
}

impl<P: ObjectStore> Channels for AzureChannels<P> {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        match message.channel {
            Channel::Email => self.email(message).await,
            Channel::Sms => self.sms(message).await,
            Channel::Post => self.print(message).await,
            // Nothing to send: the inbox shows every document issued.
            Channel::InApp => Ok(format!("inbox:{}", message.document)),
            // Push belongs to the app services, which hold the device
            // registrations. Without them the next channel is tried.
            Channel::Push => Err(SendError::Permanent("push is sent by the app services".into())),
        }
    }
}

/// What a provider says became of a message it accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReport {
    pub provider_reference: String,
    pub outcome: crate::model::AttemptOutcome,
}

#[derive(Deserialize)]
struct GridEvent {
    #[serde(rename = "eventType", alias = "type")]
    event_type: String,
    data: Value,
}

/// Read the delivery reports out of an Event Grid delivery. Events of other
/// types, and reports that are not final, are left out.
pub fn delivery_reports(body: &[u8]) -> Result<Vec<DeliveryReport>, String> {
    use crate::model::AttemptOutcome::{Bounced, Delivered, Failed};
    let events: Vec<GridEvent> = serde_json::from_slice(body).map_err(|e| format!("not an Event Grid delivery: {e}"))?;
    let mut reports = vec![];
    for e in events {
        let status = e.data["status"].as_str().or(e.data["deliveryStatus"].as_str()).unwrap_or_default().to_string();
        let detail = e.data["deliveryStatusDetails"]["statusMessage"]
            .as_str()
            .or(e.data["deliveryStatusDetails"].as_str())
            .unwrap_or(&status)
            .to_string();
        let (reference, outcome) = match e.event_type.as_str() {
            "Microsoft.Communication.EmailDeliveryReportReceived" => (
                e.data["messageId"].as_str(),
                match status.as_str() {
                    "Delivered" => Some(Delivered),
                    "Bounced" | "Suppressed" => Some(Bounced { reason: detail }),
                    "Failed" | "Quarantined" | "FilteredSpam" => Some(Failed { reason: detail }),
                    _ => None,
                },
            ),
            "Microsoft.Communication.SMSDeliveryReportReceived" => (
                e.data["messageId"].as_str(),
                match status.as_str() {
                    "Delivered" => Some(Delivered),
                    "" => None,
                    _ => Some(Failed { reason: detail }),
                },
            ),
            _ => (None, None),
        };
        if let (Some(reference), Some(outcome)) = (reference, outcome) {
            reports.push(DeliveryReport {
                provider_reference: reference.to_string(),
                outcome,
            });
        }
    }
    Ok(reports)
}

/// The code Event Grid sends once to prove the endpoint is ours to call,
/// which must be sent back.
pub fn subscription_validation(body: &[u8]) -> Option<String> {
    let events: Vec<GridEvent> = serde_json::from_slice(body).ok()?;
    events
        .iter()
        .find(|e| e.event_type == "Microsoft.EventGrid.SubscriptionValidationEvent")
        .and_then(|e| e.data["validationCode"].as_str().map(str::to_string))
}
