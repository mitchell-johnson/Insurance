//! Paying withheld tax to the revenue authority.
//!
//! Tax kept back from payments sits on each partition as a liability by
//! kind. When the payments module pays the authority for a period, it tells
//! each partition its share: the liability is discharged against the bank
//! account the money left from. A payment the bank sends back puts the
//! liability on again.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{Dimensions, EntryId, Ledger};
use ic_money::Money;
use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::postings::PostingSet;

/// One partition's share of a payment of withheld tax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remittance {
    /// The kind of tax, as it was withheld: `rwt`, say.
    pub kind: String,
    /// The return period the payment is for, `2027-11`.
    pub period: String,
    /// The payments module's own item ID.
    pub payment: String,
    pub amount: Money,
    pub bank_account: String,
    /// The statutory fund the tax was withheld in, where the payer splits
    /// the payment by fund. Both lines of the entry carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statutory_fund: Option<String>,
}

/// A remittance as it was posted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemittanceRecord {
    pub remittance: Remittance,
    pub remitted_on: NaiveDate,
    pub entry: Option<EntryId>,
    #[serde(default)]
    pub returned: bool,
}

/// Tax held, by liability account and then by statutory fund (empty for none).
pub type Withheld = BTreeMap<String, BTreeMap<String, i128>>;

/// One fund's share of a kind of withheld tax on a partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldByFund {
    pub statutory_fund: Option<String>,
    pub minor: i128,
}

/// Keep the per-fund tally in step with what a command posted.
pub(crate) fn tally(withheld: &mut Withheld, entries: &[ic_ledger::JournalEntry]) {
    for posting in entries.iter().flat_map(|e| &e.body.postings) {
        if !posting.account.0.starts_with("int/withholding_tax_payable/") {
            continue;
        }
        let fund = posting.dims.statutory_fund.clone().unwrap_or_default();
        let funds = withheld.entry(posting.account.0.clone()).or_default();
        // A liability: credits add to what is held.
        let held = funds.entry(fund.clone()).or_default();
        *held -= posting.signed();
        if *held == 0 {
            funds.remove(&fund);
        }
    }
    withheld.retain(|_, funds| !funds.is_empty());
}

/// What a partition holds of one kind of withheld tax, fund by fund.
pub fn held(withheld: &Withheld, kind: &str, c: ic_money::Currency) -> Vec<HeldByFund> {
    withheld
        .get(&accounts::withholding_tax_payable(kind, c).id.0)
        .into_iter()
        .flatten()
        .map(|(fund, minor)| HeldByFund {
            statutory_fund: Some(fund.clone()).filter(|f| !f.is_empty()),
            minor: *minor,
        })
        .collect()
}

fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
}

/// A return period is a calendar month, `YYYY-MM`.
pub(crate) fn valid_period(period: &str) -> bool {
    let b = period.as_bytes();
    b.len() == 7
        && b[4] == b'-'
        && b.iter().enumerate().all(|(i, ch)| i == 4 || ch.is_ascii_digit())
        && period[5..].parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))
}

fn dims(r: &Remittance) -> Dimensions {
    Dimensions {
        statutory_fund: r.statutory_fund.clone(),
        product: None,
        cover_type: None,
        posting_type: Some(format!("withholding_remittance_{}", r.kind)),
    }
}

fn detail(r: &Remittance) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("payment".to_string(), r.payment.clone()),
        ("bank_account".to_string(), r.bank_account.clone()),
        ("tax".to_string(), r.kind.clone()),
        ("period".to_string(), r.period.clone()),
    ])
}

