//! Accounts the module may pay, and the controls each one passes first:
//! the bank's name check, confirmation through a second channel by a second
//! person, sanctions screening, legal's approval where the payee stands in
//! for the person owed, and a cooling-off period on new details.

use chrono::{Duration, NaiveDateTime};

use crate::authority::check_account;
use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{AccountDetails, ForeignAccount, Id, NameCheck, Payee, PayeeKind, Reject, Role};

impl Payee {
    /// Why this account cannot be paid at `now`, if it cannot.
    pub fn blocked(&self, now: NaiveDateTime) -> Option<&'static str> {
        if self.closed {
            Some("payee_closed")
        } else if self.suspect {
            Some("payee_account_returned")
        } else if self.name_check == NameCheck::NoMatch {
            Some("payee_name_mismatch")
        } else if self.confirmed_by.is_none() {
            Some("payee_not_confirmed")
        } else if self.screened_clear != Some(true) {
            Some("payee_not_screened")
        } else if self.kind.needs_legal() && self.legal_approved_by.is_none() {
            Some("payee_needs_legal_approval")
        } else if self.cooling_off_until.is_some_and(|until| now < until) {
            Some("payee_cooling_off")
        } else {
            None
        }
    }
}

/// A payee's account: a New Zealand one, or one abroad with what the bank
/// needs to reach it.
fn check_payee_account(account: &AccountDetails, foreign: Option<&ForeignAccount>) -> Result<(), Reject> {
    let Some(f) = foreign else { return check_account(account) };
    let alnum = |s: &str, len: std::ops::RangeInclusive<usize>| len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let upper = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_uppercase());
    let shaped = !account.name.trim().is_empty()
        && alnum(&account.number, 5..=34)
        && alnum(&f.bic, 8..=11)
        && matches!(f.bic.len(), 8 | 11)
        && upper(&f.country, 2)
        && f.country != "NZ"
        && upper(&f.currency, 3)
        && !f.address.trim().is_empty();
    if !shaped {
        return Err(Reject::new(
            "invalid_account",
            "an account abroad has a name, a number (or IBAN) of 5 to 34 capitals and digits, a BIC of 8 or 11, a country other than NZ, a currency and an address",
        ));
    }
    Ok(())
}

impl PaymentsState {
    fn payee_mut(&mut self, id: &str) -> Result<&mut Payee, Reject> {
        match self.payees.get_mut(id) {
            Some(p) if !p.closed => Ok(p),
            Some(_) => Err(Reject::new("payee_closed", format!("payee {id} is closed"))),
            None => Err(Reject::new("unknown_payee", format!("there is no payee {id}"))),
        }
    }

    fn cooling_off(&self, tx: &Tx<'_>) -> Result<NaiveDateTime, Reject> {
        Ok(tx.now + Duration::hours(i64::from(self.cfg()?.rules.cooling_off_hours)))
    }

