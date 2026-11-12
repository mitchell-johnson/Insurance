//! The driver of the close: what carries facts between the finance ledger
//! and everything outside it.
//!
//! The ledger decides; the driver carries. One pass asks the ledger what the
//! close is waiting for and does what it can of it:
//!
//! - records the published Merkle root of each closed date, read from
//!   immutable storage and verified before it is sent;
//! - for a period past its last day, reads each extract from the ledger's
//!   data mart, writes the file to immutable storage and registers it with
//!   its hash, totals and the Merkle root of its date;
//! - asks each module that owes the close a report and passes the answer
//!   on under that module's name;
//! - reports whether every sub-ledger answered, and closes a period that is
//!   signed off with nothing blocking.
//!
//! A pass keeps nothing between runs. Everything it sends carries an
//! idempotency key made from what it is about, and a file is never written
//! twice, so a driver stopped anywhere is started again and carries on. One
//! thing that cannot be done yet never stops the rest: it is listed in
//! [`Pass::waiting`] and tried again on the next pass.

use std::future::Future;

use chrono::NaiveDate;
use ic_azure::blob::WormStore;
use ic_azure::merkle::{root_name, verify_published, PublishedRoot};
use ic_azure::AzureError;
use ic_core::invest::WaitingCash;
use ic_core::Reject;
use ic_ledger::{sha256, Hash32};
use serde_json::{json, Value};

use crate::books::{AgedItem, ExtractRecord, FinCommand, FinEnvelope};
use crate::close::Outcome;
use crate::mart::{DEFINITION_VERSION, EXPERIENCE, IN_FORCE, MOVEMENT};
use crate::period::PeriodId;
use crate::report::{ExtractPage, PeriodWork, Work};

/// The check the driver reports for itself.
pub const SUBLEDGER_CHECK: &str = "subledger_reconciliations";
/// The name the driver closes periods and reports its own check under.
pub const CLOSE_WORKFLOW: &str = "close-workflow";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    /// Something could not be reached. Nothing is known to have happened:
    /// the next pass does the same again.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("unexpected answer: {0}")]
    Answer(String),
}

impl From<AzureError> for DriverError {
    fn from(e: AzureError) -> Self {
        DriverError::Unavailable(e.to_string())
    }
}

