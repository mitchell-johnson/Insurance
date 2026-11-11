# Stop loss for 2027: retained claims of the year above an attachment point.
# Synthetic terms for development and tests. Not a negotiated treaty.

AGGREGATE_TREATY = aggregate_treaty(
    id = "stop-loss-2027",
    name = "Stop loss 2027",
    version = 1,
    period_start = "2027-01-01",
    period_end = "2027-12-31",
    currency = "NZD",
    scope = [scope("*", ["*"])],
    kind = stop_loss(attachment = dollars(30000000), limit = dollars(50000000)),
    panel = [reinsurer("reinsurer-south", "1")],
)
