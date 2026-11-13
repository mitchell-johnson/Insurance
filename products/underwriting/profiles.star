# Authority grades. Each underwriter's own profile is activated from one of
# these with their staff id; the grades and their limits are the chief
# underwriter's to set.

def _grade(underwriter, life, trauma, health, queues, loading, exclude = False, decline = False, defer = False, senior = False, role = "underwriter"):
    return profile(
        underwriter = underwriter,
        version = 1,
        limits = {"life": dollars(life), "trauma": dollars(trauma), "health": dollars(health), "income_protection": dollars(trauma), "group_life": dollars(life)},
        products = ["term-life", "health-plan", "health-flex", "group-life"],
        queues = queues,
        max_loading_percent = loading,
        may_exclude = exclude,
        may_decline = decline,
        may_defer = defer,
        senior = senior,
        role = role,
    )

PROFILES = [
    _grade("grade-assistant", 500000, 250000, 250000, ["tele_interview", "medical"], 0),
    _grade("grade-underwriter", 1500000, 750000, 500000, ["tele_interview", "medical", "occupational", "financial"], 100, exclude = True, defer = True),
    _grade("grade-senior", 5000000, 2000000, 1000000, ["medical", "occupational", "financial", "senior_review", "reinsurer_liaison"], 300,
           exclude = True, decline = True, defer = True, senior = True, role = "queue_lead"),
    _grade("grade-chief", 20000000, 5000000, 2000000, ["medical", "occupational", "financial", "senior_review", "reinsurer_liaison"], 500,
           exclude = True, decline = True, defer = True, senior = True, role = "manager"),
]
