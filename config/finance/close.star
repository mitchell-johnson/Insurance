# The period close: which checks run, who owns each, the business day it is
# due on, and which block sign-off. The day numbers and thresholds are
# proposals for finance to set.
#
# Holidays are the national public holidays on the day they are observed
# (a holiday on a weekend is listed on the Monday or Tuesday it moves to).
# Regional anniversary days are left out. Add each new year before it starts:
# a year with no holidays listed has its steps fall due on weekdays only.
#
# A blocking check that is red stops sign-off unless the finance lead waives
# it with a reason, a remediation owner and a date.

CLOSE = close_config(
    version = 1,
    checks = [
        automatic("control_check", "Core to finance ledger control check, anchored to Merkle roots", "finance_operations", 1),
        reported("commission_sweep", "Commission statement cut and sweep; clearing nets to statements", "distribution_operations", 1, by = "commission"),
        reported("reinsurance_sweep", "Reinsurer statement cut and sweep; clearing nets to statements", "reinsurance_operations", 1, by = "reinsurance", quarterly = True),
        reported("unit_pricing", "Unit prices struck and approved, no price error open, holdings extracts written", "investment_operations", 1, by = "unit-pricing"),
        reported("fund_cash_settlement", "Statutory fund cash settlement; bank equals ledger position", "treasury", 1, by = "payments"),
        reported("subledger_reconciliations", "Receivable, unearned premium, benefit payable, recoverable and unit reconciliations", "finance_operations", 2, by = "close-workflow"),
        automatic("valuation_extracts", "Valuation extracts cut, hashed and reconciled opening to closing", "reporting", 2),
        reported("actuarial_handover", "Handover to the actuarial run with extract IDs and hashes", "reporting", 2, by = "reporting", quarterly = True),
        automatic("unapproved_journals", "No manual journal left unapproved", "finance_operations", 3),
        reported("tax_reconciliation", "GST, levies and withholding reconciled to posting types", "tax", 3, by = "finance-console"),
        automatic("actuarial_journals", "Actuarial journals posted with the appointed actuary's sign-off", "appointed_actuary", 5, quarterly = True),
        reported("analytical_review", "Movements against prior period and budget explained", "finance_operations", 4, by = "finance-console", blocking = False),
        automatic("trial_balance", "Trial balance: the books balance in every currency and unit", "finance_operations", 5),
        automatic("fund_balance", "Every statutory fund balances on its own", "finance_operations", 5),
    ],
    aged = [
        aged("unidentified_receipts", "Bank receipts nobody has identified, aged with an owner", "treasury", 2, by = "payments", max_age_days = 30, max_amount_minor = 5000000),
        aged("policy_suspense", "Policy suspense aged with an owner", "collections", 2, by = "close-workflow", max_age_days = 30, max_amount_minor = 5000000),
        aged("pending_investment_cash", "Pending investment cash beyond pricing lag", "investment_operations", 2, by = "investment", max_age_days = 5, max_amount_minor = 5000000),
        aged("held_balance", "Held balances aged by party", "counterparty_finance", 2, by = "commission", max_age_days = 90, max_amount_minor = 10000000),
    ],
    holidays = [
        # 2026 (from the first close this configuration serves)
        "2026-12-25", "2026-12-28",
        # 2027
        "2027-01-01", "2027-01-04", "2027-02-08", "2027-03-26", "2027-03-29", "2027-04-26",
        "2027-06-07", "2027-06-25", "2027-10-25", "2027-12-27", "2027-12-28",
        # 2028
        "2028-01-03", "2028-01-04", "2028-02-07", "2028-04-14", "2028-04-17", "2028-04-25",
        "2028-06-05", "2028-07-14", "2028-10-23", "2028-12-25", "2028-12-26",
    ],
)
