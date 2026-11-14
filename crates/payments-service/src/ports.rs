//! What the pump needs from the rest of the system, as traits, with the
//! implementations that reach a running cluster and Azure.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use ic_azure::credential::AzureCredential;
use ic_azure::keyvault::{KeyVaultSigner, RootSigner, Signature};
use ic_cashmatch::{Candidates, IncomingPayment, Matcher, Outcome};
use ic_core::{Command, Envelope as CoreEnvelope, PolicyId};
use ic_payments::{Envelope, Output, Role};
use ic_raft::client::fresh_salt;
use ic_raft::ClusterClient;
use serde_json::Value;

use crate::MODULE;

/// The payments module, wherever it is hosted.
pub trait Module: Send + Sync {
    fn propose(&self, envelope: Envelope) -> impl Future<Output = Result<Output, String>> + Send;
    fn query(&self, question: Value) -> impl Future<Output = Result<Value, String>> + Send;
    /// Give another hosted module a command in its own form. `Ok` is that
    /// module's answer, whatever it is; `Err` means it could not be asked.
    fn tell(&self, module: &str, command: Value) -> impl Future<Output = Result<Value, String>> + Send;
    /// Ask another hosted module a question in its own form.
    fn ask(&self, module: &str, question: Value) -> impl Future<Output = Result<Value, String>> + Send;
}

/// The core. `Ok(None)` is done, `Ok(Some(code))` is the core's refusal,
/// and `Err` means the core could not be asked: try again later.
pub trait Core: Send + Sync {
    fn send(&self, key: &str, caller: &str, command: Command) -> impl Future<Output = Result<Option<String>, String>> + Send {
        async move {
            match policy_of(&command).cloned() {
                Some(policy) => self.send_to(&policy, key, caller, command).await,
                None => Ok(Some("not_a_payments_command".to_string())),
            }
        }
    }
    /// Send a command that names no policy to one partition.
    fn send_on(&self, partition: u16, key: &str, caller: &str, command: Command) -> impl Future<Output = Result<Option<String>, String>> + Send;
    /// What each partition holds of a kind of withheld tax in New Zealand
    /// dollars, fund by fund. Holdings of nothing are left out.
    fn withholding_held(&self, kind: &str) -> impl Future<Output = Result<Vec<ic_payments::tax::Held>, String>> + Send;
    /// Send a command to the partition that holds `policy`.
    fn send_to(&self, policy: &PolicyId, key: &str, caller: &str, command: Command) -> impl Future<Output = Result<Option<String>, String>> + Send;
    /// Hand an unexpected credit to cash matching. `Ok(Some(reason))` means
    /// nobody could say whose it is.
    fn match_receipt(&self, payment: IncomingPayment) -> impl Future<Output = Result<Option<String>, String>> + Send;
    /// The same, knowing the policy whose direct debit authority draws on
    /// the paying account. A core that cannot use it matches without it.
    fn match_receipt_from(&self, payment: IncomingPayment, payer_policy: Option<PolicyId>) -> impl Future<Output = Result<Option<String>, String>> + Send {
        let _ = payer_policy;
        self.match_receipt(payment)
    }
}

/// Signing keys by the name they are held under.
pub trait Keys: Send + Sync {
    fn sign(&self, key: &str, message: &[u8]) -> impl Future<Output = Result<Signature, String>> + Send;
}

/// Bank credentials by the name they are held under.
pub trait Secrets: Send + Sync {
    fn secret(&self, name: &str) -> Option<String>;
}

/// People who must act on something.
pub trait Notifier: Send + Sync {
    fn notify(&self, to: Role, subject: &str, detail: &str) -> impl Future<Output = Result<(), String>> + Send;
}

impl Module for ClusterClient {
    async fn propose(&self, envelope: Envelope) -> Result<Output, String> {
        // Our own envelope always has a JSON form.
        let command = serde_json::json!(envelope);
        let reply = self.propose_module_until(MODULE, &command, Duration::from_secs(20)).await.map_err(lost)?;
        serde_json::from_value(reply.result?).map_err(unreadable)
    }

