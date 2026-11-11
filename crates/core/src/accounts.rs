//! Ledger mapping: every account named in the design, per policy and per
//! partition.

use ic_ledger::{Account, AccountClass, AccountId, AccountUnit, Dimensions};
use ic_money::{Currency, FundId};

use crate::policy::Policy;
use crate::PolicyId;

/// Roles of the accounts each policy has in its policyholder's group.
pub mod role {
    pub const PREMIUM_RECEIVABLE: &str = "premium_receivable";
    pub const UNEARNED_PREMIUM: &str = "unearned_premium";
    /// Also called premium suspense. One account.
    pub const POLICY_SUSPENSE: &str = "policy_suspense";
    pub const PENDING_INVESTMENT_CASH: &str = "pending_investment_cash";
    /// Admitted claims, surrenders and maturities awaiting payment.
    pub const BENEFIT_PAYABLE: &str = "benefit_payable";
    pub const UNIT_ACCOUNT: &str = "unit_account";
    pub const POLICY_LOAN: &str = "policy_loan";

    /// Case estimates for claims notified and not yet admitted or closed.
    pub const CLAIM_RESERVE: &str = "claim_reserve";
    /// Claim money overpaid and owed back.
    pub const CLAIM_RECOVERY_RECEIVABLE: &str = "claim_recovery_receivable";
    pub const CLAIMS_INCURRED: &str = "claims_incurred";
    pub const EX_GRATIA: &str = "ex_gratia";
    pub const CLAIM_RECOVERY_WRITE_OFF: &str = "claim_recovery_write_off";

    pub const SETTLEMENT: &str = "settlement";
    pub const EARNED_PREMIUM: &str = "earned_premium";
    pub const GST_PAYABLE: &str = "gst_payable";
    pub const LEVY_PAYABLE: &str = "levy_payable";
    pub const LAPSE_WRITE_OFF: &str = "lapse_write_off";
    pub const ROUNDING: &str = "rounding";
    pub const TRANSFER_IN_TRANSIT: &str = "transfer_in_transit";
    pub const FUND_UNIT_POSITION: &str = "fund_unit_position";
    pub const FUND_CASH_POSITION: &str = "fund_cash_position";
}

fn acct(id: String, unit: AccountUnit, class: AccountClass, role: &str) -> Account {
    Account {
        id: AccountId(id),
        unit,
        class,
        role: role.to_string(),
    }
}

