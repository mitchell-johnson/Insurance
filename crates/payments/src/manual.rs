//! Money nobody could identify, applied by a person (COL-020, COL-022).
//!
//! A reconciler proposes how an unidentified receipt is applied: to one
//! policy, or split across several (one transfer paying four relatives'
//! premiums). The parts must add up to the receipt. At or above
//! `Rules::second_person_match_minor` (a recorded best estimate of $1,000
//! when the configuration does not say) a second reconciler must approve it,
//! and the proposer cannot. Each part goes to the core as cash matching's
//! `ApplyReceipt` under its own receipt ID `{exception}/{n}`, with the payer
//! remembered so the payer-not-owner rule (`aml.rs`) checks each policy. The
//! exception is resolved when the core has applied every part; a part the
//! core refuses leaves it open and tells reconcilers.

use std::collections::BTreeMap;

use ic_core::PolicyId;
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{About, ExceptionKind, Instruction, OutboxEntry, Reject, Role};

/// The threshold when the configuration names none: $1,000.
pub const DEFAULT_SECOND_PERSON_MINOR: i64 = 1_000_00;

/// The most policies one receipt is split across.
pub const MAX_PARTS: usize = 20;

/// Outbox key prefix of a part applied by hand.
pub(crate) const PART: &str = "manual-match-";

/// A person's proposal for one receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub by: String,
    pub parts: Vec<(PolicyId, i64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    /// Parts the core has not yet answered.
    #[serde(default)]
    pub waiting: usize,
    /// What the core refused of the parts answered.
    #[serde(default)]
    pub refused_minor: i64,
}

fn invalid(message: &str) -> Reject {
    Reject::new("invalid_match", message)
}

impl PaymentsState {
    fn second_person_minor(&self) -> i64 {
        self.config
            .as_ref()
            .and_then(|c| c.rules.second_person_match_minor)
            .unwrap_or(DEFAULT_SECOND_PERSON_MINOR)
    }

    pub(crate) fn propose_receipt_match(&mut self, tx: &mut Tx<'_>, exception: &str, parts: Vec<(PolicyId, i64)>) -> Result<(), Reject> {
        tx.need(Role::Reconciler)?;
        let e = self
            .exceptions
            .get(exception)
            .filter(|e| e.resolution.is_none())
            .ok_or_else(|| Reject::new("unknown_exception", format!("there is no open exception {exception}")))?;
        if !matches!(e.kind, ExceptionKind::UnidentifiedReceipt | ExceptionKind::UnexplainedCredit) {
            return Err(invalid("only money received can be applied to policies"));
        }
        if self.manual_matches.get(exception).is_some_and(|p| p.approved_by.is_some()) {
            return Err(Reject::new("match_approved", "this receipt is already being applied"));
        }
        let total = parts.iter().try_fold(0i64, |sum, (_, a)| (*a > 0).then(|| sum.checked_add(*a)).flatten());
        let mut policies: Vec<&PolicyId> = parts.iter().map(|(p, _)| p).collect();
        policies.sort();
        policies.dedup();
        if parts.is_empty() || parts.len() > MAX_PARTS || policies.len() != parts.len() || total != Some(e.amount_minor) {
            return Err(invalid("each policy once, each part positive, adding up to the receipt"));
        }
        let amount = e.amount_minor;
        self.manual_matches.insert(
            exception.to_string(),
            Proposal {
                by: tx.actor.id.clone(),
                parts,
                approved_by: None,
                waiting: 0,
                refused_minor: 0,
            },
        );
        tx.emit(Event::ReceiptMatchProposed {
            exception: exception.to_string(),
        });
        if amount < self.second_person_minor() {
            let me = tx.actor.id.clone();
            self.apply_match(tx, exception, me);
        }
        Ok(())
    }

