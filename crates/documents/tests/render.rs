//! The renderer: a pure function of template and data.

use std::collections::BTreeMap;

use ic_documents::render::{compile, date_in_words, money, reading_grade_x10, supplied, RenderError};
use proptest::prelude::*;
use serde_json::json;

fn render(source: &str, data: serde_json::Value) -> Result<String, RenderError> {
    compile(source, &BTreeMap::new())?.render(&data)
}

/// Purpose: values, money, dates, lists, conditions and root references render as written,
/// and yes/no and numbers are written in plain form.
/// Why it matters: every document the insurer issues goes through these few constructs. A
/// wrong figure in a schedule is a wrong contract document.
#[test]
fn a_template_renders_values_lists_and_conditions() {
    let data = json!({
        "name": "Aroha",
        "smoker": false,
        "age": 41,
        "start": "2027-11-15",
        "total": 123456789,
        "covers": [
            {"name": "Life", "sum": 50000000, "terms": ["Standard rates"]},
            {"name": "Trauma", "sum": 10000000, "terms": []}
        ],
        "lender": {"name": "Kiwi Bank"}
    });
    let source = "Dear {{ name }}, aged {{age}}, smoker: {{smoker}}. From {{start|date}}.\n\
        {{#each covers}}- {{name}} {{sum|money}} for {{@root.name}}{{#each terms}} ({{this}}){{/each}}{{#unless terms}} (no terms){{/unless}}\n{{/each}}\
        Total {{total|money}}.{{#if lender}} Copy sent to {{lender.name}}.{{/if}}{{#if missing}} never shown{{/if}}";
    assert_eq!(
        render(source, data).unwrap(),
        "Dear Aroha, aged 41, smoker: no. From 15 November 2027.\n\
         - Life $500,000.00 for Aroha (Standard rates)\n\
         - Trauma $100,000.00 for Aroha (no terms)\n\
         Total $1,234,567.89. Copy sent to Kiwi Bank."
    );
    assert_eq!(render("{{ok}}", json!({"ok": true})).unwrap(), "yes");
    // Each kind of value has its own idea of empty.
    for (value, shown) in [
        (json!(0), false),
        (json!(2), true),
        (json!(""), false),
        (json!(null), false),
        (json!({}), true),
        (json!(true), true),
    ] {
        assert_eq!(render("{{#if v}}x{{/if}}", json!({ "v": value })).unwrap() == "x", shown);
    }
}

/// Purpose: a value the template needs and the data lacks is an error naming the field, and
/// a value of the wrong type for its format is an error (money is whole minor units, as a
/// number or the text of one); nothing is rendered blank.
/// Why it matters: a notice with a blank where the premium should be is a defective notice.
/// Failing the render is the only safe behaviour.
#[test]
fn a_missing_or_mistyped_value_fails_the_render() {
    assert_eq!(render("{{premium|money}}", json!({})), Err(RenderError::Missing("premium".into())));
    assert_eq!(render("{{a.b}}", json!({"a": 3})), Err(RenderError::Missing("a.b".into())));
    assert_eq!(render("{{#each covers}}x{{/each}}", json!({})), Err(RenderError::Missing("covers".into())));
    assert!(matches!(
        render("{{#each covers}}x{{/each}}", json!({"covers": 3})),
        Err(RenderError::WrongType { .. })
    ));
    // Whole minor units written as text (a requested letter's detail) are
    // money; anything else in text is not.
    assert_eq!(render("{{premium|money}}", json!({"premium": "1250"})).unwrap(), "$12.50");
    for text in ["12.50", "twelve", ""] {
        assert!(matches!(
            render("{{premium|money}}", json!({ "premium": text })),
            Err(RenderError::WrongType { .. })
        ));
    }
    assert!(matches!(
        render("{{start|date}}", json!({"start": "15/11/2027"})),
        Err(RenderError::WrongType { .. })
    ));
    assert!(matches!(render("{{start|date}}", json!({"start": 5})), Err(RenderError::WrongType { .. })));
    for bad in [json!(null), json!([1]), json!({"x": 1})] {
        let error = render("{{v}}", json!({ "v": bad })).unwrap_err();
        assert!(error.to_string().contains("cannot be written as text"));
    }
}

/// Purpose: malformed templates are refused at compile time with a message: an unclosed tag,
/// an unclosed or mismatched block, a stray close, an unknown format, an empty or invalid
/// path, an include of an undeclared part, and parts that include themselves.
/// Why it matters: a template is checked when it is submitted, long before a customer's data
/// meets it. A syntax error found at issue time would stop a billing run's notices.
#[test]
fn malformed_templates_do_not_compile() {
    let none = BTreeMap::new();
    for source in [
        "{{name",
        "{{#if a}}x",
        "{{#each a}}x{{/if}}",
        "{{/each}}",
        "{{/wat}}",
        "{{a|shout}}",
        "{{}}",
        "{{a b}}",
        "{{a..b}}",
        "{{>}}",
        "{{#unless a}}x",
    ] {
        assert!(matches!(compile(source, &none), Err(RenderError::Syntax(_))), "{source}");
    }
    assert_eq!(compile("{{> clause}}", &none), Err(RenderError::UnknownPart("clause".into())));
    let looped = BTreeMap::from([("a".to_string(), "{{> b}}".to_string()), ("b".to_string(), "{{> a}}".to_string())]);
    assert!(matches!(compile("{{> a}}", &looped), Err(RenderError::IncludeDepth(_))));
    let nested = BTreeMap::from([
        ("outer".to_string(), "[{{> inner}}]".to_string()),
        ("inner".to_string(), "{{name}}".to_string()),
    ]);
    assert_eq!(compile("{{> outer}}", &nested).unwrap().render(&json!({"name": "A"})).unwrap(), "[A]");
    assert!(RenderError::Syntax("x".into()).to_string().contains("malformed"));
    assert!(RenderError::IncludeDepth("a".into()).to_string().contains("nested too deeply"));
    assert!(RenderError::UnknownPart("a".into()).to_string().contains("does not declare"));
}

