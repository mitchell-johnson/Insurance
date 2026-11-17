//! The disclosure intake: the process that takes a life's answers, seals
//! them in the clinical store and submits the disclosure to the module.
//!
//! It is the only writer of disclosures. The module never sees an answer:
//! it gets the record's hash, where the record is sealed, the versions the
//! answers were given against, what the rules said, and the life's
//! signature over the hash.
//!
//! Two calls. The first checks the answers against the question set in
//! force for the proposal and returns the hash for the life to sign on their
//! device, storing nothing. The second takes the same answers with the
//! signature, prepares them again (the record is canonical, so the hash is
//! the same), checks the signature before anything is written, seals the
//! record under the life's own key and submits. Every step can be repeated:
//! the record's name is its hash, and the submission's key is the proposal,
//! life, step and hash.

use std::collections::{BTreeMap, BTreeSet};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use ic_azure::keywrap::KeyWrapper;
use ic_azure::records::RecordStore;
use ic_core::LifeId;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::clinical::{ClinicalError, ClinicalStore, Fresh};
use crate::command::{Command, Envelope, Reject, SigningProof, SubmitDisclosure};
use crate::driver::{DriverError, Underwriting};
use crate::model::{Proposal, ProposalId};
use crate::prepare::{prepare_with, Basis, PrepareError, Prepared};
use crate::questions::{Answer, AnswerProblem};
use crate::signing::{message, SignedStep};
use crate::special::Query;

/// The caller the intake submits as.
pub const CALLER: &str = "new-business";
/// The caller an underwriter's reading goes through: the staff console.
pub const CONSOLE: &str = "underwriting-console";

/// An underwriter asks to read a life's disclosures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRequest {
    /// The console's ID for this reading; a repeat is the same reading.
    pub request: String,
    pub proposal: ProposalId,
    pub life: LifeId,
    /// The staff ID the console signed in.
    pub underwriter: String,
    pub purpose: String,
}

/// One disclosure, opened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Opened {
    pub vault_ref: String,
    pub hash: String,
    pub submitted_at: DateTime<Utc>,
    /// The record as sealed: the answers, the question set and who typed what.
    pub record: serde_json::Value,
}

/// Which signed step the answers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// The application's disclosure: every question answered.
    Disclosure,
    /// Answers confirmed after a tele-interview: a supplement.
    Interview,
}

impl Step {
    /// The signing domain, the record kind and the key segment.
    pub fn key(self) -> &'static str {
        match self {
            Step::Disclosure => "disclosure",
            Step::Interview => "interview",
        }
    }
}

/// A life's answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answers {
    pub proposal: ProposalId,
    pub life: LifeId,
    pub step: Step,
    /// By question ID: the question's version and the answer.
    pub answers: BTreeMap<String, (u32, Answer)>,
    /// The questions the adviser typed for the client.
    #[serde(default)]
    pub adviser_entered: BTreeSet<String>,
}

/// The answers with the life's signature over their hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signed {
    #[serde(flatten)]
    pub answers: Answers,
    pub signature: SignedStep,
}

/// What the life signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToSign {
    pub hash: String,
    /// The exact bytes to sign, base64.
    pub message: String,
}

/// What was submitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    pub hash: String,
    pub vault_ref: String,
    /// The module had this submission already.
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IntakeError {
    #[error("no proposal {0}")]
    UnknownProposal(ProposalId),
    #[error("{0} is not on the proposal")]
    UnknownLife(LifeId),
    #[error("no question set or rules are in force for the proposal")]
    NoRules,
    #[error("the life's date of birth is needed to run the rules")]
    NoDateOfBirth,
    #[error("the answers do not fit the question set")]
    Answers(Vec<AnswerProblem>),
    #[error("the signature is not the life's over these answers")]
    Signature,
    #[error("the module refused the disclosure: {}: {}", .0.code, .0.message)]
    Refused(Reject),
    #[error("the module answered without the disclosures")]
    NoView,
    #[error("the clinical store: {0}")]
    Store(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("no evidence request {0}")]
    UnknownRequest(String),
    #[error("the document: {0}")]
    Document(String),
}

impl From<DriverError> for IntakeError {
    fn from(e: DriverError) -> Self {
        IntakeError::Unavailable(e.to_string())
    }
}

impl From<PrepareError> for IntakeError {
    fn from(e: PrepareError) -> Self {
        match e {
            PrepareError::UnknownLife(life) => IntakeError::UnknownLife(life),
            PrepareError::NoRules => IntakeError::NoRules,
            PrepareError::NoDateOfBirth => IntakeError::NoDateOfBirth,
            PrepareError::Answers(problems) => IntakeError::Answers(problems),
        }
    }
}

/// The intake over the module and the clinical store.
pub struct Intake<U, W, R> {
    pub module: U,
    pub store: ClinicalStore<W, R>,
    pub clock: fn() -> DateTime<Utc>,
}

