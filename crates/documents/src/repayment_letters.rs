//! Letters asking someone to repay a claim overpayment.
//!
//! The claims module lives inside each core partition and cannot ask for a
//! letter itself, so this step pulls: it asks every partition which
//! repayment letters are due (`repayment_letters`), asks payments for the
//! repayment reference of each overpayment, and records the letter as a
//! request from claims (`RequestLetter`, kind `overpayment_repayment`),
//! which the requested letters step then issues like any other. It then
//! tells the partition the letter is taken (`RecordRepaymentLetter`), so
//! it leaves the list.
//!
//! A letter whose overpayment payments says nothing is owed on is skipped:
//! claims drops it from the list on its own once the overpayment is
//! repaid, written off or no longer pursued. Every step is safe to repeat:
//! the request id is claims' own, and the module takes a request once.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ic_core::claims_module::report::{LetterDue, Question, Reply};
use ic_core::claims_module::Command as ClaimsCommand;
use ic_core::{PartyId, PolicyId};
use ic_raft::ClusterClient;
use serde_json::{json, Value};

use crate::channels::{Channels, ContactDirectory};
use crate::command::Command;
use crate::complaint_letters::LetterPass;
use crate::crypto::KeyWrap;
use crate::driver::{Driver, DriverError, Module};
use crate::model::{LetterRecipient, LetterRequest};
use crate::vault::ObjectStore;

/// The letter kind, and the module it is recorded as coming from.
pub const KIND: &str = "overpayment_repayment";
pub const FROM: &str = "claims";

/// What this step needs of the claims held on the partitions.
pub trait ClaimsLetters: Send + Sync {
    /// How many partitions there are.
    fn partitions(&self) -> u16;
    /// The repayment letters due on one partition. `None` while its
    /// business date is not open.
    fn due(&self, partition: u16) -> impl Future<Output = Result<Option<Vec<LetterDue>>, String>> + Send;
    /// Tell the partition the letter is taken. `Ok(Some(code))` is a refusal.
    fn record(&self, partition: u16, key: String, command: ClaimsCommand) -> impl Future<Output = Result<Option<String>, String>> + Send;
}

/// What this step needs of payments: the reference a repayment must quote,
/// or `None` once nothing is owed.
pub trait RepaymentReferences: Send + Sync {
    fn reference(&self, overpayment: &str) -> impl Future<Output = Result<Option<String>, String>> + Send;
}

/// The partitions, through the cluster.
#[derive(Clone)]
pub struct ClusterClaimsLetters {
    client: ClusterClient,
    partitions: u16,
    patience: Duration,
}

impl ClusterClaimsLetters {
    pub fn new(client: ClusterClient, partitions: u16, patience: Duration) -> Self {
        Self { client, partitions, patience }
    }
}

impl ClaimsLetters for ClusterClaimsLetters {
    fn partitions(&self) -> u16 {
        self.partitions
    }

    async fn due(&self, partition: u16) -> Result<Option<Vec<LetterDue>>, String> {
        match self.client.claims(partition, &Question::RepaymentLetters).await.map_err(|e| e.to_string())? {
            Some(Reply::RepaymentLetters(due)) => Ok(Some(due)),
            Some(_) => Err("claims answered another question".into()),
            None => Ok(None),
        }
    }

