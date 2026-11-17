# Underwriting rules for individual life, trauma and health cover, for
# contracts made once the Contracts of Insurance Act 2024 applies on
# 15 November 2027, read against the reasonable care question set
# (questions_2027.star). They are the rules in rules_individual.star, and
# send to a person what that set asks besides: blood pressure or
# cholesterol, asthma or a lung condition, current medication, and anything
# else the applicant tells us in their own words. A rule set with its own
# id, so the set in force before that date is unchanged.

def _rules():
    return [
        # Build and habits.
        rule("UR-0001", any_of(answered_yes("smoked_last_12_months"), smoker()), loading(50), ["RC-0103"], about = "Smoked in the last year, or rated as a smoker: loading"),
        rule("UR-0002", at_least("alcohol_units_per_week", 35), refer("medical"), ["RC-0104", "RC-0201"], about = "Alcohol at 35 units a week or more: refer to medical"),
        rule("UR-0003", all_of(at_least("alcohol_units_per_week", 22), at_most("alcohol_units_per_week", 34)), loading(25), ["RC-0104"], about = "Alcohol at 22 to 34 units a week: loading"),
        rule("UR-0004", at_least("weight_kg", 130), evidence("medical_exam"), ["RC-0102", "RC-0302"], about = "Weight of 130 kg or more: medical examination"),

        # Medical history.
        rule("UR-0005", answered_yes("heart_condition"), evidence("gp_report"), ["RC-0101", "RC-0301"], about = "Heart condition: GP report"),
        rule("UR-0006", answered_yes("heart_condition"), refer("medical"), ["RC-0101", "RC-0201"], about = "Heart condition: refer to medical"),
        rule("UR-0007", answered_yes("cancer_history"), evidence("specialist_report"), ["RC-0101", "RC-0301"], about = "Cancer history: specialist report"),
        rule("UR-0008", answered_yes("cancer_history"), refer("medical"), ["RC-0101", "RC-0201"], about = "Cancer history: refer to medical"),
        rule("UR-0009", answered_yes("diabetes"), evidence("blood_test"), ["RC-0101", "RC-0302"], about = "Diabetes: blood test"),
        rule("UR-0010", answered_yes("mental_health_last_5_years"), tele_interview(), ["RC-0106", "RC-0202"], about = "Mental health in the last five years: tele-interview"),
        rule("UR-0011", all_of(answered_yes("back_or_neck_last_5_years"), cover_type_in(["trauma", "health", "income_protection", "tpd", "accidental_injury"])),
             exclude("EX-1001"), ["RC-0101"], about = "Back or neck in the last five years, on living covers: exclusion"),
        rule("UR-0012", answered_yes("awaiting_tests_or_results"), defer(6), ["RC-0108"], about = "Awaiting tests or results: defer six months"),
        rule("UR-0013", all_of(answered_yes("family_history_before_60"), cover_type_in(["trauma"])), loading(25), ["RC-0105"], about = "Family history before 60, on trauma: loading"),
        rule("UR-0027", answered_yes("high_blood_pressure_or_cholesterol"), refer("medical"), ["RC-0101", "RC-0201"], about = "Blood pressure or cholesterol: refer to medical"),
        rule("UR-0028", answered_yes("asthma_or_lung_condition"), refer("medical"), ["RC-0101", "RC-0201"], about = "Asthma or a lung condition: refer to medical"),
        rule("UR-0029", answered_yes("current_medication"), refer("medical"), ["RC-0101", "RC-0201"], about = "Current medication: refer to medical"),
        rule("UR-0030", not_(unanswered("anything_else")), refer("medical"), ["RC-0101", "RC-0201"], about = "Something else told in the applicant's own words: refer to medical"),
        rule("UR-0014", answered_yes("lived_overseas_last_5_years"), evidence("alternative_medical_history"), ["RC-0305"], about = "Lived overseas in the last five years: alternative medical history"),

        # Pursuits, occupation and travel.
        rule("UR-0015", chose("pursuit_kind", "aviation"), exclude("EX-4001"), ["RC-0121"], about = "Aviation: exclusion"),
        rule("UR-0016", chose("pursuit_kind", "motorsport"), exclude("EX-4002"), ["RC-0121"], about = "Motorsport: exclusion"),
        rule("UR-0017", chose("pursuit_kind", "diving"), exclude("EX-4003"), ["RC-0121"], about = "Diving: exclusion"),
        rule("UR-0018", chose("pursuit_kind", "climbing"), exclude("EX-4004"), ["RC-0121"], about = "Climbing: exclusion"),
        rule("UR-0019", all_of(answered_yes("hazardous_pursuits"), any_of(chose("pursuit_kind", "other"), unanswered("pursuit_kind"))),
             refer("occupational"), ["RC-0121", "RC-0201"], about = "Another hazardous pursuit, or none named: refer to occupational"),
        rule("UR-0020", answered_yes("hazardous_occupation"), refer("occupational"), ["RC-0120", "RC-0201"], about = "Hazardous occupation: refer to occupational"),
        rule("UR-0021", answered_yes("travel_high_risk_12_months"), refer("occupational"), ["RC-0122", "RC-0201"], about = "High-risk travel in the last year: refer to occupational"),

        # History with insurers, and amounts.
        rule("UR-0022", answered_yes("previous_decline_or_terms"), evidence("previous_insurer"), ["RC-0304"], about = "Declined or given terms before: previous insurer's file"),
        rule("UR-0023", all_of(cover_type_in(["life", "trauma", "tpd"]), total_on_life_above(dollars(1500000))),
             evidence("financial_statement"), ["RC-0130", "RC-0303"], about = "Life and trauma above $1.5m on the life: financial statement"),
        rule("UR-0024", total_on_life_above(dollars(3000000)), refer("financial"), ["RC-0131", "RC-0203"], about = "Above $3m on the life: refer to financial"),
        rule("UR-0025", all_of(age_at_least(60), sum_insured_above(dollars(500000))), evidence("medical_exam"), ["RC-0140", "RC-0302"], about = "Aged 60 or more for over $500,000: medical examination"),
        rule("UR-0026", age_at_least(75), decline(), ["RC-0140"], about = "Aged 75 or more: decline"),
    ]

RULES = rule_set(
    id = "individual-reasonable-care",
    version = 1,
    effective_from = "2027-11-15",
    question_set = "personal-statement",
    products = ["term-life", "health-plan", "health-flex", "income-protection", "group-life",
                "aia-living", "aia-living-business", "aia-start-up-income", "aia-cover-for-life", "aia-essentials-life", "aia-starter", "aia-private-health", "aia-cancer-care", "aia-corporate"],
    rules = _rules(),
)
