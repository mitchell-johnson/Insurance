//! Direct debit authorities: permission to collect a policy's premium from
//! a bank account.

use ic_core::PolicyId;

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{AccountDetails, Authority, AuthorityStatus, Id, Instruction, OutboxEntry, Reject, Role};

pub(crate) fn check_account(account: &AccountDetails) -> Result<(), Reject> {
    let digits = account.number.chars().filter(char::is_ascii_digit).count();
    let shape = account.number.chars().all(|c| c.is_ascii_digit() || c == '-');
    if account.name.trim().is_empty() || !shape || !(8..=20).contains(&digits) {
        return Err(Reject::new("invalid_account", "an account has a name and a number of 8 to 20 digits"));
    }
    Ok(())
}

impl PaymentsState {
    fn authority_mut(&mut self, id: &str) -> Result<&mut Authority, Reject> {
        self.authorities
            .get_mut(id)
            .ok_or_else(|| Reject::new("unknown_authority", format!("there is no authority {id}")))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create_authority(
        &mut self,
        tx: &mut Tx<'_>,
        id: Id,
        policy: PolicyId,
        party: String,
        life: Option<ic_core::LifeId>,
        account: AccountDetails,
        sole_name: bool,
    ) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        check_account(&account)?;
        if self.authorities.contains_key(&id) {
            return Err(Reject::new("duplicate_authority", format!("authority {id} already exists")));
        }
        if self.blocked_accounts.contains(&account.number) {
            return Err(Reject::new("account_blocked", "that account was in the sole name of a payer who has died"));
        }
        self.authorities.insert(
            id.clone(),
            Authority {
                policy: policy.clone(),
                party,
                life,
                account,
                sole_name,
                status: AuthorityStatus::Active,
                reason: None,
                dishonours: 0,
            },
        );
        tx.emit(Event::AuthorityCreated { authority: id, policy });
        Ok(())
    }

    /// The scheme member an item was collected from, if its authority is a
    /// member's own.
    pub(crate) fn member_collected_from(&self, item: &crate::model::NewItem) -> Option<ic_core::LifeId> {
        match &item.counterparty {
            crate::model::Counterparty::Authority(a) => self.authorities.get(a)?.life.clone(),
            _ => None,
        }
    }

    pub(crate) fn suspend_authority(&mut self, tx: &mut Tx<'_>, id: &str, reason: String) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        let a = self.authority_mut(id)?;
        if a.status != AuthorityStatus::Active {
            return Err(Reject::new("authority_not_active", format!("authority {id} is not active")));
        }
        a.status = AuthorityStatus::Suspended;
        a.reason = Some(reason.clone());
        tx.emit(Event::AuthoritySuspended {
            authority: id.to_string(),
            reason,
        });
        Ok(())
    }

    pub(crate) fn reinstate_authority(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        let a = self.authority_mut(id)?;
        if a.status != AuthorityStatus::Suspended {
            return Err(Reject::new("authority_not_suspended", format!("authority {id} is not suspended")));
        }
        a.status = AuthorityStatus::Active;
        a.reason = None;
        a.dishonours = 0;
        tx.emit(Event::AuthorityReinstated { authority: id.to_string() });
        Ok(())
    }

    pub(crate) fn cancel_authority(&mut self, tx: &mut Tx<'_>, id: &str, reason: String) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        if self.authority_mut(id)?.status == AuthorityStatus::Cancelled {
            return Err(Reject::new("authority_cancelled", format!("authority {id} is already cancelled")));
        }
        self.end_authority(tx, id, &reason);
        Ok(())
    }

    /// Cancel an authority that exists and is not yet cancelled.
    pub(crate) fn end_authority(&mut self, tx: &mut Tx<'_>, id: &str, reason: &str) {
        if let Some(a) = self.authorities.get_mut(id) {
            a.status = AuthorityStatus::Cancelled;
            a.reason = Some(reason.to_string());
            tx.emit(Event::AuthorityCancelled {
                authority: id.to_string(),
                policy: a.policy.clone(),
                reason: reason.to_string(),
            });
        }
    }

    pub(crate) fn payer_deceased(&mut self, tx: &mut Tx<'_>, policy: &PolicyId, claim: &str) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let live = |a: &Authority| a.status != AuthorityStatus::Cancelled;
        let on_policy: Vec<Id> = self
            .authorities
            .iter()
            .filter(|(_, a)| &a.policy == policy && live(a))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &on_policy {
            let a = &self.authorities[id];
            if a.sole_name && self.blocked_accounts.insert(a.account.number.clone()) {
                tx.emit(Event::AccountBlocked { authority: id.clone() });
            }
            self.end_authority(tx, id, "payer_deceased");
        }
        // The same account may pay for other policies. The core cancelled the
        // authority on this one only; the others stop here, and someone is
        // told to arrange another way to pay.
        let elsewhere: Vec<Id> = self
            .authorities
            .iter()
            .filter(|(_, a)| live(a) && self.blocked_accounts.contains(&a.account.number))
            .map(|(id, _)| id.clone())
            .collect();
        for id in elsewhere {
            self.end_authority(tx, &id, "account_holder_deceased");
            let other = self.authorities[&id].policy.clone();
            self.push_outbox(OutboxEntry {
                key: format!("deceased-{claim}-{id}"),
                instruction: Instruction::Notify {
                    to: Role::PayeeMaintainer,
                    subject: "payment_method_needed".to_string(),
                    detail: format!("policy {other} was paid from an account whose holder has died (claim {claim})"),
                },
                about: None,
            });
        }
        self.reroute_refunds_after_death(tx, policy, claim);
        Ok(())
    }
}