impl<U: Underwriting, W: KeyWrapper, R: RecordStore> Intake<U, W, R> {
    async fn prepared(&self, a: &Answers) -> Result<(Proposal, Prepared), IntakeError> {
        let read = |e: serde_json::Error| IntakeError::Unavailable(format!("the module answered in a form the intake does not read: {e}"));
        let proposal: Option<Proposal> = serde_json::from_value(self.module.ask(Query::Proposal { proposal: a.proposal.clone() }).await?).map_err(read)?;
        let proposal = proposal.ok_or_else(|| IntakeError::UnknownProposal(a.proposal.clone()))?;
        let basis: Option<Basis> = serde_json::from_value(self.module.ask(Query::DisclosureBasis { proposal: a.proposal.clone() }).await?).map_err(read)?;
        let basis = basis.ok_or(IntakeError::NoRules)?;
        let prepared = prepare_with(&basis, &proposal, &a.life, &a.answers, &a.adviser_entered, a.step == Step::Disclosure)?;
        Ok((proposal, prepared))
    }

    /// Check the answers and say what the life signs. Nothing is stored.
    pub async fn to_sign(&self, a: &Answers) -> Result<ToSign, IntakeError> {
        let (proposal, prepared) = self.prepared(a).await?;
        Ok(ToSign {
            message: STANDARD.encode(message(a.step.key(), &[&proposal.id.0, &a.life.0, &prepared.hash])),
            hash: prepared.hash,
        })
    }

    /// Seal the signed answers and submit them.
    pub async fn submit(&self, s: &Signed) -> Result<Submitted, IntakeError> {
        let a = &s.answers;
        let (proposal, prepared) = self.prepared(a).await?;
        // Checked here as well as in the module, so an unsigned record is never written.
        if !s.signature.verifies(&message(a.step.key(), &[&proposal.id.0, &a.life.0, &prepared.hash])) {
            return Err(IntakeError::Signature);
        }
        let party = &proposal.lives[&a.life].party;
        let vault_ref = match self.store.put(party, a.step.key(), &prepared.hash, &prepared.record, Fresh::random()).await {
            Ok(stored) => stored.vault_ref,
            // The record's name is its hash: what is there is this record.
            Err(ClinicalError::Exists(vault_ref)) => vault_ref,
            Err(e) => return Err(IntakeError::Store(e.to_string())),
        };
        let disclosure = Box::new(SubmitDisclosure {
            proposal: proposal.id.clone(),
            life: a.life.clone(),
            hash: prepared.hash.clone(),
            vault_ref: vault_ref.clone(),
            question_set: prepared.question_set,
            rule_set: prepared.rule_set,
            signing: SigningProof::Device(s.signature.clone()),
            entered_by_adviser: prepared.entered_by_adviser,
            assessments: prepared.assessments,
            risk_tags: prepared.risk_tags,
        });
        let command = match a.step {
            Step::Disclosure => Command::SubmitDisclosure(disclosure),
            Step::Interview => Command::ConfirmInterview(disclosure),
        };
        let at = (self.clock)();
        let today = crate::driver::books_today(&self.module, at).await?;
        let applied = self
            .module
            .propose(Envelope {
                key: format!("{}/{}/{}/{}", proposal.id, a.life, a.step.key(), prepared.hash),
                caller: CALLER.to_string(),
                at,
                today,
                command,
            })
            .await?
            .map_err(IntakeError::Refused)?;
        Ok(Submitted {
            hash: prepared.hash,
            vault_ref,
            duplicate: applied.duplicate,
        })
    }

    /// Open a life's disclosures for an underwriter. The module decides
    /// whether they may and logs the access first; only then is each record
    /// read, and its hash checked against the one the module holds.
    pub async fn open(&self, r: &OpenRequest) -> Result<Vec<Opened>, IntakeError> {
        let at = (self.clock)();
        let today = crate::driver::books_today(&self.module, at).await?;
        let applied = self
            .module
            .propose(Envelope {
                key: format!("open:{}", r.request),
                caller: CONSOLE.to_string(),
                at,
                today,
                command: Command::OpenDisclosures {
                    proposal: r.proposal.clone(),
                    life: r.life.clone(),
                    underwriter: r.underwriter.clone(),
                    purpose: r.purpose.clone(),
                },
            })
            .await?
            .map_err(IntakeError::Refused)?;
        let Some(crate::command::View::Disclosures(refs)) = applied.view else {
            return Err(IntakeError::NoView);
        };
        let mut out = Vec::new();
        for d in refs {
            let (bytes, _) = self
                .store
                .get(&d.vault_ref, &r.underwriter, &r.purpose)
                .await
                .map_err(|e| IntakeError::Store(e.to_string()))?;
            if hex::encode(sha2::Sha256::digest(&bytes)) != d.hash {
                return Err(IntakeError::Store(format!("{} does not have the hash it was submitted with", d.vault_ref)));
            }
            out.push(Opened {
                record: serde_json::from_slice(&bytes).map_err(|e| IntakeError::Store(format!("{} is not a disclosure record: {e}", d.vault_ref)))?,
                vault_ref: d.vault_ref,
                hash: d.hash,
                submitted_at: d.submitted_at,
            });
        }
        Ok(out)
    }
}
