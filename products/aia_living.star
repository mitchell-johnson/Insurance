# AIA Living (personal): AIA New Zealand's modular retail range, sold through
# advisers. One policy holds any of the benefits below for each life assured,
# each under its own appendix to the AIA Living umbrella wording.
#
# Built from AIA's published wordings and product pages as at November 2026
# (docs/aia-products.md lists each source and version). Benefit structures,
# entry ages, limits, option menus and built-in benefits follow the published
# material. Premium rates and every premium factor below are best-estimate
# placeholders: AIA does not publish its rates. Each one is marked `PH` and
# listed in docs/aia-products.md; the appointed actuary replaces them.
#
# Wording is not copied: definitions name the AIA appendix they follow and
# describe the mechanics in this system's own words.

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
        built_in("future-insurability", "Future Insurability Benefit (each third anniversary, the lower of 20% and $200,000)", "facility"),
        built_in("special-events-increase", "Special Events Increase Facility", "facility"),
        built_in("childrens-trauma", "Children's Trauma Benefit", "additional", max = dollars(50000), percent_of_cover = "50"),
        built_in("childrens-trauma-conversion", "Children's Trauma Conversion Facility", "facility"),
        built_in("parents-grieving", "Parents Grieving Benefit", "additional", max = dollars(15000)),
        built_in("tpd-condition", "Optional Total Permanent Disablement condition (own occupation, before age 65)", "early_payment",
                 percent_of_cover = "100", requires = "tpd-condition", definitions = ["aia-tpd-own-occupation"]),
    ]

# The special events that allow more cover without evidence of health, and the
# limits AIA Living sets on each increase: half the original cover or $250,000,
# whichever is less, within 12 months of the event, up to age 55. All increases
# together are at most the original cover or $750,000, whichever is less, and
# only one increase without evidence (this or Life Future Insurability) is made
# in any 12 months. The wording sets no count of uses: the money and the 12
# months bound them.
SPECIAL_EVENTS = ["marriage", "birth_or_adoption", "separation", "death_of_partner", "child_secondary_school",
                  "child_tertiary", "mortgage", "close_relative_care", "salary_increase"]

def special_events():
    return insurability_option(
        events = SPECIAL_EVENTS,
        max_percent_of_original = "50",
        max_increase = dollars(250000),
        max_uses = 99,
        within_days = 365,
        to_age = 55,
        max_total_percent_of_original = "100",
        max_total_increase = dollars(750000),
        once_in_days = 365,
    )

# Trauma and TPD (1104 AL-TPD, 1105 AL-CC, 1106 AL-PC, 1119 AL-ST): special events
# increases of the lower of half the original cover and $250,000, the lower of the
# original cover and $750,000 in all, asked for within 60 days either side of the
# event, for events from 16 (TPD: any age) and before 55; and Future Insurability
# at each third policy anniversary, the lower of 20% and $200,000, the lower of the
# original cover and $500,000 in all, within 60 days either side. Only one of these
# increases is made for a life's trauma covers (or TPD covers) in any 12 months.
def lump_sum_special_events(from_age):
    return insurability_option(
        events = SPECIAL_EVENTS,
        max_percent_of_original = "50",
        max_increase = dollars(250000),
        max_uses = 99,
        within_days = 60,
        days_before = 60,
        from_age = from_age,
        to_age = 55,
        max_total_percent_of_original = "100",
        max_total_increase = dollars(750000),
        once_in_days = 365,
    )

def lump_sum_future_insurability():
    return anniversary_option(
        every_years = 3,
        max_percent_of_original = "20",
        at_least = dollars(200000),
        lesser = True,
        max_total_percent_of_original = "100",
        max_total_increase = dollars(500000),
        within_days = 60,
        days_before = 60,
        from_age = 16,
        to_age = 55,
        once_in_days = 365,
    )