    async fn query(&self, question: Value) -> Result<Value, String> {
        self.query_module(MODULE, &question).await.map_err(lost)?
    }

    async fn tell(&self, module: &str, command: Value) -> Result<Value, String> {
        self.propose_module_until(module, &command, Duration::from_secs(20)).await.map_err(lost)?.result
    }

    async fn ask(&self, module: &str, question: Value) -> Result<Value, String> {
        self.query_module(module, &question).await.map_err(lost)?
    }
}

/// The cluster could not be reached or did not answer.
fn lost(e: ic_raft::client::ClientError) -> String {
    e.to_string()
}

/// An answer that is not in the form expected.
pub(crate) fn unreadable(e: serde_json::Error) -> String {
    format!("unreadable answer: {e}")
}

/// The code another module refused a command with, if it refused. Modules
/// answer `{"Err": {"code": ..}}`, alone or under `result`.
pub fn refusal(answer: &Value) -> Option<String> {
    let refused = answer.pointer("/result/Err").or_else(|| answer.pointer("/Err"))?;
    Some(refused.get("code").and_then(Value::as_str).unwrap_or("refused").to_string())
}

/// The policy a command of ours is about.
pub fn policy_of(command: &Command) -> Option<&PolicyId> {
    match command {
        Command::SettlePayable { policy, .. }
        | Command::ReturnPayment { policy, .. }
        | Command::RaiseRefundPayable { policy, .. }
        | Command::ReleaseRefundPayable { policy, .. }
        | Command::ApplyReceipt { policy, .. }
        | Command::ReverseReceipt { policy, .. } => Some(policy),
        _ => None,
    }
}

/// The core reached through a cluster's nodes.
#[derive(Clone)]
pub struct ClusterCore {
    pub client: ClusterClient,
    pub matcher: Matcher,
}

impl ClusterCore {
    async fn propose(&self, partition: u16, key: &str, caller: &str, command: Command) -> Result<Option<String>, String> {
        let envelope = CoreEnvelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        };
        let reply = self.client.propose_until(partition, &envelope, Duration::from_secs(20)).await.map_err(lost)?;
        Ok(reply.rejected.map(|r| r.code))
    }
}

impl Core for ClusterCore {
    async fn send_on(&self, partition: u16, key: &str, caller: &str, command: Command) -> Result<Option<String>, String> {
        self.propose(partition, key, caller, command).await
    }

    async fn withholding_held(&self, kind: &str) -> Result<Vec<ic_payments::tax::Held>, String> {
        let partitions = self.client.partitions(Duration::from_secs(20)).await?;
        let mut held = Vec::new();
        for partition in 0..partitions {
            for fund in self.client.withholding_held(partition, kind, ic_money::Currency::NZD).await.map_err(lost)? {
                let minor = i64::try_from(fund.minor).map_err(|_| format!("partition {partition} holds more {kind} than can be paid"))?;
                held.push((partition, fund.statutory_fund, minor));
            }
        }
        Ok(held)
    }

    async fn send_to(&self, policy: &PolicyId, key: &str, caller: &str, command: Command) -> Result<Option<String>, String> {
        let partitions = self.client.partitions(Duration::from_secs(20)).await?;
        for partition in 0..partitions {
            if self.client.locate(partition, policy).await.map_err(lost)?.is_some() {
                return self.propose(partition, key, caller, command).await;
            }
        }
        Ok(Some("unknown_policy".to_string()))
    }

    async fn match_receipt(&self, payment: IncomingPayment) -> Result<Option<String>, String> {
        self.match_receipt_from(payment, None).await
    }

    async fn match_receipt_from(&self, payment: IncomingPayment, payer_policy: Option<PolicyId>) -> Result<Option<String>, String> {
        let mut matcher = self.matcher.clone();
        if let (Some(account), Some(policy)) = (&payment.payer_account, payer_policy) {
            matcher.authorities.insert(account.clone(), policy);
        }
        let partitions = self.client.partitions(Duration::from_secs(20)).await?;
        let mut all = Candidates::default();
        for partition in 0..partitions {
            let found = self.client.payment_candidates(partition, &matcher, &payment).await.map_err(lost)?;
            all = all.merge(found);
        }
        match matcher.decide(all, &payment) {
            Outcome::Apply { command, .. } => self.send(&payment.id, "cash-matching", command).await,
            Outcome::Exception { reason, .. } => Ok(Some(reason)),
        }
    }
}

