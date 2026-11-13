//! A hold a customer sets on their own money going out (SVC-022, decision 3
//! in `docs/decisions.md`).
//!
//! The customer master owns the hold: the person places it, asks for it to
//! end under fresh verification, waits out the cooling-off and can change
//! their mind there; only legal lifts it at once (`docs/customers.md`, "A
//! person's own spending hold"). This module enforces it. The pump asks the
//! master about every party that money of this kind is about to go to, and
//! records the answer here (`record_customer_hold`, service role).
//!
//! While a party's hold stands, a refund, or a surrender, withdrawal or
//! policy loan payout, to any payee or authority of that party is refused
//! when it joins a run and held (`customer_hold`) if it is already in one
//! when the run is checked; a payee cannot be registered for the party nor a
//! payee's account changed. Premium collection and claim payments are not
//! held: a claim paid to a payee the customer nominated, or to the customer,
//! is not value the customer is taking out of a policy.
//!
//! When `Rules::hold_check_max_age_minutes` is set, the check also holds such
//! an item (`hold_unconfirmed`) when the master has not been asked about its
//! party within that time, so a master that cannot be reached stops these
//! payments rather than letting them through.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, NaiveDateTime};
use ic_core::PayableKind;
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{Counterparty, NewItem, RefundDestination, RefundStatus, Reject, Role, RunStatus};

/// Source prefixes of payments that take value out of a policy. A unit-
/// linked payout (withdrawal, surrender) is released as `payout:`; the
/// others are the names a releasing module uses for the same things.
pub const VALUE_SOURCES: [&str; 4] = ["payout:", "surrender:", "withdrawal:", "loan:"];

/// The refusal and hold reason while a hold stands.
pub const HELD: &str = "customer_hold";

/// The hold reason when the master was not asked recently enough.
pub const UNCONFIRMED: &str = "hold_unconfirmed";

/// What the customer master last said about a party.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldRecord {
    pub held: bool,
    pub checked_at: NaiveDateTime,
}

/// Whether an outbound item takes value out of a policy: a refund, or a
/// surrender, withdrawal or loan payout.
pub fn releases_value(n: &NewItem) -> bool {
    n.refund.is_some() || n.payable == Some(PayableKind::Refund) || VALUE_SOURCES.iter().any(|p| n.source.starts_with(p))
}

impl PaymentsState {
    /// Whether the master last said the party's hold stands.
    pub fn held(&self, party: &str) -> bool {
        self.customer_holds.get(party).is_some_and(|h| h.held)
    }

    /// Refuses a change to the party's payout accounts while a hold stands.
    pub(crate) fn refuse_if_held(&self, party: &str) -> Result<(), Reject> {
        if self.held(party) {
            return Err(Reject::new(HELD, format!("{party} has a hold on money going out")));
        }
        Ok(())
    }

    /// The party an item pays.
    fn party_paid(&self, counterparty: &Counterparty) -> Option<&str> {
        match counterparty {
            Counterparty::Payee(p) => self.payees.get(p).map(|p| p.party.as_str()),
            Counterparty::Authority(a) => self.authorities.get(a).map(|a| a.party.as_str()),
        }
    }

    /// Why an outbound item may not go now, if the hold stops it: the hold
    /// stands, or (at the check, with `fresh` set) the master was not asked
    /// about the party recently enough.
    pub(crate) fn hold_reason(&self, n: &NewItem, now: NaiveDateTime, fresh: Option<u32>) -> Option<&'static str> {
        if !releases_value(n) {
            return None;
        }
        let party = self.party_paid(&n.counterparty)?;
        let record = self.customer_holds.get(party);
        if record.is_some_and(|r| r.held) {
            return Some(HELD);
        }
        let limit = Duration::minutes(i64::from(fresh?));
        record.is_none_or(|r| now - r.checked_at > limit).then_some(UNCONFIRMED)
    }

    /// What the customer master said about a party's hold.
    pub(crate) fn record_customer_hold(&mut self, tx: &mut Tx<'_>, party: &str, held: bool) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if party.is_empty() {
            return Err(Reject::new("invalid_party", "a hold is recorded against a party"));
        }
        let before = self.customer_holds.insert(party.to_string(), HoldRecord { held, checked_at: tx.now });
        if before.is_none_or(|b| b.held != held) {
            tx.emit(Event::CustomerHoldRecorded {
                party: party.to_string(),
                held,
            });
        }
        Ok(())
    }

    /// The parties the pump should ask the customer master about, with what
    /// it last said: those that money taking value out of a policy is
    /// waiting for or in an unapproved run for, and those whose hold
    /// stands, so its end is heard.
    pub fn hold_watch(&self) -> BTreeMap<String, Option<HoldRecord>> {
        let mut parties: BTreeSet<String> = self.customer_holds.iter().filter(|(_, r)| r.held).map(|(p, _)| p.clone()).collect();
        let unapproved = self.runs.values().filter(|r| matches!(r.status, RunStatus::Prepared | RunStatus::Checked));
        for item in unapproved.flat_map(|r| r.items.values()) {
            if releases_value(&item.new) {
                parties.extend(self.party_paid(&item.new.counterparty).map(str::to_string));
            }
        }
        for refund in self.refunds.values().filter(|r| r.status == RefundStatus::Raised) {
            let to = match &refund.destination {
                RefundDestination::Source { authority } => Counterparty::Authority(authority.clone()),
                RefundDestination::Payee { payee } => Counterparty::Payee(payee.clone()),
                // Paid with a death claim, to whoever the claim pays.
                RefundDestination::WithClaim { .. } => continue,
            };
            parties.extend(self.party_paid(&to).map(str::to_string));
        }
        // A unit-linked payout names the policyholder's party.
        parties.extend(self.waiting.values().filter(|w| w.source.starts_with("payout:")).map(|w| w.payee.clone()));
        parties
            .into_iter()
            .filter(|p| !p.is_empty())
            .map(|p| {
                let record = self.customer_holds.get(&p).cloned();
                (p, record)
            })
            .collect()
    }
}