/// Purpose: the fields a template reads are reported in the same notation as the fields a
/// piece of data supplies, with fields used only under a condition marked optional.
/// Why it matters: this is how "a template cannot reference a field a product does not
/// supply" is checked. If the two notations disagreed the check would pass wrong templates
/// or fail right ones.
#[test]
fn required_fields_match_the_notation_of_supplied_fields() {
    let compiled = compile(
        "{{name}} {{#each covers}}{{name}} {{@root.currency}} {{#each terms}}{{this}}{{/each}}{{#if loading}}{{loading}}{{/if}}{{/each}}{{#if lender}}{{lender.name}}{{#each notes}}{{text}}{{/each}}{{/if}}{{#if name}}!{{/if}}",
        &BTreeMap::new(),
    )
    .unwrap();
    let fields = compiled.fields();
    let required: Vec<&str> = fields.required.iter().map(String::as_str).collect();
    assert_eq!(
        required,
        vec!["covers", "covers[].name", "covers[].terms", "covers[].terms[]", "currency", "name"]
    );
    let optional: Vec<&str> = fields.optional.iter().map(String::as_str).collect();
    assert_eq!(optional, vec!["covers[].loading", "lender", "lender.name", "notes", "notes[].text"]);

    let data = json!({"name": "A", "currency": "NZD", "covers": [{"name": "Life", "terms": ["x"]}, {"name": "Trauma", "terms": [], "loading": "50%"}]});
    let have = supplied(&data);
    assert!(
        fields.required.iter().filter(|f| *f != "covers[].terms[]").all(|f| have.contains(f)),
        "{have:?}"
    );
    assert!(have.contains("covers[].loading"));
    assert_eq!(compiled.prose().matches(" it ").count(), 7);
}

/// Purpose: money and dates are written the way a New Zealand customer reads them, including
/// negatives, small amounts and the largest amount an i64 holds.
/// Why it matters: a refund shown without its sign, or a thousands separator in the wrong
/// place, changes what the document says.
#[test]
fn money_and_dates_are_written_for_people() {
    assert_eq!(money(0), "$0.00");
    assert_eq!(money(5), "$0.05");
    assert_eq!(money(-1050), "-$10.50");
    assert_eq!(money(100000), "$1,000.00");
    assert_eq!(money(i64::MIN), "-$92,233,720,368,547,758.08");
    assert_eq!(date_in_words("2028-02-29").as_deref(), Some("29 February 2028"));
    assert_eq!(date_in_words("2027-02-29"), None);
}

/// Purpose: the reading grade is low for plain sentences, high for dense legal prose, zero
/// for no words, and the same every time.
/// Why it matters: plain language is a requirement of the template. The measure gates
/// submission, so it must rank wording sensibly and be deterministic on every replica.
#[test]
fn reading_grade_ranks_plain_words_below_dense_ones() {
    let plain = reading_grade_x10("We will pay you if you die. Tell us if you move. Pay on time to keep your cover.");
    let dense = reading_grade_x10("Notwithstanding antecedent contractual representations, indemnification obligations remain unconditionally enforceable irrespective of subsequent circumstantial modifications.");
    assert!(plain < 60, "{plain}");
    assert!(dense > 200, "{dense}");
    assert_eq!(reading_grade_x10(""), 0);
    assert_eq!(reading_grade_x10("123 456"), 0);
    assert_eq!(
        reading_grade_x10("The able people settle here"),
        reading_grade_x10("The able people settle here")
    );
}

proptest! {
    /// Purpose: for any name and amount, rendering twice gives the same text, and the text
    /// carries the amount exactly as the money format writes it.
    /// Why it matters: rendering must be a pure function, because an issued document is
    /// verified later by rendering its stored data again and comparing hashes.
    #[test]
    fn rendering_is_deterministic(name in "[A-Za-z ]{1,20}", amount in any::<i64>()) {
        let compiled = compile("{{name}} owes {{amount|money}}", &BTreeMap::new()).unwrap();
        let data = json!({"name": name, "amount": amount});
        let once = compiled.render(&data).unwrap();
        prop_assert_eq!(&once, &compiled.render(&data).unwrap());
        prop_assert_eq!(once, format!("{} owes {}", name, money(amount)));
    }

    /// Purpose: any text with no tags renders as itself, and never panics the compiler.
    /// Why it matters: wording is written by people. The parser must not mangle or crash on
    /// ordinary punctuation, including single braces.
    #[test]
    fn plain_text_passes_through(text in "[^{}]{0,80}[{}]?[^{}]{0,20}") {
        prop_assert_eq!(render(&text, json!({})).unwrap(), text);
    }
}
