# AIA Living (business): the AIA Living benefits a business owns to protect
# itself against losing a key person or an owner: life, trauma and total
# permanent disablement, which can also be taken out personally, and the
# business-only income covers, Business Continuity and Rural Continuity.
# Business cover adds the optional Business Safeguard Facility: more cover
# without evidence of health as the person's value to the business grows.
#
# Built from AIA's published business and rural wordings and brochures as at
# November 2026 (docs/aia-products.md). Premium rates and factors are
# best-estimate placeholders marked PH.

# ---- Option menus shared by several benefits ---------------------------------

def premium_axis(level_to = [65, 80], level_years = [10]):
    """AIA Living premium types: rate for age, level for 10 years, to age 65 or to age 80."""
    choices = [choice("rate-for-age", "Rate for age", premium = stepped_premium())]
    for years in level_years:
        choices.append(choice("level-%dy" % years, "Level for %d years" % years, premium_factor = "1.18",  # PH
                              premium = level_term_premium(years)))
    factors = {65: "1.62", 80: "2.05"}  # PH
    for age in level_to:
        choices.append(choice("level-to-%d" % age, "Level to age %d" % age, premium_factor = factors[age],
                              premium = level_premium(age)))
    return option_axis("premium", "Premium type", choices)

def optional(code, name, factor, feature = None, only_with = []):
    """An optional benefit a life may add to a cover, at a premium factor."""
    return option_axis(code, name, [
        choice("no", "Not included"),
        choice("yes", "Included", premium_factor = factor, features = [feature or code], only_with = only_with),
    ])

def waiver_axis():
    """Optional Waiver of Premium: the benefit's premium is not collected while the life is totally disabled."""
    return optional("waiver", "Waiver of Premium", "1.06", feature = "waiver-of-premium")  # PH

def safeguard_axis():
    """Optional Business Safeguard Facility on business life, TPD, Critical Conditions and Severe Trauma."""
    return optional("safeguard", "Business Safeguard Facility", "1.03", feature = "business-safeguard")  # PH

SAFEGUARD = built_in("business-safeguard", "Business Safeguard Facility (more cover as the business value grows)", "facility",
                     requires = "business-safeguard")

# ---- Built-in benefits shared by several benefits ------------------------------

def common_built_ins():
    return [
        built_in("financial-legal-advice", "Financial and Legal Advice Benefit", "reimbursement", max = dollars(2500)),
        built_in("counselling", "Counselling Benefit", "reimbursement", max = dollars(2500)),
        built_in("suspension", "Suspension of Cover Benefit (up to 12 months)", "facility"),
        built_in("waiver-of-premium", "Waiver of Premium", "premium_waiver", requires = "waiver-of-premium"),
    ]

def trauma_built_ins():
    return common_built_ins() + [
        built_in("return-home", "Return Home Benefit", "reimbursement", max = dollars(10000)),
        built_in("medical-advancements", "Medical Advancements Provision", "facility"),
        built_in("standalone-conversion", "Standalone Conversion Facility", "facility"),
        built_in("premium-conversion", "Premium Conversion Facility (before age 65)", "facility"),
        built_in("childrens-trauma", "Children's Trauma Benefit", "additional", max = dollars(50000), percent_of_cover = "50"),
        built_in("childrens-trauma-conversion", "Children's Trauma Conversion Facility", "facility"),
        built_in("parents-grieving", "Parents Grieving Benefit", "additional", max = dollars(15000)),
        built_in("tpd-condition", "Optional Total Permanent Disablement condition (own occupation, before age 65)", "early_payment",
                 percent_of_cover = "100", requires = "tpd-condition", definitions = ["aia-tpd-own-occupation"]),
    ]

# AIA Living Business has no special events increases and no Future Insurability
# (1151, 1154, 1155, 1166 ALB wordings). Its Optional Business Safeguard Facility
# adds cover without evidence when the life's financial interest in the business, a
# loan guarantee or their value to the business grows: asked for within three
# months of the business event, not in the first six months of cover, at most two
# increases in any 12 months, and the cover after it at most the lower of five
# times the original cover and the cap (life $10,000,000, TPD $5,000,000, Critical
# Conditions and Severe Trauma $2,000,000). The wording sets no age limit.
BUSINESS_EVENTS = ["business_interest_increase", "loan_guarantee_increase", "value_to_business_increase"]

