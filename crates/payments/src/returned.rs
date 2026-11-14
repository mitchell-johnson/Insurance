//! Money received that goes back to whoever sent it (GRP-015).
//!
//! An employer's remittance paid twice is recalled by its bank as a
//! duplicate. When a reconciler agrees to give a credit back
//! (`return_credit`), every policy cash matching applied it to is told to
//! reverse that receipt (`ReverseReceipt`, by new entries: the core works
//! out standing again from the original due dates), and the debit the bank
//! will make is expected on the account under the credit's own reference,
//! so the statement reconciles.
//!
//! The receipts cash matching applied are remembered by the core's
//! `ReceiptApplied` (`PremiumApplied` here) for `KEEP_DAYS`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, NaiveDate};
use ic_core::PolicyId;
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{About, ExceptionKind, Expected, Instruction, OutboxEntry, Reject, Role};

/// How long the policies a receipt was applied to are remembered: a
/// payer's bank can recall a credit for about a year. A recorded best
/// estimate.
pub const KEEP_DAYS: i64 = 400;

/// Outbox key prefix of a receipt reversed because the money went back.
pub(crate) const RETURNED: &str = "credit-returned-";

/// Policies each receipt was applied to, with the day it was first seen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub receipts: BTreeMap<String, (NaiveDate, BTreeSet<String>)>,
    /// Credits given back, with the reason.
    #[serde(default)]
    pub returned: BTreeMap<String, String>,
}

impl PaymentsState {
    /// The core applied a receipt to a policy.
    pub(crate) fn note_applied(&mut self, receipt: &str, policy: &str, today: NaiveDate) {
        let keep_from = today - Duration::days(KEEP_DAYS);
        self.applied.receipts.retain(|_, (seen, _)| *seen >= keep_from);
        self.applied
            .receipts
            .entry(receipt.to_string())
            .or_insert_with(|| (today, BTreeSet::new()))
            .1
            .insert(policy.to_string());
    }

    pub(crate) fn return_credit(&mut self, tx: &mut Tx<'_>, exception: &str, reason: String) -> Result<(), Reject> {
        tx.need(Role::Reconciler)?;
        let e = self
            .exceptions
            .get(exception)
            .ok_or_else(|| Reject::new("unknown_exception", format!("there is no exception {exception}")))?;
        if !matches!(e.kind, ExceptionKind::UnidentifiedReceipt | ExceptionKind::UnexplainedCredit) || e.amount_minor <= 0 {
            return Err(Reject::new("invalid_return", "only money received can be given back"));
        }
        if reason.trim().is_empty() {
            return Err(Reject::new("invalid_return", "say why it goes back"));
        }
        if self.applied.returned.contains_key(exception) {
            return Err(Reject::new("already_returned", format!("{exception} has been given back")));
        }
        let (account, amount, reference) = (e.account.clone(), e.amount_minor, e.reference.clone());
        let receipt = format!("match-{exception}");
        let policies = self.applied.receipts.get(&receipt).map(|(_, p)| p.clone()).unwrap_or_default();
        for policy in &policies {
            self.push_outbox(OutboxEntry {
                key: format!("{RETURNED}{exception}-{policy}"),
                instruction: Instruction::Core {
                    caller: "cash-matching".to_string(),
                    command: Box::new(ic_core::Command::ReverseReceipt {
                        policy: PolicyId::from(policy.as_str()),
                        receipt: receipt.clone(),
                    }),
                },
                about: Some(About::Item(receipt.clone())),
            });
        }
        self.books.entry(account).or_default().expected.insert(
            reference,
            Expected {
                amount_minor: -amount,
                since: tx.now.date(),
            },
        );
        if let Some(e) = self.exceptions.get_mut(exception).filter(|e| e.resolution.is_none()) {
            e.resolution = Some(format!("given back: {reason}"));
        }
        self.applied.returned.insert(exception.to_string(), reason);
        tx.emit(Event::CreditReturned {
            exception: exception.to_string(),
            policies: policies.into_iter().collect(),
        });
        Ok(())
    }
}
