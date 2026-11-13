# Synthetic rates for development and tests. Not an actuarial basis.
# Annual rate per life, excluding GST.

RATE_TABLE = rate_table(
    id = "flex-hospital",
    version = 1,
    effective_from = "2027-01-01",
    rows = [
        rate("360.00", age_from = 0, age_to = 20),
        rate("660.00", age_from = 21, age_to = 30),
        rate("840.00", age_from = 31, age_to = 40),
        rate("1200.00", age_from = 41, age_to = 50),
        rate("1800.00", age_from = 51, age_to = 60),
        rate("2760.00", age_from = 61, age_to = 70),
        rate("3900.00", age_from = 71, age_to = 150),
    ],
)
