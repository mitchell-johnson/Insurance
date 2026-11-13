# Product authoring kit for insurance covers.
#
# These helpers only build plain data. The engine validates the result into
# strongly typed Rust structs at load time; Starlark plays no part at runtime.

def dollars(n):
    """Whole dollars as minor units."""
    return n * 100

def lump_sum(survival_days = 0, ends_cover = True):
    return {"kind": "lump_sum", "survival_days": survival_days, "ends_cover": ends_cover}

def monthly_benefit(waiting_days, benefit_months, benefit_while_imprisoned = "suspend"):
    return {
        "kind": "monthly_benefit",
        "waiting_days": waiting_days,
        "benefit_months": benefit_months,
        "benefit_while_imprisoned": benefit_while_imprisoned,
    }

def benefit_limit(category, per_event = None, per_year = None, excess = 0):
    return {"category": category, "per_event_minor": per_event, "per_year_minor": per_year, "excess_minor": excess}

def health_schedule(limits):
    return {"kind": "health_schedule", "limits": limits}

def investment_account(funds):
    return {"kind": "investment_account", "funds": funds}

def lower_of():
    """A life whose sex a sex-rated table does not hold pays the lower of its female and male rates."""
    return {"kind": "lower_of"}

def blended(female_share):
    """Such a life pays this share of the female rate and the rest of the male rate."""
    return {"kind": "blended", "female_share": female_share}

def stepped(table, unmatched_sex_rating = lower_of()):
    return {"premium_type": "stepped", "table": table, "basis": "per_thousand", "unmatched_sex_rating": unmatched_sex_rating}

def level(table, to_age, unmatched_sex_rating = lower_of()):
    return {
        "premium_type": "level",
        "table": table,
        "basis": "per_thousand",
        "level_to_age": to_age,
        "unmatched_sex_rating": unmatched_sex_rating,
    }

def per_life(table, unmatched_sex_rating = lower_of()):
    return {"premium_type": "stepped", "table": table, "basis": "per_life", "unmatched_sex_rating": unmatched_sex_rating}

def group_unit_rate(table, unmatched_sex_rating = lower_of()):
    return {"premium_type": "group_unit_rate", "table": table, "basis": "per_thousand", "unmatched_sex_rating": unmatched_sex_rating}

def benefit_category(code, salary_multiple, max_sum_insured):
    return {"code": code, "salary_multiple": salary_multiple, "max_sum_insured_minor": max_sum_insured}

def cover(code, cover_type, benefit, rating, definitions = [], gst_rate = None, levies = [],
          expiry_age = None, min_sum_insured = 0, max_sum_insured = None, categories = [],
          automatic_acceptance_limit = None, excess_options = [], premium_waiver = None,
          shared_counter = None):
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
        "excess_options": excess_options,
        "premium_waiver": premium_waiver,
        "shared_counter": shared_counter,
    }

def excess_option(code, excess, premium_factor = "1"):
    """An excess level a customer may choose on a cover, and what it does to the premium."""
    return {"code": code, "excess_minor": excess, "premium_factor": premium_factor}

def levy(code, rate):
    return {"code": code, "rate": rate}

# Lifecycle features. They run in the order the engine defines, not the order
# they are listed in.

def billing(frequencies, policy_fee_annual = "0", modal_loading = {}):
    return {"kind": "billing", "frequencies": frequencies, "policy_fee_annual": policy_fee_annual, "modal_loading": modal_loading}

def earning(day_count = "actual"):
    return {"kind": "earning", "day_count": day_count}

def arrears(grace_days, lapse_days, suspend_after_grace = False, reinstatement_days = 90, reinstatement_evidence_free_days = None):
    a = {"kind": "arrears", "grace_days": grace_days, "lapse_days": lapse_days,
         "suspend_after_grace": suspend_after_grace, "reinstatement_days": reinstatement_days}
    if reinstatement_evidence_free_days != None:
        a["reinstatement_evidence_free_days"] = reinstatement_evidence_free_days
    return a

def indexation(rate_code, opt_out = True, pays_step_applied_after_event = False):
    return {
        "kind": "indexation",
        "rate_code": rate_code,
        "opt_out": opt_out,
        "pays_step_applied_after_event": pays_step_applied_after_event,
    }

def multi_cover_discount(min_covers, percent):
    return {"kind": "multi_cover_discount", "min_covers": min_covers, "percent": percent}

def composition(core = [], requires = {}, exclusive = []):
    return {"kind": "composition", "core": core, "requires": requires, "exclusive": exclusive}

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

def rate(annual_rate, age_from = 0, age_to = 150, sex = None, smoker = None, category = None, occupation_class = None):
    return {
        "age_from": age_from,
        "age_to": age_to,
        "sex": sex,
        "smoker": smoker,
        "category": category,
        "occupation_class": occupation_class,
        "annual_rate": annual_rate,
    }

def rate_table(id, version, effective_from, rows, scope = None):
    return {"id": id, "version": version, "effective_from": effective_from, "rows": rows, "scope": scope}

def definition(id, version, effective_from, title, text):
    return {"id": id, "version": version, "effective_from": effective_from, "title": title, "text": text}
