# AIA Cover for Life: whole-of-life cover with guaranteed level premiums paid
# only until an age chosen at the start; the cover then carries on for life
# with nothing more to pay. Launched 20 March 2026 (wording 1400 A-CFL v1).
#
# Not eligible for the AIA Vitality discount, and counts towards the
# Multi-Benefit Discount without receiving it, so it declares no discounts.
# After half the premium term the owner may stop paying and keep a reduced
# sum assured (paid up): the proportion of the term paid, times the sum
# assured, times 0.95. The core has no paid-up alteration yet; it is listed
# in docs/aia-products.md.
#
# Built from AIA's published wording and product page as at November 2026.
# Rates and factors are placeholders marked PH.

PAY_TO = {65: "2.10", 70: "1.85", 75: "1.70", 80: "1.60", 85: "1.55"}  # PH: factor on the entry-age rate

# ---- The published wordings each cover is bound to ---------------------------------

# Each cover names the AIA wordings that govern it (products/definitions/
# aia_wordings.star, the text as published), beside the summaries above. A
# cover missing from this map fails to load.
WORDINGS = {
    "life": ["aia-wording-1400-a-cfl"],
}

def with_wordings(covers):
    return [dict(c, definitions = c["definitions"] + WORDINGS[c["code"]]) for c in covers]

PRODUCT = product(
    code = "aia-cover-for-life",
    name = "AIA Cover for Life",
    version = 1,
    effective_from = "2026-11-14",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = with_wordings([
        cover(
            code = "life",
            cover_type = "life",
            benefit = lump_sum(),
            rating = stepped("aia-life"),
            definitions = ["aia-death", "aia-terminal-illness"],
            min_entry_age = 16,
            max_entry_age = 70,
            # No minimum and no maximum cover amount.
            min_sum_insured = 1,
            option_axes = [
                # The premium payment term ends at the anniversary after the
                # life reaches the age chosen. It cannot be changed later.
                option_axis("pay-to", "Premiums paid to age", [
                    choice("to-%d" % age, "To age %d" % age, premium_factor = PAY_TO[age], premium = limited_pay_premium(age))
                    for age in sorted(PAY_TO.keys())
                ]),
            ],
            insurability_option = insurability_option(
                events = ["marriage", "birth_or_adoption", "separation", "death_of_partner", "child_secondary_school",
                          "child_tertiary", "mortgage", "salary_increase"],
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
                built_in("guaranteed-premiums", "Guaranteed level premiums for the payment term", "facility"),
                built_in("paid-up", "Paid-up option after half the premium term (reduced cover, no more premiums)", "facility"),
                built_in("special-events-increase", "Special Events Increase Facility", "facility"),
                built_in("non-smoker-review", "Smoker to non-smoker rates after 12 months without smoking", "facility"),
            ],
        ),
    ]),
    features = [
        billing(
            frequencies = ["fortnightly", "monthly", "yearly"],
            # No fee: premiums stop at the end of the term.
            policy_fee_annual = "0",
            modal_loading = {"fortnightly": "0.06", "monthly": "0.06"},  # PH
        ),
        earning(),
        # A premium 31 days overdue: before the paid-up date AIA may cancel;
        # after it the policy is made paid up (not built: see the header).
        arrears(grace_days = 30, lapse_days = 31, reinstatement_days = 180, reinstatement_evidence_free_days = 60),  # PH
        cooling_off(days = 20),
    ],
)
