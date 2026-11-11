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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// What one read model offers for a payment under each rule. Where the read
/// models are kept per partition, each partition's candidates are merged
/// before the decision, so the answer is the same as over one read model of
/// everything.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Candidates {
    /// The policy with the longest number found in the reference, and that length.
    pub reference: Option<(usize, Match)>,
    /// The policy the payer's direct debit authority names.
    pub authority: Option<Match>,
    /// The first policy that fits by amount and date, and how many fit.
    pub amount_and_date: Option<Match>,
    pub amount_and_date_count: u64,
}

impl Candidates {
    /// Combine two read models' candidates.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            // The longest number wins; between equals, the first.
            reference: match (self.reference, other.reference) {
                (Some(a), Some(b)) if b.0 > a.0 => Some(b),
                (a, b) => a.or(b),
            },
            authority: self.authority.or(other.authority),
            amount_and_date: self.amount_and_date.or(other.amount_and_date),
            amount_and_date_count: self.amount_and_date_count + other.amount_and_date_count,
        }
    }

    /// The match the rules give, tried in order.
    fn best(self) -> Option<Match> {
        if let Some((_, m)) = self.reference {
            return Some(m);
        }
        if self.authority.is_some() {
            return self.authority;
        }
        // Amount and date identify a policy only when exactly one fits.
        self.amount_and_date.filter(|_| self.amount_and_date_count == 1)
    }
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

    /// What this read model offers for the payment under each rule.
    pub fn candidates(&self, view: &Projections, p: &IncomingPayment) -> Candidates {
        let reference = p.reference.as_ref().and_then(|reference| {
            let r = normalise(reference);
            // Longest policy number contained in the reference wins.
            let hit = view
                .directory()
                .filter(|e| !e.number.is_empty() && r.contains(&normalise(&e.number)))
                .max_by_key(|e| e.number.len())?;
            Some((hit.number.len(), self.found(view, &hit.policy, MatchRule::Reference, 100)?))
        });
        let authority = p
            .payer_account
            .as_ref()
            .and_then(|a| self.authorities.get(a))
            .and_then(|policy| self.found(view, policy, MatchRule::PayerAuthority, 90));
        let mut fitting = view.directory().filter(|e| {
            view.billing(&e.policy).is_some_and(|b| {
                (b.outstanding_minor - p.amount.minor).abs() <= self.amount_tolerance_minor
                    && b.outstanding_minor > 0
                    && b.oldest_unpaid_due
                        .is_some_and(|d| (p.received_on - d).num_days().abs() <= self.date_tolerance_days)
            })
        });
        let amount_and_date = fitting.next().and_then(|e| self.found(view, &e.policy, MatchRule::AmountAndDate, 60));
        let amount_and_date_count = u64::from(amount_and_date.is_some()) + fitting.count() as u64;
        Candidates {
            reference,
            authority,
            amount_and_date,
            amount_and_date_count,
        }
    }

    /// Decide what to do with a payment given every read model's candidates.
    pub fn decide(&self, candidates: Candidates, p: &IncomingPayment) -> Outcome {
        match candidates.best() {
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

    pub fn match_payment(&self, view: &Projections, p: &IncomingPayment) -> Outcome {
        self.decide(self.candidates(view, p), p)
    }
}
