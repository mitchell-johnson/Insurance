//! A payment that went into the wrong account (CLM-022).
//!
//! A reconciler reports a settled payment, or part of it, as misdirected:
//! a premium refund paid to an account that was not the customer's, say,
//! half of it already spent. From then on:
//!
//! - The bank is asked to recall it (`Instruction::Recall`: this system's
//!   own JSON, or ISO 20022 `camt.055`), under the misdirection's ID as the
//!   case. Its answer (`camt.029`) is recorded; a refusal tells collections.
//! - The core holds what the holder owes as a payee debt and owes the right
//!   person again (`PayeeDebtCommand::Raise`). A refund is then waiting to
//!   be paid once more; the payee it went to is marked suspect, or the
//!   authority suspended, so nothing more goes to that account until its
//!   details are put right.
//! - Collections is told to arrange repayment with the holder: the case,
//!   with a payment reference `RP` and six digits (question
//!   `misdirection_reference {misdirection}`). A credit quoting it, and the
//!   bank's return of the recalled payment, are recovered against the debt
//!   (`Recover`). Nothing is ever netted from a later payment to anyone.
//! - What will not come back is written off by an approver who did not
//!   report it, with a plain reason code (`WriteOff`).
//!
//! The misdirection closes when the core says nothing is owed.

use chrono::NaiveDate;
use ic_core::payee_debt::PayeeDebtCommand;
use ic_core::PolicyId;
use ic_money::{Currency, Money};
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{About, AuthorityStatus, Counterparty, Expected, Id, Instruction, ItemStatus, OutboxEntry, Reject, Role};
use crate::repay::Repayable;

/// The cancellation reason a recall gives: wrong account number.
pub const WRONG_ACCOUNT: &str = "AC03";

/// Outbox key prefixes.
pub(crate) const RAISE: &str = "payee-debt-";
pub(crate) const RECALL: &str = "recall-";
pub(crate) const RECOVER: &str = "payee-debt-recovered-";
pub(crate) const WRITE_OFF: &str = "payee-debt-write-off-";

/// Where the bank's recall has got to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallStatus {
    /// Waiting for the pump to send it.
    Asking,
    /// The bank has the request.
    Asked,
    /// The bank could not take the request.
    NotSent(String),
    /// The receiving bank agreed: money is coming back.
    Accepted,
    /// It will not come back through the bank.
    Refused(String),
}

/// One payment that went astray, and what has been done about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Misdirection {
    pub item: Id,
    pub policy: PolicyId,
    pub currency: Currency,
    pub amount_minor: i64,
    /// Who holds the money, where known: an opaque party ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    pub reported_by: String,
    pub reported_on: NaiveDate,
    pub note: String,
    pub recall: RecallStatus,
    /// What the core last said is still owed.
    pub outstanding_minor: i64,
    /// Write-offs asked of the core, for their keys.
    #[serde(default)]
    pub write_offs: u32,
    #[serde(default)]
    pub closed: bool,
}

