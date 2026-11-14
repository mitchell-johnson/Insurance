# Placeholder rates for the AIA products (tools/aia_placeholder_rates.py).
# Not AIA's rates and not an actuarial basis: best estimates until the
# appointed actuary supplies the real table (docs/aia-products.md).
# Annual premium per life, before GST.

RATE_TABLE = rate_table(
    id = "aia-specialist-testing",
    version = 1,
    effective_from = "2026-11-14",
    rows = [
        rate("59.87", age_from = 0, age_to = 4),
        rate("64.66", age_from = 5, age_to = 9),
        rate("69.83", age_from = 10, age_to = 14),
        rate("75.41", age_from = 15, age_to = 19),
        rate("81.45", age_from = 20, age_to = 24),
        rate("87.96", age_from = 25, age_to = 29),
        rate("95.00", age_from = 30, age_to = 34),
        rate("123.50", age_from = 35, age_to = 39),
        rate("160.55", age_from = 40, age_to = 44),
        rate("208.72", age_from = 45, age_to = 49),
        rate("271.33", age_from = 50, age_to = 54),
        rate("352.73", age_from = 55, age_to = 59),
        rate("458.55", age_from = 60, age_to = 64),
        rate("596.11", age_from = 65, age_to = 69),
        rate("774.94", age_from = 70, age_to = 74),
        rate("1007.43", age_from = 75, age_to = 79),
        rate("1309.66", age_from = 80, age_to = 84),
        rate("1702.55", age_from = 85, age_to = 89),
        rate("2213.32", age_from = 90, age_to = 94),
        rate("2877.31", age_from = 95, age_to = 99),
        rate("2877.31", age_from = 100, age_to = 104),
        rate("2877.31", age_from = 105, age_to = 109),
        rate("2877.31", age_from = 110, age_to = 114),
        rate("2877.31", age_from = 115, age_to = 119),
        rate("2877.31", age_from = 120, age_to = 124),
        rate("2877.31", age_from = 125, age_to = 129),
        rate("2877.31", age_from = 130, age_to = 134),
        rate("2877.31", age_from = 135, age_to = 139),
        rate("2877.31", age_from = 140, age_to = 144),
        rate("2877.31", age_from = 145, age_to = 149),
        rate("2877.31", age_from = 150, age_to = 150),
    ],
)
