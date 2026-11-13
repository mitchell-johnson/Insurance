# Synthetic rates for development and tests. Not an actuarial basis.
# Annual rate per life, excluding GST.

RATE_TABLE = rate_table(
    id = "flex-optical",
    version = 1,
    effective_from = "2027-01-01",
    rows = [
        rate("60.00", age_from = 0, age_to = 20),
        rate("132.00", age_from = 21, age_to = 150),
    ],
)
