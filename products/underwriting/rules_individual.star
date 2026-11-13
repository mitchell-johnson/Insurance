# Underwriting rules for individual life, trauma and health cover.
#
# Every rule is evaluated for each life on each cover. Loadings add; the most
# serious action sets what happens next. Each rule names the reason codes
# that explain it. Thresholds are placeholders for the chief underwriter and
# the reinsurer's manual to settle before go-live.

def _rules():
    return [
        # Build and habits.
        rule("smoker", any_of(answered_yes("smoked_last_12_months"), smoker()), loading(50), ["RC-0103"]),
        rule("alcohol-high", at_least("alcohol_units_per_week", 35), refer("medical"), ["RC-0104", "RC-0201"]),
        rule("alcohol-raised", all_of(at_least("alcohol_units_per_week", 22), at_most("alcohol_units_per_week", 34)), loading(25), ["RC-0104"]),
        rule("weight-high", at_least("weight_kg", 130), evidence("medical_exam"), ["RC-0102", "RC-0302"]),

        # Medical history.
        rule("heart", answered_yes("heart_condition"), evidence("gp_report"), ["RC-0101", "RC-0301"]),
        rule("heart-refer", answered_yes("heart_condition"), refer("medical"), ["RC-0101", "RC-0201"]),
        rule("cancer", answered_yes("cancer_history"), evidence("specialist_report"), ["RC-0101", "RC-0301"]),
        rule("cancer-refer", answered_yes("cancer_history"), refer("medical"), ["RC-0101", "RC-0201"]),
        rule("diabetes", answered_yes("diabetes"), evidence("blood_test"), ["RC-0101", "RC-0302"]),
        rule("mental-health", answered_yes("mental_health_last_5_years"), tele_interview(), ["RC-0106", "RC-0202"]),
        rule("back-trauma-health", all_of(answered_yes("back_or_neck_last_5_years"), cover_type_in(["trauma", "health", "income_protection"])),
             exclude("EX-1001"), ["RC-0101"]),
        rule("awaiting-tests", answered_yes("awaiting_tests_or_results"), defer(6), ["RC-0108"]),
        rule("family-history", all_of(answered_yes("family_history_before_60"), cover_type_in(["trauma"])), loading(25), ["RC-0105"]),
        rule("overseas-history", answered_yes("lived_overseas_last_5_years"), evidence("alternative_medical_history"), ["RC-0305"]),

        # Pursuits, occupation and travel.
        rule("pursuit-aviation", chose("pursuit_kind", "aviation"), exclude("EX-4001"), ["RC-0121"]),
        rule("pursuit-motorsport", chose("pursuit_kind", "motorsport"), exclude("EX-4002"), ["RC-0121"]),
        rule("pursuit-diving", chose("pursuit_kind", "diving"), exclude("EX-4003"), ["RC-0121"]),
        rule("pursuit-climbing", chose("pursuit_kind", "climbing"), exclude("EX-4004"), ["RC-0121"]),
        rule("pursuit-other", all_of(answered_yes("hazardous_pursuits"), any_of(chose("pursuit_kind", "other"), unanswered("pursuit_kind"))),
             refer("occupational"), ["RC-0121", "RC-0201"]),
        rule("occupation", answered_yes("hazardous_occupation"), refer("occupational"), ["RC-0120", "RC-0201"]),
        rule("travel", answered_yes("travel_high_risk_12_months"), refer("occupational"), ["RC-0122", "RC-0201"]),

        # History with insurers, and amounts.
        rule("previous-terms", answered_yes("previous_decline_or_terms"), evidence("previous_insurer"), ["RC-0304"]),
        rule("financial", all_of(cover_type_in(["life", "trauma"]), total_on_life_above(dollars(1500000))),
             evidence("financial_statement"), ["RC-0130", "RC-0303"]),
        rule("existing-cover", total_on_life_above(dollars(3000000)), refer("financial"), ["RC-0131", "RC-0203"]),
        rule("age-amount", all_of(age_at_least(60), sum_insured_above(dollars(500000))), evidence("medical_exam"), ["RC-0140", "RC-0302"]),
        rule("age-limit", age_at_least(75), decline(), ["RC-0140"]),
    ]

RULES = rule_set(
    id = "individual",
    version = 1,
    effective_from = "2026-01-01",
    question_set = "personal-statement",
    products = ["term-life", "health-plan", "health-flex", "group-life"],
    rules = _rules(),
)
