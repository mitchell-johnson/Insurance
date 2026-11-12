//! The driver between the module and the core.
//!
//! The module's state machine decides; the driver carries. Each pass reads
//! the oldest instructions from the module's outbox, sends each to the core
//! as caller `reinsurance` with the instruction's ID as its idempotency key,
//! and acknowledges what the core has taken. Instructions go in the order
//! they were issued, because a later cession for a cover replaces an earlier
//! one. A driver that stops anywhere is restarted and carries on: the core
//! applies an instruction once however often it is sent.

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::Reject;
use ic_ledger::PartitionId;
use ic_raft::ClusterClient;
use serde_json::Value;

use crate::delivery::CoreDelivery;
use crate::host::{self, Query};
use crate::machine::{Applied, Audience, Command, Envelope, Instruction, InstructionBody, Notice, Reinsurance};

/// The name the driver sends its own commands to the module under.
pub const DRIVER: &str = "driver";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    /// The module or the core could not be reached. Nothing is known to
    /// have happened: the next pass does the same again.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("unexpected answer: {0}")]
    Answer(String),
}

/// The reinsurance module, wherever it runs.
pub trait Module: Send + Sync {
    fn propose(&self, envelope: Envelope) -> impl Future<Output = Result<Applied, DriverError>> + Send;
    fn ask(&self, query: Query) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// The core's partitions.
pub trait Core: Send + Sync {
    /// The partition's open business date. `None` before its first day.
    fn business_date(&self, partition: PartitionId) -> impl Future<Output = Result<Option<NaiveDate>, DriverError>> + Send;
    /// Send one command. The inner result is the core's own answer.
    fn send(&self, delivery: &CoreDelivery) -> impl Future<Output = Result<Result<(), Reject>, DriverError>> + Send;
}

/// What became of a notice offered to its audience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handed {
    /// Taken, now or on an earlier try.
    Taken,
    /// The audience refused it, with its own code and message.
    Refused(Reject),
}

/// Whoever the module's notices are for.
pub trait Audiences: Send + Sync {
    /// The audiences this carrier can reach. Notices for any other wait in
    /// the module's outbox.
    fn reached(&self) -> Vec<Audience>;
    /// Hand one notice on. `seq` is its number in the outbox, for the
    /// idempotency key; `on` is the business date.
    fn hand_on(&self, seq: u64, notice: &Notice, on: NaiveDate) -> impl Future<Output = Result<Handed, DriverError>> + Send;
}

/// The name of the group administration module's replicated group.
pub const GROUP_MODULE: &str = "group";

/// The audiences that are other modules on the cluster.
#[derive(Clone)]
pub struct ClusterAudiences {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterAudiences {
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

/// A notice for group administration as that module's command, in its own
/// wire format. `None` for a notice that is not for it.
pub fn group_command(seq: u64, notice: &Notice, on: NaiveDate) -> Option<Value> {
    let Notice::RequireUnderwrittenExcess {
        policy,
        life,
        cover,
        excess_minor,
        reference,
    } = notice
    else {
        return None;
    };
    Some(serde_json::json!({
        "key": format!("ri-notice:{seq}"),
        "caller": crate::delivery::CALLER,
        "actor": "reinsurance-driver",
        "today": on,
        "command": {"require_underwritten_excess": {
            "policy": policy,
            "life": life,
            "cover": cover,
            "excess_minor": excess_minor,
            "reference": reference,
        }},
    }))
}

impl Audiences for ClusterAudiences {
    fn reached(&self) -> Vec<Audience> {
        vec![Audience::Group]
    }