def business_safeguard(cap):
    return insurability_option(
        events = BUSINESS_EVENTS,
        max_percent_of_original = "400",
        max_increase = cap,
        max_uses = 99,
        within_days = 92,
        to_age = 120,
        max_total_percent_of_original = "400",
        once_in_days = 365,
        uses_in_window = 2,
        requires = "business-safeguard",
        not_before_days = 183,
        max_cover_after = cap,
    )

# ---- Lump sum benefits ---------------------------------------------------------

def trauma_cover(code, table, definitions, accelerated, max_sum_insured, ends_cover, extra_axes, extra_built_ins):
    return cover(
        code = code,
        cover_type = "trauma",
        benefit = lump_sum(survival_days = 14, ends_cover = ends_cover),
        rating = stepped(table),
        definitions = definitions,
        min_entry_age = 0,
        max_entry_age = 70,
        min_sum_insured = dollars(10000),  # PH: no published minimum
        max_sum_insured = max_sum_insured,
        # Medical Advancements Provision: a later version of the trauma definitions
        # applies to a claim where it is more favourable.
        pass_back_definitions = True,
        reduces = ["life"] if accelerated else [],
        option_axes = [premium_axis(), waiver_axis()] + extra_axes,
        # Accelerated trauma is already accelerated: only stand-alone trauma converts to it.
        built_in = [b for b in trauma_built_ins() if not (accelerated and b["code"] == "standalone-conversion")] + extra_built_ins,
    )

# Buy-back (1105 AL-CC sections 18 and 19, 1119 AL-ST; the business wordings the same):
# once, without evidence, within 60 days after the life survives six months (for the
# conditions the wording names) or twelve months from the claim, at most the lower of
# what the claim took and $2,000,000. Life Cover Buy-back writes the life cover an
# accelerated claim took off again; trauma buy-back writes the trauma cover again,
# only after a claim took all of it.
def trauma_buybacks(code, accelerated):
    backs = [buyback(code, "trauma-buyback", [183, 365], 60, max = dollars(2000000), only_after_nil = True)]
    if accelerated:
        backs.append(buyback("life", "life-buyback", [183, 365], 60, max = dollars(2000000)))
    return backs

def with_buybacks(c, accelerated):
    c["buybacks"] = trauma_buybacks(c["code"], accelerated)
    return c

def critical_conditions(code, accelerated):
    axes = [
        optional("early-cancer-upgrade", "Early Cancer Upgrade", "1.04"),  # PH
        optional("tpd-condition", "Total Permanent Disablement condition", "1.12"),  # PH
        optional("children-maternity", "Children's and Maternity Benefit", "1.03"),  # PH
        optional("buyback", "Critical Conditions Buy-back", "1.07", feature = "trauma-buyback"),  # PH
    ]
    if accelerated:
        axes.append(optional("life-buyback", "Life Cover Buy-back", "1.05"))  # PH
    extra = [
        built_in("newborn-children", "Newborn Children's Benefit", "additional", max = dollars(50000), percent_of_cover = "50"),
        built_in("partial-payment", "Partial payment conditions", "early_payment", max = dollars(50000), percent_of_cover = "10"),
        built_in("early-cancer-upgrade", "Early Cancer Upgrade Benefit", "additional", max = dollars(50000), percent_of_cover = "10",
                 requires = "early-cancer-upgrade"),
        built_in("children-maternity", "Children's and Maternity Benefit", "additional", max = dollars(75000), percent_of_cover = "50",
                 requires = "children-maternity"),
        built_in("trauma-buyback", "Critical Conditions Buy-back Benefit", "facility", requires = "trauma-buyback"),
    ]
    if accelerated:
        extra.append(built_in("life-buyback", "Life Cover Buy-back Benefit", "facility", requires = "life-buyback"))
    return with_buybacks(trauma_cover(code, "aia-trauma", ["aia-critical-conditions"], accelerated, dollars(2000000), True, axes, extra), accelerated)

