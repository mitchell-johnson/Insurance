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

TABLE_SCHEMAS = {
    "life-base": by_age_sex_smoker("mortality_per_mille", "1000"),
    "trauma-base": by_age_sex_smoker("morbidity_per_mille", "1000"),
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
}
