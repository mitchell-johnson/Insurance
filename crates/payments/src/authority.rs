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

/// Whether a New Zealand account number's check digits hold, by the
/// algorithm the bank uses (Payments NZ's account number validation): the
/// weights over bank (2), branch (4), account (8) and suffix (4) digits,
/// the modulus, and whether each product's digits are added (twice).
fn nz_check_digits(bank: u32, digits: &[u32; 18], account_base: u32) -> bool {
    let (weights, modulus, fold): ([u32; 18], u32, bool) = match bank {
        8 => ([0, 0, 0, 0, 0, 0, 0, 7, 6, 5, 4, 3, 2, 1, 0, 0, 0, 0], 11, false),
        9 => ([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 4, 3, 2, 0, 0, 0, 1], 10, true),
        25 | 33 => ([0, 0, 0, 0, 0, 0, 0, 1, 7, 3, 1, 7, 3, 1, 0, 0, 0, 0], 10, false),
        26 | 28 | 29 => ([0, 0, 0, 0, 0, 0, 0, 1, 3, 7, 1, 3, 7, 1, 0, 3, 7, 1], 10, true),
        // Bank 31 publishes no check.
        31 => ([0; 18], 1, false),
        _ if account_base < 990_000 => ([0, 0, 6, 3, 7, 9, 0, 0, 10, 5, 8, 4, 2, 1, 0, 0, 0, 0], 11, false),
        _ => ([0, 0, 0, 0, 0, 0, 0, 0, 10, 5, 8, 4, 2, 1, 0, 0, 0, 0], 11, false),
    };
    let add_digits = |n: u32| n / 10 + n % 10;
    let total: u32 = digits
        .iter()
        .zip(weights)
        .map(|(d, w)| if fold { add_digits(add_digits(d * w)) } else { d * w })
        .sum();
    total.is_multiple_of(modulus)
}

