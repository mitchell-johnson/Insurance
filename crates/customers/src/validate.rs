//! What each field may hold, and the normal form it is kept in.
//!
//! Values are checked and normalised before anything is written, so a fact
//! in the record is always well formed, and blind indexes over normal forms
//! find the same person however a value was typed.

use chrono::NaiveDate;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::model::Field;

/// Valid NZ prescribed investor rates, in percent.
pub const PIR_RATES: [&str; 3] = ["10.5", "17.5", "28"];
/// Valid NZ resident withholding tax rates on interest, in percent.
pub const RWT_RATES: [&str; 6] = ["10.5", "17.5", "30", "33", "39", "45"];
pub const PRESENTATION_FORMATS: [&str; 4] = ["standard", "large_print", "audio", "braille"];
pub const NOTICE_CHANNELS: [&str; 4] = ["email", "post", "in_app", "sms"];

/// Why a value was refused.
pub type Problem = String;

fn text(value: &Value, max: usize) -> Result<String, Problem> {
    let s = value.as_str().ok_or("must be text")?;
    let s: String = s.nfc().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() {
        return Err("must not be empty".into());
    }
    if s.chars().count() > max {
        return Err(format!("must be at most {max} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err("must not contain control characters".into());
    }
    Ok(s)
}

fn one_of(value: &Value, allowed: &[&str]) -> Result<Value, Problem> {
    match value.as_str() {
        Some(s) if allowed.contains(&s) => Ok(Value::String(s.to_string())),
        _ => Err(format!("must be one of {}", allowed.join(", "))),
    }
}

pub fn date(value: &Value) -> Result<NaiveDate, Problem> {
    value
        .as_str()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .ok_or_else(|| "must be a date written YYYY-MM-DD".to_string())
}

/// The normal form of an email address: trimmed and lower case.
pub fn email(text: &str) -> Result<String, Problem> {
    let e = text.trim().to_lowercase();
    let problem = || "must be an email address".to_string();
    if e.len() > 254 || e.chars().any(char::is_whitespace) {
        return Err(problem());
    }
    let (local, domain) = e.split_once('@').ok_or_else(problem)?;
    if local.is_empty() || domain.contains('@') || !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err(problem());
    }
    Ok(e)
}

/// The normal form of a mobile number: `+` and digits, in international form.
pub fn phone(text: &str) -> Result<String, Problem> {
    let digits: String = text.chars().filter(|c| !matches!(c, ' ' | '-' | '(' | ')')).collect();
    match digits.strip_prefix('+') {
        Some(rest) if (8..=15).contains(&rest.len()) && rest.chars().all(|c| c.is_ascii_digit()) && !rest.starts_with('0') => Ok(digits),
        _ => Err("must be a phone number in international form, such as +64211234567".into()),
    }
}

/// The normal form of an IRD number: digits only, without leading zeros,
/// with a valid check digit.
pub fn ird(text: &str) -> Result<String, Problem> {
    let digits: String = text.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    let problem = || "must be a valid IRD number".to_string();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(problem());
    }
    let n: u64 = digits.parse().map_err(|_| problem())?;
    if !(10_000_000..150_000_000).contains(&n) {
        return Err(problem());
    }
    let check = (n % 10) as u32;
    let base = format!("{:08}", n / 10);
    let digit = |weights: [u32; 8]| -> u32 {
        let sum: u32 = base.chars().zip(weights).map(|(c, w)| c.to_digit(10).unwrap_or(0) * w).sum();
        match sum % 11 {
            0 => 0,
            r => 11 - r,
        }
    };
    let mut expected = digit([3, 2, 7, 6, 5, 4, 3, 2]);
    if expected == 10 {
        expected = digit([7, 4, 3, 2, 5, 2, 7, 6]);
    }
    if expected == check {
        Ok(n.to_string())
    } else {
        Err(problem())
    }
}

fn address(value: &Value) -> Result<Value, Problem> {
    let lines = value.as_array().ok_or("must be a list of address lines")?;
    if lines.is_empty() || lines.len() > 6 {
        return Err("must have between one and six lines".into());
    }
    let lines = lines.iter().map(|l| text(l, 100)).collect::<Result<Vec<_>, _>>()?;
    Ok(Value::from(lines))
}