pub(crate) fn remit(
    ledger: &mut Ledger,
    book: &mut BTreeMap<String, RemittanceRecord>,
    withheld: &Withheld,
    ctx: &mut Ctx<'_>,
    r: Remittance,
) -> Result<(), Reject> {
    let bad = |m: &str| Reject::new("invalid_remittance", m);
    if !plain(&r.kind) || !plain(&r.payment) || r.statutory_fund.as_deref().is_some_and(|f| !plain(f)) {
        return Err(bad("the kind of tax, the payment and the fund are plain identifiers"));
    }
    if !valid_period(&r.period) {
        return Err(bad("the period is a calendar month, YYYY-MM"));
    }
    if r.amount.minor <= 0 {
        return Err(bad("a remittance is a positive amount"));
    }
    crate::payments::valid_bank_account(&r.bank_account)?;
    if let Some(done) = book.get(&r.payment) {
        return if done.remittance == r && !done.returned {
            Ok(())
        } else {
            Err(Reject::new(
                "duplicate_payment",
                format!("payment {} has already remitted tax on other terms, or was returned", r.payment),
            ))
        };
    }
    let c = r.amount.currency;
    let payable = accounts::withholding_tax_payable(&r.kind, c);
    let amount = i128::from(r.amount.minor);
    // Never more than this partition holds, in all and in the fund named:
    // the liability does not go into debit, and no fund pays another's tax.
    let in_fund = withheld
        .get(&payable.id.0)
        .and_then(|funds| funds.get(r.statutory_fund.as_deref().unwrap_or_default()))
        .copied()
        .unwrap_or(0);
    if ledger.available(&payable.id) < amount || in_fund < amount {
        return Err(Reject::new(
            "over_remittance",
            format!("this partition holds less {} withheld in that fund than that", r.kind),
        ));
    }
    let mut set = PostingSet::new();
    set.debit(payable.clone(), amount, dims(&r));
    set.credit(accounts::bank(&r.bank_account, c), amount, dims(&r));
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "withholding_remitted", None, ctx.date, postings, detail(&r))?;
    let entry = ctx.entries.last().map(|e| e.id);
    ctx.emit(Event::WithholdingRemitted {
        kind: r.kind.clone(),
        period: r.period.clone(),
        payment: r.payment.clone(),
        amount_minor: r.amount.minor,
        bank_account: r.bank_account.clone(),
        outstanding_minor: ledger.available(&payable.id),
    });
    book.insert(
        r.payment.clone(),
        RemittanceRecord {
            remittance: r,
            remitted_on: ctx.date,
            entry,
            returned: false,
        },
    );
    Ok(())
}

/// The bank sent the payment back: the tax is owed again.
pub(crate) fn returned(ledger: &mut Ledger, book: &mut BTreeMap<String, RemittanceRecord>, ctx: &mut Ctx<'_>, kind: &str, payment: &str) -> Result<(), Reject> {
    let Some(rec) = book.get(payment).filter(|rec| rec.remittance.kind == kind).cloned() else {
        return Err(Reject::new(
            "unknown_payment",
            format!("payment {payment} has not remitted {kind} on this partition"),
        ));
    };
    if rec.returned {
        // Told twice: it is back on already.
        return Ok(());
    }
    let r = &rec.remittance;
    let c = r.amount.currency;
    let payable = accounts::withholding_tax_payable(&r.kind, c);
    let amount = i128::from(r.amount.minor);
    let mut set = PostingSet::new();
    set.debit(accounts::bank(&r.bank_account, c), amount, dims(r));
    set.credit(payable.clone(), amount, dims(r));
    let mut detail = detail(r);
    if let Some(entry) = rec.entry {
        detail.insert("reverses".to_string(), entry.to_string());
    }
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "withholding_remittance_returned", None, rec.remitted_on, postings, detail)?;
    if let Some(done) = book.get_mut(payment) {
        done.returned = true;
    }
    ctx.emit(Event::WithholdingRemittanceReturned {
        kind: r.kind.clone(),
        period: r.period.clone(),
        payment: payment.to_string(),
        amount_minor: r.amount.minor,
        bank_account: r.bank_account.clone(),
        outstanding_minor: ledger.available(&payable.id),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: a return period is accepted only as a calendar month written `YYYY-MM`.
    /// Why it matters: the period is how finance agrees what was paid to the tax return for
    /// that month. A free-text period ("Nov 27", "2027-13") would post and then match nothing.
    #[test]
    fn a_period_is_a_calendar_month() {
        for good in ["2027-11", "2028-01", "2027-12"] {
            assert!(valid_period(good), "{good}");
        }
        for bad in ["2027-13", "2027-00", "2027-1", "202711", "2027/11", "27-11", "2027-11-01", "abcd-ef", ""] {
            assert!(!valid_period(bad), "{bad}");
        }
    }
}
