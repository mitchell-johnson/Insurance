# The schema each actuarial table is validated against before it can be
# activated. The bounds, steps and tolerances are proposals for the
# appointed actuary to set; the rules themselves are versioned here and
# owned jointly by actuarial and engineering.
#
# max_age_step is the smoothness rule: a band's rate may be at most that
# multiple of the band before it. A cell mis-keyed by a factor of ten passes
# a range check and fails this one.

def by_age_sex_smoker(kind, rate_max):
    return {
        "kind": kind,
        "version": 1,
        "by_sex": True,
        "by_smoker": True,
        "ages": [18, 102],
        "rate_min": "0.0001",
        "rate_max": rate_max,
        "decimal_places": 4,
        "rising_with_age": True,
        "max_age_step": "2.5",
        "smoker_not_below_non_smoker": True,
        "change_review_pct": 10,
        "change_block_factor": 3,
    }

def to_age(schema, last):
    """The same schema for a table that stops at an earlier age, as a cover that expires does."""
    schema["ages"] = [schema["ages"][0], last]
    return schema

def per_life_by_age(rate_max):
    return {
        "kind": "annual_premium_per_life",
        "version": 1,
        "by_sex": False,
        "by_smoker": False,
        "ages": [0, 150],
        "rate_min": "1",
        "rate_max": rate_max,
        "decimal_places": 2,
        "rising_with_age": True,
        "max_age_step": "2.5",
        "change_review_pct": 10,
        "change_block_factor": 3,
    }

def aia_by_age(kind, first, last, rate_max, by_class = False):
    """An AIA placeholder table (docs/aia-products.md): by age from the
    product's youngest entry to its oldest cover age, sex and smoking, and
    by occupation class 1 to 5 where the risk depends on the work."""
    schema = by_age_sex_smoker(kind, rate_max)
    schema["ages"] = [first, last]
    if by_class:
        schema["occupation_classes"] = ["1", "2", "3", "4", "5"]
    return schema

def aia_group(rate_max):
    return {
        "kind": "unit_rate_per_mille_by_category",
        "version": 1,
        "by_sex": False,
        "by_smoker": False,
        "categories": ["staff", "executive"],
        "rate_min": "0.01",
        "rate_max": rate_max,
        "decimal_places": 2,
        "change_review_pct": 10,
        "change_block_factor": 3,
    }

TABLE_SCHEMAS = {
    "life-base": by_age_sex_smoker("mortality_per_mille", "1000"),
    "trauma-base": by_age_sex_smoker("morbidity_per_mille", "1000"),
    # Income protection: per thousand of monthly benefit, to the cover's expiry at 65.
    "income-base": to_age(by_age_sex_smoker("disability_income_per_mille", "10000"), 67),
    "health-base": {
        "kind": "annual_premium_per_life",
        "version": 1,
        "by_sex": False,
        "by_smoker": False,
        "ages": [0, 150],
        "rate_min": "1",
        "rate_max": "50000",
        "decimal_places": 2,
        "rising_with_age": True,
        "max_age_step": "2.5",
        "change_review_pct": 10,
        "change_block_factor": 3,
    },
    # The flexible health plan's benefit options: an annual premium per life by age band.
    "flex-hospital": per_life_by_age("50000"),
    "flex-specialist": per_life_by_age("50000"),
    "flex-dental": per_life_by_age("5000"),
    "flex-optical": per_life_by_age("5000"),
    # A benefit with no premium of its own: its only rate is nil.
    "no-premium": {
        "kind": "nil_premium",
        "version": 1,
        "by_sex": False,
        "by_smoker": False,
        "rate_min": "0",
        "rate_max": "0",
        "decimal_places": 0,
        "change_review_pct": 10,
        "change_block_factor": 3,
    },
    "group-life-standard": {
        "kind": "unit_rate_per_mille_by_category",
        "version": 1,
        "by_sex": False,
        "by_smoker": False,
        "categories": ["staff", "executive"],
        "rate_min": "0.01",
        "rate_max": "100",
        "decimal_places": 2,
        "change_review_pct": 10,
        "change_block_factor": 3,
    },
    # AIA New Zealand products: placeholders until the appointed actuary
    # supplies the real tables (tools/aia_placeholder_rates.py).
    "aia-life": aia_by_age("mortality_per_mille", 10, 105, "1000"),
    "aia-family-protection": aia_by_age("mortality_per_mille", 10, 85, "10000"),
    "aia-accidental-death": aia_by_age("mortality_per_mille", 16, 66, "100"),
    "aia-trauma": aia_by_age("morbidity_per_mille", 0, 105, "1000"),
    "aia-progressive-care": aia_by_age("morbidity_per_mille", 0, 105, "1000"),
    "aia-severe-trauma": aia_by_age("morbidity_per_mille", 0, 105, "1000"),
    "aia-tpd": aia_by_age("morbidity_per_mille", 16, 71, "1000", by_class = True),
    "aia-income": aia_by_age("disability_income_per_mille", 16, 71, "100000", by_class = True),
    "aia-accidental-injury": aia_by_age("disability_income_per_mille", 16, 66, "10000", by_class = True),
    "aia-business-continuity": aia_by_age("disability_income_per_mille", 16, 66, "100000", by_class = True),
    "aia-specialist-testing": per_life_by_age("50000"),
    "aia-private-health": per_life_by_age("50000"),
    "aia-private-health-plus": per_life_by_age("50000"),
    "aia-cancer-care": per_life_by_age("50000"),
    "aia-group-life": aia_group("100"),
    "aia-group-tpd": aia_group("100"),
    "aia-group-critical-illness": aia_group("100"),
    "aia-group-severe-trauma": aia_group("100"),
    "aia-group-income": aia_group("1000"),
}
