//! A voter that keeps the log but holds no state: the tiebreaker.
//!
//! The ledger runs five voters per group: two in each of two regions and one
//! in a third. The fifth is here only so that either main region can be lost
//! with a majority left. It votes and acknowledges entries like any voter,
//! but applies nothing: it holds no policies, no balances, no read models and
//! no outputs, and it answers no questions.
//!
//! What it does keep is the log, because Raft needs it to be safe. An entry
//! can be committed on the leader's region and the witness alone (three of
//! five) before the other region has it. If the leader's region is then lost,
//! the witness holds entries the survivors lack. It refuses its vote to a
//! survivor whose log is behind its own, stands itself, and as leader sends
//! them what they are missing; then it hands the lead to one of them
//! (`Host::hand_off_as_witness`). No acknowledged write is lost.
//!
//! The log cannot grow for ever, and the witness cannot compact it the usual
//! way, behind a snapshot of its state, because it has none. Instead every
//! data voter's leader records in the log how far every data voter holds it
//! ([`crate::types::Proposal::Durable`]). Entries up to that point are held
//! by a data voter in each region, so the witness no longer needs them: it
//! takes a marker snapshot there (no state, only the position) and Raft drops
//! the log behind it. A data voter that stays down holds that point back, and
//! the witness's log grows until it returns or is replaced; `ic-admin watch`
//! reports it.
//!
//! The marker is never sent to a data voter as a snapshot: the network
//! refuses to carry it ([`is_marker`]), and a data voter refuses to install
//! it. A data voter that has fallen behind the witness's log while the
//! witness leads is brought up by the next data leader instead.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use openraft::alias::{EntryPayloadOf, LogIdOf, SnapshotMetaOf, SnapshotOf, StoredMembershipOf};
use openraft::EntryPayload;
use serde::{Deserialize, Serialize};

use crate::store::write_atomic;
use crate::types::{Proposal, Reply, TypeConfig};

/// The whole content of a witness's snapshot. It is not the state of
/// anything, and a data voter must never take it for one.
pub const MARKER: &[u8] = b"{\"witness\":true}";

const FILE: &str = "witness.json";

/// Whether snapshot bytes are a witness's marker rather than a state.
pub fn is_marker(bytes: &[u8]) -> bool {
    bytes == MARKER
}

/// The error a data voter returns when offered a witness's marker.
pub fn refuse_marker() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "a witness's marker is not a state; a data voter never installs it")
}

/// What the witness writes to disk: the position of its marker snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    through: Option<LogIdOf<TypeConfig>>,
    membership: StoredMembershipOf<TypeConfig>,
}

#[derive(Debug)]
struct Inner {
    dir: PathBuf,
    last_applied: Option<LogIdOf<TypeConfig>>,
    /// The membership as of `last_applied`.
    membership: StoredMembershipOf<TypeConfig>,
    /// Memberships applied since the marker, oldest first, so the marker's
    /// own membership can be the one in force at its position.
    since: Vec<StoredMembershipOf<TypeConfig>>,
    /// How far every data voter is known to hold the log.
    durable: Option<LogIdOf<TypeConfig>>,
    saved: Saved,
}

/// One group's witness. Clones share the same record.
#[derive(Debug, Clone)]
pub struct Witness {
    inner: Arc<Mutex<Inner>>,
}

