# Treaty authoring kit.
#
# These helpers only build plain data. The engine validates the result into
# strongly typed Rust structs at load time; Starlark plays no part at runtime.
# Shares and rates are written as decimal text ("0.40"), never as floats.

def dollars(n):
    """Whole dollars as minor units."""
    return n * 100

def scope(product, cover_types):
    return {"product": product, "cover_types": cover_types}

def quota_share(share, automatic_limit):
    return {"kind": "quota_share", "share": share, "automatic_limit_minor": automatic_limit}

def surplus(retention, automatic_limit):
    return {"kind": "surplus", "retention_minor": retention, "automatic_limit_minor": automatic_limit}

def excess_of_loss(retention, limit):
    return {"kind": "excess_of_loss", "retention_minor": retention, "limit_minor": limit}

def reinsurer(id, share):
    return {"reinsurer": id, "share": share}

def rate_table_premium(table):
    return {"kind": "rate_table", "table": table}

def original_premium(of = "net"):
    return {"kind": "original_premium", "of": of}

def claim_terms(notify_at = None, consent_at = None, consent_days = 0, ex_gratia_recoverable = False):
    return {
        "notification_threshold_minor": notify_at,
        "consent_threshold_minor": consent_at,
        "consent_days": consent_days,
        "ex_gratia_recoverable": ex_gratia_recoverable,
    }

def scheme_share(share):
    return {"kind": "scheme_share", "share": share}

def excess_per_member(limit):
    return {"kind": "excess_per_member", "limit_minor": limit}

def group_terms(unnamed, excess_path, facultative_reinsurer, facultative_limit, facultative_rate):
    return {
        "unnamed": unnamed,
        "excess_path": excess_path,
        "facultative_reinsurer": facultative_reinsurer,
        "facultative_limit_minor": facultative_limit,
        "facultative_rate": facultative_rate,
    }

def statements(period, agree_before_payment = False, listing_part_rows = 2000):
    return {"period": period, "agree_before_payment": agree_before_payment, "listing_part_rows": listing_part_rows}

def profit_commission(share, expense_allowance):
    return {"share": share, "expense_allowance": expense_allowance}

def clauses(errors_and_omissions = "", claims_cooperation = ""):
    return {"errors_and_omissions": errors_and_omissions, "claims_cooperation": claims_cooperation}

def treaty(id, name, version, effective_from, currency, scope, basis, panel, premium, claims, statement,
           commission = "0", closed_to_new_business_from = None, reallocate_freed_retention = False,
           exclusions = [], group = None, recapture_after_years = None, profit_commission = None,
           listing_fields = [], clauses = None, retention_group = None):
    return {
        "id": id,
        "name": name,
        "version": version,
        "effective_from": effective_from,
        "closed_to_new_business_from": closed_to_new_business_from,
        "currency": currency,
        "scope": scope,
        "basis": basis,
        "retention_group": retention_group,
        "panel": panel,
        "premium": premium,
        "commission": commission,
        "claims": claims,
        "reallocate_freed_retention": reallocate_freed_retention,
        "exclusions": exclusions,
        "group": group,
        "recapture_after_years": recapture_after_years,
        "statement": statement,
        "profit_commission": profit_commission,
        "listing_fields": listing_fields,
        "clauses": clauses if clauses != None else {},
    }

def catastrophe(min_lives, retention, limit):
    return {"kind": "catastrophe", "min_lives": min_lives, "retention_minor": retention, "limit_minor": limit}

def stop_loss(attachment, limit):
    return {"kind": "stop_loss", "attachment_minor": attachment, "limit_minor": limit}

def aggregate_treaty(id, name, version, period_start, period_end, currency, scope, kind, panel):
    return {
        "id": id,
        "name": name,
        "version": version,
        "period_start": period_start,
        "period_end": period_end,
        "currency": currency,
        "scope": scope,
        "kind": kind,
        "panel": panel,
    }

def reinsurer_rate(rate, age_from = None, age_to = None, sex = None, smoker = None):
    return {"rate": rate, "age_from": age_from, "age_to": age_to, "sex": sex, "smoker": smoker}

def reinsurer_rate_table(id, version, effective_from, rows):
    return {"id": id, "version": version, "effective_from": effective_from, "rows": rows}

# Worked examples. Every treaty ships at least one; they run when the treaty
# is loaded, so a treaty that does not do what its author says cannot be
# activated.

def covered(sum_insured, start, end = None, member = False, cover_type = None):
    return {"sum_insured_minor": sum_insured, "start": start, "end": end, "member": member, "cover_type": cover_type}

def expect(retained, ceded, facultative = 0, unplaced = 0):
    return {"retained_minor": retained, "ceded_minor": ceded, "facultative_minor": facultative, "unplaced_minor": unplaced}

def scenario(name, covers, expect, matched = True):
    return {"name": name, "covers": covers, "expect": expect, "matched": matched}
