//! The driver's ports as they are in production: the module and the core
//! on the cluster, reinsurance in its own replicated group, the event
//! stream on Azure Event Hubs, and quarantine storage on Azure Blob Storage
//! with Microsoft Defender for Storage scanning what lands there.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use ic_azure::blob::{BlobContainer, WormStore, MALWARE_SCAN_CLEAN, MALWARE_SCAN_TAG};
use ic_azure::eventhubs::EventHubsSink;
use ic_azure::AzureError;
use ic_core::ids::home_partition;
use ic_core::{Command as CoreCommand, GroupId};
use ic_raft::ClusterClient;
use serde_json::{json, Value};

use crate::command::{Envelope, ExcessDecision};
use crate::driver::{Answers, Core, DriverError, ExcessAnswer, FileStore, Module, Peers, Scan};
use crate::event::{GroupEvent, IssueOutcome, ReinsuranceNotice, UnderwritingRequest};
use crate::host::{Question, MODULE};
use crate::state::Outcome;

/// The group module in its replicated group on the cluster.
#[derive(Clone)]
pub struct ClusterModule {
    client: ClusterClient,
    patience: Duration,
}

impl ClusterModule {
    /// `patience` is how long one command keeps looking for a leader.
    pub fn new(client: ClusterClient, patience: Duration) -> Self {
        Self { client, patience }
    }
}

impl Module for ClusterModule {
    async fn propose(&self, envelope: Envelope) -> Result<Outcome, DriverError> {
        let command = serde_json::to_value(&envelope).map_err(|e| DriverError::Answer(e.to_string()))?;
        let reply = self
            .client
            .propose_module_until(MODULE, &command, self.patience)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        serde_json::from_value(reply.result.map_err(DriverError::Answer)?).map_err(|e| DriverError::Answer(e.to_string()))
    }

    async fn ask(&self, question: Question) -> Result<Value, DriverError> {
        let question = serde_json::to_value(&question).map_err(|e| DriverError::Answer(e.to_string()))?;
        self.client
            .query_module(MODULE, &question)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?
            .map_err(DriverError::Answer)
    }
}

/// The core's partitions on the cluster.
#[derive(Clone)]
pub struct ClusterCore {
    client: ClusterClient,
    partitions: u16,
    patience: Duration,
}

impl ClusterCore {
    pub fn new(client: ClusterClient, partitions: u16, patience: Duration) -> Self {
        Self {
            client,
            partitions: partitions.max(1),
            patience,
        }
    }

