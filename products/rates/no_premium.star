# A cover with no premium of its own. An investment account is paid into by
# contributions, which are the customer's money and never premium, so its
# rate is zero at every age.

RATE_TABLE = rate_table(
    id = "no-premium",
    version = 1,
    effective_from = "2027-01-01",
    rows = [
        rate("0"),
    ],
)