    pub(crate) fn approve_receipt_match(&mut self, tx: &mut Tx<'_>, exception: &str) -> Result<(), Reject> {
        tx.need(Role::Reconciler)?;
        let proposal = self
            .manual_matches
            .get(exception)
            .ok_or_else(|| Reject::new("unknown_match", format!("nothing is proposed for {exception}")))?;
        if proposal.approved_by.is_some() {
            return Err(Reject::new("match_approved", "this receipt is already being applied"));
        }
        if proposal.by == tx.actor.id {
            return Err(Reject::new("same_person", "the person who proposed the match cannot approve it"));
        }
        let me = tx.actor.id.clone();
        self.apply_match(tx, exception, me);
        Ok(())
    }

    fn apply_match(&mut self, tx: &mut Tx<'_>, exception: &str, approver: String) {
        let Some(proposal) = self.manual_matches.get_mut(exception) else { return };
        proposal.approved_by = Some(approver);
        proposal.waiting = proposal.parts.len();
        let parts = proposal.parts.clone();
        let currency = self.exception_currency(exception);
        let payer = self.payer_checks.pending.get(&format!("match-{exception}")).cloned();
        for (n, (policy, amount_minor)) in parts.into_iter().enumerate() {
            let receipt = format!("{exception}/{}", n + 1);
            if let Some(p) = &payer {
                self.note_payer(&receipt, p.payer.clone(), p.seen);
            }
            self.push_outbox(OutboxEntry {
                key: format!("{PART}{receipt}"),
                instruction: Instruction::CoreAt {
                    policy: policy.clone(),
                    caller: "cash-matching".to_string(),
                    command: Box::new(ic_core::Command::ApplyReceipt {
                        policy,
                        receipt,
                        amount: ic_money::Money::new(amount_minor, currency),
                    }),
                },
                about: Some(About::Exception(exception.to_string())),
            });
        }
        tx.emit(Event::ReceiptMatchApproved {
            exception: exception.to_string(),
        });
    }

    fn exception_currency(&self, exception: &str) -> ic_money::Currency {
        let account = self.exceptions.get(exception).map(|e| e.account.clone()).unwrap_or_default();
        self.config
            .as_ref()
            .and_then(|c| c.accounts.get(&account))
            .map_or(ic_money::Currency::NZD, |a| a.currency)
    }

    /// The core applied a part, or refused it. Returns whether the
    /// exception is now fully applied. When the last part is answered and
    /// some were refused, the exception stays open for what was refused, so
    /// a person can propose again for that.
    pub(crate) fn part_done(&mut self, exception: &str, key: &str, refused: Option<&str>) -> bool {
        let Some(proposal) = self.manual_matches.get_mut(exception) else {
            return false;
        };
        proposal.waiting = proposal.waiting.saturating_sub(1);
        if let Some(code) = refused {
            let n: usize = key.rsplit('/').next().and_then(|n| n.parse().ok()).unwrap_or(0);
            proposal.refused_minor += proposal.parts.get(n.wrapping_sub(1)).map_or(0, |(_, a)| *a);
            self.push_outbox(OutboxEntry {
                key: format!("refused-{key}"),
                instruction: Instruction::Notify {
                    to: Role::Reconciler,
                    subject: "manual_match_refused".to_string(),
                    detail: format!("{key}: the core refused it ({code}); exception {exception} stays open for that part"),
                },
                about: None,
            });
        }
        let Some(proposal) = self.manual_matches.get(exception).filter(|p| p.waiting == 0) else {
            return false;
        };
        let left = proposal.refused_minor;
        self.manual_matches.remove(exception);
        if left > 0 {
            if let Some(e) = self.exceptions.get_mut(exception) {
                e.amount_minor = left;
            }
            return false;
        }
        true
    }

    /// Proposals waiting for a second person, oldest exception first.
    pub fn matches_to_approve(&self) -> BTreeMap<&String, &Proposal> {
        self.manual_matches.iter().filter(|(_, p)| p.approved_by.is_none()).collect()
    }
}
