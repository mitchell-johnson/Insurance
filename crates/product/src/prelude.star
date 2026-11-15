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

def benefit_limit(category, per_event = None, per_year = None, excess = 0, per_lifetime = None):
    return {"category": category, "per_event_minor": per_event, "per_year_minor": per_year, "excess_minor": excess,
            "per_lifetime_minor": per_lifetime}

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

def benefit_category(code, salary_multiple, max_sum_insured, member_pays_percent = None):
    return {"code": code, "salary_multiple": salary_multiple, "max_sum_insured_minor": max_sum_insured, "member_pays_percent": member_pays_percent}

def cover(code, cover_type, benefit, rating, definitions = [], gst_rate = None, levies = [],
          expiry_age = None, min_sum_insured = 0, max_sum_insured = None, categories = [],
          automatic_acceptance_limit = None, excess_options = [], premium_waiver = None,
          shared_counter = None, reduces = [], joint_first_event = False, insurability_option = None, anniversary_option = None, adds_cover = None, buybacks = [], conversions = [], guaranteed_rates = False, pass_back_definitions = False,
          option_axes = [], min_entry_age = None, max_entry_age = None, built_in = [], paid_up_option = None):
    return {
        "paid_up_option": paid_up_option,
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
        "reduces": reduces,
        "guaranteed_rates": guaranteed_rates,
        "pass_back_definitions": pass_back_definitions,
        "joint_first_event": joint_first_event,
        "insurability_option": insurability_option,
        "anniversary_option": anniversary_option,
        "adds_cover": adds_cover,
        "buybacks": buybacks,
        "conversions": conversions,
        "option_axes": option_axes,
        "min_entry_age": min_entry_age,
        "max_entry_age": max_entry_age,
        "built_in": built_in,
    }

def option_axis(code, name, choices):
    """One question on a cover's options menu; a life's cover answers every axis."""
    return {"code": code, "name": name, "choices": choices}

def choice(code, name, premium_factor = "1", excess = None, waiting_days = None, benefit_months = None,
           benefit_to_age = None, expiry_age = None, expiry_after_years = None, premium = None, features = [],
           only_with = []):
    """One answer on an axis: what it does to the premium and the terms it sets."""
    terms = {"features": features}
    for key, value in [("excess_minor", excess), ("waiting_days", waiting_days), ("benefit_months", benefit_months),
                       ("benefit_to_age", benefit_to_age), ("expiry_age", expiry_age),
                       ("expiry_after_years", expiry_after_years), ("premium", premium)]:
        if value != None:
            terms[key] = value
    return {"code": code, "name": name, "premium_factor": premium_factor, "terms": terms, "only_with": only_with}

def stepped_premium():
    """Re-rated at each anniversary on the life's age then."""
    return {"kind": "stepped"}

def level_premium(to_age):
    """Fixed at the age the layer was first rated, until this age."""
    return {"kind": "level", "to_age": to_age}

def limited_pay_premium(to_age):
    """Level, and paid only until the anniversary on or after this age; the cover carries on."""
    return {"kind": "limited_pay", "to_age": to_age}

def level_term_premium(years):
    """Fixed for blocks of this many years, each priced at the age it began."""
    return {"kind": "level_term", "years": years}

def built_in(code, name, kind, max = None, percent_of_cover = None, multiple_of_cover = None, requires = None, definitions = []):
    """A benefit the cover's wording gives besides its main one. kind: early_payment,
    additional, reimbursement, monthly_addition, facility or premium_waiver."""
    return {
        "code": code,
        "name": name,
        "kind": kind,
        "max_minor": max,
        "percent_of_cover": percent_of_cover,
        "multiple_of_cover": multiple_of_cover,
        "requires_feature": requires,
        "definitions": definitions,
    }

def cover_suspension(max_months, min_months_in_force):
    """The owner may suspend the policy, premium and cover both, for up to this many months
    in all, once it has been in force this long."""
    return {"max_months": max_months, "min_months_in_force": min_months_in_force}

def paid_up_option(min_share_of_term, factor):
    """A limited-pay cover may stop paying after this share of its premium term and keep
    the share of the sum insured paid for, times the factor."""
    return {"min_share_of_term": min_share_of_term, "factor": factor}

