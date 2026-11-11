//! The durable Raft log of one node.
//!
//! Entries live in memory for reads and, when the store has a directory, in
//! an append-only journal on disk. A truncation (a follower discarding
//! entries a new leader never committed) is itself a journal record, so the
//! file is only ever appended to. An append is acknowledged after `fsync`.
//!
//! The vote and the commit index are small files replaced atomically.
//!
//! When a snapshot covers the start of the log, those entries are dropped
//! and the journal is rewritten without them.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::ops::RangeBounds;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use openraft::alias::{EntryOf, LogIdOf, VoteOf};
use openraft::entry::RaftEntry;
use openraft::storage::{IOFlushed, LogState, RaftLogReader, RaftLogStorage};
use openraft::OptionalSend;
use serde::{Deserialize, Serialize};

use crate::types::TypeConfig;

type Entry = EntryOf<TypeConfig>;
type LogId = LogIdOf<TypeConfig>;
type Vote = VoteOf<TypeConfig>;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Record {
    Append(Entry),
    /// Discard every entry after this index. `None` discards everything.
    TruncateAfter(Option<u64>),
    /// Every entry up to and including this one is covered by a snapshot and
    /// has been dropped.
    Purged(LogId),
}

struct Inner {
    entries: BTreeMap<u64, Entry>,
    vote: Option<Vote>,
    committed: Option<LogId>,
    purged: Option<LogId>,
    dir: Option<PathBuf>,
    journal: Option<File>,
}

/// A node's Raft log. Cheap to clone: clones share the same log.
#[derive(Clone)]
pub struct LogStore {
    inner: Arc<Mutex<Inner>>,
}

fn invalid(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Replace a small file atomically and durably.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(invalid),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl LogStore {
    /// A log held only in memory: lost when the node stops.
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: BTreeMap::new(),
                vote: None,
                committed: None,
                purged: None,
                dir: None,
                journal: None,
            })),
        }
    }

    /// Open, or create, a durable log in `dir`, replaying its journal.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("log.jsonl");
        let mut entries = BTreeMap::new();
        let mut purged = None;
        if path.exists() {
            let mut reader = BufReader::new(File::open(&path)?);
            let mut line = String::new();
            let mut good = 0u64;
            loop {
                line.clear();
                let n = reader.read_line(&mut line)?;
                if n == 0 {
                    break;
                }
                // A record is acknowledged only once its newline is on disk. A
                // final line without one is a write the crash interrupted: it
                // was never acknowledged, so it is dropped.
                if !line.ends_with('\n') {
                    break;
                }
                match serde_json::from_str::<Record>(&line).map_err(invalid)? {
                    Record::Append(e) => {
                        entries.insert(e.index(), e);
                    }
                    Record::TruncateAfter(Some(index)) => {
                        entries.split_off(&(index + 1));
                    }
                    Record::TruncateAfter(None) => entries.clear(),
                    Record::Purged(upto) => {
                        entries = entries.split_off(&(upto.index() + 1));
                        purged = Some(upto);
                    }
                }
                good += n as u64;
            }
            // Cut the torn tail off so the next append starts on a clean line.
            OpenOptions::new().write(true).open(&path)?.set_len(good)?;
        }
        let journal = OpenOptions::new().create(true).append(true).open(&path)?;
        let vote = read_json(&dir.join("vote.json"))?;
        let committed = read_json(&dir.join("committed.json"))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                entries,
                vote,
                committed,
                purged,
                dir: Some(dir),
                journal: Some(journal),
            })),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means a panic mid-write; the data is still the best we have.
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Index of the last entry held, for tests and status pages.
    pub fn last_index(&self) -> Option<u64> {
        self.lock().entries.keys().next_back().copied()
    }

    /// Index of the first entry still held: one past the last one dropped
    /// behind a snapshot.
    pub fn first_index(&self) -> Option<u64> {
        self.lock().entries.keys().next().copied()
    }

    /// The committed command envelopes still in the log, in order: what an
    /// auditor replays on top of the snapshot before them.
    pub fn commands(&self) -> Vec<(u64, ic_core::Envelope)> {
        let inner = self.lock();
        let upto = inner.committed.as_ref().map(|c| c.index());
        inner
            .entries
            .values()
            .filter(|e| upto.is_some_and(|u| e.index() <= u))
            .filter_map(|e| match &e.payload {
                openraft::EntryPayload::Normal(crate::types::Proposal::Command(envelope)) => Some((e.index(), envelope.clone())),
                _ => None,
            })
            .collect()
    }
}

