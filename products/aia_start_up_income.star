# AIA Start-Up Income Protection: income cover for owners of a business that
# has traded for three years or less, before the business can prove the
# profit Business Continuity needs. The monthly benefit is agreed at
# application from a share of business revenue. A separate policy from AIA
# Living (wording 1180 A-SU IP v3), with the enhancement pass back benefit.
#
# Built from AIA's published wording and business brochure as at November
# 2026 (docs/aia-products.md). Rates and factors are placeholders marked PH.

# ---- The published wordings each cover is bound to ---------------------------------

# Each cover names the AIA wordings that govern it (products/definitions/
# aia_wordings.star, the text as published), beside the summaries above. A
# cover missing from this map fails to load.
WORDINGS = {
    "start-up-income": ["aia-wording-1180-a-su-ip"],
}

def with_wordings(covers):
    return [dict(c, definitions = c["definitions"] + WORDINGS[c["code"]]) for c in covers]

PRODUCT = product(
    code = "aia-start-up-income",
    name = "AIA Start-Up Income Protection",
    version = 1,
    effective_from = "2026-11-14",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = with_wordings([
        cover(
            code = "start-up-income",
            cover_type = "income_protection",
            benefit = monthly_benefit(waiting_days = 28, benefit_months = 12),
            rating = stepped("aia-business-continuity"),
            premium_waiver = True,
            definitions = ["aia-start-up-income"],
            min_entry_age = 16,
            max_entry_age = 60,  # PH
            expiry_age = 65,
            min_sum_insured = dollars(500),  # PH: monthly benefit
            max_sum_insured = dollars(10000),  # PH
            option_axes = [
                option_axis("waiting", "Waiting period", [
                    choice("4w", "4 weeks", premium_factor = "1.00", waiting_days = 28),  # PH
                    choice("8w", "8 weeks", premium_factor = "0.78", waiting_days = 56),  # PH
                    choice("13w", "13 weeks", premium_factor = "0.62", waiting_days = 91),  # PH
                ]),
                option_axis("period", "Benefit period", [
                    choice("6m", "6 months", premium_factor = "0.62", benefit_months = 6),  # PH
                    choice("12m", "12 months", premium_factor = "1.00", benefit_months = 12),
                    choice("24m", "24 months", premium_factor = "1.45", benefit_months = 24),  # PH
                ]),
            ],
            built_in = [
                built_in("recurrent-disablement", "Recurrent Disablement Benefit", "facility"),
                built_in("enhancement-pass-back", "Enhancement Pass Back Benefit", "facility"),
                built_in("move-to-business-continuity", "Move to Business Continuity after three years of trading", "facility"),
            ],
        ),
    ]),
    features = [
        billing(
            frequencies = ["fortnightly", "monthly", "yearly"],
            policy_fee_annual = "75.00",  # PH
            modal_loading = {"fortnightly": "0.06", "monthly": "0.06"},  # PH
        ),
        earning(),
        arrears(grace_days = 30, lapse_days = 31, reinstatement_days = 180, reinstatement_evidence_free_days = 60),  # PH
        indexation(rate_code = "cpi"),
        # Start-Up Income Protection counts towards the Multi-Benefit
        # Discount but sits outside AIA Living, so it does not receive it.
        discounts(chain = [
            discount("vitality", "AIA Vitality discount", "vitality_discount",
                     by_tier({"%d" % p: "%d" % p for p in range(0, 21)}), condition = entitled("aia-vitality")),
        ]),
        cooling_off(days = 20),
    ],
)
