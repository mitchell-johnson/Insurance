//! The finance ledger as a module of the deployed node.
//!
//! The node keeps the ledger in its own replicated group and shows it every
//! partition's events once, in order. Each event becomes one `CoreEvent`
//! command. An event the books cannot take yet, because no chart is active
//! or a new role has not been placed, is held in the replicated state and
//! the feed resumes when the cause is fixed, so nothing the core published
//! can be dropped on the way to the books.
//!
//! Everything else reaches the ledger as a `FinEnvelope` proposed to the
//! module by its drivers: the finance console, the close workflow, the
//! Merkle root publisher and the other modules' month-end reports.

use chrono::NaiveDate;
use ic_core::event::EventRecord;
use ic_ledger::PartitionId;
use ic_raft::module::{hosted_reading, ModuleState};
use ic_raft::ModuleSpec;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::books::{FinCommand, FinEnvelope, FinanceState};
use crate::period::PeriodId;

/// The module's name: its directory on disk and its routes.
pub const MODULE: &str = "finance";

/// What can be asked of the ledger. All answers are read from one member's
/// state and change nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "q", rename_all = "snake_case", deny_unknown_fields)]
pub enum Question {
    /// Where the feed has reached and whether anything is held.
    Status,
    Dashboard {
        period: PeriodId,
        today: NaiveDate,
    },
    Checks {
        period: PeriodId,
    },
    TrialBalance {
        period: PeriodId,
        #[serde(default)]
        fund: Option<String>,
    },
    Movements {
        period: PeriodId,
    },
    /// The partition days behind a period's figures.
    Sources {
        period: PeriodId,
    },
    /// Backdated entries and reversals posted in a period.
    Backdated {
        period: PeriodId,
        #[serde(default)]
        origin: Option<PeriodId>,
    },
    Journal {
        id: String,
    },
    Conduct {
        period: PeriodId,
        #[serde(default)]
        product: Option<String>,
    },
    /// Every filed version of a return.
    Return {
        id: String,
    },
    /// One figure on a filed return with what is behind it.
    ReturnLine {
        id: String,
        #[serde(default)]
        version: Option<u32>,
        code: String,
    },
    /// What the close is waiting for from its drivers.
    Work,
    /// Configurations proposed and waiting for approval.
    Proposals,
    /// Money held in policy suspense, by policy, as an aged items report.
    AgedSuspense {
        as_at: NaiveDate,
    },
    /// The period's tax and levy accounts by posting type and product.
    TaxReconciliation {
        period: PeriodId,
    },
    /// How many policies are in each accounting group, and how many have none.
    AccountingGroups,
    /// One policy's accounting group, or null while it has none.
    AccountingGroup {
        partition: PartitionId,
        policy: String,
    },
    /// A proposed rate table priced against the book in force on a closed
    /// date, beside the version it replaces.
    TableDryRun {
        as_at: NaiveDate,
        request: Box<crate::tables::DryRunRequest>,
    },
    /// Part of an extract cut from the data mart for a period.
    Extract {
        kind: String,
        period: PeriodId,
        #[serde(default)]
        offset: u64,
        #[serde(default)]
        limit: Option<u64>,
    },
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Answer one question from the state.
pub fn answer(state: &FinanceState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question the finance ledger answers: {e}"))?;
    match question {
        Question::Status => {
            let partitions: Vec<Value> = (0..state.partitions())
                .map(|p: PartitionId| {
                    json!({
                        "partition": p,
                        "next_event": state.next_event_seq(p),
                        "held": state.held_events(p),
                        "stalled": state.stalled.get(&p).map(|e| e.to_string()),
                    })
                })
                .collect();
            Ok(json!({
                "configured": state.partitions() > 0,
                "chart": state.chart.as_ref().map(|c| c.version),
                "close_config": state.close_config.as_ref().map(|c| c.version),
                "last_date": state.last_date(),
                "entries": state.ledger.entry_count(),
                "partitions": partitions,
                "state_hash": state.state_hash(),
            }))
        }
        Question::Dashboard { period, today } => encode(&state.dashboard(period, today)),
        Question::Checks { period } => encode(&state.checks(period)),
        Question::TrialBalance { period, fund } => encode(&state.trial_balance(period, fund.as_deref())),
        Question::Movements { period } => encode(&state.movements(period)),
        Question::Sources { period } => encode(&state.sources(period)),
        Question::Backdated { period, origin } => encode(&state.backdated_in(period, origin)),
        Question::Journal { id } => state.journals.get(&id).map_or_else(|| Err(format!("there is no journal {id}")), encode),
        Question::Conduct { period, product } => encode(&state.conduct_report(period, product.as_deref())),
        Question::Work => encode(&state.work()),
        Question::Proposals => Ok(state.pending_proposals()),
        Question::AgedSuspense { as_at } => Ok(state.aged_suspense(as_at)),
        Question::TaxReconciliation { period } => encode(&state.tax_reconciliation(period)),
        Question::AccountingGroups => Ok(state.accounting_group_counts()),
        Question::AccountingGroup { partition, policy } => encode(&state.accounting_groups.get(&(partition, ic_core::PolicyId::from(policy.as_str())))),
        Question::TableDryRun { as_at, request } => {
            let covers = request.covers.iter().cloned().collect();
            let book = state.mart.rated_covers(as_at, &covers).map_err(|e| e.to_string())?;
            encode(&crate::tables::dry_run(as_at, &request, &book))
        }
        Question::Extract { kind, period, offset, limit } => {
            encode(&state.extract(&kind, period, offset, limit.unwrap_or(crate::report::MAX_EXTRACT_LINES))?)
        }
        Question::ReturnLine { id, version, code } => encode(&state.line_lineage(&id, version, &code)?),
        Question::Return { id } => state.returns.get(&id).map_or_else(|| Err(format!("return {id} has not been filed")), encode),
    }
}

/// One of the core's events as the ledger's own command.
pub fn react(record: &EventRecord) -> Vec<FinEnvelope> {
    vec![FinEnvelope {
        key: format!("event:{}", record.id),
        caller: "feed".to_string(),
        actor: None,
        roles: vec![],
        command: FinCommand::CoreEvent {
            record: Box::new(record.clone()),
        },
    }]
}

fn make() -> Box<dyn ModuleState> {
    hosted_reading(FinanceState::new(), answer, react)
}

/// The finance ledger, for a ledger node to host.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events())
}
