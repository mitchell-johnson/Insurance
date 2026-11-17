//! New business as the portal says it: the questions in words, a walk-in
//! quote, an offer's terms, the intake's refusals and a possible duplicate.

use ic_channels::{customers, disclosures, questions, quote};
use serde_json::{json, Value};

/// Purpose: every question in every shipped question set, at its version, has words a person reads; a question kept at
/// another version has its own words where they differ; and the form says what kind of answer each takes.
/// Why it matters: the customer could not answer her questions on any screen, and an assistant had nothing to go on but
/// ids, found by the scenario run, F2-02-1 and F2-02-3.
#[test]
fn every_shipped_question_is_worded() {
    let shipped = ic_underwriting::shipped::load().unwrap();
    for set in &shipped.question_sets {
        for q in &set.questions {
            let (text, _) = questions::wording(&q.id, u64::from(q.version)).unwrap_or_else(|| panic!("{} v{} has no words", q.id, q.version));
            assert!(!text.is_empty(), "{}", q.id);
        }
    }
    assert_ne!(questions::wording("anything_else", 1), questions::wording("anything_else", 2));
    assert_eq!(questions::wording("nope", 1), None);

    let basis = json!({ "question_set": { "id": "personal-statement", "version": 2, "regime": "reasonable_care", "questions": [
        { "id": "height_cm", "version": 1, "required": true, "kind": { "type": "number", "min": 100, "max": 250 } },
        { "id": "pursuit_kind", "version": 1, "required": false, "kind": { "type": "choice", "options": ["none", "diving", "kite"] } },
        { "id": "annual_income", "version": 1, "required": false, "kind": { "type": "money" } },
        { "id": "anything_else", "version": 2, "required": false, "kind": { "type": "text" } },
        { "id": "heart_condition", "version": 1, "required": true, "kind": { "type": "yes_no" } }] } });
    let proposal = json!({ "adviser": "adv-1", "entry": { "entry": "group_excess" }, "replacement": true,
        "lives": { "L1": { "consents": { "obtain_records": true } } } });
    let asked = questions::for_life(&basis, &proposal, "L1");
    let q = &asked["questions"];
    assert_eq!((&q[0]["unit"], &q[0]["min"], &q[0]["max"]), (&json!("centimetres"), &json!(100), &json!(250)));
    assert_eq!(q[1]["options"][1], json!({ "value": "diving", "label": "Underwater diving" }));
    assert_eq!(q[1]["options"][2]["label"], "kite", "an option with no words of its own is shown as it is");
    assert!(q[2]["answer"]["money"].as_str().unwrap().contains("cents"));
    assert!(q[3]["text"].as_str().unwrap().contains("own words"));
    assert_eq!(q[4]["answer"], json!({ "yes_no": "true or false" }));
    assert!(asked["duty"].as_str().unwrap().starts_with("Take reasonable care"));
    let kinds: Vec<&str> = asked["consents"].as_array().unwrap().iter().filter_map(|c| c["kind"].as_str()).collect();
    assert_eq!(
        kinds,
        [
            "obtain_records",
            "existing_cover_prefill",
            "adviser_access",
            "employer_sees_loading",
            "replacement_disclosure"
        ]
    );
    let duty = questions::for_life(&json!({ "question_set": { "regime": "duty_of_disclosure" } }), &json!({}), "L1");
    assert!(duty["duty"].as_str().unwrap().starts_with("You must tell us everything"));
}

/// Purpose: an underwriter reads each answer beside the words the person was asked, at the version they answered; an
/// answer with no words held, or a record that is not a list, is left as it is.
/// Why it matters: the disclosure showed question ids made into words and raw codes, found by the scenario run, F2-02-13.
#[test]
fn a_disclosure_is_read_with_its_questions() {
    let mut records = json!([{ "record": { "answers": {
        "height_cm": { "question_version": 1, "answer": { "number": 172 } },
        "anything_else": { "question_version": 2, "answer": { "text": "no" } },
        "q-unknown": { "question_version": 1, "answer": { "yes_no": true } } } } }, { "record": {} }]);
    questions::worded(&mut records);
    let answers = &records[0]["record"]["answers"];
    assert_eq!(
        (&answers["height_cm"]["question"], &answers["height_cm"]["unit"]),
        (&json!("How tall are you?"), &json!("centimetres"))
    );
    assert!(answers["anything_else"]["question"].as_str().unwrap().contains("own words"));
    assert!(answers["anything_else"].get("unit").is_none());
    assert!(answers["q-unknown"].get("question").is_none());
    let mut other = json!({ "not": "a list" });
    questions::worded(&mut other);
    assert_eq!(other, json!({ "not": "a list" }));
}