def insurability_option(events, max_percent_of_original, max_increase, max_uses, within_days, to_age,
                        max_total_percent_of_original = None, max_total_increase = None, once_in_days = None,
                        uses_in_window = None, days_before = None, from_age = None, requires = None,
                        not_before_days = None, max_cover_after = None):
    """More of a lump sum cover without evidence of health, after an event in the life's own life.
    The totals cap all uses together; once_in_days keeps increases without evidence apart."""
    option = {
        "events": events,
        "max_percent_of_original": max_percent_of_original,
        "max_increase_minor": max_increase,
        "max_uses": max_uses,
        "within_days": within_days,
        "to_age": to_age,
    }
    if max_total_percent_of_original != None:
        option["max_total_percent_of_original"] = max_total_percent_of_original
    if max_total_increase != None:
        option["max_total_increase_minor"] = max_total_increase
    if once_in_days != None:
        option["once_in_days"] = once_in_days
    for key, value in [("uses_in_window", uses_in_window), ("days_before", days_before), ("from_age", from_age),
                       ("requires_feature", requires), ("not_before_days", not_before_days),
                       ("max_cover_after_minor", max_cover_after)]:
        if value != None:
            option[key] = value
    return option

def adds_cover_option(events, covers, max_percent_of_original, max_increase, max_uses, within_days, to_age,
                      days_before = None, from_age = None, once_in_days = None):
    """The right to add one of `covers` (a dict of cover code to the choices, axis to choice, it must
    carry) without evidence after an event, up to the lower of the share of this cover and the amount."""
    option = {
        "events": events,
        "covers": covers,
        "max_percent_of_original": max_percent_of_original,
        "max_increase_minor": max_increase,
        "max_uses": max_uses,
        "within_days": within_days,
        "to_age": to_age,
    }
    for key, value in [("days_before", days_before), ("from_age", from_age), ("once_in_days", once_in_days)]:
        if value != None:
            option[key] = value
    return option

def buyback(restores, requires, after_days, within_days, max = None, only_after_nil = False):
    """After a claim on the cover took `restores` (the cover itself, or one it reduces) off the
    life, write it again without evidence, once, where the life chose the feature: asked for
    within `within_days` after surviving one of `after_days`."""
    option = {"restores": restores, "requires_feature": requires, "after_days": after_days, "within_days": within_days}
    if max != None:
        option["max_minor"] = max
    if only_after_nil:
        option["only_after_nil"] = True
    return option

def conversion(to, before_age, axis = None, choices = [], standard_terms_only = False, with_cover = None,
               from_choices = [], requires_evidence = False):
    """All or part of the cover converted without evidence: to another choice on `axis` of the
    same cover (`to` is its own code; `choices` longest commitment first), or to cover `to`."""
    option = {"to": to, "before_age": before_age, "choices": choices}
    if axis != None:
        option["axis"] = axis
    if standard_terms_only:
        option["standard_terms_only"] = True
    if with_cover != None:
        option["with_cover"] = with_cover
    if from_choices:
        option["from_choices"] = from_choices
    if requires_evidence:
        option["requires_evidence"] = True
    return option

def anniversary_option(every_years, max_percent_of_original, at_least, max_total_percent_of_original, max_total_increase,
                       within_days, to_age, requires = None, once_in_days = None, days_before = None, from_age = None,
                       lesser = False, max_cover_after = None, shared_total = False):
    """More of a lump sum cover without evidence of health every few years from the day the cover
    started: the greater of the share and at_least each time, the lower of the totals in all."""
    option = {
        "every_years": every_years,
        "max_percent_of_original": max_percent_of_original,
        "at_least_minor": at_least,
        "max_total_percent_of_original": max_total_percent_of_original,
        "max_total_increase_minor": max_total_increase,
        "within_days": within_days,
        "to_age": to_age,
    }
    if requires != None:
        option["requires_feature"] = requires
    if once_in_days != None:
        option["once_in_days"] = once_in_days
    if days_before != None:
        option["days_before"] = days_before
    if from_age != None:
        option["from_age"] = from_age
    if lesser:
        option["lesser"] = True
    if max_cover_after != None:
        option["max_cover_after_minor"] = max_cover_after
    if shared_total:
        option["shared_total"] = True
    return option

