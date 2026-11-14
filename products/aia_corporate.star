# AIA Corporate Solutions: employer group schemes. Cover is set per member
# by benefit category as a multiple of salary, up to a category maximum and
# the scheme's automatic acceptance limit; above it, a member is
# underwritten. Rates are per thousand of cover per category and are
# supplied per scheme by the pricing function as a scoped rate table; the
# shipped tables are placeholders (docs/aia-products.md). The scheme's own
# policy document governs each claim (definitions aia-group-*). Categories,
# multiples, maximums and limits marked PH are placeholders, set per scheme
# in practice.

def categories(life_multiple, exec_multiple, staff_max, exec_max):
    return [
        benefit_category("staff", salary_multiple = life_multiple, max_sum_insured = staff_max),
        benefit_category("executive", salary_multiple = exec_multiple, max_sum_insured = exec_max),
    ]

PRODUCT = product(
    code = "aia-corporate",
    name = "AIA Corporate Solutions",
    version = 1,
    effective_from = "2026-11-14",
    line = "group",
    currency = "NZD",
    statutory_fund = "SF1-life",
    covers = [
        cover(
            code = "group-life",
            cover_type = "group_life",
            benefit = lump_sum(),
            rating = group_unit_rate("aia-group-life"),
            definitions = ["aia-group-life"],
            categories = categories("3", "5", dollars(1500000), dollars(5000000)),  # PH
            automatic_acceptance_limit = dollars(1000000),  # PH
            expiry_age = 70,
        ),
        # Optional, paid with life cover and reducing it by what it pays.
        cover(
            code = "group-tpd",
            cover_type = "group_life",
            benefit = lump_sum(),
            rating = group_unit_rate("aia-group-tpd"),
            definitions = ["aia-group-tpd"],
            categories = categories("3", "5", dollars(1500000), dollars(3000000)),  # PH
            automatic_acceptance_limit = dollars(750000),  # PH
            expiry_age = 65,
            reduces = ["group-life"],
        ),
        cover(
            code = "group-critical-illness",
            cover_type = "group_life",
            benefit = lump_sum(survival_days = 14, ends_cover = False),
            rating = group_unit_rate("aia-group-critical-illness"),
            definitions = ["aia-group-critical-illness"],
            categories = categories("1", "1", dollars(250000), dollars(500000)),  # PH
            automatic_acceptance_limit = dollars(250000),  # PH
            expiry_age = 65,
        ),
        # Severe Trauma for Corporate Solutions (from 5 May 2026): fewer,
        # more severe conditions, paid in full only.
        cover(
            code = "group-severe-trauma",
            cover_type = "group_life",
            benefit = lump_sum(survival_days = 14, ends_cover = False),
            rating = group_unit_rate("aia-group-severe-trauma"),
            definitions = ["aia-group-critical-illness"],
            categories = categories("1", "1", dollars(250000), dollars(500000)),  # PH
            automatic_acceptance_limit = dollars(250000),  # PH
            expiry_age = 65,
        ),
        # A monthly share of salary: 75% a year is 0.0625 of annual salary a
        # month. Waiting and benefit periods are the scheme's (PH).
        cover(
            code = "group-income",
            cover_type = "group_life",
            benefit = monthly_benefit(waiting_days = 90, benefit_months = 24),
            rating = group_unit_rate("aia-group-income"),
            definitions = ["aia-group-income"],
            categories = categories("0.0625", "0.0625", dollars(30000), dollars(50000)),  # PH
            automatic_acceptance_limit = dollars(20000),  # PH
            expiry_age = 65,
        ),
    ],
    features = [
        composition(requires = {
            "group-tpd": ["group-life"],
            "group-critical-illness": ["group-life"],
            "group-severe-trauma": ["group-life"],
        }, exclusive = [["group-critical-illness", "group-severe-trauma"]]),
        billing(frequencies = ["monthly", "quarterly", "yearly"]),
        earning(),
        arrears(grace_days = 45, lapse_days = 120, reinstatement_days = 90),
    ],
)