/// Purpose: a walk-in's application is made up from the officer's own words, with nothing for them to invent, starting on
/// the books' date unless another is given; each fact that is missing is named with how to give it; underwriting is asked
/// about the product with each cover's type; and the quote's answer has the total in dollars and GST in words.
/// Why it matters: the quote made the officer invent a policy, number, group and party, and answered with no total, found
/// by the scenario run, F2-01-3 and F2-01-4.
#[test]
fn a_walk_in_application_is_made_from_what_the_officer_says() {
    let sent = json!({ "product_code": " aia-essentials-life ", "frequency": "yearly",
        "life": { "date_of_birth": "1980-02-03", "sex": "male", "smoker": true, "occupation_class": 2 },
        "covers": [{ "code": "life", "sum_insured_minor": 40_000_000 }] });
    let application = quote::walk_in(&sent, "2027-11-15").unwrap();
    assert_eq!(
        (application.product_code.as_str(), application.inception.to_string()),
        ("aia-essentials-life", "2027-11-15".to_string())
    );
    assert_eq!(application.covers[0].sum_insured_minor, 40_000_000);
    assert_eq!(application.lives[0].factors.occupation_class.as_deref(), Some("2"));
    let mut later = sent.clone();
    later["inception"] = json!("2027-12-01");
    assert_eq!(quote::walk_in(&later, "2027-11-15").unwrap().inception.to_string(), "2027-12-01");

    let asked = quote::eligibility(&application, "2027-11-15");
    assert_eq!((&asked["query"], &asked["on"]), (&json!("eligibility"), &json!("2027-11-15")));
    assert_eq!(asked["covers"][0]["cover_type"], "life");
    assert_eq!(asked["lives"][0]["factors"]["date_of_birth"], "1980-02-03");

    let without = |path: &[&str]| {
        let mut v = sent.clone();
        match path {
            [one] => v[*one] = Value::Null,
            [a, b] => v[*a][*b] = Value::Null,
            _ => v["covers"][0][path[2]] = Value::Null,
        }
        quote::walk_in(&v, "2027-11-15").unwrap_err()
    };
    assert!(without(&["product_code"]).contains("product_code"));
    assert!(without(&["frequency"]).contains("frequency"));
    assert!(without(&["life", "date_of_birth"]).contains("date_of_birth"));
    assert!(without(&["life", "sex"]).contains("life.sex"));
    assert!(without(&["life", "smoker"]).contains("life.smoker"));
    assert!(without(&["covers"]).contains("covers"));
    assert!(without(&["covers", "0", "code"]).contains("code"));
    assert!(without(&["covers", "0", "sum_insured_minor"]).contains("cents"));
    let mut odd = sent.clone();
    odd["frequency"] = json!("hourly");
    assert!(quote::walk_in(&odd, "2027-11-15")
        .unwrap_err()
        .starts_with("that is not a quote the core can price"));

    let bill = json!({ "by_cover": { "life": { "minor": 4100, "currency": "NZD" } }, "policy_fee": { "minor": 0, "currency": "NZD" },
        "gst": {}, "levies": { "fsl": { "minor": 34, "currency": "NZD" } } });
    let said = quote::answer(&bill, "monthly").unwrap();
    assert_eq!((&said["total_minor"], &said["total"]), (&json!(4134), &json!("$41.34")));
    assert!(said["gst"].as_str().unwrap().starts_with("none"));
    let mut taxed = bill.clone();
    taxed["gst"] = json!({ "gst": { "minor": 100, "currency": "NZD" } });
    assert_eq!(quote::answer(&taxed, "monthly").unwrap()["gst"], "included in the total");
    assert!(quote::answer(&json!({ "total_minor": 1 }), "monthly").is_err());
}

/// Purpose: an offer's terms are said in words, cover by cover: standard, an exclusion, an extra premium, declined,
/// deferred with its date, or another outcome, with each exclusion's kind from the code list and a revised amount where
/// there is one.
/// Why it matters: the customer accepted an offer without seeing its terms, found by the scenario run, F2-02-7.
#[test]
fn an_offers_terms_are_said_in_words() {
    let categories = json!({ "EX-1": { "category": "condition" }, "EX-2": { "category": "pursuit" }, "EX-3": { "category": "travel" },
        "EX-4": { "category": "body_system" }, "EX-5": { "category": "mental_health" }, "EX-6": { "category": "pre_existing" } });
    let excluded = |code: &str| json!({ "cover": "life", "outcome": { "outcome": "accept", "exclusions": [{ "code": code }] } });
    let terms = json!([
        { "cover": "life", "outcome": { "outcome": "accept" } },
        excluded("EX-1"), excluded("EX-2"), excluded("EX-3"), excluded("EX-4"), excluded("EX-5"), excluded("EX-6"), excluded("EX-9"),
        { "cover": "tpd", "outcome": { "outcome": "accept", "loading_percent": 25, "revised_sum_insured_minor": 100 } },
        { "cover": "trauma", "outcome": { "outcome": "decline" } },
        { "cover": "income", "outcome": { "outcome": "defer", "until": "2028-01-01" } },
        { "cover": "other", "outcome": { "outcome": "counter" } },
    ]);
    let said = disclosures::offered_terms(&terms, &categories);
    let words: Vec<&str> = said.iter().map(|t| t["said"].as_str().unwrap()).collect();
    assert_eq!(words[0], "Offered at standard rates");
    assert_eq!(words[1], "Offered with an exclusion");
    let about: Vec<&str> = said[1..8].iter().map(|t| t["exclusions"][0]["about"].as_str().unwrap()).collect();
    assert_eq!(
        about,
        [
            "Not covered: a medical condition",
            "Not covered: a sport or pastime",
            "Not covered: travel or living overseas",
            "Not covered: a part of the body",
            "Not covered: mental health",
            "Not covered: a condition you already have",
            "Not covered: a clause in your offer letter",
        ]
    );
    assert_eq!(
        (words[8], &said[8]["revised_sum_insured_minor"]),
        ("Offered with an extra 25% on its premium", &json!(100))
    );
    assert_eq!(
        &words[9..],
        [
            "Not offered",
            "Not offered now: you may apply again after 2028-01-01",
            "Offered on terms in your offer letter"
        ]
    );
    assert!(disclosures::offered_terms(&Value::Null, &categories).is_empty());
}

