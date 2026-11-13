//! Settling cash between statutory funds (PAY-010).
//!
//! A customer's payment lands in one collection account, but the core may
//! apply it to policies in more than one statutory fund: a life policy in
//! one, an investment policy in another. The core says which fund each
//! dollar went to (`ReceiptApplied.fund`). Money applied to a fund other
//! than the one whose account it landed in is owed by that account to the
//! other fund, and is moved by a fund settlement run, so each fund's bank
//! accounts equal its ledger. A dishonour or reversal takes back what the
//! receipt had made owed.
//!
//! A sweep item names its source `fund-sweep:{from account}:{to fund}:{n}`
//! and pays one of the insurer's own accounts. It is refused for more than
//! is owed less what other sweep items already carry.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{Counterparty, Id, ItemStatus, NewItem, PayeeKind, Purpose, Reject, Run, RunType};

/// How long a receipt's landing is remembered, waiting for the core to say
/// which fund it went to.
pub const LANDED_DAYS: i64 = 400;

/// The source prefix of a sweep item.
pub const SWEEP: &str = "fund-sweep:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
    pub account: Id,
    pub on: NaiveDate,
}

/// What one collection account owes one other fund.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owed {
    pub amount_minor: i64,
    /// The first day this balance became owed and has not been cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sweeps {
    /// Receipts by ID, with the account the money landed in.
    pub landed: BTreeMap<String, Landed>,
    /// The same, by day, so old ones can be forgotten in order.
    pub landed_by_day: BTreeSet<(NaiveDate, String)>,
    /// What each applied receipt made owed, so a reversal can take it back:
    /// receipt to (from account, to fund, amount).
    pub moved: BTreeMap<String, (Id, String, i64)>,
    /// By (from account, to fund).
    pub owed: BTreeMap<(Id, String), Owed>,
}

/// An item still on its way: not settled, returned or refused.
fn unfinished(status: &ItemStatus) -> bool {
    use ItemStatus::*;
    matches!(status, Pending | Held(_) | Submitted | Acknowledged)
}

/// The sweep a source names: (from account, to fund).
pub fn sweep_of(source: &str) -> Option<(Id, String)> {
    let mut parts = source.strip_prefix(SWEEP)?.split(':');
    let (from, to) = (parts.next()?, parts.next()?);
    (!from.is_empty() && !to.is_empty()).then(|| (from.to_string(), to.to_string()))
}

impl Sweeps {
    /// Money landed in `account` under `receipt`.
    pub fn land(&mut self, receipt: &str, account: &str, on: NaiveDate) {
        while let Some((day, old)) = self.landed_by_day.first().cloned() {
            if (on - day).num_days() <= LANDED_DAYS {
                break;
            }
            self.landed_by_day.remove(&(day, old.clone()));
            self.landed.remove(&old);
        }
        self.landed.insert(
            receipt.to_string(),
            Landed {
                account: account.to_string(),
                on,
            },
        );
        self.landed_by_day.insert((on, receipt.to_string()));
    }

    fn add(&mut self, key: (Id, String), amount_minor: i64, on: NaiveDate) {
        let owed = self.owed.entry(key.clone()).or_default();
        owed.amount_minor += amount_minor;
        if owed.amount_minor <= 0 {
            owed.since = None;
        } else if owed.since.is_none() {
            owed.since = Some(on);
        }
        if owed.amount_minor == 0 {
            self.owed.remove(&key);
        }
    }
}

impl PaymentsState {
    /// The collection account a fund keeps at the bank `account` is with.
    fn collection_account(&self, account: &str, fund: &str) -> Option<Id> {
        let config = self.config.as_ref()?;
        let bank = &config.accounts.get(account)?.bank;
        config
            .accounts
            .iter()
            .find(|(_, a)| &a.bank == bank && a.fund == fund && a.purpose == Purpose::PremiumCollection)
            .map(|(id, _)| id.clone())
    }

    /// Change what `key.0` owes fund `key.1`. A balance that would go below
    /// nothing is owed the other way instead: by the other fund's collection
    /// account at the same bank, to this account's fund.
    fn owe(&mut self, key: (Id, String), amount_minor: i64, on: NaiveDate) {
        let now = self.sweeps.owed.get(&key).map_or(0, |o| o.amount_minor);
        let after = now + amount_minor;
        let from_fund = self.config.as_ref().and_then(|c| c.accounts.get(&key.0)).map(|a| a.fund.clone());
        // Configuration gives every fund a collection account at every bank
        // it collects through; without one the balance stays here, below
        // nothing, for a person to see.
        let back = self.collection_account(&key.0, &key.1).zip(from_fund).filter(|_| after < 0);
        if let Some(back) = back {
            self.sweeps.add(key, -now, on);
            self.sweeps.add(back, -after, on);
            return;
        }
        self.sweeps.add(key, amount_minor, on);
    }

