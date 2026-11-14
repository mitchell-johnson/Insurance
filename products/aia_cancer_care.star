# AIA Cancer Care: health insurance for the diagnosis, treatment and recovery
# of cancer only. Wording 1304 AIA Cancer Care-UMB v5, effective 1 November
# 2026. Built from the published wording as at November 2026
# (docs/aia-products.md); rates and factors are placeholders marked PH.

# ---- The published wordings each cover is bound to ---------------------------------

# Each cover names the AIA wordings that govern it (products/definitions/
# aia_wordings.star, the text as published), beside the summaries above. A
# cover missing from this map fails to load.
WORDINGS = {
    "cancer-care": ["aia-wording-1304-cancer-care"],
}

def with_wordings(covers):
    return [dict(c, definitions = c["definitions"] + WORDINGS[c["code"]]) for c in covers]

PRODUCT = product(
    code = "aia-cancer-care",
    name = "AIA Cancer Care",
    version = 1,
    effective_from = "2026-11-14",
    line = "health",
    currency = "NZD",
    statutory_fund = "SF2-health",
    covers = with_wordings([
        cover(
            code = "cancer-care",
            cover_type = "health",
            benefit = health_schedule([
                # Cancer surgery is unlimited, including accommodation.
                benefit_limit("cancer-surgery"),
                benefit_limit("cancer-treatment", per_year = dollars(500000)),
                benefit_limit("cancer-diagnostics", per_year = dollars(5000)),
                benefit_limit("post-cancer-care", per_year = dollars(1000)),
                benefit_limit("public-hospital-cancer-cash", per_lifetime = dollars(5000)),
                benefit_limit("palliative-hospice-respite", per_lifetime = dollars(1500)),
                benefit_limit("dental-before-treatment", per_year = dollars(1500)),
                benefit_limit("mental-health-support", per_year = dollars(2500)),
                benefit_limit("minor-surgery", per_year = dollars(3000)),
                benefit_limit("treatment-away-from-home", per_year = dollars(3000)),
                benefit_limit("ambulance-transfer", per_year = dollars(200)),
                benefit_limit("treatment-overseas", per_year = dollars(30000)),
                benefit_limit("public-hospital-credit", per_year = dollars(3000)),
                benefit_limit("public-hospital-cash-grant", per_lifetime = dollars(30000)),
            ]),
            rating = per_life("aia-cancer-care"),
            excess_options = [
                excess_option("0", dollars(0), premium_factor = "1.30"),  # PH: the levels follow AIA Private Health
                excess_option("500", dollars(500), premium_factor = "1.00"),
                excess_option("2000", dollars(2000), premium_factor = "0.78"),
            ],
            definitions = ["aia-cancer-care"],
            gst_rate = "0.15",
            min_entry_age = 0,
            max_entry_age = 74,  # PH
            built_in = [
                built_in("waiver-on-death", "Waiver of Premium on Death (two years' free cover for the family)", "premium_waiver"),
                built_in("cancer-screening", "Cancer Screening Health Allowance", "reimbursement", max = dollars(500)),  # PH amount
                built_in("suspension", "Suspension of Cover Benefit", "facility"),
            ],
        ),
    ]),
    features = [
        billing(frequencies = ["fortnightly", "monthly", "quarterly", "half_yearly", "yearly"], policy_fee_annual = "0"),
        earning(),
        arrears(grace_days = 30, lapse_days = 31, reinstatement_days = 90, reinstatement_evidence_free_days = 30),  # PH
        discounts(chain = [
            discount("vitality", "AIA Vitality discount", "vitality_discount",
                     by_tier({"%d" % p: "%d" % p for p in range(0, 21)}), condition = entitled("aia-vitality")),
        ]),
        cooling_off(days = 20),
    ],
)
