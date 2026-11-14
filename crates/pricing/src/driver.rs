//! The driver: everything the module does with the outside world, as a
//! pass that can be run again at any time.
//!
//! Each pass
//!
//! 1. reads the administrator's valuation files from the inbound container
//!    (`valuations/<YYYY-MM-DD>/<file>`, the last [`LOOKBACK_DAYS`] days),
//!    reads each once its malware scan is clean, and proposes its
//!    valuations, or records the file as refused with the reason;
//! 2. asks every partition of the core for each fund's units on issue and
//!    proposes any figure that changed;
//! 3. writes each extract that is due to the outbound container and
//!    records it as written;
//! 4. raises an alert, once, for each price that is late, each price a
//!    partition refused and each valuation file that could not be read, as
//!    a log line with the target [`ALERT_TARGET`] that Azure Monitor alerts on.
//!
//! Every proposal carries a key derived from what it is about, so a pass
//! repeated after a crash proposes nothing new. Everything the driver needs
//! to carry on is in the module's replicated state or in storage; the
//! in-memory caches only save work.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Mutex;

use chrono::{DateTime, Days, NaiveDate, Utc};
use ic_ledger::PartitionId;
use ic_money::{Currency, FundId};
use serde_json::{json, Value};

use crate::command::{Command, Envelope, Output, CALLER_INTAKE};
use crate::extract;
use crate::state::Extract;
use crate::valuation;

/// How many days of inbound folders each pass looks at.
pub const LOOKBACK_DAYS: u64 = 7;
/// The log target of an alert line.
pub const ALERT_TARGET: &str = "unit_pricing.alert";
/// The driver's name as the actor of its commands.
pub const ACTOR: &str = "unit-pricing-driver";

/// The module's replicated group.
pub trait Module: Send + Sync {
    fn propose(&self, envelope: &Envelope) -> impl Future<Output = Result<Output, String>> + Send;
    fn ask(&self, question: &Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// The core's partitions.
pub trait Core: Send + Sync {
    fn partitions(&self) -> PartitionId;
    /// A fund's units on issue on one partition and the latest pricing date
    /// fully executed there.
    fn units_on_issue(&self, partition: PartitionId, fund: &FundId) -> impl Future<Output = Result<(i64, Option<NaiveDate>), String>> + Send;
}

/// What a malware scan says of an inbound file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    Pending,
    Clean,
    Malicious,
}

/// The inbound container the administrator writes to.
pub trait Inbox: Send + Sync {
    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    fn scan(&self, name: &str) -> impl Future<Output = Result<Scan, String>> + Send;
    fn read(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}

/// The outbound container the administrator reads from. Write-once: a name
/// already written is success, since the extract it holds never changes.
pub trait Outbound: Send + Sync {
    fn put_new(&self, name: &str, body: Vec<u8>) -> impl Future<Output = Result<(), String>> + Send;
}

/// What one pass did, for the log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PassReport {
    pub files_taken: usize,
    pub files_refused: usize,
    pub files_waiting: usize,
    pub units_reported: usize,
    pub extracts_written: usize,
    pub alerts_raised: usize,
    pub errors: Vec<String>,
}

pub struct Driver<M, C, I, O> {
    module: M,
    core: C,
    inbox: I,
    outbound: O,
    /// Files already proposed. A restart forgets them; the keys make the
    /// second proposal harmless.
    taken: Mutex<BTreeSet<String>>,
    /// The last units figure proposed for each fund and partition.
    reported: Mutex<BTreeMap<(FundId, PartitionId), (i64, Option<NaiveDate>)>>,
    /// Alerts already raised by this process, by their stable ID.
    alerted: Mutex<BTreeSet<String>>,
}

fn envelope(key: String, at: DateTime<Utc>, command: Command) -> Envelope {
    Envelope {
        key,
        caller: CALLER_INTAKE.to_string(),
        actor: ACTOR.to_string(),
        roles: Vec::new(),
        at,
        command,
    }
}

fn refused(output: &Output) -> Option<String> {
    match output {
        Output::Refused { code, message } => Some(format!("{code}: {message}")),
        _ => None,
    }
}

impl<M: Module, C: Core, I: Inbox, O: Outbound> Driver<M, C, I, O> {
    pub fn new(module: M, core: C, inbox: I, outbound: O) -> Self {
        Self {
            module,
            core,
            inbox,
            outbound,
            taken: Mutex::new(BTreeSet::new()),
            reported: Mutex::new(BTreeMap::new()),
            alerted: Mutex::new(BTreeSet::new()),
        }
    }

    /// Record the ledger's partition count. Run once at start. Files
    /// refused before this start were alerted by an earlier run and are not
    /// raised again; late and refused prices still stand, so they are.
    pub async fn start(&self, at: DateTime<Utc>) -> Result<(), String> {
        let count = self.core.partitions();
        let out = self
            .module
            .propose(&envelope(format!("partitions:{count}"), at, Command::RecordPartitions { count }))
            .await?;
        if let Some(why) = refused(&out) {
            return Err(why);
        }
        let earlier = self.module.ask(&json!({"q": "alerts", "as_at": at.date_naive()})).await?;
        let mut alerted = self.alerted.lock().map_err(|e| e.to_string())?;
        for item in earlier.as_array().into_iter().flatten().filter(|i| i["kind"] == "file_refused") {
            alerted.insert(item["id"].as_str().unwrap_or_default().to_string());
        }
        Ok(())
    }

