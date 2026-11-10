//! The seam between the ledger state machine and consensus.
//!
//! The state machine is written against [`OrderedLog`]: an ordered, durable
//! log per partition. It knows nothing about Raft. Production puts an
//! `openraft` group behind this trait (five voters across New Zealand North,
//! Australia East and Australia Southeast); development, tests and the
//! simulation use [`MemoryLog`] or [`FileLog`].

use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt record at index {0}")]
    Corrupt(u64),
    #[error("encoding: {0}")]
    Encoding(String),
}

/// An ordered, durable log. Indexes start at 1 and have no gaps.
pub trait OrderedLog {
    /// Append a record and return its index once it is durable.
    fn append(&mut self, record: &[u8]) -> Result<u64, LogError>;
    /// Read records with index >= `from`, at most `max`.
    fn read(&self, from: u64, max: usize) -> Result<Vec<(u64, Vec<u8>)>, LogError>;
    fn last_index(&self) -> u64;
}

impl OrderedLog for Box<dyn OrderedLog + Send> {
    fn append(&mut self, record: &[u8]) -> Result<u64, LogError> {
        (**self).append(record)
    }
    fn read(&self, from: u64, max: usize) -> Result<Vec<(u64, Vec<u8>)>, LogError> {
        (**self).read(from, max)
    }
    fn last_index(&self) -> u64 {
        (**self).last_index()
    }
}

/// In-memory log for tests and simulation.
#[derive(Debug, Default, Clone)]
pub struct MemoryLog {
    records: Vec<Vec<u8>>,
}

impl MemoryLog {
    pub fn new() -> Self {
        Self::default()
    }
}

impl OrderedLog for MemoryLog {
    fn append(&mut self, record: &[u8]) -> Result<u64, LogError> {
        self.records.push(record.to_vec());
        Ok(self.records.len() as u64)
    }
    fn read(&self, from: u64, max: usize) -> Result<Vec<(u64, Vec<u8>)>, LogError> {
        let start = from.max(1) as usize - 1;
        Ok(self
            .records
            .iter()
            .enumerate()
            .skip(start)
            .take(max)
            .map(|(i, r)| (i as u64 + 1, r.clone()))
            .collect())
    }
    fn last_index(&self) -> u64 {
        self.records.len() as u64
    }
}

/// A single-node file log: length-prefixed, checksummed records, fsynced
/// before the append is acknowledged. For the insurance in a box, not for
/// production.
#[derive(Debug)]
pub struct FileLog {
    path: PathBuf,
    file: File,
    last: u64,
}

fn checksum(index: u64, record: &[u8]) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(index.to_be_bytes());
    h.update(record);
    let d = h.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(&d[..8]);
    out
}

impl FileLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LogError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().create(true).append(true).read(true).open(&path)?;
        let mut log = Self { path, file, last: 0 };
        log.last = log.scan(1, usize::MAX)?.last().map_or(0, |(i, _)| *i);
        Ok(log)
    }

    fn scan(&self, from: u64, max: usize) -> Result<Vec<(u64, Vec<u8>)>, LogError> {
        let mut r = BufReader::new(File::open(&self.path)?);
        let mut out = Vec::new();
        let mut index = 0u64;
        loop {
            let mut len = [0u8; 4];
            match r.read_exact(&mut len) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            index += 1;
            let mut sum = [0u8; 8];
            r.read_exact(&mut sum).map_err(|_| LogError::Corrupt(index))?;
            let mut rec = vec![0u8; u32::from_be_bytes(len) as usize];
            r.read_exact(&mut rec).map_err(|_| LogError::Corrupt(index))?;
            if checksum(index, &rec) != sum {
                return Err(LogError::Corrupt(index));
            }
            if index >= from && out.len() < max {
                out.push((index, rec));
            }
        }
        Ok(out)
    }
}

