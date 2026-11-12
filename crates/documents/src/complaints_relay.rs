//! Time and the event stream for the complaints module.
//!
//! The complaints module (`ic-complaints`) is a state machine and nothing
//! else: something must tell it that time has passed, and carry its events
//! to the stream. The documents service does both, because it is the
//! service that answers those events with letters. It also feeds the
//! conduct register: a module cannot call another while it applies a
//! command, so the relay asks the commission module which licence events
//! have opened a case and proposes each to the complaints module. Like the
//! rest of the driver this holds no state and is safe to stop anywhere.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use ic_complaints::conduct::licence_case_id;
use ic_complaints::model::{ConductCase, ConductKind};
use ic_complaints::module::{Query, Role, NAME};
use ic_complaints::state::MAX_CHUNK;
use ic_complaints::{Applied, Command, ComplaintsState, Envelope, EventRecord};
use ic_core::{PartyId, PolicyId};
use ic_log::StateMachine;
use ic_raft::ClusterClient;
use serde::Deserialize;
use serde_json::Value;

use crate::driver::{DriverError, EventHubsPublisher};

/// The complaints module, wherever it runs.
pub trait Complaints: Send + Sync {
    fn propose(&self, envelope: Envelope) -> impl Future<Output = Result<Applied, DriverError>> + Send;
    fn ask(&self, query: Query) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// The module in its replicated group on the cluster.
#[derive(Clone)]
pub struct ClusterComplaints {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterComplaints {
    /// `patience` is how long one command keeps looking for a leader.
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Complaints for ClusterComplaints {
    async fn propose(&self, envelope: Envelope) -> Result<Applied, DriverError> {
        let command = serde_json::to_value(&envelope).map_err(|e| DriverError::Answer(e.to_string()))?;
        let reply = self
            .client
            .propose_module_until(NAME, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let applied = reply.result.map_err(DriverError::Answer)?;
        serde_json::from_value(applied).map_err(|e| DriverError::Answer(e.to_string()))
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        let question = serde_json::to_value(&query).map_err(|e| DriverError::Answer(e.to_string()))?;
        self.client
            .query_module(NAME, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(DriverError::Answer)
    }
}

/// The module in this process, for the insurance in a box and tests. It
/// can be made unavailable to play a lost cluster.
#[derive(Clone, Default)]
pub struct LocalComplaints {
    inner: Arc<Mutex<(ComplaintsState, u64)>>,
    down: Arc<Mutex<bool>>,
}

impl LocalComplaints {
    fn lock(&self) -> std::sync::MutexGuard<'_, (ComplaintsState, u64)> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_down(&self) -> bool {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }

    /// Read the state as it stands.
    pub fn read<T>(&self, f: impl FnOnce(&ComplaintsState) -> T) -> T {
        f(&self.lock().0)
    }
}

impl Complaints for LocalComplaints {
    async fn propose(&self, envelope: Envelope) -> Result<Applied, DriverError> {
        if self.is_down() {
            return Err(DriverError::Unavailable("no leader".into()));
        }
        let mut inner = self.lock();
        inner.1 += 1;
        let index = inner.1;
        Ok(inner.0.apply(index, envelope))
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        if self.is_down() {
            return Err(DriverError::Unavailable("no leader".into()));
        }
        let question = serde_json::to_value(&query).map_err(|e| DriverError::Answer(e.to_string()))?;
        ic_complaints::module::answer(&self.lock().0, question).map_err(DriverError::Answer)
    }
}

/// Where the complaints module's events go.
pub trait ComplaintsStream: Send + Sync {
    /// Publish in order. All or nothing: on failure the same events are
    /// offered again.
    fn publish(&self, events: &[EventRecord]) -> impl Future<Output = Result<(), String>> + Send;
}

/// The events on an Azure Event Hub of one partition, so they stay in order.
impl ComplaintsStream for EventHubsPublisher {
    async fn publish(&self, events: &[EventRecord]) -> Result<(), String> {
        let items: Vec<(&str, u64, String)> = events
            .iter()
            .map(|e| (e.id.as_str(), e.seq, serde_json::to_string(e).unwrap_or_default()))
            .collect();
        self.publish_items(&items).await
    }
}

/// A stream that keeps what it is given, for the insurance in a box and tests.
#[derive(Clone, Default)]
pub struct MemoryStream {
    events: Arc<Mutex<Vec<EventRecord>>>,
    down: Arc<Mutex<bool>>,
}

impl MemoryStream {
    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }

    pub fn seen(&self) -> Vec<EventRecord> {
        self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl ComplaintsStream for MemoryStream {
    async fn publish(&self, events: &[EventRecord]) -> Result<(), String> {
        if *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err("the stream is unavailable".into());
        }
        self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(events);
        Ok(())
    }
}

/// A conduct case as the commission module reports it: the licence event
/// and the advisers and policies it affects. Fields this relay does not
/// use are left out, so that module can add to its answer freely.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LicenceCase {
    /// The regulator's notice reference: the event's id.
    pub licence_event: String,
    pub party: PartyId,
    /// `suspended`, `revoked` or `lapsed`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub effective: Option<NaiveDate>,
    #[serde(default)]
    pub advisers: Vec<PartyId>,
    #[serde(default)]
    pub policies: Vec<PolicyId>,
}

/// Where licence cases and the owners of their policies are read from.
pub trait LicenceFeed: Send + Sync {
    /// Every conduct case a licence event has opened in the commission module.
    fn cases(&self) -> impl Future<Output = Result<Vec<LicenceCase>, String>> + Send;
    /// The policyholder of a policy: the customer to be told. `None` if no
    /// partition holds the policy.
    fn policyholder(&self, policy: &PolicyId) -> impl Future<Output = Result<Option<PartyId>, String>> + Send;
}

/// The commission module's name on the nodes.
pub const COMMISSION_MODULE: &str = "distribution";

/// The commission module and the policy directory on the cluster.
#[derive(Clone)]
pub struct ClusterLicences {
    client: ClusterClient,
    partitions: u16,
}

impl ClusterLicences {
    pub fn new(client: ClusterClient, partitions: u16) -> Self {
        Self { client, partitions }
    }
}

impl LicenceFeed for ClusterLicences {
    async fn cases(&self) -> Result<Vec<LicenceCase>, String> {
        let answer = self
            .client
            .query_module(COMMISSION_MODULE, &Value::String("conduct_cases".into()))
            .await
            .map_err(|e| e.to_string())??;
        serde_json::from_value(answer).map_err(|e| format!("the commission module's conduct cases could not be read: {e}"))
    }

    async fn policyholder(&self, policy: &PolicyId) -> Result<Option<PartyId>, String> {
        for partition in 0..self.partitions {
            let Some(entry) = self.client.locate(partition, policy).await.map_err(|e| e.to_string())? else {
                continue;
            };
            let view = self.client.policy_by_number(partition, &entry.number).await.map_err(|e| e.to_string())?;
            return Ok(view.map(|v| v.policy.policyholder));
        }
        Ok(None)
    }
}

/// A deployment with no commission module: there are no licence cases.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLicences;

impl LicenceFeed for NoLicences {
    async fn cases(&self) -> Result<Vec<LicenceCase>, String> {
        Ok(vec![])
    }

    async fn policyholder(&self, _policy: &PolicyId) -> Result<Option<PartyId>, String> {
        Ok(None)
    }
}

/// Licence cases and policyholders held in memory, for the insurance in a
/// box and tests.
#[derive(Clone, Default)]
pub struct MemoryLicences {
    pub cases: Arc<Mutex<Vec<LicenceCase>>>,
    pub policyholders: Arc<Mutex<BTreeMap<PolicyId, PartyId>>>,
    down: Arc<Mutex<bool>>,
}

impl MemoryLicences {
    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = down;
    }

    fn check(&self) -> Result<(), String> {
        if *self.down.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err("the commission module is unavailable".into());
        }
        Ok(())
    }
}

impl LicenceFeed for MemoryLicences {
    async fn cases(&self) -> Result<Vec<LicenceCase>, String> {
        self.check()?;
        Ok(self.cases.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }

    async fn policyholder(&self, policy: &PolicyId) -> Result<Option<PartyId>, String> {
        self.check()?;
        Ok(self
            .policyholders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(policy)
            .cloned())
    }
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelayCycle {
    /// Escalations, deadlocks and removals the tick produced.
    pub raised: u32,
    pub published: u32,
    /// Licence cases whose population was completed in this pass.
    pub cases: u32,
    pub errors: Vec<String>,
}

pub struct Relay<M> {
    pub module: M,
    /// Who the relay's commands are recorded as.
    pub actor: String,
    /// The owner given to a conduct case a licence event opens.
    pub conduct_officer: String,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    /// The most events one publish carries.
    pub batch: usize,
}

impl<M: Complaints> Relay<M> {
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

    /// Let the module's time pass: warnings, escalations, deadlock by time,
    /// records past their retention. Returns how many events it raised.
    pub async fn tick(&self) -> Result<u32, DriverError> {
        let key = format!("tick:{}", (self.clock)().timestamp());
        Ok(self.send(key, "scheduler", Command::Tick).await?.events.len() as u32)
    }

    /// Put the module's events on the stream, in order. An event is
    /// acknowledged only after the stream has it, so a crash between the
    /// two publishes it again and consumers de-duplicate on its id.
    pub async fn publish<P: ComplaintsStream>(&self, stream: &P) -> Result<u32, DriverError> {
        let pending = self.module.ask(Query::PendingEvents { limit: self.batch }).await?;
        let events: Vec<EventRecord> = serde_json::from_value(pending).map_err(|e| DriverError::Answer(e.to_string()))?;
        let Some(last) = events.last() else {
            return Ok(0);
        };
        stream.publish(&events).await.map_err(DriverError::Unavailable)?;
        let command = Command::MarkPublished { through_seq: last.seq };
        self.send(format!("published:{}", last.seq), "relay", command).await?;
        Ok(events.len() as u32)
    }

    async fn conduct_case(&self, id: &str) -> Result<Option<ConductCase>, DriverError> {
        let query = Query::ConductCase {
            id: id.to_string(),
            role: Role::Conduct,
        };
        serde_json::from_value(self.module.ask(query).await?).map_err(|e| DriverError::Answer(e.to_string()))
    }

    /// Open a conduct case for each licence event the commission module
    /// reports, attach its advisers, policies and their policyholders, and
    /// complete the population. Every command is safe to repeat, so a pass
    /// cut short is finished by the next. A case whose population is
    /// already complete is left alone. Returns the cases completed.
    pub async fn forward_licence_cases<L: LicenceFeed>(&self, feed: &L) -> Result<u32, DriverError> {
        let reported = feed.cases().await.map_err(DriverError::Unavailable)?;
        let mut completed = 0;
        let mut failed = None;
        for case in reported {
            match self.forward_one(feed, &case).await {
                Ok(true) => completed += 1,
                Ok(false) => {}
                // One case that cannot be finished does not hold up the rest.
                Err(e) => failed = Some(format!("{}: {e}", case.licence_event)),
            }
        }
        match failed {
            Some(e) => Err(DriverError::Unavailable(format!("a licence case could not be forwarded: {e}"))),
            None => Ok(completed),
        }
    }

    async fn forward_one<L: LicenceFeed>(&self, feed: &L, case: &LicenceCase) -> Result<bool, DriverError> {
        let (Some(status), Some(effective)) = (case.status.clone(), case.effective) else {
            return Err(DriverError::Answer("the commission module reported no licence event for the case".into()));
        };
        let id = licence_case_id(&case.licence_event);
        // Each of the three commands changes nothing when repeated, so a
        // retry goes under a new key: a refusal met once, such as timers
        // not yet active, is not remembered against the case for ever.
        let attempt = (self.clock)().timestamp();
        if self.conduct_case(&id).await?.is_some_and(|c| c.population_complete) {
            return Ok(false);
        }
        // Everyone to be told is known before anything is attached.
        let mut customers = BTreeSet::new();
        for policy in &case.policies {
            match feed.policyholder(policy).await.map_err(DriverError::Unavailable)? {
                Some(party) => customers.insert(party),
                None => return Err(DriverError::NotFound(format!("policy {policy}"))),
            };
        }
        self.send(
            format!("licence-open:{id}:{attempt}"),
            "distribution",
            Command::OpenConductCase {
                case: id.clone(),
                kind: ConductKind::LicenceEvent {
                    licence_event: case.licence_event.clone(),
                    party: case.party.clone(),
                    status,
                    effective,
                },
                owner: self.conduct_officer.clone(),
                summary_document: None,
                notify_regulator: true,
            },
        )
        .await?;
        let customers: Vec<PartyId> = customers.into_iter().collect();
        let chunks = customers
            .chunks(MAX_CHUNK)
            .map(|c| (c.to_vec(), vec![], vec![]))
            .chain(case.advisers.chunks(MAX_CHUNK).map(|a| (vec![], a.to_vec(), vec![])))
            .chain(case.policies.chunks(MAX_CHUNK).map(|p| (vec![], vec![], p.to_vec())));
        for (n, (customers, advisers, policies)) in chunks.enumerate() {
            let attach = Command::AttachPopulation {
                case: id.clone(),
                customers,
                advisers,
                policies,
            };
            self.send(format!("licence-population:{id}:{n}:{attempt}"), "distribution", attach).await?;
        }
        // Completed with what the case now holds, read back: the counts
        // confirm every chunk arrived.
        let Some(held) = self.conduct_case(&id).await? else {
            return Err(DriverError::NotFound(format!("conduct case {id}")));
        };
        let (customers, policies) = (held.customers.len() as u32, held.policies.len() as u32);
        let complete = Command::CompletePopulation {
            case: id.clone(),
            customers,
            policies,
        };
        self.send(format!("licence-complete:{id}:{attempt}"), "distribution", complete).await?;
        Ok(true)
    }

    /// One pass. A step that fails is reported and does not stop the others.
    pub async fn cycle<P: ComplaintsStream, L: LicenceFeed>(&self, stream: &P, feed: &L) -> RelayCycle {
        let mut cycle = RelayCycle::default();
        match self.forward_licence_cases(feed).await {
            Ok(cases) => cycle.cases = cases,
            Err(e) => cycle.errors.push(format!("licence cases: {e}")),
        }
        match self.tick().await {
            Ok(raised) => cycle.raised = raised,
            Err(e) => cycle.errors.push(format!("tick: {e}")),
        }
        match self.publish(stream).await {
            Ok(published) => cycle.published = published,
            Err(e) => cycle.errors.push(format!("publish: {e}")),
        }
        cycle
    }
}

/// Run the relay every `every` until `stop` completes. A pass that could
/// not do everything says so in the log and the next one tries again.
pub async fn run_relay<M: Complaints, P: ComplaintsStream, L: LicenceFeed>(
    relay: Relay<M>,
    stream: P,
    feed: L,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&RelayCycle),
) {
    tokio::pin!(stop);
    loop {
        let cycle = relay.cycle(&stream, &feed).await;
        if cycle.errors.is_empty() {
            tracing::debug!(
                raised = cycle.raised,
                published = cycle.published,
                cases = cycle.cases,
                "complaints relay cycle"
            );
        } else {
            tracing::warn!(errors = ?cycle.errors, "a complaints relay cycle could not do everything");
        }
        seen(&cycle);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}