# The Special Events TPD/Trauma Facility on Life Cover (1101 AL-LIFE section 7): once
# for a life, on a special event, add an accelerated Critical Conditions, accelerated
# Progressive Care or accelerated any occupation TPD to age 65, the lower of half the
# original life cover and $100,000, without evidence, within 60 days either side.
def special_events_tpd_trauma():
    return adds_cover_option(
        events = SPECIAL_EVENTS,
        covers = {
            "critical-conditions-accelerated": {},
            "progressive-care-accelerated": {},
            "tpd-any-occupation-accelerated": {"expiry": "to-65"},
        },
        max_percent_of_original = "50",
        max_increase = dollars(100000),
        max_uses = 1,
        within_days = 60,
        days_before = 60,
        to_age = 55,
        once_in_days = 365,
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
        insurability_option = lump_sum_special_events(16),
        anniversary_option = lump_sum_future_insurability(),
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
        insurability_option = lump_sum_special_events(None),
        anniversary_option = lump_sum_future_insurability(),
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
            built_in("future-insurability", "Future Insurability Benefit (each third anniversary)", "facility"),
            built_in("special-events-increase", "Special Events Increase Facility", "facility"),
        ],
    )

# ---- Income benefits -----------------------------------------------------------

WAITING_FACTORS = {2: "1.38", 4: "1.00", 8: "0.74", 13: "0.57", 26: "0.43", 52: "0.34", 104: "0.29"}  # PH
PERIOD_FACTORS = {"1y": "0.46", "2y": "0.61", "5y": "0.79", "to-65": "1.00", "to-70": "1.12"}  # PH

def waiting_axis(weeks):
    return option_axis("waiting", "Waiting period", [
        choice("%dw" % w, "%d weeks" % w, premium_factor = WAITING_FACTORS[w], waiting_days = w * 7)
        for w in weeks
    ])

def period_axis(periods):
    choices = []
    for p in periods:
        if p == "to-65":
            choices.append(choice(p, "To age 65", premium_factor = PERIOD_FACTORS[p], benefit_to_age = 65, expiry_age = 65))
        elif p == "to-70":
            # Only for occupations AIA accepts to 70: underwriting checks it.
            choices.append(choice(p, "To age 70", premium_factor = PERIOD_FACTORS[p], benefit_to_age = 70, expiry_age = 70))
        else:
            years = int(p[:-1])
            choices.append(choice(p, "%d year%s" % (years, "" if years == 1 else "s"), premium_factor = PERIOD_FACTORS[p],
                                  benefit_months = years * 12, expiry_age = 65))
    return option_axis("period", "Benefit period", choices)

def income_axes(weeks, periods, premier):
    axes = [
        waiting_axis(weeks),
        period_axis(periods),
        premium_axis(level_to = [65], level_years = []),
    ]
    if premier:
        axes.append(option_axis("cover", "Cover option", [
            choice("base", "Base Cover"),
            choice("premier", "Premier Cover", premium_factor = "1.11", features = ["premier"]),  # PH
        ]))
    axes += [
        # Only with a benefit period to age 65 or 70.
        option_axis("mental-health", "Mental Health Limitation", [
            choice("full", "Not limited"),
            choice("limited", "Mental health claims limited to two years", premium_factor = "0.90",  # PH
                   features = ["mental-health-limit"], only_with = ["period=to-65", "period=to-70"]),
        ]),
        # The Future Insurability Benefit is built in unless the Claim
        # Indexation Benefit is chosen.
        option_axis("claim-indexation", "Claim Indexation Benefit", [
            choice("no", "Not included", features = ["future-insurability"]),
            choice("cpi", "Claim payments indexed to CPI", premium_factor = "1.10", features = ["claim-indexation"]),  # PH
        ]),
    ]
    return axes

