//! Time and the event stream for the complaints module.
//!
//! The complaints module (`ic-complaints`) is a state machine and nothing
//! else: something must tell it that time has passed, and carry its events
//! to the stream. The documents service does both, because it is the
//! service that answers those events with letters. Like the rest of the
//! driver this holds no state and is safe to stop anywhere.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use ic_complaints::module::{Query, NAME};
use ic_complaints::{Applied, Command, ComplaintsState, Envelope, EventRecord};
use ic_log::StateMachine;
use ic_raft::ClusterClient;
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

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelayCycle {
    /// Escalations, deadlocks and removals the tick produced.
    pub raised: u32,
    pub published: u32,
    pub errors: Vec<String>,
}

pub struct Relay<M> {
    pub module: M,
    /// Who the relay's commands are recorded as.
    pub actor: String,
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

    /// One pass. A step that fails is reported and does not stop the other.
    pub async fn cycle<P: ComplaintsStream>(&self, stream: &P) -> RelayCycle {
        let mut cycle = RelayCycle::default();
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
pub async fn run_relay<M: Complaints, P: ComplaintsStream>(
    relay: Relay<M>,
    stream: P,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&RelayCycle),
) {
    tokio::pin!(stop);
    loop {
        let cycle = relay.cycle(&stream).await;
        if cycle.errors.is_empty() {
            tracing::debug!(raised = cycle.raised, published = cycle.published, "complaints relay cycle");
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
