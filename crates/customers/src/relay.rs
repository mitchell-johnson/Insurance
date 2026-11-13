//! Telling claims of deaths. Claims stops every benefit in payment to a
//! person the day after their death, on whichever core partition each claim
//! sits, so every death recorded here goes to every partition as
//! `RecordDeath` from caller `customer-master`. A partition with no claim
//! for the person accepts it and does nothing; the same death again is a
//! no-op, so the relay is at least once. A death withdrawn here goes the
//! same way as `RetractDeath`, for the date that was recorded.

use std::future::Future;
use std::time::Duration;

use chrono::NaiveDate;
use ic_raft::{ClusterAuth, ClusterClient};

use crate::model::PartyId;
use crate::store::DeathNotice;

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

/// The channels directory, which decides who sees which policy in the app.
pub trait Directory: Send + Sync {
    /// Propose a directory command. `Ok(None)` is accepted, `Ok(Some(why))`
    /// refused, `Err` not reached.
    fn direct(&self, key: &str, command: &ic_channels::directory::DirectoryCommand) -> impl Future<Output = Result<Option<String>, String>> + Send;
}

/// What the directory needs to hear about a relationship change, if
/// anything: policy ownership, an assignee coming or going, a child with the
/// date they control their own health information.
pub fn directory_command(party: &PartyId, relationship: &crate::model::Relationship, ended: bool) -> Option<ic_channels::directory::DirectoryCommand> {
    use crate::model::{RelationshipKind, Target};
    use ic_channels::directory::DirectoryCommand;
    let Target::Policy(policy) = &relationship.target else {
        return None;
    };
    let policy = ic_core::PolicyId(policy.clone());
    let who = ic_core::PartyId(party.0.clone());
    match (relationship.kind, ended) {
        (RelationshipKind::Owner, false) => Some(DirectoryCommand::RecordOwnership { policy, policyholder: who }),
        (RelationshipKind::Assignee, _) => Some(DirectoryCommand::RecordAssignment {
            policy,
            party: who,
            active: !ended,
        }),
        (RelationshipKind::Dependant, false) => relationship
            .attributes
            .get("controls_from")
            .and_then(|v| crate::validate::date(v).ok())
            .map(|controls_from| DirectoryCommand::RecordDependant {
                policy,
                party: who,
                controls_from,
            }),
        _ => None,
    }
}

/// The claims command for a death.
pub fn record_death(party: &PartyId, date_of_death: NaiveDate) -> ic_core::Command {
    ic_core::Command::Claims(Box::new(ic_claims::Command::RecordDeath {
        party: party.0.clone(),
        date_of_death,
    }))
}

/// The claims command withdrawing a death recorded in error, for the date
/// that was recorded.
pub fn retract_death(party: &PartyId, date_of_death: NaiveDate) -> ic_core::Command {
    ic_core::Command::Claims(Box::new(ic_claims::Command::RetractDeath {
        party: party.0.clone(),
        date_of_death,
    }))
}

/// The idempotency key of one attempt at a death on one partition. The
/// core remembers a refusal under its key, so each attempt has its own;
/// claims treats the same death again as nothing new, so a second attempt
/// after an answer that was lost does no harm.
pub fn key(party: &PartyId, date_of_death: NaiveDate, retract: bool, partition: u16, attempt: i32) -> String {
    let what = if retract { "retraction" } else { "death" };
    format!("{what}:{party}:{date_of_death}:{partition}:{attempt}")
}

/// Why a death was not accepted everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A partition answered no. Others may still take their deaths.
    Refused(String),
    /// The cluster did not answer. Nothing more is tried this pass.
    Unreached(String),
}

/// Send one death, or its retraction, to each of `partitions`.
pub async fn send<C: Core>(core: &C, partitions: u16, notice: &DeathNotice) -> Result<(), Failure> {
    let DeathNotice {
        party,
        date_of_death,
        retract,
        attempts,
    } = notice;
    let command = if *retract {
        retract_death(party, *date_of_death)
    } else {
        record_death(party, *date_of_death)
    };
    for partition in 0..partitions {
        match core
            .propose(partition, &key(party, *date_of_death, *retract, partition, *attempts), &command)
            .await
        {
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
    /// The partition count once known; 0 until then (a cluster has at least one).
    partitions: std::sync::atomic::AtomicU16,
}

impl ClusterCore {
    /// `nodes` as `IC_CLUSTER_NODES` gives them. Nothing is contacted until
    /// the first death, so the customer master starts without the cluster.
    pub fn new(nodes: &str, auth: ClusterAuth, patience: Duration) -> Result<Self, String> {
        Ok(Self {
            client: ClusterClient::from_list(nodes)?.with_auth(auth),
            patience,
            partitions: std::sync::atomic::AtomicU16::new(0),
        })
    }
}

impl Directory for ClusterCore {
    async fn direct(&self, key: &str, command: &ic_channels::directory::DirectoryCommand) -> Result<Option<String>, String> {
        let envelope = ic_channels::Envelope {
            key: key.to_string(),
            at: chrono::Utc::now().timestamp(),
            actor: ic_channels::Actor::Module(CALLER.to_string()),
            command: ic_channels::Command::Directory(command.clone()),
        };
        let command = serde_json::to_value(&envelope).map_err(crate::text)?;
        let reply = self
            .client
            .propose_module_until(ic_channels::query::MODULE, &command, self.patience)
            .await
            .map_err(crate::text)?;
        // The module answers with its own verdict inside: a refusal there
        // is the directory saying no, not the cluster.
        let output: ic_channels::state::Output = match reply.result {
            Ok(v) => serde_json::from_value(v).map_err(|e| format!("the directory's answer was not understood: {e}"))?,
            Err(e) => return Ok(Some(e)),
        };
        Ok(output.result.err().map(|r| format!("{}: {}", r.code, r.message)))
    }
}

impl Core for ClusterCore {
    async fn partitions(&self) -> Result<u16, String> {
        let known = self.partitions.load(std::sync::atomic::Ordering::Relaxed);
        if known > 0 {
            return Ok(known);
        }
        let n = self.client.partitions(self.patience).await?;
        self.partitions.store(n, std::sync::atomic::Ordering::Relaxed);
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
        let reply = self.client.propose_until(partition, &envelope, self.patience).await.map_err(crate::text)?;
        Ok(reply.rejected.map(|r| r.code))
    }
}
