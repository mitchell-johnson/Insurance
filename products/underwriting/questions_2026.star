# Personal statement for individual life, trauma and health cover.
#
# Asked under the duty of disclosure, for contracts made before the Contracts
# of Insurance Act 2024 applies on 15 November 2027. The wording shown to the
# applicant is held with the digital channel against each question id and
# version; a change of wording is a new question version here.

QUESTIONS = question_set(
    id = "personal-statement",
    version = 1,
    effective_from = "2026-01-01",
    effective_to = "2027-11-15",
    regime = "duty_of_disclosure",
    questions = [
        number("height_cm", 100, 230),
        number("weight_kg", 30, 300),
        yes_no("smoked_last_12_months"),
        number("alcohol_units_per_week", 0, 200),
        yes_no("heart_condition"),
        yes_no("cancer_history"),
        yes_no("diabetes"),
        yes_no("mental_health_last_5_years"),
        yes_no("back_or_neck_last_5_years"),
        yes_no("awaiting_tests_or_results"),
        yes_no("family_history_before_60"),
        yes_no("hazardous_pursuits", clinical = False),
        choice("pursuit_kind", ["none", "aviation", "motorsport", "diving", "climbing", "other"], required = False, clinical = False),
        yes_no("hazardous_occupation", clinical = False),
        yes_no("travel_high_risk_12_months", clinical = False),
        yes_no("lived_overseas_last_5_years", clinical = False),
        yes_no("previous_decline_or_terms", clinical = False),
        money("annual_income", clinical = False),
        money("existing_cover_elsewhere", clinical = False),
        text("anything_else", clinical = True),
    ],
)