def income_built_ins(premier):
    out = [
        built_in("partial-disability-bridging", "Partial Disability Bridging Benefit", "additional", multiple_of_cover = "0.3333"),
        built_in("recurrent-disablement", "Recurrent Disablement Benefit (waiting period waived within 12 months)", "facility"),
        built_in("rehabilitation-support", "Rehabilitation and Support", "facility"),
        built_in("vocational-assistance", "Vocational Assistance", "reimbursement", multiple_of_cover = "12"),
        built_in("home-modification", "Home Modification and Equipment Expenses Benefit", "reimbursement", multiple_of_cover = "12"),
        built_in("back-to-work", "Back to Work Payment", "additional", multiple_of_cover = "3"),
        built_in("return-home", "Return Home Benefit", "reimbursement", max = dollars(10000)),
        built_in("pregnancy-premium-waiver", "Pregnancy Premium Waiver (six months)", "premium_waiver"),
        built_in("waiting-period-reduction", "Waiting Period Reduction Benefit", "facility"),
        built_in("concurrent-waiting-period", "Concurrent Waiting Period Benefit", "facility"),
        built_in("suspension", "Suspension of Cover Benefit (up to 12 months)", "facility"),
        built_in("future-insurability", "Future Insurability Benefit", "facility", requires = "future-insurability"),
        built_in("claim-indexation", "Claim Indexation Benefit", "facility", requires = "claim-indexation"),
        built_in("mental-health-limit", "Mental health claims paid for two years in all", "facility", requires = "mental-health-limit"),
        built_in("redundancy", "Redundancy Cover (up to six months)", "additional", multiple_of_cover = "6", requires = "redundancy"),
    ]
    if premier:
        out += [
            built_in("extra-cash", "Extra Cash Benefit (a third of the benefit for three months)", "additional",
                     multiple_of_cover = "1", requires = "premier"),
            built_in("bereavement-grant", "Bereavement Grant", "additional", multiple_of_cover = "3", requires = "premier"),
            built_in("permanent-disablement", "Permanent Disablement Benefit", "facility", requires = "premier"),
        ]
    return out

# Income Future Insurability (1108 AL-IP, 1109 AL-LOE, 1115 AL-IND section 17), built in
# unless Claim Indexation is chosen: at each third anniversary (60 days either side),
# or within 60 days of a rise in income (once a year), the monthly benefit grows by the
# lower of 10% of the original benefit and $1,000 a month ($12,000 a year), 100% of the
# original in all across both, never to more than $20,000 a month ($240,000 a year), and
# not from age 56. The agreed replacement ratio is checked on the income evidence the
# request carries.
def income_future_insurability():
    return anniversary_option(
        every_years = 3,
        max_percent_of_original = "10",
        at_least = dollars(1000),
        lesser = True,
        max_total_percent_of_original = "100",
        max_total_increase = dollars(20000),
        within_days = 60,
        days_before = 60,
        to_age = 56,
        requires = "future-insurability",
        max_cover_after = dollars(20000),
        shared_total = True,
    )

def income_increase():
    return insurability_option(
        events = ["salary_increase"],
        max_percent_of_original = "10",
        max_increase = dollars(1000),
        max_uses = 99,
        within_days = 60,
        to_age = 56,
        max_total_percent_of_original = "100",
        once_in_days = 365,
        requires = "future-insurability",
        max_cover_after = dollars(20000),
    )

# Property Special Event Facility (1117 AL-MIRC section 12): within six months of taking
# out or increasing a mortgage for a home, holiday home or residential land, the benefit
# grows by the lower of 115% of the rise in the mortgage repayment and $750 a month, once
# every three years, not from age 56, never to more than $20,000 a month. The 115% is
# checked on the lender's evidence the request carries; the $750 and the rest here.
def property_special_event():
    return insurability_option(
        events = ["mortgage"],
        max_percent_of_original = "1000",
        max_increase = dollars(750),
        max_uses = 99,
        within_days = 183,
        to_age = 56,
        once_in_days = 1095,
        max_cover_after = dollars(20000),
    )

