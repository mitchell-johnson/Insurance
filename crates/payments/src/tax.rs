//! Paying withheld tax to Inland Revenue.
//!
//! Tax kept back from claims benefits, commission and interest sits in the
//! core, on each partition and statutory fund, in
//! `withholding_tax_payable/{kind}`. Each month a maker asks for a kind's
//! remittance for a closed period (`request_tax_remittance`), naming the
//! revenue's payee. The pump asks finance what is due
//! (`withholding_due`: the figure the books closed on, and the due date) and
//! each partition what it holds by fund, and records both
//! (`price_tax_remittance`). The shares must add up to finance's figure, or
//! nothing is paid and treasury is told. When they do, the payment waits
//! for a maker to put it in a `tax_and_levies` run like any other, under the
//! remittance's ID. When it settles, each partition is told its share
//! (`RemitWithholding`, payment `{remittance}:{n}`), which discharges the
//! liability against the bank account; a return puts it back
//! (`ReturnRemittance`).

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::intake::Waiting;
use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{About, Id, Instruction, NewItem, OutboxEntry, Reject, Role};

/// Source of a tax remittance's payment: `tax:{kind}:{period}`.
pub(crate) const SOURCE: &str = "tax:";

/// What one partition holds of a kind of tax, in one statutory fund.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    pub partition: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fund: Option<String>,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaxStatus {
    /// Waiting for the pump to find what is due and who holds it.
    Requested,
    /// The partitions' shares do not add up to what finance says is due.
    Mismatch { due_minor: i64, held_minor: i64 },
    /// Waiting to be paid.
    Priced,
    /// The revenue has it; the partitions were told.
    Paid,
    /// The bank sent it back; the partitions owe it again.
    Returned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxRemittance {
    pub kind: String,
    pub period: String,
    pub payee: Id,
    pub requested_by: String,
    pub status: TaxStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_on: Option<NaiveDate>,
    #[serde(default)]
    pub shares: Vec<Share>,
}

fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A return period: a calendar month, `YYYY-MM`.
fn month(period: &str) -> bool {
    let b = period.as_bytes();
    b.len() == 7
        && b[4] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || c.is_ascii_digit())
        && period[5..].parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))
}

