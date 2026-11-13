//! Money paid back against a claim overpayment.
//!
//! Claims publishes every change to an overpayment with what is still owed
//! (`overpayment_raised`, `overpayment_recovered` and the rest, each with
//! `outstanding_minor`). While something is owed, this module holds a short
//! payment reference for it (`OP` and six digits, inside the twelve
//! characters a New Zealand bank reference carries), which the letter asking
//! for the money quotes (question `repayment_reference {overpayment}`).
//!
//! A credit on any of the insurer's accounts quoting that reference, for no
//! more than is owed, is sent to claims as `RecordRepayment` through the
//! claim's policy; the core then moves the receivable to cash. Until claims
//! records it the money is an unidentified receipt, and it stays one for a
//! person if claims refuses it or it is more than is owed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::machine::{PaymentsState, Tx};
use crate::model::{Reject, Role};

/// Outbox key prefix of a repayment sent to claims.
pub(crate) const REPAID: &str = "claims-repaid-";

/// An overpayment still owed, under its payment reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repayable {
    pub claim: String,
    pub overpayment: String,
    pub policy: String,
    pub outstanding_minor: i64,
}

/// What is owed, by payment reference, and each overpayment's reference.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repayables {
    pub next: u64,
    pub by_reference: BTreeMap<String, Repayable>,
    pub references: BTreeMap<String, String>,
}

/// The words of a reference as a payer types it, upper case, and each
/// word run together with the next ("OP 000123" is OP000123).
fn words(reference: &str) -> Vec<String> {
    let upper = reference.to_ascii_uppercase();
    let single: Vec<&str> = upper.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let pairs = single.windows(2).map(|w| w.concat());
    single.iter().map(|w| w.to_string()).chain(pairs).collect()
}

impl Repayables {
    /// The overpayment a credit's reference names, if it names exactly one
    /// of the references held.
    pub fn named_by(&self, reference: &str) -> Option<&Repayable> {
        let said = words(reference);
        let mut named = said.iter().filter_map(|w| self.by_reference.get(w));
        let first = named.next()?;
        named.all(|o| o == first).then_some(first)
    }
}

impl PaymentsState {
    /// Claims changed an overpayment: hold a reference while anything is
    /// owed on it, and forget it once nothing is.
    pub(crate) fn overpayment_changed(&mut self, tx: &mut Tx<'_>, claim: &str, overpayment: &str, policy: &str, outstanding_minor: i64) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if claim.is_empty() || overpayment.is_empty() || policy.is_empty() {
            return Err(Reject::new("invalid_overpayment", "an overpayment names its claim and policy"));
        }
        let book = &mut self.repayables;
        if outstanding_minor <= 0 {
            if let Some(reference) = book.references.remove(overpayment) {
                book.by_reference.remove(&reference);
            }
            return Ok(());
        }
        let reference = match book.references.get(overpayment) {
            Some(reference) => reference.clone(),
            None => {
                book.next += 1;
                let reference = format!("OP{:06}", book.next);
                book.references.insert(overpayment.to_string(), reference.clone());
                reference
            }
        };
        book.by_reference.insert(
            reference,
            Repayable {
                claim: claim.to_string(),
                overpayment: overpayment.to_string(),
                policy: policy.to_string(),
                outstanding_minor,
            },
        );
        Ok(())
    }
}
