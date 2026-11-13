# Flex Health: a mix-and-match health product. The shell declares the menu
# of covers and the rules for combining them; a policy is the covers each
# life chose. Synthetic terms for development and tests: the menu, limits,
# rules and discount are placeholders, not terms anyone has approved.

PRODUCT = product(
    code = "health-flex",
    name = "Flex Health",
    version = 1,
    effective_from = "2027-01-01",
    line = "health",
    currency = "NZD",
    statutory_fund = "SF2-health",
    covers = [
        cover(
            code = "hospital",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("surgical", per_event = dollars(300000), per_year = dollars(500000), excess = dollars(500)),
            ]),
            rating = per_life("flex-hospital"),
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
        cover(
            code = "optical",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("optical", per_year = dollars(600)),
            ]),
            rating = per_life("flex-optical"),
            gst_rate = "0.15",
        ),
    ],
    features = [
        composition(
            # Everyday covers can stand alone; specialist needs hospital.
            core = ["hospital", "dental", "optical"],
            requires = {"specialist": ["hospital"]},
        ),
        billing(frequencies = ["fortnightly", "monthly", "yearly"], policy_fee_annual = "0"),
        earning(),
        arrears(grace_days = 30, lapse_days = 60, reinstatement_days = 90, reinstatement_evidence_free_days = 30),
        multi_cover_discount(min_covers = 3, percent = "5"),
        cooling_off(days = 14),
    ],
)
