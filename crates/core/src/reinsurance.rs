//! The reinsurance module's commands: the cession recorded on a cover, and
//! the entries reinsurance posts.
//!
//! The reinsurance module works out cessions across every partition, because
//! one life can have policies on several. What it works out comes here by
//! command: the cession is recorded on the layer or member cover it belongs
//! to, and money moves only through the balanced entries below. Nothing here
//! calculates a cession.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{Dimensions, Ledger};
use ic_money::Currency;
use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::Policy;
use crate::postings::PostingSet;
use crate::LifeId;

/// A facultative share of a cession.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacultativeShare {
    pub reference: String,
    pub reinsurer: String,
    pub amount_minor: i64,
}

/// What is ceded on one cover. The parts add up to the sum at risk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CessionRecord {
    /// The treaty and version the cover is ceded under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub treaty: Option<String>,
    #[serde(default)]
    pub treaty_version: u32,
    pub sum_at_risk_minor: i64,
    pub retained_minor: i64,
    /// The treaty's share, by reinsurer.
    #[serde(default)]
    pub ceded: Vec<(String, i64)>,
    #[serde(default)]
    pub facultative: Vec<FacultativeShare>,
    /// Over the retention with no reinsurance in place.
    #[serde(default)]
    pub unplaced_minor: i64,
}

impl CessionRecord {
    fn valid(&self) -> bool {
        let ceded: i64 = self.ceded.iter().map(|(_, a)| *a).sum();
        let facultative: i64 = self.facultative.iter().map(|f| f.amount_minor).sum();
        self.retained_minor >= 0
            && self.unplaced_minor >= 0
            && self.ceded.iter().all(|(_, a)| *a >= 0)
            && self.facultative.iter().all(|f| f.amount_minor >= 0)
            && self.retained_minor + ceded + facultative + self.unplaced_minor == self.sum_at_risk_minor
    }
}

/// The cession from a date. `None`: the cover is not in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CessionSegment {
    pub from: NaiveDate,
    pub cession: Option<CessionRecord>,
}

/// The key a cover's cession is kept under on its policy.
pub fn cession_key(cover: &str, layer: u32, life: &LifeId) -> String {
    format!("{cover}/{layer}/{life}")
}

/// The cession recorded for a cover on a date.
pub fn cession_on<'a>(policy: &'a Policy, cover: &str, layer: u32, life: &LifeId, on: NaiveDate) -> Option<&'a CessionRecord> {
    policy
        .cessions
        .get(&cession_key(cover, layer, life))?
        .iter()
        .rev()
        .find(|s| s.from <= on)
        .and_then(|s| s.cession.as_ref())
}

/// Record the cession on a cover from a date. Whatever the record said from
/// that date on is replaced; what it says before is kept.
pub(crate) fn record_cession(
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    cover: String,
    layer: u32,
    life: LifeId,
    from: NaiveDate,
    segments: Vec<CessionSegment>,
) -> Result<(), Reject> {
    if !policy.covers.contains_key(&cover) {
        return Err(Reject::new("unknown_cover", format!("policy {} has no cover {cover}", policy.id)));
    }
    let ordered = segments.first().is_none_or(|s| s.from >= from) && segments.windows(2).all(|w| w[0].from < w[1].from);
    if !ordered || segments.iter().any(|s| s.cession.as_ref().is_some_and(|c| !c.valid())) {
        return Err(Reject::new(
            "invalid_cession",
            "segments must be in date order from the given date, and the parts of each cession must add up to its sum at risk",
        ));
    }
    ctx.work += segments.len() as u32;
    let key = cession_key(&cover, layer, &life);
    let record = policy.cessions.entry(key.clone()).or_default();
    record.retain(|s| s.from < from);
    record.extend(segments);
    if record.is_empty() {
        policy.cessions.remove(&key);
    }
    ctx.emit(Event::CessionRecorded {
        policy: policy.id.clone(),
        cover,
        layer,
        life,
        from,
    });
    Ok(())
}

/// The entries reinsurance posts. Each is described for a positive amount; a
/// negative amount posts the opposite entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReinsurancePosting {
    /// Debit reinsurance premium, credit the reinsurer's account.
    CededPremium,
    /// Debit the reinsurer's account, credit reinsurance commission.
    Commission,
    /// An expected recovery: debit reinsurance recoverable, credit
    /// reinsurance recoveries.
    Recoverable,
    /// A recovery now due: debit the reinsurer's account, credit reinsurance
    /// recoverable.
    RecoveryDue,
    /// A catastrophe or stop loss recovery: debit the reinsurer's account,
    /// credit reinsurance recoveries.
    AggregateRecovery,
    /// Debit the reinsurer's account, credit profit commission.
    ProfitCommission,
    /// A disputed recovery repaid: debit reinsurance recoveries, credit the
    /// reinsurer's account.
    DisputeSettlement,
    /// Debit the migration control account, credit the reinsurer's account.
    OpeningBalance,
    /// A statement going for payment: debit the reinsurer's account, credit
    /// reinsurance settlement clearing.
    SettlementSweep,
    /// The bank moved a statement's settlement: debit reinsurance settlement
    /// clearing, credit the bank account named on the command. Money received
    /// from a reinsurer is a negative amount.
    SettlementCleared,
}

