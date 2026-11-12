# Everyday Health: a health plan with a benefit schedule. Premiums carry GST.

PRODUCT = product(
    code = "health-plan",
    name = "Everyday Health",
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
                benefit_limit("specialist", per_year = dollars(10000)),
                benefit_limit("gp", per_event = dollars(60), per_year = dollars(600)),
            ]),
            rating = per_life("health-base"),
            definitions = ["pre-existing-condition"],
            gst_rate = "0.15",
        ),
    ],
    features = [
        billing(frequencies = ["fortnightly", "monthly", "yearly"], policy_fee_annual = "0"),
        earning(),
        arrears(grace_days = 30, lapse_days = 60, reinstatement_days = 90, reinstatement_evidence_free_days = 30),
        cooling_off(days = 14),
    ],
)
