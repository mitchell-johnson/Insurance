//! Premium refunds. A refund goes back to the account the premium came
//! from. Where the payer has died, or that account was in the deceased's
//! sole name, it is paid with the claim to the estate or the beneficiary.

use ic_core::PolicyId;
use ic_money::{Currency, Money};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{
    About, AuthorityStatus, Exception, ExceptionKind, Id, Instruction, ItemStatus, OutboxEntry, Refund, RefundDestination, RefundStatus, Reject, Role,
    RunStatus,
};

impl PaymentsState {
    fn core(&mut self, key: String, command: ic_core::Command, about: About) {
        self.push_outbox(OutboxEntry {
            key,
            instruction: Instruction::Core {
                caller: "payments".to_string(),
                command: Box::new(command),
            },
            about: Some(about),
        });
    }

    pub(crate) fn request_refund(
        &mut self,
        tx: &mut Tx<'_>,
        id: Id,
        policy: PolicyId,
        amount_minor: i64,
        currency: Currency,
        destination: RefundDestination,
    ) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        if self.refunds.contains_key(&id) {
            return Err(Reject::new("duplicate_refund", format!("refund {id} already exists")));
        }
        if amount_minor <= 0 {
            return Err(Reject::new("invalid_amount", "a refund is a positive amount"));
        }
        let with_claim_only = || Reject::new("refund_with_claim", "the payer has died: the refund is paid with the claim");
        let with_claim = match &destination {
            RefundDestination::WithClaim { claim, payee } => {
                let kind = self.payees.get(payee).map(|p| p.kind);
                if !matches!(kind, Some(crate::model::PayeeKind::Estate | crate::model::PayeeKind::Beneficiary)) {
                    return Err(Reject::new("wrong_destination", "a refund with a claim goes to the estate or a beneficiary"));
                }
                Some(claim.clone())
            }
            _ if self.deceased_payers.contains_key(&policy) => return Err(with_claim_only()),
            RefundDestination::Source { authority } => {
                let a = self.authorities.get(authority).filter(|a| a.policy == policy);
                let a = a.ok_or_else(|| Reject::new("unknown_authority", format!("policy {policy} has no authority {authority}")))?;
                if self.blocked_accounts.contains(&a.account.number) {
                    return Err(with_claim_only());
                }
                if a.status == AuthorityStatus::Cancelled {
                    return Err(Reject::new(
                        "source_unavailable",
                        "the source account can no longer be used: name a verified account",
                    ));
                }
                None
            }
            RefundDestination::Payee { payee } => {
                let Some(account) = self.payees.get(payee) else {
                    return Err(Reject::new("unknown_payee", format!("there is no payee {payee}")));
                };
                // Money goes back to where it came from while that is possible.
                if self.authorities.values().any(|a| a.policy == policy && a.status != AuthorityStatus::Cancelled) {
                    return Err(Reject::new("refund_to_source", "the policy has a source account: the refund goes back to it"));
                }
                // A refund is the owner's money: it goes only to the owner's own account.
                match self.payer_checks.owners.get(&policy.to_string()) {
                    None => {
                        return Err(Reject::new(
                            "owner_unknown",
                            format!("payments has not been told who owns policy {policy}, so it cannot tell whose account payee {payee} is: the refund waits until the core has said"),
                        ));
                    }
                    Some(owner) if *owner != account.party => {
                        return Err(Reject::new(
                            "not_owners_account",
                            format!("payee {payee} is another person's account, not the owner's of policy {policy}: a refund goes only to the owner's own verified account"),
                        ));
                    }
                    Some(_) => {}
                }
                None
            }
        };
        self.refunds.insert(
            id.clone(),
            Refund {
                policy: policy.clone(),
                amount_minor,
                currency,
                destination,
                status: RefundStatus::Requested,
            },
        );
        // A refund paid to an estate with a claim is not the owner taking
        // money back (VUL-020).
        let by_owner = with_claim.is_none();
        let command = ic_core::Command::RaiseRefundPayable {
            policy: policy.clone(),
            refund: id.clone(),
            amount: Money::new(amount_minor, currency),
            with_claim,
        };
        self.core(format!("refund-{id}-raise"), command, About::RefundRaise(id.clone()));
        if by_owner {
            self.refund_asked(tx, &policy.to_string());
        }
        tx.emit(Event::RefundRequested {
            refund: id,
            policy,
            amount_minor,
        });
        Ok(())
    }

    /// Give an owed refund back to suspense.
    fn start_release(&mut self, tx: &mut Tx<'_>, id: &str) {
        let Some(refund) = self.refunds.get_mut(id) else { return };
        refund.status = RefundStatus::Releasing;
        let command = ic_core::Command::ReleaseRefundPayable {
            policy: refund.policy.clone(),
            refund: id.to_string(),
        };
        self.core(format!("refund-{id}-release"), command, About::RefundRelease(id.to_string()));
        tx.emit(Event::RefundReleasing { refund: id.to_string() });
    }

    pub(crate) fn release_refund(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let refund = self
            .refunds
            .get(id)
            .ok_or_else(|| Reject::new("unknown_refund", format!("there is no refund {id}")))?;
        if refund.status != RefundStatus::Raised {
            return Err(Reject::new("refund_not_owed", "only a refund that is owed and not in a run can be released"));
        }
        self.start_release(tx, id);
        Ok(())
    }

    /// The payer of this policy has died. Refunds on their way to anyone but
    /// the claim's payee stop, and the claim is told to carry them.
    pub(crate) fn reroute_refunds_after_death(&mut self, tx: &mut Tx<'_>, policy: &PolicyId, claim: &str) {
        self.deceased_payers.insert(policy.clone(), claim.to_string());
        let affected: Vec<(Id, RefundStatus)> = self
            .refunds
            .iter()
            .filter(|(_, r)| &r.policy == policy && !matches!(r.destination, RefundDestination::WithClaim { .. }))
            .map(|(id, r)| (id.clone(), r.status.clone()))
            .collect();
        for (id, status) in affected {
            match status {
                // The core has not answered yet. If it raises the refund,
                // the answer releases it (see `refund_raised`).
                RefundStatus::Requested => continue,
                RefundStatus::Raised => self.start_release(tx, &id),
                RefundStatus::InRun(item) => {
                    let run_id = self.items.get(&item).cloned().unwrap_or_default();
                    let unapproved = self
                        .runs
                        .get(&run_id)
                        .is_some_and(|r| matches!(r.status, RunStatus::Prepared | RunStatus::Checked));
                    let unsent = self.item(&item).is_some_and(|i| i.status == ItemStatus::Pending);
                    if unapproved || unsent {
                        self.drop_item(&run_id, &item);
                        self.start_release(tx, &id);
                    } else {
                        // It is with the bank. Someone must chase it.
                        let amount_minor = self.refunds[&id].amount_minor;
                        let account = self
                            .item(&item)
                            .and_then(|i| i.submission.clone())
                            .and_then(|s| self.submissions.get(&s))
                            .map(|s| s.account.clone());
                        self.open_exception(
                            tx,
                            format!("deceased-{item}"),
                            Exception {
                                kind: ExceptionKind::PaidToBlockedAccount,
                                account: account.unwrap_or_default(),
                                amount_minor,
                                reference: item,
                                opened_on: tx.now.date(),
                                owner: None,
                                resolution: None,
                            },
                        );
                        continue;
                    }
                }
                _ => continue,
            }
            tx.emit(Event::RefundRerouted {
                refund: id,
                claim: claim.to_string(),
            });
        }
    }

    /// The core's answer to a request to raise a refund.
    pub(crate) fn refund_raised(&mut self, tx: &mut Tx<'_>, id: &str, refused: Option<String>) {
        let Some(refund) = self.refunds.get_mut(id) else { return };
        match refused {
            Some(code) => {
                refund.status = RefundStatus::Refused(code.clone());
                tx.emit(Event::RefundRefused { refund: id.to_string(), code });
            }
            None => {
                refund.status = RefundStatus::Raised;
                let reroute = !matches!(refund.destination, RefundDestination::WithClaim { .. });
                let claim = self.deceased_payers.get(&refund.policy).cloned();
                tx.emit(Event::RefundRaised { refund: id.to_string() });
                // The payer died between the request and the answer.
                if let (true, Some(claim)) = (reroute, claim) {
                    self.start_release(tx, id);
                    tx.emit(Event::RefundRerouted { refund: id.to_string(), claim });
                }
            }
        }
    }

    /// The core's answer to a request to release a refund.
    pub(crate) fn refund_released(&mut self, tx: &mut Tx<'_>, id: &str) {
        if let Some(refund) = self.refunds.get_mut(id) {
            refund.status = RefundStatus::Released;
            tx.emit(Event::RefundReleased { refund: id.to_string() });
        }
    }
}
