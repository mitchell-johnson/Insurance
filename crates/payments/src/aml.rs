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
    /// A clearance naming a relationship, until the customer master
    /// confirms the relationship links the payer and the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<Clearance>,
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
    /// Days each owner asked for a refund, within the pattern window.
    #[serde(default)]
    pub refunds: BTreeMap<String, Vec<NaiveDate>>,
}

/// Refunds asked for by one owner within this many days that make a
/// pattern (VUL-020): overpaying and asking for the money back is how
/// money is passed through an insurer. A recorded best estimate.
pub const REFUND_PATTERN_DAYS: i64 = 60;
pub const REFUND_PATTERN_COUNT: usize = 3;

/// The payer of a refund-pattern case: not a payer at all, the pattern.
pub const REFUND_PATTERN: &str = "refund-pattern";

pub(crate) fn digits(number: &str) -> String {
    number.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// The customer master's reference for a relationship: `{party}/{id}`,
/// each part plain.
pub fn relationship_reference(reference: &str) -> bool {
    let plain = |s: &str| !s.is_empty() && s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    reference.split_once('/').is_some_and(|(party, id)| plain(party) && plain(id))
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
        self.note_applied(receipt, policy, tx.now.date());
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
                        proposed: None,
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
        if note.trim().is_empty() || relationship.as_deref().is_some_and(|r| !relationship_reference(r)) {
            return Err(Reject::new(
                "invalid_clearance",
                "a clearance says why, and names a relationship by the customer master's {party}/{id}",
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
        let clearance = Clearance {
            by: tx.actor.id.clone(),
            on: tx.now.date(),
            relationship,
            note,
        };
        // A relationship is taken only once the customer master confirms it.
        if clearance.relationship.is_some() {
            record.proposed = Some(clearance);
            return Ok(());
        }
        record.cleared = Some(clearance);
        tx.emit(Event::PayerCaseCleared { case: case.to_string() });
        Ok(())
    }

    /// What the customer master said about a proposed clearance's
    /// relationship. Confirmed, the case is cleared for good; otherwise the
    /// proposal is dropped and compliance is told why.
    pub(crate) fn confirm_relationship(&mut self, tx: &mut Tx<'_>, case: &str, confirmed: bool, reason: String) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let Some(proposed) = self.payer_checks.cases.get_mut(case).and_then(|c| c.proposed.take()) else {
            // Answered already: a second answer changes nothing.
            return Ok(());
        };
        if confirmed {
            if let Some(record) = self.payer_checks.cases.get_mut(case) {
                record.cleared = Some(proposed);
            }
            tx.emit(Event::PayerCaseCleared { case: case.to_string() });
            return Ok(());
        }
        let relationship = proposed.relationship.unwrap_or_default();
        self.push_outbox(OutboxEntry {
            key: format!("aml-unconfirmed-{case}-{relationship}"),
            instruction: Instruction::Notify {
                to: Role::Compliance,
                subject: "relationship_not_confirmed".to_string(),
                detail: format!("payer case {case} stays open: relationship {relationship}: {reason}"),
            },
            about: None,
        });
        Ok(())
    }

    /// Proposed clearances for the pump to check with the customer master:
    /// (case, owner, payer, relationship reference).
    pub fn relationships_to_check(&self) -> Vec<(String, String, String, String)> {
        self.payer_checks
            .cases
            .iter()
            .filter_map(|(key, c)| {
                let relationship = c.proposed.as_ref()?.relationship.clone()?;
                Some((key.clone(), c.owner.clone(), c.payer.clone(), relationship))
            })
            .collect()
    }

    /// A refund was asked for on a policy. The third within the window
    /// for one owner opens a restricted case for compliance, once; staff
    /// see only that the party needs support (`support_prompt`).
    pub(crate) fn refund_asked(&mut self, tx: &mut Tx<'_>, policy: &str) {
        let Some(owner) = self.payer_checks.owners.get(policy).cloned() else { return };
        let today = tx.now.date();
        let from = today - Duration::days(REFUND_PATTERN_DAYS);
        let days = self.payer_checks.refunds.entry(owner.clone()).or_default();
        days.retain(|d| *d > from);
        days.push(today);
        if days.len() < REFUND_PATTERN_COUNT {
            return;
        }
        let key = case_key(&owner, REFUND_PATTERN);
        let fresh = match self.payer_checks.cases.get(&key) {
            None => true,
            // Cleared earlier: a new pattern since then is raised again.
            Some(case) => case.cleared.as_ref().is_some_and(|c| c.on < from),
        };
        if !fresh {
            return;
        }
        self.payer_checks.cases.insert(
            key.clone(),
            PayerCase {
                owner,
                payer: REFUND_PATTERN.to_string(),
                policies: vec![policy.to_string()],
                opened: today,
                last_paid: today,
                cleared: None,
                proposed: None,
            },
        );
        self.push_outbox(OutboxEntry {
            key: format!("aml-{key}-{today}"),
            instruction: Instruction::Notify {
                to: Role::Compliance,
                subject: "refund_pattern".to_string(),
                detail: format!("payer case {key}: {REFUND_PATTERN_COUNT} refunds asked for within {REFUND_PATTERN_DAYS} days"),
            },
            about: None,
        });
        tx.emit(Event::PayerCaseOpened { case: key });
    }

    /// Whether staff should be prompted to offer the party support: a
    /// restricted case is open about them. Says nothing of why.
    pub fn support_prompt(&self, party: &str) -> bool {
        self.payer_checks.cases.values().any(|c| c.owner == party && c.cleared.is_none())
    }

    /// Open cases, oldest first.
    pub fn open_payer_cases(&self) -> Vec<(&String, &PayerCase)> {
        let mut open: Vec<_> = self.payer_checks.cases.iter().filter(|(_, c)| c.cleared.is_none()).collect();
        open.sort_by_key(|(_, c)| c.opened);
        open
    }
}
