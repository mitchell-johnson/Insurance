//! The ledger engine: double entry, append-only, immutable, hash-chained.
//!
//! * Every money movement is a [`JournalEntry`] whose postings net to exactly
//!   zero in each currency (minor and fine units of one currency net together
//!   in fine units) and in each fund's units separately.
//! * Balances are materialised and updated in the same step as the postings.
//! * Holds are replicated state but not postings.
//! * Each entry carries the hash of the previous entry on its partition. The
//!   chain covers a salted commitment to the payload, so the payload can be
//!   re-keyed or shredded without breaking the chain.
//!
//! The ledger never reads a clock or randomness: the business date and the
//! commitment salt arrive with the command.

use std::collections::BTreeMap;
use std::fmt;

use chrono::NaiveDate;
use ic_money::{AccrualAmount, Currency, FundId, Money, UnitQty, FINE_PER_MINOR};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type PartitionId = u16;

/// Number of ledger partitions. Fixed: no split or merge.
pub const PARTITION_COUNT: PartitionId = 64;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Hash32(#[serde(with = "hex32")] pub [u8; 32]);

impl Hash32 {
    pub const ZERO: Hash32 = Hash32([0u8; 32]);
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}
impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let b = hex::decode(&s).map_err(serde::de::Error::custom)?;
        b.try_into().map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

pub fn sha256(parts: &[&[u8]]) -> Hash32 {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p);
    }
    Hash32(h.finalize().into())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccountId(pub String);

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The single unit an account is held in, fixed at creation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AccountUnit {
    /// Whole minor units (cents).
    Minor(Currency),
    /// Millionths of a minor unit, for accrual and earning accounts.
    Fine(Currency),
    /// Micro-units of one fund.
    Units(FundId),
}

/// What must net to zero together inside an entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BalanceFamily {
    Currency(Currency),
    Fund(FundId),
}

