# Personal statement for individual life, trauma and health cover.
#
# Asked under the duty to take reasonable care, for contracts made once the Contracts
# of Insurance Act 2024 applies on 15 November 2027: only what is asked counts. The wording shown to the
# applicant is held with the digital channel against each question id and
# version; a change of wording is a new question version here.
#
# Blood pressure or cholesterol, asthma or another lung condition, current
# medication, and anything else in the applicant's own words were added
# before this set came into force: an applicant on a blood pressure tablet
# had nowhere to say so (scenario round 2, F2-02-4).

QUESTIONS = question_set(
    id = "personal-statement",
    version = 2,
    effective_from = "2027-11-15",
    regime = "reasonable_care",
    questions = [
        number("height_cm", 100, 230),
        number("weight_kg", 30, 300),
        yes_no("smoked_last_12_months"),
        number("alcohol_units_per_week", 0, 200),
        yes_no("heart_condition"),
        yes_no("high_blood_pressure_or_cholesterol"),
        yes_no("asthma_or_lung_condition"),
        yes_no("current_medication"),
        yes_no("cancer_history"),
        yes_no("diabetes"),
        yes_no("mental_health_last_5_years"),
        yes_no("back_or_neck_last_5_years"),
        yes_no("awaiting_tests_or_results"),
        yes_no("family_history_before_60"),
        yes_no("hazardous_pursuits", clinical = False, risk_tag = "pursuit.unspecified"),
        choice("pursuit_kind", ["none", "aviation", "motorsport", "diving", "climbing", "other"], required = False, clinical = False,
               risk_tags = {"aviation": "pursuit.aviation", "motorsport": "pursuit.motorsport", "diving": "pursuit.diving",
                            "climbing": "pursuit.climbing", "other": "pursuit.other"}),
        yes_no("hazardous_occupation", clinical = False, risk_tag = "occupation.hazardous"),
        yes_no("travel_high_risk_12_months", clinical = False, risk_tag = "travel.high_risk"),
        yes_no("lived_overseas_last_5_years", clinical = False, risk_tag = "travel.lived_overseas"),
        yes_no("previous_decline_or_terms", clinical = False),
        money("annual_income", clinical = False),
        money("existing_cover_elsewhere", clinical = False),
        # Anything else about your health the questions above did not ask:
        # the reasonable care set asks it in its own words, so it is version 2.
        text("anything_else", version = 2, clinical = True),
    ],
)
