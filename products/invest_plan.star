# Kiwi Invest Plan: a unit-linked savings policy on three funds.
# Synthetic terms for development and tests. The fees are placeholders, not
# terms anyone has approved.

PRODUCT = product(
    code = "invest-plan",
    name = "Kiwi Invest Plan",
    version = 1,
    effective_from = "2027-01-01",
    line = "investment",
    currency = "NZD",
    statutory_fund = "SF2-investment",
    rounding = "half_even",
    covers = [
        cover(
            code = "account",
            cover_type = "investment",
            benefit = investment_account(["cash", "balanced", "growth"]),
            rating = per_life("no-premium"),
        ),
    ],
    features = [
        billing(frequencies = ["monthly", "yearly"]),
        earning(),
        # Nothing is billed, so nothing can fall into arrears. The engine
        # asks every product for the feature.
        arrears(grace_days = 30, lapse_days = 90),
        cooling_off(days = 14),
        # The owner may borrow up to half the account's value. Placeholder.
        policy_loans(max_percent_of_value = "50", rate_code = "policy-loan-rate"),
    ],
)
