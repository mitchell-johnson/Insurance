//! The finance ledger and what is built on it.
//!
//! The core ledger keeps every policy's money on 64 partitions. This crate
//! keeps the insurer's books: one consolidated ledger by account, statutory
//! fund, product and posting type, built only from the events the core
//! publishes, and checked against the core every business day.
//!
//! * [`chart`]: the chart of accounts, written in Starlark, mapping each
//!   account role of the core to a finance account.
//! * [`books`]: the finance ledger state machine. It consumes core events,
//!   posts one summarised journal per partition and business date, runs the
//!   daily control check, takes manual journals from a maker and a checker,
//!   and controls periods. A closed period never changes.
//! * [`close`]: the period close: declared checks, waivers, sign-off.
//! * [`report`]: trial balance, movements with prior-period items, and the
//!   close dashboard, as read models of the books.
//! * [`returns`]: regulatory returns worked out from closed periods with
//!   their lineage, restatements, and conduct measures.
//! * [`host`]: the ledger as a module of the deployed node: its questions
//!   and how it reads the core's events.
//! * [`tables`]: the validation suite an actuarial table must pass before
//!   it can be activated.
//!
//! Like the core, the state machines here never read a clock, randomness or
//! the network. Everything arrives as a logged command.

pub mod books;
pub mod chart;
pub mod close;
pub mod config;
pub mod host;
pub mod period;
pub mod report;
pub mod returns;
pub mod tables;

pub use books::{FinApplied, FinCommand, FinEnvelope, FinEvent, FinanceState};
pub use chart::Chart;
pub use close::CloseConfig;
pub use ic_core::Reject;
pub use period::PeriodId;

/// Serialise a map with structured keys as a list of pairs, because JSON
/// object keys must be strings.
pub(crate) mod pairs {
    use std::collections::BTreeMap;

    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(map: &BTreeMap<K, V>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K: DeserializeOwned + Ord, V: DeserializeOwned, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<K, V>, D::Error> {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}