impl OrderedLog for FileLog {
    fn append(&mut self, record: &[u8]) -> Result<u64, LogError> {
        let index = self.last + 1;
        let mut buf = Vec::with_capacity(record.len() + 12);
        buf.extend_from_slice(&(record.len() as u32).to_be_bytes());
        buf.extend_from_slice(&checksum(index, record));
        buf.extend_from_slice(record);
        self.file.write_all(&buf)?;
        self.file.sync_data()?;
        self.last = index;
        Ok(index)
    }
    fn read(&self, from: u64, max: usize) -> Result<Vec<(u64, Vec<u8>)>, LogError> {
        self.scan(from, max)
    }
    fn last_index(&self) -> u64 {
        self.last
    }
}

/// A deterministic state machine driven only by the log.
pub trait StateMachine {
    type Command: Serialize + DeserializeOwned;
    type Output;
    /// Apply the command at `index`. Must be a pure function of the current
    /// state and the command.
    fn apply(&mut self, index: u64, command: Self::Command) -> Self::Output;
}

/// A log plus the state machine it drives. Stands in for one partition's
/// Raft group leader.
pub struct Replica<L: OrderedLog, S: StateMachine> {
    pub log: L,
    pub state: S,
    applied: u64,
}

impl<L: OrderedLog, S: StateMachine> Replica<L, S> {
    /// Build a replica and replay whatever the log already holds.
    pub fn recover(log: L, state: S) -> Result<Self, LogError> {
        Self::recover_with(log, state, |_, _| {})
    }

    /// Like [`Self::recover`], handing each replayed output to `on_output`,
    /// so a host can rebuild what it derives from outputs (its outbox).
    pub fn recover_with(log: L, state: S, mut on_output: impl FnMut(u64, S::Output)) -> Result<Self, LogError> {
        let mut r = Self { log, state, applied: 0 };
        r.catch_up(&mut on_output)?;
        Ok(r)
    }

    pub fn applied(&self) -> u64 {
        self.applied
    }

    /// Append the command, then apply it. Returns the log index and output.
    pub fn propose(&mut self, command: S::Command) -> Result<(u64, S::Output), LogError> {
        let bytes = serde_json::to_vec(&command).map_err(|e| LogError::Encoding(e.to_string()))?;
        let index = self.log.append(&bytes)?;
        // Apply what was written, not what was passed in, so apply and replay
        // take exactly the same path.
        let cmd: S::Command = serde_json::from_slice(&bytes).map_err(|e| LogError::Encoding(e.to_string()))?;
        let out = self.state.apply(index, cmd);
        self.applied = index;
        Ok((index, out))
    }

