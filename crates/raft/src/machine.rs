//! The core's state machine as Raft sees it.
//!
//! Every member of a group applies the same committed entries in the same
//! order to the same deterministic [`CoreState`], so every member holds the
//! same state. Nothing here reads a clock or generates randomness.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use futures::{Stream, TryStreamExt};
use ic_core::event::OutboundMessage;
use ic_core::CoreState;
use ic_ledger::PartitionId;
use ic_log::StateMachine as _;
use openraft::alias::{LogIdOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf};
use openraft::storage::{EntryResponder, RaftSnapshotBuilder, RaftStateMachine};
use openraft::{EntryPayload, OptionalSend};

use crate::types::{Reply, TypeConfig};

struct Inner {
    core: CoreState,
    last_applied: Option<LogIdOf<TypeConfig>>,
    membership: StoredMembershipOf<TypeConfig>,
    /// Cross-partition messages produced by applied commands, waiting for
    /// the host's relay. Every member builds the same outbox; only the
    /// leader's host drains it, and receivers de-duplicate on the message ID.
    /// A member that takes over redelivers what it holds, which is safe.
    messages: Vec<OutboundMessage>,
}

/// One member's copy of a partition's state. Clones share the same state.
#[derive(Clone)]
pub struct Machine {
    inner: Arc<Mutex<Inner>>,
}

impl Machine {
    pub fn new(partition: PartitionId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                core: CoreState::new(partition),
                last_applied: None,
                membership: StoredMembershipOf::<TypeConfig>::default(),
                messages: vec![],
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Read the partition's state. The closure must not block.
    pub fn read<T>(&self, f: impl FnOnce(&CoreState) -> T) -> T {
        f(&self.lock().core)
    }

    /// Raft log index of the last entry applied.
    pub fn last_applied_index(&self) -> Option<u64> {
        self.lock().last_applied.as_ref().map(|l| l.index())
    }

    /// Take the cross-partition messages waiting for the relay.
    pub fn take_messages(&self) -> Vec<OutboundMessage> {
        std::mem::take(&mut self.lock().messages)
    }

    /// Put back messages the relay could not deliver, ahead of newer ones.
    pub fn return_messages(&self, mut undelivered: Vec<OutboundMessage>) {
        let mut inner = self.lock();
        undelivered.append(&mut inner.messages);
        inner.messages = undelivered;
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "snapshots are not used in this phase: a member rebuilds from the log",
    )
}

impl RaftSnapshotBuilder<TypeConfig> for Machine {
    type SnapshotData = ();

    async fn build_snapshot(&mut self) -> Result<SnapshotOf<TypeConfig, ()>, io::Error> {
        Err(unsupported())
    }
}

impl RaftStateMachine<TypeConfig> for Machine {
    type SnapshotData = ();
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
                match entry.payload {
                    EntryPayload::Blank => Reply::default(),
                    EntryPayload::Membership(m) => {
                        inner.membership = StoredMembershipOf::<TypeConfig>::new(Some(entry.log_id), m);
                        Reply::default()
                    }
                    EntryPayload::Normal(proposal) => {
                        let applied = inner.core.apply(entry.log_id.index(), proposal.0);
                        inner.messages.extend(applied.messages.iter().cloned());
                        Reply::from_applied(applied)
                    }
                }
            };
            if let Some(responder) = responder {
                responder.send(reply);
            }
        }
        Ok(())
    }

    async fn try_create_snapshot_builder(&mut self, _force: bool) -> Option<Self::SnapshotBuilder> {
        None
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn install_snapshot(&mut self, _meta: &SnapshotMetaOf<TypeConfig>, _snapshot: ()) -> Result<(), io::Error> {
        Err(unsupported())
    }

    async fn get_current_snapshot(&mut self) -> Result<Option<SnapshotOf<TypeConfig, ()>>, io::Error> {
        Ok(None)
    }
}
