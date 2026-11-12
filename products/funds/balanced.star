# Synthetic fund for development and tests. The spreads, tolerances and the
# compensation minimum are placeholders, not terms anyone has approved.

FUND = fund(
    id = "balanced",
    version = 1,
    name = "Balanced Fund",
    currency = "NZD",
    cut_off = "15:00",
    price_decimals = 4,
    unit_rounding = "down",
    buy_spread_bps = 15,
    sell_spread_bps = 15,
    # New Zealand public holidays the fund does not price on.
    non_pricing_days = ["2027-12-27", "2027-12-28", "2028-01-03", "2028-01-04", "2028-02-07"],
    # Terms the unit pricing module reads. All values are strings.
    pricing = {
        "previous_move": "0.03",
        "benchmark_gap": "0.01",
        "stale_asset_days": "3",
        "unexplained_move": "0.005",
        "error_threshold": "0.003",
        "launch_price": "1.0000",
        "pricing_lag_days": "1",
        "compensation_minimum_minor": "2000",
    },
)
