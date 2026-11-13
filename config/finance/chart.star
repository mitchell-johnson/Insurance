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
        account("1010", "Bank accounts", "asset", control = True, roles = ["bank"]),
        account("1100", "Premiums receivable", "asset", control = True, roles = ["premium_receivable"]),
        account("1110", "Claim recoveries receivable", "asset", control = True, roles = ["claim_recovery_receivable"]),
        account("1150", "Policy loans", "asset", control = True, roles = ["policy_loan"]),
        account("1200", "Policies in transit between groups", "asset", control = True, roles = ["transfer_in_transit"]),
        account("1210", "Claim offsets in transit between partitions", "asset", control = True, roles = ["claims_offset_in_transit"]),
        account("1300", "Fund unit positions", "asset", control = True, roles = ["fund_unit_position"]),
        account("1400", "Reinsurance assets (actuarial)", "asset"),
        account("1410", "Reinsurance recoverable on claims", "asset", control = True, roles = ["reinsurance_recoverable"]),
        account("1420", "Reinsurance migration control", "asset", control = True, roles = ["reinsurance_migration_control"]),
        account("1430", "Claims migration control", "asset", control = True, roles = ["claims_migration_control"]),
        account("1500", "Prepayments", "asset"),
        account("1600", "Interfund receivable", "asset"),
        # Liabilities
        account("2000", "Unearned premium", "liability", control = True, roles = ["unearned_premium"]),
        account("2050", "Policy suspense", "liability", control = True, roles = ["policy_suspense"]),
        account("2060", "Pending investment cash", "liability", control = True, roles = ["pending_investment_cash"]),
        account("2100", "Benefits payable", "liability", control = True, roles = ["benefit_payable"]),
        account("2110", "Premium refunds payable", "liability", control = True, roles = ["refund_payable"]),
        account("2120", "Claim reserves (case estimates)", "liability", control = True, roles = ["claim_reserve"]),
        account("2200", "GST payable", "liability", control = True, roles = ["gst_payable"]),
        account("2210", "Levies payable", "liability", control = True, roles = ["levy_payable"]),
        account("2220", "Withholding tax payable", "liability", control = True, roles = ["withholding_tax_payable"]),
        account("2300", "Policyholder unit accounts", "liability", control = True, roles = ["unit_account"]),
        account("2310", "Fund cash positions", "liability", control = True, roles = ["fund_cash_position"]),
        account("2400", "Policy liabilities (actuarial)", "liability"),
        account("2410", "Risk adjustment", "liability"),
        account("2420", "Contractual service margin", "liability"),
        account("2430", "Claims incurred but not reported", "liability"),
        account("2500", "Accrued expenses", "liability"),
        account("2600", "Interfund payable", "liability"),
        account("2700", "Intercompany", "liability"),
        account("2800", "Reinsurer accounts", "liability", control = True, roles = ["reinsurer_account"]),
        account("2810", "Reinsurance settlement clearing", "liability", control = True, roles = ["reinsurance_settlement_clearing"]),
        account("3900", "Retained earnings", "liability"),
        # Income
        account("4000", "Earned premium", "income", control = True, roles = ["earned_premium"]),
        account("4010", "Premium retained on avoided policies", "income", control = True, roles = ["avoidance_premium_retained"]),
        account("4020", "Premium before discounts", "income", control = True, roles = ["premium_before_discounts"]),
        account("4030", "Discounts given", "income", control = True, roles = ["discounts_given"]),
        account("4040", "Interest on policy loans", "income", control = True, roles = ["policy_loan_interest"]),
        account("4100", "Reinsurance commission", "income", control = True, roles = ["reinsurance_commission", "reinsurance_profit_commission"]),
        account("4110", "Reinsurance recoveries", "income", control = True, roles = ["reinsurance_recoveries"]),
        account("4900", "Other income", "income"),
        # Expenses
        account("5000", "Claims incurred", "expense", control = True, roles = ["claims_incurred"]),
        account("5010", "Ex gratia payments", "expense", control = True, roles = ["ex_gratia"]),
        account("5020", "Claim recovery write-offs", "expense", control = True, roles = ["claim_recovery_write_off"]),
        account("5030", "Claims handling expenses", "expense", control = True, roles = ["claims_handling"]),
        account("5040", "Interest on claims paid late", "expense", control = True, roles = ["claim_interest"]),
        account("5200", "Reinsurance premiums ceded", "expense", control = True, roles = ["reinsurance_premium"]),
        account("5300", "Unit pricing errors borne by the insurer", "expense", control = True, roles = ["pricing_error"]),
        account("5100", "Lapse write-offs", "expense", control = True, roles = ["lapse_write_off"]),
        account("5110", "Policy loans written off", "expense", control = True, roles = ["policy_loan_written_off"]),
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
