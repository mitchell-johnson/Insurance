# Kiwi Interim Accident Cover: accidental death cover, free of charge, while
# a term life application is being underwritten. Underwriting issues it when
# the proposal opens (settings.interim) and ends it when the policy is
# issued or the proposal ends. Synthetic terms: the amount and the wording
# are placeholders until product and legal approve them.

PRODUCT = product(
    code = "interim-accident",
    name = "Kiwi Interim Accident Cover",
    version = 1,
    effective_from = "2027-01-01",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = [
        cover(
            code = "accidental-death",
            cover_type = "life",
            benefit = lump_sum(),
            rating = per_life("no-premium"),
            definitions = ["accidental-death"],
            expiry_age = 100,
            min_sum_insured = dollars(1),
            max_sum_insured = dollars(1000000),
        ),
    ],
    features = [
        billing(frequencies = ["fortnightly", "monthly", "yearly"]),
        earning(),
        # Nothing is billed, so nothing can fall into arrears. The engine
        # asks every product for the feature.
        arrears(grace_days = 30, lapse_days = 90),
        cooling_off(days = 14),
    ],
)
