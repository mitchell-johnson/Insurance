//! The claims console: how claims staff reach the claims they work.
//!
//! Claims live inside the core, on the partition of each claim's policy
//! (`docs/claims.md`). This service asks every partition the claims
//! module's questions and sends a person's claims command to the partition
//! that holds the claim, under the caller `claims`, with the person as the
//! command's actor and the purpose they gave. Who may decide what is the
//! claims module's own authority book, keyed by the same staff ID as the
//! workforce directory; this service only lets in people whose templates
//! give them the claims action, and never lets one person act as another.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use ic_core::claims_module::report::{Question, Reply};
use ic_core::claims_module::Command as ClaimsCommand;
use ic_core::{Command, Envelope, PartyId, PolicyId};
use ic_ledger::PartitionId;
use ic_raft::client::{fresh_salt, ClusterClient};
use serde_json::{json, Value};

use crate::staff::{At, StaffState};

/// The caller the core takes claims staff's commands from.
pub const CALLER: &str = "claims";

/// The staff action that opens the console.
pub mod actions {
    pub const WORK: &str = "claims.work";
}

/// Commands only other modules and the core's own services send.
const SERVICE_ONLY: [&str; 10] = [
    "activate_authority",
    "activate_rules",
    "acknowledge",
    "payment_returned",
    "confirm_run",
    "exclusion_control",
    "record_death",
    "reinsurance",
    "non_disclosure_outcome",
    "drain_due",
];

/// People's commands that name no actor in the claims module.
const NO_ACTOR: [&str; 3] = ["notify", "receive_evidence", "record_repayment"];

/// Commands about a payment run, which lives on one partition.
const RUNS: [&str; 3] = ["prepare_run", "approve_run", "cancel_run"];

/// Whether this person may open the console.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    json!({ "view": staff.holds_action(party, actions::WORK, at), "staff": party })
}

type Asked<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// What the core said to one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    pub duplicate: bool,
    pub events: usize,
    /// The refusal's code and message.
    pub rejected: Option<(String, String)>,
}

/// The core's partitions, as far as the console needs them.
pub trait Partitions: Send + Sync {
    fn count(&self) -> PartitionId;
    /// `None` while the partition's business date is not open.
    fn ask(&self, partition: PartitionId, question: Question) -> Asked<'_, Option<Reply>>;
    /// Whether this partition's directory holds the policy, and where it lives.
    fn locate(&self, partition: PartitionId, policy: PolicyId) -> Asked<'_, Option<PartitionId>>;
    fn propose(&self, partition: PartitionId, envelope: Envelope) -> Asked<'_, Proposed>;
}

/// The core on the ledger nodes.
pub struct Hosted {
    pub client: ClusterClient,
    pub partitions: PartitionId,
    pub patience: Duration,
}

impl Partitions for Hosted {
    fn count(&self) -> PartitionId {
        self.partitions
    }

    fn ask(&self, partition: PartitionId, question: Question) -> Asked<'_, Option<Reply>> {
        Box::pin(async move { self.client.claims(partition, &question).await.map_err(|e| e.to_string()) })
    }

    fn locate(&self, partition: PartitionId, policy: PolicyId) -> Asked<'_, Option<PartitionId>> {
        Box::pin(async move {
            let entry = self.client.locate(partition, &policy).await.map_err(|e| e.to_string())?;
            Ok(entry.map(|e| e.partition))
        })
    }

    fn propose(&self, partition: PartitionId, envelope: Envelope) -> Asked<'_, Proposed> {
        Box::pin(async move {
            let reply = self
                .client
                .propose_until(partition, &envelope, self.patience)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Proposed {
                duplicate: reply.duplicate,
                events: reply.events.len(),
                rejected: reply.rejected.map(|r| (r.code, r.message)),
            })
        })
    }
}

/// A question to the claims module, from its name and the page's fields.
/// `partition` is where a remittance is asked; everything else is asked of
/// every partition.
pub fn question(name: &str, field: impl Fn(&str) -> Option<String>) -> Result<Question, String> {
    let needs = |f: &str| field(f).ok_or_else(|| format!("{name} needs {f}"));
    Ok(match name {
        "queue" => Question::Queue {
            queue: serde_json::from_value(json!(needs("queue")?)).map_err(|_| "there is no such queue".to_string())?,
        },
        "claim" => Question::Claim { claim: needs("claim")? },
        "work" => Question::Work { claim: needs("claim")? },
        "book" => Question::Book,
        "report" => Question::Report,
        "remittance" => Question::Remittance { run: needs("run")? },
        other => return Err(format!("{other} is not a question the console asks")),
    })
}

/// Where a person's command goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The partition holding this claim.
    Claim(String),
    /// The partition holding this policy.
    Policy(String),
    /// A partition the person named: payment runs.
    Partition(PartitionId),
}

/// A person's command, made theirs: every actor is the person with the
/// purpose they gave, whatever the page sent. Returns the command's name,
/// where it goes and the core command.
pub fn command(staff: &PartyId, purpose: &str, partition: Option<PartitionId>, mut command: Value) -> Result<(String, Target, Command), String> {
    let name = match &command {
        Value::Object(fields) if fields.len() == 1 => fields.keys().next().cloned(),
        _ => None,
    }
    .ok_or("a command is one name with its fields")?;
    if SERVICE_ONLY.contains(&name.as_str()) {
        return Err(format!("{name} is not sent by a person"));
    }
    let fields = command
        .get_mut(&name)
        .and_then(Value::as_object_mut)
        .ok_or("a command's fields are an object")?;
    if !NO_ACTOR.contains(&name.as_str()) {
        if purpose.trim().is_empty() {
            return Err("say why you are doing this: the purpose is recorded with it".to_string());
        }
        fields.insert("actor".to_string(), json!({ "staff": staff, "purpose": purpose.trim() }));
    }
    // A notification opens the claim, so it goes where its policy lives.
    let text = |field: &str| fields.get(field).and_then(Value::as_str).map(str::to_string);
    let target = match (RUNS.contains(&name.as_str()), name == "notify") {
        (true, _) => Target::Partition(partition.ok_or("a payment run is on one partition: name it")?),
        (false, true) => Target::Policy(text("policy").ok_or("a notification names its policy")?),
        (false, false) => Target::Claim(text("claim").ok_or_else(|| format!("{name} names no claim"))?),
    };
    // The core checks the caller again: a command not open to `claims` is refused there.
    let typed: ClaimsCommand = serde_json::from_value(command).map_err(|e| format!("not a claims command: {e}"))?;
    Ok((name, target, Command::Claims(Box::new(typed))))
}

/// The envelope for one person's command. The key is theirs, so a retry
/// after a timeout applies once.
pub fn envelope(key: String, command: Command) -> Envelope {
    Envelope {
        key,
        caller: CALLER.to_string(),
        salt: fresh_salt(),
        traceparent: None,
        command,
    }
}