def progressive_care(code, accelerated):
    axes = [
        optional("tpd-condition", "Total Permanent Disablement condition", "1.12"),  # PH
        optional("children-maternity", "Children's and Maternity Benefit", "1.03"),  # PH
    ]
    extra = [
        built_in("newborn-children", "Newborn Children's Benefit", "additional", max = dollars(50000), percent_of_cover = "50"),
        built_in("children-maternity", "Children's and Maternity Benefit", "additional", max = dollars(75000), percent_of_cover = "50",
                 requires = "children-maternity"),
    ]
    # Progressive Care pays by severity, more than once, across five
    # categories; a payment reduces the cover rather than ending it.
    return trauma_cover(code, "aia-progressive-care", ["aia-progressive-care"], accelerated, dollars(1500000), False, axes, extra)

def severe_trauma(code, accelerated):
    axes = [
        optional("tpd-condition", "Total Permanent Disablement condition", "1.12"),  # PH
        optional("buyback", "Severe Trauma Buy-back", "1.07", feature = "trauma-buyback"),  # PH
    ]
    if accelerated:
        axes.append(optional("life-buyback", "Life Cover Buy-back", "1.05"))  # PH
    extra = [built_in("trauma-buyback", "Severe Trauma Buy-back Benefit", "facility", requires = "trauma-buyback")]
    if accelerated:
        extra.append(built_in("life-buyback", "Life Cover Buy-back Benefit", "facility", requires = "life-buyback"))
    # Severe Trauma has no partial payments.
    return with_buybacks(trauma_cover(code, "aia-severe-trauma", ["aia-severe-trauma"], accelerated, dollars(2000000), True, axes, extra), accelerated)

def tpd(code, own_occupation, accelerated):
    return cover(
        code = code,
        cover_type = "tpd",
        # Disabled for three months and expected never to return to work;
        # the life must survive the three months.
        benefit = lump_sum(survival_days = 90),
        rating = stepped("aia-tpd"),
        definitions = ["aia-tpd-own-occupation" if own_occupation else "aia-tpd-any-occupation"],
        min_entry_age = 16,
        # 55 for most occupations, 60 for clerical, managerial and
        # professional work: underwriting holds the occupation test.
        max_entry_age = 60,
        expiry_age = 65,
        min_sum_insured = dollars(10000),  # PH: no published minimum
        max_sum_insured = dollars(5000000),
        reduces = ["life"] if accelerated else [],
        option_axes = [
            premium_axis(level_to = [65]),
            waiver_axis(),
            option_axis("expiry", "Expiry age", [
                choice("to-65", "To age 65", expiry_age = 65),
                choice("to-70", "To age 70", premium_factor = "1.09", expiry_age = 70),  # PH
            ]),
        ],
        built_in = common_built_ins() + [
            built_in("partial-permanent-disablement", "Partial Permanent Disablement Benefit", "early_payment",
                     max = dollars(100000), percent_of_cover = "25"),
            built_in("assistance", "Assistance Benefit (home, vehicle, equipment)", "reimbursement", max = dollars(15000)),
            built_in("waiver-of-waiting-period", "Waiver of the three-month waiting period", "facility"),
            built_in("premium-conversion", "Premium Conversion Facility (before age 65)", "facility"),
        ],
    )


# ---- Business and rural income ------------------------------------------------------

def business_income_axes(weeks, months):
    return [
        option_axis("waiting", "Waiting period", [
            choice("%dw" % w, "%d weeks" % w, premium_factor = {4: "1.00", 8: "0.78", 13: "0.62"}[w], waiting_days = w * 7)  # PH
            for w in weeks
        ]),
        option_axis("period", "Benefit period", [
            choice("%dm" % m, "%d months" % m, premium_factor = {6: "0.62", 12: "1.00", 24: "1.45"}[m], benefit_months = m)  # PH
            for m in months
        ]),
        waiver_axis(),
    ]

def business_income_cover(code, definitions, axes, built_ins):
    return cover(
        code = code,
        cover_type = "income_protection",
        benefit = monthly_benefit(waiting_days = 28, benefit_months = 12),
        rating = stepped("aia-business-continuity"),
        definitions = definitions,
        min_entry_age = 16,
        max_entry_age = 60,  # PH
        expiry_age = 65,
        min_sum_insured = dollars(500),  # PH: monthly benefit
        max_sum_insured = dollars(30000),  # PH
        option_axes = axes,
        built_in = [
            built_in("recurrent-disablement", "Recurrent Disablement Benefit", "facility"),
            built_in("rehabilitation-support", "Rehabilitation and Support", "facility"),
            built_in("waiver-of-premium", "Waiver of Premium", "premium_waiver", requires = "waiver-of-premium"),
        ] + built_ins,
    )