    pub(crate) fn register_payee(
        &mut self,
        tx: &mut Tx<'_>,
        id: Id,
        party: String,
        kind: PayeeKind,
        account: AccountDetails,
        name_check: NameCheck,
        foreign: Option<ForeignAccount>,
    ) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        check_payee_account(&account, foreign.as_ref())?;
        self.refuse_if_erased(&party)?;
        self.refuse_if_held(&party, tx.now.date())?;
        if self.payees.contains_key(&id) {
            return Err(Reject::new("duplicate_payee", format!("payee {id} already exists")));
        }
        // A first account for a customer has nothing to be swapped for. An
        // account for someone standing in for the person owed always waits.
        let cooling_off_until = if kind.needs_legal() { Some(self.cooling_off(tx)?) } else { None };
        self.payees.insert(
            id.clone(),
            Payee {
                party,
                kind,
                account,
                name_check,
                maintained_by: tx.actor.id.clone(),
                confirmed_by: None,
                screened_clear: None,
                legal_approved_by: None,
                cooling_off_until,
                suspect: false,
                closed: false,
                foreign,
            },
        );
        if kind.needs_legal() {
            self.ask_legal(&id);
        }
        tx.emit(Event::PayeeRegistered { payee: id, kind });
        Ok(())
    }

    /// Legal's worklist: an account for someone standing in for the person
    /// owed waits for legal's approval, and legal is told so on its console.
    fn ask_legal(&mut self, id: &str) {
        let Some(p) = self.payees.get(id) else { return };
        let detail = format!(
            "payee {id} for {} ({}) waits for legal's approval of its authority before it can be paid: open the payee and approve it",
            p.party,
            serde_json::to_value(p.kind)
                .ok()
                .and_then(|k| k.as_str().map(str::to_string))
                .unwrap_or_default()
        );
        self.raise_notice(Role::Legal, "payee_needs_legal_approval", &detail);
    }

    pub(crate) fn change_payee_account(&mut self, tx: &mut Tx<'_>, id: &str, account: AccountDetails, name_check: NameCheck) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        let foreign = self.payees.get(id).and_then(|p| p.foreign.clone());
        check_payee_account(&account, foreign.as_ref())?;
        let until = self.cooling_off(tx)?;
        let by = tx.actor.id.clone();
        let party = self.payee_mut(id)?.party.clone();
        self.refuse_if_held(&party, tx.now.date())?;
        let p = self.payee_mut(id)?;
        p.account = account;
        p.name_check = name_check;
        p.maintained_by = by;
        p.confirmed_by = None;
        p.screened_clear = None;
        p.legal_approved_by = None;
        p.cooling_off_until = Some(until);
        p.suspect = false;
        if p.kind.needs_legal() {
            self.ask_legal(id);
        }
        tx.emit(Event::PayeeAccountChanged {
            payee: id.to_string(),
            cooling_off_until: until,
        });
        Ok(())
    }

    pub(crate) fn confirm_payee(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        let by = tx.actor.id.clone();
        let p = self.payee_mut(id)?;
        if p.maintained_by == by {
            return Err(Reject::new("same_person", "the person who entered the details cannot confirm them"));
        }
        if p.name_check == NameCheck::NoMatch {
            return Err(Reject::new("payee_name_mismatch", "the bank says the account is in another name"));
        }
        p.confirmed_by = Some(by.clone());
        tx.emit(Event::PayeeConfirmed { payee: id.to_string(), by });
        Ok(())
    }

    pub(crate) fn screen_payee(&mut self, tx: &mut Tx<'_>, id: &str, clear: bool) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        self.payee_mut(id)?.screened_clear = Some(clear);
        tx.emit(Event::PayeeScreened { payee: id.to_string(), clear });
        Ok(())
    }

    pub(crate) fn approve_payee(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::Legal)?;
        let by = tx.actor.id.clone();
        let p = self.payee_mut(id)?;
        if !p.kind.needs_legal() {
            return Err(Reject::new("approval_not_needed", "only an estate or a liquidator needs legal's approval"));
        }
        if p.maintained_by == by {
            return Err(Reject::new("same_person", "the person who entered the details cannot approve them"));
        }
        p.legal_approved_by = Some(by.clone());
        // The notice that asked for it is done with.
        let asked = format!("payee {id} for ");
        for n in self.notices.items.values_mut() {
            if n.to == Role::Legal && n.closed.is_none() && n.subject == "payee_needs_legal_approval" && n.detail.starts_with(&asked) {
                n.closed = Some((by.clone(), tx.now, "approved the payee".to_string()));
            }
        }
        tx.emit(Event::PayeeApproved { payee: id.to_string(), by });
        Ok(())
    }

    pub(crate) fn close_payee(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::PayeeMaintainer)?;
        self.payee_mut(id)?.closed = true;
        tx.emit(Event::PayeeClosed { payee: id.to_string() });
        Ok(())
    }
}
