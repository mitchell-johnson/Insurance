# Workplace Life: a corporate group life scheme. Benefits are a multiple of
# salary by category; rates are per thousand of cover per category and are
# supplied per scheme by the pricing function as a scoped rate table.

PRODUCT = product(
    code = "group-life",
    name = "Workplace Life",
    version = 1,
    effective_from = "2027-01-01",
    line = "group",
    currency = "NZD",
    statutory_fund = "SF1-life",
    covers = [
        cover(
            code = "group-life",
            cover_type = "group_life",
            benefit = lump_sum(),
            rating = group_unit_rate("group-life-standard"),
            definitions = ["death", "terminal-illness"],
            categories = [
                benefit_category("staff", salary_multiple = "2", max_sum_insured = dollars(1000000)),
                benefit_category("executive", salary_multiple = "4", max_sum_insured = dollars(3000000)),
            ],
            automatic_acceptance_limit = dollars(750000),
            expiry_age = 70,
        ),
    ],
    features = [
        billing(frequencies = ["monthly"]),
        earning(),
        arrears(grace_days = 45, lapse_days = 120, reinstatement_days = 90),
    ],
)
