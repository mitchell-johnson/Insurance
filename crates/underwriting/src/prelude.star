# Authoring kit for underwriting configuration: exclusion and reason code
# libraries, question sets and rule sets.
#
# These helpers only build plain data. The loader validates the result into
# typed Rust values; Starlark plays no part when a rule runs or a code is
# matched.

def dollars(n):
    """Whole dollars as minor units."""
    return n * 100

# --- codes ---

def exclusion(code, version, category, applies_to, cause_tags, title, wording):
    return {"code": code, "version": version, "category": category, "applies_to": applies_to,
            "cause_tags": cause_tags, "title": title, "wording": wording}

def exclusion_library(id, version, effective_from, entries):
    return {"id": id, "version": version, "effective_from": effective_from, "entries": entries}

def reason(code, kind, disclosable, text):
    return {"code": code, "kind": kind, "disclosable": disclosable, "text": text}

def reason_library(id, version, effective_from, entries):
    return {"id": id, "version": version, "effective_from": effective_from, "entries": entries}

# --- questions ---

def yes_no(id, version = 1, required = True, clinical = True, risk_tag = None):
    return {"id": id, "version": version, "kind": {"type": "yes_no"}, "required": required, "clinical": clinical,
            "risk_tags": {"yes": risk_tag} if risk_tag else {}}

def number(id, min, max, version = 1, required = True, clinical = True):
    return {"id": id, "version": version, "kind": {"type": "number", "min": min, "max": max}, "required": required, "clinical": clinical}

def choice(id, options, version = 1, required = True, clinical = True, risk_tags = {}):
    return {"id": id, "version": version, "kind": {"type": "choice", "options": options}, "required": required, "clinical": clinical,
            "risk_tags": risk_tags}

def text(id, version = 1, required = False, clinical = True):
    return {"id": id, "version": version, "kind": {"type": "text"}, "required": required, "clinical": clinical}

def money(id, version = 1, required = True, clinical = False):
    return {"id": id, "version": version, "kind": {"type": "money"}, "required": required, "clinical": clinical}

def question_set(id, version, effective_from, regime, questions, effective_to = None):
    return {"id": id, "version": version, "effective_from": effective_from, "effective_to": effective_to,
            "regime": regime, "questions": questions}

# --- conditions ---

def always():
    return {"if": "always"}

def all_of(*conds):
    return {"if": "all", "of": list(conds)}

def any_of(*conds):
    return {"if": "any", "of": list(conds)}

def not_(cond):
    return {"if": "not", "cond": cond}

def answered_yes(question):
    return {"if": "answer_is", "question": question, "value": {"yes_no": True}}

def answered_no(question):
    return {"if": "answer_is", "question": question, "value": {"yes_no": False}}

def chose(question, option):
    return {"if": "answer_is", "question": question, "value": {"choice": option}}

def at_least(question, value):
    return {"if": "answer_at_least", "question": question, "value": value}

def at_most(question, value):
    return {"if": "answer_at_most", "question": question, "value": value}

def unanswered(question):
    return {"if": "answer_missing", "question": question}

def age_at_least(years):
    return {"if": "age_at_least", "years": years}

def age_at_most(years):
    return {"if": "age_at_most", "years": years}

def smoker(is_smoker = True):
    return {"if": "smoker", "is": is_smoker}

def cover_type_in(types):
    return {"if": "cover_type_in", "types": types}

def sum_insured_above(minor):
    return {"if": "sum_insured_above", "minor": minor}

def total_on_life_above(minor):
    return {"if": "total_on_life_above", "minor": minor}

# --- actions ---

def loading(percent):
    return {"do": "load", "percent": percent}

def exclude(code):
    return {"do": "exclude", "code": code}

def evidence(kind):
    return {"do": "evidence", "kind": kind}

def refer(queue):
    return {"do": "refer", "queue": queue}

def tele_interview():
    return {"do": "tele_interview"}

def decline():
    return {"do": "decline"}

def defer(months):
    return {"do": "defer", "months": months}

def rule(id, when, then, reasons):
    return {"id": id, "when": when, "then": then, "reasons": reasons}

def rule_set(id, version, effective_from, question_set, products, rules, effective_to = None):
    return {"id": id, "version": version, "effective_from": effective_from, "effective_to": effective_to,
            "question_set": question_set, "products": products, "rules": rules}

# --- authority ---

def profile(underwriter, version, limits, products, queues, max_loading_percent = 0, may_exclude = False,
            may_decline = False, may_defer = False, senior = False, role = "underwriter"):
    return {"underwriter": underwriter, "version": version, "limits": limits, "products": products,
            "queues": queues, "max_loading_percent": max_loading_percent, "may_exclude": may_exclude,
            "may_decline": may_decline, "may_defer": may_defer, "senior": senior, "role": role}