/// Purpose: an answer that is not [version, {one kind}] is refused naming the question and how to send it, before the
/// intake is asked, as is an adviser list that is not ids; the intake's list of problems is said in words with each
/// question's own; and an intake that answers in plain text is read as its message.
/// Why it matters: a wrong shape was refused without words, so an assistant could not correct it, found by the scenario
/// run, F2-02-3.
#[test]
fn answers_that_do_not_fit_are_said_in_words() {
    let send = |answers: Value, adviser: Value| {
        let mut body = json!({ "step": "disclosure", "answers": answers });
        if !adviser.is_null() {
            body["adviser_entered"] = adviser;
        }
        disclosures::answers("P1", "L1", &body)
    };
    assert!(send(json!({ "heart_condition": [1, { "yes_no": false }] }), Value::Null).is_ok());
    for wrong in [
        json!({ "heart_condition": false }),
        json!({ "heart_condition": [1, { "yes_no": false, "number": 1 }] }),
        json!({ "heart_condition": ["1", { "yes_no": false }] }),
        json!({ "heart_condition": [1, { "maybe": 1 }] }),
    ] {
        assert!(
            send(wrong.clone(), Value::Null)
                .unwrap_err()
                .starts_with("the answer to heart_condition is [its version"),
            "{wrong}"
        );
    }
    assert!(send(json!({}), json!("adviser")).unwrap_err().starts_with("adviser_entered"));
    assert!(send(json!({}), json!([1])).is_err());
    assert!(send(json!({}), json!(["heart_condition"])).is_ok());

    let mut refused = json!({ "problems": [
        { "question": "height_cm", "problem": "missing", "expected": 1 },
        { "question": "q_x", "problem": "unknown_question" },
        { "question": "anything_else", "problem": "wrong_version", "expected": 2 },
        { "question": "weight_kg", "problem": "out_of_range", "expected": 1 },
        { "question": "some_thing", "problem": "wrong_kind" }] });
    disclosures::answer_problems(&mut refused);
    let message = refused["message"].as_str().unwrap();
    assert!(
        message.starts_with("Some answers need attention: \"How tall are you?\" needs an answer; q_x is not one of the questions asked; "),
        "{message}"
    );
    assert!(
        message.contains("is version 2") && message.contains("outside what it takes") && message.contains("the answer to some thing is not the kind it takes"),
        "{message}"
    );
    let mut fine = json!({ "problems": [] });
    disclosures::answer_problems(&mut fine);
    assert!(fine.get("message").is_none());

    assert_eq!(disclosures::answered(br#"{"ok":true}"#), json!({ "ok": true }));
    assert_eq!(disclosures::answered(b" bad gateway \n"), json!({ "message": "bad gateway" }));
    assert_eq!(disclosures::answered(b"  "), Value::Null);
    assert!(disclosures::premium_total(&json!({})).unwrap_err().contains("not understood"));
}

/// Purpose: a new record refused as a possible duplicate names the records it may be and says what to do next (look with
/// match_customer, or send allow_duplicate); with no candidates it still says what to do; any other refusal is left alone.
/// Why it matters: the refusal named no candidate and gave no next step, found by the scenario run, F2-01-5.
#[test]
fn a_possible_duplicate_says_who_and_what_next() {
    let mut two = json!({ "title": "possible_duplicate", "candidates": [{ "party": "p-1" }, "p-2"] });
    customers::possible_duplicate(&mut two);
    let said = two["detail"].as_str().unwrap();
    assert!(
        said.starts_with("this person may already be known: p-1, p-2 on file may be them. Look at each"),
        "{said}"
    );
    let mut one = json!({ "title": "possible_duplicate", "candidates": ["p-1"] });
    customers::possible_duplicate(&mut one);
    assert!(one["detail"].as_str().unwrap().contains("Look at them"));
    let mut none = json!({ "title": "possible_duplicate" });
    customers::possible_duplicate(&mut none);
    assert!(none["detail"].as_str().unwrap().starts_with("this person may already be known. Look at each"));
    let mut other = json!({ "title": "stale", "detail": "as it was" });
    customers::possible_duplicate(&mut other);
    assert_eq!(other["detail"], "as it was");
}