fn invalid(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

impl Witness {
    /// A witness that keeps its marker in `dir`, starting from it if it is there.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(FILE);
        let saved: Saved = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{} is not a witness marker: {e}", path.display())))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Saved::default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                dir,
                last_applied: saved.through,
                membership: saved.membership.clone(),
                since: Vec::new(),
                durable: saved.through,
                saved,
            })),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Take in a committed entry. Only memberships and the data voters'
    /// durable mark mean anything here; everything else is passed over.
    pub fn apply(&self, log_id: LogIdOf<TypeConfig>, payload: EntryPayloadOf<TypeConfig>) -> Reply {
        let mut inner = self.lock();
        inner.last_applied = Some(log_id);
        match payload {
            EntryPayload::Membership(m) => {
                let membership = StoredMembershipOf::<TypeConfig>::new(Some(log_id), m);
                inner.membership = membership.clone();
                inner.since.push(membership);
            }
            EntryPayload::Normal(Proposal::Durable { through }) => {
                // Never past this entry: a mark can only speak of what came before it.
                if inner.durable < Some(through) && through <= log_id {
                    inner.durable = Some(through);
                }
            }
            EntryPayload::Blank | EntryPayload::Normal(_) => {}
        }
        Reply {
            log_index: log_id.index(),
            ..Reply::default()
        }
    }

    pub fn applied_state(&self) -> (Option<LogIdOf<TypeConfig>>, StoredMembershipOf<TypeConfig>) {
        let inner = self.lock();
        (inner.last_applied, inner.membership.clone())
    }

    pub fn last_applied_index(&self) -> Option<u64> {
        self.lock().last_applied.map(|l| l.index())
    }

    /// How far every data voter holds the log, as last recorded in it.
    pub fn durable_index(&self) -> Option<u64> {
        self.lock().durable.map(|l| l.index())
    }

    /// Whether the data voters hold more of the log than the marker covers,
    /// so a new marker would let Raft drop some of it.
    pub fn wants_marker(&self) -> bool {
        let inner = self.lock();
        inner.durable > inner.saved.through
    }

    fn snapshot_of(saved: &Saved) -> SnapshotOf<TypeConfig, Vec<u8>> {
        SnapshotOf::<TypeConfig, Vec<u8>> {
            meta: SnapshotMetaOf::<TypeConfig> {
                last_log_id: saved.through,
                last_membership: saved.membership.clone(),
            },
            snapshot: MARKER.to_vec(),
        }
    }

    fn write(inner: &Inner, saved: &Saved) -> io::Result<()> {
        write_atomic(&inner.dir.join(FILE), &serde_json::to_vec(saved).map_err(invalid)?)
    }

    /// A marker at the durable point, with the membership in force there.
    /// Written to disk before Raft is told, so the log behind it is dropped
    /// only once the marker can be read back.
    pub fn build(&self) -> io::Result<SnapshotOf<TypeConfig, Vec<u8>>> {
        let mut inner = self.lock();
        let through = inner.durable;
        if through <= inner.saved.through {
            return Ok(Self::snapshot_of(&inner.saved));
        }
        // The last membership logged at or before the mark; earlier ones
        // are of no more use.
        let keep_from = inner.since.iter().rposition(|m| m.log_id().as_ref() <= through.as_ref());
        let membership = match keep_from {
            Some(i) => {
                let m = inner.since[i].clone();
                inner.since.drain(..=i);
                m
            }
            None => inner.saved.membership.clone(),
        };
        let saved = Saved { through, membership };
        Self::write(&inner, &saved)?;
        inner.saved = saved;
        tracing::info!(through = ?through.map(|l| l.index()), "witness marker written; the log behind it can go");
        Ok(Self::snapshot_of(&inner.saved))
    }

    /// Take the leader's snapshot as a position only: whatever state it
    /// carries is not kept.
    pub fn install(&self, meta: &SnapshotMetaOf<TypeConfig>) -> io::Result<()> {
        let mut inner = self.lock();
        let saved = Saved {
            through: meta.last_log_id,
            membership: meta.last_membership.clone(),
        };
        Self::write(&inner, &saved)?;
        inner.last_applied = meta.last_log_id;
        inner.membership = meta.last_membership.clone();
        inner.since.clear();
        inner.durable = inner.durable.max(meta.last_log_id);
        inner.saved = saved;
        tracing::info!(through = ?meta.last_log_id.map(|l| l.index()), "witness took the leader's position; its state was not kept");
        Ok(())
    }

    pub fn current(&self) -> Option<SnapshotOf<TypeConfig, Vec<u8>>> {
        let inner = self.lock();
        inner.saved.through.is_some().then(|| Self::snapshot_of(&inner.saved))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use openraft::testing::log_id;
    use openraft::Membership;

    use super::*;

    fn at(index: u64) -> LogIdOf<TypeConfig> {
        log_id::<TypeConfig>(1, 1, index)
    }

    fn voters(ids: &[u64]) -> EntryPayloadOf<TypeConfig> {
        EntryPayload::Membership(Membership::new_with_defaults(vec![ids.iter().copied().collect::<BTreeSet<u64>>()], []))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ic-witness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Purpose: a witness takes in only memberships and the data voters' durable marks; a
    /// marker is wanted only once the mark is past the last marker, is written at the mark
    /// (not at the last applied entry) with the membership in force there, survives a restart,
    /// and a mark that claims an entry after itself is ignored. Its snapshot content is the
    /// marker, which a data voter recognises and refuses.
    /// Why it matters: the marker decides what log the witness drops. One taken past the mark
    /// would drop entries a surviving region may still need from it after a region loss; one
    /// with the wrong membership would make a restarted witness vote in the wrong group.
    #[test]
    fn the_marker_follows_the_data_voters_mark_and_survives_a_restart() {
        let dir = scratch("marker");
        let witness = Witness::open(&dir).unwrap();
        assert!(witness.current().is_none() && !witness.wants_marker());
        witness.apply(at(1), voters(&[1, 2, 3]));
        let reply = witness.apply(at(2), EntryPayload::Normal(Proposal::Durable { through: at(1) }));
        assert_eq!(reply.log_index, 2);
        witness.apply(at(3), voters(&[1, 2, 3, 4, 5]));
        witness.apply(at(4), EntryPayload::Blank);
        // A mark can only speak of what came before it.
        witness.apply(at(5), EntryPayload::Normal(Proposal::Durable { through: at(9) }));
        assert_eq!((witness.durable_index(), witness.last_applied_index()), (Some(1), Some(5)));
        assert!(witness.wants_marker());

        let marker = witness.build().unwrap();
        assert!(is_marker(&marker.snapshot));
        assert_eq!(marker.meta.last_log_id, Some(at(1)));
        assert_eq!(marker.meta.last_membership.log_id(), &Some(at(1)), "the membership in force at the mark");
        assert!(!witness.wants_marker());
        // Built again with nothing new, it is the same marker.
        assert_eq!(witness.build().unwrap().meta, marker.meta);

        witness.apply(at(6), EntryPayload::Normal(Proposal::Durable { through: at(4) }));
        let later = witness.build().unwrap();
        assert_eq!(later.meta.last_log_id, Some(at(4)));
        assert_eq!(later.meta.last_membership.log_id(), &Some(at(3)));

        let reopened = Witness::open(&dir).unwrap();
        assert_eq!(reopened.current().unwrap().meta, later.meta);
        assert_eq!(reopened.applied_state().0, Some(at(4)));
        assert!(is_marker(MARKER) && !is_marker(b"{}"));
        assert!(refuse_marker().to_string().contains("never installs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Purpose: a leader's snapshot sent to a witness is taken as a position only: the witness
    /// moves to it, keeps the leader's membership, writes it down, and keeps no state; a file
    /// that is not a marker is refused at start.
    /// Why it matters: a witness that fell behind is brought up by a data voter's snapshot,
    /// which is the whole partition. Keeping it would put the ledger's data on the one node
    /// meant to have none; refusing it would leave the witness unable ever to catch up.
    #[test]
    fn a_leaders_snapshot_moves_the_witness_and_leaves_it_empty() {
        let dir = scratch("install");
        let witness = Witness::open(&dir).unwrap();
        witness.apply(at(1), voters(&[1, 2, 3]));
        let meta = SnapshotMetaOf::<TypeConfig> {
            last_log_id: Some(at(40)),
            last_membership: StoredMembershipOf::<TypeConfig>::new(Some(at(30)), Membership::new_with_defaults(vec![BTreeSet::from([1, 2, 5])], [])),
        };
        witness.install(&meta).unwrap();
        assert_eq!(witness.applied_state(), (Some(at(40)), meta.last_membership.clone()));
        assert_eq!(witness.durable_index(), Some(40));
        assert!(is_marker(&witness.current().unwrap().snapshot));
        assert_eq!(Witness::open(&dir).unwrap().current().unwrap().meta, meta);
        std::fs::write(dir.join(FILE), b"not json").unwrap();
        assert!(Witness::open(&dir).unwrap_err().to_string().contains("not a witness marker"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
