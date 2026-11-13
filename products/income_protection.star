# Kiwi Income Protection: a monthly benefit while the life cannot work
# through illness or injury, after a waiting period, for up to five years a
# claim. The sum insured is the monthly benefit.

PRODUCT = product(
    code = "income-protection",
    name = "Kiwi Income Protection",
    version = 1,
    effective_from = "2027-01-01",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = [
        cover(
            code = "income",
            cover_type = "income_protection",
            # In custody after sentence the benefit is suspended; the claim,
            # the premium waiver and the cover carry on (docs/decisions.md,
            # decision 5).
            benefit = monthly_benefit(waiting_days = 28, benefit_months = 60, benefit_while_imprisoned = "suspend"),
            rating = stepped("income-base"),
            premium_waiver = True,
            definitions = ["total-disability"],
            expiry_age = 65,
            min_sum_insured = dollars(500),
            max_sum_insured = dollars(30000),
        ),
    ],
    features = [
        billing(
            frequencies = ["fortnightly", "monthly", "yearly"],
            policy_fee_annual = "72.00",
            modal_loading = {"fortnightly": "0.04", "monthly": "0.04"},
        ),
        earning(),
        arrears(grace_days = 30, lapse_days = 90, suspend_after_grace = True, reinstatement_days = 180, reinstatement_evidence_free_days = 30),
        indexation(rate_code = "cpi"),
        cooling_off(days = 14),
    ],
)
