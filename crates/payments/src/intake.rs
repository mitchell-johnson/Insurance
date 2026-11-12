//! Payments other modules have released, waiting for a maker to put them in
//! a run. Claims releases a benefit payment and says so with an event; the
//! module keeps it here, under the reference claims gave it, until it is
//! paid. Nothing here moves money: a waiting payment joins a run like any
//! other item and passes the same checks and approvals.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::{PayableKind, PolicyId};
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{About, Counterparty, Id, Instruction, NewItem, OutboxEntry, Reject, Role};

/// The statement a reinsurer settlement pays, from the source the
/// reinsurance module gave it.
fn statement_number(source: &str) -> Option<u32> {
    source.strip_prefix("reinsurance:statement:")?.parse().ok()
}

/// One released payment that has not yet been settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    /// The releasing module's payment reference. It becomes the item's ID,
    /// the end-to-end reference at the bank, and the `payment` the core is
    /// told has settled.
    pub payment: Id,
    /// What is being paid, in the releasing module's words.
    pub source: String,
    /// Who is paid: a payee's ID here, or the party a payee belongs to.
    /// Empty when the release does not say (a unit-linked payout): the
    /// maker then names the payee when taking it into a run.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub payee: String,
    /// The releasing module's reference to the account it verified. Shown
    /// to the maker; this module pays the account it verified itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account_ref: String,
    /// The policy whose ledger the money settles on, and the payable it
    /// discharges there. Both or neither: a payment to a reinsurer or
    /// another counterparty is not policy money.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PolicyId>,
    /// The claim it pays, when claims released it: claims is told if the
    /// bank refuses it before it settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
    pub amount_minor: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payable: Option<PayableKind>,
}