impl AccountUnit {
    pub fn family(&self) -> BalanceFamily {
        match self {
            AccountUnit::Minor(c) | AccountUnit::Fine(c) => BalanceFamily::Currency(*c),
            AccountUnit::Units(f) => BalanceFamily::Fund(f.clone()),
        }
    }
    /// Scale of this unit to the finest unit of its family.
    fn to_finest(&self, amount: i128) -> Option<i128> {
        match self {
            AccountUnit::Minor(_) => amount.checked_mul(FINE_PER_MINOR),
            AccountUnit::Fine(_) | AccountUnit::Units(_) => Some(amount),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountClass {
    Asset,
    Liability,
    Income,
    Expense,
}

impl AccountClass {
    pub fn debit_normal(self) -> bool {
        matches!(self, AccountClass::Asset | AccountClass::Expense)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub unit: AccountUnit,
    pub class: AccountClass,
    /// Stable role name, used by reconciliations and the finance ledger feed
    /// (for example `premium_receivable`).
    pub role: String,
}

/// A typed amount for one side of a posting. The type decides which kind of
/// account it may be posted to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Amount {
    Money(Money),
    Accrual(AccrualAmount),
    Units(UnitQty),
}

impl Amount {
    fn unit(&self) -> AccountUnit {
        match self {
            Amount::Money(m) => AccountUnit::Minor(m.currency),
            Amount::Accrual(a) => AccountUnit::Fine(a.currency),
            Amount::Units(u) => AccountUnit::Units(u.fund.clone()),
        }
    }
    fn raw(&self) -> i128 {
        match self {
            Amount::Money(m) => m.minor as i128,
            Amount::Accrual(a) => a.fine,
            Amount::Units(u) => u.micro as i128,
        }
    }
}

impl From<Money> for Amount {
    fn from(m: Money) -> Self {
        Amount::Money(m)
    }
}
impl From<AccrualAmount> for Amount {
    fn from(a: AccrualAmount) -> Self {
        Amount::Accrual(a)
    }
}
impl From<UnitQty> for Amount {
    fn from(u: UnitQty) -> Self {
        Amount::Units(u)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Debit,
    Credit,
}

/// Dimensions the finance ledger or a control account needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dimensions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statutory_fund: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_type: Option<String>,
    /// Posting type, for example `premium`, `gst`, `levy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posting_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    pub account: AccountId,
    pub side: Side,
    pub amount: Amount,
    #[serde(default)]
    pub dims: Dimensions,
}

impl Posting {
    pub fn debit(account: AccountId, amount: impl Into<Amount>) -> Self {
        Self {
            account,
            side: Side::Debit,
            amount: amount.into(),
            dims: Dimensions::default(),
        }
    }
    pub fn credit(account: AccountId, amount: impl Into<Amount>) -> Self {
        Self {
            account,
            side: Side::Credit,
            amount: amount.into(),
            dims: Dimensions::default(),
        }
    }
    pub fn with_dims(mut self, dims: Dimensions) -> Self {
        self.dims = dims;
        self
    }
    /// Signed amount in the account's own unit: debits positive.
    pub fn signed(&self) -> i128 {
        match self.side {
            Side::Debit => self.amount.raw(),
            Side::Credit => -self.amount.raw(),
        }
    }
    /// The mirror posting, used for reversals.
    pub fn reversed(&self) -> Posting {
        let mut p = self.clone();
        p.side = match self.side {
            Side::Debit => Side::Credit,
            Side::Credit => Side::Debit,
        };
        p
    }
}

/// What a feature or command proposes. The ledger balances and commits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryDraft {
    /// Entry type, for example `premium_due`, `premium_earned`.
    pub kind: String,
    pub value_date: NaiveDate,
    pub postings: Vec<Posting>,
    /// Content hash of the product version that produced the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_hash: Option<String>,
    /// The module that sent the command.
    pub caller: String,
    /// Opaque ID of the policy the entry concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Detail only a statement needs. Never personal data.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub detail: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntryId {
    pub partition: PartitionId,
    pub seq: u64,
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "je-{:02}-{:012}", self.partition, self.seq)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: EntryId,
    /// The day the entry was recorded. Never in a closed day.
    pub business_date: NaiveDate,
    pub body: EntryDraft,
    /// Salt of the payload commitment. Deleting it (with the payload) shreds
    /// the entry's content while the chain stays verifiable.
    pub salt: String,
    pub commitment: Hash32,
    pub prev_hash: Hash32,
    pub hash: Hash32,
}

impl JournalEntry {
    fn commitment_of(salt: &str, body: &EntryDraft) -> Result<Hash32, LedgerError> {
        let payload = serde_json::to_vec(body).map_err(|e| LedgerError::Encoding(e.to_string()))?;
        Ok(sha256(&[b"ic.commitment.v1", salt.as_bytes(), &payload]))
    }
    fn chain_hash(prev: Hash32, id: EntryId, business_date: NaiveDate, commitment: Hash32) -> Hash32 {
        sha256(&[
            b"ic.entry.v1",
            &prev.0,
            &id.partition.to_be_bytes(),
            &id.seq.to_be_bytes(),
            business_date.to_string().as_bytes(),
            &commitment.0,
        ])
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LedgerError {
    #[error("account {0} does not exist")]
    UnknownAccount(AccountId),
    #[error("account {0} already exists with a different definition")]
    AccountConflict(AccountId),
    #[error("posting to {account} uses {got:?} but the account is held in {want:?}")]
    UnitMismatch { account: AccountId, want: AccountUnit, got: AccountUnit },
    #[error("entry does not balance in {family:?}: off by {off}")]
    Unbalanced { family: BalanceFamily, off: i128 },
    #[error("entry has no postings")]
    Empty,
    #[error("posting amounts must be positive")]
    NonPositive,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("insufficient available balance on {0}")]
    InsufficientAvailable(AccountId),
    #[error("hold {0} not found")]
    UnknownHold(String),
    #[error("hold {0} already exists")]
    DuplicateHold(String),
    #[error("encoding failed: {0}")]
    Encoding(String),
    #[error("hash chain broken at {0}")]
    ChainBroken(EntryId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AccountState {
    account: Account,
    /// Sum of postings, debits positive, in the account's unit.
    balance: i128,
    /// Sum of active holds, in the account's unit.
    held: i128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub id: String,
    pub account: AccountId,
    pub amount: i128,
    pub expires_on: Option<NaiveDate>,
}

/// One partition's ledger state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    partition: PartitionId,
    accounts: BTreeMap<AccountId, AccountState>,
    holds: BTreeMap<String, Hold>,
    head: Hash32,
    next_seq: u64,
}

impl Ledger {
    pub fn new(partition: PartitionId) -> Self {
        let genesis = sha256(&[b"ic.genesis.v1", &partition.to_be_bytes()]);
        Self {
            partition,
            accounts: BTreeMap::new(),
            holds: BTreeMap::new(),
            head: genesis,
            next_seq: 1,
        }
    }

    pub fn genesis(partition: PartitionId) -> Hash32 {
        sha256(&[b"ic.genesis.v1", &partition.to_be_bytes()])
    }

    pub fn partition(&self) -> PartitionId {
        self.partition
    }
    pub fn head(&self) -> Hash32 {
        self.head
    }
    pub fn entry_count(&self) -> u64 {
        self.next_seq - 1
    }

    /// Open an account. Opening the same definition twice is a no-op.
    pub fn open_account(&mut self, account: Account) -> Result<(), LedgerError> {
        match self.accounts.get(&account.id) {
            Some(existing) if existing.account == account => Ok(()),
            Some(_) => Err(LedgerError::AccountConflict(account.id)),
            None => {
                self.accounts.insert(account.id.clone(), AccountState { account, balance: 0, held: 0 });
                Ok(())
            }
        }
    }

    pub fn has_account(&self, id: &AccountId) -> bool {
        self.accounts.contains_key(id)
    }

    pub fn account(&self, id: &AccountId) -> Option<&Account> {
        self.accounts.get(id).map(|s| &s.account)
    }

    /// Signed balance, debits positive, in the account's unit.
    pub fn balance(&self, id: &AccountId) -> i128 {
        self.accounts.get(id).map_or(0, |s| s.balance)
    }

    /// Balance in the account's natural direction (credit balances of
    /// liability and income accounts come back positive).
    pub fn natural_balance(&self, id: &AccountId) -> i128 {
        self.accounts
            .get(id)
            .map_or(0, |s| if s.account.class.debit_normal() { s.balance } else { -s.balance })
    }

    /// Natural balance less active holds.
    pub fn available(&self, id: &AccountId) -> i128 {
        self.accounts.get(id).map_or(0, |s| {
            let n = if s.account.class.debit_normal() { s.balance } else { -s.balance };
            n - s.held
        })
    }

    pub fn accounts(&self) -> impl Iterator<Item = (&Account, i128)> {
        self.accounts.values().map(|s| (&s.account, s.balance))
    }

    /// Sum of signed balances over every account with this role.
    pub fn role_total(&self, role: &str) -> i128 {
        self.accounts.values().filter(|s| s.account.role == role).map(|s| s.balance).sum()
    }

    /// Validate a draft without committing it.
    pub fn validate(&self, draft: &EntryDraft) -> Result<(), LedgerError> {
        if draft.postings.is_empty() {
            return Err(LedgerError::Empty);
        }
        let mut nets: BTreeMap<BalanceFamily, i128> = BTreeMap::new();
        for p in &draft.postings {
            let st = self.accounts.get(&p.account).ok_or_else(|| LedgerError::UnknownAccount(p.account.clone()))?;
            let got = p.amount.unit();
            if got != st.account.unit {
                return Err(LedgerError::UnitMismatch {
                    account: p.account.clone(),
                    want: st.account.unit.clone(),
                    got,
                });
            }
            if p.amount.raw() <= 0 {
                return Err(LedgerError::NonPositive);
            }
            let fine = got.to_finest(p.signed()).ok_or(LedgerError::Overflow)?;
            let net = nets.entry(got.family()).or_insert(0);
            *net = net.checked_add(fine).ok_or(LedgerError::Overflow)?;
        }
        for (family, off) in nets {
            if off != 0 {
                return Err(LedgerError::Unbalanced { family, off });
            }
        }
        Ok(())
    }

    /// Commit a balanced entry: append to the chain and update balances in
    /// one step. `salt` comes from the command, never from the replica.
    pub fn post(&mut self, draft: EntryDraft, business_date: NaiveDate, salt: &str) -> Result<JournalEntry, LedgerError> {
        self.validate(&draft)?;
        // Check every balance update before applying any, so the step is atomic.
        let mut updates: BTreeMap<AccountId, i128> = BTreeMap::new();
        for p in &draft.postings {
            let cur = match updates.get(&p.account) {
                Some(v) => *v,
                None => self.balance(&p.account),
            };
            updates.insert(p.account.clone(), cur.checked_add(p.signed()).ok_or(LedgerError::Overflow)?);
        }
        let id = EntryId {
            partition: self.partition,
            seq: self.next_seq,
        };
        let commitment = JournalEntry::commitment_of(salt, &draft)?;
        let hash = JournalEntry::chain_hash(self.head, id, business_date, commitment);
        for (acct, bal) in updates {
            if let Some(st) = self.accounts.get_mut(&acct) {
                st.balance = bal;
            }
        }
        let entry = JournalEntry {
            id,
            business_date,
            body: draft,
            salt: salt.to_string(),
            commitment,
            prev_hash: self.head,
            hash,
        };
        self.head = hash;
        self.next_seq += 1;
        Ok(entry)
    }

    /// Place a hold. Holds lower the available balance but are not postings.
    pub fn place_hold(&mut self, hold: Hold) -> Result<(), LedgerError> {
        if self.holds.contains_key(&hold.id) {
            return Err(LedgerError::DuplicateHold(hold.id));
        }
        if hold.amount <= 0 {
            return Err(LedgerError::NonPositive);
        }
        if self.available(&hold.account) < hold.amount {
            return Err(LedgerError::InsufficientAvailable(hold.account));
        }
        let st = self
            .accounts
            .get_mut(&hold.account)
            .ok_or_else(|| LedgerError::UnknownAccount(hold.account.clone()))?;
        st.held += hold.amount;
        self.holds.insert(hold.id.clone(), hold);
        Ok(())
    }

    pub fn release_hold(&mut self, id: &str) -> Result<Hold, LedgerError> {
        let hold = self.holds.remove(id).ok_or_else(|| LedgerError::UnknownHold(id.to_string()))?;
        if let Some(st) = self.accounts.get_mut(&hold.account) {
            st.held -= hold.amount;
        }
        Ok(hold)
    }

    /// Holds whose expiry is on or before the given business date. Expiry is
    /// driven by a logged command that names the date.
    pub fn expired_holds(&self, on: NaiveDate) -> Vec<String> {
        self.holds
            .values()
            .filter(|h| h.expires_on.is_some_and(|d| d <= on))
            .map(|h| h.id.clone())
            .collect()
    }

    /// The whole partition must net to zero in every balance family.
    pub fn trial_balance(&self) -> BTreeMap<BalanceFamily, i128> {
        let mut nets = BTreeMap::new();
        for s in self.accounts.values() {
            let fine = s.account.unit.to_finest(s.balance).unwrap_or(i128::MAX);
            *nets.entry(s.account.unit.family()).or_insert(0) += fine;
        }
        nets
    }

    /// Feed the ledger state into a state hash.
    pub fn digest(&self) -> Hash32 {
        let mut h = Sha256::new();
        h.update(b"ic.ledger.state.v1");
        h.update(self.partition.to_be_bytes());
        h.update(self.head.0);
        h.update(self.next_seq.to_be_bytes());
        for (id, s) in &self.accounts {
            h.update(id.0.as_bytes());
            h.update([0]);
            h.update(s.balance.to_be_bytes());
            h.update(s.held.to_be_bytes());
        }
        for (id, hold) in &self.holds {
            h.update(id.as_bytes());
            h.update([0]);
            h.update(hold.amount.to_be_bytes());
        }
        Hash32(h.finalize().into())
    }
}

/// Independent verification of one partition's chain. Recomputes every link
/// and, where the payload and salt are still present, every commitment.
pub fn verify_chain(partition: PartitionId, entries: &[JournalEntry]) -> Result<Hash32, LedgerError> {
    let mut head = Ledger::genesis(partition);
    for (i, e) in entries.iter().enumerate() {
        let expected_id = EntryId { partition, seq: i as u64 + 1 };
        if e.id != expected_id || e.prev_hash != head {
            return Err(LedgerError::ChainBroken(e.id));
        }
        if JournalEntry::commitment_of(&e.salt, &e.body)? != e.commitment {
            return Err(LedgerError::ChainBroken(e.id));
        }
        if JournalEntry::chain_hash(head, e.id, e.business_date, e.commitment) != e.hash {
            return Err(LedgerError::ChainBroken(e.id));
        }
        head = e.hash;
    }
    Ok(head)
}

/// Re-sum postings and compare to nothing but the entries themselves: the
/// reconciliation job that checks materialised balances cannot drift.
pub fn resum(entries: &[JournalEntry]) -> BTreeMap<AccountId, i128> {
    let mut out = BTreeMap::new();
    for e in entries {
        for p in &e.body.postings {
            *out.entry(p.account.clone()).or_insert(0) += p.signed();
        }
    }
    out
}

/// Merkle root over every partition's head hash for one business date. The
/// root is signed and published to write-once storage outside the core.
pub fn merkle_root(business_date: NaiveDate, heads: &BTreeMap<PartitionId, Hash32>) -> Hash32 {
    let mut level: Vec<Hash32> = heads.iter().map(|(p, h)| sha256(&[b"ic.merkle.leaf.v1", &p.to_be_bytes(), &h.0])).collect();
    if level.is_empty() {
        return sha256(&[b"ic.merkle.empty.v1", business_date.to_string().as_bytes()]);
    }
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|c| match c {
                [a, b] => sha256(&[b"ic.merkle.node.v1", &a.0, &b.0]),
                [a] => sha256(&[b"ic.merkle.node.v1", &a.0, &a.0]),
                _ => Hash32::ZERO,
            })
            .collect();
    }
    sha256(&[b"ic.merkle.root.v1", business_date.to_string().as_bytes(), &level[0].0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ic_money::{Currency::NZD, FundId};
    use proptest::prelude::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn acct(id: &str, unit: AccountUnit, class: AccountClass) -> Account {
        Account {
            id: AccountId(id.into()),
            unit,
            class,
            role: id.into(),
        }
    }
    fn ledger() -> Ledger {
        let mut l = Ledger::new(3);
        l.open_account(acct("recv", AccountUnit::Minor(NZD), AccountClass::Asset)).unwrap();
        l.open_account(acct("unearned", AccountUnit::Fine(NZD), AccountClass::Liability)).unwrap();
        l.open_account(acct("earned", AccountUnit::Fine(NZD), AccountClass::Income)).unwrap();
        l.open_account(acct("units", AccountUnit::Units(FundId("g".into())), AccountClass::Liability))
            .unwrap();
        l.open_account(acct("unitpos", AccountUnit::Units(FundId("g".into())), AccountClass::Asset))
            .unwrap();
        l
    }
    fn draft(postings: Vec<Posting>) -> EntryDraft {
        EntryDraft {
            kind: "t".into(),
            value_date: d(2027, 1, 1),
            postings,
            product_hash: None,
            caller: "test".into(),
            policy: None,
            detail: BTreeMap::new(),
        }
    }

    #[test]
    fn cents_and_fine_units_net_together() {
        let mut l = ledger();
        let m = Money::new(1000, NZD);
        l.post(
            draft(vec![
                Posting::debit(AccountId("recv".into()), m),
                Posting::credit(AccountId("unearned".into()), m.to_fine()),
            ]),
            d(2027, 1, 1),
            "s",
        )
        .unwrap();
        assert_eq!(l.balance(&AccountId("recv".into())), 1000);
        assert_eq!(l.natural_balance(&AccountId("unearned".into())), 1_000_000_000);
        assert!(l.trial_balance().values().all(|v| *v == 0));
    }

    #[test]
    fn unbalanced_is_rejected_and_changes_nothing() {
        let mut l = ledger();
        let before = l.digest();
        let r = l.post(
            draft(vec![
                Posting::debit(AccountId("recv".into()), Money::new(1000, NZD)),
                Posting::credit(AccountId("unearned".into()), AccrualAmount::new(1, NZD)),
            ]),
            d(2027, 1, 1),
            "s",
        );
        assert!(matches!(r, Err(LedgerError::Unbalanced { .. })));
        assert_eq!(before, l.digest());
    }

    #[test]
    fn unit_types_are_enforced() {
        let mut l = ledger();
        let r = l.post(
            draft(vec![
                Posting::debit(AccountId("recv".into()), AccrualAmount::new(5, NZD)),
                Posting::credit(AccountId("unearned".into()), AccrualAmount::new(5, NZD)),
            ]),
            d(2027, 1, 1),
            "s",
        );
        assert!(matches!(r, Err(LedgerError::UnitMismatch { .. })));
    }

    #[test]
    fn units_balance_separately_from_currency() {
        let mut l = ledger();
        let u = UnitQty::new(5_000_000, FundId("g".into()));
        // units alone balance; mixing units against currency does not
        l.post(
            draft(vec![
                Posting::debit(AccountId("unitpos".into()), u.clone()),
                Posting::credit(AccountId("units".into()), u.clone()),
            ]),
            d(2027, 1, 1),
            "s",
        )
        .unwrap();
        let r = l.post(
            draft(vec![
                Posting::debit(AccountId("recv".into()), Money::new(5, NZD)),
                Posting::credit(AccountId("units".into()), u),
            ]),
            d(2027, 1, 1),
            "s",
        );
        assert!(matches!(r, Err(LedgerError::Unbalanced { .. })));
    }

    #[test]
    fn chain_detects_tampering() {
        let mut l = ledger();
        let mut entries = vec![];
        for i in 1..=5 {
            let m = Money::new(100 * i, NZD);
            entries.push(
                l.post(
                    draft(vec![
                        Posting::debit(AccountId("recv".into()), m),
                        Posting::credit(AccountId("unearned".into()), m.to_fine()),
                    ]),
                    d(2027, 1, 1),
                    &format!("salt{i}"),
                )
                .unwrap(),
            );
        }
        assert_eq!(verify_chain(3, &entries).unwrap(), l.head());
        let resummed = resum(&entries);
        assert_eq!(resummed[&AccountId("recv".into())], l.balance(&AccountId("recv".into())));
        entries[2].body.postings[0].amount = Amount::Money(Money::new(1, NZD));
        assert!(verify_chain(3, &entries).is_err());
    }

    #[test]
    fn holds_reduce_available_only() {
        let mut l = ledger();
        let m = Money::new(1000, NZD);
        l.post(
            draft(vec![
                Posting::debit(AccountId("recv".into()), m),
                Posting::credit(AccountId("unearned".into()), m.to_fine()),
            ]),
            d(2027, 1, 1),
            "s",
        )
        .unwrap();
        l.place_hold(Hold {
            id: "h1".into(),
            account: AccountId("recv".into()),
            amount: 600,
            expires_on: Some(d(2027, 1, 5)),
        })
        .unwrap();
        assert_eq!(l.available(&AccountId("recv".into())), 400);
        assert_eq!(l.balance(&AccountId("recv".into())), 1000);
        assert!(l
            .place_hold(Hold {
                id: "h2".into(),
                account: AccountId("recv".into()),
                amount: 600,
                expires_on: None
            })
            .is_err());
        assert_eq!(l.expired_holds(d(2027, 1, 5)), vec!["h1".to_string()]);
        l.release_hold("h1").unwrap();
        assert_eq!(l.available(&AccountId("recv".into())), 1000);
    }

    #[test]
    fn merkle_root_changes_with_any_head() {
        let mut heads = BTreeMap::new();
        for p in 0..5u16 {
            heads.insert(p, Ledger::genesis(p));
        }
        let a = merkle_root(d(2027, 1, 1), &heads);
        heads.insert(4, Hash32::ZERO);
        assert_ne!(a, merkle_root(d(2027, 1, 1), &heads));
    }

    proptest! {
        #[test]
        fn ledger_always_nets_to_zero(amounts in proptest::collection::vec(1i64..10_000_000, 1..50)) {
            let mut l = ledger();
            for (i, a) in amounts.iter().enumerate() {
                let m = Money::new(*a, NZD);
                let (dr, cr) = if i % 2 == 0 { ("recv", "unearned") } else { ("unearned", "recv") };
                let (dp, cp) = if i % 2 == 0 {
                    (Posting::debit(AccountId(dr.into()), m), Posting::credit(AccountId(cr.into()), m.to_fine()))
                } else {
                    (Posting::debit(AccountId(dr.into()), m.to_fine()), Posting::credit(AccountId(cr.into()), m))
                };
                l.post(draft(vec![dp, cp]), d(2027, 1, 1), "s").unwrap();
                prop_assert!(l.trial_balance().values().all(|v| *v == 0));
            }
        }
    }
}
