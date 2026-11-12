//! The driver around the group module: everything that touches the world
//! outside its state.
//!
//! The state machine decides; the driver does. It carries the module's
//! outbox to the core, to reinsurance, to underwriting and to the event
//! stream; it asks the customer master who a file's joiners are; and it
//! takes an employer's file from quarantine storage, through the malware
//! scan, to mapped rows. Every result goes back as a command whose key is
//! derived from the work itself, so a driver that stops anywhere is started
//! again and carries on: nothing is lost and nothing is done twice.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::NaiveDate;
use ic_core::{Command as CoreCommand, GroupId, Reject};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::command::{Envelope, ExcessDecision, GroupCommand, Resolution};
use crate::csv::{self, CsvError};
use crate::event::{GroupEvent, IssueOutcome, Outbound, ReinsuranceNotice, UnderwritingRequest};
use crate::host::{FileToRead, Question};
use crate::mapping::{FileMapping, Mapped, MappingError};
use crate::record::MemberIdentity;
use crate::state::{FileStatus, Outcome, MAX_ROWS_PER_PART};

/// Joiners answered per command, so that no command is too large.
const RESOLUTIONS_PER_PART: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    /// Something could not be reached. Nothing is known to have happened:
    /// do the same again.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("refused: {0}")]
    Refused(Reject),
    #[error("unexpected answer from the module: {0}")]
    Answer(String),
}

/// The group module, wherever it runs.
pub trait Module: Send + Sync {
    fn propose(&self, envelope: Envelope) -> impl Future<Output = Result<Outcome, DriverError>> + Send;
    fn ask(&self, question: Question) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// The core.
pub trait Core: Send + Sync {
    /// Send one command as `group-admin` under its key. `Ok(None)` is
    /// accepted, `Ok(Some(code))` is the core's refusal, and `Err` means
    /// nothing is known: the same command goes again under the same key.
    fn submit(&self, key: &str, group: &GroupId, command: &CoreCommand) -> impl Future<Output = Result<Option<String>, String>> + Send;
    /// The business date, once every partition is open.
    fn business_date(&self) -> impl Future<Output = Result<Option<NaiveDate>, String>> + Send;
}

/// The other modules and the event stream. Each call is at least once: the
/// receiver de-duplicates on the key or the sequence number.
pub trait Peers: Send + Sync {
    fn reinsurance(&self, key: &str, on: NaiveDate, notice: &ReinsuranceNotice) -> impl Future<Output = Result<(), String>> + Send;
    /// Ask underwriting to decide a member's cover above the automatic
    /// amount. The request names the member's person.
    fn underwriting(&self, key: &str, today: NaiveDate, request: &UnderwritingRequest) -> impl Future<Output = Result<(), String>> + Send;
    /// Tell underwriting a request is no longer wanted.
    fn withdraw_underwriting(&self, key: &str, today: NaiveDate, request_key: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Tell underwriting what the core made of terms it accepted.
    fn underwriting_outcome(&self, key: &str, today: NaiveDate, request_key: &str, outcome: &IssueOutcome) -> impl Future<Output = Result<(), String>> + Send;
    /// Underwriting's answers to this module's requests, numbered after
    /// `after`, oldest first.
    fn underwriting_answers(&self, after: u64, max: usize) -> impl Future<Output = Result<Answers, String>> + Send;
    fn publish(&self, seq: u64, date: NaiveDate, event: &GroupEvent) -> impl Future<Output = Result<(), String>> + Send;
}

/// One answer from underwriting to a request of this module's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcessAnswer {
    /// The answer's own key there. It is stable, so it makes the key the
    /// decision is recorded under and an answer seen twice is recorded once.
    pub key: String,
    pub scheme: String,
    pub case: u64,
    pub decision: ExcessDecision,
}

/// A page of underwriting's answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answers {
    /// The last number read, whether or not it was an answer for this
    /// module. The next page starts after it.
    pub through: u64,
    /// There may be more after this page.
    pub more: bool,
    pub answers: Vec<ExcessAnswer>,
}

/// The customer master: who a joiner is. The only place a member's name goes.
pub trait CustomerMaster: Send + Sync {
    /// Match one joiner. A single high-confidence match is `Linked`; no
    /// candidate creates the person and is `Created`; anything between is
    /// `Possible` and a person decides.
    fn resolve(&self, scheme: &str, identity: &MemberIdentity) -> impl Future<Output = Result<Resolution, String>> + Send;
}

/// What the malware scan says about a stored file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    Pending,
    Clean,
    Malicious,
}