/// A direct debit is collected from a New Zealand account: bank, branch,
/// account and suffix, whose check digits must hold. A digit missing or
/// mistyped is refused here, beside the field, not by the bank a week later.
pub(crate) fn check_nz_account(account: &AccountDetails) -> Result<(), Reject> {
    const FORM: &str = "a New Zealand account is written bank-branch-account-suffix, such as 12-3140-0123456-00: 2, 4, 7 (or 8) and 2 or 3 digits";
    if account.name.trim().is_empty() {
        return Err(Reject::new("invalid_account", "an account has the name it is held in"));
    }
    let parts: Vec<&str> = account.number.split(['-', ' ']).filter(|p| !p.is_empty()).collect();
    let numeric = parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit()));
    let [bank, branch, base, suffix] = parts.as_slice() else {
        return Err(Reject::new("invalid_account", FORM));
    };
    if !numeric || bank.len() != 2 || branch.len() != 4 || !(2..=4).contains(&suffix.len()) {
        return Err(Reject::new("invalid_account", FORM));
    }
    if !(7..=8).contains(&base.len()) {
        return Err(Reject::new(
            "invalid_account",
            format!(
                "the account part {base} has {} digits, and a New Zealand account part has 7 (or 8): check the number with the payer",
                base.len()
            ),
        ));
    }
    let padded = format!("{bank}{branch}{base:0>8}{suffix:0>4}");
    let mut digits = [0u32; 18];
    for (slot, c) in digits.iter_mut().zip(padded.chars()) {
        *slot = c.to_digit(10).unwrap_or(0);
    }
    let bank_code: u32 = bank.parse().unwrap_or(0);
    let account_base: u32 = base.parse().unwrap_or(0);
    if !nz_check_digits(bank_code, &digits, account_base) {
        return Err(Reject::new(
            "invalid_account",
            format!(
                "{} fails the bank's check digits: a digit is wrong or missing, so check the number with the payer",
                account.number
            ),
        ));
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
        check_nz_account(&account)?;
        self.refuse_if_erased(&party)?;
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
                hardship: false,
                hardship_until: None,
                maintained_by: Some(tx.actor.id.clone()),
                confirmed_by: None,
                cooling_off_until: None,
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

    /// A second payee maintainer confirms the account with the payer.
    pub(crate) fn confirm_authority(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        let by = tx.actor.id.clone();
        let a = self.authority_mut(id)?;
        if a.status == AuthorityStatus::Cancelled {
            return Err(Reject::new("authority_cancelled", format!("authority {id} is cancelled")));
        }
        if !a.unconfirmed() {
            return Err(Reject::new("already_confirmed", format!("authority {id} has nothing waiting to be confirmed")));
        }
        if a.maintained_by.as_deref() == Some(by.as_str()) {
            return Err(Reject::new(
                "same_person",
                "the person who entered the account cannot confirm it: another payee maintainer confirms it with the payer",
            ));
        }
        a.confirmed_by = Some(by.clone());
        tx.emit(Event::AuthorityConfirmed { authority: id.to_string(), by });
        Ok(())
    }

    /// The payer collects from another account. It waits for a second
    /// person's confirmation and the cooling-off period, as a payee's does.
    pub(crate) fn change_authority_account(&mut self, tx: &mut Tx<'_>, id: &str, account: AccountDetails) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        check_nz_account(&account)?;
        if self.blocked_accounts.contains(&account.number) {
            return Err(Reject::new("account_blocked", "that account was in the sole name of a payer who has died"));
        }
        let hours = i64::from(self.cfg()?.rules.cooling_off_hours);
        let until = tx.now + chrono::Duration::hours(hours);
        let by = tx.actor.id.clone();
        let a = self.authority_mut(id)?;
        if a.status == AuthorityStatus::Cancelled {
            return Err(Reject::new(
                "authority_cancelled",
                format!("authority {id} is cancelled: the payer gives a new authority"),
            ));
        }
        a.account = account;
        a.maintained_by = Some(by);
        a.confirmed_by = None;
        a.cooling_off_until = Some(until);
        tx.emit(Event::AuthorityAccountChanged {
            authority: id.to_string(),
            cooling_off_until: until,
        });
        Ok(())
    }

    pub(crate) fn mark_hardship(&mut self, tx: &mut Tx<'_>, id: &str, hardship: bool, until: Option<chrono::NaiveDate>) -> Result<(), Reject> {
        if !tx.actor.roles.contains(&Role::Collections) {
            tx.need(Role::PayeeMaintainer)?;
        }
        if hardship && until.is_some_and(|u| u < tx.now.date()) {
            return Err(Reject::new("hardship_ended", "a hardship mark ends today or later"));
        }
        let a = self.authority_mut(id)?;
        a.hardship = hardship;
        a.hardship_until = until.filter(|_| hardship);
        tx.emit(Event::HardshipMarked {
            authority: id.to_string(),
            hardship,
        });
        Ok(())
    }

    /// A dishonour on an authority in hardship: nothing is presented again
    /// until someone has talked to the payer, and collections is asked to
    /// call and offer hardship help.
    pub(crate) fn hold_for_conversation(&mut self, tx: &mut Tx<'_>, item: &str, authority: &str) {
        self.awaiting_conversation.insert(item.to_string(), authority.to_string());
        let policy = self.authorities.get(authority).map(|a| a.policy.to_string()).unwrap_or_default();
        self.push_outbox(OutboxEntry {
            key: format!("hardship-call-{item}"),
            instruction: Instruction::Notify {
                to: Role::Collections,
                subject: "hardship_conversation".to_string(),
                detail: format!("collection {item} on policy {policy} was dishonoured; the payer is in hardship: call, offer hardship help, and record the outcome before anything is presented again"),
            },
            about: None,
        });
        tx.emit(Event::ConversationNeeded {
            item: item.to_string(),
            authority: authority.to_string(),
        });
    }

    pub(crate) fn record_conversation(&mut self, tx: &mut Tx<'_>, item: &str, outcome: crate::model::ConversationOutcome, note: String) -> Result<(), Reject> {
        tx.need(Role::Collections)?;
        if note.trim().is_empty() {
            return Err(Reject::new("no_note", "a conversation is recorded with what was said"));
        }
        let authority = self
            .awaiting_conversation
            .remove(item)
            .ok_or_else(|| Reject::new("no_conversation_due", format!("collection {item} is not waiting for a conversation")))?;
        let active = self.authorities.get(&authority).is_some_and(|a| a.status == AuthorityStatus::Active);
        if outcome == crate::model::ConversationOutcome::Represent && active {
            self.represent.insert(item.to_string());
        }
        tx.emit(Event::ConversationRecorded {
            item: item.to_string(),
            outcome,
        });
        Ok(())
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
        // Nothing is collected again until a second person has confirmed it
        // with the payer.
        a.maintained_by = Some(tx.actor.id.clone());
        a.confirmed_by = None;
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

    /// A scheme member's own collection ended in the core. Their authorities
    /// on the policy end with the core's reason; anyone else's, the
    /// employer's included, are left alone. Said twice, it changes nothing.
    pub(crate) fn member_collection_ended(&mut self, tx: &mut Tx<'_>, policy: &PolicyId, life: &ic_core::LifeId, reason: &str) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let ending: Vec<Id> = self
            .authorities
            .iter()
            .filter(|(_, a)| &a.policy == policy && a.life.as_ref() == Some(life) && a.status != AuthorityStatus::Cancelled)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ending {
            self.end_authority(tx, &id, reason);
        }
        Ok(())
    }
}
