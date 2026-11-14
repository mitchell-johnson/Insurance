# Placeholder rates for the AIA products (tools/aia_placeholder_rates.py).
# Not AIA's rates and not an actuarial basis: best estimates until the
# appointed actuary supplies the real table (docs/aia-products.md).
# Annual premium per life, before GST.

RATE_TABLE = rate_table(
    id = "aia-private-health-plus",
    version = 1,
    effective_from = "2026-11-14",
    rows = [
        rate("100.83", age_from = 0, age_to = 4),
        rate("108.89", age_from = 5, age_to = 9),
        rate("117.60", age_from = 10, age_to = 14),
        rate("127.01", age_from = 15, age_to = 19),
        rate("137.17", age_from = 20, age_to = 24),
        rate("148.15", age_from = 25, age_to = 29),
        rate("160.00", age_from = 30, age_to = 34),
        rate("208.00", age_from = 35, age_to = 39),
        rate("270.40", age_from = 40, age_to = 44),
        rate("351.52", age_from = 45, age_to = 49),
        rate("456.98", age_from = 50, age_to = 54),
        rate("594.07", age_from = 55, age_to = 59),
        rate("772.29", age_from = 60, age_to = 64),
        rate("1003.98", age_from = 65, age_to = 69),
        rate("1305.17", age_from = 70, age_to = 74),
        rate("1696.72", age_from = 75, age_to = 79),
        rate("2205.74", age_from = 80, age_to = 84),
        rate("2867.46", age_from = 85, age_to = 89),
        rate("3727.69", age_from = 90, age_to = 94),
        rate("4846.00", age_from = 95, age_to = 99),
        rate("4846.00", age_from = 100, age_to = 104),
        rate("4846.00", age_from = 105, age_to = 109),
        rate("4846.00", age_from = 110, age_to = 114),
        rate("4846.00", age_from = 115, age_to = 119),
        rate("4846.00", age_from = 120, age_to = 124),
        rate("4846.00", age_from = 125, age_to = 129),
        rate("4846.00", age_from = 130, age_to = 134),
        rate("4846.00", age_from = 135, age_to = 139),
        rate("4846.00", age_from = 140, age_to = 144),
        rate("4846.00", age_from = 145, age_to = 149),
        rate("4846.00", age_from = 150, age_to = 150),
    ],
)
