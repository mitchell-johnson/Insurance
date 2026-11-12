//! What goes into the module's log, what it answers, and the events it
//! publishes.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use ic_ledger::PartitionId;
use ic_money::{DecStr, FundId};
use serde::{Deserialize, Serialize};

use crate::fund::FundDefinition;
use crate::strike::CheckKind;
use crate::valuation::Valuation;

/// The caller fund definitions arrive from, as the core's do.
pub const CALLER_CONFIG: &str = "config";
/// People act through the unit pricing console, which establishes who they
/// are and their roles.
pub const CALLER_CONSOLE: &str = "unit-pricing-console";
/// The driver: valuation files, the core's units on issue, partitions.
pub const CALLER_INTAKE: &str = "unit-pricing-intake";
/// The host's outbox carrier, reporting what the core made of a publication.
pub const CALLER_OUTBOX: &str = "core-outbox";
/// The host's event relay, carrying the core's unit events.
pub const CALLER_EVENTS: &str = "event-relay";

/// Prepares a price from a valuation.
pub const ROLE_PREPARER: &str = "unit_pricing_preparer";
/// Approves or rejects a prepared price. Never the person who prepared it.
pub const ROLE_APPROVER: &str = "unit_pricing_approver";
/// Suspends and resumes funds, raises and closes price errors, republishes.
pub const ROLE_LEAD: &str = "unit_pricing_lead";

/// One command in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A repeat is answered with the first result.
    pub key: String,
    pub caller: String,
    /// The person responsible, or the service for a service's command.
    pub actor: String,
    /// The actor's roles, as the console established them.
    #[serde(default)]
    pub roles: Vec<String>,
    /// The proposer's clock. Time moves only as commands carry it.
    pub at: DateTime<Utc>,
    pub command: Command,
}

/// What the core made of one partition's copy of a price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Applied,
    Refused { code: String, message: String },
}