    async fn record(&self, partition: u16, key: String, command: ClaimsCommand) -> Result<Option<String>, String> {
        let envelope = ic_core::Envelope {
            key,
            caller: "documents".into(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command: ic_core::Command::Claims(Box::new(command)),
        };
        let reply = self
            .client
            .propose_until(partition, &envelope, self.patience)
            .await
            .map_err(|e| e.to_string())?;
        Ok(reply.rejected.map(|r| r.code))
    }
}

/// Payments, through the cluster.
#[derive(Clone)]
pub struct ClusterReferences {
    client: ClusterClient,
}

impl ClusterReferences {
    pub fn new(client: ClusterClient) -> Self {
        Self { client }
    }
}

impl RepaymentReferences for ClusterReferences {
    async fn reference(&self, overpayment: &str) -> Result<Option<String>, String> {
        let question = json!({ "q": "repayment_reference", "overpayment": overpayment });
        let answer = self.client.query_module("payments", &question).await.map_err(|e| e.to_string())??;
        reference_of(answer)
    }
}

/// Payments' answer: a reference, or null once nothing is owed.
pub fn reference_of(answer: Value) -> Result<Option<String>, String> {
    match answer {
        Value::Null => Ok(None),
        Value::String(reference) if !reference.trim().is_empty() => Ok(Some(reference)),
        other => Err(format!("payments answered {other} for a repayment reference")),
    }
}

/// An amount in minor units as the letter shows it: `1250.00`.
pub fn amount(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let minor = minor.unsigned_abs();
    format!("{sign}{}.{:02}", minor / 100, minor % 100)
}

/// The request recorded for a letter that is due.
pub fn request(letter: &LetterDue, reference: &str) -> LetterRequest {
    LetterRequest {
        request: letter.request.clone(),
        kind: KIND.into(),
        recipient: LetterRecipient::Party {
            party: PartyId(letter.party.clone()),
        },
        policy: Some(PolicyId(letter.policy.clone())),
        subject: letter.claim.clone(),
        life: letter.life.clone(),
        reasons: vec![],
        detail: [
            ("repayment_reference".to_string(), reference.to_string()),
            ("amount".to_string(), amount(letter.amount_minor)),
        ]
        .into(),
        business_date: letter.asked_on,
    }
}

pub struct RepaymentRelay<K, R> {
    pub claims: K,
    pub references: R,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl<K: ClaimsLetters, R: RepaymentReferences> RepaymentRelay<K, R> {
    /// Record one letter as asked for and tell its partition. `Ok(false)`
    /// skips it: nothing is owed any more.
    async fn one<M, S, W, C, D>(&self, driver: &Driver<M, S, W, C, D>, partition: u16, letter: &LetterDue) -> Result<bool, String>
    where
        M: Module,
        S: ObjectStore,
        W: KeyWrap,
        C: Channels,
        D: ContactDirectory,
    {
        let Some(reference) = self.references.reference(&letter.overpayment).await? else {
            return Ok(false);
        };
        let key = format!("repayment-letter:{}", letter.request);
        match driver.send(key.clone(), FROM, Command::RequestLetter(request(letter, &reference))).await {
            Ok(_) => {}
            // Recorded on an earlier pass, when less or more was owed: the
            // letter stands as asked then.
            Err(DriverError::Refused(reject)) if reject.code == "request_conflict" => {}
            Err(e) => return Err(e.to_string()),
        }
        let taken = ClaimsCommand::RecordRepaymentLetter {
            claim: letter.claim.clone(),
            overpayment: letter.overpayment.clone(),
            request: letter.request.clone(),
        };
        // The time is in the key, so that a refusal is not remembered
        // against a later attempt.
        let key = format!("{key}:{}", (self.clock)().timestamp());
        match self.claims.record(partition, key, taken).await? {
            None => Ok(true),
            Some(code) => Err(format!("claims refused the record: {code}")),
        }
    }

    /// One pass over every partition.
    pub async fn pass<M, S, W, C, D>(&self, driver: &Driver<M, S, W, C, D>) -> LetterPass
    where
        M: Module,
        S: ObjectStore,
        W: KeyWrap,
        C: Channels,
        D: ContactDirectory,
    {
        let mut pass = LetterPass::default();
        for partition in 0..self.claims.partitions() {
            let due = match self.claims.due(partition).await {
                Ok(Some(due)) => due,
                Ok(None) => continue,
                Err(e) => {
                    pass.errors.push(format!("partition {partition}: repayment letters: {e}"));
                    continue;
                }
            };
            for letter in &due {
                match self.one(driver, partition, letter).await {
                    Ok(true) => pass.issued += 1,
                    Ok(false) => pass.left += 1,
                    Err(e) => pass.errors.push(format!("{}: {e}", letter.request)),
                }
            }
        }
        pass
    }
}

/// Run the step every `every` until `stop` completes.
pub async fn run_repayment_letters<K, R, M, S, W, C, D>(
    relay: RepaymentRelay<K, R>,
    driver: Arc<Driver<M, S, W, C, D>>,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&LetterPass),
) where
    K: ClaimsLetters,
    R: RepaymentReferences,
    M: Module,
    S: ObjectStore,
    W: KeyWrap,
    C: Channels,
    D: ContactDirectory,
{
    tokio::pin!(stop);
    loop {
        let pass = relay.pass(&driver).await;
        if pass.errors.is_empty() {
            tracing::debug!(asked = pass.issued, "repayment letters pass");
        } else {
            tracing::warn!(asked = pass.issued, errors = ?pass.errors, "a repayment letters pass could not do everything");
        }
        seen(&pass);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}
