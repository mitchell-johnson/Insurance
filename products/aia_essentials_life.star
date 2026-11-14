# AIA Essentials Life Cover: simple life cover bought online in about ten
# minutes with simplified questions and an instant decision. Cover of
# $50,000 to $1,000,000 for ages 16 to 60, and to $500,000 for ages 61 to 70
# (underwriting holds the age-banded limit). Indexed to CPI unless the owner
# keeps it level. Not eligible for the AIA Vitality discount.
#
# Built from AIA's product page as at November 2026 (docs/aia-products.md).
# Rates and factors are placeholders marked PH.

PRODUCT = product(
    code = "aia-essentials-life",
    name = "AIA Essentials Life Cover",
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
            min_entry_age = 16,
            max_entry_age = 70,
            min_sum_insured = dollars(50000),
            max_sum_insured = dollars(1000000),
            option_axes = [
                # Simplified underwriting is priced above fully underwritten cover.
                option_axis("basis", "Underwriting basis", [
                    choice("simplified", "Simplified questions", premium_factor = "1.15"),  # PH
                ]),
            ],
            insurability_option = insurability_option(
                events = ["marriage", "birth_or_adoption", "mortgage", "child_secondary_school"],
                max_percent_of_original = "50",
                max_increase = dollars(250000),
                max_uses = 2,
                within_days = 365,
                to_age = 55,
            ),
            built_in = [
                built_in("terminal-illness", "Terminal Illness Benefit", "early_payment", percent_of_cover = "100",
                         definitions = ["aia-terminal-illness"]),
                built_in("bereavement-support", "Bereavement Support Benefit", "early_payment", max = dollars(25000)),
                built_in("repatriation", "Repatriation Benefit", "early_payment", max = dollars(25000)),
                built_in("special-events-increase", "Special Events Increase Benefit", "facility"),
            ],
        ),
    ],
    features = [
        billing(
            frequencies = ["fortnightly", "monthly", "yearly"],
            policy_fee_annual = "0",  # PH
            modal_loading = {"fortnightly": "0.06", "monthly": "0.06"},  # PH
        ),
        earning(),
        arrears(grace_days = 30, lapse_days = 31, reinstatement_days = 90, reinstatement_evidence_free_days = 30),  # PH
        indexation(rate_code = "cpi"),
        cooling_off(days = 20),
    ],
)
