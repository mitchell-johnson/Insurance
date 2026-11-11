# Group life: scheme share on members not linked to a party, surplus per life
# on those who are, with standing facultative terms for cover a membership
# file writes over the retention.
# Synthetic terms for development and tests. Not a negotiated treaty.

TREATY = treaty(
    id = "group-life-surplus",
    name = "Group life surplus",
    version = 1,
    effective_from = "2027-01-01",
    currency = "NZD",
    scope = [scope("group-life", ["group_life"])],
    retention_group = "life",
    basis = surplus(retention = dollars(500000), automatic_limit = dollars(2000000)),
    panel = [reinsurer("reinsurer-north", "1")],
    premium = original_premium("net"),
    commission = "0.15",
    claims = claim_terms(notify_at = dollars(750000)),
    group = group_terms(
        unnamed = scheme_share("0.30"),
        excess_path = "facultative",
        facultative_reinsurer = "reinsurer-north",
        facultative_limit = dollars(1000000),
        facultative_rate = "1.20",
    ),
    statement = statements("monthly"),
    clauses = clauses(errors_and_omissions = "article 9", claims_cooperation = "article 7"),
)

SCENARIOS = [
    scenario(
        "a member not linked to a party is ceded on the scheme share",
        matched = False,
        covers = [covered(dollars(200000), "2027-03-01", member = True)],
        expect = [expect(retained = dollars(140000), ceded = dollars(60000))],
    ),
    scenario(
        "scheme cover that takes a known life over the retention is ceded facultatively",
        covers = [covered(dollars(400000), "2027-02-01"), covered(dollars(300000), "2027-03-01", member = True)],
        expect = [
            expect(retained = dollars(400000), ceded = 0),
            expect(retained = dollars(100000), ceded = 0, facultative = dollars(200000)),
        ],
    ),
]
