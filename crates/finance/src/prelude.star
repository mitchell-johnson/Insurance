# Authoring kit for finance configuration.
#
# These helpers only build plain data. The finance ledger validates the
# result into typed Rust structs at load time.

def account(code, name, kind, control = False, roles = []):
    """A finance ledger account. `kind` is asset, liability, income or expense.

    A control account is locked: only the feed from the core posts to it.
    `roles` are the core account roles consolidated into it.
    """
    return {"code": code, "name": name, "class": kind, "control": control, "roles": roles}

def chart(version, effective_from, retained_earnings, accounts):
    return {
        "version": version,
        "effective_from": effective_from,
        "retained_earnings": retained_earnings,
        "accounts": accounts,
    }

def automatic(id, name, owner, day, blocking = True, quarterly = False):
    """A check the finance ledger works out from its own state."""
    return {"id": id, "name": name, "owner": owner, "day": day, "blocking": blocking, "quarterly": quarterly, "source": {"kind": "automatic"}}

def reported(id, name, owner, day, by, blocking = True, quarterly = False):
    """A check whose result another module, or a person, reports. `by` is the caller allowed to."""
    return {"id": id, "name": name, "owner": owner, "day": day, "blocking": blocking, "quarterly": quarterly, "source": {"kind": "reported", "by": by}}

def aged(kind, name, owner, day, by, max_age_days, max_amount_minor, blocking = True):
    """Aged items another module reports (suspense, held balances), checked against thresholds.

    An item older than `max_age_days` or larger than `max_amount_minor` with no documented
    action turns the check red.
    """
    return {
        "kind": kind,
        "name": name,
        "owner": owner,
        "day": day,
        "by": by,
        "max_age_days": max_age_days,
        "max_amount_minor": max_amount_minor,
        "blocking": blocking,
    }

def close_config(version, checks, aged = [], holidays = []):
    return {"version": version, "checks": checks, "aged": aged, "holidays": holidays}
