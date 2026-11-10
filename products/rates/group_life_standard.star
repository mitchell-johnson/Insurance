# Synthetic rates for development and tests. Not an actuarial basis.
# Annual unit rate per thousand of cover, by benefit category.

RATE_TABLE = rate_table(
    id = "group-life-standard",
    version = 1,
    effective_from = "2027-01-01",
    rows = [
        rate("1.10", category = "staff"),
        rate("1.45", category = "executive"),
    ],
)
