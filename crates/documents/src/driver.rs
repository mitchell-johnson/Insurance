//! The driver around the documents module: everything that renders,
//! encrypts, stores, sends and reports.
//!
//! The module's state machine decides; the driver does. Every step reads
//! what is outstanding from the module, does it against the outside world,
//! and records the result with a command whose idempotency key comes from
//! the module's own state. A driver that stops at any point is restarted
//! and carries on: nothing is lost, and nothing is sent or issued twice.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use ic_core::{PartyId, PolicyId, Reject};
use ic_log::StateMachine;
use ic_raft::ClusterClient;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::channels::{mask, Channels, ContactDirectory, DeliveryReport, Outbound, SendError};
use crate::command::{Command, Envelope, Issue, NoticeSpec};
use crate::crypto::{self, CryptoError, Key, KeyWrap};
use crate::event::EventRecord;
use crate::kinds::DataClass;
use crate::model::{AttemptOutcome, CoreInstruction, Document, Recipient, RetentionBasis, RetentionRule};
use crate::module::{self, Query};
use crate::state::{nz_date, Applied, DocumentsState, PendingSend, TemplateRecord};
use crate::template::{AlternateFormat, Part, PartRef, TemplateError, TemplateRef};
use crate::vault::{self, Contents, ObjectStore, VaultError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    /// The module could not be reached. Nothing is known to have happened:
    /// do the same again.
    #[error("the documents module is unavailable: {0}")]
    Unavailable(String),
    #[error("refused: {0}")]
    Refused(Reject),
    #[error("{0} was not found")]
    NotFound(String),
    #[error(transparent)]
    Template(#[from] TemplateError),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("unexpected answer from the module: {0}")]
    Answer(String),
}

/// The documents module, wherever it runs.
pub trait Module: Send + Sync {
    fn propose(&self, envelope: Envelope) -> impl Future<Output = Result<Applied, DriverError>> + Send;
    fn ask(&self, query: Query) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// The module in its replicated group on the cluster.
#[derive(Clone)]
pub struct ClusterModule {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterModule {
    /// `patience` is how long one command keeps looking for a leader.
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Module for ClusterModule {
    async fn propose(&self, envelope: Envelope) -> Result<Applied, DriverError> {
        let command = serde_json::to_value(&envelope).map_err(|e| DriverError::Answer(e.to_string()))?;
        let reply = self
            .client
            .propose_module_until(module::NAME, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let applied = reply.result.map_err(DriverError::Answer)?;
        serde_json::from_value(applied).map_err(|e| DriverError::Answer(e.to_string()))
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        let question = serde_json::to_value(&query).map_err(|e| DriverError::Answer(e.to_string()))?;
        self.client
            .query_module(module::NAME, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(DriverError::Answer)
    }
}

/// The module in this process, for the insurance in a box and tests. It can
/// be made unavailable to play a lost cluster.
#[derive(Clone, Default)]
pub struct LocalModule {
    inner: Arc<Mutex<(DocumentsState, u64)>>,
    down: Arc<Mutex<bool>>,
}

impl LocalModule {
    fn lock(&self) -> std::sync::MutexGuard<'_, (DocumentsState, u64)> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A copy of the state as it stands.
    pub fn state(&self) -> DocumentsState {
        self.lock().0.clone()
    }

    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }

    fn check(&self) -> Result<(), DriverError> {
        if *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err(DriverError::Unavailable("no node answered".into()));
        }
        Ok(())
    }
}

impl Module for LocalModule {
    async fn propose(&self, envelope: Envelope) -> Result<Applied, DriverError> {
        self.check()?;
        let mut inner = self.lock();
        inner.1 += 1;
        let index = inner.1;
        Ok(inner.0.apply(index, envelope))
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        self.check()?;
        let question = serde_json::to_value(&query).map_err(|e| DriverError::Answer(e.to_string()))?;
        module::answer(&self.lock().0, question).map_err(DriverError::Answer)
    }
}

/// Where the module's own events are published for other services.
pub trait EventPublisher: Send + Sync {
    /// Publish in order. All or nothing: on failure the same events are
    /// offered again.
    fn publish(&self, events: &[EventRecord]) -> impl Future<Output = Result<(), String>> + Send;
}

/// The core, as far as this module instructs it.
pub trait CoreLink: Send + Sync {
    /// Carry one instruction to the core. `id` is stable for the
    /// instruction, so the core applies a repeat once.
    fn instruct(&self, id: &str, instruction: &CoreInstruction) -> impl Future<Output = Result<(), String>> + Send;
}

/// A request to issue a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IssueRequest {
    /// The document's ID, chosen by the requester and stable across
    /// retries: asking twice issues once.
    pub document: String,
    /// The service asking: `documents`, `remediation` or `complaints`.
    pub caller: String,
    pub template: String,
    /// A specific template version. Otherwise the version in force today.
    #[serde(default)]
    pub template_version: Option<u32>,
    #[serde(default)]
    pub policy: Option<PolicyId>,
    #[serde(default)]
    pub other_policies: Vec<PolicyId>,
    #[serde(default)]
    pub data_class: DataClass,
    #[serde(default)]
    pub lives: Vec<String>,
    #[serde(default)]
    pub prove_delivery: bool,
    pub owner: PartyId,
    pub recipients: Vec<Recipient>,
    pub language: String,
    #[serde(default)]
    pub format: Option<AlternateFormat>,
    /// The data the document is rendered from. It is stored, encrypted,
    /// with the document, so the document can be explained later.
    pub data: Value,
    #[serde(default)]
    pub trigger_event: Option<String>,
    #[serde(default)]
    pub case: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub wordings: Vec<TemplateRef>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub notice: Option<NoticeSpec>,
}

/// What one pass over the outstanding sends did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Sends {
    pub sent: u32,
    /// The channel refused the message; the next channel will be tried.
    pub refused: u32,
    pub no_contact_point: u32,
    /// Left for the next pass: a channel or the customer master was down.
    pub deferred: u32,
}

/// What one cycle of the driver did, and what it could not do.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Cycle {
    pub sends: Sends,
    pub deleted: u32,
    pub published: u32,
    pub instructed: u32,
    /// Steps that failed. Each is tried again next cycle.
    pub errors: Vec<String>,
}

pub struct Driver<M, S, W, C, D> {
    pub module: M,
    pub objects: S,
    pub wrap: W,
    pub channels: C,
    pub directory: D,
    /// The name this process acts under in the record.
    pub actor: String,
    pub clock: fn() -> DateTime<Utc>,
    /// How many outstanding items one step takes at a time.
    pub batch: usize,
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, DriverError> {
    serde_json::from_value(value).map_err(|e| DriverError::Answer(e.to_string()))
}

impl<M: Module, S: ObjectStore, W: KeyWrap, C: Channels, D: ContactDirectory> Driver<M, S, W, C, D> {
    async fn send(&self, key: String, caller: &str, command: Command) -> Result<Applied, DriverError> {
        let applied = self
            .module
            .propose(Envelope {
                key,
                caller: caller.to_string(),
                actor: self.actor.clone(),
                at: (self.clock)(),
                command,
            })
            .await?;
        match &applied.result {
            Ok(()) => Ok(applied),
            Err(reject) => Err(DriverError::Refused(reject.clone())),
        }
    }

    pub async fn document(&self, id: &str) -> Result<Document, DriverError> {
        let found: Option<Document> = decode(self.module.ask(Query::Document { id: id.to_string() }).await?)?;
        found.ok_or_else(|| DriverError::NotFound(format!("document {id}")))
    }

    /// The owner's key, made and registered on first use. Two drivers that
    /// make one at the same moment both end up using the one that stood.
    async fn owner_key(&self, party: &PartyId) -> Result<Key, DriverError> {
        let ask = || self.module.ask(Query::PartyKey { party: party.clone() });
        let mut wrapped: Option<String> = decode(ask().await?)?;
        if wrapped.is_none() {
            let wrapped_key = self.wrap.wrap(&crypto::new_key()).await?;
            self.send(
                format!("party-key:{party}"),
                "documents",
                Command::RegisterPartyKey {
                    party: party.clone(),
                    wrapped_key,
                },
            )
            .await?;
            wrapped = decode(ask().await?)?;
        }
        let wrapped = wrapped.ok_or_else(|| DriverError::NotFound(format!("the key of party {party}")))?;
        Ok(self.wrap.unwrap(&wrapped).await?)
    }

    async fn template(&self, r: &TemplateRef) -> Result<(TemplateRecord, std::collections::BTreeMap<PartRef, Part>), DriverError> {
        let record: Option<TemplateRecord> = decode(self.module.ask(Query::Template { template: r.clone() }).await?)?;
        let record = record.ok_or_else(|| DriverError::NotFound(format!("template {} version {}", r.id, r.version)))?;
        let parts: Vec<Part> = decode(self.module.ask(Query::Parts { template: r.clone() }).await?)?;
        let parts = parts
            .into_iter()
            .map(|p| {
                (
                    PartRef {
                        id: p.id.clone(),
                        version: p.version,
                    },
                    p,
                )
            })
            .collect();
        Ok((record, parts))
    }

    /// Render a document, store it encrypted, and record it as issued.
    /// Safe to repeat: the same request issues the same document once.
    pub async fn issue(&self, request: IssueRequest) -> Result<Document, DriverError> {
        if let Ok(existing) = self.document(&request.document).await {
            return Ok(existing);
        }
        let today = nz_date((self.clock)());
        let template_ref = match request.template_version {
            Some(version) => TemplateRef {
                id: request.template.clone(),
                version,
            },
            None => {
                let current: Option<TemplateRecord> = decode(
                    self.module
                        .ask(Query::TemplateFor {
                            id: request.template.clone(),
                            on: today,
                        })
                        .await?,
                )?;
                current
                    .ok_or_else(|| DriverError::NotFound(format!("an active version of template {}", request.template)))?
                    .template
                    .reference()
            }
        };
        let (record, parts) = self.template(&template_ref).await?;
        // A language the template does not have is served in its primary one.
        let language = if record.template.bodies.contains_key(&request.language) {
            request.language.clone()
        } else {
            record.template.primary_language.clone()
        };
        let text = record.template.render(&parts, &language, &request.data)?;
        let kind = record.template.kind;
        let rule: RetentionRule = decode(self.module.ask(Query::Retention { kind }).await?)?;
        // Where the period runs from issue it is known now, and storage
        // itself is asked to refuse deletion before it ends.
        let keep_until = (rule.basis == RetentionBasis::Issue).then(|| today + chrono::Duration::days(rule.days as i64));
        let owner_key = self.owner_key(&request.owner).await?;
        let contents = Contents { text, data: request.data };
        let stored = vault::store(&self.objects, &owner_key, &request.document, &contents, keep_until).await?;
        let issue = Issue {
            document: request.document.clone(),
            kind,
            template: template_ref,
            policy: request.policy,
            other_policies: request.other_policies,
            data_class: request.data_class,
            lives: request.lives,
            prove_delivery: request.prove_delivery,
            owner: request.owner,
            recipients: request.recipients,
            language,
            format: request.format,
            content_sha256: stored.content_sha256,
            data_sha256: stored.data_sha256,
            blob: stored.blob.clone(),
            wrapped_key: stored.wrapped_key,
            bytes: stored.bytes,
            trigger_event: request.trigger_event,
            case: request.case,
            supersedes: request.supersedes,
            reason: request.reason,
            wordings: request.wordings,
            components: request.components,
            notice: request.notice,
        };
        let key = format!("issue:{}", request.document);
        if let Err(e) = self.send(key, &request.caller, Command::IssueDocument(Box::new(issue))).await {
            if matches!(e, DriverError::Refused(_)) {
                // Nothing refers to the object, and its key was never kept.
                let _ = self.objects.delete(&stored.blob).await;
            }
            return Err(e);
        }
        self.document(&request.document).await
    }

    /// A document exactly as issued, with the data it was rendered from.
    pub async fn read(&self, id: &str) -> Result<Contents, DriverError> {
        let document = self.document(id).await?;
        self.contents(&document).await
    }

    async fn contents(&self, document: &Document) -> Result<Contents, DriverError> {
        let wrapped: Option<String> = decode(self.module.ask(Query::PartyKey { party: document.owner.clone() }).await?)?;
        let wrapped = wrapped.ok_or(DriverError::Vault(VaultError::Destroyed))?;
        let owner_key = self.wrap.unwrap(&wrapped).await?;
        Ok(vault::fetch(&self.objects, &owner_key, document).await?)
    }

    /// Render the document again from its stored data and the template
    /// version recorded for it, and say whether it comes out the same.
    pub async fn verify(&self, id: &str) -> Result<bool, DriverError> {
        let document = self.document(id).await?;
        let contents = self.contents(&document).await?;
        let Some(template_ref) = &document.template else {
            // A legacy document was never rendered here: its hash is all there is.
            return Ok(true);
        };
        let (record, parts) = self.template(template_ref).await?;
        let again = record.template.render(&parts, &document.language, &contents.data)?;
        Ok(again == contents.text && Some(&record.content_hash) == document.template_hash.as_ref())
    }

    async fn outcome(&self, send: &PendingSend, attempt: usize, outcome: AttemptOutcome) -> Result<(), DriverError> {
        let key = format!("outcome:{}:{}:{:?}:{attempt}", send.document, send.party, send.channel);
        let command = Command::RecordOutcome {
            document: send.document.clone(),
            party: send.party.clone(),
            channel: send.channel,
            outcome,
        };
        self.send(key, "delivery", command).await.map(|_| ())
    }

    async fn send_one(&self, send: &PendingSend, sends: &mut Sends) -> Result<(), DriverError> {
        let document = self.document(&send.document).await?;
        let delivery = document
            .deliveries
            .iter()
            .find(|d| d.recipient.party == send.party)
            .ok_or_else(|| DriverError::Answer("a pending send has no delivery".into()))?;
        let attempt = delivery.attempts.len();
        let contact = match self
            .directory
            .contact(&send.party, delivery.recipient.relationship.as_deref(), send.channel)
            .await
        {
            Ok(Some(contact)) => contact,
            Ok(None) if send.channel == crate::model::Channel::InApp => crate::channels::Contact {
                name: String::new(),
                destination: String::new(),
            },
            Ok(None) => {
                sends.no_contact_point += 1;
                return self.outcome(send, attempt, AttemptOutcome::NoContactPoint).await;
            }
            Err(_) => {
                sends.deferred += 1;
                return Ok(());
            }
        };
        let title = match &document.template {
            Some(r) => self.template(r).await?.0.template.title,
            None => "Your document".to_string(),
        };
        let message = Outbound {
            document: document.id.clone(),
            party: send.party.clone(),
            channel: send.channel,
            kind: document.kind,
            data_class: document.data_class,
            title,
            text: self.contents(&document).await?.text,
            format: document.format,
            contact,
            attempt: attempt as u32,
        };
        match self.channels.send(&message).await {
            Ok(reference) => {
                let command = Command::RecordSend {
                    document: send.document.clone(),
                    party: send.party.clone(),
                    channel: send.channel,
                    masked_destination: mask(send.channel, &message.contact.destination),
                    provider_reference: reference,
                };
                let key = format!("send:{}:{}:{:?}:{attempt}", send.document, send.party, send.channel);
                self.send(key, "delivery", command).await?;
                sends.sent += 1;
            }
            Err(SendError::Permanent(reason)) => {
                sends.refused += 1;
                self.outcome(send, attempt, AttemptOutcome::Failed { reason }).await?;
            }
            Err(SendError::Temporary(_)) => sends.deferred += 1,
        }
        Ok(())
    }

    /// Make the sends that are waiting. One that fails does not stop the
    /// others: it is counted as deferred and taken up on the next pass.
    pub async fn deliver(&self) -> Result<Sends, DriverError> {
        let pending: Vec<PendingSend> = decode(self.module.ask(Query::PendingSends { limit: self.batch }).await?)?;
        let mut sends = Sends::default();
        for send in &pending {
            if let Err(e) = self.send_one(send, &mut sends).await {
                tracing::warn!(document = %send.document, error = %e, "a send was left for the next pass");
                sends.deferred += 1;
            }
        }
        Ok(sends)
    }

    /// Record what a provider reports became of a message. `false` when no
    /// send in progress has that reference, which is not an error: reports
    /// arrive late and more than once.
    pub async fn report(&self, report: DeliveryReport) -> Result<bool, DriverError> {
        let question = Query::SendByReference {
            reference: report.provider_reference.clone(),
        };
        let Some(send): Option<PendingSend> = decode(self.module.ask(question).await?)? else {
            return Ok(false);
        };
        let command = Command::RecordOutcome {
            document: send.document,
            party: send.party,
            channel: send.channel,
            outcome: report.outcome,
        };
        match self.send(format!("report:{}", report.provider_reference), "delivery", command).await {
            Ok(_) => Ok(true),
            // The attempt already has its outcome.
            Err(DriverError::Refused(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Let the module's time pass: fall back, deem post delivered, end
    /// retention periods.
    pub async fn tick(&self) -> Result<(), DriverError> {
        let key = format!("tick:{}", (self.clock)().timestamp());
        self.send(key, "delivery", Command::Tick).await.map(|_| ())
    }

    /// Delete the vault objects of destroyed documents.
    pub async fn delete_destroyed(&self) -> Result<u32, DriverError> {
        let pending: Vec<(String, String)> = decode(self.module.ask(Query::PendingDeletes { limit: self.batch }).await?)?;
        let mut gone = vec![];
        for (document, blob) in pending {
            match self.objects.delete(&blob).await {
                Ok(()) => gone.push(document),
                // Storage still holds it under its retention lock. Its key
                // is already gone; the object is deleted when the lock ends.
                Err(VaultError::Locked(_)) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if gone.is_empty() {
            return Ok(0);
        }
        let key = format!("deleted:{}", crate::template::sha256_hex(gone.join("|").as_bytes()));
        self.send(key, "retention", Command::AckDeleted { documents: gone.clone() }).await?;
        Ok(gone.len() as u32)
    }

    /// Put the module's events on the stream, in order.
    pub async fn publish<P: EventPublisher>(&self, publisher: &P) -> Result<u32, DriverError> {
        let events: Vec<EventRecord> = decode(self.module.ask(Query::PendingEvents { limit: self.batch }).await?)?;
        let Some(last) = events.last() else {
            return Ok(0);
        };
        publisher.publish(&events).await.map_err(DriverError::Unavailable)?;
        let command = Command::MarkPublished { through_seq: last.seq };
        self.send(format!("published:{}", last.seq), "relay", command).await?;
        Ok(events.len() as u32)
    }

    /// Carry the module's instructions to the core.
    pub async fn instruct_core<L: CoreLink>(&self, core: &L) -> Result<u32, DriverError> {
        let outbox: Vec<(String, CoreInstruction)> = decode(self.module.ask(Query::CoreOutbox { limit: self.batch }).await?)?;
        let mut done = vec![];
        let mut failed = None;
        for (id, instruction) in &outbox {
            match core.instruct(id, instruction).await {
                Ok(()) => done.push(id.clone()),
                Err(e) => failed = Some(e),
            }
        }
        if !done.is_empty() {
            let key = format!("core-ack:{}", crate::template::sha256_hex(done.join("|").as_bytes()));
            self.send(key, "relay", Command::AckCoreInstructions { ids: done.clone() }).await?;
        }
        match failed {
            Some(e) => Err(DriverError::Unavailable(format!("the core did not take an instruction: {e}"))),
            None => Ok(done.len() as u32),
        }
    }

    /// One pass of everything. A step that fails is reported and does not
    /// stop the steps after it.
    pub async fn cycle<P: EventPublisher, L: CoreLink>(&self, publisher: &P, core: Option<&L>) -> Cycle {
        let mut cycle = Cycle::default();
        let mut note = |step: &str, e: DriverError| cycle.errors.push(format!("{step}: {e}"));
        if let Err(e) = self.tick().await {
            note("tick", e);
        }
        match self.deliver().await {
            Ok(sends) => cycle.sends = sends,
            Err(e) => cycle.errors.push(format!("deliver: {e}")),
        }
        match self.delete_destroyed().await {
            Ok(n) => cycle.deleted = n,
            Err(e) => cycle.errors.push(format!("delete: {e}")),
        }
        if let Some(core) = core {
            match self.instruct_core(core).await {
                Ok(n) => cycle.instructed = n,
                Err(e) => cycle.errors.push(format!("core: {e}")),
            }
        }
        match self.publish(publisher).await {
            Ok(n) => cycle.published = n,
            Err(e) => cycle.errors.push(format!("publish: {e}")),
        }
        cycle
    }
}

/// A publisher that keeps events in memory, for the insurance in a box and
/// tests.
#[derive(Clone, Default)]
pub struct MemoryPublisher {
    pub events: Arc<Mutex<Vec<EventRecord>>>,
    down: Arc<Mutex<bool>>,
}

impl MemoryPublisher {
    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }

    pub fn seen(&self) -> Vec<EventRecord> {
        self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl EventPublisher for MemoryPublisher {
    async fn publish(&self, events: &[EventRecord]) -> Result<(), String> {
        if *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err("the stream is unavailable".into());
        }
        self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(events);
        Ok(())
    }
}

/// The module's events on an Azure Event Hub, for claims, the portal,
/// complaints and reporting to read.
#[derive(Clone)]
pub struct EventHubsPublisher {
    namespace: String,
    hub: String,
    credential: ic_azure::credential::AzureCredential,
    http: reqwest::Client,
}

impl EventHubsPublisher {
    /// `namespace` is `https://<namespace>.servicebus.windows.net`.
    pub fn new(namespace: &str, hub: &str, credential: ic_azure::credential::AzureCredential) -> Self {
        Self {
            namespace: namespace.trim_end_matches('/').to_string(),
            hub: hub.to_string(),
            credential,
            http: ic_azure::http_client(),
        }
    }
}

impl EventPublisher for EventHubsPublisher {
    async fn publish(&self, events: &[EventRecord]) -> Result<(), String> {
        let token = self.credential.token(ic_azure::credential::SCOPE_EVENT_HUBS).await.map_err(|e| e.to_string())?;
        let batch: Vec<Value> = events
            .iter()
            .map(|e| {
                serde_json::json!({
                    "Body": serde_json::to_string(e).unwrap_or_default(),
                    // Consumers de-duplicate and detect gaps from these.
                    "UserProperties": { "event-id": e.id, "seq": e.seq },
                })
            })
            .collect();
        // One partition, so the module's events stay in their order.
        let url = format!("{}/{}/partitions/0/messages?api-version=2014-01&timeout=60", self.namespace, self.hub);
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .header("content-type", "application/vnd.microsoft.servicebus.json")
            .body(serde_json::to_vec(&batch).unwrap_or_default())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Event Hubs returned {}", resp.status().as_u16()));
        }
        Ok(())
    }
}
