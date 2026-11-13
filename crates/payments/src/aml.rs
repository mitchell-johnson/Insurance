//! Premium paid by someone other than the policy owner (FIN-015, FIN-016).
//!
//! A third party paying someone else's premium is an anti-money-laundering
//! indicator. Finance never sees who paid; this module does. When a receipt
//! is applied to a policy (`ReceiptApplied`), the payer it came from is
//! compared with the policy's owner (`PolicyIssued`):
//!
//! - a direct debit is paid by the authority's party;
//! - a direct credit is paid by the one party whose authorities or payees
//!   use the paying account, or else by the account itself.
//!
//! An unexplained payer opens one case per owner and payer, and compliance
//! is told once (`third_party_payer`). Later payments by the same payer, to
//! any of the owner's policies, join the open case. Compliance closes it
//! with `clear_payer`: naming the relationship recorded in the customer
//! master (a business paying its director's premium, a parent a child's), it
//! never fires again for that payer and owner; without one, it is cleared
//! for this month only and a payment in a later month raises it again.

use std::collections::BTreeMap;

use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{Counterparty, Instruction, NewItem, OutboxEntry, Reject, Role};

/// The notice compliance is sent for a new case.
pub const SUBJECT: &str = "third_party_payer";

/// How long a payer is remembered while its receipt waits to be applied.
const KEEP_PAYER_DAYS: i64 = 60;

/// Who paid a receipt not yet applied, and when it was seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payer {
    pub payer: String,
    pub seen: NaiveDate,
}

/// One owner and one payer who is not them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayerCase {
    pub owner: String,
    pub payer: String,
    pub policies: Vec<String>,
    pub opened: NaiveDate,
    pub last_paid: NaiveDate,
    /// Open while `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared: Option<Clearance>,
}

/// How compliance closed a case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clearance {
    pub by: String,
    pub on: NaiveDate,
    /// The customer master's record of the payer's relationship to the
    /// owner. With it the case never re-opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    pub note: String,
}

/// Everything this rule keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayerChecks {
    /// Policy owners, from the core.
    pub owners: BTreeMap<String, String>,
    /// Payers by receipt, until the core applies the receipt.
    pub pending: BTreeMap<String, Payer>,
    /// Cases by `{owner}|{payer}`.
    pub cases: BTreeMap<String, PayerCase>,
}

fn digits(number: &str) -> String {
    number.chars().filter(char::is_ascii_alphanumeric).collect()
}

fn case_key(owner: &str, payer: &str) -> String {
    format!("{owner}|{payer}")
}

fn month(day: NaiveDate) -> (i32, u32) {
    (day.year(), day.month())
}

impl PaymentsState {
    /// The core issued a policy to its owner.
    pub(crate) fn policy_owner(&mut self, tx: &mut Tx<'_>, policy: &str, owner: &str) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        self.payer_checks.owners.insert(policy.to_string(), owner.to_string());
        Ok(())
    }

    /// Who pays from an account: the one party whose authorities or payees
    /// use it, or else the account itself.
    pub(crate) fn account_payer(&self, number: &str) -> String {
        let wanted = digits(number);
        let authorities = self.authorities.values().map(|a| (&a.party, &a.account.number));
        let payees = self.payees.values().map(|p| (&p.party, &p.account.number));
        let mut parties: Vec<&String> = authorities.chain(payees).filter(|(_, n)| digits(n) == wanted).map(|(p, _)| p).collect();
        parties.dedup();
        match parties.as_slice() {
            [one] => format!("party:{one}"),
            _ => format!("account:{wanted}"),
        }
    }

    /// Remember who paid a receipt until the core applies it.
    pub(crate) fn note_payer(&mut self, receipt: &str, payer: String, seen: NaiveDate) {
        let keep_from = seen - Duration::days(KEEP_PAYER_DAYS);
        self.payer_checks.pending.retain(|_, p| p.seen >= keep_from);
        self.payer_checks.pending.insert(receipt.to_string(), Payer { payer, seen });
    }