    /// One pass. A fault in one step is reported and the others still run.
    pub async fn pass(&self, at: DateTime<Utc>) -> PassReport {
        let mut report = PassReport::default();
        if let Err(e) = self.intake(at, &mut report).await {
            report.errors.push(format!("valuation intake: {e}"));
        }
        if let Err(e) = self.units(at, &mut report).await {
            report.errors.push(format!("units on issue: {e}"));
        }
        if let Err(e) = self.extracts(at, &mut report).await {
            report.errors.push(format!("extracts: {e}"));
        }
        if let Err(e) = self.alerts(at, &mut report).await {
            report.errors.push(format!("alerts: {e}"));
        }
        report
    }

    async fn alerts(&self, at: DateTime<Utc>, report: &mut PassReport) -> Result<(), String> {
        let items = self.module.ask(&json!({"q": "alerts", "as_at": at.date_naive()})).await?;
        let items = items.as_array().cloned().unwrap_or_default();
        let current: BTreeSet<String> = items.iter().filter_map(|i| i["id"].as_str().map(str::to_string)).collect();
        let mut alerted = self.alerted.lock().map_err(|e| e.to_string())?;
        for item in &items {
            let id = item["id"].as_str().unwrap_or_default();
            if !alerted.insert(id.to_string()) {
                continue;
            }
            let kind = item["kind"].as_str().unwrap_or_default();
            tracing::error!(target: ALERT_TARGET, kind, id, item = %item, "unit pricing needs a person");
            report.alerts_raised += 1;
        }
        // A cleared item may come back (a price refused again): raise it then.
        alerted.retain(|id| current.contains(id) || id.starts_with("file:"));
        Ok(())
    }

    async fn intake(&self, at: DateTime<Utc>, report: &mut PassReport) -> Result<(), String> {
        let today = at.date_naive();
        for back in 0..=LOOKBACK_DAYS {
            let day = today.checked_sub_days(Days::new(back)).unwrap_or(today);
            for name in self.inbox.list(&format!("valuations/{day}/")).await? {
                if self.taken.lock().map_err(|e| e.to_string())?.contains(&name) {
                    continue;
                }
                let command = match self.inbox.scan(&name).await? {
                    Scan::Pending => {
                        report.files_waiting += 1;
                        continue;
                    }
                    Scan::Malicious => Command::RefuseValuationFile {
                        file_id: name.clone(),
                        reason: "the malware scan found a threat; the file was not read".to_string(),
                    },
                    Scan::Clean => match self.inbox.read(&name).await? {
                        None => continue,
                        Some(bytes) => match valuation::parse(&bytes) {
                            Ok(file) => Command::ReceiveValuations {
                                administrator: file.administrator,
                                file_id: file.file_id,
                                valuations: file.valuations,
                            },
                            Err(reason) => Command::RefuseValuationFile { file_id: name.clone(), reason },
                        },
                    },
                };
                let refusing = matches!(command, Command::RefuseValuationFile { .. });
                let out = self.module.propose(&envelope(format!("valuation-file:{name}"), at, command)).await?;
                if let Some(why) = refused(&out) {
                    return Err(format!("{name}: {why}"));
                }
                if refusing {
                    report.files_refused += 1;
                } else {
                    report.files_taken += 1;
                }
                self.taken.lock().map_err(|e| e.to_string())?.insert(name);
            }
        }
        Ok(())
    }

    async fn units(&self, at: DateTime<Utc>, report: &mut PassReport) -> Result<(), String> {
        let funds = self.module.ask(&json!({"q": "funds"})).await?;
        let funds: Vec<FundId> = funds
            .as_array()
            .map(|rows| rows.iter().filter_map(|r| serde_json::from_value(r["fund"].clone()).ok()).collect())
            .unwrap_or_default();
        for fund in funds {
            for partition in 0..self.core.partitions() {
                let figure = self.core.units_on_issue(partition, &fund).await?;
                let key = (fund.clone(), partition);
                if self.reported.lock().map_err(|e| e.to_string())?.get(&key) == Some(&figure) {
                    continue;
                }
                let (units_micro, executed_through) = figure;
                let through = executed_through.map_or("none".to_string(), |d| d.to_string());
                let command = Command::RecordCoreUnits {
                    fund: fund.clone(),
                    partition,
                    executed_through,
                    units_micro,
                };
                let out = self
                    .module
                    .propose(&envelope(format!("units:{}:{partition}:{through}:{units_micro}", fund.0), at, command))
                    .await?;
                if let Some(why) = refused(&out) {
                    return Err(format!("{} on partition {partition}: {why}", fund.0));
                }
                report.units_reported += 1;
                self.reported.lock().map_err(|e| e.to_string())?.insert(key, figure);
            }
        }
        Ok(())
    }

    async fn extracts(&self, at: DateTime<Utc>, report: &mut PassReport) -> Result<(), String> {
        let due = self.module.ask(&json!({"q": "extracts_due", "limit": 5})).await?;
        let rows = due.as_array().cloned().unwrap_or_default();
        for row in rows {
            let x: Extract = serde_json::from_value(row["extract"].clone()).map_err(|e| format!("not an extract: {e}"))?;
            let currency: Currency = serde_json::from_value(row["currency"].clone()).map_err(|_| format!("{} has no fund currency", x.id))?;
            let name = extract::name(&x);
            let body = extract::render(&x, currency);
            let sha256 = extract::digest(&body);
            self.outbound.put_new(&name, body).await?;
            let command = Command::RecordExtractWritten {
                extract: x.id.clone(),
                name,
                sha256,
            };
            let out = self.module.propose(&envelope(format!("extract-written:{}", x.id), at, command)).await?;
            if let Some(why) = refused(&out) {
                return Err(format!("{}: {why}", x.id));
            }
            report.extracts_written += 1;
        }
        Ok(())
    }
}
