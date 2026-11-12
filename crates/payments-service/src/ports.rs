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
    /// Send a command to the partition that holds `policy`.
    fn send_to(&self, policy: &PolicyId, key: &str, caller: &str, command: Command) -> impl Future<Output = Result<Option<String>, String>> + Send;
    /// Hand an unexpected credit to cash matching. `Ok(Some(reason))` means
    /// nobody could say whose it is.
    fn match_receipt(&self, payment: IncomingPayment) -> impl Future<Output = Result<Option<String>, String>> + Send;
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
        let partitions = self.client.partitions(Duration::from_secs(20)).await?;
        let mut all = Candidates::default();
        for partition in 0..partitions {
            let found = self.client.payment_candidates(partition, &self.matcher, &payment).await.map_err(lost)?;
            all = all.merge(found);
        }
        match self.matcher.decide(all, &payment) {
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
