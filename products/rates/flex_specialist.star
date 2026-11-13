# Synthetic rates for development and tests. Not an actuarial basis.
# Annual rate per life, excluding GST.

RATE_TABLE = rate_table(
    id = "flex-specialist",
    version = 1,
    effective_from = "2027-01-01",
    rows = [
        rate("120.00", age_from = 0, age_to = 20),
        rate("180.00", age_from = 21, age_to = 40),
        rate("300.00", age_from = 41, age_to = 60),
        rate("480.00", age_from = 61, age_to = 150),
    ],
)