    /// The core applied part of a receipt to a policy in `fund`.
    pub(crate) fn receipt_funded(&mut self, tx: &mut Tx<'_>, receipt: &str, fund: &str, amount_minor: i64) -> Result<(), Reject> {
        let Some(landed) = self.sweeps.landed.get(receipt).cloned() else {
            // Not money this module brought in (a transfer between policies,
            // a migrated receipt): no bank account holds it.
            return Ok(());
        };
        let landed_fund = self.config.as_ref().and_then(|c| c.accounts.get(&landed.account)).map(|a| a.fund.clone());
        if amount_minor <= 0 || landed_fund.as_deref().is_none_or(|f| f == fund) {
            return Ok(());
        }
        let key = (landed.account.clone(), fund.to_string());
        let moved = self.sweeps.moved.entry(receipt.to_string()).or_insert((key.0.clone(), key.1.clone(), 0));
        moved.2 += amount_minor;
        self.owe(key.clone(), amount_minor, tx.now.date());
        tx.emit(Event::FundOwed {
            account: key.0,
            fund: key.1,
            amount_minor,
        });
        Ok(())
    }

    /// The core reversed a receipt: what it made owed between funds is
    /// taken back, as far as the reversal goes.
    pub(crate) fn receipt_unfunded(&mut self, tx: &mut Tx<'_>, receipt: &str, amount_minor: i64) -> Result<(), Reject> {
        let Some((account, fund, moved)) = self.sweeps.moved.get(receipt).cloned() else {
            return Ok(());
        };
        let back = amount_minor.clamp(0, moved);
        if back == moved {
            self.sweeps.moved.remove(receipt);
        } else if let Some(m) = self.sweeps.moved.get_mut(receipt) {
            m.2 -= back;
        }
        self.owe((account.clone(), fund.clone()), -back, tx.now.date());
        tx.emit(Event::FundOwed {
            account,
            fund,
            amount_minor: -back,
        });
        Ok(())
    }

    /// A sweep item may only be what it says: in a fund settlement run of
    /// the paying account's fund, to one of the insurer's own accounts, for
    /// no more than is owed and not already on its way.
    pub(crate) fn vet_sweep(&self, run: &Run, n: &NewItem) -> Result<(), Reject> {
        let Some(key) = sweep_of(&n.source) else {
            return Ok(());
        };
        let bad = |message: String| Err(Reject::new("invalid_sweep", format!("item {}: {message}", n.item)));
        let from_fund = self.config.as_ref().and_then(|c| c.accounts.get(&key.0)).map(|a| a.fund.as_str());
        if run.run_type != RunType::FundSettlement || from_fund != Some(run.fund.as_str()) {
            return bad(format!("a sweep from {} goes in a fund settlement run of that account's fund", key.0));
        }
        let own = match &n.counterparty {
            Counterparty::Payee(p) => self.payees.get(p).is_some_and(|p| p.kind == PayeeKind::Own),
            Counterparty::Authority(_) => false,
        };
        if !own || n.policy.is_some() {
            return bad("a sweep pays one of the insurer's own accounts and no policy".to_string());
        }
        let open = self.sweeps.owed.get(&key).map_or(0, |o| o.amount_minor) - self.in_flight(&key);
        if n.amount_minor > open {
            return bad(format!("{} owes {} only {open}", key.0, key.1));
        }
        Ok(())
    }

    /// What sweep items in runs carry for a sweep and have not yet settled
    /// or failed.
    pub fn in_flight(&self, key: &(Id, String)) -> i64 {
        let prefix = format!("{SWEEP}{}:{}:", key.0, key.1);
        self.sources
            .range(prefix.clone()..)
            .take_while(|(source, _)| source.starts_with(&prefix))
            .filter_map(|(_, item)| self.item(item))
            .filter(|i| unfinished(&i.status))
            .map(|i| i.new.amount_minor)
            .sum()
    }

    /// A sweep item settled: the fund is paid.
    pub(crate) fn sweep_settled(&mut self, n: &NewItem, on: NaiveDate) {
        if let Some(key) = sweep_of(&n.source) {
            self.owe(key, -n.amount_minor, on);
        }
    }

    /// A settled sweep came back: the fund is owed it again.
    pub(crate) fn sweep_returned(&mut self, n: &NewItem, on: NaiveDate) {
        if let Some(key) = sweep_of(&n.source) {
            self.owe(key, n.amount_minor, on);
        }
    }

    /// What each collection account owes other funds, with the account each
    /// fund collects into at the same bank.
    pub fn fund_sweep(&self, as_at: NaiveDate) -> serde_json::Value {
        let items: Vec<serde_json::Value> = self
            .sweeps
            .owed
            .iter()
            .filter(|(_, o)| o.since.is_some_and(|s| s <= as_at))
            .map(|((from, fund), o)| {
                serde_json::json!({
                    "from_account": from,
                    "to_fund": fund,
                    "to_account": self.collection_account(from, fund),
                    "owed_minor": o.amount_minor,
                    "in_flight_minor": self.in_flight(&(from.clone(), fund.clone())),
                    "open_minor": o.amount_minor - self.in_flight(&(from.clone(), fund.clone())),
                    "since": o.since,
                })
            })
            .collect();
        serde_json::json!({ "as_at": as_at, "items": items })
    }

    /// Owed between funds since before `as_at`: the close is not green.
    pub(crate) fn unswept(&self, as_at: NaiveDate) -> Vec<serde_json::Value> {
        self.sweeps
            .owed
            .iter()
            .filter(|(_, o)| o.since.is_some_and(|s| s < as_at))
            .map(|((from, fund), o)| serde_json::json!({ "sweep": from, "to_fund": fund, "owed_minor": o.amount_minor, "since": o.since }))
            .collect()
    }
}