# Waiting Period Reduction (1108 AL-IP, 1109, 1115, 1117 section 22): when the life's
# Business Continuity, Rural Continuity, Business Overheads or Locum cover is cancelled
# because the business was sold, stopped trading or the life left it, a 26, 52 or
# 104 week waiting period drops to the cancelled cover's, without evidence, on the
# lesser of the two monthly benefits. Underwriting checks the cancellation and the
# amount; the request names the evidence.
def waiting_period_reduction(code, weeks):
    longest_first = ["%dw" % w for w in sorted(weeks, reverse = True)]
    return conversion(code, 65, axis = "waiting", choices = longest_first,
                      from_choices = [w for w in ["26w", "52w", "104w"] if w in longest_first], requires_evidence = True)

def income_cover(code, definitions, premier, weeks, periods, max_monthly):
    mirc = code == "mortgage-income-rent"
    built_ins = income_built_ins(premier)
    if mirc:
        # Mortgage, Income or Rent Cover has the Property Special Event Facility in place
        # of Future Insurability.
        built_ins = [b for b in built_ins if b["code"] != "future-insurability"] + [
            built_in("property-special-event", "Property Special Event Facility (the lower of 115% of the repayment rise and $750 a month)", "facility"),
        ]
    return cover(
        code = code,
        cover_type = "income_protection",
        # The option chosen sets the waiting period and how long a claim is
        # paid; these are what the menu falls back on and are never used alone.
        benefit = monthly_benefit(waiting_days = 28, benefit_months = 24, benefit_while_imprisoned = "suspend"),
        rating = stepped("aia-income"),
        premium_waiver = True,
        definitions = definitions,
        min_entry_age = 16,
        max_entry_age = 60,
        expiry_age = 65,
        min_sum_insured = dollars(500),  # PH: monthly benefit
        max_sum_insured = max_monthly,
        insurability_option = property_special_event() if mirc else income_increase(),
        anniversary_option = None if mirc else income_future_insurability(),
        conversions = [waiting_period_reduction(code, weeks)],
        option_axes = income_axes(weeks, periods, premier) + [optional("redundancy", "Redundancy Cover", "1.04")],  # PH
        built_in = built_ins,
    )

IP_WEEKS = [2, 4, 8, 13, 26, 52, 104]
IP_PERIODS = ["1y", "2y", "5y", "to-65", "to-70"]
MIRC_WEEKS = [4, 8, 13, 26, 52, 104]
MIRC_PERIODS = ["2y", "5y", "to-65", "to-70"]

# ---- Multi-Benefit Discount ------------------------------------------------------

# Life insurance is the compulsory category; trauma, total permanent
# disablement and income protection count once each, from the minimum cover
# amounts AIA publishes. Family Protection counts as life insurance on its
# monthly amount: AIA converts it to a lump sum over the term, which this
# system does not, so the minimum here is a best estimate (PH).
#
# Cover the same life holds on its other AIA Living, AIA Living for business,
# Cover for Life and Start-Up Income Protection policies counts towards the
# minimums too (AIA's Multi-Benefit Discount terms, the table of benefits that
# count), though only this policy's covers are discounted here.
BUSINESS_INCOME = ["business-continuity-agreed-value", "business-continuity-indemnity", "rural-continuity"]

def on(pairs):
    return ["%s/%s" % (product, cover) for (product, covers) in pairs for cover in covers]

def MBD(count):
    return benefit_categories(
        required = [
            discount_category("life", ["life", "accidental-death"], min_sum_insured = dollars(100000),
                              elsewhere = on([("aia-living", ["life", "accidental-death"]), ("aia-living-business", ["life"]),
                                              ("aia-cover-for-life", ["life"])])),
            discount_category("family-protection", ["family-protection"], min_sum_insured = dollars(1000),
                              elsewhere = on([("aia-living", ["family-protection"])])),
        ],
        categories = [
            discount_category("trauma", TRAUMA, min_sum_insured = dollars(75000),
                              elsewhere = on([("aia-living", TRAUMA), ("aia-living-business", TRAUMA)])),
            discount_category("tpd", TPD, min_sum_insured = dollars(75000),
                              elsewhere = on([("aia-living", TPD), ("aia-living-business", TPD)])),
            discount_category("income", INCOME_MAIN, min_sum_insured = dollars(2000),
                              elsewhere = on([("aia-living", INCOME_MAIN), ("aia-living-business", BUSINESS_INCOME),
                                              ("aia-start-up-income", ["start-up-income"])])),
        ],
        count = count,
    )

