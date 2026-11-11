# Synthetic reinsurer rates for development and tests. Not an actuarial basis.
# Annual rate per thousand of ceded sum at risk.

RATE_TABLE = reinsurer_rate_table(
    id = "ri-life-base",
    version = 1,
    effective_from = "2027-01-01",
    rows = [
        reinsurer_rate("0.65", age_to = 39, smoker = False),
        reinsurer_rate("1.30", age_to = 39, smoker = True),
        reinsurer_rate("1.80", age_from = 40, age_to = 59, smoker = False),
        reinsurer_rate("3.90", age_from = 40, age_to = 59, smoker = True),
        reinsurer_rate("9.50", age_from = 60),
    ],
)