pub fn premium_receivable(p: &PolicyId, c: Currency) -> Account {
    acct(
        format!("pol/{p}/premium_receivable"),
        AccountUnit::Minor(c),
        AccountClass::Asset,
        role::PREMIUM_RECEIVABLE,
    )
}
pub fn unearned_premium(p: &PolicyId, c: Currency) -> Account {
    acct(
        format!("pol/{p}/unearned_premium"),
        AccountUnit::Fine(c),
        AccountClass::Liability,
        role::UNEARNED_PREMIUM,
    )
}
pub fn policy_suspense(p: &PolicyId, c: Currency) -> Account {
    acct(
        format!("pol/{p}/policy_suspense"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::POLICY_SUSPENSE,
    )
}
pub fn benefit_payable(p: &PolicyId, c: Currency) -> Account {
    acct(
        format!("pol/{p}/benefit_payable"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::BENEFIT_PAYABLE,
    )
}
pub fn pending_investment_cash(p: &PolicyId, c: Currency) -> Account {
    acct(
        format!("pol/{p}/pending_investment_cash"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::PENDING_INVESTMENT_CASH,
    )
}
/// Units are owed to the policyholder, so a unit account carries a credit balance.
pub fn unit_account(p: &PolicyId, fund: &FundId) -> Account {
    acct(
        format!("pol/{p}/units/{}", fund.0),
        AccountUnit::Units(fund.clone()),
        AccountClass::Liability,
        role::UNIT_ACCOUNT,
    )
}
pub fn policy_loan(p: &PolicyId, c: Currency) -> Account {
    acct(format!("pol/{p}/policy_loan"), AccountUnit::Minor(c), AccountClass::Asset, role::POLICY_LOAN)
}

/// Opened with a policy's first claim.
pub fn claim_reserve(p: &PolicyId, c: Currency) -> Account {
    acct(format!("pol/{p}/claim_reserve"), AccountUnit::Minor(c), AccountClass::Liability, role::CLAIM_RESERVE)
}
/// Opened with a policy's first overpayment.
pub fn claim_recovery_receivable(p: &PolicyId, c: Currency) -> Account {
    acct(
        format!("pol/{p}/claim_recovery_receivable"),
        AccountUnit::Minor(c),
        AccountClass::Asset,
        role::CLAIM_RECOVERY_RECEIVABLE,
    )
}
pub fn claims_incurred(product: &str, cover_type: &str, c: Currency) -> Account {
    acct(
        format!("int/claims_incurred/{product}/{cover_type}/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Expense,
        role::CLAIMS_INCURRED,
    )
}
/// Goodwill payments, never mixed with contractual ones.
pub fn ex_gratia(product: &str, c: Currency) -> Account {
    acct(format!("int/ex_gratia/{product}/{c}"), AccountUnit::Minor(c), AccountClass::Expense, role::EX_GRATIA)
}
pub fn claim_recovery_write_off(product: &str, c: Currency) -> Account {
    acct(
        format!("int/claim_recovery_write_off/{product}/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Expense,
        role::CLAIM_RECOVERY_WRITE_OFF,
    )
}

/// The accounts every policy gets at issue.
pub fn policy_accounts(p: &PolicyId, c: Currency) -> Vec<Account> {
    vec![premium_receivable(p, c), unearned_premium(p, c), policy_suspense(p, c), benefit_payable(p, c)]
}

// Internal accounts, one instance per partition, created on first use and
// consolidated in the finance ledger.

pub fn settlement(c: Currency) -> Account {
    acct(format!("int/settlement/{c}"), AccountUnit::Minor(c), AccountClass::Asset, role::SETTLEMENT)
}
pub fn earned_premium(product: &str, cover_type: &str, c: Currency) -> Account {
    acct(
        format!("int/earned_premium/{product}/{cover_type}/{c}"),
        AccountUnit::Fine(c),
        AccountClass::Income,
        role::EARNED_PREMIUM,
    )
}
pub fn gst_payable(c: Currency) -> Account {
    acct(
        format!("int/gst_payable/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::GST_PAYABLE,
    )
}
pub fn levy_payable(code: &str, c: Currency) -> Account {
    acct(
        format!("int/levy_payable/{code}/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::LEVY_PAYABLE,
    )
}
pub fn lapse_write_off(product: &str, c: Currency) -> Account {
    acct(
        format!("int/lapse_write_off/{product}/{c}"),
        AccountUnit::Fine(c),
        AccountClass::Expense,
        role::LAPSE_WRITE_OFF,
    )
}
pub fn rounding(c: Currency) -> Account {
    acct(format!("int/rounding/{c}"), AccountUnit::Fine(c), AccountClass::Expense, role::ROUNDING)
}
/// One in-transit account per unit type, for policies moving between groups.
pub fn transfer_in_transit(unit: &AccountUnit) -> Account {
    let name = match unit {
        AccountUnit::Minor(c) => format!("int/transfer_in_transit/{c}/minor"),
        AccountUnit::Fine(c) => format!("int/transfer_in_transit/{c}/fine"),
        AccountUnit::Units(f) => format!("int/transfer_in_transit/units/{}", f.0),
    };
    acct(name, unit.clone(), AccountClass::Asset, role::TRANSFER_IN_TRANSIT)
}
pub fn fund_unit_position(fund: &FundId) -> Account {
    acct(
        format!("int/fund_position/{}/units", fund.0),
        AccountUnit::Units(fund.clone()),
        AccountClass::Asset,
        role::FUND_UNIT_POSITION,
    )
}
pub fn fund_cash_position(fund: &FundId, c: Currency) -> Account {
    acct(
        format!("int/fund_position/{}/cash/{c}", fund.0),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::FUND_CASH_POSITION,
    )
}

/// Statutory fund is a dimension on every posting, taken from the policy.
pub fn dims(policy: &Policy, cover_type: Option<&str>, posting_type: &str) -> Dimensions {
    Dimensions {
        statutory_fund: Some(policy.statutory_fund.clone()),
        product: Some(policy.product_code.clone()),
        cover_type: cover_type.map(str::to_string),
        posting_type: Some(posting_type.to_string()),
    }
}