# ---- The product -----------------------------------------------------------------

LUMP_SUM_MAIN = [
    "life", "family-protection", "accidental-death",
    "critical-conditions", "critical-conditions-accelerated",
    "progressive-care", "progressive-care-accelerated",
    "severe-trauma", "severe-trauma-accelerated",
    "tpd-own-occupation", "tpd-own-occupation-accelerated",
    "tpd-any-occupation", "tpd-any-occupation-accelerated",
]
INCOME_MAIN = ["income-agreed-value", "income-indemnity", "income-loss-of-earnings", "mortgage-income-rent"]
TRAUMA = ["critical-conditions", "critical-conditions-accelerated", "progressive-care", "progressive-care-accelerated",
          "severe-trauma", "severe-trauma-accelerated"]
TPD = ["tpd-own-occupation", "tpd-own-occupation-accelerated", "tpd-any-occupation", "tpd-any-occupation-accelerated"]

# ---- The published wordings each cover is bound to ---------------------------------

# Each cover names the AIA wordings that govern it (products/definitions/
# aia_wordings.star, the text as published), beside the summaries above. A
# cover missing from this map fails to load.
WORDINGS = {
    "life": ["aia-wording-1100-al-umb", "aia-wording-1101-al-life", "aia-wording-1114-alp-wop"],
    "family-protection": ["aia-wording-1100-al-umb", "aia-wording-1103-al-fp", "aia-wording-1114-alp-wop"],
    "accidental-death": ["aia-wording-1100-al-umb", "aia-wording-1102-al-ad"],
    "critical-conditions": ["aia-wording-1100-al-umb", "aia-wording-1105-al-cc", "aia-wording-1114-alp-wop"],
    "critical-conditions-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1105-al-cc", "aia-wording-1114-alp-wop"],
    "progressive-care": ["aia-wording-1100-al-umb", "aia-wording-1106-al-pc", "aia-wording-1114-alp-wop"],
    "progressive-care-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1106-al-pc", "aia-wording-1114-alp-wop"],
    "severe-trauma": ["aia-wording-1100-al-umb", "aia-wording-1119-al-st", "aia-wording-1114-alp-wop"],
    "severe-trauma-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1119-al-st", "aia-wording-1114-alp-wop"],
    "tpd-own-occupation": ["aia-wording-1100-al-umb", "aia-wording-1104-al-tpd", "aia-wording-1114-alp-wop"],
    "tpd-own-occupation-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1104-al-tpd", "aia-wording-1114-alp-wop"],
    "tpd-any-occupation": ["aia-wording-1100-al-umb", "aia-wording-1104-al-tpd", "aia-wording-1114-alp-wop"],
    "tpd-any-occupation-accelerated": ["aia-wording-1100-al-umb", "aia-wording-1104-al-tpd", "aia-wording-1114-alp-wop"],
    "income-agreed-value": ["aia-wording-1100-al-umb", "aia-wording-1108-al-ip", "aia-wording-1113-al-red", "aia-wording-1114-alp-wop"],
    "income-indemnity": ["aia-wording-1100-al-umb", "aia-wording-1115-al-ind", "aia-wording-1113-al-red", "aia-wording-1114-alp-wop"],
    "income-loss-of-earnings": ["aia-wording-1100-al-umb", "aia-wording-1109-al-loe", "aia-wording-1113-al-red", "aia-wording-1114-alp-wop"],
    "mortgage-income-rent": ["aia-wording-1100-al-umb", "aia-wording-1117-al-mirc", "aia-wording-1113-al-red", "aia-wording-1114-alp-wop"],
    "retirement-protection": ["aia-wording-1100-al-umb", "aia-wording-1111-al-rp"],
    "specified-accidental-injury": ["aia-wording-1100-al-umb", "aia-wording-1116-al-saic"],
    "specialist-testing": ["aia-wording-1100-al-umb", "aia-wording-1118-al-sts"],
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
    convs = list(c["conversions"])
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
    code = "aia-living",
    name = "AIA Living",
    version = 1,
    effective_from = "2026-11-14",
    line = "life",
    currency = "NZD",
    # The umbrella wording: Statutory Fund Number 1.
    statutory_fund = "SF1-life",
    rounding = "half_even",
    covers = with_wordings([
        cover(
            code = "life",
            cover_type = "life",
            benefit = lump_sum(),
            rating = stepped("aia-life"),
            definitions = ["aia-death", "aia-terminal-illness", "aia-specified-terminal-conditions"],
            min_entry_age = 10,
            max_entry_age = 70,
            min_sum_insured = dollars(10000),  # PH: AIA publishes no minimum or maximum
            insurability_option = special_events(),
            adds_cover = special_events_tpd_trauma(),
            # Optional Life Future Insurability (1101 AL-LIFE, sections 13 and 16): at each third
            # anniversary, the greater of 20% of the original cover and $100,000, accepted within
            # 60 days after it, before age 60, at most the lower of twice the original cover and
            # $1,000,000 in all, and not within 12 months of another increase without evidence.
            anniversary_option = anniversary_option(
                every_years = 3,
                max_percent_of_original = "20",
                at_least = dollars(100000),
                max_total_percent_of_original = "200",
                max_total_increase = dollars(1000000),
                within_days = 60,
                to_age = 60,
                requires = "future-insurability",
                once_in_days = 365,
            ),
            option_axes = [
                premium_axis(),
                waiver_axis(),
                optional("future-insurability", "Optional Life Future Insurability Benefit", "1.02"),  # PH
            ],
            built_in = common_built_ins() + [
                built_in("terminal-illness", "Terminal Illness Benefit", "early_payment", percent_of_cover = "100",
                         definitions = ["aia-terminal-illness"]),
                built_in("specified-terminal-conditions", "Specified Terminal Conditions Benefit", "early_payment",
                         max = dollars(250000), percent_of_cover = "30", definitions = ["aia-specified-terminal-conditions"]),
                # $15,000 where the life assured is the sole owner.
                built_in("bereavement-support", "Bereavement Support Benefit", "early_payment", max = dollars(25000)),
                built_in("repatriation", "Repatriation Benefit", "early_payment", max = dollars(25000)),
                built_in("parents-grieving", "Parents Grieving Benefit", "additional", max = dollars(15000)),
                built_in("special-events-increase", "Special Events Increase Facility", "facility"),
                built_in("special-events-tpd-trauma", "Special Events TPD/Trauma Facility (the lower of 50% and $100,000)", "facility"),
                built_in("premium-conversion", "Premium Conversion Facility (before age 65)", "facility"),
                built_in("life-future-insurability", "Optional Life Future Insurability Benefit (each third anniversary, the greater of 20% and $100,000)",
                         "facility", requires = "future-insurability"),
            ],
        ),
        cover(
            code = "family-protection",
            cover_type = "life",
            # A monthly payment from death to the end of the benefit term the
            # owner chose; the term is the cover's term, so the cover's end
            # is when payments stop.
            benefit = monthly_benefit(waiting_days = 0, benefit_months = 360),
            rating = stepped("aia-family-protection"),
            definitions = ["aia-death", "aia-terminal-illness"],
            min_entry_age = 10,
            max_entry_age = 70,
            min_sum_insured = dollars(500),  # PH: monthly benefit
            # Special Events Increase Facility (1103 AL-FP section 3): the lower of 50% of
            # the original monthly benefit and $500 a month each event, the lower of 50%
            # and $1,500 a month in all, asked within 12 months of the event, not over age
            # 50, once in any 12 months.
            insurability_option = insurability_option(
                events = SPECIAL_EVENTS,
                max_percent_of_original = "50",
                max_increase = dollars(500),
                max_uses = 99,
                within_days = 365,
                to_age = 51,
                max_total_percent_of_original = "50",
                max_total_increase = dollars(1500),
                once_in_days = 365,
            ),
            option_axes = [
                option_axis("term", "Benefit term",
                    [choice("%dy" % y, "Fixed term of %d years" % y, expiry_after_years = y) for y in range(5, 31)] +
                    # A term to an age runs for at least 15 years: underwriting checks it.
                    [choice("to-%d" % a, "To age %d" % a, expiry_age = a) for a in range(31, 71)]),
                premium_axis(),
                waiver_axis(),
            ],
            built_in = common_built_ins() + [
                built_in("bereavement-support", "Bereavement Support Benefit", "early_payment", max = dollars(25000)),
                built_in("parents-grieving", "Parents Grieving Benefit", "additional", max = dollars(15000)),
                built_in("special-events-increase", "Special Events Increase Facility", "facility"),
                built_in("premium-conversion", "Premium Conversion Facility (before age 65)", "facility"),
            ],
        ),
        cover(
            code = "accidental-death",
            cover_type = "life",
            benefit = lump_sum(),
            rating = stepped("aia-accidental-death"),
            definitions = ["aia-accidental-death"],
            min_entry_age = 16,
            max_entry_age = 60,
            expiry_age = 65,
            min_sum_insured = dollars(10000),  # PH
            max_sum_insured = dollars(500000),
            option_axes = [waiver_axis()],
            built_in = common_built_ins(),
        ),
        critical_conditions("critical-conditions", False),
        critical_conditions("critical-conditions-accelerated", True),
        progressive_care("progressive-care", False),
        progressive_care("progressive-care-accelerated", True),
        severe_trauma("severe-trauma", False),
        severe_trauma("severe-trauma-accelerated", True),
        tpd("tpd-own-occupation", True, False),
        tpd("tpd-own-occupation-accelerated", True, True),
        tpd("tpd-any-occupation", False, False),
        tpd("tpd-any-occupation-accelerated", False, True),
        income_cover("income-agreed-value", ["aia-ip-total-disability", "aia-ip-agreed-value"], True, IP_WEEKS, IP_PERIODS, dollars(30000)),  # PH max
        income_cover("income-indemnity", ["aia-ip-total-disability", "aia-ip-indemnity"], True, IP_WEEKS, IP_PERIODS, dollars(30000)),  # PH max
        income_cover("income-loss-of-earnings", ["aia-ip-total-disability", "aia-ip-loss-of-earnings"], True, IP_WEEKS, IP_PERIODS, dollars(30000)),  # PH max
        # Up to 115% of mortgage or rent payments, or 45% of income, and $20,000 a
        # month with all increases (1117 AL-MIRC). ACC and other mortgage cover are
        # offset only above $7,500 a month.
        income_cover("mortgage-income-rent", ["aia-ip-total-disability", "aia-mortgage-income-rent"], False, MIRC_WEEKS, MIRC_PERIODS, dollars(20000)),
        cover(
            code = "retirement-protection",
            cover_type = "income_protection",
            # Contributions towards retirement saving while an income claim is paid.
            benefit = monthly_benefit(waiting_days = 28, benefit_months = 24),
            rating = stepped("aia-income"),
            premium_waiver = True,
            definitions = ["aia-ip-total-disability", "aia-retirement-protection"],
            min_entry_age = 16,
            max_entry_age = 60,
            expiry_age = 65,
            min_sum_insured = dollars(100),  # PH
            max_sum_insured = dollars(5000),  # PH
            option_axes = [waiting_axis(IP_WEEKS), period_axis(IP_PERIODS)],
        ),
        cover(
            code = "specified-accidental-injury",
            cover_type = "accidental_injury",
            # A lump sum by injury category, in multiples of the chosen
            # monthly amount: A once, B twice, C three times, D six, E twelve.
            # Paid on top of ACC.
            benefit = lump_sum(ends_cover = False),
            rating = stepped("aia-accidental-injury"),
            definitions = ["aia-specified-accidental-injury"],
            min_entry_age = 16,
            max_entry_age = 60,  # PH
            expiry_age = 65,  # PH
            min_sum_insured = dollars(500),  # PH: monthly amount
            max_sum_insured = dollars(5000),
            built_in = [
                built_in("category-a", "Category A injuries", "additional", multiple_of_cover = "1"),
                built_in("category-b", "Category B injuries", "additional", multiple_of_cover = "2"),
                built_in("category-c", "Category C injuries", "additional", multiple_of_cover = "3"),
                built_in("category-d", "Category D injuries", "additional", multiple_of_cover = "6"),
                built_in("category-e", "Category E injuries", "additional", multiple_of_cover = "12"),
            ],
        ),
        cover(
            code = "specialist-testing",
            cover_type = "health",
            benefit = health_schedule([
                benefit_limit("specialist", per_year = dollars(10000)),
                benefit_limit("diagnostics", per_year = dollars(100000)),
                # Every three years; $750 for AIA Vitality members.
                benefit_limit("health-screening", per_event = dollars(500)),
                # After two years of continuous cover.
                benefit_limit("obstetric-fertility", per_year = dollars(1500)),
            ]),
            rating = per_life("aia-specialist-testing"),
            excess_options = [
                excess_option("nil", dollars(0), premium_factor = "1.25"),  # PH
                excess_option("250", dollars(250), premium_factor = "1.00"),
            ],
            definitions = ["aia-specialist-testing"],
            gst_rate = "0.15",
            min_entry_age = 0,
            max_entry_age = 74,  # PH
        ),
    ]),
    features = [
        composition(
            # Every life holds at least one main benefit: the add-ons cannot
            # stand alone.
            core = LUMP_SUM_MAIN + INCOME_MAIN,
            requires_any = {"retirement-protection": ["income-agreed-value", "income-indemnity", "income-loss-of-earnings"]},
            exclusive = [
                ["critical-conditions", "critical-conditions-accelerated"],
                ["progressive-care", "progressive-care-accelerated"],
                ["severe-trauma", "severe-trauma-accelerated"],
                ["income-agreed-value", "income-indemnity", "income-loss-of-earnings"],
            ],
            # Accelerated benefits are paid out of life cover.
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
        # 30 days' grace; AIA may cancel once a premium is 31 days overdue.
        arrears(grace_days = 30, lapse_days = 31, reinstatement_days = 180, reinstatement_evidence_free_days = 60),  # PH: reinstatement
        # Level cover or CPI linked: indexation is offered and may be declined.
        indexation(rate_code = "cpi"),
        discounts(
            chain = [
                # Multi-Benefit Discount: life cover and benefits from other
                # categories, each life on its own, minimum cover amounts.
                best_of([
                    discount("mbd-15", "Multi-Benefit Discount", "multi_benefit_discount", percent_off("15"),
                             condition = MBD(3)),
                    discount("mbd-12-5", "Multi-Benefit Discount", "multi_benefit_discount", percent_off("12.5"),
                             condition = MBD(2)),
                    discount("mbd-10", "Multi-Benefit Discount", "multi_benefit_discount", percent_off("10"),
                             condition = MBD(1)),
                ]),
                # AIA Vitality Premium Flex: 10% in the first year, then
                # moved each anniversary by the member's status, between 0%
                # and 20%. The wellness module sets the tier for each life.
                discount("vitality", "AIA Vitality discount", "vitality_discount",
                         by_tier({"%d" % p: "%d" % p for p in range(0, 21)}), condition = entitled("aia-vitality")),
            ],
        ),
        # 15 days from receiving the policy document, deemed five days after posting.
        cooling_off(days = 20),
    ],
    # Suspension of cover: no premium and no cover for up to 12 months in
    # all. The 12 months in force first is a best estimate until AIA confirms it.
    cover_suspension = cover_suspension(max_months = 12, min_months_in_force = 12),
)
