# AIA Living Interim Cover (wording X00066 002a 2512): limited cover, free
# of charge, while an AIA Living application is underwritten. Underwriting
# issues it when the proposal opens (settings.interim names this product)
# and ends it when the policy is issued, the application is withdrawn or 60
# days pass. Each benefit pays the lower of the amount applied for and the
# maximum below; across all benefits for one claim event at most $500,000
# (`event_cap` below, which claims applies). Claim events other
# than death must result from accidental, violent, external and visible
# injury. Definitions are those of the AIA Living appendices.

# ---- The published wordings each cover is bound to ---------------------------------

# Each cover names the AIA wordings that govern it (products/definitions/
# aia_wordings.star, the text as published), beside the summaries above. A
# cover missing from this map fails to load.
WORDINGS = {
    "life": ["aia-wording-interim", "aia-wording-1101-al-life"],
    "accidental-tpd": ["aia-wording-interim", "aia-wording-1104-al-tpd"],
    "accidental-trauma": ["aia-wording-interim", "aia-wording-1105-al-cc", "aia-wording-1106-al-pc", "aia-wording-1119-al-st"],
    "accidental-income": ["aia-wording-interim", "aia-wording-1108-al-ip"],
}

def with_wordings(covers):
    return [dict(c, definitions = c["definitions"] + WORDINGS[c["code"]]) for c in covers]

PRODUCT = product(
    code = "aia-interim",
    name = "AIA Living Interim Cover",
    version = 1,
    effective_from = "2026-11-14",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    event_cap = dollars(500000),
    covers = with_wordings([
        cover(
            code = "life",
            cover_type = "life",
            benefit = lump_sum(),
            rating = per_life("no-premium"),
            definitions = ["aia-death"],
            expiry_age = 100,
            min_sum_insured = dollars(1),
            max_sum_insured = dollars(500000),
        ),
        cover(
            code = "accidental-tpd",
            cover_type = "life",
            benefit = lump_sum(),
            rating = per_life("no-premium"),
            definitions = ["aia-tpd-own-occupation", "aia-accidental-death"],
            expiry_age = 100,
            min_sum_insured = dollars(1),
            max_sum_insured = dollars(150000),
        ),
        cover(
            code = "accidental-trauma",
            cover_type = "trauma",
            benefit = lump_sum(survival_days = 14, ends_cover = False),
            rating = per_life("no-premium"),
            definitions = ["aia-critical-conditions", "aia-progressive-care", "aia-severe-trauma"],
            expiry_age = 100,
            min_sum_insured = dollars(1),
            max_sum_insured = dollars(150000),
        ),
        cover(
            code = "accidental-income",
            cover_type = "income_protection",
            benefit = monthly_benefit(waiting_days = 0, benefit_months = 24),
            rating = per_life("no-premium"),
            definitions = ["aia-ip-total-disability"],
            expiry_age = 100,
            min_sum_insured = dollars(1),
            max_sum_insured = dollars(2000),
        ),
    ]),
    features = [
        billing(frequencies = ["fortnightly", "monthly", "yearly"]),
        earning(),
        # Nothing is billed, so nothing falls into arrears; every product
        # declares the feature.
        arrears(grace_days = 30, lapse_days = 90),
        cooling_off(days = 14),
    ],
)
