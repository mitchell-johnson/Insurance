//! A quote for a walk-in: the new business officer says what the person
//! wants and the few facts the price rests on, and the system makes up the
//! application the core prices. No identifier is invented by the officer,
//! and nothing is written.

use serde_json::{json, Value};

/// The application the core prices, from the officer's own words:
/// `{product_code, frequency, life: {date_of_birth, sex, smoker,
/// occupation_class?}, covers: [{code, sum_insured_minor, excess_option?}],
/// inception?}`. The policy, its number, group and policyholder are the
/// quote's own, as nothing is issued from it; it starts on the books'
/// date `today` unless another day is given.
pub fn walk_in(sent: &Value, today: &str) -> Result<ic_core::command::IssuePolicy, String> {
    let text = |v: &Value, what: &str| {
        v.as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .ok_or(format!("say {what}"))
    };
    let product = text(&sent["product_code"], "the product (product_code), such as aia-essentials-life")?;
    let life = &sent["life"];
    let factors = json!({
        "date_of_birth": text(&life["date_of_birth"], "the life's birth date (life.date_of_birth, YYYY-MM-DD)")?,
        "sex": text(&life["sex"], "the life's sex (life.sex: female or male)")?,
        "smoker": life["smoker"].as_bool().ok_or("say whether the life smokes (life.smoker: true or false)")?,
        // A class said as a number (2) is the class "2" the rate tables name.
        "occupation_class": match &life["occupation_class"] {
            Value::Number(n) => json!(n.to_string()),
            other => other.clone(),
        },
    });
    let covers = sent["covers"]
        .as_array()
        .filter(|c| !c.is_empty())
        .ok_or("name each cover and its amount in cents: covers: [{\"code\": \"life\", \"sum_insured_minor\": 40000000}]")?;
    let covers: Vec<Value> = covers
        .iter()
        .map(|c| {
            Ok(json!({ "code": text(&c["code"], "each cover's code")?, "life": "life-1",
                "sum_insured_minor": c["sum_insured_minor"].as_i64().ok_or("give each cover's amount in cents (sum_insured_minor): $400,000 is 40000000")?,
                "excess_option": c["excess_option"] }))
        })
        .collect::<Result<_, String>>()?;
    let application = json!({
        "policy": "quote", "number": "QUOTE", "group": "quote", "policyholder": "quote",
        "product_code": product,
        "inception": sent["inception"].as_str().unwrap_or(today),
        "frequency": text(&sent["frequency"], "how often they would pay (frequency), such as monthly or yearly")?,
        "lives": [{ "id": "life-1", "kind": { "kind": "named", "party": "quote" }, "factors": factors }],
        "covers": covers,
    });
    serde_json::from_value(application).map_err(|e| format!("that is not a quote the core can price: {e}"))
}

/// The question to underwriting: does the product take this application
/// on `today`. Each cover's type is taken as its code, as the core's
/// application does not carry one.
pub fn eligibility(application: &ic_core::command::IssuePolicy, today: &str) -> Value {
    let lives: Vec<Value> = application
        .lives
        .iter()
        .map(|l| json!({ "life": l.id, "party": application.policyholder, "factors": l.factors }))
        .collect();
    let covers: Vec<Value> = application
        .covers
        .iter()
        .filter_map(|c| {
            let life = c.life.as_ref()?;
            Some(json!({ "code": c.code, "cover_type": c.code, "life": life, "sum_insured_minor": c.sum_insured_minor, "option": c.excess_option }))
        })
        .collect();
    json!({ "query": "eligibility", "product_code": application.product_code, "lives": lives, "covers": covers, "on": today })
}

/// The quote as the officer reads it: the premium for each period with
/// its parts, the total, and what tax applies, in words.
pub fn answer(premium: &Value, frequency: impl serde::Serialize) -> Result<Value, String> {
    let total = crate::disclosures::premium_total(premium)?;
    let gst_none = premium["gst"].as_object().is_none_or(|g| g.is_empty());
    Ok(json!({
        "frequency": frequency,
        "total_minor": total,
        "total": ic_money::dollars(total),
        "premium": premium,
        "gst": if gst_none { "none: life and health cover is exempt from GST" } else { "included in the total" },
    }))
}
