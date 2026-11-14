//! Bank details of people the customer master has erased.
//!
//! Payee and authority account details are kept here in clear, not under
//! the customer master's per-person key, so its retention shredding does not
//! reach them by itself. The pump asks the customer master about every
//! party this module holds account details for, each one again after
//! `RECHECK_DAYS`; the master answers 410 Gone for a person it has erased.
//! The module then forgets that person here: each payee and authority keeps
//! its ID and history but its account name and number are blanked, payees
//! are closed and authorities cancelled, the account on every finished
//! payment to or from them is blanked, and payer cases, refund patterns,
//! holds and insolvency records about them go. No new payee or authority is
//! taken for them.
//!
//! A payment still in flight keeps its account until the bank has finished
//! with it, so the party stays on the list and the next check finishes the
//! job. What the bank was sent (files kept in the vault) is the vault's to
//! erase by its own retention.

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::aml::digits;
use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{AccountDetails, AuthorityStatus, Counterparty, Reject, Role};

/// How often each party's record is checked with the customer master. A
/// recorded best estimate: shredding comes years after a person's last
/// dealing, so a month's lag is small beside it.
pub const RECHECK_DAYS: i64 = 30;

/// The most parties one pass asks about, oldest check first.
pub const BATCH: usize = 200;

/// Refusal code for a party the customer master has erased.
pub const ERASED: &str = "party_erased";

/// What this module knows of erasure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Erasure {
    /// When each party was last found still on the master's books.
    pub checked: std::collections::BTreeMap<String, NaiveDateTime>,
    /// Parties the master has erased.
    pub erased: BTreeSet<String>,
    /// Erased parties with a payment the bank had not finished with when
    /// they were forgotten: asked about again each day until it has.
    #[serde(default)]
    pub unfinished: BTreeSet<String>,
}

fn blank() -> AccountDetails {
    AccountDetails {
        name: String::new(),
        number: String::new(),
    }
}

fn is_blank(a: &AccountDetails) -> bool {
    a.name.is_empty() && a.number.is_empty()
}

impl PaymentsState {
    /// Refuses a new payee or authority for an erased party.
    pub(crate) fn refuse_if_erased(&self, party: &str) -> Result<(), Reject> {
        if self.erasure.erased.contains(party) {
            return Err(Reject::new(ERASED, format!("the customer master has erased {party}")));
        }
        Ok(())
    }

    /// Parties holding account details here: a payee's or an authority's,
    /// or a payment's still with the bank.
    fn parties_with_details(&self) -> BTreeSet<&String> {
        let payees = self.payees.values().filter(|p| !is_blank(&p.account)).map(|p| &p.party);
        let authorities = self.authorities.values().filter(|a| !is_blank(&a.account)).map(|a| &a.party);
        payees.chain(authorities).chain(&self.erasure.unfinished).collect()
    }

    /// Parties to ask the customer master about now, never asked or asked
    /// longest ago first, at most `BATCH`. An erased party whose payment was
    /// still in flight is asked again each day.
    /// `now` is the asker's clock; the module's own when it gives none.
    pub fn erasure_watch(&self, now: Option<NaiveDateTime>) -> Vec<String> {
        let now = now.or(self.clock).unwrap_or_default();
        let mut due: Vec<(Option<NaiveDateTime>, &String)> = self
            .parties_with_details()
            .into_iter()
            .map(|p| (self.erasure.checked.get(p).copied(), p))
            .filter(|(at, p)| {
                let every = if self.erasure.unfinished.contains(*p) { 1 } else { RECHECK_DAYS };
                at.is_none_or(|at| now - at >= Duration::days(every))
            })
            .collect();
        due.sort();
        due.into_iter().take(BATCH).map(|(_, p)| p.clone()).collect()
    }

    /// What the customer master said about a party: still on its books, or
    /// erased.
    pub(crate) fn party_checked(&mut self, tx: &mut Tx<'_>, party: &str, erased: bool) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if party.is_empty() {
            return Err(Reject::new("invalid_party", "name the party"));
        }
        if !erased {
            self.erasure.checked.insert(party.to_string(), tx.now);
            return Ok(());
        }
        let first = self.erasure.erased.insert(party.to_string());
        self.forget(party);
        self.erasure.checked.insert(party.to_string(), tx.now);
        if first {
            tx.emit(Event::PartyErased { party: party.to_string() });
        }
        Ok(())
    }

    fn forget(&mut self, party: &str) {
        let mut payers: BTreeSet<String> = BTreeSet::from([format!("party:{party}")]);
        let mut counterparties = Vec::new();
        for (id, p) in self.payees.iter_mut().filter(|(_, p)| p.party == party) {
            payers.insert(format!("account:{}", digits(&p.account.number)));
            p.account = blank();
            p.closed = true;
            counterparties.push(Counterparty::Payee(id.clone()));
        }
        for (id, a) in self.authorities.iter_mut().filter(|(_, a)| a.party == party) {
            payers.insert(format!("account:{}", digits(&a.account.number)));
            a.account = blank();
            if a.status != AuthorityStatus::Cancelled {
                a.status = AuthorityStatus::Cancelled;
                a.reason = Some(ERASED.to_string());
            }
            counterparties.push(Counterparty::Authority(id.clone()));
        }
        let mut in_flight = false;
        for item in self.runs.values_mut().flat_map(|r| r.items.values_mut()) {
            if !counterparties.contains(&item.new.counterparty) {
                continue;
            }
            if item.status.terminal() {
                item.account = None;
            } else {
                in_flight |= item.account.is_some();
            }
        }
        if in_flight {
            self.erasure.unfinished.insert(party.to_string());
        } else {
            self.erasure.unfinished.remove(party);
        }
        let checks = &mut self.payer_checks;
        checks.cases.retain(|_, c| c.owner != party && !payers.contains(&c.payer));
        checks.pending.retain(|_, p| !payers.contains(&p.payer));
        checks.refunds.remove(party);
        self.customer_holds.remove(party);
        self.insolvencies.remove(party);
    }
}
