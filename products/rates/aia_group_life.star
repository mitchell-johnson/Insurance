# Placeholder rates for the AIA products. Not AIA's rates and not an
# actuarial basis: best estimates until the appointed actuary supplies the
# real table (docs/aia-products.md). Annual unit rate per thousand of cover
# (of monthly benefit for income), by benefit category; a scheme's own
# table replaces it.

RATE_TABLE = rate_table(
    id = "aia-group-life",
    version = 1,
    effective_from = "2026-11-14",
    rows = [
        rate("0.95", category = "staff"),
        rate("1.20", category = "executive"),
    ],
)