/// The finance ledger, wherever it runs.
pub trait Books: Send + Sync {
    /// Send one command. The inner result is the ledger's own answer.
    fn propose(&self, envelope: FinEnvelope) -> impl Future<Output = Result<Result<(), Reject>, DriverError>> + Send;
    fn ask(&self, question: Value) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// The other modules of the node.
pub trait Modules: Send + Sync {
    fn ask(&self, module: &str, question: Value) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// What a module's answer is passed on as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reports {
    /// A close check. The answer is `{"green": bool, "items": [...]}`.
    Check(String),
    /// Aged items of a kind. The answer is the body of `ReportAgedItems`.
    Aged(String),
}

impl Reports {
    fn check_id(&self) -> String {
        match self {
            Reports::Check(id) => id.clone(),
            Reports::Aged(kind) => format!("aged:{kind}"),
        }
    }
}

/// One thing a module owes the close.
#[derive(Debug, Clone)]
pub struct Source {
    /// The caller the close configuration names for this report.
    pub caller: String,
    pub module: String,
    pub reports: Reports,
    /// The question to ask for a period ending on the date given.
    pub question: fn(NaiveDate) -> Value,
}

/// What the adviser commission module owes: its payment sweep and the
/// balances it holds, aged by party.
pub fn distribution_sources() -> Vec<Source> {
    vec![
        Source {
            caller: "commission".to_string(),
            module: "distribution".to_string(),
            reports: Reports::Check("commission_sweep".to_string()),
            question: |_| json!("sweep_check"),
        },
        Source {
            caller: "commission".to_string(),
            module: "distribution".to_string(),
            reports: Reports::Aged("held_balance".to_string()),
            question: |as_at| json!({ "aged_held": { "as_at": as_at } }),
        },
    ]
}

/// What the payments module owes: the statutory fund cash settlement and
/// the bank receipts nobody has identified.
pub fn payments_sources() -> Vec<Source> {
    vec![
        Source {
            caller: "payments".to_string(),
            module: "payments".to_string(),
            reports: Reports::Check("fund_cash_settlement".to_string()),
            question: |as_at| json!({ "q": "fund_cash_settlement", "as_at": as_at }),
        },
        Source {
            caller: "payments".to_string(),
            module: "payments".to_string(),
            reports: Reports::Aged("unidentified_receipts".to_string()),
            question: |as_at| json!({ "q": "aged_unidentified_receipts", "as_at": as_at }),
        },
    ]
}

/// What the reinsurance module owes at a quarter end: statements cut and
/// swept, and nothing undelivered or unreconciled.
pub fn reinsurance_sources() -> Vec<Source> {
    vec![Source {
        caller: "reinsurance".to_string(),
        module: "reinsurance".to_string(),
        reports: Reports::Check("reinsurance_sweep".to_string()),
        question: |end| json!({ "sweep_check": { "period_end": end } }),
    }]
}

/// The name the core's partitions are asked under, beside the modules.
pub const CORE: &str = "core";

/// The most aged items one report lists. The total and count cover all.
pub const MAX_AGED_ITEMS: usize = 2_000;

/// What the core's investment ledger owes: cash waiting to be invested.
pub fn core_sources() -> Vec<Source> {
    vec![Source {
        caller: "investment".to_string(),
        module: CORE.to_string(),
        reports: Reports::Aged("pending_investment_cash".to_string()),
        question: |as_at| json!({ "waiting_cash": { "as_at": as_at } }),
    }]
}

/// Every partition's waiting cash as one aged report: one item for each
/// policy, oldest first, so what a long list cuts off is the newest.
pub fn waiting_cash_report(as_at: NaiveDate, mut waiting: Vec<WaitingCash>) -> Value {
    waiting.retain(|w| w.amount_minor != 0);
    waiting.sort_by(|a, b| (a.since, &a.policy).cmp(&(b.since, &b.policy)));
    let total: i64 = waiting.iter().map(|w| w.amount_minor).sum();
    let items: Vec<Value> = waiting
        .iter()
        .take(MAX_AGED_ITEMS)
        .map(|w| json!({ "party": w.policy, "reason": "pending_investment_cash", "amount_minor": w.amount_minor, "since": w.since }))
        .collect();
    json!({ "as_at": as_at, "items": items, "total_minor": total, "count": waiting.len() })
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    pub roots_recorded: Vec<NaiveDate>,
    /// IDs of the extracts stored and registered.
    pub extracts_registered: Vec<String>,
    /// IDs of the checks reported.
    pub reports_sent: Vec<String>,
    pub closed: Vec<PeriodId>,
    /// What could not be done yet, in words a person can act on.
    pub waiting: Vec<String>,
}

pub struct Driver<B, M, R, X> {
    pub books: B,
    pub modules: M,
    /// Where the core publishes each day's signed Merkle root.
    pub roots: R,
    /// Immutable storage for extract files.
    pub extracts: X,
    /// What is written before a file's name in a registered location.
    pub location: String,
    pub sources: Vec<Source>,
}

#[derive(serde::Deserialize)]
struct CheckAnswer {
    green: bool,
    #[serde(default)]
    items: Vec<Value>,
}

#[derive(serde::Deserialize)]
struct AgedAnswer {
    as_at: NaiveDate,
    #[serde(default)]
    items: Vec<AgedItem>,
    total_minor: i64,
    count: u32,
}

fn decode<T: serde::de::DeserializeOwned>(what: &str, value: Value) -> Result<T, DriverError> {
    serde_json::from_value(value).map_err(|e| DriverError::Answer(format!("{what}: {e}")))
}

fn short(hash: Hash32) -> String {
    hash.to_string().chars().take(16).collect()
}

impl<B: Books, M: Modules, R: WormStore, X: WormStore> Driver<B, M, R, X> {
    async fn send(&self, key: String, caller: &str, command: FinCommand) -> Result<Result<(), Reject>, DriverError> {
        self.books
            .propose(FinEnvelope {
                key,
                caller: caller.to_string(),
                actor: None,
                roles: vec![],
                command,
            })
            .await
    }

    /// The published root of a date, verified. `Ok(None)` while it is not
    /// published; an error in words if what is published does not verify.
    async fn published(&self, date: NaiveDate) -> Result<Result<Option<Hash32>, String>, DriverError> {
        let Some(bytes) = self.roots.get(&root_name(date)).await? else {
            return Ok(Ok(None));
        };
        Ok(match serde_json::from_slice::<PublishedRoot>(&bytes) {
            Ok(p) if p.business_date == date && verify_published(&p) => Ok(Some(p.root)),
            _ => Err(format!("the Merkle root published for {date} does not verify")),
        })
    }

