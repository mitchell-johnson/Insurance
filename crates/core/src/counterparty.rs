//! Counterparty accounts: what the insurer owes to, holds for, or is owed by
//! an outside party on this partition. Commission payees, reinsurers,
//! employers and overpaid claimants all use the same small set of accounts.
//!
//! The distribution module and the held balance workbench decide what to
//! post; the core checks the posting is well formed, balances it against a
//! named internal account, and records it as one journal entry. A reference
//! is accepted once per partition, whatever idempotency key carries it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_ledger::{Account, AccountClass, AccountId, AccountUnit, Dimensions, EntryId, Ledger};
use ic_money::Currency;
use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, Receivable};
use crate::postings::{div_half_even, PostingSet};
use crate::{PartyId, PolicyId};

/// Roles of the counterparty accounts, for control totals in finance.
pub mod role {
    pub const COMMISSION_PAYABLE: &str = "commission_payable";
    pub const HELD: &str = "counterparty_held";
    pub const RECEIVABLE: &str = "counterparty_receivable";
    pub const RETENTION: &str = "commission_retention";
    pub const ADVANCE: &str = "commission_advance";
    pub const CLEARING: &str = "counterparty_clearing";
    pub const COMMISSION_EXPENSE: &str = "commission_expense";
    pub const COMMISSION_BAD_DEBT: &str = "commission_bad_debt";
    pub const WRITE_OFF: &str = "counterparty_write_off";
}

/// Largest number of lines one counterparty posting may carry.
pub const MAX_COUNTERPARTY_LINES: usize = 200;

/// The accounts a party can have on a partition. Created on first use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyLedger {
    /// Commission earned and not yet paid. A debit balance is a debt: clawback
    /// or a backdated correction took more than was earned.
    CommissionPayable,
    /// Money earned by, or disputed for, a party that may not be paid yet.
    Held,
    /// Money the party owes the insurer outside commission: an overpayment,
    /// a recovery due, premiums in arrears moved for collection.
    Receivable,
    /// Initial commission kept back as security under the agreement.
    Retention,
    /// Money advanced to the party, recovered from later statements.
    Advance,
    /// A swept balance waiting for the one net payment of a statement.
    Clearing,
}

impl PartyLedger {
    fn name(self) -> &'static str {
        match self {
            PartyLedger::CommissionPayable => "commission_payable",
            PartyLedger::Held => "held",
            PartyLedger::Receivable => "receivable",
            PartyLedger::Retention => "retention",
            PartyLedger::Advance => "advance",
            PartyLedger::Clearing => "clearing",
        }
    }
    fn class_and_role(self) -> (AccountClass, &'static str) {
        match self {
            PartyLedger::CommissionPayable => (AccountClass::Liability, role::COMMISSION_PAYABLE),
            PartyLedger::Held => (AccountClass::Liability, role::HELD),
            PartyLedger::Receivable => (AccountClass::Asset, role::RECEIVABLE),
            PartyLedger::Retention => (AccountClass::Liability, role::RETENTION),
            PartyLedger::Advance => (AccountClass::Asset, role::ADVANCE),
            PartyLedger::Clearing => (AccountClass::Liability, role::CLEARING),
        }
    }
}

/// The internal accounts a counterparty posting may balance against.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalLedger {
    /// Commission expense, by product and cover type.
    CommissionExpense { product: String, cover_type: String },
    /// Commission debts that cannot be recovered.
    CommissionBadDebt,
    /// Other counterparty balances written off, by the class of the balance.
    WriteOff { class: String },
    /// Cash: a payment out or a receipt in.
    Settlement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterpartyAccount {
    Party { party: PartyId, ledger: PartyLedger },
    Internal(InternalLedger),
}

/// One side of a counterparty posting. Debits are positive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterpartyLine {
    pub account: CounterpartyAccount,
    pub amount_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterpartyPosting {
    /// The sender's own reference for this posting. Accepted once.
    pub reference: String,
    /// The entry kind, for example `commission_earned` or `held_released`.
    pub kind: String,
    pub currency: Currency,
    /// The day the cause happened. Defaults to the business date.
    #[serde(default)]
    pub value_date: Option<NaiveDate>,
    /// The policy that caused it, where one did. It must be on this partition.
    #[serde(default)]
    pub policy: Option<PolicyId>,
    /// The reference of an earlier posting this one reverses.
    #[serde(default)]
    pub reverses: Option<String>,
    pub lines: Vec<CounterpartyLine>,
    /// Detail only a statement needs: book, layer, earner, rule version.
    #[serde(default)]
    pub detail: BTreeMap<String, String>,
}

/// References already posted on this partition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterpartyBook {
    #[serde(default)]
    references: BTreeSet<String>,
}

impl CounterpartyBook {
    pub fn is_empty(&self) -> bool {
        self.references.is_empty()
    }
    pub fn has(&self, reference: &str) -> bool {
        self.references.contains(reference)
    }
}

pub fn party_account(party: &PartyId, ledger: PartyLedger, c: Currency) -> Account {
    let (class, role) = ledger.class_and_role();
    Account {
        id: AccountId(format!("cp/{party}/{}/{c}", ledger.name())),
        unit: AccountUnit::Minor(c),
        class,
        role: role.to_string(),
    }
}

