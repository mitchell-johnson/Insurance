//! Statements and payment. A statement is a deterministic rendering of what
//! the core confirmed for a payee since the last cut, by the module's own
//! sequence, so the statement and the sweep cover exactly the same events.
//! Payment is the payee's net balance: each partition's balance is swept to
//! a clearing account, one payment is asked of the payments module, and the
//! clearing accounts return to zero when it settles.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyAccount, CounterpartyLine, CounterpartyPosting, InternalLedger, PartyLedger};
use ic_core::{PartyId, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_money::Currency;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::*;
use crate::rules::{portion, Component};
use crate::state::{reject, CaseKind, Ctx, DistributionState, Event};
use crate::structure::PartyKind;
use crate::{BookId, EngagementId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementLine {
    pub date: NaiveDate,
    pub reference: String,
    pub kind: String,
    pub ledger: PartyLedger,
    pub policy: Option<PolicyId>,
    pub book: Option<BookId>,
    pub layer: Option<String>,
    pub component: Option<Component>,
    /// The engagement that earned it, so a provider paid gross can pay its people.
    pub earner: Option<EngagementId>,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentPart {
    pub reference: String,
    pub account_ref: String,
    pub amount_minor: i64,
    /// None while the payments module has it; then settled or returned.
    pub settled: Option<bool>,
}

/// Immutable once issued. Corrections appear on the next statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    pub payee: PartyId,
    pub number: u32,
    pub date: NaiveDate,
    /// The module sequence the statement was cut at.
    pub cut_seq: u64,
    pub opening_minor: i64,
    pub lines: Vec<StatementLine>,
    pub closing_minor: i64,
    /// Held for the payee and not paid, with the reasons.
    pub held_minor: i64,
    pub held_reasons: Vec<String>,
    pub retention_minor: i64,
    pub advance_outstanding_minor: i64,
    pub advance_recovered_minor: i64,
    /// Why nothing was paid, where payments to the payee are stopped.
    pub payment_held: Option<String>,
    pub payments: Vec<PaymentPart>,
    /// Tax withheld from this payment and owed to the revenue authority.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub withheld_minor: i64,
    pub hash: String,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PaymentOwner {
    Statement,
    Action,
}

/// A payment in the hands of the payments module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PaymentRecord {
    pub owner: PaymentOwner,
    pub owner_id: String,
    pub party: PartyId,
    pub currency: Currency,
    pub parts: Vec<PaymentPart>,
    /// What was swept to clearing on each partition.
    pub clearing: Vec<(PartitionId, i64)>,
    /// Where a returned item goes back to.
    pub return_to: PartyLedger,
    /// The tax withheld on each partition, given back in proportion to
    /// what comes back unpaid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withheld: Vec<(PartitionId, i64)>,
}

/// The kind of withholding on commission: a schedular payment.
pub const SCHEDULAR: &str = "schedular";

/// The posting kind, which finance's tax reconciliation lists for the account.
pub const WITHHOLDING_KIND: &str = "withholding_schedular";

fn tax_account() -> CounterpartyAccount {
    CounterpartyAccount::Internal(InternalLedger::WithholdingTax { kind: SCHEDULAR.to_string() })
}

/// Tax withheld from a payee's payment: out of clearing, owed to the
/// revenue authority.
pub(crate) fn withholding_lines(payee: &PartyId, tax: i64) -> Vec<CounterpartyLine> {
    two_lines(party(payee, PartyLedger::Clearing), tax_account(), tax)
}

fn party(p: &PartyId, ledger: PartyLedger) -> CounterpartyAccount {
    CounterpartyAccount::Party { party: p.clone(), ledger }
}

pub(crate) fn two_lines(debit: CounterpartyAccount, credit: CounterpartyAccount, amount_minor: i64) -> Vec<CounterpartyLine> {
    vec![
        CounterpartyLine { account: debit, amount_minor },
        CounterpartyLine {
            account: credit,
            amount_minor: -amount_minor,
        },
    ]
}

pub(crate) fn simple_posting(reference: String, kind: &str, currency: Currency, lines: Vec<CounterpartyLine>, owner: &str) -> CounterpartyPosting {
    let mut detail = BTreeMap::new();
    detail.insert("owner".to_string(), owner.to_string());
    CounterpartyPosting {
        reference,
        kind: kind.to_string(),
        currency,
        value_date: None,
        policy: None,
        reverses: None,
        lines,
        detail,
    }
}

impl DistributionState {
    /// A party's balance on one kind of account, across partitions. Positive
    /// is in the party's favour.
    pub fn balance(&self, party: &PartyId, ledger: PartyLedger) -> i64 {
        self.by_partition(party, ledger).iter().map(|(_, v)| v).sum()
    }

    pub fn by_partition(&self, party: &PartyId, ledger: PartyLedger) -> Vec<(PartitionId, i64)> {
        self.positions
            .get(party)
            .into_iter()
            .flatten()
            .filter_map(|(p, ledgers)| ledgers.get(&ledger).copied().filter(|v| *v != 0).map(|v| (*p, v)))
            .collect()
    }

    /// Cut a payee's statement and, where money is due and may be paid,
    /// sweep it and ask for the payment.
    pub(crate) fn cut_statement(&mut self, ctx: &mut Ctx, payee_id: &PartyId) -> Result<(), Reject> {
        let today = self.date()?;
        let payee = self
            .payees
            .get(payee_id)
            .ok_or_else(|| Reject::new("unknown_payee", format!("{payee_id} is not a registered payee")))?
            .clone();
        if self.payments.values().any(|p| &p.party == payee_id && p.owner == PaymentOwner::Statement) {
            return reject("payment_in_progress", "the payee's last payment has not settled or returned yet");
        }
        if self.requested.values().any(|r| {
            r.state == PostingState::Pending
                && r.posting
                    .lines
                    .iter()
                    .any(|l| matches!(&l.account, CounterpartyAccount::Party { party, .. } if party == payee_id))
        }) {
            return reject(
                "postings_in_flight",
                "postings for the payee are still on their way to the ledger; cut again when they land",
            );
        }
        let number = payee.statements + 1;
        let id = format!("{payee_id}.{number}");
        let cut_seq = self.seq;
        let lines: Vec<StatementLine> = self
            .movements
            .iter()
            .filter(|m| &m.party == payee_id && m.seq > payee.statement_cut && m.seq <= cut_seq)
            .flat_map(|m| {
                let allocation = self.allocations.get(&m.reference);
                let want = if m.ledger == PartyLedger::Held { None } else { Some(Dest::Payable) };
                let detail: Vec<&AllocLine> = allocation
                    .map(|a| {
                        a.lines
                            .iter()
                            .filter(|l| &l.payee == payee_id && (want.is_none() == matches!(l.dest, Dest::Held(_))))
                            .collect()
                    })
                    .unwrap_or_default();
                let policy = allocation.map(|a| a.policy.clone());
                // Break the movement down where the detail accounts for all of it.
                if !detail.is_empty() && detail.iter().map(|l| l.amount_minor).sum::<i64>() == m.amount_minor && m.ledger != PartyLedger::Clearing {
                    detail
                        .into_iter()
                        .map(|l| StatementLine {
                            date: m.date,
                            reference: m.reference.clone(),
                            kind: m.kind.clone(),
                            ledger: m.ledger,
                            policy: policy.clone(),
                            book: l.book.clone(),
                            layer: l.layer.clone(),
                            component: l.component.clone(),
                            earner: l.earner.clone(),
                            amount_minor: l.amount_minor,
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![StatementLine {
                        date: m.date,
                        reference: m.reference.clone(),
                        kind: m.kind.clone(),
                        ledger: m.ledger,
                        policy,
                        book: None,
                        layer: None,
                        component: None,
                        earner: None,
                        amount_minor: m.amount_minor,
                    }]
                }
            })
            .collect();
        let closing = self.balance(payee_id, PartyLedger::CommissionPayable);
        let activity: i64 = lines
            .iter()
            .filter(|l| l.ledger == PartyLedger::CommissionPayable)
            .map(|l| l.amount_minor)
            .sum();
        let held_minor = self.balance(payee_id, PartyLedger::Held);
        let mut held_reasons: Vec<String> = self
            .held
            .iter()
            .filter(|h| &h.party == payee_id && h.remaining_minor > 0)
            .map(|h| h.reason.map_or("other".to_string(), |r| format!("{r:?}")))
            .collect();
        held_reasons.sort();
        held_reasons.dedup();
        let advance_owed = -self.balance(payee_id, PartyLedger::Advance);
        let impaired = self.config.structure.payee_impairment(payee_id, today).is_some();
        let payment_held = payee
            .payment_hold
            .clone()
            .or_else(|| impaired.then(|| "licence not active".to_string()))
            .or_else(|| (closing > 0 && payee.default_account(today).is_none()).then(|| "no verified account past its cooling-off".to_string()));
        let currency = Currency::NZD;
        let mut statement = Statement {
            payee: payee_id.clone(),
            number,
            date: today,
            cut_seq,
            opening_minor: closing - activity,
            lines,
            closing_minor: closing,
            held_minor,
            held_reasons,
            retention_minor: self.balance(payee_id, PartyLedger::Retention),
            advance_outstanding_minor: advance_owed.max(0),
            advance_recovered_minor: 0,
            payment_held: payment_held.clone(),
            payments: vec![],
            withheld_minor: 0,
            hash: String::new(),
        };
        if closing > 0 && payment_held.is_none() {
            // Sweep every partition's balance, in either direction.
            let swept = self.by_partition(payee_id, PartyLedger::CommissionPayable);
            for (partition, amount) in &swept {
                let lines = two_lines(party(payee_id, PartyLedger::CommissionPayable), party(payee_id, PartyLedger::Clearing), *amount);
                let posting = simple_posting(format!("sweep:{id}:{partition}"), "commission_swept", currency, lines, &id);
                self.request(ctx, *partition, posting, None);
            }
            let mut clearing = swept;
            // Advances are recovered first, at no more than the agreed share.
            let mut payable = closing;
            let recover = advance_owed.max(0).min(portion(closing, payee.advance_recovery_ppm));
            if recover > 0 {
                let mut left = recover;
                for (partition, owed) in self.by_partition(payee_id, PartyLedger::Advance) {
                    let take = left.min(-owed);
                    if take <= 0 {
                        continue;
                    }
                    let lines = two_lines(party(payee_id, PartyLedger::Clearing), party(payee_id, PartyLedger::Advance), take);
                    let posting = simple_posting(format!("advrec:{id}:{partition}"), "advance_recovered", currency, lines, &id);
                    self.request(ctx, partition, posting, None);
                    match clearing.iter_mut().find(|(p, _)| *p == partition) {
                        Some((_, c)) => *c -= take,
                        None => clearing.push((partition, -take)),
                    }
                    left -= take;
                }
                statement.advance_recovered_minor = recover - left;
                payable -= recover - left;
            }
            let withheld = self.withhold(payee_id, payable, &mut clearing);
            for (partition, tax) in &withheld {
                let posting = simple_posting(
                    format!("wht:{id}:{partition}"),
                    WITHHOLDING_KIND,
                    currency,
                    withholding_lines(payee_id, *tax),
                    &id,
                );
                self.request(ctx, *partition, posting, None);
            }
            statement.withheld_minor = withheld.iter().map(|(_, t)| t).sum();
            payable -= statement.withheld_minor;
            // Instructions on the payee decide where the money goes: legal
            // priority first, then assignments, then the payee's own account.
            let mut left = payable;
            let mut parts = vec![];
            for i in payee.instructions.iter().filter(|i| i.from <= today && i.until.is_none_or(|u| today <= u)) {
                let want = i.amount_minor.unwrap_or_else(|| portion(payable, i.share_ppm.unwrap_or(0)));
                let take = want.min(left);
                if take > 0 {
                    parts.push((i.to_account_ref.clone(), take));
                    left -= take;
                }
            }
            if left > 0 {
                if let Some(a) = payee.default_account(today) {
                    parts.push((a.account_ref.clone(), left));
                }
            }
            statement.payments = parts
                .into_iter()
                .enumerate()
                .map(|(n, (account_ref, amount_minor))| PaymentPart {
                    reference: format!("pay:{id}:{}", n + 1),
                    account_ref,
                    amount_minor,
                    settled: None,
                })
                .collect();
            for p in &statement.payments {
                ctx.emit(Event::PaymentRequested {
                    reference: p.reference.clone(),
                    party: payee_id.clone(),
                    account_ref: p.account_ref.clone(),
                    amount_minor: p.amount_minor,
                    currency,
                });
            }
            self.payments.insert(
                id.clone(),
                PaymentRecord {
                    owner: PaymentOwner::Statement,
                    owner_id: id.clone(),
                    party: payee_id.clone(),
                    currency,
                    parts: statement.payments.clone(),
                    clearing,
                    return_to: PartyLedger::CommissionPayable,
                    withheld,
                },
            );
        }
        if closing < 0 {
            // A debit balance is recovered from future commission first. Aged
            // beyond the threshold it becomes a case with a repayment arrangement.
            let since = self
                .statements
                .values()
                .filter(|s| &s.payee == payee_id)
                .rev()
                .take_while(|s| s.closing_minor < 0)
                .map(|s| s.date)
                .min();
            let case = format!("debt-{payee_id}");
            if since.is_some_and(|d| (today - d).num_days() >= self.settings.debt_case_after_days as i64)
                && self.cases.get(&case).is_none_or(|c| c.closed.is_some())
            {
                self.cases.insert(
                    case.clone(),
                    crate::state::Case {
                        id: case.clone(),
                        kind: CaseKind::Debt,
                        owner: crate::authority::Role::CounterpartyFinance,
                        subject: payee_id.clone(),
                        opened: today,
                        source_ref: id.clone(),
                        engagements: Default::default(),
                        payees: [payee_id.clone()].into(),
                        policies: Default::default(),
                        workstreams: vec!["repayment_arrangement".into()],
                        closed: None,
                    },
                );
                ctx.emit(Event::CaseOpened {
                    case,
                    kind: CaseKind::Debt,
                    subject: payee_id.clone(),
                });
            }
        }
        let mut h = Sha256::new();
        h.update(b"ic.distribution.statement.v1");
        h.update(serde_json::to_vec(&statement).unwrap_or_default());
        statement.hash = hex::encode(h.finalize());
        ctx.emit(Event::StatementIssued {
            payee: payee_id.clone(),
            number,
            closing_minor: closing,
            hash: statement.hash.clone(),
        });
        if let Some(p) = self.payees.get_mut(payee_id) {
            p.statement_cut = cut_seq;
            p.statements = number;
        }
        self.statements.insert(id, statement);
        Ok(())
    }

    /// The rate tax is withheld at from commission paid to a payee, in
    /// basis points: the payee's declared rate; for a natural person who
    /// declared nothing, the no-declaration rate; otherwise none.
    pub fn withholding_bp(&self, payee: &PartyId) -> u32 {
        if !self.settings.commission_withholding {
            return 0;
        }
        match self.payees.get(payee).map(|p| p.withholding).unwrap_or_default() {
            Withholding::Rate(bp) => bp,
            Withholding::Exempt => 0,
            Withholding::Undeclared => {
                let person = self.config.structure.parties.get(payee).is_some_and(|p| p.kind == PartyKind::Adviser);
                if person {
                    self.settings.no_declaration_withholding_bp
                } else {
                    0
                }
            }
        }
    }

    /// The tax to withhold from `gross` about to be paid out of clearing,
    /// rounded down, taken from the partitions holding the money; clearing
    /// is reduced by it. Returns the tax on each partition, which the caller
    /// posts with [`withholding_lines`] after the money reaches clearing.
    pub(crate) fn withhold(&self, payee: &PartyId, gross: i64, clearing: &mut [(PartitionId, i64)]) -> Vec<(PartitionId, i64)> {
        let bp = self.withholding_bp(payee);
        let mut left = (gross.max(0) as i128 * bp as i128 / 10_000) as i64;
        let mut taken = vec![];
        for (partition, amount) in clearing.iter_mut() {
            let take = left.min(*amount);
            if take <= 0 {
                continue;
            }
            *amount -= take;
            left -= take;
            taken.push((*partition, take));
        }
        taken
    }

    fn part_mut(&mut self, reference: &str) -> Result<(String, &mut PaymentRecord), Reject> {
        let key = self
            .payments
            .iter()
            .find(|(_, r)| r.parts.iter().any(|p| p.reference == reference))
            .map(|(k, _)| k.clone())
            .ok_or_else(|| Reject::new("unknown_payment", format!("payment {reference} is not outstanding")))?;
        let record = self
            .payments
            .get_mut(&key)
            .ok_or_else(|| Reject::new("unknown_payment", "the payment is gone"))?;
        Ok((key, record))
    }

    pub(crate) fn payment_settled(&mut self, ctx: &mut Ctx, reference: &str) -> Result<(), Reject> {
        self.payment_outcome(ctx, reference, true, None)
    }

    pub(crate) fn payment_returned(&mut self, ctx: &mut Ctx, reference: &str, reason: String) -> Result<(), Reject> {
        self.payment_outcome(ctx, reference, false, Some(reason))
    }

    fn payment_outcome(&mut self, ctx: &mut Ctx, reference: &str, settled: bool, reason: Option<String>) -> Result<(), Reject> {
        let today = self.date()?;
        let (key, record) = self.part_mut(reference)?;
        let part = record.parts.iter_mut().find(|p| p.reference == reference).filter(|p| p.settled.is_none());
        let Some(part) = part else {
            return reject("payment_settled", "that payment has already settled or returned");
        };
        part.settled = Some(settled);
        let done = record.parts.iter().all(|p| p.settled.is_some());
        let record = record.clone();
        match reason {
            None => ctx.emit(Event::PaymentCompleted {
                reference: reference.to_string(),
            }),
            Some(reason) => {
                // A returned item goes back to the account it came from, with
                // a task, never to suspense.
                self.task(ctx, "payment_returned", &record.party.0, today, reason.clone());
                ctx.emit(Event::PaymentFailed {
                    reference: reference.to_string(),
                    reason,
                });
            }
        }
        if !done {
            return Ok(());
        }
        // Every part is back. Clear each partition's clearing account: what
        // settled goes to cash, what returned goes back where it came from.
        let mut returned: i64 = record.parts.iter().filter(|p| p.settled == Some(false)).map(|p| p.amount_minor).sum();
        let owner = (record.owner == PaymentOwner::Action).then(|| record.owner_id.clone());
        // Tax withheld on money that came back was never paid to the payee:
        // it goes back with it, in proportion, so the next payment withholds
        // once on the whole.
        let paid: i64 = record.parts.iter().map(|p| p.amount_minor).sum();
        let withheld: i64 = record.withheld.iter().map(|(_, t)| t).sum();
        if returned > 0 && withheld > 0 && paid > 0 {
            let mut back = (withheld as i128 * returned as i128 / paid as i128) as i64;
            for (partition, tax) in &record.withheld {
                let take = back.min(*tax);
                if take <= 0 {
                    continue;
                }
                back -= take;
                let lines = two_lines(tax_account(), party(&record.party, record.return_to), take);
                let posting = simple_posting(format!("whtback:{key}:{partition}"), WITHHOLDING_KIND, record.currency, lines, &key);
                self.request(ctx, *partition, posting, owner.clone());
            }
        }
        for (partition, amount) in record.clearing.iter().filter(|(_, a)| *a != 0) {
            let back = if *amount > 0 { returned.min(*amount) } else { 0 };
            returned -= back;
            let mut lines = vec![CounterpartyLine {
                account: party(&record.party, PartyLedger::Clearing),
                amount_minor: *amount,
            }];
            if back != 0 {
                lines.push(CounterpartyLine {
                    account: party(&record.party, record.return_to),
                    amount_minor: -back,
                });
            }
            if *amount - back != 0 {
                lines.push(CounterpartyLine {
                    account: CounterpartyAccount::Internal(InternalLedger::Settlement),
                    amount_minor: -(*amount - back),
                });
            }
            let posting = simple_posting(format!("settle:{key}:{partition}"), "counterparty_paid", record.currency, lines, &key);
            self.request(ctx, *partition, posting, owner.clone());
        }
        self.payments.remove(&key);
        Ok(())
    }
}
