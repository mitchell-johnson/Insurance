# Workplace Health: an employer health scheme with no cover by formula. The
# scheme starts with its members and no cover; each member chooses from the
# menu, for themselves and for their dependants, and each chosen cover is
# paid for by the employer or by the member. Synthetic terms for development and tests: the menu, limits,
# rules and rates are placeholders, not terms anyone has approved.

PRODUCT = product(
    code = "group-health",
    name = "Workplace Health",
    version = 1,
    effective_from = "2027-01-01",
    line = "group",
    currency = "NZD",
    statutory_fund = "SF2-health",
    covers = [
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
