//! The health and lifestyle questions as the person insured reads them.
//!
//! The underwriting module holds each question set: the question IDs, their
//! versions, what kind of answer each takes, and which must be answered
//! (`docs/underwriting.md`, "Question sets"). The words a person reads are
//! the digital channel's, held here against each question ID and version
//! (`products/underwriting/questions_*.star` says so). A question whose
//! words change is a new version there, and gets its own words here.
//!
//! The page and an assistant both read the set from here with the exact
//! shape an answer takes, so nobody guesses it.

use serde_json::{json, Value};

/// The words for each question: by ID, with a version where the words
/// differ between versions, and the unit an answer is given in.
const WORDING: [(&str, Option<u64>, &str, &str); 24] = [
    ("height_cm", None, "How tall are you?", "centimetres"),
    ("weight_kg", None, "How much do you weigh?", "kilograms"),
    (
        "smoked_last_12_months",
        None,
        "Have you smoked or vaped tobacco or nicotine in the last 12 months?",
        "",
    ),
    (
        "alcohol_units_per_week",
        None,
        "How many standard drinks of alcohol do you have in a usual week?",
        "standard drinks a week",
    ),
    ("heart_condition", None, "Have you ever had a heart condition, chest pain or a stroke?", ""),
    (
        "high_blood_pressure_or_cholesterol",
        None,
        "Have you ever been told you have high blood pressure or high cholesterol, or been treated for either?",
        "",
    ),
    (
        "asthma_or_lung_condition",
        None,
        "Have you ever had asthma, or another lung or breathing condition?",
        "",
    ),
    (
        "current_medication",
        None,
        "Do you take any medication now, prescribed or not, other than the occasional pain relief?",
        "",
    ),
    ("cancer_history", None, "Have you ever had cancer, a tumour or a growth of any kind?", ""),
    ("diabetes", None, "Have you ever had diabetes, or been told your blood sugar is raised?", ""),
    (
        "mental_health_last_5_years",
        None,
        "In the last five years, have you had anxiety, depression, stress or another mental health condition?",
        "",
    ),
    (
        "back_or_neck_last_5_years",
        None,
        "In the last five years, have you had back or neck pain or injury that needed treatment or time off work?",
        "",
    ),
    (
        "awaiting_tests_or_results",
        None,
        "Are you waiting on any medical test, its results, a specialist appointment or treatment?",
        "",
    ),
    (
        "family_history_before_60",
        None,
        "Did a parent, brother or sister have heart disease, a stroke, cancer, diabetes or a hereditary condition before age 60?",
        "",
    ),
    (
        "hazardous_pursuits",
        None,
        "Do you take part in, or plan to take part in, a hazardous sport or pastime?",
        "",
    ),
    ("pursuit_kind", None, "If so, which kind?", ""),
    (
        "hazardous_occupation",
        None,
        "Does your work involve heights, explosives, diving, offshore work, armed duties or other hazards?",
        "",
    ),
    (
        "travel_high_risk_12_months",
        None,
        "In the next 12 months, do you plan to travel to a country our government advises against travel to?",
        "",
    ),
    (
        "lived_overseas_last_5_years",
        None,
        "In the last five years, have you lived outside New Zealand for more than three months in a row?",
        "",
    ),
    (
        "previous_decline_or_terms",
        None,
        "Has an insurer ever declined, deferred or charged more for life, trauma, disability or income cover for you?",
        "",
    ),
    ("annual_income", None, "What is your income before tax in a year?", "dollars"),
    (
        "existing_cover_elsewhere",
        None,
        "How much life cover do you hold with other insurers, in total?",
        "dollars",
    ),
    ("anything_else", Some(1), "Is there anything else you want to tell us?", ""),
    (
        "anything_else",
        Some(2),
        "Is there anything else about your health you have not been asked? Tell us about any condition, treatment or medication in your own words.",
        "",
    ),
];

/// The words for a pursuit kind.
fn option_words(option: &str) -> &str {
    match option {
        "none" => "None",
        "aviation" => "Flying, other than as a fare-paying passenger",
        "motorsport" => "Motor racing",
        "diving" => "Underwater diving",
        "climbing" => "Climbing or mountaineering",
        "other" => "Something else",
        other => other,
    }
}

/// What each kind of answer is sent as, for the question's `answer` entry.
fn shape(kind: &Value) -> Value {
    match kind["type"].as_str().unwrap_or_default() {
        "yes_no" => json!({ "yes_no": "true or false" }),
        "number" => json!({ "number": format!("a whole number from {} to {}", kind["min"], kind["max"]) }),
        "choice" => json!({ "choice": "one of the options' values" }),
        "money" => json!({ "money": "an amount in cents: $95,000 is 9500000" }),
        _ => json!({ "text": "the person's own words" }),
    }
}