fn language(value: &Value) -> Result<Value, Problem> {
    let s = value.as_str().ok_or("must be a language tag")?;
    let (lang, region) = match s.split_once('-') {
        Some((l, r)) => (l, Some(r)),
        None => (s, None),
    };
    let ok_lang = (2..=3).contains(&lang.len()) && lang.chars().all(|c| c.is_ascii_lowercase());
    let ok_region = region.is_none_or(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_uppercase()));
    if ok_lang && ok_region {
        Ok(Value::String(s.to_string()))
    } else {
        Err("must be a language tag such as en, mi or en-NZ".into())
    }
}

fn countries(value: &Value) -> Result<Value, Problem> {
    let list = value.as_array().ok_or("must be a list of country codes")?;
    let mut codes = Vec::new();
    for c in list {
        match c.as_str() {
            Some(code) if code.len() == 2 && code.chars().all(|ch| ch.is_ascii_uppercase()) => {
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
            _ => return Err("must be two-letter country codes such as NZ".into()),
        }
    }
    if codes.is_empty() {
        return Err("must name at least one country".into());
    }
    codes.sort_unstable();
    Ok(Value::from(codes))
}

/// Check a value for a field and return it in its normal form.
pub fn normalise(field: Field, value: &Value) -> Result<Value, Problem> {
    match field {
        Field::GivenName | Field::MiddleNames | Field::FamilyName | Field::PreferredName => Ok(Value::String(text(value, 200)?)),
        Field::Gender => Ok(Value::String(text(value, 100)?)),
        Field::DateOfBirth | Field::DateOfDeath => Ok(Value::String(date(value)?.to_string())),
        Field::Email => Ok(Value::String(email(value.as_str().ok_or("must be text")?)?)),
        Field::MobilePhone => Ok(Value::String(phone(value.as_str().ok_or("must be text")?)?)),
        Field::PostalAddress | Field::ResidentialAddress => address(value),
        Field::PreferredLanguage => language(value),
        Field::PresentationFormat => one_of(value, &PRESENTATION_FORMATS),
        Field::NoticeChannel => one_of(value, &NOTICE_CHANNELS),
        Field::IrdNumber => Ok(Value::String(ird(value.as_str().ok_or("must be text")?)?)),
        Field::TaxResidency => countries(value),
        Field::PrescribedInvestorRate => one_of(value, &PIR_RATES),
        Field::ResidentWithholdingRate => one_of(value, &RWT_RATES),
        Field::OrganisationName => Ok(Value::String(text(value, 300)?)),
        Field::RegistrationNumber => {
            let s = text(value, 50)?;
            if s.chars().all(|c| c.is_ascii_alphanumeric()) {
                Ok(Value::String(s))
            } else {
                Err("must be letters and digits".into())
            }
        }
        Field::Vulnerability => marker(value),
    }
}

/// The kinds of vulnerability a marker may name. A kind, never a diagnosis.
pub const MARKER_KINDS: [&str; 7] = [
    "health",
    "life_event",
    "bereavement",
    "resilience",
    "capability",
    "financial_abuse_risk",
    "other",
];

/// What the insurer does differently for a marked customer.
pub const ADJUSTMENTS: [&str; 9] = [
    "more_time",
    "plain_language",
    "large_print",
    "speak_to_trained_person",
    "pause_before_large_withdrawal",
    "no_phone_calls",
    "support_person_present",
    "written_follow_up",
    "check_understanding",
];

/// A vulnerability marker: `{kind, review_on, adjustments}`, or `false` to
/// remove one. Nothing else is accepted, so no diagnosis or free text can be
/// recorded in it.
fn marker(value: &Value) -> Result<Value, String> {
    let shape = "a marker is {kind, review_on, adjustments}, or false to remove it";
    match value {
        Value::Bool(false) => Ok(Value::Bool(false)),
        Value::Object(m) => {
            if m.keys().any(|k| !["kind", "review_on", "adjustments"].contains(&k.as_str())) {
                return Err(shape.into());
            }
            let kind = one_of(m.get("kind").unwrap_or(&Value::Null), &MARKER_KINDS)?;
            let review_on = m
                .get("review_on")
                .and_then(Value::as_str)
                .and_then(|d| d.parse::<chrono::NaiveDate>().ok())
                .ok_or("review_on is a date")?;
            let mut adjustments = Vec::new();
            for a in m.get("adjustments").and_then(Value::as_array).ok_or("adjustments is a list")? {
                let a = one_of(a, &ADJUSTMENTS)?;
                if !adjustments.contains(&a) {
                    adjustments.push(a);
                }
            }
            Ok(serde_json::json!({ "kind": kind, "review_on": review_on.to_string(), "adjustments": adjustments }))
        }
        _ => Err(shape.into()),
    }
}

/// A name in the form matching compares: letters only, lower case, accents
/// removed, so "O'Brien", "obrien" and "Ó Brien" agree.
pub fn name_key(name: &str) -> String {
    name.nfd()
        .filter(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .filter(char::is_ascii_lowercase)
        .collect()
}

/// A phonetic key for a name (American Soundex over [`name_key`]), so that
/// "Smith" and "Smyth" share a blind index and can be found as candidates.
pub fn phonetic(name: &str) -> String {
    let key = name_key(name);
    let code = |c: char| match c {
        'b' | 'f' | 'p' | 'v' => '1',
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2',
        'd' | 't' => '3',
        'l' => '4',
        'm' | 'n' => '5',
        'r' => '6',
        _ => '0',
    };
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::from(first.to_ascii_uppercase());
    let mut last = code(first);
    for c in chars {
        let d = code(c);
        if d != '0' && d != last {
            out.push(d);
            if out.len() == 4 {
                break;
            }
        }
        // h and w do not separate letters with the same code; vowels do.
        if c != 'h' && c != 'w' {
            last = d;
        }
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Purpose: IRD numbers with valid check digits (including those that need the
    /// secondary weights) are accepted in normal form, and wrong ones are refused.
    /// Why it matters: the IRD number is the strongest identifier matching uses; a typo that
    /// slipped through would either fail to match a person or, worse, match the wrong one.
    #[test]
    fn ird_numbers_are_checked() {
        assert_eq!(ird("49-091-850").unwrap(), "49091850");
        assert_eq!(ird("35901981").unwrap(), "35901981");
        assert_eq!(ird("49098576").unwrap(), "49098576");
        assert_eq!(ird("136410132").unwrap(), "136410132");
        assert!(ird("136410133").is_err());
        assert!(ird("9125568").is_err());
        assert!(ird("150000000").is_err());
        assert!(ird("12a45678").is_err());
        assert!(ird("").is_err());
        // Every check digit outcome, including the secondary weights and the
        // number that has no valid digit at all, is exercised over a range.
        let valid = (10_000_000u64..10_002_000).filter(|n| ird(&n.to_string()).is_ok()).count();
        assert!(valid > 100 && valid < 250, "{valid}");
    }

    /// Purpose: every field accepts its well formed values in normal form and refuses
    /// malformed ones with a reason.
    /// Why it matters: the record must only ever hold well formed facts, because notices,
    /// tax files and matching all read them without re-checking.
    #[test]
    fn fields_are_normalised_and_checked() {
        let ok = |f: Field, v: Value| normalise(f, &v).unwrap();
        let bad = |f: Field, v: Value| assert!(normalise(f, &v).is_err(), "{f} {v}");
        assert_eq!(ok(Field::GivenName, json!("  Aroha   Mere ")), json!("Aroha Mere"));
        bad(Field::FamilyName, json!(""));
        bad(Field::FamilyName, json!(3));
        bad(Field::FamilyName, json!("a\u{7}b"));
        bad(Field::FamilyName, json!("x".repeat(201)));
        assert_eq!(ok(Field::DateOfBirth, json!("1980-02-03")), json!("1980-02-03"));
        bad(Field::DateOfBirth, json!("03/02/1980"));
        assert_eq!(ok(Field::Email, json!(" A@Example.NZ ")), json!("a@example.nz"));
        for e in ["a@b", "@b.nz", "a@b@c.nz", "a b@c.nz", "a@.nz", "a@nz.", "ab.nz"] {
            bad(Field::Email, json!(e));
        }
        bad(Field::Email, json!(format!("{}@b.nz", "a".repeat(260))));
        bad(Field::Email, json!(1));
        assert_eq!(ok(Field::MobilePhone, json!("+64 21 123-4567")), json!("+64211234567"));
        for p in ["021 123 4567", "+0211234567", "+64", "+64a1234567"] {
            bad(Field::MobilePhone, json!(p));
        }
        bad(Field::MobilePhone, json!(5));
        assert_eq!(ok(Field::PostalAddress, json!(["1 Queen St", " Auckland "])), json!(["1 Queen St", "Auckland"]));
        bad(Field::PostalAddress, json!([]));
        bad(Field::PostalAddress, json!(["1", "2", "3", "4", "5", "6", "7"]));
        bad(Field::ResidentialAddress, json!("1 Queen St"));
        bad(Field::ResidentialAddress, json!([""]));
        for l in ["en", "mi", "en-NZ", "haw"] {
            assert_eq!(ok(Field::PreferredLanguage, json!(l)), json!(l));
        }
        for l in ["EN", "e", "en-nz", "en-NZL"] {
            bad(Field::PreferredLanguage, json!(l));
        }
        bad(Field::PreferredLanguage, json!(1));
        assert_eq!(ok(Field::PresentationFormat, json!("braille")), json!("braille"));
        bad(Field::PresentationFormat, json!("huge"));
        assert_eq!(ok(Field::NoticeChannel, json!("post")), json!("post"));
        assert_eq!(ok(Field::IrdNumber, json!("49091850")), json!("49091850"));
        bad(Field::IrdNumber, json!(49091850));
        assert_eq!(ok(Field::TaxResidency, json!(["NZ", "AU", "NZ"])), json!(["AU", "NZ"]));
        bad(Field::TaxResidency, json!([]));
        bad(Field::TaxResidency, json!(["nz"]));
        bad(Field::TaxResidency, json!("NZ"));
        assert_eq!(ok(Field::PrescribedInvestorRate, json!("28")), json!("28"));
        bad(Field::PrescribedInvestorRate, json!("33"));
        assert_eq!(ok(Field::ResidentWithholdingRate, json!("39")), json!("39"));
        assert_eq!(ok(Field::OrganisationName, json!("Acme Ltd")), json!("Acme Ltd"));
        assert_eq!(ok(Field::RegistrationNumber, json!("9429041234567")), json!("9429041234567"));
        bad(Field::RegistrationNumber, json!("94-29"));
        let m = json!({ "kind": "bereavement", "review_on": "2027-05-01", "adjustments": ["more_time", "more_time", "plain_language"] });
        assert_eq!(
            ok(Field::Vulnerability, m),
            json!({ "kind": "bereavement", "review_on": "2027-05-01", "adjustments": ["more_time", "plain_language"] })
        );
        assert_eq!(ok(Field::Vulnerability, json!(false)), json!(false));
        bad(Field::Vulnerability, json!(true));
        bad(Field::Vulnerability, json!("yes"));
        bad(
            Field::Vulnerability,
            json!({ "kind": "health", "review_on": "2027-01-01", "adjustments": [], "diagnosis": "x" }),
        );
        bad(Field::Vulnerability, json!({ "kind": "cancer", "review_on": "2027-01-01", "adjustments": [] }));
        bad(Field::Vulnerability, json!({ "kind": "health", "review_on": "soon", "adjustments": [] }));
        bad(
            Field::Vulnerability,
            json!({ "kind": "health", "review_on": "2027-01-01", "adjustments": "more_time" }),
        );
        bad(
            Field::Vulnerability,
            json!({ "kind": "health", "review_on": "2027-01-01", "adjustments": ["hug"] }),
        );
    }

    /// Purpose: name keys ignore case, punctuation and accents, and the phonetic key groups
    /// names that sound alike while keeping different names apart.
    /// Why it matters: candidate search runs on blind indexes of these keys; if "Ngāti" and
    /// "Ngati" or "Smith" and "Smyth" got different keys, an existing customer would be
    /// created again as a duplicate.
    #[test]
    fn names_reduce_to_comparable_keys() {
        assert_eq!(name_key("O'Brien"), "obrien");
        assert_eq!(name_key("Ó Brien"), "obrien");
        assert_eq!(name_key("Ngāti"), "ngati");
        assert_eq!(phonetic("Smith"), phonetic("Smyth"));
        assert_eq!(phonetic("Robert"), "R163");
        assert_eq!(phonetic("Rupert"), "R163");
        assert_eq!(phonetic("Ashcraft"), "A261");
        assert_eq!(phonetic("Tymczak"), "T522");
        assert_eq!(phonetic("Lee"), "L000");
        assert_ne!(phonetic("Smith"), phonetic("Jones"));
        assert_eq!(phonetic("--"), "");
    }
}