PARTIAL = [
    built_in("partial-disablement", "Partial Disablement Benefit", "facility", requires = "partial-disablement"),
    built_in("partial-disablement-bridging", "Partial Disablement Bridging Benefit", "additional", multiple_of_cover = "0.3333",
             requires = "partial-disablement"),
]

TRAUMA = ["critical-conditions", "critical-conditions-accelerated", "progressive-care", "progressive-care-accelerated",
          "severe-trauma", "severe-trauma-accelerated"]
TPD = ["tpd-own-occupation", "tpd-own-occupation-accelerated", "tpd-any-occupation", "tpd-any-occupation-accelerated"]
INCOME = ["business-continuity-agreed-value", "business-continuity-indemnity", "rural-continuity"]

# Cover the same life holds on its other AIA Living, Cover for Life and Start-Up
# Income Protection policies counts towards the minimums too; only this
# policy's covers are discounted here.
PERSONAL_INCOME = ["income-agreed-value", "income-indemnity", "income-loss-of-earnings", "mortgage-income-rent"]

def on(pairs):
    return ["%s/%s" % (product, cover) for (product, covers) in pairs for cover in covers]

def MBD(count):
    return benefit_categories(
        required = [discount_category("life", ["life"], min_sum_insured = dollars(100000),
                                      elsewhere = on([("aia-living", ["life", "accidental-death"]), ("aia-living-business", ["life"]),
                                                      ("aia-cover-for-life", ["life"])]))],
        categories = [
            discount_category("trauma", TRAUMA, min_sum_insured = dollars(75000),
                              elsewhere = on([("aia-living", TRAUMA), ("aia-living-business", TRAUMA)])),
            discount_category("tpd", TPD, min_sum_insured = dollars(75000),
                              elsewhere = on([("aia-living", TPD), ("aia-living-business", TPD)])),
            discount_category("income", INCOME, min_sum_insured = dollars(2000),
                              elsewhere = on([("aia-living", PERSONAL_INCOME), ("aia-living-business", INCOME),
                                              ("aia-start-up-income", ["start-up-income"])])),
        ],
        count = count,
    )

def with_safeguard(c, cap):
    c["option_axes"] = c["option_axes"] + [safeguard_axis()]
    c["insurability_option"] = business_safeguard(cap)
    c["built_in"] = c["built_in"] + [SAFEGUARD]
    return c

# ---- The published wordings each cover is bound to ---------------------------------

# Each cover names the AIA wordings that govern it (products/definitions/
# aia_wordings.star, the text as published), beside the summaries above. A
# cover missing from this map fails to load.
WORDINGS = {
    "life": ["aia-wording-1100-al-umb", "aia-wording-1151-alb-life", "aia-wording-1164-alb-wop"],
    "critical-conditions": ["aia-wording-1100-al-umb", "aia-wording-1155-alb-cc", "aia-wording-1164-alb-wop"],
    "critical-conditions-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1155-alb-cc", "aia-wording-1164-alb-wop"],
    "progressive-care": ["aia-wording-1100-al-umb", "aia-wording-1156-alb-pc", "aia-wording-1164-alb-wop"],
    "progressive-care-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1156-alb-pc", "aia-wording-1164-alb-wop"],
    "severe-trauma": ["aia-wording-1100-al-umb", "aia-wording-1166-alb-st", "aia-wording-1164-alb-wop"],
    "severe-trauma-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1166-alb-st", "aia-wording-1164-alb-wop"],
    "tpd-own-occupation": ["aia-wording-1100-al-umb", "aia-wording-1154-alb-tpd", "aia-wording-1164-alb-wop"],
    "tpd-own-occupation-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1154-alb-tpd", "aia-wording-1164-alb-wop"],
    "tpd-any-occupation": ["aia-wording-1100-al-umb", "aia-wording-1154-alb-tpd", "aia-wording-1164-alb-wop"],
    "tpd-any-occupation-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1154-alb-tpd", "aia-wording-1164-alb-wop"],
    "business-continuity-agreed-value": ["aia-wording-1100-al-umb", "aia-wording-1158-alb-bc", "aia-wording-1164-alb-wop"],
    "business-continuity-indemnity": ["aia-wording-1100-al-umb", "aia-wording-1158-alb-bc", "aia-wording-1164-alb-wop"],
    "rural-continuity": ["aia-wording-1100-al-umb", "aia-wording-1159-alb-rc", "aia-wording-1160-alb-bi", "aia-wording-1164-alb-wop"],
    "specified-accidental-injury": ["aia-wording-1100-al-umb", "aia-wording-1165-alb-saic"],
}