    async fn hand_on(&self, seq: u64, notice: &Notice, on: NaiveDate) -> Result<Handed, DriverError> {
        let command = group_command(seq, notice, on).ok_or_else(|| DriverError::Answer(format!("notice {seq} is not for a module this carrier reaches")))?;
        let reply = self
            .client
            .propose_module_until(GROUP_MODULE, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        // The host refuses what the module cannot read; the module refuses
        // in its own answer.
        let outcome = match reply.result {
            Ok(outcome) => outcome,
            Err(unreadable) => return Ok(Handed::Refused(Reject::new("unreadable", unreadable))),
        };
        let result: Result<(), Reject> =
            serde_json::from_value(outcome.get("result").cloned().unwrap_or(Value::Null)).map_err(|e| DriverError::Answer(e.to_string()))?;
        Ok(result.map_or_else(Handed::Refused, |()| Handed::Taken))
    }
}

/// What one round of handing notices on did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Round {
    pub taken: u32,
    /// Set aside in the module with an exception for a person.
    pub refused: u32,
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
            .propose_module_until(host::MODULE, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let applied = reply.result.map_err(DriverError::Answer)?;
        serde_json::from_value(applied).map_err(|e| DriverError::Answer(e.to_string()))
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        let question = serde_json::to_value(&query).map_err(|e| DriverError::Answer(e.to_string()))?;
        self.client
            .query_module(host::MODULE, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(DriverError::Answer)
    }
}

/// The core's partitions on the cluster.
#[derive(Clone)]
pub struct ClusterCore {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterCore {
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Core for ClusterCore {
    async fn business_date(&self, partition: PartitionId) -> Result<Option<NaiveDate>, DriverError> {
        self.client.business_date(partition).await.map_err(|e| DriverError::Unavailable(e.to_string()))
    }

    async fn send(&self, delivery: &CoreDelivery) -> Result<Result<(), Reject>, DriverError> {
        let envelope = ic_core::Envelope {
            key: delivery.key.clone(),
            caller: delivery.caller.to_string(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command: delivery.command.clone(),
        };
        let reply = self
            .client
            .propose_until(delivery.partition, &envelope, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        Ok(reply.rejected.map_or(Ok(()), Err))
    }
}

/// The module in this process, for tests and a single-process setup. It can
/// be made unavailable to play a lost cluster.
#[derive(Clone, Default)]
pub struct LocalModule {
    inner: Arc<Mutex<(Reinsurance, u64)>>,
    down: Arc<Mutex<bool>>,
}

impl LocalModule {
    fn lock(&self) -> std::sync::MutexGuard<'_, (Reinsurance, u64)> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A copy of the state as it stands.
    pub fn state(&self) -> Reinsurance {
        self.lock().0.clone()
    }

    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(PoisonError::into_inner) = down;
    }

    fn check(&self) -> Result<(), DriverError> {
        if *self.down.lock().unwrap_or_else(PoisonError::into_inner) {
            return Err(DriverError::Unavailable("no node answered".into()));
        }
        Ok(())
    }

    /// Apply a command as the group's log would.
    pub fn apply(&self, envelope: Envelope) -> Applied {
        let mut inner = self.lock();
        inner.1 += 1;
        let index = inner.1;
        inner.0.apply_envelope(index, envelope)
    }
}

impl Module for LocalModule {
    async fn propose(&self, envelope: Envelope) -> Result<Applied, DriverError> {
        self.check()?;
        Ok(self.apply(envelope))
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        self.check()?;
        let question = serde_json::to_value(&query).map_err(|e| DriverError::Answer(e.to_string()))?;
        host::answer(&self.lock().0, question).map_err(DriverError::Answer)
    }
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    /// Instructions the core applied, or had applied before.
    pub delivered: u32,
    /// Cessions the core refused because the policy was moving between
    /// partitions. The module sends the cover's whole record again when the
    /// transfer lands or is abandoned.
    pub dropped: u32,
    /// The instruction delivery stopped at, and the core's refusal.
    pub held: Option<(u64, Reject)>,
    /// A partition with no business date yet: its instructions wait.
    pub waiting_on: Option<PartitionId>,
}

impl Pass {
    /// The pass did everything there was to do.
    pub fn clear(&self) -> bool {
        self.held.is_none() && self.waiting_on.is_none()
    }
}

/// A cession for a policy in transfer is refused by the partition it left,
/// and is not owed: the record travels with the policy.
fn superseded(instruction: &Instruction, refusal: &Reject) -> bool {
    matches!(instruction.body, InstructionBody::RecordCession { .. }) && matches!(refusal.code.as_str(), "policy_transferring" | "policy_transferred")
}

pub struct Driver<M, C> {
    pub module: M,
    pub core: C,
    /// Instructions read from the outbox at a time.
    pub batch: usize,
}

impl<M: Module, C: Core> Driver<M, C> {
    async fn tell(&self, key: String, on: NaiveDate, command: Command) -> Result<(), DriverError> {
        let applied = self
            .module
            .propose(Envelope {
                key,
                caller: DRIVER.to_string(),
                on,
                command,
            })
            .await?;
        applied
            .result
            .map_err(|e| DriverError::Answer(format!("the module refused the driver: {}: {}", e.code, e.message)))
    }

    /// Carry the outbox to the core until it is empty or something stops
    /// the pass. What was taken before a stop is acknowledged.
    pub async fn deliver(&self) -> Result<Pass, DriverError> {
        let mut pass = Pass::default();
        loop {
            let answer = self.module.ask(Query::Outbox { limit: self.batch.max(1) }).await?;
            let outbox: Vec<(u64, Instruction)> = serde_json::from_value(answer).map_err(|e| DriverError::Answer(e.to_string()))?;
            if outbox.is_empty() {
                return Ok(pass);
            }
            let mut done: Option<(u64, NaiveDate)> = None;
            let mut stopped: Option<DriverError> = None;
            for (seq, instruction) in &outbox {
                match self.one(*seq, instruction, &mut pass).await {
                    Ok(Some(on)) => done = Some((*seq, on)),
                    Ok(None) => break,
                    Err(e) => {
                        stopped = Some(e);
                        break;
                    }
                }
            }
            if let Some((through, on)) = done {
                self.tell(format!("ack:{through}"), on, Command::AcknowledgeInstructions { through }).await?;
            }
            if let Some(e) = stopped {
                return Err(e);
            }
            if !pass.clear() {
                return Ok(pass);
            }
        }
    }

    /// Deliver one instruction. `Some(date)` when the outbox can move past
    /// it; `None` when delivery stops here, with the reason on the pass.
    async fn one(&self, seq: u64, instruction: &Instruction, pass: &mut Pass) -> Result<Option<NaiveDate>, DriverError> {
        let Some(on) = self.core.business_date(instruction.partition).await? else {
            pass.waiting_on = Some(instruction.partition);
            return Ok(None);
        };
        let answer = self.module.ask(Query::Delivery { seq, on }).await?;
        let (partition, key, caller, command): (PartitionId, String, String, ic_core::Command) =
            serde_json::from_value(answer).map_err(|e| DriverError::Answer(e.to_string()))?;
        debug_assert_eq!(caller, crate::delivery::CALLER);
        let delivery = CoreDelivery {
            partition,
            key,
            caller: crate::delivery::CALLER,
            command,
        };
        match self.core.send(&delivery).await? {
            Ok(()) => pass.delivered += 1,
            Err(refusal) if superseded(instruction, &refusal) => pass.dropped += 1,
            Err(refusal) => {
                // Stable key: the same refusal reported on every pass is one command.
                self.tell(
                    format!("refused:{seq}:{}", refusal.code),
                    on,
                    Command::InstructionRefused {
                        seq,
                        code: refusal.code.clone(),
                        message: refusal.message.clone(),
                    },
                )
                .await?;
                pass.held = Some((seq, refusal));
                return Ok(None);
            }
        }
        Ok(Some(on))
    }

    /// Hand on the notices for every audience `audiences` reaches. A notice
    /// that is refused is set aside in the module, with an exception, and
    /// the ones behind it carry on.
    pub async fn hand_on<A: Audiences>(&self, audiences: &A, on: NaiveDate) -> Result<Round, DriverError> {
        let mut round = Round::default();
        for audience in audiences.reached() {
            loop {
                let query = Query::Notices {
                    audience: Some(audience),
                    limit: self.batch.max(1),
                };
                let waiting: Vec<(u64, Notice)> = serde_json::from_value(self.module.ask(query).await?).map_err(|e| DriverError::Answer(e.to_string()))?;
                if waiting.is_empty() {
                    break;
                }
                let mut taken = Vec::new();
                let mut stopped = None;
                for (seq, notice) in &waiting {
                    match audiences.hand_on(*seq, notice, on).await {
                        Ok(Handed::Taken) => taken.push(*seq),
                        Ok(Handed::Refused(refusal)) => {
                            let told = Command::NoticeRefused {
                                seq: *seq,
                                code: refusal.code.clone(),
                                message: refusal.message.clone(),
                            };
                            if let Err(e) = self.tell(format!("notice-refused:{seq}:{}", refusal.code), on, told).await {
                                stopped = Some(e);
                                break;
                            }
                            round.refused += 1;
                        }
                        Err(e) => {
                            stopped = Some(e);
                            break;
                        }
                    }
                }
                if let Some(last) = taken.last() {
                    round.taken += taken.len() as u32;
                    self.tell(format!("notices:{last}"), on, Command::AcknowledgeNotices { seqs: taken }).await?;
                }
                if let Some(e) = stopped {
                    return Err(e);
                }
            }
        }
        Ok(round)
    }

    /// One cycle: carry instructions to the core, then hand notices on,
    /// dated by the first partition's business date. Notices wait while
    /// that partition has no date.
    pub async fn cycle<A: Audiences>(&self, audiences: &A) -> (Result<Pass, DriverError>, Result<Round, DriverError>) {
        let pass = self.deliver().await;
        let round = match self.core.business_date(0).await {
            Ok(Some(on)) => self.hand_on(audiences, on).await,
            Ok(None) => Ok(Round::default()),
            Err(e) => Err(e),
        };
        (pass, round)
    }

    /// Run cycles until `stop` resolves. `seen` is shown each cycle's results.
    pub async fn run<A: Audiences>(
        &self,
        audiences: &A,
        every: Duration,
        stop: impl Future<Output = ()>,
        mut seen: impl FnMut(&Result<Pass, DriverError>, &Result<Round, DriverError>),
    ) {
        tokio::pin!(stop);
        loop {
            let (pass, round) = self.cycle(audiences).await;
            match &pass {
                Ok(p) if p.clear() => tracing::debug!(delivered = p.delivered, dropped = p.dropped, "reinsurance delivery pass"),
                Ok(p) => tracing::warn!(held = ?p.held, waiting_on = ?p.waiting_on, "reinsurance delivery is stopped"),
                Err(e) => tracing::warn!(error = %e, "a reinsurance delivery pass could not finish"),
            }
            match &round {
                Ok(r) if r.refused == 0 => tracing::debug!(taken = r.taken, "reinsurance notices handed on"),
                Ok(r) => tracing::warn!(taken = r.taken, refused = r.refused, "reinsurance notices were refused"),
                Err(e) => tracing::warn!(error = %e, "reinsurance notices could not all be handed on"),
            }
            seen(&pass, &round);
            tokio::select! {
                () = &mut stop => return,
                () = tokio::time::sleep(every) => {}
            }
        }
    }
}
