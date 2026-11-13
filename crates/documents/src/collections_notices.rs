//! The dunning notices the collections module writes.
//!
//! Collections decides when a customer in arrears is reminded, told their
//! cover is suspended, or given final notice before lapse; it cannot issue
//! a document. This step takes each notice it has written, issues it from
//! its template to the customer (and, for the mandatory notices, to every
//! lender or other interested party standing on the policies), and tells
//! collections it was sent. The document id is made from the notice, so a
//! pass cut short issues nothing twice.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use ic_collections::model::Notice;
use ic_collections::{Applied, CollectionsState, Command, Envelope, MODULE};
use ic_core::PartyId;
use ic_log::StateMachine;
use ic_raft::ClusterClient;
use serde_json::{json, Value};

use crate::channels::{Channels, ContactDirectory};
use crate::complaint_letters::{LetterPass, Letters};
use crate::crypto::KeyWrap;
use crate::driver::{Driver, DriverError, IssueRequest, Module};
use crate::kinds::DataClass;
use crate::model::{Channel, Recipient, RecipientRole};
use crate::module::Query;
use crate::vault::ObjectStore;

/// The collections module, wherever it runs.
pub trait Collections: Send + Sync {
    fn propose(&self, envelope: Envelope) -> impl Future<Output = Result<Applied, DriverError>> + Send;
    fn ask(&self, question: Value) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// The collections module on the cluster.
#[derive(Clone)]
pub struct ClusterCollections {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterCollections {
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Collections for ClusterCollections {
    async fn propose(&self, envelope: Envelope) -> Result<Applied, DriverError> {
        let command = serde_json::to_value(&envelope).map_err(|e| DriverError::Answer(e.to_string()))?;
        let reply = self
            .client
            .propose_module_until(MODULE, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let applied = reply.result.map_err(DriverError::Answer)?;
        serde_json::from_value(applied).map_err(|e| DriverError::Answer(e.to_string()))
    }

    async fn ask(&self, question: Value) -> Result<Value, DriverError> {
        self.client
            .query_module(MODULE, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(DriverError::Answer)
    }
}

/// The module in this process, for the insurance in a box and tests.
#[derive(Clone, Default)]
pub struct LocalCollections {
    inner: Arc<Mutex<(CollectionsState, u64)>>,
    down: Arc<Mutex<bool>>,
}

impl LocalCollections {
    /// A module that starts from `state`, applied up to `index`.
    pub fn from_state(state: CollectionsState, index: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new((state, index))),
            down: Arc::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (CollectionsState, u64)> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_down(&self) -> bool {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }

    /// Read the state as it stands.
    pub fn read<T>(&self, f: impl FnOnce(&CollectionsState) -> T) -> T {
        f(&self.lock().0)
    }
}

impl Collections for LocalCollections {
    async fn propose(&self, envelope: Envelope) -> Result<Applied, DriverError> {
        if self.is_down() {
            return Err(DriverError::Unavailable("no leader".into()));
        }
        let mut inner = self.lock();
        inner.1 += 1;
        let index = inner.1;
        Ok(inner.0.apply(index, envelope))
    }

    async fn ask(&self, question: Value) -> Result<Value, DriverError> {
        if self.is_down() {
            return Err(DriverError::Unavailable("no leader".into()));
        }
        ic_collections::query::answer(&self.lock().0, question).map_err(DriverError::Answer)
    }
}

/// The document a notice is, the same on every pass.
pub fn document_id(notice: &str) -> String {
    format!("collections:{notice}")
}

/// The channels a notice goes on, from the channel its step names.
pub fn channels_for(channel: ic_collections::Channel) -> Vec<Channel> {
    use ic_collections::Channel as C;
    match channel {
        C::Email => vec![Channel::Email],
        C::Sms => vec![Channel::Sms],
        C::App => vec![Channel::InApp],
        // A call step has no notice; a notice on it goes by post.
        C::Post | C::Queue => vec![Channel::Post],
    }
}

pub struct NoticeRelay<K> {
    pub collections: K,
    pub actor: String,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    /// The most notices one pass takes.
    pub batch: usize,
}

impl<K: Collections> NoticeRelay<K> {
    async fn request<M, S, W, C, D>(&self, driver: &Driver<M, S, W, C, D>, notice: &Notice) -> Result<IssueRequest, DriverError>
    where
        M: Module,
        S: ObjectStore,
        W: KeyWrap,
        C: Channels,
        D: ContactDirectory,
    {
        // The person's name and needs if the customer master has them. A
        // notice to someone it cannot reach is still issued: its delivery
        // then fails, and collections opens a task to find them.
        let contact = Letters::person(driver, &notice.party)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let mut recipients = vec![Recipient {
            party: notice.party.clone(),
            role: RecipientRole::Addressee,
            relationship: Some("policyholder".into()),
            channels: channels_for(notice.channel),
        }];
        if notice.kind.mandatory() {
            let mut seen: BTreeSet<PartyId> = BTreeSet::from([notice.party.clone()]);
            for policy in &notice.policies {
                let answer = driver
                    .module
                    .ask(Query::InterestedParties {
                        policy: policy.clone(),
                        on: notice.on,
                    })
                    .await?;
                let parties: Vec<PartyId> = serde_json::from_value(answer).map_err(|e| DriverError::Answer(e.to_string()))?;
                for party in parties.into_iter().filter(|p| seen.insert(p.clone())) {
                    recipients.push(Recipient {
                        party,
                        role: RecipientRole::InterestedParty,
                        relationship: None,
                        channels: vec![Channel::Email, Channel::Post],
                    });
                }
            }
        }
        let policy = notice.policies.first().cloned();
        let others = notice.policies.iter().skip(1).cloned().collect();
        Ok(IssueRequest {
            document: document_id(&notice.id),
            caller: "documents".into(),
            template: notice.template.clone(),
            template_version: None,
            policy,
            other_policies: others,
            data_class: DataClass::Financial,
            lives: vec![],
            prove_delivery: notice.proof_of_delivery,
            owner: notice.party.clone(),
            recipients,
            language: contact.as_ref().and_then(|c| c.language.clone()).unwrap_or_else(|| "en".into()),
            // `standard` or an unknown format is the ordinary one.
            format: contact.as_ref().and_then(|c| serde_json::from_value(json!(c.format)).ok()),
            data: json!({
                "name": contact.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                "reference": notice.id,
                "date": notice.on.to_string(),
                "step": notice.kind.name(),
                "policies": notice.policies.iter().map(|p| p.0.clone()).collect::<Vec<_>>(),
            }),
            trigger_event: Some(document_id(&notice.id)),
            case: None,
            supersedes: None,
            reason: None,
            wordings: vec![],
            components: vec![],
            notice: None,
        })
    }

    async fn tell(&self, key: String, command: Command) -> Result<(), DriverError> {
        let at = (self.clock)();
        let applied = self
            .collections
            .propose(Envelope {
                key,
                caller: ic_collections::machine::DELIVERY.into(),
                actor: self.actor.clone(),
                roles: vec![],
                at,
                command,
            })
            .await?;
        applied.result.map_err(|r| DriverError::Refused(ic_core::Reject::new(&r.code, r.message)))
    }

    async fn one<M, S, W, C, D>(&self, driver: &Driver<M, S, W, C, D>, notice: &Notice) -> Result<(), DriverError>
    where
        M: Module,
        S: ObjectStore,
        W: KeyWrap,
        C: Channels,
        D: ContactDirectory,
    {
        let request = self.request(driver, notice).await?;
        match driver.issue(request.clone()).await {
            Err(DriverError::Refused(reject)) if reject.code == "no_format" => driver.issue(IssueRequest { format: None, ..request }).await?,
            other => other?,
        };
        self.tell(format!("notice-sent:{}", notice.id), Command::RecordNoticeSent { notice: notice.id.clone() })
            .await
    }

    /// One pass over the notices collections has written.
    pub async fn pass<M, S, W, C, D>(&self, driver: &Driver<M, S, W, C, D>) -> LetterPass
    where
        M: Module,
        S: ObjectStore,
        W: KeyWrap,
        C: Channels,
        D: ContactDirectory,
    {
        let mut pass = LetterPass::default();
        let asked = self.collections.ask(json!({ "q": "notices_due", "limit": self.batch })).await;
        let due = asked.and_then(|a| serde_json::from_value::<Vec<Notice>>(a).map_err(|e| DriverError::Answer(e.to_string())));
        let due = match due {
            Ok(due) => due,
            Err(e) => {
                pass.errors.push(format!("notices due: {e}"));
                return pass;
            }
        };
        // A template with no wording is said once, not once a notice.
        let mut no_wording = BTreeSet::new();
        for notice in &due {
            if no_wording.contains(&notice.template) {
                pass.left += 1;
                continue;
            }
            match self.one(driver, notice).await {
                Ok(()) => pass.issued += 1,
                Err(DriverError::NotFound(what)) if what.contains("template") => {
                    no_wording.insert(notice.template.clone());
                    pass.left += 1;
                    pass.errors
                        .push(format!("template {} has no active version; its notices wait", notice.template));
                }
                Err(e) => pass.errors.push(format!("{}: {e}", document_id(&notice.id))),
            }
        }
        pass
    }
}

/// Run the relay every `every` until `stop` completes.
pub async fn run_notices<K, M, S, W, C, D>(
    relay: NoticeRelay<K>,
    driver: Arc<Driver<M, S, W, C, D>>,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&LetterPass),
) where
    K: Collections,
    M: Module,
    S: ObjectStore,
    W: KeyWrap,
    C: Channels,
    D: ContactDirectory,
{
    tokio::pin!(stop);
    loop {
        let pass = relay.pass(&driver).await;
        if pass.errors.is_empty() {
            tracing::debug!(issued = pass.issued, "collections notices pass");
        } else {
            tracing::warn!(issued = pass.issued, left = pass.left, errors = ?pass.errors, "a collections notices pass could not do everything");
        }
        seen(&pass);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}
