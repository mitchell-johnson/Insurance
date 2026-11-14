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
//!
//! A policy loan is repaid the same way. While the core says anything is
//! owed on a policy's loan (`PolicyLoanDrawn`, `PolicyLoanRepaid`), the loan
//! has a reference `LN` and six digits (question `loan_repayment_reference
//! {policy}`), and a credit quoting it is sent to the core as cash
//! matching's `LoanCommand::Repay`. Interest accrues daily, so the core, not
//! this module, decides whether the amount is more than is owed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::machine::{PaymentsState, Tx};
use crate::model::{Reject, Role};

/// Outbox key prefix of a repayment sent to claims.
pub(crate) const REPAID: &str = "claims-repaid-";

/// Outbox key prefix of a policy loan repayment sent to the core.
pub(crate) const LOAN_REPAID: &str = "loan-repaid-";

/// An overpayment or a policy loan still owed, under its payment
/// reference. A loan has no claim, and its `overpayment` is `loan:{policy}`.
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
    /// Hold a reference for what is owed while anything is, under the
    /// prefix given; forget it once nothing is.
    fn hold(&mut self, prefix: &str, owed: Repayable) {
        let key = owed.overpayment.clone();
        if owed.outstanding_minor <= 0 {
            if let Some(reference) = self.references.remove(&key) {
                self.by_reference.remove(&reference);
            }
            return;
        }
        let reference = match self.references.get(&key) {
            Some(reference) => reference.clone(),
            None => {
                self.next += 1;
                let reference = format!("{prefix}{:06}", self.next);
                self.references.insert(key, reference.clone());
                reference
            }
        };
        self.by_reference.insert(reference, owed);
    }

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
        let owed = Repayable {
            claim: claim.to_string(),
            overpayment: overpayment.to_string(),
            policy: policy.to_string(),
            outstanding_minor,
        };
        self.repayables.hold("OP", owed);
        Ok(())
    }

    /// The core changed what is owed on a policy's loan.
    pub(crate) fn loan_changed(&mut self, tx: &mut Tx<'_>, policy: &str, outstanding_minor: i64) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if policy.is_empty() {
            return Err(Reject::new("invalid_loan", "a loan names its policy"));
        }
        let owed = Repayable {
            claim: String::new(),
            overpayment: format!("loan:{policy}"),
            policy: policy.to_string(),
            outstanding_minor,
        };
        self.repayables.hold("LN", owed);
        Ok(())
    }
}
