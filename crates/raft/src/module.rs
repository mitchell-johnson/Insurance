//! A replicated group for a module whose state spans partitions.
//!
//! A partition's group holds what belongs to the policies on that partition.
//! Some state belongs to no one partition: a life's reinsurance accumulation,
//! an adviser's commission account, the finance ledger. A module that owns
//! such state gets a Raft group of its own on the same ledger nodes, with the
//! same durable log, snapshots, membership changes and failover as a
//! partition. This file is that group.
//!
//! The host knows nothing about any module. A module hands it a
//! [`ModuleState`]: a deterministic state machine that takes its commands and
//! questions as JSON and can save and load itself. [`hosted`] wraps any
//! [`ic_log::StateMachine`] that serialises, which is what the modules are.
//!
//! A module that reads the core's events says so, and the host feeds it:
//! the module group's leader fetches each partition's events after the
//! group's own checkpoint and proposes them through the group's log, so every
//! member sees each event once, in sequence, and the checkpoint is part of
//! the replicated state. Each partition keeps its events until every module
//! that reads them has them.
//!
//! Delivery is at least once, as it is for a partition: a caller that does
//! not hear back offers the command again. A module's commands therefore
//! carry their own idempotency key and the module answers a repeat with the
//! original result.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::{Stream, TryStreamExt};
use ic_core::EventRecord;
use ic_ledger::PartitionId;
use ic_log::StateMachine;
use openraft::alias::{LogIdOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf};
use openraft::async_runtime::WatchReceiver;
use openraft::raft::ReadPolicy;
use openraft::storage::{EntryResponder, RaftSnapshotBuilder, RaftStateMachine};
use openraft::{BasicNode, Config, EntryPayload, OptionalSend};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::group;
use crate::network::{GroupOf, Network};
use crate::node::ProposeError;
use crate::peers::Identity;
use crate::store::{write_atomic, LogStore};
use crate::types::{NodeId, Proposal, Reply, TypeConfig};

const SNAPSHOT_FILE: &str = "snapshot.json";

/// A module's state as the host drives it.
///
/// Everything here must be a pure function of the commands applied so far:
/// no clock, no randomness, no network, and no iteration over a `HashMap`
/// where the order can reach the saved state. Every member applies the same
/// commands in the same order and must arrive at the same saved state, byte
/// for byte; the members' state hashes are compared to prove it.
pub trait ModuleState: Send + 'static {
    /// Apply the command at log index `index`. `Err` is a refusal: the
    /// command was understood to be wrong and the state is unchanged.
    fn apply(&mut self, index: u64, command: Value) -> Result<Value, String>;
    /// Take in one event of a partition. Called once for each event, in
    /// the partition's sequence, on every member. A module that reads no
    /// events leaves this as it is.
    fn on_event(&mut self, _index: u64, _event: &EventRecord) {}
    /// Answer a question from the state as it stands. Never changes it.
    fn query(&self, question: Value) -> Result<Value, String>;
    /// The whole state, for a snapshot and for the state hash.
    fn save(&self) -> Result<Value, String>;
    /// Replace the state with a saved one. Called on a newly made state.
    fn load(&mut self, saved: Value) -> Result<(), String>;
}

struct Hosted<S: StateMachine> {
    state: S,
    query: fn(&S, Value) -> Result<Value, String>,
    /// The commands an event becomes, if the module reads events.
    react: Option<fn(&EventRecord) -> Vec<S::Command>>,
}