/// Signing keys in Key Vault or Managed HSM.
#[derive(Clone)]
pub struct VaultKeys {
    pub vault: String,
    pub credential: AzureCredential,
}

impl Keys for VaultKeys {
    async fn sign(&self, key: &str, message: &[u8]) -> Result<Signature, String> {
        KeyVaultSigner::new(&self.vault, key, self.credential.clone())
            .sign(message)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Secrets as files in a directory: how the Key Vault CSI driver hands a
/// pod its secrets, one file per secret, re-read on every use so a rotated
/// secret is picked up without a restart.
#[derive(Clone)]
pub struct MountedSecrets(pub PathBuf);

impl Secrets for MountedSecrets {
    fn secret(&self, name: &str) -> Option<String> {
        let safe = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !safe {
            return None;
        }
        let text = std::fs::read_to_string(self.0.join(name)).ok()?;
        Some(text.trim().to_string()).filter(|s| !s.is_empty())
    }
}

impl Secrets for BTreeMap<String, String> {
    fn secret(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
}

/// Notices written to the structured log, where the alerting rules for each
/// subject pick them up and page the role named.
#[derive(Clone, Default)]
pub struct LogNotifier;

impl Notifier for LogNotifier {
    async fn notify(&self, to: Role, subject: &str, detail: &str) -> Result<(), String> {
        tracing::warn!(target: "payments.notice", role = ?to, subject, detail, "action needed");
        Ok(())
    }
}

/// What an answer about a party's own hold resolves to.
pub type HoldAnswer<'a> = std::pin::Pin<Box<dyn Future<Output = Result<bool, String>> + Send + 'a>>;

/// The customer master, as payments asks it whether a party's own hold on
/// money going out stands (SVC-022). `Err` means it could not say: the
/// module then holds what it would have stopped, once its record is stale.
pub trait Holds: Send + Sync {
    fn held<'a>(&'a self, party: &'a str) -> HoldAnswer<'a>;

    /// A relationship by the master's reference `{party}/{id}`: its record,
    /// `None` when the master has no such relationship, `Err` when it
    /// could not say.
    fn relationship<'a>(&'a self, reference: &'a str) -> RelationshipAnswer<'a>;

    /// Whether the master has erased the party (retention shredding).
    fn erased<'a>(&'a self, party: &'a str) -> HoldAnswer<'a>;
}

/// Whether the master's answer about a party says it has erased them: a
/// 410 Gone is erased; a 200, or a 404 for a party it never knew, is not;
/// anything else is no answer.
pub fn erased_in(status: u16) -> Result<bool, String> {
    match status {
        410 => Ok(true),
        200 | 404 => Ok(false),
        status => Err(format!("the customer master answered {status}")),
    }
}

/// What an answer about a relationship resolves to.
pub type RelationshipAnswer<'a> = std::pin::Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send + 'a>>;

/// Whether the master's record of a relationship links the policy owner
/// and the payer (FIN-016): it exists, is in force, and has the owner, and a
/// payer known as a party, at one end or the other. A payer known only by
/// an account has no party for the master to link.
pub fn relationship_links(record: Option<&Value>, owner: &str, payer: &str) -> Result<(), String> {
    let record = record.ok_or("the customer master has no such relationship")?;
    if record.get("in_force").and_then(Value::as_bool) != Some(true) {
        return Err("the relationship is not in force".to_string());
    }
    let ends = [record.get("party"), record.pointer("/target/id")].map(|v| v.and_then(Value::as_str).unwrap_or_default());
    let links = |party: &str| ends.contains(&party);
    match payer.strip_prefix("party:") {
        _ if !links(owner) => Err(format!("the relationship does not name the owner {owner}")),
        Some(party) if !links(party) => Err(format!("the relationship does not name the payer {party}")),
        _ => Ok(()),
    }
}

/// The relationship the master's answer gives: a 200 is the record, a 404
/// is none, anything else is no answer.
pub fn relationship_in(status: u16, body: Value) -> Result<Option<Value>, String> {
    match status {
        200 => Ok(Some(body)),
        404 => Ok(None),
        status => Err(format!("the customer master answered {status}")),
    }
}

/// The customer master service, reached with the pump's own Entra token
/// as the `payments` caller (`GET /v1/parties/{id}/spending-hold`).
pub struct MasterHolds {
    pub base: String,
    /// The master's application ID URI scope, such as `api://ic-customers/.default`.
    pub scope: String,
    pub credential: AzureCredential,
    pub http: reqwest::Client,
}

/// Whether a party ID can go into a path as it is. Anything else is not
/// asked about: the master would not know it.
fn plain_party(party: &str) -> bool {
    !party.is_empty() && party.len() <= 128 && party.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The hold the master's answer states. Anything but a 200 with a boolean
/// `held` is not an answer.
pub fn hold_in(status: u16, body: &Value) -> Result<bool, String> {
    match (status, body.get("held").and_then(Value::as_bool)) {
        (200, Some(held)) => Ok(held),
        (200, None) => Err("the customer master's answer has no `held`".to_string()),
        (status, _) => Err(format!("the customer master answered {status}")),
    }
}

impl MasterHolds {
    async fn ask(&self, party: &str) -> Result<bool, String> {
        let (status, body) = self.spending_hold(party).await?;
        hold_in(status, &body)
    }

    async fn ask_erased(&self, party: &str) -> Result<bool, String> {
        let (status, _) = self.spending_hold(party).await?;
        erased_in(status)
    }

    async fn spending_hold(&self, party: &str) -> Result<(u16, Value), String> {
        if !plain_party(party) {
            return Err(format!("{party:?} is not a party ID the customer master knows"));
        }
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let response = self
            .http
            .get(format!("{}/v1/parties/{party}/spending-hold", self.base.trim_end_matches('/')))
            .bearer_auth(token)
            .header("x-ic-caller", "payments")
            .header("x-ic-purpose", "payment")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        Ok((status, response.json::<Value>().await.unwrap_or(Value::Null)))
    }
}

impl MasterHolds {
    async fn find_relationship(&self, reference: &str) -> Result<Option<Value>, String> {
        let Some((party, id)) = reference.split_once('/').filter(|(p, i)| plain_party(p) && plain_party(i)) else {
            return Err(format!("{reference:?} is not a relationship reference"));
        };
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let response = self
            .http
            .get(format!("{}/v1/parties/{party}/relationships/{id}", self.base.trim_end_matches('/')))
            .bearer_auth(token)
            .header("x-ic-caller", "payments")
            .header("x-ic-purpose", "compliance")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        relationship_in(status, response.json::<Value>().await.unwrap_or(Value::Null))
    }
}

impl Holds for MasterHolds {
    fn held<'a>(&'a self, party: &'a str) -> HoldAnswer<'a> {
        Box::pin(self.ask(party))
    }

    fn relationship<'a>(&'a self, reference: &'a str) -> RelationshipAnswer<'a> {
        Box::pin(self.find_relationship(reference))
    }

    fn erased<'a>(&'a self, party: &'a str) -> HoldAnswer<'a> {
        Box::pin(self.ask_erased(party))
    }
}

impl Holds for BTreeMap<String, bool> {
    fn held<'a>(&'a self, party: &'a str) -> HoldAnswer<'a> {
        let answer = self.get(party).copied().ok_or_else(|| format!("no answer for {party}"));
        Box::pin(std::future::ready(answer))
    }

    fn relationship<'a>(&'a self, reference: &'a str) -> RelationshipAnswer<'a> {
        Box::pin(std::future::ready(Err(format!("no answer for {reference}"))))
    }

    fn erased<'a>(&'a self, party: &'a str) -> HoldAnswer<'a> {
        Box::pin(std::future::ready(Err(format!("no answer for {party}"))))
    }
}
