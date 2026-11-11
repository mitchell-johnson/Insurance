//! What goes to a bank and what comes back.
//!
//! A submission becomes a batch: the same content whether it travels as a
//! file with a signed manifest or as one API request. The file's text is
//! fixed by the batch alone, so its hash is the same on every replica and on
//! every attempt, and the bank can tell a repeat from something new.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_money::Currency;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::machine::{Command, PaymentsState};
use crate::model::{Id, ReturnReason, StatementLine};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Pay the accounts listed.
    Credit,
    /// Collect from the accounts listed.
    Debit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchItem {
    pub item: Id,
    pub name: String,
    pub number: String,
    pub amount_minor: i64,
}

/// One submission as the bank receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    /// The submission's ID: the file ID, and the API's idempotency key.
    pub id: Id,
    pub channel: Id,
    pub account: Id,
    pub direction: Direction,
    pub value_date: NaiveDate,
    pub currency: Currency,
    pub items: Vec<BatchItem>,
}

/// What travels beside a file: enough for the bank to check that the file is
/// whole and is ours. The signature is over the file's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub file_id: Id,
    pub sha256: String,
    pub bytes: usize,
    pub items: usize,
    pub total_minor: i64,
    pub algorithm: String,
    pub key_id: String,
    pub signature: String,
}

/// Field separators and line ends never appear inside a field.
fn field(text: &str) -> String {
    text.chars().map(|c| if c == '|' || c.is_control() { ' ' } else { c }).collect()
}

impl Batch {
    pub fn total_minor(&self) -> i64 {
        self.items.iter().map(|i| i.amount_minor).sum()
    }

    /// The file: a header, one line per item, and a trailer with the count
    /// and total the bank checks before it touches a line.
    pub fn file(&self) -> String {
        let direction = match self.direction {
            Direction::Credit => "CREDIT",
            Direction::Debit => "DEBIT",
        };
        let mut out = format!(
            "H|ICPAY1|{}|{}|{direction}|{}|{:?}\n",
            field(&self.id),
            field(&self.account),
            self.value_date,
            self.currency
        );
        for i in &self.items {
            out.push_str(&format!("D|{}|{}|{}|{}\n", field(&i.item), field(&i.name), field(&i.number), i.amount_minor));
        }
        out.push_str(&format!("T|{}|{}\n", self.items.len(), self.total_minor()));
        out
    }

    pub fn content_hash(&self) -> String {
        hex::encode(Sha256::digest(self.file().as_bytes()))
    }

    pub fn manifest(&self, algorithm: String, key_id: String, signature: String) -> Manifest {
        Manifest {
            file_id: self.id.clone(),
            sha256: self.content_hash(),
            bytes: self.file().len(),
            items: self.items.len(),
            total_minor: self.total_minor(),
            algorithm,
            key_id,
            signature,
        }
    }
}

impl PaymentsState {
    /// The batch for a submission, from the accounts recorded when its run
    /// was checked. `None` if the submission or any of its accounts is gone.
    pub fn batch(&self, submission: &str) -> Option<Batch> {
        let s = self.submissions.get(submission)?;
        let run = self.runs.get(&s.run)?;
        let items = s
            .items
            .iter()
            .map(|id| {
                let item = run.items.get(id)?;
                let account = item.account.as_ref()?;
                Some(BatchItem {
                    item: id.clone(),
                    name: account.name.clone(),
                    number: account.number.clone(),
                    amount_minor: item.new.amount_minor,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Batch {
            id: submission.to_string(),
            channel: s.channel.clone(),
            account: s.account.clone(),
            direction: if run.run_type.inbound() { Direction::Debit } else { Direction::Credit },
            value_date: run.value_date,
            currency: run.currency,
            items,
        })
    }
}

/// What a bank tells us after a submission, and each day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BankMessage {
    /// The bank's answer to a submission: refused whole, or accepted with
    /// some items refused.
    Acknowledgement {
        id: Id,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refused: Option<String>,
        #[serde(default)]
        rejected: BTreeMap<Id, String>,
    },
    Settlement {
        id: Id,
        settled_on: NaiveDate,
    },
    /// One item came back: a payment that could not be delivered, or a
    /// collection that was dishonoured.
    Return {
        item: Id,
        reason: ReturnReason,
        returned_on: NaiveDate,
    },
    Statement {
        account: Id,
        date: NaiveDate,
        opening_minor: i64,
        closing_minor: i64,
        lines: Vec<StatementLine>,
    },
}

impl BankMessage {
    /// The module command that records this message.
    pub fn command(self) -> Command {
        match self {
            BankMessage::Acknowledgement { id, refused, rejected } => Command::RecordAcknowledgement {
                submission: id,
                refused,
                rejected,
            },
            BankMessage::Settlement { id, settled_on } => Command::RecordSettlement { submission: id, settled_on },
            BankMessage::Return { item, reason, returned_on } => Command::RecordReturn { item, reason, returned_on },
            BankMessage::Statement {
                account,
                date,
                opening_minor,
                closing_minor,
                lines,
            } => Command::RecordStatement {
                account,
                date,
                opening_minor,
                closing_minor,
                lines,
            },
        }
    }
}