pub fn internal_account(ledger: &InternalLedger, c: Currency) -> Account {
    let (id, class, role) = match ledger {
        InternalLedger::CommissionExpense { product, cover_type } => (
            format!("int/commission_expense/{product}/{cover_type}/{c}"),
            AccountClass::Expense,
            role::COMMISSION_EXPENSE,
        ),
        InternalLedger::CommissionBadDebt => (format!("int/commission_bad_debt/{c}"), AccountClass::Expense, role::COMMISSION_BAD_DEBT),
        InternalLedger::WriteOff { class } => (format!("int/counterparty_write_off/{class}/{c}"), AccountClass::Expense, role::WRITE_OFF),
        InternalLedger::Settlement => return crate::accounts::settlement(c),
    };
    Account {
        id: AccountId(id),
        unit: AccountUnit::Minor(c),
        class,
        role: role.to_string(),
    }
}

fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
}

/// Post one counterparty entry. Everything is checked before anything is
/// written, so a refusal leaves no trace.
pub(crate) fn post(
    ledger: &mut Ledger,
    book: &mut CounterpartyBook,
    policy: Option<&Policy>,
    ctx: &mut Ctx<'_>,
    posting: CounterpartyPosting,
) -> Result<(), Reject> {
    let bad = |m: &str| Reject::new("invalid_counterparty_posting", m);
    if !plain(&posting.reference) || !plain(&posting.kind) {
        return Err(bad("the reference and the kind must be plain identifiers"));
    }
    if posting.lines.is_empty() || posting.lines.len() > MAX_COUNTERPARTY_LINES {
        return Err(bad("a posting carries between 1 and 200 lines"));
    }
    if book.references.contains(&posting.reference) {
        return Err(Reject::new(
            "duplicate_reference",
            format!("{} has already been posted on this partition", posting.reference),
        ));
    }
    if let Some(r) = &posting.reverses {
        if !book.references.contains(r) {
            return Err(Reject::new("unknown_reference", format!("{r} was not posted on this partition")));
        }
    }
    let value_date = posting.value_date.unwrap_or(ctx.date);
    if value_date > ctx.date {
        return Err(bad("a posting cannot be dated in the future"));
    }
    let c = posting.currency;
    if policy.is_some_and(|p| p.currency != c) {
        return Err(bad("the posting is not in the policy's currency"));
    }
    let mut net: i128 = 0;
    let mut set = PostingSet::new();
    for line in &posting.lines {
        if line.amount_minor == 0 {
            return Err(bad("a line of zero posts nothing"));
        }
        net += line.amount_minor as i128;
        let (account, dims) = match &line.account {
            CounterpartyAccount::Party { party, ledger } => {
                if !plain(&party.0) {
                    return Err(bad("a party is named by its opaque ID"));
                }
                (party_account(party, *ledger, c), Dimensions::default())
            }
            CounterpartyAccount::Internal(internal) => {
                let mut dims = Dimensions {
                    statutory_fund: policy.map(|p| p.statutory_fund.clone()),
                    product: policy.map(|p| p.product_code.clone()),
                    cover_type: None,
                    posting_type: Some(posting.kind.clone()),
                };
                match internal {
                    InternalLedger::CommissionExpense { product, cover_type } => {
                        if !plain(product) || !plain(cover_type) {
                            return Err(bad("commission expense is kept by product and cover type"));
                        }
                        dims.product = Some(product.clone());
                        dims.cover_type = Some(cover_type.clone());
                    }
                    InternalLedger::WriteOff { class } if !plain(class) => return Err(bad("a write-off names the class of balance")),
                    _ => {}
                }
                (internal_account(internal, c), dims)
            }
        };
        set.debit(account, line.amount_minor as i128, dims);
    }
    if net != 0 {
        return Err(bad("debits and credits must be equal"));
    }
    if set.is_empty() {
        return Err(bad("the lines cancel out and post nothing"));
    }
    let mut detail = posting.detail.clone();
    detail.insert("reference".to_string(), posting.reference.clone());
    if let Some(r) = &posting.reverses {
        detail.insert("reverses".to_string(), r.clone());
    }
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, &posting.kind, policy, value_date, postings, detail)?;
    let entry = ctx.entries.last().map(|e| e.id).unwrap_or(EntryId {
        partition: ledger.partition(),
        seq: ledger.entry_count(),
    });
    book.references.insert(posting.reference.clone());
    ctx.emit(Event::CounterpartyPosted {
        entry,
        reference: posting.reference,
        kind: posting.kind,
        currency: c,
        value_date,
        policy: posting.policy,
        reverses: posting.reverses,
        lines: posting.lines,
    });
    Ok(())
}

/// What part of a receipt paid for cover, by cover type, leaving out the
/// policy fee, GST and levies. Commission is earned on this.
pub(crate) fn net_by_cover_type(receivables: &[Receivable], allocations: &BTreeMap<u32, i64>) -> BTreeMap<String, i64> {
    let mut out: BTreeMap<String, i64> = BTreeMap::new();
    for r in receivables {
        let Some(paid) = allocations.get(&r.id).copied().filter(|_| r.total_minor != 0) else {
            continue;
        };
        for (cover_type, fine) in r.net_fine.iter().filter(|(k, _)| k.as_str() != "policy_fee") {
            let share = div_half_even(paid as i128 * *fine, r.total_minor as i128 * ic_money::FINE_PER_MINOR);
            *out.entry(cover_type.clone()).or_insert(0) += share as i64;
        }
    }
    out.retain(|_, v| *v != 0);
    out
}