    /// The payer of a collection: the party of the authority it drew on.
    pub(crate) fn note_collection_payer(&mut self, n: &NewItem, seen: NaiveDate) {
        if let Counterparty::Authority(a) = &n.counterparty {
            if let Some(party) = self.authorities.get(a).map(|a| a.party.clone()) {
                self.note_payer(&n.item, format!("party:{party}"), seen);
            }
        }
    }

    /// Whether the payer is the owner: the owner themself, or an account
    /// one of the owner's authorities or payees uses.
    fn owners_own(&self, owner: &str, payer: &str) -> bool {
        if payer == format!("party:{owner}") {
            return true;
        }
        let Some(account) = payer.strip_prefix("account:") else { return false };
        let authorities = self.authorities.values().map(|a| (&a.party, &a.account.number));
        let payees = self.payees.values().map(|p| (&p.party, &p.account.number));
        authorities.chain(payees).any(|(party, number)| party == owner && digits(number) == account)
    }

    /// The core applied a receipt to a policy: check who paid it.
    pub(crate) fn premium_applied(&mut self, tx: &mut Tx<'_>, policy: &str, receipt: &str) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let Some(paid) = self.payer_checks.pending.remove(receipt) else {
            return Ok(());
        };
        let Some(owner) = self.payer_checks.owners.get(policy).cloned() else {
            return Ok(());
        };
        if self.owners_own(&owner, &paid.payer) {
            return Ok(());
        }
        let key = case_key(&owner, &paid.payer);
        let raise = match self.payer_checks.cases.get_mut(&key) {
            None => {
                self.payer_checks.cases.insert(
                    key.clone(),
                    PayerCase {
                        owner,
                        payer: paid.payer,
                        policies: vec![policy.to_string()],
                        opened: paid.seen,
                        last_paid: paid.seen,
                        cleared: None,
                    },
                );
                true
            }
            Some(case) => {
                if !case.policies.iter().any(|p| p == policy) {
                    case.policies.push(policy.to_string());
                }
                case.last_paid = case.last_paid.max(paid.seen);
                // Cleared without a relationship: good for the month only.
                let lapsed = case
                    .cleared
                    .as_ref()
                    .is_some_and(|c| c.relationship.is_none() && month(paid.seen) > month(c.on));
                if lapsed {
                    case.cleared = None;
                    case.opened = paid.seen;
                }
                lapsed
            }
        };
        if raise {
            self.push_outbox(OutboxEntry {
                key: format!("aml-{key}-{}", paid.seen),
                instruction: Instruction::Notify {
                    to: Role::Compliance,
                    subject: SUBJECT.to_string(),
                    detail: format!("payer case {key}: premium on {policy} paid by someone other than its owner"),
                },
                about: None,
            });
            tx.emit(Event::PayerCaseOpened { case: key });
        }
        Ok(())
    }

    /// Compliance closes a case, naming the relationship the customer
    /// master records when there is one.
    pub(crate) fn clear_payer(&mut self, tx: &mut Tx<'_>, case: &str, relationship: Option<String>, note: String) -> Result<(), Reject> {
        tx.need(Role::Compliance)?;
        if note.trim().is_empty() || relationship.as_deref().is_some_and(|r| r.trim().is_empty()) {
            return Err(Reject::new(
                "invalid_clearance",
                "a clearance says why, and names a relationship only by its reference",
            ));
        }
        let record = self
            .payer_checks
            .cases
            .get_mut(case)
            .ok_or_else(|| Reject::new("unknown_case", format!("there is no payer case {case}")))?;
        if record.cleared.is_some() {
            return Err(Reject::new("case_cleared", format!("payer case {case} is already cleared")));
        }
        record.cleared = Some(Clearance {
            by: tx.actor.id.clone(),
            on: tx.now.date(),
            relationship,
            note,
        });
        tx.emit(Event::PayerCaseCleared { case: case.to_string() });
        Ok(())
    }

    /// Open cases, oldest first.
    pub fn open_payer_cases(&self) -> Vec<(&String, &PayerCase)> {
        let mut open: Vec<_> = self.payer_checks.cases.iter().filter(|(_, c)| c.cleared.is_none()).collect();
        open.sort_by_key(|(_, c)| c.opened);
        open
    }
}