/// Quarantine storage for employer files.
pub trait FileStore: Send + Sync {
    /// Store a file. Storing the same name again is not an error.
    fn put(&self, scheme: &str, file: &str, bytes: Vec<u8>) -> impl Future<Output = Result<(), String>> + Send;
    fn scan(&self, scheme: &str, file: &str) -> impl Future<Output = Result<Scan, String>> + Send;
    fn read(&self, scheme: &str, file: &str) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pass {
    /// Outbox entries delivered and file steps taken.
    pub done: u32,
    /// Steps that could not be taken this time and will be tried again.
    pub failed: u32,
}

pub fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn decode<T: DeserializeOwned>(value: Value, field: &str) -> Result<T, DriverError> {
    let inner = value.get(field).cloned().unwrap_or(Value::Null);
    serde_json::from_value(inner).map_err(|e| DriverError::Answer(format!("{field}: {e}")))
}

fn csv_code(e: &CsvError) -> &'static str {
    match e {
        CsvError::NotText => "not_text",
        CsvError::UnclosedQuote(_) => "unclosed_quote",
        CsvError::TooManyRows => "too_many_rows",
        CsvError::TooManyFields(_) => "too_many_fields",
    }
}

fn mapping_code(e: &MappingError) -> &'static str {
    match e {
        MappingError::Invalid(_) => "invalid_mapping",
        MappingError::NoHeader => "no_header",
        MappingError::MissingColumn(_) => "missing_column",
        MappingError::DuplicateColumn(_) => "duplicate_column",
    }
}

pub struct Driver<M, C, P, D, F> {
    pub module: M,
    pub core: C,
    pub peers: P,
    pub directory: D,
    pub files: F,
    /// The secret the member fingerprints are salted with, from Key Vault.
    /// It never changes: changing it would make every member look new.
    salt: String,
    /// Outbox entries taken per pass.
    batch: usize,
    /// How far underwriting's answers have been read. Kept only in memory:
    /// a driver that starts again reads them all again, and each is
    /// recorded once because its key is the answer's own.
    answers_through: AtomicU64,
}

impl<M: Module, C: Core, P: Peers, D: CustomerMaster, F: FileStore> Driver<M, C, P, D, F> {
    pub fn new(module: M, core: C, peers: P, directory: D, files: F, salt: &str) -> Self {
        Self {
            module,
            core,
            peers,
            directory,
            files,
            salt: salt.to_string(),
            batch: 200,
            answers_through: AtomicU64::new(0),
        }
    }

    pub fn with_batch(mut self, batch: usize) -> Self {
        self.batch = batch.max(1);
        self
    }

    async fn today(&self) -> Result<NaiveDate, DriverError> {
        self.core
            .business_date()
            .await
            .map_err(DriverError::Unavailable)?
            .ok_or_else(|| DriverError::Unavailable("the business date is not open".to_string()))
    }

    async fn send(&self, key: String, command: GroupCommand) -> Result<Result<(), Reject>, DriverError> {
        let envelope = Envelope {
            key,
            caller: crate::command::caller::DRIVER.to_string(),
            actor: "group-driver".to_string(),
            today: self.today().await?,
            command,
        };
        Ok(self.module.propose(envelope).await?.result)
    }

    /// Store an uploaded file in quarantine and tell the module it is
    /// there. The bytes are stored first, so the module never knows of a
    /// file that storage does not hold. Nothing reads the file until the
    /// scan has passed it.
    pub async fn upload(&self, caller: &str, actor: &str, scheme: &str, file: &str, bytes: Vec<u8>) -> Result<(), DriverError> {
        let hash = content_hash(&bytes);
        self.files.put(scheme, file, bytes).await.map_err(DriverError::Unavailable)?;
        let envelope = Envelope {
            // The same bytes under the same name are the same upload; other
            // bytes under a name already used are refused by the module.
            key: format!("receive:{scheme}:{file}:{hash}"),
            caller: caller.to_string(),
            actor: actor.to_string(),
            today: self.today().await?,
            command: GroupCommand::ReceiveFile {
                scheme: scheme.to_string(),
                file: file.to_string(),
                content_hash: hash,
            },
        };
        self.module.propose(envelope).await?.result.map_err(DriverError::Refused)
    }

    /// One pass over everything outstanding.
    pub async fn run_once(&self) -> Result<Pass, DriverError> {
        let files = self.read_files().await?;
        let outbox = self.deliver().await?;
        let answers = self.collect_answers().await?;
        Ok(Pass {
            done: files.done + outbox.done + answers.done,
            failed: files.failed + outbox.failed + answers.failed,
        })
    }

