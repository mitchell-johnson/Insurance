//! Cash matching. Billing posts what is owed; the payments module brings
//! money in; this service decides which policy a payment belongs to and
//! sends the core one command: apply this receipt to this policy.
//!
//! Match keys are tried in order. Each match records which rule matched and
//! at what confidence. Automatic above the threshold; below it the payment
//! becomes an exception item for the collections team.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::{Command, PolicyId};
use ic_ledger::PartitionId;
use ic_money::Money;
use ic_readmodel::Projections;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingPayment {
    /// The rail's own ID for the payment: becomes the receipt ID.
    pub id: String,
    pub amount: Money,
    /// The reference the payer or the rail supplied.
    pub reference: Option<String>,
    /// Opaque token for the paying account, from the payments module.
    pub payer_account: Option<String>,
    pub received_on: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchRule {
    /// The rail's reference against the policy number.
    Reference,
    /// The payer's account against a direct debit authority on file.
    PayerAuthority,
    /// Amount and date within a tolerance, where exactly one policy fits.
    AmountAndDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Match {
    pub policy: PolicyId,
    pub partition: PartitionId,
    pub rule: MatchRule,
    pub confidence: u8,
    /// The read model sequence the decision reflects, per the partition.
    pub as_of_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Outcome {
    /// Send this command to this partition. The idempotency key is the receipt ID.
    Apply { matched: Match, command: Command },
    /// Low confidence or no match: an exception item. The money stays in the
    /// payments module's unidentified receipts until someone decides.
    Exception { reason: String, suggestion: Option<Match> },
}

#[derive(Debug, Clone)]
pub struct Matcher {
    /// Matches at or above this confidence are applied without a person.
    pub auto_threshold: u8,
    pub amount_tolerance_minor: i64,
    pub date_tolerance_days: i64,
    /// Payer account token -> policy, from direct debit authorities.
    pub authorities: BTreeMap<String, PolicyId>,
}

impl Default for Matcher {
    fn default() -> Self {
        Self {
            auto_threshold: 80,
            amount_tolerance_minor: 0,
            date_tolerance_days: 5,
            authorities: BTreeMap::new(),
        }
    }
}

fn normalise(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

impl Matcher {
    fn found(&self, view: &Projections, policy: &PolicyId, rule: MatchRule, confidence: u8) -> Option<Match> {
        let e = view.policy(policy)?;
        Some(Match {
            policy: policy.clone(),
            partition: e.partition,
            rule,
            confidence,
            as_of_seq: view.checkpoint(e.partition),
        })
    }

    fn find(&self, view: &Projections, p: &IncomingPayment) -> Option<Match> {
        if let Some(reference) = &p.reference {
            let r = normalise(reference);
            // Longest policy number contained in the reference wins.
            let hit = view
                .directory()
                .filter(|e| !e.number.is_empty() && r.contains(&normalise(&e.number)))
                .max_by_key(|e| e.number.len());
            if let Some(e) = hit {
                return self.found(view, &e.policy, MatchRule::Reference, 100);
            }
        }
        if let Some(policy) = p.payer_account.as_ref().and_then(|a| self.authorities.get(a)) {
            return self.found(view, policy, MatchRule::PayerAuthority, 90);
        }
        let mut candidates = view.directory().filter(|e| {
            view.billing(&e.policy).is_some_and(|b| {
                (b.outstanding_minor - p.amount.minor).abs() <= self.amount_tolerance_minor
                    && b.outstanding_minor > 0
                    && b.oldest_unpaid_due
                        .is_some_and(|d| (p.received_on - d).num_days().abs() <= self.date_tolerance_days)
            })
        });
        match (candidates.next(), candidates.next()) {
            (Some(e), None) => self.found(view, &e.policy, MatchRule::AmountAndDate, 60),
            _ => None,
        }
    }

    pub fn match_payment(&self, view: &Projections, p: &IncomingPayment) -> Outcome {
        match self.find(view, p) {
            Some(m) if m.confidence >= self.auto_threshold => {
                let command = Command::ApplyReceipt {
                    policy: m.policy.clone(),
                    receipt: p.id.clone(),
                    amount: p.amount,
                };
                Outcome::Apply { matched: m, command }
            }
            Some(m) => Outcome::Exception {
                reason: format!("matched by {:?} at confidence {}, below the automatic threshold", m.rule, m.confidence),
                suggestion: Some(m),
            },
            None => Outcome::Exception {
                reason: "no policy matches this payment".to_string(),
                suggestion: None,
            },
        }
    }
}
