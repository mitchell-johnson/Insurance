//! The core's state machine as Raft sees it.
//!
//! Every member of a group applies the same committed entries in the same
//! order to the same deterministic [`CoreState`], so every member holds the
//! same state. Nothing here reads a clock or generates randomness.
//!
//! Applying a command produces three kinds of output that must leave the
//! node: journal entries (for the cold tier), events (for the stream) and
//! messages (for other partitions). They are part of the replicated state
//! until the leader's host records, through the log, that they have left.
//! Losing any member, the leader included, therefore loses none of them.
//!
//! The state is held in memory. A snapshot of it is written to disk when
//! the log has grown enough, after which the log before it is dropped; a
//! restarted member loads the snapshot and replays the log that follows.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::{Stream, TryStreamExt};
use ic_core::event::OutboundMessage;
use ic_core::{CoreState, EventRecord};
use ic_ledger::{JournalEntry, PartitionId};
use ic_log::StateMachine as _;
use ic_readmodel::Projections;
use openraft::alias::{LogIdOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf};
use openraft::storage::{EntryResponder, RaftSnapshotBuilder, RaftStateMachine};
use openraft::{EntryPayload, OptionalSend};
use serde::{Deserialize, Serialize};

use crate::store::write_atomic;
use crate::types::{Housekeeping, Proposal, Reply, TypeConfig};

const SNAPSHOT_FILE: &str = "snapshot.json";

/// Everything a member holds, all of it a pure function of the log.
#[derive(Clone, Serialize, Deserialize)]
struct Replicated {
    core: CoreState,
    last_applied: Option<LogIdOf<TypeConfig>>,
    membership: StoredMembershipOf<TypeConfig>,
    /// Messages for other partitions, not yet confirmed delivered.
    messages: Vec<OutboundMessage>,
    /// Journal entries not yet sealed in the cold tier, oldest first.
    journal: VecDeque<JournalEntry>,
    /// Events not yet on the stream, oldest first.
    events: VecDeque<EventRecord>,
    /// The partition's read models, fed from every event as it is produced.
    view: Projections,
}

struct Inner {
    state: Replicated,
    /// Where snapshots are kept. `None` keeps none: the log is never dropped.
    dir: PathBuf,
}

/// One member's copy of a partition's state. Clones share the same state.
#[derive(Clone)]
pub struct Machine {
    inner: Arc<Mutex<Inner>>,
}

/// How much output a member is holding for the leader's host to move on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Backlog {
    pub messages: u64,
    pub journal: u64,
    pub events: u64,
}