/// A unit movement the core reported (from `UnitOrderExecuted` or `UnitAdjustment`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitMovement {
    /// The core's order or adjustment ID: movements are taken once each.
    pub id: String,
    pub partition: PartitionId,
    pub policy: String,
    pub fund: FundId,
    pub kind: MovementKind,
    /// Signed: positive is units to the policy.
    pub units_micro: i64,
    /// Signed: positive is cash into the fund.
    pub cash_minor: i64,
    /// The price it executed at, if any.
    #[serde(default)]
    pub price_id: Option<String>,
    /// The pricing date the movement counts on in holdings.
    pub pricing_date: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovementKind {
    /// An order that bought units.
    Buy,
    /// An order that sold units.
    Sell,
    /// A replay of an order at a corrected price.
    Correction,
    /// Units loaded at migration or another opening entry.
    Opening,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// A fund definition, as activated on the core.
    ActivateFund(Box<FundDefinition>),
    /// How many partitions the ledger has. Fixed once set.
    RecordPartitions {
        count: PartitionId,
    },
    /// The valuations of one administrator file.
    ReceiveValuations {
        administrator: String,
        file_id: String,
        valuations: Vec<Valuation>,
    },
    /// A file that could not be read, so the queue shows it.
    RefuseValuationFile {
        file_id: String,
        reason: String,
    },
    /// One partition's units on issue of a fund, from the core's
    /// `UnitsOnIssue` query: its unit position once every order up to
    /// `executed_through` has executed.
    RecordCoreUnits {
        fund: FundId,
        partition: PartitionId,
        #[serde(default)]
        executed_through: Option<NaiveDate>,
        units_micro: i64,
    },
    PreparePrice {
        fund: FundId,
        pricing_date: NaiveDate,
    },
    /// Approve a prepared price, accepting each held check with a reason.
    ApprovePrice {
        price: String,
        #[serde(default)]
        accepted: BTreeMap<CheckKind, String>,
    },
    RejectPrice {
        price: String,
        reason: String,
    },
    /// Stop activating prices for a fund from a pricing date.
    SuspendFund {
        fund: FundId,
        from: NaiveDate,
        reason: String,
    },
    /// Activate prices again from a pricing date.
    ResumeFund {
        fund: FundId,
        from: NaiveDate,
        reason: String,
    },
    /// Open a price error over active prices of one fund. The fund's prices
    /// are held until every one of them has a corrected price.
    RaisePriceError {
        error: String,
        prices: Vec<String>,
        cause: String,
        detected_on: NaiveDate,
    },
    CloseError {
        error: String,
        summary: String,
    },
    /// Offer the core again the partitions that refused a price, once what
    /// it refused for is fixed.
    Republish {
        price: String,
    },
    /// The outbox carrier's report on one publication.
    RecordDelivery {
        publication: String,
        delivery: Delivery,
    },
    /// A unit movement from the core's events.
    RecordMovement(Box<UnitMovement>),
    /// Every order of a fund up to a pricing date has executed on a
    /// partition, so its holdings for that date are complete.
    RecordExecuted {
        fund: FundId,
        partition: PartitionId,
        pricing_date: NaiveDate,
    },
    /// The holdings extract or net flow instruction was written for the administrator.
    RecordExtractWritten {
        extract: String,
        name: String,
        sha256: String,
    },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::ActivateFund(_) => "activate_fund",
            Command::RecordPartitions { .. } => "record_partitions",
            Command::ReceiveValuations { .. } => "receive_valuations",
            Command::RefuseValuationFile { .. } => "refuse_valuation_file",
            Command::RecordCoreUnits { .. } => "record_core_units",
            Command::PreparePrice { .. } => "prepare_price",
            Command::ApprovePrice { .. } => "approve_price",
            Command::RejectPrice { .. } => "reject_price",
            Command::SuspendFund { .. } => "suspend_fund",
            Command::ResumeFund { .. } => "resume_fund",
            Command::RaisePriceError { .. } => "raise_price_error",
            Command::CloseError { .. } => "close_error",
            Command::Republish { .. } => "republish",
            Command::RecordDelivery { .. } => "record_delivery",
            Command::RecordMovement(_) => "record_movement",
            Command::RecordExecuted { .. } => "record_executed",
            Command::RecordExtractWritten { .. } => "record_extract_written",
        }
    }

    /// The callers that may send this command.
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        match self {
            Command::ActivateFund(_) => &[CALLER_CONFIG],
            Command::RecordPartitions { .. }
            | Command::ReceiveValuations { .. }
            | Command::RefuseValuationFile { .. }
            | Command::RecordCoreUnits { .. }
            | Command::RecordExtractWritten { .. } => &[CALLER_INTAKE],
            Command::PreparePrice { .. }
            | Command::ApprovePrice { .. }
            | Command::RejectPrice { .. }
            | Command::SuspendFund { .. }
            | Command::ResumeFund { .. }
            | Command::RaisePriceError { .. }
            | Command::CloseError { .. }
            | Command::Republish { .. } => &[CALLER_CONSOLE],
            Command::RecordDelivery { .. } => &[CALLER_OUTBOX],
            Command::RecordMovement(_) | Command::RecordExecuted { .. } => &[CALLER_EVENTS],
        }
    }

    /// The role a person needs, for commands people send.
    pub fn role(&self) -> Option<&'static str> {
        match self {
            Command::PreparePrice { .. } => Some(ROLE_PREPARER),
            Command::ApprovePrice { .. } | Command::RejectPrice { .. } => Some(ROLE_APPROVER),
            Command::SuspendFund { .. }
            | Command::ResumeFund { .. }
            | Command::RaisePriceError { .. }
            | Command::CloseError { .. }
            | Command::Republish { .. } => Some(ROLE_LEAD),
            _ => None,
        }
    }
}

/// What a command did. A refusal changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    Done,
    /// A price was prepared, with its ID and where it stands.
    Prepared {
        price: String,
        status: crate::state::PriceStatus,
    },
    /// What became of each valuation in a file.
    Valuations {
        accepted: Vec<String>,
        refused: Vec<(String, String)>,
    },
    Refused {
        code: String,
        message: String,
    },
}

impl Output {
    pub fn refused(code: &str, message: impl Into<String>) -> Self {
        Output::Refused {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// What the module publishes for notifications, documents and the finance
/// ledger. No event carries personal data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    FundSuspended {
        fund: FundId,
        from: NaiveDate,
        reason: String,
    },
    FundResumed {
        fund: FundId,
        from: NaiveDate,
    },
    PriceActivated {
        price: String,
        fund: FundId,
        pricing_date: NaiveDate,
        version: u32,
        mid: DecStr,
        buy: DecStr,
        sell: DecStr,
    },
    PriceErrorRaised {
        error: String,
        fund: FundId,
        prices: Vec<String>,
    },
    /// A corrected price is active, or the error was found immaterial.
    PriceErrorCorrected {
        error: String,
        material: bool,
    },
    PriceErrorClosed {
        error: String,
    },
}

/// An event with its place in the module's stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub event: Event,
}
