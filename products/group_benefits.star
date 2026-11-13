# Workplace Benefits: a group scheme with life cover every member has by
# formula and a health menu each member chooses from, for themselves and
# for their dependants. Each chosen cover is paid for by the employer or by
# the member. Synthetic terms for development and tests: the menu, limits,
# rules and rates are placeholders, not terms anyone has approved.

PRODUCT = product(
    code = "group-benefits",
    name = "Workplace Benefits",
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
        # The menu. A member holds none, some or all of these.
        cover(
            code = "hospital",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("surgical", per_event = dollars(300000), per_year = dollars(500000), excess = dollars(500)),
            ]),
            rating = per_life("flex-hospital"),
            excess_options = [
                excess_option("nil", dollars(0), premium_factor = "1.20"),
                excess_option("standard", dollars(500), premium_factor = "1.00"),
                excess_option("high", dollars(2000), premium_factor = "0.80"),
            ],
            definitions = ["pre-existing-condition"],
            gst_rate = "0.15",
        ),
        cover(
            code = "specialist",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("specialist", per_year = dollars(10000)),
                benefit_limit("diagnostics", per_year = dollars(5000)),
            ]),
            rating = per_life("flex-specialist"),
            definitions = ["pre-existing-condition"],
            gst_rate = "0.15",
        ),
        cover(
            code = "dental",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("dental", per_event = dollars(500), per_year = dollars(1500)),
            ]),
            rating = per_life("flex-dental"),
            gst_rate = "0.15",
        ),
    ],
    features = [
        # The specialist upgrade is held only with the hospital plan.
        composition(requires = {"specialist": ["hospital"]}),
        billing(frequencies = ["monthly"]),
        earning(),
        arrears(grace_days = 45, lapse_days = 120, reinstatement_days = 90),
    ],
)