    /// Bring underwriting's answers back as decisions on the cases they
    /// answer. Underwriting keeps its answers until its own driver clears
    /// them; reading them here takes nothing away.
    pub async fn collect_answers(&self) -> Result<Pass, DriverError> {
        let mut pass = Pass::default();
        loop {
            let after = self.answers_through.load(Ordering::Relaxed);
            let Ok(page) = self.peers.underwriting_answers(after, self.batch).await else {
                pass.failed += 1;
                return Ok(pass);
            };
            for answer in &page.answers {
                let command = GroupCommand::RecordExcessDecision {
                    scheme: answer.scheme.clone(),
                    case: answer.case,
                    decision: answer.decision.clone(),
                };
                match self.send(format!("uw:{}", answer.key), command).await? {
                    Ok(()) => pass.done += 1,
                    // The case was closed here first, or the answer cannot
                    // stand as given. Asking again would not change it.
                    Err(reject) => tracing::warn!(scheme = %answer.scheme, case = answer.case, code = %reject.code, "an underwriting answer was not recorded"),
                }
            }
            self.answers_through.store(page.through.max(after), Ordering::Relaxed);
            if !page.more {
                return Ok(pass);
            }
        }
    }

    /// Carry the module's outbox to where each entry belongs. An entry that
    /// cannot be delivered stays where it is and does not hold up the ones
    /// behind it: the pass reads on past it.
    pub async fn deliver(&self) -> Result<Pass, DriverError> {
        let today = self.today().await?;
        let mut pass = Pass::default();
        let mut after = None;
        loop {
            let question = Question::Outbox { limit: self.batch, after };
            let items: Vec<(u64, Outbound)> = decode(self.module.ask(question).await?, "items")?;
            let full = items.len() >= self.batch;
            // Entries that stay where they are after this batch.
            let mut left = 0usize;
            let mut delivered = vec![];
            for (id, out) in items {
                after = Some(id);
                let result = match &out {
                    Outbound::Core { key, group, command } => match self.core.submit(key, group, command).await {
                        Ok(error) => {
                            // The answer is the acknowledgement: the entry leaves the outbox with it.
                            self.send(format!("core-result:{key}"), GroupCommand::CoreResult { key: key.clone(), error })
                                .await?
                                .map_err(DriverError::Refused)?;
                            pass.done += 1;
                            continue;
                        }
                        Err(e) => Err(e),
                    },
                    Outbound::Reinsurance { key, notice } => self.peers.reinsurance(key, today, notice).await,
                    // Underwriting asks a person. Until the member is linked
                    // to one the request waits, which is nobody's failure.
                    Outbound::Underwriting { request, .. } if request.party.is_none() => {
                        left += 1;
                        continue;
                    }
                    Outbound::Underwriting { key, request } => self.peers.underwriting(key, today, request).await,
                    Outbound::WithdrawUnderwriting { key, request_key } => self.peers.withdraw_underwriting(key, today, request_key).await,
                    Outbound::UnderwritingOutcome { key, request_key, outcome } => self.peers.underwriting_outcome(key, today, request_key, outcome).await,
                    Outbound::Publish { seq, date, event } => self.peers.publish(*seq, *date, event).await,
                    Outbound::ResolveParties { scheme, file, employee_ids } => self.resolve(scheme, file, employee_ids).await,
                };
                match result {
                    Ok(()) => delivered.push(id),
                    Err(_) => {
                        pass.failed += 1;
                        left += 1;
                    }
                }
            }
            if !delivered.is_empty() {
                let ids = delivered.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
                pass.done += delivered.len() as u32;
                self.send(format!("ack:{}", content_hash(ids.as_bytes())), GroupCommand::Ack { ids: delivered })
                    .await?
                    .map_err(DriverError::Refused)?;
            }
            // Read on only past entries that are stuck; otherwise the next
            // pass takes the next batch.
            if !full || left == 0 {
                return Ok(pass);
            }
        }
    }