impl Inner {
    fn journal(&mut self, records: &[Record]) -> io::Result<()> {
        let Some(file) = self.journal.as_mut() else {
            return Ok(());
        };
        let mut buf = Vec::new();
        for r in records {
            serde_json::to_writer(&mut buf, r).map_err(invalid)?;
            buf.push(b'\n');
        }
        file.write_all(&buf)?;
        file.sync_data()
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(&mut self, range: RB) -> Result<Vec<Entry>, io::Error> {
        Ok(self.lock().entries.range(range).map(|(_, e)| e.clone()).collect())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote>, io::Error> {
        Ok(self.lock().vote)
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, io::Error> {
        let inner = self.lock();
        Ok(LogState {
            last_purged_log_id: inner.purged,
            last_log_id: inner.entries.values().next_back().map(RaftEntry::log_id),
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote) -> Result<(), io::Error> {
        let mut inner = self.lock();
        if let Some(dir) = &inner.dir {
            write_atomic(&dir.join("vote.json"), &serde_json::to_vec(vote).map_err(invalid)?)?;
        }
        inner.vote = Some(*vote);
        Ok(())
    }

    async fn save_committed(&mut self, committed: Option<LogId>) -> Result<(), io::Error> {
        let mut inner = self.lock();
        if let Some(dir) = &inner.dir {
            write_atomic(&dir.join("committed.json"), &serde_json::to_vec(&committed).map_err(invalid)?)?;
        }
        inner.committed = committed;
        Ok(())
    }

    async fn read_committed(&mut self) -> Result<Option<LogId>, io::Error> {
        Ok(self.lock().committed)
    }

    async fn append<I>(&mut self, entries: I, callback: IOFlushed<TypeConfig>) -> Result<(), io::Error>
    where
        I: IntoIterator<Item = Entry> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let result = {
            let mut inner = self.lock();
            let entries: Vec<Entry> = entries.into_iter().collect();
            let records: Vec<Record> = entries.iter().cloned().map(Record::Append).collect();
            // Disk first: an entry is visible to readers only once it is durable.
            inner.journal(&records).map(|()| {
                for e in entries {
                    inner.entries.insert(e.index(), e);
                }
            })
        };
        match result {
            Ok(()) => {
                callback.io_completed(Ok(()));
                Ok(())
            }
            Err(e) => {
                callback.io_completed(Err(io::Error::new(e.kind(), e.to_string())));
                Err(e)
            }
        }
    }

    async fn truncate_after(&mut self, last_log_id: Option<LogId>) -> Result<(), io::Error> {
        let mut inner = self.lock();
        let keep = last_log_id.map(|l| l.index());
        inner.journal(&[Record::TruncateAfter(keep)])?;
        match keep {
            Some(index) => {
                inner.entries.split_off(&(index + 1));
            }
            None => inner.entries.clear(),
        }
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId) -> Result<(), io::Error> {
        let mut inner = self.lock();
        let keep = inner.entries.split_off(&(log_id.index() + 1));
        // Rewrite the journal with only what is kept, beside the old one,
        // and swap it in. A crash before the swap leaves the old journal,
        // whose extra entries a snapshot already covers; after it, the new.
        if let Some(dir) = inner.dir.clone() {
            let mut buf = Vec::new();
            for record in std::iter::once(Record::Purged(log_id)).chain(keep.values().cloned().map(Record::Append)) {
                serde_json::to_writer(&mut buf, &record).map_err(invalid)?;
                buf.push(b'\n');
            }
            let path = dir.join("log.jsonl");
            write_atomic(&path, &buf)?;
            inner.journal = Some(OpenOptions::new().append(true).open(&path)?);
        }
        inner.entries = keep;
        inner.purged = Some(log_id);
        Ok(())
    }
}
