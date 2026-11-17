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

/// Commands about something that lives on one partition and names no claim:
/// a payment run, and an investigation case (the cases list says where).
const ON_A_PARTITION: [&str; 4] = ["prepare_run", "approve_run", "cancel_run", "close_investigation"];

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
    /// The underwriting loadings on one partition's covers on a date, a
    /// page of policies at a time (pass `next` back as `after`). They imply
    /// health: pass them on for the one purpose asked and keep nothing.
    fn loadings(&self, partition: PartitionId, after: Option<PolicyId>, as_at: chrono::NaiveDate) -> Asked<'_, ic_core::loadings::LoadingsPage>;
    /// A dry run of an application on one partition: the first period's
    /// bill, or the core's refusal as (code, message). `None` while the
    /// partition's business date is not open. Nothing is written.
    fn quote(&self, partition: PartitionId, application: ic_core::command::IssuePolicy) -> Asked<'_, Option<Result<Value, (String, String)>>>;
    /// A policy by its number as one partition's read side holds it, if it
    /// holds it: the policy, its directory entry and its billing.
    fn policy(&self, partition: PartitionId, number: String) -> Asked<'_, Option<Value>>;
    /// What the end-of-day run knows about one partition: its date, the
    /// last close, the policies still to visit and any reconciliation breaks.
    fn closing(&self, partition: PartitionId) -> Asked<'_, Value>;
    /// Every product and rate table version on one partition and whether it
    /// is in use today (`ic_core::catalog::RegisterRow`, as JSON).
    fn register(&self, partition: PartitionId) -> Asked<'_, Vec<Value>>;
}

/// The partition an application's group lives on, as the core places it.
pub fn partition_of(group: &ic_core::GroupId, count: PartitionId) -> PartitionId {
    ic_core::home_partition(group) % count.max(1)
}