impl<S> ModuleState for Hosted<S>
where
    S: StateMachine + Serialize + DeserializeOwned + Send + 'static,
    S::Output: Serialize,
{
    fn apply(&mut self, index: u64, command: Value) -> Result<Value, String> {
        let command: S::Command = serde_json::from_value(command).map_err(|e| format!("not a command of this module: {e}"))?;
        serde_json::to_value(self.state.apply(index, command)).map_err(|e| e.to_string())
    }

    fn on_event(&mut self, index: u64, event: &EventRecord) {
        for command in self.react.map(|react| react(event)).unwrap_or_default() {
            self.state.apply(index, command);
        }
    }

    fn query(&self, question: Value) -> Result<Value, String> {
        (self.query)(&self.state, question)
    }

    fn save(&self) -> Result<Value, String> {
        serde_json::to_value(&self.state).map_err(|e| e.to_string())
    }

    fn load(&mut self, saved: Value) -> Result<(), String> {
        self.state = serde_json::from_value(saved).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Host a state machine written against [`ic_log::StateMachine`]. Its command
/// type is what callers send, its output is what they get back, and `query`
/// answers questions from it.
pub fn hosted<S>(state: S, query: fn(&S, Value) -> Result<Value, String>) -> Box<dyn ModuleState>
where
    S: StateMachine + Serialize + DeserializeOwned + Send + 'static,
    S::Output: Serialize,
{
    Box::new(Hosted { state, query, react: None })
}

/// As [`hosted`], for a module that reads the core's events. `react` turns
/// an event into the module's own commands, which are applied in order. It
/// must be a pure function of the event. Declare the module with
/// [`ModuleSpec::reading_events`] so the host feeds it.
pub fn hosted_reading<S>(state: S, query: fn(&S, Value) -> Result<Value, String>, react: fn(&EventRecord) -> Vec<S::Command>) -> Box<dyn ModuleState>
where
    S: StateMachine + Serialize + DeserializeOwned + Send + 'static,
    S::Output: Serialize,
    S::Command: 'static,
{
    Box::new(Hosted {
        state,
        query,
        react: Some(react),
    })
}

/// Where one of a module's core commands is to be proposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// The module knows the partition.
    Partition(PartitionId),
    /// The partition that holds this policy. The host looks it up.
    Policy(ic_core::PolicyId),
}

/// One command a module wants the core to apply, from its outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorePosting {
    /// The module's own name for this outbox item.
    pub id: String,
    pub route: Route,
    /// The command as the core takes it. Its idempotency key is the
    /// module's, so the same item offered twice is applied once.
    pub envelope: ic_core::Envelope,
}

/// What became of a [`CorePosting`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreOutcome {
    /// The core applied it, now or on an earlier offer.
    Applied { partition: PartitionId, log_index: u64, duplicate: bool },
    /// The core understood it and refused it. Offering it again changes
    /// nothing.
    Refused(ic_core::Reject),
    /// No partition holds the policy it was addressed to.
    Unroutable(String),
}

/// How the host carries a module's commands to the core.
///
/// A module that posts to the core keeps an outbox in its replicated state,
/// written in the same step as whatever caused the posting. The host, on the
/// node that leads the module's group, asks for the oldest items, proposes
/// each to its partition and tells the module what became of it. The module
/// removes an item only when it hears the outcome, so a node lost at any
/// point leaves the item to be offered again, and the core's idempotency key
/// makes the second offer harmless.
///
/// The three functions adapt the host to the module's own questions and
/// commands. All are pure.
#[derive(Clone, Copy)]
pub struct Outbox {
    /// The question that lists up to `limit` of the oldest unsent items.
    pub pending: fn(limit: usize) -> Value,
    /// Read the module's answer to that question, oldest first.
    pub read: fn(Value) -> Result<Vec<CorePosting>, String>,
    /// The module command that records an item's outcome. It carries its
    /// own idempotency key, as every module command does.
    pub outcome: fn(&CorePosting, &CoreOutcome) -> Value,
}

/// A module a node hosts: its name and how to make its empty state.
#[derive(Clone)]
pub struct ModuleSpec {
    /// Lower-case letters, digits and hyphens. It names the module's
    /// directory on disk and its routes, so it never changes.
    pub name: String,
    pub make: fn() -> Box<dyn ModuleState>,
    /// The module reads every partition's events: the host feeds them to
    /// its group and each partition keeps them until the group has them.
    pub events: bool,
    /// The module posts to the core: the host carries its outbox there.
    pub outbox: Option<Outbox>,
}