fn invalid(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn read_snapshot(dir: &Path) -> io::Result<Option<(Vec<u8>, Replicated)>> {
    let path = dir.join(SNAPSHOT_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let state = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{} is not a snapshot: {e}", path.display())))?;
            Ok(Some((bytes, state)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn meta_of(state: &Replicated) -> SnapshotMetaOf<TypeConfig> {
    SnapshotMetaOf::<TypeConfig> {
        last_log_id: state.last_applied,
        last_membership: state.membership.clone(),
    }
}

impl Machine {
    /// A member that keeps its snapshot in `dir`. If a snapshot is there, the
    /// member starts from it.
    pub fn open(partition: PartitionId, dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let restored = read_snapshot(&dir)?.map(|(_, state)| state);
        if let Some(state) = &restored {
            if state.core.partition != partition {
                return Err(invalid(format!(
                    "the snapshot in {} is of partition {}, not {partition}",
                    dir.display(),
                    state.core.partition
                )));
            }
        }
        Ok(Self::with(partition, dir, restored))
    }

    fn with(partition: PartitionId, dir: PathBuf, restored: Option<Replicated>) -> Self {
        let state = restored.unwrap_or_else(|| Replicated {
            core: CoreState::new(partition),
            last_applied: None,
            membership: StoredMembershipOf::<TypeConfig>::default(),
            messages: vec![],
            journal: VecDeque::new(),
            events: VecDeque::new(),
            view: Projections::new(),
        });
        Self {
            inner: Arc::new(Mutex::new(Inner { state, dir })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Read the partition's state. The closure must not block.
    pub fn read<T>(&self, f: impl FnOnce(&CoreState) -> T) -> T {
        f(&self.lock().state.core)
    }

    /// Answer a question from the state and read models as they stand.
    pub fn query(&self, query: crate::query::Query) -> crate::query::Answer {
        let inner = self.lock();
        crate::query::answer(&inner.state.core, &inner.state.view, query)
    }

    /// Raft log index of the last entry applied.
    pub fn last_applied_index(&self) -> Option<u64> {
        self.lock().state.last_applied.as_ref().map(|l| l.index())
    }

    pub fn backlog(&self) -> Backlog {
        let inner = self.lock();
        Backlog {
            messages: inner.state.messages.len() as u64,
            journal: inner.state.journal.len() as u64,
            events: inner.state.events.len() as u64,
        }
    }

    /// Messages waiting for delivery to other partitions.
    pub fn pending_messages(&self) -> Vec<OutboundMessage> {
        self.lock().state.messages.clone()
    }

    /// The oldest events not yet on the stream, at most `limit`.
    pub fn pending_events(&self, limit: usize) -> Vec<EventRecord> {
        self.lock().state.events.iter().take(limit).cloned().collect()
    }

    /// The next full cold-tier segment, if enough entries are waiting.
    /// Segments have a fixed size and fixed boundaries, so every leader that
    /// ever seals a given stretch of the journal produces the same segment.
    pub fn next_segment(&self, size: u64) -> Option<Vec<JournalEntry>> {
        let inner = self.lock();
        let journal = &inner.state.journal;
        let first = journal.front()?.id.seq;
        // Sequence numbers start at one: segments are [1..=size], [size+1..=2*size], ...
        let aligned = (first - 1).is_multiple_of(size);
        (aligned && journal.len() as u64 >= size).then(|| journal.iter().take(size as usize).cloned().collect())
    }

    fn housekeep(state: &mut Replicated, done: Housekeeping) {
        match done {
            Housekeeping::Delivered { ids } => state.messages.retain(|m| !ids.contains(&m.id)),
            Housekeeping::Sealed { through_seq } => {
                while state.journal.front().is_some_and(|e| e.id.seq <= through_seq) {
                    state.journal.pop_front();
                }
            }
            Housekeeping::Published { through_seq } => {
                while state.events.front().is_some_and(|e| e.seq <= through_seq) {
                    state.events.pop_front();
                }
            }
        }
    }

    /// Replace this member's state with a snapshot's, on disk first.
    fn adopt(&self, bytes: &[u8]) -> io::Result<()> {
        let state: Replicated = serde_json::from_slice(bytes).map_err(invalid)?;
        let mut inner = self.lock();
        write_atomic(&inner.dir.join(SNAPSHOT_FILE), bytes)?;
        inner.state = state;
        Ok(())
    }
}

impl RaftSnapshotBuilder<TypeConfig> for Machine {
    type SnapshotData = Vec<u8>;

    async fn build_snapshot(&mut self) -> Result<SnapshotOf<TypeConfig, Vec<u8>>, io::Error> {
        // Copy under the lock, encode and write outside it: applying entries
        // is not held up by a large state going to disk.
        let (state, dir) = {
            let inner = self.lock();
            (inner.state.clone(), inner.dir.clone())
        };
        let bytes = serde_json::to_vec(&state).map_err(invalid)?;
        // Durable before Raft is told: the log behind a snapshot is dropped
        // only once the snapshot can be read back.
        write_atomic(&dir.join(SNAPSHOT_FILE), &bytes)?;
        tracing::info!(partition = state.core.partition, through = ?state.last_applied.map(|l| l.index()), bytes = bytes.len(), "snapshot written");
        Ok(SnapshotOf::<TypeConfig, Vec<u8>> {
            meta: meta_of(&state),
            snapshot: bytes,
        })
    }
}

impl RaftStateMachine<TypeConfig> for Machine {
    type SnapshotData = Vec<u8>;
    type SnapshotBuilder = Self;

    async fn applied_state(&mut self) -> Result<(Option<LogIdOf<TypeConfig>>, StoredMembershipOf<TypeConfig>), io::Error> {
        let inner = self.lock();
        Ok((inner.state.last_applied, inner.state.membership.clone()))
    }

    async fn apply<Strm>(&mut self, mut entries: Strm) -> Result<(), io::Error>
    where
        Strm: Stream<Item = Result<EntryResponder<TypeConfig>, io::Error>> + Unpin + OptionalSend,
    {
        while let Some((entry, responder)) = entries.try_next().await? {
            let reply = {
                let mut inner = self.lock();
                let state = &mut inner.state;
                state.last_applied = Some(entry.log_id);
                match entry.payload {
                    EntryPayload::Blank => Reply::default(),
                    EntryPayload::Membership(m) => {
                        state.membership = StoredMembershipOf::<TypeConfig>::new(Some(entry.log_id), m);
                        Reply::default()
                    }
                    EntryPayload::Normal(Proposal::Command(envelope)) => {
                        let applied = state.core.apply(entry.log_id.index(), envelope);
                        state.messages.extend(applied.messages.iter().cloned());
                        state.journal.extend(applied.entries.iter().cloned());
                        for event in &applied.events {
                            // Events arrive in sequence from the state they
                            // describe, so a gap here is a defect in the core.
                            if let Err(error) = state.view.apply(event) {
                                tracing::error!(%error, "a read model missed an event");
                            }
                        }
                        state.events.extend(applied.events.iter().cloned());
                        Reply::from_applied(applied)
                    }
                    EntryPayload::Normal(Proposal::Housekeeping(done)) => {
                        Self::housekeep(state, done);
                        Reply {
                            log_index: entry.log_id.index(),
                            ..Reply::default()
                        }
                    }
                    // A module's command has no meaning to a partition. The
                    // node never proposes one here; a log that holds one is
                    // applied the same way by every member: as nothing.
                    EntryPayload::Normal(Proposal::Module(_)) => Reply {
                        log_index: entry.log_id.index(),
                        module: Some(Err("this group is a ledger partition, not a module".to_string())),
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
        self.adopt(&snapshot)?;
        tracing::info!(through = ?meta.last_log_id.map(|l| l.index()), bytes = snapshot.len(), "snapshot installed from the leader");
        Ok(())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<SnapshotOf<TypeConfig, Vec<u8>>>, io::Error> {
        let dir = self.lock().dir.clone();
        Ok(read_snapshot(&dir)?.map(|(bytes, state)| SnapshotOf::<TypeConfig, Vec<u8>> {
            meta: meta_of(&state),
            snapshot: bytes,
        }))
    }
}
