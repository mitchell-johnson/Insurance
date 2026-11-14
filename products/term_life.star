# Kiwi Term Life: retail life and trauma cover, stepped premium, indexation.
# The spine's simple term life product: no commission and no reinsurance yet.

PRODUCT = product(
    code = "term-life",
    name = "Kiwi Term Life",
    version = 1,
    effective_from = "2027-01-01",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = [
        cover(
            code = "life",
            cover_type = "life",
            benefit = lump_sum(),
            rating = stepped("life-base"),
            definitions = ["death", "terminal-illness"],
            expiry_age = 100,
            min_sum_insured = dollars(50000),
            max_sum_insured = dollars(5000000),
            # More cover without evidence of health after a life event.
            # Placeholder limits, not terms anyone has approved.
            insurability_option = insurability_option(
                events = ["marriage", "birth_or_adoption", "mortgage"],
                max_percent_of_original = "25",
                max_increase = dollars(200000),
                max_uses = 3,
                within_days = 90,
                to_age = 55,
            ),
        ),
        cover(
            code = "trauma",
            cover_type = "trauma",
            benefit = lump_sum(survival_days = 14, ends_cover = False),
            rating = stepped("trauma-base"),
            definitions = ["cancer", "heart-attack", "stroke"],
            expiry_age = 70,
            min_sum_insured = dollars(20000),
            max_sum_insured = dollars(2000000),
        ),
    ],
    features = [
        billing(
            frequencies = ["fortnightly", "monthly", "yearly"],
            policy_fee_annual = "72.00",
            modal_loading = {"fortnightly": "0.04", "monthly": "0.04"},
        ),
        earning(),
        arrears(grace_days = 30, lapse_days = 90, suspend_after_grace = True, reinstatement_days = 180, reinstatement_evidence_free_days = 60),
        indexation(rate_code = "cpi"),
        multi_cover_discount(min_covers = 2, percent = "5"),
        cooling_off(days = 14),
    ],
)