    /// Do everything that can be done now.
    pub async fn pass(&self) -> Result<Pass, DriverError> {
        let work: Work = decode("work", self.books.ask(json!({ "q": "work" })).await?)?;
        let mut pass = Pass::default();
        for date in work.roots_wanted {
            match self.published(date).await? {
                Ok(None) => pass.waiting.push(format!("the Merkle root of {date} is not published")),
                Err(problem) => pass.waiting.push(problem),
                Ok(Some(root)) => {
                    let command = FinCommand::RecordMerkleRoot {
                        date,
                        root,
                        reference: root_name(date),
                    };
                    match self.send(format!("root:{date}"), "feed", command).await? {
                        Ok(()) => pass.roots_recorded.push(date),
                        Err(reject) => pass.waiting.push(format!("the Merkle root of {date} was refused: {reject}")),
                    }
                }
            }
        }
        let mut earlier_open = false;
        for period in &work.closing {
            let closed = self.period(period, earlier_open, &mut pass).await?;
            earlier_open |= !closed;
        }
        Ok(pass)
    }

    /// One closing period. Returns whether it was closed.
    async fn period(&self, w: &PeriodWork, earlier_open: bool, pass: &mut Pass) -> Result<bool, DriverError> {
        let mut extracts = w.extracts.clone();
        if !w.mart_ready {
            pass.waiting
                .push(format!("{}: the data mart has not every partition's events to {}", w.period, w.end));
        } else {
            for kind in [IN_FORCE, MOVEMENT, EXPERIENCE] {
                if !extracts.iter().any(|k| k == kind) && self.extract(w, kind, pass).await? {
                    extracts.push(kind.to_string());
                }
            }
        }

        let due = |id: &str, kind: Option<&str>| {
            w.blockers.iter().any(|b| b == id) || w.reports_wanted.iter().any(|r| r == id) || kind.is_some_and(|k| w.aged_wanted.iter().any(|a| a == k))
        };
        let mut unanswered = Vec::new();
        for source in &self.sources {
            let id = source.reports.check_id();
            let kind = match &source.reports {
                Reports::Aged(kind) => Some(kind.as_str()),
                Reports::Check(_) => None,
            };
            if !due(&id, kind) {
                continue;
            }
            let answer = match self.modules.ask(&source.module, (source.question)(w.end)).await {
                Ok(answer) => answer,
                Err(e) => {
                    unanswered.push(format!("{} did not answer for {id}: {e}", source.module));
                    continue;
                }
            };
            let key = format!("report:{}:{id}:{}", w.period, short(sha256(&[answer.to_string().as_bytes()])));
            let command = match &source.reports {
                Reports::Check(check) => {
                    let a: CheckAnswer = decode(&id, answer)?;
                    FinCommand::ReportCheck {
                        period: w.period,
                        check: check.clone(),
                        outcome: match a.green {
                            true => Outcome::Green,
                            false => Outcome::Red {
                                items: a.items.iter().map(Value::to_string).collect(),
                            },
                        },
                        evidence: Some(format!("module {}", source.module)),
                    }
                }
                Reports::Aged(kind) => {
                    let a: AgedAnswer = decode(&id, answer)?;
                    FinCommand::ReportAgedItems {
                        kind: kind.clone(),
                        as_at: a.as_at,
                        items: a.items,
                        total_minor: a.total_minor,
                        count: a.count,
                    }
                }
            };
            match self.send(key, &source.caller, command).await? {
                Ok(()) => pass.reports_sent.push(id),
                Err(reject) => unanswered.push(format!("the ledger refused {id} from {}: {reject}", source.module)),
            }
        }

        // The driver's own check: every sub-ledger answered and the extracts
        // of the period are stored and registered.
        if due(SUBLEDGER_CHECK, None) || !unanswered.is_empty() {
            let mut items = unanswered.clone();
            for kind in [IN_FORCE, MOVEMENT, EXPERIENCE] {
                if !extracts.iter().any(|k| k == kind) {
                    items.push(format!("the {kind} extract as at {} is not registered", w.end));
                }
            }
            let outcome = match items.is_empty() {
                true => Outcome::Green,
                false => Outcome::Red { items },
            };
            let key = format!("report:{}:{SUBLEDGER_CHECK}:{}", w.period, short(sha256(&[format!("{outcome:?}").as_bytes()])));
            let command = FinCommand::ReportCheck {
                period: w.period,
                check: SUBLEDGER_CHECK.to_string(),
                outcome,
                evidence: Some(format!("{}/extracts/{}", self.location, w.end)),
            };
            match self.send(key, CLOSE_WORKFLOW, command).await? {
                Ok(()) => pass.reports_sent.push(SUBLEDGER_CHECK.to_string()),
                Err(reject) => pass.waiting.push(format!("{}: {SUBLEDGER_CHECK} was refused: {reject}", w.period)),
            }
        }
        pass.waiting.extend(unanswered.into_iter().map(|u| format!("{}: {u}", w.period)));

        // Close what was already clear when the pass began. What this pass
        // turned green is closed by the next one, on the ledger's own word.
        if earlier_open || !w.blockers.is_empty() || !w.sign_offs_needed.is_empty() {
            return Ok(false);
        }
        match self
            .send(format!("close:{}", w.period), CLOSE_WORKFLOW, FinCommand::ClosePeriod { period: w.period })
            .await?
        {
            Ok(()) => {
                pass.closed.push(w.period);
                Ok(true)
            }
            Err(reject) => {
                pass.waiting.push(format!("{} was not closed: {reject}", w.period));
                Ok(false)
            }
        }
    }