    async fn propose(&self, partition: u16, key: &str, command: &CoreCommand) -> Result<Option<String>, String> {
        let envelope = ic_core::Envelope {
            key: key.to_string(),
            caller: crate::command::caller::STAFF.to_string(),
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

/// Send a command to a group's home partition and, if its policies were
/// moved, to whichever partition holds them now. `propose` gives the
/// core's refusal code, or `None` when it accepted.
pub async fn follow_transfer<F, Fut>(home: u16, partitions: u16, mut propose: F) -> Result<Option<String>, String>
where
    F: FnMut(u16) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, String>>,
{
    let answer = propose(home).await?;
    if answer.as_deref() != Some("policy_transferred") {
        return Ok(answer);
    }
    for partition in (0..partitions).filter(|p| *p != home) {
        let elsewhere = propose(partition).await?;
        if !matches!(elsewhere.as_deref(), Some("unknown_policy" | "policy_transferred")) {
            return Ok(elsewhere);
        }
    }
    Ok(answer)
}

impl Core for ClusterCore {
    async fn submit(&self, key: &str, group: &GroupId, command: &CoreCommand) -> Result<Option<String>, String> {
        let home = home_partition(group) % self.partitions;
        follow_transfer(home, self.partitions, |partition| self.propose(partition, key, command)).await
    }

    async fn business_date(&self) -> Result<Option<NaiveDate>, String> {
        // Not open until every partition is; the earliest is the date.
        let mut earliest: Option<NaiveDate> = None;
        for partition in 0..self.partitions {
            match self.client.business_date(partition).await.map_err(|e| e.to_string())? {
                None => return Ok(None),
                Some(date) => earliest = Some(earliest.map_or(date, |e| e.min(date))),
            }
        }
        Ok(earliest)
    }
}

/// The command the reinsurance module takes for a notice, in its own envelope.
pub fn reinsurance_envelope(key: &str, on: NaiveDate, notice: &ReinsuranceNotice) -> Value {
    let command = match notice {
        ReinsuranceNotice::RecordRiskFactors {
            policy,
            life,
            date_of_birth,
            sex,
            smoker,
        } => json!({ "record_risk_factors": {
            "policy": policy,
            "life": life,
            "factors": { "date_of_birth": date_of_birth, "sex": sex, "smoker": smoker },
        }}),
        other => json!(other),
    };
    json!({ "key": key, "caller": crate::command::caller::STAFF, "on": on, "command": command })
}

/// A request for a member's excess as the underwriting module's proposal:
/// one life, one cover, the amount above the automatic limit. `None` while
/// the member is not linked to a person. The proposal is named by the key,
/// `grp-uw:{scheme}:{case}`, which is how the answer finds its case.
///
/// The scheme policy is in force and its premium is the scheme's, so there
/// is no quote to bind: the quote names the product version and no premium.
/// The policy number is the policy's ID, which is all this module holds.
pub fn underwriting_envelope(key: &str, at: DateTime<Utc>, today: NaiveDate, request: &UnderwritingRequest) -> Option<Value> {
    let party = request.party.as_ref()?;
    let proposal = json!({
        "proposal": key,
        "entry": { "entry": "group_excess", "policy": request.policy },
        "policy": request.policy,
        "policy_number": request.policy.to_string(),
        "group": request.group,
        "policyholder": request.policyholder,
        "product_code": request.product_code,
        "frequency": "monthly",
        "channel": "group-scheme",
        "quote": { "product_hash": request.product_hash, "rate_tables": [], "premium_minor": 0, "quoted_on": today },
        "lives": [{
            "life": request.life,
            "party": party,
            "factors": { "date_of_birth": request.date_of_birth, "sex": request.sex, "smoker": request.smoker },
        }],
        "covers": [{
            "code": request.cover,
            "cover_type": request.cover_type,
            "life": request.life,
            "sum_insured_minor": request.excess_minor,
        }],
    });
    Some(underwriting_command(key, at, today, json!({ "open_proposal": proposal })))
}

/// The withdrawal of a request underwriting has not answered.
pub fn underwriting_withdrawal(key: &str, at: DateTime<Utc>, today: NaiveDate, request_key: &str) -> Value {
    underwriting_command(key, at, today, json!({ "withdraw": { "proposal": request_key } }))
}

/// What the core made of accepted terms, as the issuer's report to
/// underwriting: the layer written, or the code it was refused with.
pub fn underwriting_outcome(key: &str, at: DateTime<Utc>, today: NaiveDate, request_key: &str, outcome: &IssueOutcome) -> Value {
    let command = match outcome {
        IssueOutcome::Issued { life, cover, layer } => json!({ "policy_issued": {
            "proposal": request_key,
            "layers": [{ "life": life, "cover": cover, "layer": layer }],
        }}),
        IssueOutcome::Rejected { code } => json!({ "issue_rejected": { "proposal": request_key, "code": code } }),
    };
    json!({ "key": key, "caller": "issuer", "at": at, "today": today, "command": command })
}

fn underwriting_command(key: &str, at: DateTime<Utc>, today: NaiveDate, command: Value) -> Value {
    json!({ "key": key, "caller": crate::command::caller::STAFF, "at": at, "today": today, "command": command })
}

/// The scheme and case a proposal named `grp-uw:{scheme}:{case}` answers.
fn case_of(proposal: &str) -> Option<(String, u64)> {
    let (scheme, case) = proposal.strip_prefix("grp-uw:")?.rsplit_once(':')?;
    Some((scheme.to_string(), case.parse().ok()?))
}

/// One of underwriting's effects as an answer to a request of this
/// module's, or `None` when it is something else. Terms are accepted cover
/// with its loadings and exclusions; a proposal that ended without terms
/// was declined, or was not proceeded with.
///
/// Underwriting does not say whether the member agreed to the employer
/// seeing a loading, so an answer never claims that consent.
pub fn excess_answer(effect: &Value) -> Option<ExcessAnswer> {
    let group_excess = |body: &Value| body["entry"]["entry"] == "group_excess";
    let (body, decision) = if let Some(body) = effect.get("apply_terms").filter(|b| group_excess(b)) {
        let cover = body["covers"].get(0)?;
        let decision = ExcessDecision::Accepted {
            sum_insured_minor: cover["sum_insured_minor"].as_i64()?,
            loadings: serde_json::from_value(cover["loadings"].clone()).ok()?,
            exclusions: serde_json::from_value(cover["exclusions"].clone()).ok()?,
            member_consents_to_itemising: false,
        };
        (body, decision)
    } else {
        let body = effect.get("proposal_ended").filter(|b| group_excess(b))?;
        let decision = match body["outcome"].as_str()? {
            "declined" => ExcessDecision::Declined,
            _ => ExcessDecision::NotProceeded,
        };
        (body, decision)
    };
    let (scheme, case) = case_of(body["proposal"].as_str()?)?;
    Some(ExcessAnswer {
        key: body["key"].as_str()?.to_string(),
        scheme,
        case,
        decision,
    })
}

/// The event as it is put on the stream, with what a consumer needs to
/// order and de-duplicate it without reading the body.
pub fn event_message(seq: u64, date: NaiveDate, event: &GroupEvent) -> (Value, Value) {
    let body = json!({ "seq": seq, "date": date, "event": event });
    let kind = body["event"]["type"].as_str().unwrap_or_default().to_string();
    (body, json!({ "event-id": format!("group:{seq}"), "seq": seq, "module": MODULE, "type": kind }))
}

/// The other modules on the cluster and the event stream on Event Hubs.
#[derive(Clone)]
pub struct ClusterPeers {
    client: ClusterClient,
    patience: Duration,
    /// The name the reinsurance module is hosted under.
    reinsurance: String,
    /// The name the underwriting module is hosted under.
    underwriting: String,
    events: EventHubsSink,
}

impl ClusterPeers {
    pub fn new(client: ClusterClient, patience: Duration, reinsurance: &str, underwriting: &str, events: EventHubsSink) -> Self {
        Self {
            client,
            patience,
            reinsurance: reinsurance.to_string(),
            underwriting: underwriting.to_string(),
            events,
        }
    }

    /// Send one command to underwriting. Its refusal comes back as the
    /// code; `settled` names the refusals that mean the work is done.
    async fn to_underwriting(&self, command: Value, settled: &[&str]) -> Result<(), String> {
        let reply = self
            .client
            .propose_module_until(&self.underwriting, &command, self.patience)
            .await
            .map_err(|e| e.to_string())?;
        let answer = reply.result.map_err(|e| format!("underwriting could not read the command: {e}"))?;
        match answer["Err"]["code"].as_str() {
            Some(code) if !settled.contains(&code) => Err(format!("underwriting refused: {code}")),
            _ => Ok(()),
        }
    }
}

impl Peers for ClusterPeers {
    async fn reinsurance(&self, key: &str, on: NaiveDate, notice: &ReinsuranceNotice) -> Result<(), String> {
        let command = reinsurance_envelope(key, on, notice);
        let reply = self
            .client
            .propose_module_until(&self.reinsurance, &command, self.patience)
            .await
            .map_err(|e| e.to_string())?;
        // Applied is delivered, whatever the module made of it: its own
        // refusal is its to raise, and sending again would not change it.
        reply.result.map(|_| ()).map_err(|e| format!("reinsurance could not read the notice: {e}"))
    }

    async fn underwriting(&self, key: &str, today: NaiveDate, request: &UnderwritingRequest) -> Result<(), String> {
        let command = underwriting_envelope(key, Utc::now(), today, request).ok_or("the member is not linked to a person")?;
        // A refusal is not stored under the key there, so the request stays
        // in the outbox and goes again: with no rules active for the
        // product, say, until they are.
        self.to_underwriting(command, &["duplicate_proposal"]).await
    }

    async fn withdraw_underwriting(&self, key: &str, today: NaiveDate, request_key: &str) -> Result<(), String> {
        // Answered, ended or never opened: there is nothing left to withdraw.
        self.to_underwriting(underwriting_withdrawal(key, Utc::now(), today, request_key), &["closed", "unknown_proposal"])
            .await
    }

    async fn underwriting_outcome(&self, key: &str, today: NaiveDate, request_key: &str, outcome: &IssueOutcome) -> Result<(), String> {
        // Recorded already, ended, or decided without a proposal there.
        self.to_underwriting(
            underwriting_outcome(key, Utc::now(), today, request_key, outcome),
            &["not_issuing", "unknown_proposal"],
        )
        .await
    }

    async fn underwriting_answers(&self, after: u64, max: usize) -> Result<Answers, String> {
        let question = json!({ "query": "outbox", "after": after, "max": max });
        let answer = self.client.query_module(&self.underwriting, &question).await.map_err(|e| e.to_string())??;
        let effects: Vec<(u64, Value)> = serde_json::from_value(answer).map_err(|e| e.to_string())?;
        Ok(Answers {
            through: effects.last().map_or(after, |(n, _)| *n),
            more: effects.len() >= max,
            answers: effects.iter().filter_map(|(_, effect)| excess_answer(effect)).collect(),
        })
    }

    async fn publish(&self, seq: u64, date: NaiveDate, event: &GroupEvent) -> Result<(), String> {
        self.events.send_json(0, &[event_message(seq, date, event)]).await.map_err(|e| e.to_string())
    }
}

/// Quarantine storage: a container of its own, written by the portal's
/// upload and read only here. Defender for Storage scans each blob as it
/// lands and records the result as an index tag; nothing reads a blob
/// whose tag does not say it is clean.
#[derive(Clone)]
pub struct QuarantineBlobs {
    container: BlobContainer,
}

impl QuarantineBlobs {
    pub fn new(container: BlobContainer) -> Self {
        Self { container }
    }

    /// Scheme and file names come from outside: only plain names make a
    /// blob path.
    fn name(scheme: &str, file: &str) -> Result<String, String> {
        let plain = |s: &str| !s.is_empty() && s.len() <= 128 && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        if plain(scheme) && plain(file) {
            Ok(format!("{scheme}/{file}"))
        } else {
            Err("scheme and file names are letters, digits, hyphens, underscores and dots".to_string())
        }
    }
}

impl FileStore for QuarantineBlobs {
    async fn put(&self, scheme: &str, file: &str, bytes: Vec<u8>) -> Result<(), String> {
        match self.container.put_new(&Self::name(scheme, file)?, "application/octet-stream", bytes).await {
            // Already stored: the module decides whether it is the same upload.
            Ok(()) | Err(AzureError::AlreadyExists(_)) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn scan(&self, scheme: &str, file: &str) -> Result<Scan, String> {
        let tags = self.container.tags(&Self::name(scheme, file)?).await.map_err(|e| e.to_string())?;
        Ok(match tags.as_ref().and_then(|t| t.get(MALWARE_SCAN_TAG)) {
            None => Scan::Pending,
            Some(result) if result == MALWARE_SCAN_CLEAN => Scan::Clean,
            Some(_) => Scan::Malicious,
        })
    }

    async fn read(&self, scheme: &str, file: &str) -> Result<Option<Vec<u8>>, String> {
        self.container.get(&Self::name(scheme, file)?).await.map_err(|e| e.to_string())
    }
}