    async fn mapped(&self, scheme: &str, file: &str, expected_hash: Option<&str>) -> Result<Result<(FileMapping, Mapped), &'static str>, String> {
        let Some(bytes) = self.files.read(scheme, file).await? else {
            return Ok(Err("file_missing"));
        };
        if expected_hash.is_some_and(|h| h != content_hash(&bytes)) {
            return Ok(Err("content_changed"));
        }
        let answer = self
            .module
            .ask(Question::Mapping { scheme: scheme.to_string() })
            .await
            .map_err(|e| e.to_string())?;
        let mapping: Option<FileMapping> = decode(answer, "mapping").map_err(|e| e.to_string())?;
        let Some(mapping) = mapping else { return Ok(Err("no_mapping")) };
        let table = match csv::parse(&bytes, mapping.delimiter) {
            Ok(t) => t,
            Err(e) => return Ok(Err(csv_code(&e))),
        };
        Ok(match mapping.map(&table, &format!("{}:{scheme}", self.salt)) {
            Ok(mapped) => Ok((mapping, mapped)),
            Err(e) => Err(mapping_code(&e)),
        })
    }

    /// Ask the customer master who a file's joiners are. The names are read
    /// again from the stored file: the module's state never held them.
    async fn resolve(&self, scheme: &str, file: &str, employee_ids: &[String]) -> Result<(), String> {
        let (_, mapped) = self
            .mapped(scheme, file, None)
            .await?
            .map_err(|code| format!("file {file} cannot be read again: {code}"))?;
        let mut resolutions = Vec::with_capacity(employee_ids.len());
        for id in employee_ids {
            let identity = mapped
                .identities
                .iter()
                .find(|i| i.employee_id == *id)
                .ok_or_else(|| format!("file {file} has no name for a joiner"))?;
            resolutions.push((id.clone(), self.directory.resolve(scheme, identity).await?));
        }
        let parts: Vec<&[(String, Resolution)]> = resolutions.chunks(RESOLUTIONS_PER_PART).collect();
        for (n, part) in parts.iter().enumerate() {
            let command = GroupCommand::PartiesResolved {
                scheme: scheme.to_string(),
                file: file.to_string(),
                resolutions: part.to_vec(),
                last: n + 1 == parts.len(),
            };
            // A refusal here means the answers were already taken on an
            // earlier pass whose acknowledgement was lost.
            let _ = self.send(format!("parties:{scheme}:{file}:{n}"), command).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Take each waiting file one step: record its scan, or read and map it.
    pub async fn read_files(&self) -> Result<Pass, DriverError> {
        let files: Vec<FileToRead> = decode(self.module.ask(Question::FilesToRead).await?, "files")?;
        let mut pass = Pass::default();
        for f in files {
            let step = match f.status {
                FileStatus::Quarantined => self.record_scan(&f).await,
                _ => self.submit_rows(&f).await,
            };
            match step {
                Ok(true) => pass.done += 1,
                Ok(false) => {}
                Err(DriverError::Unavailable(_)) => pass.failed += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(pass)
    }

    async fn record_scan(&self, f: &FileToRead) -> Result<bool, DriverError> {
        let clean = match self.files.scan(&f.scheme, &f.file).await.map_err(DriverError::Unavailable)? {
            Scan::Pending => return Ok(false),
            Scan::Clean => true,
            Scan::Malicious => false,
        };
        let command = GroupCommand::RecordScan {
            scheme: f.scheme.clone(),
            file: f.file.clone(),
            clean,
        };
        self.send(format!("scan:{}:{}", f.scheme, f.file), command)
            .await?
            .map_err(DriverError::Refused)?;
        Ok(true)
    }

    async fn refuse(&self, f: &FileToRead, code: &str) -> Result<bool, DriverError> {
        let command = GroupCommand::RefuseFile {
            scheme: f.scheme.clone(),
            file: f.file.clone(),
            code: code.to_string(),
        };
        self.send(format!("refuse:{}:{}", f.scheme, f.file), command)
            .await?
            .map_err(DriverError::Refused)?;
        Ok(true)
    }

    async fn submit_rows(&self, f: &FileToRead) -> Result<bool, DriverError> {
        let (mapping, mapped) = match self.mapped(&f.scheme, &f.file, Some(&f.content_hash)).await.map_err(DriverError::Unavailable)? {
            Ok(m) => m,
            Err(code) => return self.refuse(f, code).await,
        };
        let today = self.today().await?;
        let mut rejected = Some(mapped.rejected);
        let mut parts: Vec<_> = mapped.rows.chunks(MAX_ROWS_PER_PART).map(<[_]>::to_vec).collect();
        if parts.is_empty() {
            parts.push(vec![]);
        }
        let count = parts.len();
        for (n, rows) in parts.into_iter().enumerate() {
            let command = GroupCommand::SubmitRows {
                scheme: f.scheme.clone(),
                file: f.file.clone(),
                mapping_hash: mapping.content_hash.clone(),
                kind: mapping.kind,
                file_date: today,
                rows,
                rejected: rejected.take().unwrap_or_default(),
                last: n + 1 == count,
            };
            if let Err(reject) = self.send(format!("rows:{}:{}:{n}", f.scheme, f.file), command).await? {
                // The module will not take the rows as they are: the file is
                // refused and the employer sends it again.
                return self.refuse(f, &reject.code).await;
            }
        }
        Ok(true)
    }
}
