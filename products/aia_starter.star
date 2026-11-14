# AIA Starter Plans: simple plans bought online. Healthy Starter is rent
# cover; Starter Life is life cover; Starter Life Plus is both, with rent or
# mortgage cover. Every plan includes an AIA Vitality Starter membership and
# may add Specialist and Diagnostic Testing. One wording covers all three
# (AIA Starter Plan version 1); a plan is the set of covers a life holds.
#
# Built from AIA's product pages and wording as at November 2026
# (docs/aia-products.md). Rates and factors are placeholders marked PH.

PRODUCT = product(
    code = "aia-starter",
    name = "AIA Starter Plans",
    version = 1,
    effective_from = "2026-11-14",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = [
        cover(
            code = "life",
            cover_type = "life",
            benefit = lump_sum(),
            rating = stepped("aia-life"),
            definitions = ["aia-death", "aia-terminal-illness"],
            min_entry_age = 18,  # PH
            max_entry_age = 55,  # PH
            expiry_age = 70,  # PH
            min_sum_insured = dollars(100000),
            max_sum_insured = dollars(300000),
            option_axes = [option_axis("basis", "Underwriting basis", [choice("online", "Online", premium_factor = "1.12")])],  # PH
            built_in = [
                built_in("terminal-illness", "Terminal Illness Benefit", "early_payment", percent_of_cover = "100",
                         definitions = ["aia-terminal-illness"]),
                built_in("bereavement-support", "Bereavement Support Benefit", "early_payment", max = dollars(15000)),
            ],
        ),
        cover(
            code = "rent-mortgage",
            cover_type = "income_protection",
            benefit = monthly_benefit(waiting_days = 56, benefit_months = 6),
            rating = stepped("aia-income"),
            premium_waiver = True,
            definitions = ["aia-ip-total-disability", "aia-mortgage-income-rent"],
            min_entry_age = 18,  # PH
            max_entry_age = 55,  # PH
            # Rent or mortgage cover ends when the life reaches 65.
            expiry_age = 65,
            min_sum_insured = dollars(1000),
            max_sum_insured = dollars(3000),
            option_axes = [
                option_axis("kind", "Cover for", [
                    choice("rent", "Rent"),
                    choice("mortgage", "Mortgage repayments", features = ["mortgage"]),
                ]),
                option_axis("period", "Payment period", [
                    choice("6m", "6 months", premium_factor = "0.50", benefit_months = 6),  # PH
                    choice("12m", "12 months", premium_factor = "0.75", benefit_months = 12),  # PH
                    # Healthy Starter offers 6 or 12 months; Starter Life Plus
                    # also 24. The online journey offers each plan its own.
                    choice("24m", "24 months", premium_factor = "1.00", benefit_months = 24),
                ]),
            ],
            built_in = [
                built_in("partial-disability-bridging", "Partial Disability Bridging Benefit", "additional", multiple_of_cover = "0.3333"),
                built_in("recurrent-disablement", "Recurrent Disablement Benefit", "facility"),
            ],
        ),
        cover(
            code = "specialist-testing",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("specialist", per_year = dollars(10000)),
                benefit_limit("diagnostics", per_year = dollars(100000)),
            ]),
            rating = per_life("aia-specialist-testing"),
            definitions = ["aia-specialist-testing"],
            gst_rate = "0.15",
        ),
    ],
    features = [
        composition(core = ["life", "rent-mortgage"]),
        billing(frequencies = ["fortnightly", "monthly"], policy_fee_annual = "0"),
        earning(),
        arrears(grace_days = 30, lapse_days = 31, reinstatement_days = 90, reinstatement_evidence_free_days = 30),  # PH
        cooling_off(days = 20),
    ],
)
