//! Evidence and delegate files coming back: the provider's adapter reports
//! what happened to an order, and sends the report itself; a delegate sends
//! the file it was asked for. Each document is sealed in the clinical store
//! under the person's own key before the module hears of it, and the module
//! is given only where it is and, for a delegate's file, its hash.
//!
//! Every call can be repeated. A document's name is its hash, and each
//! command's key comes from the request (or, for a file, the proposal, life
//! and hash), so a retry after a lost answer files nothing twice.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ic_azure::keywrap::KeyWrapper;
use ic_azure::records::RecordStore;
use ic_core::LifeId;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::clinical::{ClinicalError, Fresh};
use crate::command::{Command, Envelope};
use crate::disclosures::{Intake, IntakeError};
use crate::driver::Underwriting;
use crate::model::{Proposal, ProposalId};
use crate::special::{EvidenceOrder, Query};

/// The caller an evidence provider's adapter submits as.
pub const EVIDENCE: &str = "evidence";
/// The caller a delegate's file is submitted as.
pub const DELEGATE: &str = "delegate-api";
/// The largest document taken, before base64.
pub const MOST_BYTES: usize = 20 * 1024 * 1024;

/// What a provider reports about an order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EvidenceAnswer {
    /// The provider has the order, under its own reference.
    Acknowledged { request: String, provider_ref: String },
    /// The report, base64, and what it cost.
    Received { request: String, report: String, cost_minor: i64 },
    /// The provider cannot get it.
    Unobtainable { request: String },
}

impl EvidenceAnswer {
    fn request(&self) -> &str {
        match self {
            EvidenceAnswer::Acknowledged { request, .. } | EvidenceAnswer::Received { request, .. } | EvidenceAnswer::Unobtainable { request } => request,
        }
    }
}

/// A delegate's file, base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegateFile {
    pub proposal: ProposalId,
    pub life: LifeId,
    pub file: String,
}

/// What was filed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filed {
    /// Where the document is sealed, when there was one.
    pub vault_ref: Option<String>,
    pub hash: Option<String>,
    /// The module had this already.
    pub duplicate: bool,
}

fn document(base64: &str) -> Result<(Vec<u8>, String), IntakeError> {
    let bytes = STANDARD.decode(base64.trim()).map_err(|e| IntakeError::Document(format!("not base64: {e}")))?;
    if bytes.is_empty() || bytes.len() > MOST_BYTES {
        return Err(IntakeError::Document(format!("a document is between 1 byte and {MOST_BYTES} bytes")));
    }
    let hash = hex::encode(sha2::Sha256::digest(&bytes));
    Ok((bytes, hash))
}

impl<U: Underwriting, W: KeyWrapper, R: RecordStore> Intake<U, W, R> {
    async fn seal(&self, party: &ic_core::PartyId, kind: &str, hash: &str, bytes: &[u8]) -> Result<String, IntakeError> {
        match self.store.put(party, kind, hash, bytes, Fresh::random()).await {
            Ok(stored) => Ok(stored.vault_ref),
            // The document's name is its hash: what is there is this document.
            Err(ClinicalError::Exists(vault_ref)) => Ok(vault_ref),
            Err(e) => Err(IntakeError::Store(e.to_string())),
        }
    }

    async fn send(&self, key: String, caller: &str, command: Command) -> Result<bool, IntakeError> {
        let at = (self.clock)();
        let today = crate::driver::books_today(&self.module, at).await?;
        let applied = self
            .module
            .propose(Envelope {
                key,
                caller: caller.to_string(),
                at,
                today,
                command,
            })
            .await?
            .map_err(IntakeError::Refused)?;
        Ok(applied.duplicate)
    }

    /// File a provider's answer about an order.
    pub async fn evidence(&self, a: &EvidenceAnswer) -> Result<Filed, IntakeError> {
        let request = a.request().to_string();
        let order: Option<EvidenceOrder> = serde_json::from_value(self.module.ask(Query::EvidenceOrder { request: request.clone() }).await?)
            .map_err(|e| IntakeError::Unavailable(format!("the module answered in a form the intake does not read: {e}")))?;
        let order = order.ok_or_else(|| IntakeError::UnknownRequest(request.clone()))?;
        let (key, command, sealed) = match a {
            EvidenceAnswer::Acknowledged { provider_ref, .. } => (
                format!("evidence:{request}:acknowledged"),
                Command::EvidenceAcknowledged {
                    request: request.clone(),
                    provider_ref: provider_ref.clone(),
                },
                None,
            ),
            EvidenceAnswer::Received { report, cost_minor, .. } => {
                if *cost_minor < 0 {
                    return Err(IntakeError::Document("a cost is not below zero".into()));
                }
                let (bytes, hash) = document(report)?;
                let vault_ref = self.seal(&order.party, "evidence", &hash, &bytes).await?;
                (
                    format!("evidence:{request}:received"),
                    Command::EvidenceReceived {
                        request: request.clone(),
                        vault_ref: vault_ref.clone(),
                        cost_minor: *cost_minor,
                    },
                    Some((vault_ref, hash)),
                )
            }
            EvidenceAnswer::Unobtainable { .. } => (
                format!("evidence:{request}:unobtainable"),
                Command::EvidenceUnobtainable { request: request.clone() },
                None,
            ),
        };
        let duplicate = self.send(key, EVIDENCE, command).await?;
        let (vault_ref, hash) = sealed.unzip();
        Ok(Filed { vault_ref, hash, duplicate })
    }

    /// Seal a delegate's file and tell the module.
    pub async fn delegate_file(&self, d: &DelegateFile) -> Result<Filed, IntakeError> {
        let proposal: Option<Proposal> = serde_json::from_value(self.module.ask(Query::Proposal { proposal: d.proposal.clone() }).await?)
            .map_err(|e| IntakeError::Unavailable(format!("the module answered in a form the intake does not read: {e}")))?;
        let proposal = proposal.ok_or_else(|| IntakeError::UnknownProposal(d.proposal.clone()))?;
        let party = proposal
            .lives
            .get(&d.life)
            .ok_or_else(|| IntakeError::UnknownLife(d.life.clone()))?
            .party
            .clone();
        let (bytes, hash) = document(&d.file)?;
        let vault_ref = self.seal(&party, "delegate_file", &hash, &bytes).await?;
        let duplicate = self
            .send(
                format!("delegate-file:{}:{}:{hash}", d.proposal, d.life),
                DELEGATE,
                Command::DelegateFileReceived {
                    proposal: d.proposal.clone(),
                    life: d.life.clone(),
                    vault_ref: vault_ref.clone(),
                    hash: hash.clone(),
                },
            )
            .await?;
        Ok(Filed {
            vault_ref: Some(vault_ref),
            hash: Some(hash),
            duplicate,
        })
    }
}
