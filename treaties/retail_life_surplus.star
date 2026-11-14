# Retail life, trauma and TPD: surplus per life over a $500,000 retention.
# Synthetic terms for development and tests. Not a negotiated treaty.

TREATY = treaty(
    id = "retail-life-surplus",
    name = "Retail life and trauma surplus",
    version = 1,
    effective_from = "2027-01-01",
    currency = "NZD",
    scope = [
        scope("term-life", ["life", "trauma"]),
        # The AIA NZ range: lump sum cover on a life shares the one retention.
        scope("aia-living", ["life", "trauma", "tpd"]),
        scope("aia-living-business", ["life", "trauma", "tpd"]),
        scope("aia-cover-for-life", ["life"]),
        scope("aia-essentials-life", ["life"]),
        scope("aia-starter", ["life"]),
        scope("aia-interim", ["life", "trauma"]),
    ],
    retention_group = "life",
    basis = surplus(retention = dollars(500000), automatic_limit = dollars(4000000)),
    panel = [reinsurer("reinsurer-north", "0.60"), reinsurer("reinsurer-south", "0.40")],
    premium = rate_table_premium("ri-life-base"),
    commission = "0.20",
    claims = claim_terms(notify_at = dollars(1000000), consent_at = dollars(2000000), consent_days = 10),
    recapture_after_years = 10,
    statement = statements("quarterly", agree_before_payment = True),
    profit_commission = profit_commission(share = "0.50", expense_allowance = "0.10"),
    listing_fields = ["age", "sex", "smoker"],
    clauses = clauses(errors_and_omissions = "article 14", claims_cooperation = "article 11"),
)

SCENARIOS = [
    scenario(
        "a second cover on the same life fills the retention and cedes the rest",
        covers = [covered(dollars(300000), "2027-02-01"), covered(dollars(400000), "2027-06-01")],
        expect = [expect(retained = dollars(300000), ceded = 0), expect(retained = dollars(200000), ceded = dollars(200000))],
    ),
    scenario(
        "cover above the automatic limit is left for facultative terms, never retained silently",
        covers = [covered(dollars(6000000), "2027-02-01")],
        expect = [expect(retained = dollars(500000), ceded = dollars(4000000), unplaced = dollars(1500000))],
    ),
]