impl ModuleSpec {
    pub fn new(name: &str, make: fn() -> Box<dyn ModuleState>) -> Result<Self, String> {
        let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid {
            return Err(format!("a module's name is lower-case letters, digits and hyphens, not {name:?}"));
        }
        Ok(Self {
            name: name.to_string(),
            make,
            events: false,
            outbox: None,
        })
    }

    /// Carry this module's outbox to the core. See [`Outbox`].
    pub fn posting_to_core(mut self, outbox: Outbox) -> Self {
        self.outbox = Some(outbox);
        self
    }

    /// Feed this module every partition's events.
    pub fn reading_events(mut self) -> Self {
        self.events = true;
        self
    }
}

/// What a module made of a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleReply {
    /// The log index the command was applied at.
    pub log_index: u64,
    /// The module's answer, or why it refused the command.
    pub result: Result<Value, String>,
}

/// A member's view of a module's group, for health checks and tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleStatus {
    pub module: String,
    pub id: NodeId,
    /// `leader`, `follower`, `candidate` or `learner`.
    pub role: String,
    pub leader: Option<NodeId>,
    pub term: u64,
    pub last_log_index: Option<u64>,
    pub last_applied: Option<u64>,
    /// Hash of the module's saved state. Members that have applied the same
    /// entries report the same hash.
    pub state_hash: String,
    pub voters: Vec<NodeId>,
    /// The first log entry still held; earlier ones are behind a snapshot.
    pub first_log_index: Option<u64>,
    /// The last event taken in from each partition.
    #[serde(default)]
    pub cursors: BTreeMap<PartitionId, u64>,
}

/// A snapshot of a module's group, as written to disk and sent to a member
/// that has fallen behind the log.
#[derive(Serialize, Deserialize)]
struct Saved {
    module: String,
    last_applied: Option<LogIdOf<TypeConfig>>,
    membership: StoredMembershipOf<TypeConfig>,
    state: Value,
    /// The last event taken in from each partition.
    #[serde(default)]
    cursors: BTreeMap<PartitionId, u64>,
}

/// What a snapshot restores.
struct Restored {
    state: Box<dyn ModuleState>,
    last_applied: Option<LogIdOf<TypeConfig>>,
    membership: StoredMembershipOf<TypeConfig>,
    cursors: BTreeMap<PartitionId, u64>,
}

struct Inner {
    spec: ModuleSpec,
    state: Box<dyn ModuleState>,
    last_applied: Option<LogIdOf<TypeConfig>>,
    membership: StoredMembershipOf<TypeConfig>,
    cursors: BTreeMap<PartitionId, u64>,
    dir: PathBuf,
}

/// One member's copy of a module's state. Clones share the same state.
#[derive(Clone)]
pub struct ModuleMachine {
    inner: Arc<Mutex<Inner>>,
}