    fn catch_up(&mut self, on_output: &mut impl FnMut(u64, S::Output)) -> Result<(), LogError> {
        loop {
            let batch = self.log.read(self.applied + 1, 1024)?;
            if batch.is_empty() {
                return Ok(());
            }
            for (index, bytes) in batch {
                let cmd: S::Command = serde_json::from_slice(&bytes).map_err(|e| LogError::Encoding(e.to_string()))?;
                let out = self.state.apply(index, cmd);
                on_output(index, out);
                self.applied = index;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Sum(i64);
    impl StateMachine for Sum {
        type Command = i64;
        type Output = i64;
        fn apply(&mut self, _index: u64, c: i64) -> i64 {
            self.0 += c;
            self.0
        }
    }

    #[test]
    fn memory_replay_matches() {
        let mut r = Replica::recover(MemoryLog::new(), Sum::default()).unwrap();
        for i in 1..=10 {
            r.propose(i).unwrap();
        }
        let again = Replica::recover(r.log.clone(), Sum::default()).unwrap();
        assert_eq!(again.state.0, 55);
        assert_eq!(again.applied(), 10);
    }

    #[test]
    fn file_log_survives_reopen_and_detects_corruption() {
        let dir = std::env::temp_dir().join(format!("ic-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p0.log");
        let _ = std::fs::remove_file(&path);
        {
            let mut r = Replica::recover(FileLog::open(&path).unwrap(), Sum::default()).unwrap();
            r.propose(7).unwrap();
            r.propose(8).unwrap();
        }
        let r = Replica::recover(FileLog::open(&path).unwrap(), Sum::default()).unwrap();
        assert_eq!(r.state.0, 15);
        let mut bytes = std::fs::read(&path).unwrap();
        let n = bytes.len();
        bytes[n - 1] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(FileLog::open(&path), Err(LogError::Corrupt(2))));
        let _ = std::fs::remove_dir_all(&dir);
    }
    fn temp_log(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ic-log-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("p0.log")
    }

    /// Purpose: a boxed log behaves exactly like the log inside it, and recovery hands
    /// every replayed output to the host in order.
    /// Why it matters: the API host holds its logs as trait objects so it can choose file
    /// or memory at start-up, and rebuilds its outbox from replayed outputs. If the box
    /// dropped a call or replay skipped an output, events would be lost on every restart.
    #[test]
    fn a_boxed_log_replays_every_output_to_the_host() {
        let boxed: Box<dyn OrderedLog + Send> = Box::new(MemoryLog::new());
        let mut r = Replica::recover(boxed, Sum::default()).unwrap();
        for i in 1..=4 {
            r.propose(i).unwrap();
        }
        assert_eq!(r.log.last_index(), 4);
        assert_eq!(r.log.read(3, 10).unwrap().len(), 2);
        let mut seen = Vec::new();
        let again = Replica::recover_with(r.log, Sum::default(), |index, out| seen.push((index, out))).unwrap();
        assert_eq!(seen, vec![(1, 1), (2, 3), (3, 6), (4, 10)]);
        assert_eq!(again.applied(), 4);
    }

    /// Purpose: reads honour the starting index and the maximum count on both log kinds,
    /// and the last index tracks appends.
    /// Why it matters: catch-up reads the log in batches. An off-by-one in either bound
    /// would apply a command twice or skip one, and replicas would diverge.
    #[test]
    fn reads_honour_the_start_index_and_the_batch_limit() {
        let path = temp_log("reads");
        let mut file = FileLog::open(&path).unwrap();
        let mut memory = MemoryLog::new();
        assert_eq!((file.last_index(), memory.last_index()), (0, 0));
        for rec in [b"a", b"b", b"c"] {
            file.append(rec).unwrap();
            memory.append(rec).unwrap();
        }
        assert_eq!((file.last_index(), memory.last_index()), (3, 3));
        for log in [&file as &dyn OrderedLog, &memory] {
            assert_eq!(log.read(2, 1).unwrap(), vec![(2, b"b".to_vec())]);
            assert_eq!(log.read(2, 10).unwrap().len(), 2);
            assert!(log.read(4, 10).unwrap().is_empty());
        }
    }

    /// Purpose: a record cut short by a crash mid-write is reported as corruption at its
    /// index, and a record that is not a valid command stops recovery with an error.
    /// Why it matters: a torn tail must never be applied as if it were complete, and a
    /// replica must stop instead of guessing when it cannot decode what the log holds.
    #[test]
    fn a_torn_or_undecodable_record_stops_recovery() {
        let path = temp_log("torn");
        {
            let mut log = FileLog::open(&path).unwrap();
            log.append(b"1").unwrap();
            log.append(b"22").unwrap();
        }
        let whole = std::fs::read(&path).unwrap();
        // Cut inside the second record's body, then inside its checksum.
        std::fs::write(&path, &whole[..whole.len() - 1]).unwrap();
        assert!(matches!(FileLog::open(&path), Err(LogError::Corrupt(2))));
        std::fs::write(&path, &whole[..13 + 6]).unwrap();
        assert!(matches!(FileLog::open(&path), Err(LogError::Corrupt(2))));

        let mut junk = MemoryLog::new();
        junk.append(b"not a number").unwrap();
        assert!(matches!(Replica::recover(junk, Sum::default()), Err(LogError::Encoding(_))));
    }

    /// Purpose: a log file that cannot be opened is an I/O error, not a panic or an empty log.
    /// Why it matters: a missing volume must stop the partition. Starting empty would
    /// silently fork history.
    #[test]
    fn an_unopenable_log_is_an_error_not_an_empty_log() {
        let missing = std::env::temp_dir().join("ic-log-test-no-such-dir").join("deeper").join("p0.log");
        assert!(matches!(FileLog::open(missing), Err(LogError::Io(_))));
    }
}
