//! Personal data is kept out of log lines and spans.

use std::borrow::Cow;

use ic_telemetry::scrub::{self, sensitive_key, text, REDACTED};
use opentelemetry::{Array, KeyValue, Value};
use serde_json::json;

/// Purpose: field names that say they hold a name, contact detail, date of birth, government or
/// bank identifier, health detail or credential are sensitive, in any of the spellings the code
/// uses (snake case, dotted, camel case, run together); opaque identifiers and the telemetry's
/// own fields are not.
/// Why it matters: the key rule is what catches a person's details logged by a field name, even
/// when the value looks harmless (a name is just text). Redacting `party` or `policy` IDs, or
/// `otel.name`, would make the logs and traces useless for following a case.
#[test]
fn names_that_hold_personal_data_are_recognised() {
    for key in [
        "name",
        "first_name",
        "lastName",
        "customer.email",
        "phone",
        "mobile_number",
        "postal_address",
        "date_of_birth",
        "dob",
        "ird_number",
        "bank_account",
        "AccountNumber",
        "card_number",
        "passport",
        "medical_history",
        "disclosure",
        "authorization",
        "access_token",
        "client_secret",
        "http.request.header.cookie",
        "client.address",
        "user_agent.original",
        "url.query",
        "taxNumber",
        "fullname",
        "nhi",
    ] {
        assert!(sensitive_key(key), "{key} should be sensitive");
    }
    for key in [
        "party",
        "policy",
        "document",
        "partition",
        "error",
        "module",
        "otel.name",
        "service.name",
        "http.route",
        "caller",
        "tokens_issued",
        "seq",
        "code",
    ] {
        assert!(!sensitive_key(key), "{key} should not be sensitive");
    }
}

/// Purpose: e-mail addresses, bearer tokens, JWTs and New Zealand bank account numbers inside
/// free text are masked and the rest of the text is kept; text with none of them comes back
/// borrowed, unchanged.
/// Why it matters: error messages carry text from elsewhere (a refused address, a request
/// echoed back by a provider), so a log line can hold personal data under an innocent field
/// such as `error`. Keeping the rest of the message is what makes it still worth reading.
#[test]
fn personal_data_inside_text_is_masked() {
    assert_eq!(text("sent to jane.doe@example.co.nz today"), format!("sent to {REDACTED} today"));
    assert_eq!(text("sent to jane.doe@example.co.nz."), format!("sent to {REDACTED}"));
    assert_eq!(text("header was Bearer abc.def-123 and more"), format!("header was Bearer {REDACTED} and more"));
    assert_eq!(
        text("token eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl seen"),
        format!("token {REDACTED} seen")
    );
    assert_eq!(text("pay 12-3456-7890123-00 now"), format!("pay {REDACTED} now"));
    assert_eq!(text("pay 12-3456-7890123-000"), format!("pay {REDACTED}"));
    // Not personal data: left alone, and not copied.
    for plain in [
        "partition 7 has no leader",
        "user@ alone",
        "@example.com",
        "a@b",
        "2026-11-15",
        "Bearer",
        "bearer  ",
        "12-34-56-78",
        "eyJ short",
    ] {
        assert!(
            matches!(text(plain), Cow::Borrowed(_)) || text(plain) == plain,
            "{plain} changed to {}",
            text(plain)
        );
    }
    assert!(matches!(text("nothing here"), Cow::Borrowed(_)));
    // A dash-separated number that is not an account number is kept.
    assert_eq!(text("ref 1-2-3-4 at eyJ"), "ref 1-2-3-4 at eyJ");
}

/// Purpose: a value under a sensitive key is replaced whole; a string or list of strings under
/// another key is masked only where it holds personal data; numbers and unchanged text give no
/// replacement.
/// Why it matters: this is the function both span attributes and log attributes go through, so
/// each kind of value must come out right, and an unchanged value must not be rewritten (spans
/// are scrubbed on every export).
#[test]
fn attribute_values_are_scrubbed_by_key_and_by_text() {
    assert_eq!(scrub::value("email", &Value::I64(3)), Some(Value::from(REDACTED)));
    assert_eq!(scrub::value("error", &Value::from("to a@b.com")), Some(Value::from(format!("to {REDACTED}"))));
    assert_eq!(scrub::value("error", &Value::from("plain")), None);
    assert_eq!(scrub::value("count", &Value::I64(3)), None);
    let list = Value::Array(Array::String(vec!["a@b.com".into(), "plain".into()]));
    assert_eq!(
        scrub::value("to", &list),
        Some(Value::Array(Array::String(vec![REDACTED.into(), "plain".into()])))
    );
    assert_eq!(scrub::value("to", &Value::Array(Array::String(vec!["plain".into()]))), None);
    let mut kvs = vec![KeyValue::new("phone", "021 555 0101"), KeyValue::new("party", "P-1")];
    scrub::attributes(&mut kvs);
    assert_eq!(kvs, vec![KeyValue::new("phone", REDACTED), KeyValue::new("party", "P-1")]);
}

/// Purpose: a log line's JSON is scrubbed throughout: sensitive members at any depth are
/// replaced, text is masked inside nested objects and arrays, nulls and numbers are kept, and
/// the `name` of a span the line describes is kept.
/// Why it matters: the JSON formatter nests the event's fields and every enclosing span's
/// fields, so personal data can sit several levels down in a line on standard output, which
/// is what Log Analytics keeps for months.
#[test]
fn log_line_json_is_scrubbed_at_every_depth() {
    let mut line = json!({
        "fields": { "message": "mail to a@b.com", "email": "a@b.com", "party": "P-1", "name": null },
        "spans": [{ "name": "http.server", "phone": "021" }, { "note": "Bearer xyz" }],
        "span": { "name": "http.server", "nested": { "name": "Jane" } },
        "level": "INFO",
        "n": 3
    });
    scrub::json(&mut line);
    assert_eq!(
        line,
        json!({
            "fields": { "message": format!("mail to {REDACTED}"), "email": REDACTED, "party": "P-1", "name": null },
            "spans": [{ "name": "http.server", "phone": REDACTED }, { "note": format!("Bearer {REDACTED}") }],
            "span": { "name": "http.server", "nested": { "name": REDACTED } },
            "level": "INFO",
            "n": 3
        })
    );
}
