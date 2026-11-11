# The finance ledger's chart of accounts.
#
# Each control account consolidates one or more account roles of the core
# ledger, across every partition. Control accounts are locked: only the feed
# from the core posts to them. The other accounts take manual journals, each
# with a maker and a checker.
#
# A new version may add accounts and map new roles. It may not remove an
# account, change its class, or move a role that is already mapped.

CHART = chart(
    version = 1,
    effective_from = "2027-01-01",
    retained_earnings = "3900",
    accounts = [
        # Assets
        account("1000", "Cash at bank and in settlement", "asset", control = True, roles = ["settlement"]),
        account("1100", "Premiums receivable", "asset", control = True, roles = ["premium_receivable"]),
        account("1150", "Policy loans", "asset", control = True, roles = ["policy_loan"]),
        account("1200", "Policies in transit between groups", "asset", control = True, roles = ["transfer_in_transit"]),
        account("1300", "Fund unit positions", "asset", control = True, roles = ["fund_unit_position"]),
        account("1400", "Reinsurance assets (actuarial)", "asset"),
        account("1500", "Prepayments", "asset"),
        account("1600", "Interfund receivable", "asset"),
        # Liabilities
        account("2000", "Unearned premium", "liability", control = True, roles = ["unearned_premium"]),
        account("2050", "Policy suspense", "liability", control = True, roles = ["policy_suspense"]),
        account("2060", "Pending investment cash", "liability", control = True, roles = ["pending_investment_cash"]),
        account("2100", "Benefits payable", "liability", control = True, roles = ["benefit_payable"]),
        account("2200", "GST payable", "liability", control = True, roles = ["gst_payable"]),
        account("2210", "Levies payable", "liability", control = True, roles = ["levy_payable"]),
        account("2300", "Policyholder unit accounts", "liability", control = True, roles = ["unit_account"]),
        account("2310", "Fund cash positions", "liability", control = True, roles = ["fund_cash_position"]),
        account("2400", "Policy liabilities (actuarial)", "liability"),
        account("2410", "Risk adjustment", "liability"),
        account("2420", "Contractual service margin", "liability"),
        account("2430", "Claims incurred but not reported", "liability"),
        account("2500", "Accrued expenses", "liability"),
        account("2600", "Interfund payable", "liability"),
        account("2700", "Intercompany", "liability"),
        account("3900", "Retained earnings", "liability"),
        # Income
        account("4000", "Earned premium", "income", control = True, roles = ["earned_premium"]),
        account("4900", "Other income", "income"),
        # Expenses
        account("5100", "Lapse write-offs", "expense", control = True, roles = ["lapse_write_off"]),
        account("5900", "Rounding", "expense", control = True, roles = ["rounding"]),
        account("6000", "Change in policy liabilities", "expense"),
        account("6010", "Change in reinsurance assets", "expense"),
        account("6020", "Change in risk adjustment and margin", "expense"),
        account("6030", "Change in claims incurred but not reported", "expense"),
        account("6100", "Aggregate reinsurance premiums", "expense"),
        account("6500", "Operating expenses", "expense"),
        account("6600", "Tax expense", "expense"),
    ],
)