# ---- Conversions without evidence ---------------------------------------------------

# Premium Conversion Facility (before age 65): all or part of a benefit moved from
# rate for age to a level premium, or from a level premium to one level for a
# shorter time, without evidence. Standalone Conversion Facility: stand-alone
# trauma moved to the same trauma accelerated against life cover, with as much
# life cover, without evidence, before age 60 and only where the cover was
# accepted on standard terms.
LEVEL_CHOICES = ["level-to-80", "level-to-65", "level-10y"]

def with_conversions(c):
    codes = [b["code"] for b in c["built_in"]]
    convs = []
    if "premium-conversion" in codes:
        axis = [a for a in c["option_axes"] if a["code"] == "premium"][0]
        offered = [ch["code"] for ch in axis["choices"]]
        convs.append(conversion(c["code"], 65, axis = "premium", choices = [ch for ch in LEVEL_CHOICES if ch in offered]))
    if "standalone-conversion" in codes:
        convs.append(conversion(c["code"] + "-accelerated", 60, standard_terms_only = True, with_cover = "life"))
    return dict(c, conversions = convs)

def with_wordings(covers):
    return [with_conversions(dict(c, definitions = c["definitions"] + WORDINGS[c["code"]])) for c in covers]

PRODUCT = product(
    code = "aia-living-business",
    name = "AIA Living Business",
    version = 1,
    effective_from = "2026-11-14",
    line = "life",
    currency = "NZD",
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = with_wordings([
        with_safeguard(cover(
            code = "life",
            cover_type = "life",
            benefit = lump_sum(),
            rating = stepped("aia-life"),
            definitions = ["aia-death", "aia-terminal-illness"],
            min_entry_age = 10,
            max_entry_age = 70,
            min_sum_insured = dollars(10000),  # PH
                option_axes = [premium_axis(), waiver_axis()],
            built_in = common_built_ins() + [
                built_in("terminal-illness", "Terminal Illness Benefit", "early_payment", percent_of_cover = "100",
                         definitions = ["aia-terminal-illness"]),
                built_in("premium-conversion", "Premium Conversion Facility (before age 65)", "facility"),
            ],
        ), dollars(10000000)),
        with_safeguard(critical_conditions("critical-conditions", False), dollars(2000000)),
        with_safeguard(critical_conditions("critical-conditions-accelerated", True), dollars(2000000)),
        progressive_care("progressive-care", False),
        progressive_care("progressive-care-accelerated", True),
        with_safeguard(severe_trauma("severe-trauma", False), dollars(2000000)),
        with_safeguard(severe_trauma("severe-trauma-accelerated", True), dollars(2000000)),
        with_safeguard(tpd("tpd-own-occupation", True, False), dollars(5000000)),
        with_safeguard(tpd("tpd-own-occupation-accelerated", True, True), dollars(5000000)),
        with_safeguard(tpd("tpd-any-occupation", False, False), dollars(5000000)),
        with_safeguard(tpd("tpd-any-occupation-accelerated", False, True), dollars(5000000)),
        # For businesses trading more than three years; paid on top of ACC.
        business_income_cover("business-continuity-agreed-value", ["aia-business-continuity", "aia-ip-agreed-value"],
                              business_income_axes([4, 8, 13], [6, 12, 24]) + [
                                  optional("partial-disablement", "Partial Disablement Benefit", "1.12"),  # PH
                              ],
                              PARTIAL + [built_in("business-mentors", "Business Mentors Benefit", "facility")]),
        business_income_cover("business-continuity-indemnity", ["aia-business-continuity", "aia-ip-indemnity"],
                              business_income_axes([4, 8, 13], [6, 12, 24]) + [
                                  optional("partial-disablement", "Partial Disablement Benefit", "1.12"),  # PH
                              ],
                              PARTIAL + [built_in("business-mentors", "Business Mentors Benefit", "facility")]),
        business_income_cover("rural-continuity", ["aia-rural-continuity"],
                              business_income_axes([4, 8, 13], [6, 12, 24]) + [
                                  option_axis("acc", "ACC offsets", [
                                      choice("no-offset", "Paid on top of ACC"),
                                      choice("offset", "ACC payments offset", premium_factor = "0.80", features = ["acc-offset"]),  # PH
                                  ]),
                                  optional("peak-season", "Peak Season Upgrade Benefit", "1.08"),  # PH
                                  optional("business-income-support", "Business Income Support upgrade", "1.15",  # PH
                                           feature = "partial-disablement"),
                              ],
                              PARTIAL + [
                                  built_in("peak-season", "Peak Season Upgrade Benefit (25% more for peak months)", "monthly_addition",
                                           percent_of_cover = "25", requires = "peak-season"),
                                  built_in("vocational-assistance", "Vocational Assistance", "reimbursement", multiple_of_cover = "12",
                                           requires = "partial-disablement"),
                                  built_in("home-modification", "Home Modification and Equipment Expenses Benefit", "reimbursement",
                                           multiple_of_cover = "12", requires = "partial-disablement"),
                              ]),
        cover(
            code = "specified-accidental-injury",
            cover_type = "accidental_injury",
            benefit = lump_sum(ends_cover = False),
            rating = stepped("aia-accidental-injury"),
            definitions = ["aia-specified-accidental-injury"],
            min_entry_age = 16,
            max_entry_age = 60,  # PH
            expiry_age = 65,  # PH
            min_sum_insured = dollars(500),  # PH
            max_sum_insured = dollars(5000),
            built_in = [
                built_in("category-a", "Category A injuries", "additional", multiple_of_cover = "1"),
                built_in("category-b", "Category B injuries", "additional", multiple_of_cover = "2"),
                built_in("category-c", "Category C injuries", "additional", multiple_of_cover = "3"),
                built_in("category-d", "Category D injuries", "additional", multiple_of_cover = "6"),
                built_in("category-e", "Category E injuries", "additional", multiple_of_cover = "12"),
            ],
        ),
    ]),
    features = [
        composition(
            core = ["life"] + TRAUMA + TPD + INCOME,
            exclusive = [
                ["critical-conditions", "critical-conditions-accelerated"],
                ["progressive-care", "progressive-care-accelerated"],
                ["severe-trauma", "severe-trauma-accelerated"],
                ["business-continuity-agreed-value", "business-continuity-indemnity"],
            ],
            requires = {
                "critical-conditions-accelerated": ["life"],
                "progressive-care-accelerated": ["life"],
                "severe-trauma-accelerated": ["life"],
                "tpd-own-occupation-accelerated": ["life"],
                "tpd-any-occupation-accelerated": ["life"],
            },
        ),
        billing(
            frequencies = ["fortnightly", "monthly", "quarterly", "half_yearly", "yearly"],
            policy_fee_annual = "75.00",  # PH
            modal_loading = {"fortnightly": "0.06", "monthly": "0.06", "quarterly": "0.04", "half_yearly": "0.02"},  # PH
        ),
        earning(),
        arrears(grace_days = 30, lapse_days = 31, reinstatement_days = 180, reinstatement_evidence_free_days = 60),  # PH
        indexation(rate_code = "cpi"),
        discounts(chain = [
            best_of([
                discount("mbd-15", "Multi-Benefit Discount", "multi_benefit_discount", percent_off("15"), condition = MBD(3)),
                discount("mbd-12-5", "Multi-Benefit Discount", "multi_benefit_discount", percent_off("12.5"), condition = MBD(2)),
                discount("mbd-10", "Multi-Benefit Discount", "multi_benefit_discount", percent_off("10"), condition = MBD(1)),
            ]),
            discount("vitality", "AIA Vitality discount", "vitality_discount",
                     by_tier({"%d" % p: "%d" % p for p in range(0, 21)}), condition = entitled("aia-vitality")),
        ]),
        cooling_off(days = 20),
    ],
)