impl ReinsurancePosting {
    fn entry_kind(self) -> &'static str {
        match self {
            ReinsurancePosting::CededPremium => "reinsurance_premium_ceded",
            ReinsurancePosting::Commission => "reinsurance_commission",
            ReinsurancePosting::Recoverable => "reinsurance_recoverable",
            ReinsurancePosting::RecoveryDue => "reinsurance_recovery_due",
            ReinsurancePosting::AggregateRecovery => "reinsurance_aggregate_recovery",
            ReinsurancePosting::ProfitCommission => "reinsurance_profit_commission",
            ReinsurancePosting::DisputeSettlement => "reinsurance_dispute_settlement",
            ReinsurancePosting::OpeningBalance => "reinsurance_opening_balance",
            ReinsurancePosting::SettlementSweep => "reinsurance_settlement_sweep",
            ReinsurancePosting::SettlementCleared => "reinsurance_settlement_cleared",
        }
    }
}

/// Treaty and reinsurer IDs become part of a ledger account ID.
fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 64 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Post one reinsurance entry. With a policy, the entry carries the policy's
/// statutory fund and product; without one it belongs to the treaty alone.
pub(crate) fn post(
    ledger: &mut Ledger,
    policy: Option<&Policy>,
    ctx: &mut Ctx<'_>,
    kind: ReinsurancePosting,
    treaty: &str,
    reinsurer: &str,
    currency: Currency,
    amount_minor: i64,
    value_date: NaiveDate,
    reference: &str,
    bank_account: Option<&str>,
) -> Result<(), Reject> {
    if !valid_code(treaty) || !valid_code(reinsurer) {
        return Err(Reject::new(
            "invalid_code",
            "a treaty or reinsurer ID is 1 to 64 letters, digits, hyphens or underscores",
        ));
    }
    if amount_minor == 0 || reference.trim().is_empty() {
        return Err(Reject::new("invalid_amount", "a reinsurance entry needs an amount and a reference"));
    }
    if policy.is_some_and(|p| p.currency != currency) {
        return Err(Reject::new("currency", "the entry is not in the policy's currency"));
    }
    // Only the entry that moves money at the bank names a bank account.
    let bank = match (kind, bank_account) {
        (ReinsurancePosting::SettlementCleared, Some(code)) if valid_code(code) => Some(code),
        (ReinsurancePosting::SettlementCleared, _) => {
            return Err(Reject::new(
                "invalid_bank_account",
                "a cleared settlement names the bank account it moved through: 1 to 64 letters, digits, hyphens or underscores",
            ))
        }
        (_, Some(_)) => return Err(Reject::new("bank_account_not_expected", "only a cleared settlement names a bank account")),
        (_, None) => None,
    };
    let c = currency;
    let account = accounts::reinsurer_account(treaty, reinsurer, c);
    let (debit, credit) = match kind {
        ReinsurancePosting::CededPremium => (accounts::reinsurance_premium(c), account),
        ReinsurancePosting::Commission => (account, accounts::reinsurance_commission(c)),
        ReinsurancePosting::Recoverable => (accounts::reinsurance_recoverable(c), accounts::reinsurance_recoveries(c)),
        ReinsurancePosting::RecoveryDue => (account, accounts::reinsurance_recoverable(c)),
        ReinsurancePosting::AggregateRecovery => (account, accounts::reinsurance_recoveries(c)),
        ReinsurancePosting::ProfitCommission => (account, accounts::reinsurance_profit_commission(c)),
        ReinsurancePosting::DisputeSettlement => (accounts::reinsurance_recoveries(c), account),
        ReinsurancePosting::OpeningBalance => (accounts::reinsurance_migration_control(c), account),
        ReinsurancePosting::SettlementSweep => (account, accounts::reinsurance_settlement_clearing(c)),
        ReinsurancePosting::SettlementCleared => (accounts::reinsurance_settlement_clearing(c), accounts::bank(bank.unwrap_or_default(), c)),
    };
    let dims = match policy {
        Some(p) => accounts::dims(p, None, kind.entry_kind()),
        None => Dimensions {
            posting_type: Some(kind.entry_kind().to_string()),
            ..Dimensions::default()
        },
    };
    let mut set = PostingSet::new();
    set.debit(debit, amount_minor as i128, dims.clone());
    set.credit(credit, amount_minor as i128, dims);
    let mut detail = BTreeMap::new();
    detail.insert("treaty".to_string(), treaty.to_string());
    detail.insert("reinsurer".to_string(), reinsurer.to_string());
    detail.insert("reference".to_string(), reference.to_string());
    if let Some(code) = bank {
        detail.insert("bank_account".to_string(), code.to_string());
    }
    let postings = set.into_postings(ledger)?;
    // A correction to an earlier period carries that period's value date.
    ctx.post(ledger, kind.entry_kind(), policy, value_date.min(ctx.date), postings, detail)?;
    ctx.emit(Event::ReinsurancePosted {
        kind,
        treaty: treaty.to_string(),
        reinsurer: reinsurer.to_string(),
        policy: policy.map(|p| p.id.clone()),
        amount_minor,
        value_date,
        reference: reference.to_string(),
    });
    Ok(())
}

/// What a partition holds for one reinsurer under one treaty. Positive is
/// owed to the reinsurer.
pub fn reinsurer_balance(ledger: &Ledger, treaty: &str, reinsurer: &str, currency: Currency) -> i128 {
    ledger.natural_balance(&accounts::reinsurer_account(treaty, reinsurer, currency).id)
}