/// The words a person reads for a question at a version.
pub fn wording(id: &str, version: u64) -> Option<(&'static str, &'static str)> {
    WORDING
        .iter()
        .find(|(q, v, _, _)| *q == id && *v == Some(version))
        .or_else(|| WORDING.iter().find(|(q, v, _, _)| *q == id && v.is_none()))
        .map(|(_, _, text, unit)| (*text, *unit))
}

/// Why the person is asked, by the regime the set is asked under.
fn duty(regime: &str) -> &'static str {
    match regime {
        "reasonable_care" => {
            "Take reasonable care to answer each question fully and truthfully. If you are unsure, say so in the last question rather than leave something out."
        }
        _ => {
            "You must tell us everything you know that could affect our decision to insure you, whether or not a question asks about it. Use the last question for anything not asked."
        }
    }
}

/// The consents a life is asked for on this proposal, with what each lets
/// us do and whether it has been given.
fn consents(proposal: &Value, life: &str) -> Vec<Value> {
    let given = &proposal["lives"][life]["consents"];
    let mut kinds = vec![
        (
            "obtain_records",
            "Let us ask your doctors and other sources for your medical records, where your answers need them",
        ),
        (
            "existing_cover_prefill",
            "Let us fill in the cover you already hold with us, from our own records",
        ),
    ];
    if !proposal["adviser"].is_null() {
        kinds.push((
            "adviser_access",
            "Let your adviser see your answers and help with this application, for this application only",
        ));
    }
    if matches!(proposal["entry"]["entry"].as_str(), Some("group_excess" | "group_continuation")) {
        kinds.push((
            "employer_sees_loading",
            "Let your employer see an extra premium for health reasons on the scheme's invoice; without this they see only that you are covered",
        ));
    }
    if proposal["replacement"] == true {
        kinds.push((
            "replacement_disclosure",
            "Confirm you understand this cover replaces cover you hold: read what changes first",
        ));
    }
    kinds
        .into_iter()
        .map(|(kind, about)| {
            json!({ "kind": kind, "about": about, "given": given.get(kind).is_some(),
                "signed": crate::disclosures::SIGNED_CONSENTS.contains(&kind) })
        })
        .collect()
}

/// The questions for a life on a proposal, in the order asked, each with
/// its words, the kind and shape of its answer, and whether it must be
/// answered; with the duty the person answers under and the consents they
/// are asked for. `basis` is the module's disclosure basis for the
/// proposal and `proposal` the proposal as it holds it.
pub fn for_life(basis: &Value, proposal: &Value, life: &str) -> Value {
    let set = &basis["question_set"];
    let questions: Vec<Value> = set["questions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|q| {
            let (id, version) = (q["id"].as_str().unwrap_or_default(), q["version"].as_u64().unwrap_or(1));
            let (text, unit) = wording(id, version).unwrap_or(("", ""));
            let mut out = json!({ "id": id, "version": version, "text": text, "kind": q["kind"]["type"], "required": q["required"],
                "answer": shape(&q["kind"]) });
            if !unit.is_empty() {
                out["unit"] = json!(unit);
            }
            if q["kind"]["type"] == "number" {
                out["min"] = q["kind"]["min"].clone();
                out["max"] = q["kind"]["max"].clone();
            }
            if let Some(options) = q["kind"]["options"].as_array() {
                out["options"] = options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|o| json!({ "value": o, "label": option_words(o) }))
                    .collect();
            }
            out
        })
        .collect();
    let regime = set["regime"].as_str().unwrap_or_default();
    json!({
        "question_set": { "id": set["id"], "version": set["version"], "regime": regime },
        "duty": duty(regime),
        "how_to_answer": "Send answers as an object by question id, each [version, answer]: {\"height_cm\": [2, {\"number\": 172}], \"heart_condition\": [2, {\"yes_no\": false}]}. Leave out a question that is not required and has nothing to say.",
        "questions": questions,
        "consents": consents(proposal, life),
    })
}

/// Each disclosure record an underwriter reads, with the words of each
/// question beside its answer (`question`), where the channel holds them.
pub fn worded(records: &mut Value) {
    let Some(records) = records.as_array_mut() else { return };
    for record in records {
        let Some(answers) = record["record"]["answers"].as_object_mut() else {
            continue;
        };
        for (id, answer) in answers.iter_mut() {
            let version = answer["question_version"].as_u64().unwrap_or(0);
            if let Some((text, unit)) = wording(id, version) {
                answer["question"] = json!(text);
                if !unit.is_empty() {
                    answer["unit"] = json!(unit);
                }
            }
        }
    }
}