    /// Read one extract from the ledger, store it and register it.
    async fn extract(&self, w: &PeriodWork, kind: &str, pass: &mut Pass) -> Result<bool, DriverError> {
        let root = match self.published(w.end).await? {
            Ok(Some(root)) => root,
            Ok(None) => {
                pass.waiting.push(format!("{}: the Merkle root of {} is not published", w.period, w.end));
                return Ok(false);
            }
            Err(problem) => {
                pass.waiting.push(format!("{}: {problem}", w.period));
                return Ok(false);
            }
        };
        let mut lines: Vec<String> = Vec::new();
        let first: ExtractPage = loop {
            let question = json!({ "q": "extract", "kind": kind, "period": w.period, "offset": lines.len() });
            let page: ExtractPage = decode("extract", self.books.ask(question).await?)?;
            if page.lines.is_empty() && (lines.len() as u64) < page.total_lines {
                return Err(DriverError::Answer(format!("the {kind} extract stopped at line {}", lines.len())));
            }
            lines.extend(page.lines.iter().cloned());
            if lines.len() as u64 >= page.total_lines {
                break page;
            }
        };
        let mut bytes = lines.join("\n").into_bytes();
        bytes.push(b'\n');
        // The pages are read one question at a time. The hash of the whole
        // proves they are pages of one file.
        if sha256(&[&bytes]) != first.content_hash {
            pass.waiting.push(format!("{}: the {kind} extract changed while it was read", w.period));
            return Ok(false);
        }
        match self.extracts.put_new(&first.name, "text/csv", bytes.clone()).await {
            Ok(()) => {}
            Err(AzureError::AlreadyExists(_)) => {
                // Written by an earlier pass that stopped before registering it.
                if self.extracts.get(&first.name).await? != Some(bytes) {
                    pass.waiting.push(format!("{}: {} is already stored with other content", w.period, first.name));
                    return Ok(false);
                }
            }
            Err(e) => return Err(e.into()),
        }
        let extract = ExtractRecord {
            id: first.id.clone(),
            kind: kind.to_string(),
            as_at: first.as_at,
            content_hash: first.content_hash,
            rows: first.rows,
            merkle_root: root,
            definition_version: DEFINITION_VERSION,
            location: format!("{}/{}", self.location, first.name),
            control: first.control,
            reconciled: first.reconciled,
        };
        match self
            .send(format!("extract:{}", first.id), "reporting", FinCommand::RegisterExtract { extract })
            .await?
        {
            Ok(()) => {
                pass.extracts_registered.push(first.id);
                Ok(true)
            }
            Err(reject) => {
                pass.waiting.push(format!("{}: extract {} was refused: {reject}", w.period, first.id));
                Ok(false)
            }
        }
    }
}