/// The book's reconciliation from each partition's end-of-day view: every
/// break with its partition, and the in-transit accounts, which net to
/// nothing across the insurer unless a transfer is still moving. `clean`
/// only when there is no break anywhere.
pub fn reconciliation(views: &[Value]) -> Result<Value, String> {
    let mut breaks: Vec<Value> = views
        .iter()
        .flat_map(|v| {
            v["breaks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|b| json!({ "partition": v["partition"], "check": b["check"], "detail": b["detail"] }))
        })
        .collect();
    let mut in_transit: i128 = 0;
    for view in views {
        let amount = view["transfer_in_transit"].as_str().unwrap_or("0");
        in_transit += amount
            .parse::<i128>()
            .map_err(|e| format!("partition {} reported an unreadable in-transit total: {e}", view["partition"]))?;
    }
    if in_transit != 0 && !views.iter().any(|v| v["transfer_in_flight"] == true) {
        breaks.push(json!({ "check": "transfer_in_transit", "detail": format!("in-transit accounts net to {in_transit} with no transfer in flight") }));
    }
    let dates: Vec<&str> = views.iter().filter_map(|v| v["business_date"].as_str()).collect();
    let business_date = if dates.len() == views.len() { dates.into_iter().min() } else { None };
    let unfinished: usize = views.iter().map(|v| v["unfinished"].as_array().map_or(0, Vec::len)).sum();
    Ok(json!({ "business_date": business_date, "clean": breaks.is_empty(), "breaks": breaks, "unfinished": unfinished }))
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

    fn loadings(&self, partition: PartitionId, after: Option<PolicyId>, as_at: chrono::NaiveDate) -> Asked<'_, ic_core::loadings::LoadingsPage> {
        Box::pin(async move {
            self.client
                .loadings(partition, after, ic_core::loadings::MAX_PAGE, Some(as_at))
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn quote(&self, partition: PartitionId, application: ic_core::command::IssuePolicy) -> Asked<'_, Option<Result<Value, (String, String)>>> {
        Box::pin(async move {
            let answer = self.client.quote(partition, application).await.map_err(|e| e.to_string())?;
            Ok(answer.map(|r| r.map(|premium| json!(premium)).map_err(|r| (r.code, r.message))))
        })
    }

    fn policy(&self, partition: PartitionId, number: String) -> Asked<'_, Option<Value>> {
        Box::pin(async move {
            let found = self.client.policy_by_number(partition, &number).await.map_err(|e| e.to_string())?;
            Ok(found.map(|view| json!(view)))
        })
    }

    fn closing(&self, partition: PartitionId) -> Asked<'_, Value> {
        Box::pin(async move {
            let view = self.client.end_of_day_view(partition).await.map_err(|e| e.to_string())?;
            Ok(json!(view))
        })
    }

    fn register(&self, partition: PartitionId) -> Asked<'_, Vec<Value>> {
        Box::pin(async move {
            let rows = self.client.register(partition).await.map_err(|e| e.to_string())?;
            Ok(rows.iter().map(|r| json!(r)).collect())
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
            // Who is asking is filled in from the session, never from the page.
            staff: None,
        },
        "claim" => Question::Claim { claim: needs("claim")? },
        "work" => Question::Work {
            claim: needs("claim")?,
            staff: None,
        },
        "policy" => Question::PolicyStatus { policy: needs("policy")? },
        // Who is asking is filled in from the session, never from the page.
        "cases" => Question::Cases { staff: String::new() },
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
    /// A partition the person named: payment runs and investigation cases.
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
    let target = match (ON_A_PARTITION.contains(&name.as_str()), name == "notify") {
        (true, _) => Target::Partition(partition.ok_or_else(|| format!("{name} is on one partition: name it"))?),
        (false, true) => Target::Policy(text("policy").ok_or("a notification names its policy")?),
        (false, false) => Target::Claim(text("claim").ok_or_else(|| format!("{name} names no claim"))?),
    };
    // The core checks the caller again: a command not open to `claims` is refused there.
    let typed: ClaimsCommand = serde_json::from_value(command).map_err(|e| format!("not a claims command: {e}"))?;
    Ok((name, target, Command::Claims(Box::new(typed))))
}

/// A claim's payee is a payee the payments module holds, never references
/// an assessor types: `fields` are a `verify_payee` command's, whose
/// `account_ref` names the payee there, and `payee` is that module's answer
/// for it (null when there is none). The payee must have passed the bank's
/// name check, been confirmed by a second payee maintainer and screened
/// clear; who it is paid to, and the name check and screening relied on,
/// are filled in from that record. An estate is paid only to an estate's
/// account and a provider only to a provider's. Legal's approval of an
/// estate's account and the cooling-off on new details are payments' to
/// hold, and they hold the payment, not the verification.
pub fn payee_from_register(fields: &mut serde_json::Map<String, Value>, payee: &Value) -> Result<(), String> {
    let id = fields.get("account_ref").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    if id.is_empty() {
        return Err("pick the payee from the payments register: the payments team registers a payee, and a second person confirms it".to_string());
    }
    let tell = |why: &str| format!("payee {id} {why}, so a claim cannot be paid to it yet");
    if payee.is_null() {
        return Err(format!(
            "there is no payee {id} in payments: ask a payee maintainer there to register it, and a second to confirm it"
        ));
    }
    if payee["closed"] == true {
        return Err(tell("is closed"));
    }
    if payee["name_check"] == "no_match" {
        return Err(tell("failed the bank's name check"));
    }
    if payee["confirmed_by"].as_str().is_none() {
        return Err(tell("has not been confirmed by a second payee maintainer"));
    }
    if payee["screened_clear"] != true {
        return Err(tell("has not been screened clear"));
    }
    let theirs = payee["kind"].as_str().unwrap_or_default();
    let ours = fields.get("kind").and_then(Value::as_str).unwrap_or_default();
    let fits = match (ours, theirs) {
        ("estate", t) => t == "estate",
        ("provider", t) => t == "provider",
        (_, t) => t == "customer" || t == "beneficiary",
    };
    if !fits {
        return Err(format!(
            "payee {id} is held in payments as a {} account, which does not pay a claim's {ours}",
            theirs.replace('_', " ")
        ));
    }
    let party = payee["party"].as_str().unwrap_or_default();
    let check = payee["name_check"].as_str().unwrap_or_default();
    let confirmed = payee["confirmed_by"].as_str().unwrap_or_default();
    fields.insert("account_ref".to_string(), json!(id));
    fields.insert("party".to_string(), json!(party));
    fields.insert(
        "name_check_ref".to_string(),
        json!(format!("payments:{id}:name_check:{check}:confirmed_by:{confirmed}")),
    );
    fields.insert("screening_ref".to_string(), json!(format!("payments:{id}:screened_clear")));
    Ok(())
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