fn invalid(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn meta_of(saved: &Saved) -> SnapshotMetaOf<TypeConfig> {
    SnapshotMetaOf::<TypeConfig> {
        last_log_id: saved.last_applied,
        last_membership: saved.membership.clone(),
    }
}

fn read_snapshot(dir: &Path) -> io::Result<Option<(Vec<u8>, Saved)>> {
    let path = dir.join(SNAPSHOT_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let saved = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{} is not a module snapshot: {e}", path.display())))?;
            Ok(Some((bytes, saved)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Make the module's state from a snapshot, refusing another module's.
fn restore(spec: &ModuleSpec, saved: Saved) -> io::Result<Restored> {
    if saved.module != spec.name {
        return Err(invalid(format!("the snapshot is of module {}, not {}", saved.module, spec.name)));
    }
    let mut state = (spec.make)();
    state
        .load(saved.state)
        .map_err(|e| invalid(format!("module {} cannot load its snapshot: {e}", spec.name)))?;
    Ok(Restored {
        state,
        last_applied: saved.last_applied,
        membership: saved.membership,
        cursors: saved.cursors,
    })
}

impl Inner {
    /// Take in a run of a partition's events. Events already taken in are
    /// passed over, so a run offered twice counts once. A run that would
    /// leave a gap is refused whole: the feeder asks again from the
    /// checkpoint. The first run from a partition sets where it starts.
    fn feed(&mut self, index: u64, partition: PartitionId, after: u64, events: &[EventRecord]) -> Result<Value, String> {
        let mut cursor = *self.cursors.get(&partition).unwrap_or(&after);
        if after > cursor {
            return Err(format!(
                "events of partition {partition} after {after} would leave a gap: this group has them through {cursor}"
            ));
        }
        let mut expected = after;
        for event in events {
            expected += 1;
            if event.partition != partition || event.seq != expected {
                return Err(format!(
                    "event {} is not number {expected} of partition {partition}: the run is not in sequence",
                    event.id
                ));
            }
        }
        for event in events.iter().filter(|e| e.seq > cursor) {
            self.state.on_event(index, event);
        }
        cursor = cursor.max(expected);
        self.cursors.insert(partition, cursor);
        Ok(serde_json::json!({ "through": cursor }))
    }
}

impl ModuleMachine {
    /// A member that keeps its snapshot in `dir`. If a snapshot is there, the
    /// member starts from it.
    pub fn open(spec: ModuleSpec, dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let restored = match read_snapshot(&dir)? {
            Some((_, saved)) => restore(&spec, saved)?,
            None => Restored {
                state: (spec.make)(),
                last_applied: None,
                membership: StoredMembershipOf::<TypeConfig>::default(),
                cursors: BTreeMap::new(),
            },
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                spec,
                state: restored.state,
                last_applied: restored.last_applied,
                membership: restored.membership,
                cursors: restored.cursors,
                dir,
            })),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Answer a question from the state as this member holds it.
    pub fn query(&self, question: Value) -> Result<Value, String> {
        self.lock().state.query(question)
    }

    /// Raft log index of the last entry applied.
    pub fn last_applied_index(&self) -> Option<u64> {
        self.lock().last_applied.as_ref().map(|l| l.index())
    }

    /// The last event taken in from each partition.
    pub fn cursors(&self) -> BTreeMap<PartitionId, u64> {
        self.lock().cursors.clone()
    }

    /// Hash of the saved state and the event checkpoints, in hex.
    pub fn state_hash(&self) -> String {
        let saved = {
            let inner = self.lock();
            inner.state.save().map(|state| (state, inner.cursors.clone()))
        };
        // A state that cannot be saved has no hash to agree on; say so in
        // the place the hashes are compared.
        let bytes = match saved.and_then(|v| serde_json::to_vec(&v).map_err(|e| e.to_string())) {
            Ok(bytes) => bytes,
            Err(e) => return format!("unsaveable: {e}"),
        };
        Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn saved(&self) -> io::Result<(Saved, PathBuf)> {
        let inner = self.lock();
        let saved = Saved {
            module: inner.spec.name.clone(),
            last_applied: inner.last_applied,
            membership: inner.membership.clone(),
            state: inner.state.save().map_err(invalid)?,
            cursors: inner.cursors.clone(),
        };
        Ok((saved, inner.dir.clone()))
    }
}

impl RaftSnapshotBuilder<TypeConfig> for ModuleMachine {
    type SnapshotData = Vec<u8>;

    async fn build_snapshot(&mut self) -> Result<SnapshotOf<TypeConfig, Vec<u8>>, io::Error> {
        // Copy under the lock, encode and write outside it.
        let (saved, dir) = self.saved()?;
        let bytes = serde_json::to_vec(&saved).map_err(invalid)?;
        // Durable before Raft is told: the log behind a snapshot is dropped
        // only once the snapshot can be read back.
        write_atomic(&dir.join(SNAPSHOT_FILE), &bytes)?;
        tracing::info!(module = %saved.module, through = ?saved.last_applied.map(|l| l.index()), bytes = bytes.len(), "module snapshot written");
        Ok(SnapshotOf::<TypeConfig, Vec<u8>> {
            meta: meta_of(&saved),
            snapshot: bytes,
        })
    }
}

impl RaftStateMachine<TypeConfig> for ModuleMachine {
    type SnapshotData = Vec<u8>;
    type SnapshotBuilder = Self;

    async fn applied_state(&mut self) -> Result<(Option<LogIdOf<TypeConfig>>, StoredMembershipOf<TypeConfig>), io::Error> {
        let inner = self.lock();
        Ok((inner.last_applied, inner.membership.clone()))
    }

    async fn apply<Strm>(&mut self, mut entries: Strm) -> Result<(), io::Error>
    where
        Strm: Stream<Item = Result<EntryResponder<TypeConfig>, io::Error>> + Unpin + OptionalSend,
    {
        while let Some((entry, responder)) = entries.try_next().await? {
            let reply = {
                let mut inner = self.lock();
                inner.last_applied = Some(entry.log_id);
                let log_index = entry.log_id.index();
                match entry.payload {
                    EntryPayload::Blank => Reply::default(),
                    EntryPayload::Membership(m) => {
                        inner.membership = StoredMembershipOf::<TypeConfig>::new(Some(entry.log_id), m);
                        Reply::default()
                    }
                    EntryPayload::Normal(Proposal::Module(command)) => Reply {
                        log_index,
                        module: Some(inner.state.apply(log_index, command)),
                        ..Reply::default()
                    },
                    EntryPayload::Normal(Proposal::Feed { partition, after, events }) => Reply {
                        log_index,
                        module: Some(inner.feed(log_index, partition, after, &events)),
                        ..Reply::default()
                    },
                    // A partition's entry has no meaning to a module. The
                    // node never proposes one here; a log that holds one is
                    // applied the same way by every member: as nothing.
                    EntryPayload::Normal(_) => Reply {
                        log_index,
                        module: Some(Err("this group is a module, not a ledger partition".to_string())),
                        ..Reply::default()
                    },
                }
            };
            if let Some(responder) = responder {
                responder.send(reply);
            }
        }
        Ok(())
    }

    async fn try_create_snapshot_builder(&mut self, _force: bool) -> Option<Self::SnapshotBuilder> {
        Some(self.clone())
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn install_snapshot(&mut self, meta: &SnapshotMetaOf<TypeConfig>, snapshot: Vec<u8>) -> Result<(), io::Error> {
        let saved: Saved = serde_json::from_slice(&snapshot).map_err(invalid)?;
        let mut inner = self.lock();
        // Build the new state apart, write it to disk, then switch: a
        // snapshot that cannot be loaded leaves this member as it was.
        let restored = restore(&inner.spec, saved)?;
        write_atomic(&inner.dir.join(SNAPSHOT_FILE), &snapshot)?;
        inner.state = restored.state;
        inner.last_applied = restored.last_applied;
        inner.membership = restored.membership;
        inner.cursors = restored.cursors;
        tracing::info!(module = %inner.spec.name, through = ?meta.last_log_id.map(|l| l.index()), bytes = snapshot.len(), "module snapshot installed from the leader");
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<SnapshotOf<TypeConfig, Vec<u8>>>, io::Error> {
        let dir = self.lock().dir.clone();
        Ok(read_snapshot(&dir)?.map(|(bytes, saved)| SnapshotOf::<TypeConfig, Vec<u8>> {
            meta: meta_of(&saved),
            snapshot: bytes,
        }))
    }
}

/// One member of a module's Raft group.
pub struct ModuleGroup {
    pub name: String,
    pub id: NodeId,
    pub raft: GroupOf<ModuleMachine>,
    pub machine: ModuleMachine,
    pub log: LogStore,
    pub identity: Identity,
}

impl ModuleGroup {
    /// The name of a module's group in Raft's routes and on disk.
    pub fn group_name(module: &str) -> String {
        format!("module-{module}")
    }

    /// Start a member over its log. An existing log is replayed: the member
    /// comes back with the state it had and rejoins its group.
    pub async fn start(identity: Identity, name: &str, log: LogStore, machine: ModuleMachine, network: Network, config: Arc<Config>) -> Result<Self, String> {
        let id = identity.id;
        let raft = GroupOf::<ModuleMachine>::new(id, config, network, log.clone(), machine.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self {
            name: name.to_string(),
            id,
            raft,
            machine,
            log,
            identity,
        })
    }

    /// Form a new group. Calling it on a group that already exists changes nothing.
    pub async fn initialize(&self, members: BTreeMap<NodeId, BasicNode>) -> Result<(), String> {
        if self.raft.is_initialized().await.map_err(|e| e.to_string())? {
            return Ok(());
        }
        self.raft.initialize(members).await.map_err(|e| e.to_string())
    }

    /// Replicate a command and apply it. Returns once a majority has it
    /// durably and this member has applied it.
    pub async fn propose(&self, command: Value) -> Result<ModuleReply, ProposeError> {
        let reply = group::write(&self.raft, Proposal::Module(command)).await?;
        Ok(ModuleReply {
            log_index: reply.log_index,
            result: reply.module.unwrap_or_else(|| Err("the group did not apply a module command".to_string())),
        })
    }

    /// Replicate a run of a partition's events and take them in. `after`
    /// is the sequence number the run follows. Returns how far the group
    /// then has that partition's events.
    pub async fn feed(&self, partition: PartitionId, after: u64, events: Vec<EventRecord>) -> Result<Result<u64, String>, ProposeError> {
        let reply = group::write(&self.raft, Proposal::Feed { partition, after, events }).await?;
        let taken = reply.module.unwrap_or_else(|| Err("the group did not take in the events".to_string()));
        Ok(taken.map(|v| v["through"].as_u64().unwrap_or(after)))
    }

    /// Confirm with a majority that this member still leads, then answer.
    /// A deposed leader therefore never answers from a stale state.
    pub async fn query(&self, question: Value) -> Result<Result<Value, String>, ProposeError> {
        match self.raft.ensure_linearizable(ReadPolicy::ReadIndex).await {
            Ok(_) => Ok(self.machine.query(question)),
            Err(_) => {
                let (leader, addr) = self.leader_hint();
                Err(ProposeError::NotLeader { leader, addr })
            }
        }
    }

    /// Change who the group's voters are. See [`crate::Node::set_members`].
    pub async fn set_members(&self, members: BTreeMap<NodeId, String>) -> Result<(), ProposeError> {
        group::set_members(&self.raft, members).await
    }

    pub fn leader_hint(&self) -> (Option<NodeId>, Option<String>) {
        group::leader_hint(&self.raft)
    }

    pub fn status(&self) -> ModuleStatus {
        let metrics = self.raft.metrics().borrow_watched().clone();
        ModuleStatus {
            module: self.name.clone(),
            id: self.id,
            role: group::role(metrics.state).to_string(),
            leader: metrics.current_leader,
            term: metrics.current_term,
            last_log_index: metrics.last_log_index,
            last_applied: self.machine.last_applied_index(),
            state_hash: self.machine.state_hash(),
            voters: self.raft.voter_ids().collect(),
            first_log_index: self.log.first_index(),
            cursors: self.machine.cursors(),
        }
    }

    /// Stop the member. Its log stays where it is.
    pub async fn shutdown(&self) {
        let _ = self.raft.shutdown().await;
    }
}