def excess_option(code, excess, premium_factor = "1"):
    """An excess level a customer may choose on a cover, and what it does to the premium."""
    return {"code": code, "excess_minor": excess, "premium_factor": premium_factor}

def levy(code, rate):
    return {"code": code, "rate": rate}

# Lifecycle features. They run in the order the engine defines, not the order
# they are listed in.

def billing(frequencies, policy_fee_annual = "0", modal_loading = {}, frequency_change = None, billing_day_change = False):
    """How premium is billed. frequency_change says when a change of frequency the owner asks for
    takes effect: "anniversary" (the first period on or after the next policy anniversary) or
    "next_period"; left out, the frequency cannot change. billing_day_change lets the owner move
    the day of the month premium falls due."""
    b = {"kind": "billing", "frequencies": frequencies, "policy_fee_annual": policy_fee_annual, "modal_loading": modal_loading}
    if frequency_change != None:
        b["frequency_change"] = frequency_change
    if billing_day_change:
        b["billing_day_change"] = True
    return b

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

def policy_loans(max_percent_of_value, rate_code):
    return {"kind": "policy_loans", "max_percent_of_value": max_percent_of_value, "rate_code": rate_code}

def multi_cover_discount(min_covers, percent):
    return {"kind": "multi_cover_discount", "min_covers": min_covers, "percent": percent}

def discounts(chain, max_total_percent = None, min_net_annual = None, override_single_approver_percent = None,
              override_max_percent = None):
    """The product's discounts in order. Each entry is a discount, additive([...]) or best_of([...])."""
    steps = [s if s.get("kind") in ["additive", "best_of"] else {"kind": "one", "discount": s} for s in chain]
    return {
        "kind": "discounts",
        "chain": steps,
        "max_total_percent": max_total_percent,
        "min_net_annual": min_net_annual,
        "override_single_approver_percent": override_single_approver_percent,
        "override_max_percent": override_max_percent,
    }

def discount(code, name, posting_type, basis, condition = None, covers = [], max_annual = None):
    return {
        "code": code,
        "name": name,
        "posting_type": posting_type,
        "covers": covers,
        "basis": basis,
        "condition": condition if condition != None else {"kind": "always"},
        "max_annual": max_annual,
    }

def additive(members):
    return {"kind": "additive", "discounts": members}

def best_of(members):
    return {"kind": "best_of", "discounts": members}

def percent_off(percent):
    return {"kind": "percent", "percent": percent}

def fixed_annual(amount):
    return {"kind": "fixed_annual", "amount": amount}

def by_tier(percent_by_tier):
    return {"kind": "tier", "percent_by_tier": percent_by_tier}

def min_covers(count):
    return {"kind": "min_covers", "count": count}

def min_group_covers(count):
    return {"kind": "min_group_covers", "count": count}

def min_lives(count):
    return {"kind": "min_lives", "count": count}

def paid(frequencies):
    return {"kind": "frequency", "frequencies": frequencies}

def discount_category(code, covers, min_sum_insured = 0, elsewhere = []):
    """Covers that count as one category towards a multi-benefit discount; `elsewhere` names
    covers on the life's other policies that count too, as "product/cover"."""
    return {"code": code, "covers": covers, "min_sum_insured_minor": min_sum_insured, "elsewhere": elsewhere}

def benefit_categories(required, categories, count):
    """Each life holds a cover from one of the required categories and covers from at least `count` others."""
    return {"kind": "benefit_categories", "required": required, "categories": categories, "count": count}

def entitled(code):
    return {"kind": "entitlement", "code": code}

def composition(core = [], requires = {}, exclusive = [], requires_any = {}):
    return {"kind": "composition", "core": core, "requires": requires, "exclusive": exclusive, "requires_any": requires_any}

def cooling_off(days):
    return {"kind": "cooling_off", "days": days}

def product(code, name, version, effective_from, line, currency, statutory_fund, covers, features,
            rounding = "half_even", event_cap = None, cover_suspension = None):
    return {
        "event_cap_minor": event_cap,
        "cover_suspension": cover_suspension,
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
