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
    /// Premium refunds raised and awaiting payment by the payments module.
    pub const REFUND_PAYABLE: &str = "refund_payable";
    pub const UNIT_ACCOUNT: &str = "unit_account";
    pub const POLICY_LOAN: &str = "policy_loan";

    pub const SETTLEMENT: &str = "settlement";
    /// The mirror of one bank account the insurer holds.
    pub const BANK: &str = "bank";
    pub const EARNED_PREMIUM: &str = "earned_premium";
    pub const GST_PAYABLE: &str = "gst_payable";
    pub const LEVY_PAYABLE: &str = "levy_payable";
    pub const LAPSE_WRITE_OFF: &str = "lapse_write_off";
    pub const ROUNDING: &str = "rounding";
    pub const TRANSFER_IN_TRANSIT: &str = "transfer_in_transit";
    pub const FUND_UNIT_POSITION: &str = "fund_unit_position";
    pub const FUND_CASH_POSITION: &str = "fund_cash_position";

    /// What is owed to, or by, one reinsurer under one treaty.
    pub const REINSURER_ACCOUNT: &str = "reinsurer_account";
    pub const REINSURANCE_PREMIUM: &str = "reinsurance_premium";
    pub const REINSURANCE_COMMISSION: &str = "reinsurance_commission";
    /// Recoveries expected on claims and not yet due from a reinsurer.
    pub const REINSURANCE_RECOVERABLE: &str = "reinsurance_recoverable";
    pub const REINSURANCE_RECOVERIES: &str = "reinsurance_recoveries";
    pub const REINSURANCE_PROFIT_COMMISSION: &str = "reinsurance_profit_commission";
    /// Statement balances on their way to or from a reinsurer's bank.
    pub const REINSURANCE_SETTLEMENT_CLEARING: &str = "reinsurance_settlement_clearing";
    pub const REINSURANCE_MIGRATION_CONTROL: &str = "reinsurance_migration_control";
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
/// A refund that is owed and waits for a payment run. Opened on first use.
pub fn refund_payable(p: &PolicyId, c: Currency) -> Account {
    acct(
        format!("pol/{p}/refund_payable"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::REFUND_PAYABLE,
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

/// The accounts every policy gets at issue.
pub fn policy_accounts(p: &PolicyId, c: Currency) -> Vec<Account> {
    vec![premium_receivable(p, c), unearned_premium(p, c), policy_suspense(p, c), benefit_payable(p, c)]
}

/// Every account whose balance travels with a policy when it moves to
/// another group: the accounts opened at issue, then those opened on first
/// use.
pub fn transferable_accounts(p: &PolicyId, c: Currency) -> Vec<Account> {
    let mut all = policy_accounts(p, c);
    all.push(refund_payable(p, c));
    all
}

// Internal accounts, one instance per partition, created on first use and
// consolidated in the finance ledger.

pub fn settlement(c: Currency) -> Account {
    acct(format!("int/settlement/{c}"), AccountUnit::Minor(c), AccountClass::Asset, role::SETTLEMENT)
}
/// The ledger account that mirrors one bank account, named by the payments
/// module's code for it (one per statutory fund and purpose). Reconciled to
/// the bank's statement every day.
pub fn bank(code: &str, c: Currency) -> Account {
    acct(format!("int/bank/{code}/{c}"), AccountUnit::Minor(c), AccountClass::Asset, role::BANK)
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

// Reinsurance. One instance per partition; the finance ledger consolidates.

pub fn reinsurer_account(treaty: &str, reinsurer: &str, c: Currency) -> Account {
    acct(
        format!("int/reinsurer/{treaty}/{reinsurer}/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::REINSURER_ACCOUNT,
    )
}
pub fn reinsurance_premium(c: Currency) -> Account {
    acct(
        format!("int/reinsurance_premium/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Expense,
        role::REINSURANCE_PREMIUM,
    )
}
pub fn reinsurance_commission(c: Currency) -> Account {
    acct(
        format!("int/reinsurance_commission/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Income,
        role::REINSURANCE_COMMISSION,
    )
}
pub fn reinsurance_recoverable(c: Currency) -> Account {
    acct(
        format!("int/reinsurance_recoverable/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Asset,
        role::REINSURANCE_RECOVERABLE,
    )
}
pub fn reinsurance_recoveries(c: Currency) -> Account {
    acct(
        format!("int/reinsurance_recoveries/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Income,
        role::REINSURANCE_RECOVERIES,
    )
}
pub fn reinsurance_profit_commission(c: Currency) -> Account {
    acct(
        format!("int/reinsurance_profit_commission/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Income,
        role::REINSURANCE_PROFIT_COMMISSION,
    )
}
pub fn reinsurance_settlement_clearing(c: Currency) -> Account {
    acct(
        format!("int/reinsurance_settlement_clearing/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Liability,
        role::REINSURANCE_SETTLEMENT_CLEARING,
    )
}
pub fn reinsurance_migration_control(c: Currency) -> Account {
    acct(
        format!("int/reinsurance_migration_control/{c}"),
        AccountUnit::Minor(c),
        AccountClass::Asset,
        role::REINSURANCE_MIGRATION_CONTROL,
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
