//! A policy owner's bankruptcy, as the core records it (decision 4 in
//! `docs/decisions.md`, `JNT-029`).
//!
//! The core records an insolvency interest on each policy the bankrupt owns
//! and holds the payouts it raises while one stands. A change of payout
//! account is this module's to stop: while any insolvency interest names a
//! party as the policy owner, a payee cannot be registered for that party
//! nor a payee's account changed, and a refund or a surrender, withdrawal or
//! loan payout to that party is refused when it joins a run and held
//! (`owner_insolvent`) if it is already in one. Money a legal direction sends
//! elsewhere (`PayoutDirected`) is paid to the payee it names, a different
//! party, and is not stopped. Claim payments are the claims module's to hold.
//!
//! Each interest is mirrored, so one release does not end a bankruptcy
//! still recorded on another of the owner's policies. A release takes effect
//! on its own date, which can be later than the event.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{Reject, Role};

/// The refusal and hold reason while the owner is bankrupt.
pub const INSOLVENT: &str = "owner_insolvent";

/// The insolvency interests that name one party as owner, keyed by
/// `{policy}/{interest}`, each with the date its release takes effect once
/// one is recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Insolvency {
    pub interests: BTreeMap<String, Option<NaiveDate>>,
}

impl Insolvency {
    fn stands_on(&self, today: NaiveDate) -> bool {
        self.interests.values().any(|released| released.is_none_or(|on| on > today))
    }
}

fn interest_key(policy: &str, interest: &str) -> String {
    format!("{policy}/{interest}")
}

impl PaymentsState {
    /// Whether an insolvency interest naming the party as owner stands on
    /// the day.
    pub fn insolvent(&self, party: &str, today: NaiveDate) -> bool {
        self.insolvencies.get(party).is_some_and(|i| i.stands_on(today))
    }

    /// The core recorded an insolvency interest, or released one, on a
    /// policy the party owns.
    pub(crate) fn record_insolvency(&mut self, tx: &mut Tx<'_>, owner: &str, policy: &str, interest: &str, released: Option<NaiveDate>) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if owner.is_empty() || policy.is_empty() || interest.is_empty() {
            return Err(Reject::new("invalid_interest", "an insolvency names its owner, policy and interest"));
        }
        let today = tx.now.date();
        let was = self.insolvent(owner, today);
        let record = self.insolvencies.entry(owner.to_string()).or_default();
        record.interests.insert(interest_key(policy, interest), released);
        // Releases already in effect are forgotten; a later one is kept
        // until its day.
        record.interests.retain(|_, released| released.is_none_or(|on| on > today));
        if record.interests.is_empty() {
            self.insolvencies.remove(owner);
        }
        let now = self.insolvent(owner, today);
        if was != now {
            tx.emit(Event::OwnerInsolvencyChanged {
                party: owner.to_string(),
                insolvent: now,
            });
        }
        Ok(())
    }

    /// Refuses a change to the party's payout accounts while a hold of the
    /// customer's own stands or the party is bankrupt.
    pub(crate) fn refuse_if_held(&self, party: &str, today: NaiveDate) -> Result<(), Reject> {
        if self.held(party) {
            return Err(Reject::new(crate::hold::HELD, format!("{party} has a hold on money going out")));
        }
        if self.insolvent(party, today) {
            return Err(Reject::new(
                INSOLVENT,
                format!("{party} is bankrupt; the Official Assignee must release the interest"),
            ));
        }
        Ok(())
    }
}
