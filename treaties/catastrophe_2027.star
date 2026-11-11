# Catastrophe cover for 2027: many lives lost in one event.
# Synthetic terms for development and tests. Not a negotiated treaty.

AGGREGATE_TREATY = aggregate_treaty(
    id = "catastrophe-2027",
    name = "Catastrophe excess of loss 2027",
    version = 1,
    period_start = "2027-01-01",
    period_end = "2027-12-31",
    currency = "NZD",
    scope = [scope("*", ["life", "group_life"])],
    kind = catastrophe(min_lives = 3, retention = dollars(1000000), limit = dollars(20000000)),
    panel = [reinsurer("reinsurer-north", "0.50"), reinsurer("reinsurer-south", "0.50")],
)
