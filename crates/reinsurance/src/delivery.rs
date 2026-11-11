//! Carrying the module's instructions to the core.
//!
//! An instruction becomes one core command on one partition, sent as caller
//! `reinsurance` with the instruction's ID as its idempotency key, so a
//! retry after a timeout applies once. The translation is a pure function;
//! the driver that sends the result is outside the state machine.

use chrono::NaiveDate;
use ic_core::reinsurance::{CessionRecord, CessionSegment, FacultativeShare, ReinsurancePosting};
use ic_ledger::PartitionId;
use ic_money::Currency;

use crate::cession::Cession;
use crate::machine::{Instruction, InstructionBody, PostingKind, Reinsurance};
use crate::TreatyId;

/// The name the core knows this module by.
pub const CALLER: &str = "reinsurance";

/// One command for the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreDelivery {
    pub partition: PartitionId,
    pub key: String,
    pub caller: &'static str,
    pub command: ic_core::Command,
}

fn record(c: &Cession) -> CessionRecord {
    CessionRecord {
        treaty: c.treaty.as_ref().map(|t| t.id.to_string()),
        treaty_version: c.treaty.as_ref().map_or(0, |t| t.version),
        sum_at_risk_minor: c.sum_at_risk_minor,
        retained_minor: c.retained_minor,
        ceded: c.ceded.iter().map(|(r, a)| (r.to_string(), *a)).collect(),
        facultative: c
            .facultative
            .iter()
            .map(|f| FacultativeShare {
                reference: f.reference.clone(),
                reinsurer: f.reinsurer.to_string(),
                amount_minor: f.amount_minor,
            })
            .collect(),
        unplaced_minor: c.unplaced_minor,
    }
}

fn posting(kind: PostingKind) -> ReinsurancePosting {
    match kind {
        PostingKind::CededPremium => ReinsurancePosting::CededPremium,
        PostingKind::Commission => ReinsurancePosting::Commission,
        PostingKind::Recoverable => ReinsurancePosting::Recoverable,
        PostingKind::RecoveryDue => ReinsurancePosting::RecoveryDue,
        PostingKind::AggregateRecovery => ReinsurancePosting::AggregateRecovery,
        PostingKind::ProfitCommission => ReinsurancePosting::ProfitCommission,
        PostingKind::DisputeSettlement => ReinsurancePosting::DisputeSettlement,
        PostingKind::OpeningBalance => ReinsurancePosting::OpeningBalance,
    }
}

impl Reinsurance {
    /// The currency a treaty, or an aggregate treaty, settles in.
    pub fn treaty_currency(&self, treaty: &TreatyId) -> Option<Currency> {
        self.latest_treaty(treaty)
            .map(|t| t.currency)
            .or_else(|| self.aggregates.get(treaty).map(|a| a.currency))
    }

    /// The core command for an instruction. `on` is the business date, used
    /// as the value date of a settlement sweep. `None` only for a treaty the
    /// module does not know, which it cannot have issued an instruction for.
    pub fn to_core(&self, instruction: &Instruction, on: NaiveDate) -> Option<CoreDelivery> {
        let command = match &instruction.body {
            InstructionBody::RecordCession { key, from, segments } => ic_core::Command::RecordCession {
                policy: key.policy.clone(),
                cover: key.cover.clone(),
                layer: key.layer,
                life: key.life.clone(),
                from: *from,
                segments: segments
                    .iter()
                    .map(|(from, c)| CessionSegment {
                        from: *from,
                        cession: c.as_ref().map(record),
                    })
                    .collect(),
            },
            InstructionBody::Post {
                kind,
                treaty,
                reinsurer,
                policy,
                amount_minor,
                value_date,
                reference,
            } => ic_core::Command::PostReinsurance {
                kind: posting(*kind),
                treaty: treaty.to_string(),
                reinsurer: reinsurer.to_string(),
                currency: self.treaty_currency(treaty)?,
                policy: policy.clone(),
                amount_minor: *amount_minor,
                value_date: *value_date,
                reference: reference.clone(),
            },
            InstructionBody::Sweep {
                treaty,
                reinsurer,
                statement,
                amount_minor,
                ..
            } => ic_core::Command::PostReinsurance {
                kind: ReinsurancePosting::SettlementSweep,
                treaty: treaty.to_string(),
                reinsurer: reinsurer.to_string(),
                currency: self.treaty_currency(treaty)?,
                policy: None,
                amount_minor: *amount_minor,
                value_date: on,
                reference: format!("statement {statement}"),
            },
        };
        Some(CoreDelivery {
            partition: instruction.partition,
            key: instruction.id.clone(),
            caller: CALLER,
            command,
        })
    }
}
