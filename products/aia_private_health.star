# AIA Private Health: medical insurance for surgery, cancer treatment,
# diagnostics and hospital care at approved facilities in New Zealand, with
# the optional Private Health Plus upgrade (specialist consultations, more
# diagnostics, pregnancy, maternity and infertility, health screening).
# Wording 1300 APH-UMB v8, effective 1 November 2026 (renewals from 23
# October 2026); Waiver of Premium 1301 APH-WOP v1.
#
# Benefit limits are per life assured per policy year unless marked as a
# lifetime limit. Health cover applies only in New Zealand, carries GST, and
# is not part of the Multi-Benefit Discount. Built from the published wording
# as at November 2026 (docs/aia-products.md); rates and factors are
# placeholders marked PH. Claims holds the claimable benefits in its own
# configuration (docs/claims.md); this schedule is what the policy promises.

EXCESSES = [(0, "1.55"), (250, "1.32"), (500, "1.18"), (750, "1.09"), (1000, "1.00"), (2000, "0.84"), (4000, "0.68"), (10000, "0.47")]  # PH factors

# ---- The published wordings each cover is bound to ---------------------------------

# Each cover names the AIA wordings that govern it (products/definitions/
# aia_wordings.star, the text as published), beside the summaries above. A
# cover missing from this map fails to load.
WORDINGS = {
    "private-health": ["aia-wording-1300-aph-umb", "aia-wording-1301-aph-wop"],
    "private-health-plus": ["aia-wording-1300-aph-umb", "aia-wording-1301-aph-wop"],
}

def with_wordings(covers):
    return [dict(c, definitions = c["definitions"] + WORDINGS[c["code"]]) for c in covers]

PRODUCT = product(
    code = "aia-private-health",
    name = "AIA Private Health",
    version = 1,
    effective_from = "2026-11-14",
    line = "health",
    currency = "NZD",
    statutory_fund = "SF2-health",
    covers = with_wordings([
        cover(
            code = "private-health",
            cover_type = "health",
            benefit = health_schedule([
                # Surgery is unlimited, including accommodation.
                benefit_limit("surgery"),
                benefit_limit("cancer-treatment", per_year = dollars(500000)),
                benefit_limit("post-cancer-care", per_year = dollars(1000)),
                benefit_limit("public-hospital-cancer-cash", per_lifetime = dollars(5000)),
                benefit_limit("palliative-hospice-respite", per_lifetime = dollars(1500)),
                benefit_limit("dental-before-treatment", per_year = dollars(1500)),
                benefit_limit("mental-health-support", per_year = dollars(2500)),
                benefit_limit("minor-surgery", per_year = dollars(3000)),
                benefit_limit("medical-hospitalisation", per_year = dollars(500000)),
                benefit_limit("congenital-conditions-surgery", per_lifetime = dollars(2000)),
                benefit_limit("major-diagnostics", per_year = dollars(200000)),
                benefit_limit("home-nursing", per_year = dollars(5000)),
                benefit_limit("bereavement-grant", per_lifetime = dollars(3500)),
                benefit_limit("treatment-away-from-home", per_year = dollars(3000)),
                benefit_limit("ambulance-transfer", per_year = dollars(200)),
                benefit_limit("treatment-overseas", per_year = dollars(30000)),
                benefit_limit("public-hospital-credit", per_year = dollars(3000)),
                benefit_limit("public-hospital-cash-grant", per_lifetime = dollars(30000)),
            ]),
            rating = per_life("aia-private-health"),
            excess_options = [excess_option("%d" % e, dollars(e), premium_factor = f) for (e, f) in EXCESSES],
            definitions = ["aia-private-health"],
            gst_rate = "0.15",
            min_entry_age = 0,
            max_entry_age = 74,  # PH
            built_in = [
                built_in("waiver-on-death", "Waiver of Premium on Death (two years' free cover for the family)", "premium_waiver"),
                built_in("suspension", "Suspension of Cover Benefit", "facility"),
            ],
        ),
        cover(
            code = "private-health-plus",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("specialist", per_year = dollars(10000)),
                benefit_limit("extra-diagnostics", per_year = dollars(100000)),
                benefit_limit("pregnancy-maternity-infertility", per_year = dollars(1500)),
                benefit_limit("health-screening", per_year = dollars(500)),
            ]),
            rating = per_life("aia-private-health-plus"),
            # The Plus benefits have their own excess: $0 or $250.
            excess_options = [
                excess_option("0", dollars(0), premium_factor = "1.22"),  # PH
                excess_option("250", dollars(250), premium_factor = "1.00"),
            ],
            definitions = ["aia-private-health"],
            gst_rate = "0.15",
        ),
    ]),
    features = [
        composition(core = ["private-health"], requires = {"private-health-plus": ["private-health"]}),
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