fn invalid(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

impl PaymentsState {
    fn misdirection_mut(&mut self, id: &str) -> Result<&mut Misdirection, Reject> {
        self.misdirections
            .get_mut(id)
            .filter(|m| !m.closed)
            .ok_or_else(|| invalid("unknown_misdirection", format!("there is no open misdirection {id}")))
    }

    /// The open misdirection of a payment, if it has one.
    pub(crate) fn misdirection_of(&self, item: &str) -> Option<&Id> {
        self.misdirections.iter().find(|(_, m)| m.item == item && !m.closed).map(|(id, _)| id)
    }

    fn reference_of(&self, id: &str) -> String {
        self.repayables.references.get(&format!("debt:{id}")).cloned().unwrap_or_default()
    }

    fn tell(&mut self, key: String, to: Role, subject: &str, detail: String) {
        self.push_outbox(OutboxEntry {
            key,
            instruction: Instruction::Notify {
                to,
                subject: subject.to_string(),
                detail,
            },
            about: None,
        });
    }

    pub(crate) fn report_misdirected(
        &mut self,
        tx: &mut Tx<'_>,
        id: Id,
        item_id: &str,
        amount_minor: i64,
        holder: Option<String>,
        note: String,
    ) -> Result<(), Reject> {
        tx.need(Role::Reconciler)?;
        if !crate::iso20022::fits_identifier(&id) || id.contains(':') {
            return Err(invalid(
                "invalid_misdirection",
                "a misdirection ID is 1 to 35 printable characters, no spaces or colons",
            ));
        }
        if self.misdirections.contains_key(&id) {
            return Err(invalid("duplicate_misdirection", format!("misdirection {id} already exists")));
        }
        if note.trim().is_empty() {
            return Err(invalid("invalid_misdirection", "say how it was found"));
        }
        let item = self
            .item(item_id)
            .ok_or_else(|| invalid("unknown_item", format!("there is no item {item_id}")))?;
        let run = &self.runs[&self.items[item_id]];
        if item.status != ItemStatus::Settled || run.run_type.inbound() {
            return Err(invalid("wrong_state", "only a payment the bank settled can have gone to the wrong account"));
        }
        let policy = item
            .new
            .policy
            .clone()
            .ok_or_else(|| invalid("not_policy_money", "only a payment of policy money is owed again through the core"))?;
        if amount_minor <= 0 || amount_minor > item.new.amount_minor {
            return Err(invalid("invalid_amount", "more than nothing, and no more than the payment"));
        }
        if self.misdirection_of(item_id).is_some() {
            return Err(invalid("already_reported", format!("payment {item_id} is already being recovered")));
        }
        let submission_id = item.submission.clone().unwrap_or_default();
        let submission = self.submissions.get(&submission_id).cloned();
        let (counterparty, refund, currency) = (item.new.counterparty.clone(), item.new.refund.clone(), run.currency);
        let settled_on = submission.as_ref().and_then(|s| s.settled_on).unwrap_or(run.value_date);
        self.misdirections.insert(
            id.clone(),
            Misdirection {
                item: item_id.to_string(),
                policy: policy.clone(),
                currency,
                amount_minor,
                holder: holder.clone(),
                reported_by: tx.actor.id.clone(),
                reported_on: tx.now.date(),
                note,
                recall: RecallStatus::Asking,
                outstanding_minor: amount_minor,
                write_offs: 0,
                closed: false,
            },
        );
        // Nothing more goes to that account until its details are right.
        match &counterparty {
            Counterparty::Payee(p) => {
                if let Some(p) = self.payees.get_mut(p) {
                    p.suspect = true;
                }
            }
            Counterparty::Authority(a) => {
                if let Some(a) = self.authorities.get_mut(a).filter(|a| a.status == AuthorityStatus::Active) {
                    a.status = AuthorityStatus::Suspended;
                    a.reason = Some("misdirected_payment".to_string());
                }
            }
        }
        self.repayables.hold(
            "RP",
            Repayable {
                claim: String::new(),
                overpayment: format!("debt:{id}"),
                policy: policy.to_string(),
                outstanding_minor: amount_minor,
            },
        );
        self.push_outbox(OutboxEntry {
            key: format!("{RAISE}{id}"),
            instruction: Instruction::CoreAt {
                policy: policy.clone(),
                caller: "payments".to_string(),
                command: Box::new(ic_core::Command::PayeeDebt(Box::new(PayeeDebtCommand::Raise {
                    policy: policy.clone(),
                    debt: id.clone(),
                    payment: item_id.to_string(),
                    amount: Money::new(amount_minor, currency),
                    party: holder.map(ic_core::PartyId),
                }))),
            },
            about: Some(About::Misdirection(id.clone())),
        });
        if let Some(s) = submission {
            self.push_outbox(OutboxEntry {
                key: format!("{RECALL}{id}"),
                instruction: Instruction::Recall {
                    recall: Box::new(crate::file::Recall {
                        id: id.clone(),
                        channel: s.channel,
                        account: s.account,
                        submission: submission_id,
                        item: item_id.to_string(),
                        amount_minor,
                        currency,
                        settled_on,
                        reason: WRONG_ACCOUNT.to_string(),
                        format: Default::default(),
                        ordering: None,
                    }),
                },
                about: Some(About::Misdirection(id.clone())),
            });
        }
        let reference = self.reference_of(&id);
        self.tell(
            format!("misdirected-{id}"),
            Role::Collections,
            "payee_debt_case",
            format!(
                "payment {item_id} on policy {policy} went to the wrong account ({amount_minor} of it); the bank is asked to recall it. \
                 Arrange repayment with the holder, quoting {reference}. Never take it from another payment."
            ),
        );
        if refund.is_some() {
            self.tell(
                format!("misdirected-refund-{id}"),
                Role::Maker,
                "refund_owed_again",
                format!("the refund paid by {item_id} is owed again once the core records the debt; pay it once the account is put right"),
            );
        }
        tx.emit(Event::MisdirectionReported {
            misdirection: id,
            item: item_id.to_string(),
            amount_minor,
        });
        Ok(())
    }

    pub(crate) fn record_recall_answer(&mut self, tx: &mut Tx<'_>, id: &str, accepted: bool, reason: Option<String>) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let reference = self.reference_of(id);
        let m = self
            .misdirections
            .get_mut(id)
            .ok_or_else(|| invalid("unknown_recall", format!("no recall {id} was asked for")))?;
        let item = m.item.clone();
        if accepted {
            m.recall = RecallStatus::Accepted;
        } else {
            let reason = reason.unwrap_or_else(|| "no reason given".to_string());
            m.recall = RecallStatus::Refused(reason.clone());
            self.tell(
                format!("recall-refused-{id}"),
                Role::Collections,
                "recall_refused",
                format!("the bank will not recall payment {item} ({reason}); arrange repayment with the holder, quoting {reference}"),
            );
        }
        tx.emit(Event::RecallAnswered {
            misdirection: id.to_string(),
            accepted,
        });
        Ok(())
    }

    pub(crate) fn write_off_misdirected(&mut self, tx: &mut Tx<'_>, id: &str, amount_minor: i64, reason: String) -> Result<(), Reject> {
        tx.need(Role::Approver)?;
        let me = tx.actor.id.clone();
        let m = self.misdirection_mut(id)?;
        if m.reported_by == me {
            return Err(invalid("same_person", "the person who reported it cannot write it off"));
        }
        if reason.is_empty() || !reason.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return Err(invalid("invalid_reason", "the reason is a plain code, such as holder_untraceable"));
        }
        if amount_minor <= 0 || amount_minor > m.outstanding_minor {
            return Err(invalid("invalid_amount", "more than nothing, and no more than is owed"));
        }
        m.write_offs += 1;
        let (policy, currency, n) = (m.policy.clone(), m.currency, m.write_offs);
        self.push_outbox(OutboxEntry {
            key: format!("{WRITE_OFF}{id}-{n}"),
            instruction: Instruction::CoreAt {
                policy: policy.clone(),
                caller: "payments".to_string(),
                command: Box::new(ic_core::Command::PayeeDebt(Box::new(PayeeDebtCommand::WriteOff {
                    policy,
                    debt: id.to_string(),
                    amount: Money::new(amount_minor, currency),
                    reason,
                }))),
            },
            about: Some(About::Misdirection(id.to_string())),
        });
        Ok(())
    }

    /// The core changed a payee debt. The repayment reference follows what
    /// is owed; the misdirection closes when nothing is.
    pub(crate) fn payee_debt_changed(&mut self, tx: &mut Tx<'_>, policy: &str, debt: &str, outstanding_minor: i64) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if policy.is_empty() || debt.is_empty() {
            return Err(invalid("invalid_debt", "a payee debt names its policy"));
        }
        self.repayables.hold(
            "RP",
            Repayable {
                claim: String::new(),
                overpayment: format!("debt:{debt}"),
                policy: policy.to_string(),
                outstanding_minor,
            },
        );
        let Some(m) = self.misdirections.get_mut(debt).filter(|m| !m.closed) else {
            return Ok(());
        };
        m.outstanding_minor = outstanding_minor;
        if outstanding_minor <= 0 {
            m.closed = true;
            tx.emit(Event::MisdirectionClosed {
                misdirection: debt.to_string(),
            });
        }
        Ok(())
    }

    /// The core or the bank answered an instruction about a misdirection.
    pub(crate) fn misdirection_done(&mut self, id: &str, key: &str, refused: Option<String>) {
        let Some(m) = self.misdirections.get_mut(id) else { return };
        let item = m.item.clone();
        if key == format!("{RAISE}{id}") && refused.is_none() {
            // The core owes the refund again: it waits to be paid once more.
            let refund = self.item(&item).and_then(|i| i.new.refund.clone());
            if let Some(r) = refund.and_then(|r| self.refunds.get_mut(&r)) {
                r.status = crate::model::RefundStatus::Raised;
            }
            return;
        }
        let Some(m) = self.misdirections.get_mut(id) else { return };
        if key.starts_with(RECALL) {
            m.recall = match &refused {
                Some(code) => RecallStatus::NotSent(code.clone()),
                None => RecallStatus::Asked,
            };
        }
        if let Some(code) = refused {
            let what = if key.starts_with(RECALL) { "the bank" } else { "the core" };
            self.tell(
                format!("refused-{key}"),
                Role::Reconciler,
                "misdirection_refused",
                format!("{key} for payment {item}: {what} refused it ({code}); misdirection {id} needs a person"),
            );
        }
    }

    /// The bank sent back a payment that was recalled. It is recovered
    /// against the debt; any more than is owed is for a person.
    pub(crate) fn recalled_back(&mut self, id: &str, returned_on: NaiveDate) {
        let Some(m) = self.misdirections.get(id) else { return };
        let (item_id, policy, currency, owed) = (m.item.clone(), m.policy.clone(), m.currency, m.outstanding_minor);
        let Some(run) = self.items.get(&item_id).cloned() else { return };
        let Some(item) = self.runs.get_mut(&run).and_then(|r| r.items.get_mut(&item_id)) else {
            return;
        };
        item.status = ItemStatus::Returned(crate::model::ReturnReason::Other("recalled".to_string()));
        let (amount, submission) = (item.new.amount_minor, item.submission.clone());
        if let Some(account) = submission.and_then(|s| self.submissions.get(&s)).map(|s| s.account.clone()) {
            self.books.entry(account).or_default().expected.insert(
                format!("{RECALL}{item_id}"),
                Expected {
                    amount_minor: amount,
                    since: returned_on,
                },
            );
        }
        let recovered = amount.min(owed);
        if recovered > 0 {
            self.push_outbox(OutboxEntry {
                key: format!("{RECOVER}{item_id}"),
                instruction: Instruction::CoreAt {
                    policy: policy.clone(),
                    caller: "payments".to_string(),
                    command: Box::new(ic_core::Command::PayeeDebt(Box::new(PayeeDebtCommand::Recover {
                        policy,
                        debt: id.to_string(),
                        receipt: format!("{RECALL}{item_id}"),
                        amount: Money::new(recovered, currency),
                    }))),
                },
                about: Some(About::Misdirection(id.to_string())),
            });
        }
        if amount > recovered {
            self.tell(
                format!("recall-excess-{item_id}"),
                Role::Reconciler,
                "recall_excess",
                format!("the recall of {item_id} brought back {amount}; only {recovered} was owed on misdirection {id}"),
            );
        }
    }
}