impl PaymentsState {
    pub(crate) fn request_tax_remittance(&mut self, tx: &mut Tx<'_>, id: Id, kind: String, period: String, payee: Id) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        if !crate::iso20022::fits_identifier(&id) || id.contains(':') || !plain(&kind) || !month(&period) {
            return Err(Reject::new(
                "invalid_remittance",
                "an ID the bank can carry, a plain kind such as rwt, and a period YYYY-MM",
            ));
        }
        if !self.payees.get(&payee).is_some_and(|p| !p.closed) {
            return Err(Reject::new("unknown_payee", format!("there is no payee {payee}")));
        }
        if self.tax_remittances.contains_key(&id) || self.items.contains_key(&id) || self.waiting.contains_key(&id) {
            return Err(Reject::new("duplicate_remittance", format!("{id} is already used")));
        }
        let live = self
            .tax_remittances
            .values()
            .any(|t| t.kind == kind && t.period == period && t.status != TaxStatus::Returned);
        if live {
            return Err(Reject::new("duplicate_remittance", format!("{kind} for {period} is already being paid")));
        }
        self.tax_remittances.insert(
            id.clone(),
            TaxRemittance {
                kind,
                period,
                payee,
                requested_by: tx.actor.id.clone(),
                status: TaxStatus::Requested,
                due_on: None,
                shares: Vec::new(),
            },
        );
        tx.emit(Event::TaxRemittanceRequested { remittance: id });
        Ok(())
    }

    /// Remittances the pump has to find figures for.
    pub fn tax_remittances_to_price(&self) -> Vec<(&Id, &TaxRemittance)> {
        self.tax_remittances
            .iter()
            .filter(|(_, t)| matches!(t.status, TaxStatus::Requested | TaxStatus::Mismatch { .. }))
            .collect()
    }

    pub(crate) fn price_tax_remittance(&mut self, tx: &mut Tx<'_>, id: &str, due_minor: i64, due_on: NaiveDate, shares: Vec<Share>) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let t = self
            .tax_remittances
            .get(id)
            .filter(|t| matches!(t.status, TaxStatus::Requested | TaxStatus::Mismatch { .. }))
            .ok_or_else(|| Reject::new("unknown_remittance", format!("remittance {id} is not waiting for figures")))?;
        let shares: Vec<Share> = shares.into_iter().filter(|s| s.amount_minor != 0).collect();
        let held = shares.iter().try_fold(0i64, |sum, s| sum.checked_add(s.amount_minor));
        let (kind, period, payee) = (t.kind.clone(), t.period.clone(), t.payee.clone());
        let right = held == Some(due_minor) && due_minor > 0 && shares.iter().all(|s| s.amount_minor > 0);
        if !right {
            let held_minor = held.unwrap_or(i64::MAX);
            let first = t.status != (TaxStatus::Mismatch { due_minor, held_minor });
            if let Some(t) = self.tax_remittances.get_mut(id) {
                t.status = TaxStatus::Mismatch { due_minor, held_minor };
            }
            if first {
                self.push_outbox(OutboxEntry {
                    key: format!("tax-mismatch-{id}-{due_minor}-{held_minor}"),
                    instruction: Instruction::Notify {
                        to: Role::Treasury,
                        subject: "tax_shares_do_not_add_up".to_string(),
                        detail: format!(
                            "{kind} for {period}: finance says {due_minor} is due, the partitions hold {held_minor}; nothing is paid until they agree"
                        ),
                    },
                    about: None,
                });
            }
            return Ok(());
        }
        if let Some(t) = self.tax_remittances.get_mut(id) {
            t.status = TaxStatus::Priced;
            t.due_on = Some(due_on);
            t.shares = shares;
        }
        self.waiting.insert(
            id.to_string(),
            Waiting {
                payment: id.to_string(),
                source: format!("{SOURCE}{kind}:{period}"),
                payee,
                account_ref: String::new(),
                policy: None,
                claim: None,
                run: None,
                amount_minor: due_minor,
                payable: None,
            },
        );
        tx.emit(Event::PaymentOffered {
            payment: id.to_string(),
            source: format!("{SOURCE}{kind}:{period}"),
            amount_minor: due_minor,
        });
        Ok(())
    }

    fn remittance_commands(&mut self, n: &NewItem, returned: bool, bank_account: &str) {
        let Some(t) = self.tax_remittances.get(&n.item) else { return };
        let (kind, period, shares) = (t.kind.clone(), t.period.clone(), t.shares.clone());
        for (i, share) in shares.into_iter().enumerate() {
            let payment = format!("{}:{}", n.item, i + 1);
            let command = if returned {
                ic_core::Command::ReturnRemittance {
                    kind: kind.clone(),
                    payment: payment.clone(),
                }
            } else {
                ic_core::Command::RemitWithholding(Box::new(ic_core::remittance::Remittance {
                    kind: kind.clone(),
                    period: period.clone(),
                    payment: payment.clone(),
                    amount: ic_money::Money::new(share.amount_minor, ic_money::Currency::NZD),
                    bank_account: bank_account.to_string(),
                    statutory_fund: share.fund.clone(),
                }))
            };
            let what = if returned { "returned" } else { "remitted" };
            self.push_outbox(OutboxEntry {
                key: format!("tax-{what}-{payment}"),
                instruction: Instruction::CorePartition {
                    partition: share.partition,
                    caller: "payments".to_string(),
                    command: Box::new(command),
                },
                about: Some(About::Item(n.item.clone())),
            });
        }
    }

    /// The revenue's payment settled: each partition discharges its share.
    pub(crate) fn tax_settled(&mut self, n: &NewItem, bank_account: &str) {
        if !n.source.starts_with(SOURCE) {
            return;
        }
        self.remittance_commands(n, false, bank_account);
        if let Some(t) = self.tax_remittances.get_mut(&n.item) {
            t.status = TaxStatus::Paid;
        }
    }

    /// The bank sent the revenue's payment back after it settled: each
    /// partition owes its share again, and the remittance may be asked for
    /// again under a new ID.
    pub(crate) fn tax_returned(&mut self, n: &NewItem) {
        if !self.tax_remittances.get(&n.item).is_some_and(|t| t.status == TaxStatus::Paid) {
            return;
        }
        self.remittance_commands(n, true, "");
        if let Some(t) = self.tax_remittances.get_mut(&n.item) {
            t.status = TaxStatus::Returned;
        }
    }
}

/// What one partition holds of a kind of tax in one fund, as the core
/// answers it: partition, fund, minor units.
pub type Held = (u16, Option<String>, i64);

/// Split the period's figure between the partitions and funds holding the
/// tax. The core keeps a running balance, not one per month, so a month's
/// payment is taken from each holding in proportion to what it holds; that
/// is a best estimate until the core can answer by period. Every share is
/// at most its holding and the shares add up to `due_minor` exactly, the
/// odd cents going to the largest remainders, ties to the first listed.
/// Where less is held than is due, the holdings are returned as they are,
/// and the module stops the payment for treasury.
pub fn shares_for(due_minor: i64, held: &[Held]) -> Vec<Share> {
    let held: Vec<&Held> = held.iter().filter(|(_, _, minor)| *minor > 0).collect();
    let total: i128 = held.iter().map(|(_, _, minor)| i128::from(*minor)).sum();
    let share = |(partition, fund, _): &Held, amount_minor: i64| Share {
        partition: *partition,
        fund: fund.clone(),
        amount_minor,
    };
    if due_minor <= 0 || total <= i128::from(due_minor) {
        return held.iter().map(|h| share(h, h.2)).collect();
    }
    let due = i128::from(due_minor);
    let mut parts: Vec<(i64, i128)> = held
        .iter()
        .map(|(_, _, minor)| {
            let exact = due * i128::from(*minor);
            // Below `minor` because due < total, so it fits.
            ((exact / total) as i64, exact % total)
        })
        .collect();
    let given: i128 = parts.iter().map(|(floor, _)| i128::from(*floor)).sum();
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by(|a, b| parts[*b].1.cmp(&parts[*a].1).then(a.cmp(b)));
    for i in order.into_iter().take((due - given) as usize) {
        parts[i].0 += 1;
    }
    held.iter()
        .zip(parts)
        .filter(|(_, (amount, _))| *amount > 0)
        .map(|(h, (amount, _))| share(h, amount))
        .collect()
}