impl PaymentsState {
    pub(crate) fn offer_payment(&mut self, tx: &mut Tx<'_>, waiting: Waiting) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if waiting.payment.is_empty() || waiting.source.is_empty() || waiting.amount_minor <= 0 || waiting.policy.is_some() != waiting.payable.is_some() {
            return Err(Reject::new(
                "invalid_payment",
                "a released payment has a reference, a source and a positive amount, and names a policy and its payable together or not at all",
            ));
        }
        match self.waiting.get(&waiting.payment) {
            // The same release told twice is one payment.
            Some(held) if *held == waiting => return Ok(()),
            Some(_) => {
                return Err(Reject::new(
                    "payment_conflict",
                    format!("payment {} was released before with different details", waiting.payment),
                ))
            }
            None => {}
        }
        if self.items.contains_key(&waiting.payment) {
            return Err(Reject::new("duplicate_item", format!("item {} has been used before", waiting.payment)));
        }
        tx.emit(Event::PaymentOffered {
            payment: waiting.payment.clone(),
            source: waiting.source.clone(),
            amount_minor: waiting.amount_minor,
        });
        self.waiting.insert(waiting.payment.clone(), waiting);
        Ok(())
    }

    /// The payee a waiting payment is for: the one the maker names, which
    /// must be the payee or belong to the party the release names, if it
    /// names one; otherwise the payee of that ID, or the one payee the party
    /// has. A party with several payees is not guessed at.
    fn payee_for(&self, waiting: &Waiting, chosen: Option<&Id>) -> Result<Id, Reject> {
        if let Some(chosen) = chosen {
            let payee = self
                .payees
                .get(chosen)
                .ok_or_else(|| Reject::new("unknown_payee", format!("payment {}: there is no payee {chosen}", waiting.payment)))?;
            if !(waiting.payee.is_empty() || waiting.payee == *chosen || waiting.payee == payee.party) {
                return Err(Reject::new(
                    "payee_mismatch",
                    format!("payment {} was released to {}, not to {chosen}", waiting.payment, waiting.payee),
                ));
            }
            return Ok(chosen.clone());
        }
        if waiting.payee.is_empty() {
            return Err(Reject::new(
                "payee_required",
                format!("payment {} does not say who is paid: name the payee", waiting.payment),
            ));
        }
        if self.payees.contains_key(&waiting.payee) {
            return Ok(waiting.payee.clone());
        }
        let mut theirs = self.payees.iter().filter(|(_, p)| p.party == waiting.payee).map(|(id, _)| id);
        match (theirs.next(), theirs.next()) {
            (Some(only), None) => Ok(only.clone()),
            (None, _) => Err(Reject::new(
                "unknown_payee",
                format!("payment {}: there is no payee for {}", waiting.payment, waiting.payee),
            )),
            (Some(_), Some(_)) => Err(Reject::new(
                "ambiguous_payee",
                format!("payment {}: {} has more than one payee", waiting.payment, waiting.payee),
            )),
        }
    }

    /// Put waiting payments into a run, as the items they were released as.
    pub(crate) fn take_waiting(&mut self, tx: &mut Tx<'_>, run: &str, payments: BTreeSet<Id>, payees: BTreeMap<Id, Id>) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let mut items = Vec::new();
        for payment in &payments {
            let waiting = self
                .waiting
                .get(payment)
                .filter(|_| !self.items.contains_key(payment))
                .ok_or_else(|| Reject::new("not_waiting", format!("payment {payment} is not waiting to be paid")))?;
            items.push(NewItem {
                item: waiting.payment.clone(),
                source: waiting.source.clone(),
                counterparty: Counterparty::Payee(self.payee_for(waiting, payees.get(payment))?),
                amount_minor: waiting.amount_minor,
                policy: waiting.policy.clone(),
                payable: waiting.payable,
                refund: None,
                represents: None,
            });
        }
        self.add_run_items(tx, run, items)
    }

    /// The bank refused or sent back an item before it settled. Its
    /// reference is spent. If claims released it, claims is told the payment
    /// failed, so the money goes back to benefit payable there and a task is
    /// raised to release it again under a new reference.
    pub(crate) fn failed_before_settlement(&mut self, n: &NewItem) {
        let Some(waiting) = self.waiting.remove(&n.item) else { return };
        if let (Some(claim), Some(policy)) = (waiting.claim, waiting.policy) {
            self.push_outbox(OutboxEntry {
                key: format!("claim-payment-failed-{}", n.item),
                instruction: Instruction::CoreAt {
                    policy,
                    caller: "payments".to_string(),
                    command: Box::new(ic_core::Command::Claims(Box::new(ic_core::claims_module::Command::PaymentReturned {
                        claim,
                        payment_ref: n.item.clone(),
                    }))),
                },
                about: Some(About::Item(n.item.clone())),
            });
        }
    }

    /// A reinsurer settlement is paid: the reinsurance module records it
    /// against the statement, in that module's own command.
    pub(crate) fn origin_settled(&mut self, n: &NewItem, on: NaiveDate) {
        let Some(number) = statement_number(&n.source) else { return };
        self.push_outbox(OutboxEntry {
            key: format!("reinsurance-settled-{}", n.item),
            instruction: Instruction::Module {
                module: "reinsurance".to_string(),
                command: serde_json::json!({
                    "key": format!("payments:settled:{}", n.item),
                    "caller": "payments",
                    "on": on,
                    "command": {"record_settlement": {"number": number, "payment_ref": n.item, "amount_minor": n.amount_minor}},
                }),
            },
            about: Some(About::Item(n.item.clone())),
        });
    }

    /// A reinsurer settlement was refused or sent back by the bank, before
    /// or after it settled: the statement is owed again.
    pub(crate) fn origin_returned(&mut self, n: &NewItem, on: NaiveDate, reason: &str) {
        let Some(number) = statement_number(&n.source) else { return };
        self.push_outbox(OutboxEntry {
            key: format!("reinsurance-returned-{}", n.item),
            instruction: Instruction::Module {
                module: "reinsurance".to_string(),
                command: serde_json::json!({
                    "key": format!("payments:returned:{}:{on}", n.item),
                    "caller": "payments",
                    "on": on,
                    "command": {"settlement_returned": {"number": number, "payment_ref": n.item, "reason": reason}},
                }),
            },
            about: Some(About::Item(n.item.clone())),
        });
    }

    /// Released payments that are in no run.
    pub fn waiting_payments(&self) -> Vec<&Waiting> {
        self.waiting.values().filter(|w| !self.items.contains_key(&w.payment)).collect()
    }
}
