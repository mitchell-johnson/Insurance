# Product authoring kit for insurance covers.
#
# These helpers only build plain data. The engine validates the result into
# strongly typed Rust structs at load time; Starlark plays no part at runtime.

def dollars(n):
    """Whole dollars as minor units."""
    return n * 100

def lump_sum(survival_days = 0, ends_cover = True):
    return {"kind": "lump_sum", "survival_days": survival_days, "ends_cover": ends_cover}

def monthly_benefit(waiting_days, benefit_months):
    return {"kind": "monthly_benefit", "waiting_days": waiting_days, "benefit_months": benefit_months}

def benefit_limit(category, per_event = None, per_year = None, excess = 0):
    return {"category": category, "per_event_minor": per_event, "per_year_minor": per_year, "excess_minor": excess}

def health_schedule(limits):
    return {"kind": "health_schedule", "limits": limits}

def investment_account(funds):
    return {"kind": "investment_account", "funds": funds}

def stepped(table):
    return {"premium_type": "stepped", "table": table, "basis": "per_thousand"}

def level(table, to_age):
    return {"premium_type": "level", "table": table, "basis": "per_thousand", "level_to_age": to_age}

def per_life(table):
    return {"premium_type": "stepped", "table": table, "basis": "per_life"}

def group_unit_rate(table):
    return {"premium_type": "group_unit_rate", "table": table, "basis": "per_thousand"}

def benefit_category(code, salary_multiple, max_sum_insured):
    return {"code": code, "salary_multiple": salary_multiple, "max_sum_insured_minor": max_sum_insured}

def cover(code, cover_type, benefit, rating, definitions = [], gst_rate = None, levies = [],
          expiry_age = None, min_sum_insured = 0, max_sum_insured = None, categories = [],
          automatic_acceptance_limit = None):
    return {
        "code": code,
        "cover_type": cover_type,
        "benefit": benefit,
        "rating": rating,
        "definitions": definitions,
        "gst_rate": gst_rate,
        "levies": levies,
        "expiry_age": expiry_age,
        "min_sum_insured_minor": min_sum_insured,
        "max_sum_insured_minor": max_sum_insured,
        "categories": categories,
        "automatic_acceptance_limit_minor": automatic_acceptance_limit,
    }

def levy(code, rate):
    return {"code": code, "rate": rate}

# Lifecycle features. They run in the order the engine defines, not the order
# they are listed in.

def billing(frequencies, policy_fee_annual = "0", modal_loading = {}):
    return {"kind": "billing", "frequencies": frequencies, "policy_fee_annual": policy_fee_annual, "modal_loading": modal_loading}

def earning(day_count = "actual"):
    return {"kind": "earning", "day_count": day_count}

def arrears(grace_days, lapse_days, suspend_after_grace = False, reinstatement_days = 90):
    return {"kind": "arrears", "grace_days": grace_days, "lapse_days": lapse_days,
            "suspend_after_grace": suspend_after_grace, "reinstatement_days": reinstatement_days}

def indexation(rate_code, opt_out = True):
    return {"kind": "indexation", "rate_code": rate_code, "opt_out": opt_out}

def multi_cover_discount(min_covers, percent):
    return {"kind": "multi_cover_discount", "min_covers": min_covers, "percent": percent}

def cooling_off(days):
    return {"kind": "cooling_off", "days": days}

def product(code, name, version, effective_from, line, currency, statutory_fund, covers, features,
            rounding = "half_even"):
    return {
        "code": code,
        "name": name,
        "version": version,
        "effective_from": effective_from,
        "line": line,
        "currency": currency,
        "statutory_fund": statutory_fund,
        "rounding": rounding,
        "covers": covers,
        "features": features,
    }

def rate(annual_rate, age_from = 0, age_to = 150, sex = None, smoker = None, category = None):
    return {"age_from": age_from, "age_to": age_to, "sex": sex, "smoker": smoker, "category": category, "annual_rate": annual_rate}

def rate_table(id, version, effective_from, rows, scope = None):
    return {"id": id, "version": version, "effective_from": effective_from, "rows": rows, "scope": scope}

def definition(id, version, effective_from, title, text):
    return {"id": id, "version": version, "effective_from": effective_from, "title": title, "text": text}
