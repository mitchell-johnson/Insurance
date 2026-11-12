//! Telling claims of deaths. Claims stops every benefit in payment to a
//! person the day after their death, on whichever core partition each claim
//! sits, so every death recorded here goes to every partition as
//! `RecordDeath` from caller `customer-master`. A partition with no claim
//! for the person accepts it and does nothing; the same death again is a
//! no-op, so the relay is at least once.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use chrono::NaiveDate;
use ic_raft::{ClusterAuth, ClusterClient};

use crate::model::PartyId;

/// The caller name claims accepts `RecordDeath` from.
pub const CALLER: &str = "customer-master";

/// The core, as far as the relay needs it.
pub trait Core: Send + Sync {
    /// How many partitions the core has.
    fn partitions(&self) -> impl Future<Output = Result<u16, String>> + Send;
    /// Propose a command to one partition. `Ok(None)` is accepted,
    /// `Ok(Some(code))` refused, `Err` not reached.
    fn propose(&self, partition: u16, key: &str, command: &ic_core::Command) -> impl Future<Output = Result<Option<String>, String>> + Send;
}

/// The claims command for a death.
pub fn record_death(party: &PartyId, date_of_death: NaiveDate) -> ic_core::Command {
    ic_core::Command::Claims(Box::new(ic_claims::Command::RecordDeath {
        party: party.0.clone(),
        date_of_death,
    }))
}

/// The idempotency key of one attempt at a death on one partition. The
/// core remembers a refusal under its key, so each attempt has its own;
/// claims treats the same death again as nothing new, so a second attempt
/// after an answer that was lost does no harm.
pub fn key(party: &PartyId, date_of_death: NaiveDate, partition: u16, attempt: i32) -> String {
    format!("death:{party}:{date_of_death}:{partition}:{attempt}")
}

/// Why a death was not accepted everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A partition answered no. Others may still take their deaths.
    Refused(String),
    /// The cluster did not answer. Nothing more is tried this pass.
    Unreached(String),
}

/// Send one death to each of `partitions`.
pub async fn send<C: Core>(core: &C, partitions: u16, party: &PartyId, date_of_death: NaiveDate, attempt: i32) -> Result<(), Failure> {
    let command = record_death(party, date_of_death);
    for partition in 0..partitions {
        match core.propose(partition, &key(party, date_of_death, partition, attempt), &command).await {
            Ok(None) => {}
            Ok(Some(code)) => return Err(Failure::Refused(format!("partition {partition} refused: {code}"))),
            Err(e) => return Err(Failure::Unreached(format!("partition {partition}: {e}"))),
        }
    }
    Ok(())
}

/// The core's partitions on the ledger cluster.
pub struct ClusterCore {
    client: ClusterClient,
    patience: Duration,
    partitions: Mutex<Option<u16>>,
}

impl ClusterCore {
    /// `nodes` as `IC_CLUSTER_NODES` gives them. Nothing is contacted until
    /// the first death, so the customer master starts without the cluster.
    pub fn new(nodes: &str, auth: ClusterAuth, patience: Duration) -> Result<Self, String> {
        Ok(Self {
            client: ClusterClient::from_list(nodes)?.with_auth(auth),
            patience,
            partitions: Mutex::new(None),
        })
    }
}

impl Core for ClusterCore {
    async fn partitions(&self) -> Result<u16, String> {
        let known = *self.partitions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = known {
            return Ok(n);
        }
        let n = self.client.partitions(self.patience).await?;
        *self.partitions.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
        Ok(n)
    }

    async fn propose(&self, partition: u16, key: &str, command: &ic_core::Command) -> Result<Option<String>, String> {
        let envelope = ic_core::Envelope {
            key: key.to_string(),
            caller: CALLER.to_string(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command: command.clone(),
        };
        let reply = self
            .client
            .propose_until(partition, &envelope, self.patience)
            .await
            .map_err(|e| e.to_string())?;
        Ok(reply.rejected.map(|r| r.code))
    }
}
